//! Onboarding: welcome → sign in / provider → workspace → permissions →
//! test prompt → simple-or-advanced, then the window swaps to the main view.
//!
//! The same view, started with [`Purpose::NewInstance`], adds another apollo
//! instance from the main window: it skips the welcome and the mode choice.
//!
//! Layout lives in `views/*.crepus` (compiled in with `view_file!`); rows
//! whose count or click target depends on state are built here and handed
//! to the templates as children. There is no title bar — only a 2px
//! progress line along the top edge and a quiet `n / 6` in the footer.

use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::{Duration, Instant};

use crepuscularity_gpui::prelude::*;
use gpui::{relative, AnyElement, ClickEvent, KeyDownEvent, PathPromptOptions, SharedString};

use crate::models::{self, ModelList};
use crate::oauth::{OAuthKind, Support};
use crate::setup::{
    self, Auth, DesktopState, Instance, Mode, ProfileInfo, ProviderInfo, Scope, Secret,
    SetupChoices, WrittenSetup, OAUTH_PROVIDERS, PROFILES, PROVIDERS,
};
use crate::theme::*;

const GHOST: u32 = 0x52525b;
const SOFT: u32 = 0xa1a1aa;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Welcome,
    Provider,
    Workspace,
    Permissions,
    Test,
    Done,
}

const FIRST_RUN_STEPS: [Step; 6] = [
    Step::Welcome,
    Step::Provider,
    Step::Workspace,
    Step::Permissions,
    Step::Test,
    Step::Done,
];
const NEW_INSTANCE_STEPS: [Step; 5] = [
    Step::Provider,
    Step::Workspace,
    Step::Permissions,
    Step::Test,
    Step::Done,
];

/// Why the onboarding is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// First launch, or `--onboarding`: sets up (or re-sets up) the active
    /// instance and picks the mode.
    FirstRun,
    /// "+ new instance" from the main window.
    NewInstance,
}

/// Which text field receives typing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    None,
    ApiKey,
    BaseUrl,
    CustomName,
    Model,
    Workspace,
    Name,
    Prompt,
}

/// How the test prompt was answered — shown so a mocked reply is never
/// mistaken for a live one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeRoute {
    /// A running apollo agent (`apollo chat`) answered over its HTTP API,
    /// i.e. a full rx4 agent-loop turn.
    AgentServer,
    /// `apollo ask` in the instance's config dir: apollo loaded the config
    /// and credential just written and round-tripped the provider.
    Cli,
    /// No apollo binary or no credential — answered locally.
    OfflineMock(String),
}

impl ProbeRoute {
    pub fn label(&self) -> String {
        match self {
            ProbeRoute::AgentServer => "live · running apollo agent (rx4 loop)".into(),
            ProbeRoute::Cli => "live · apollo ask with the new config".into(),
            ProbeRoute::OfflineMock(why) => format!("offline mock · {why}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub route: ProbeRoute,
    pub ok: bool,
    pub text: String,
    pub secs: f32,
}

enum TestState {
    Idle,
    Running(Instant),
    Finished(ProbeResult),
}

/// Browser sign-in progress for the selected OAuth provider.
#[derive(Debug, Clone, PartialEq)]
enum SignIn {
    Idle,
    Waiting(OAuthKind, Instant),
    Failed(OAuthKind, String),
}

/// Where the model list's live half comes from.
enum LiveListing {
    Http(String, models::Auth),
    OAuth(OAuthKind),
}

/// Called once: `Some(state)` when an instance was saved, `None` when a
/// new-instance flow was cancelled.
type OnFinish = Box<dyn FnOnce(Option<DesktopState>, &mut Window, &mut App) + 'static>;

const OAUTH_KINDS: [OAuthKind; 3] = [OAuthKind::ChatGpt, OAuthKind::Copilot, OAuthKind::Claude];

fn kind_index(kind: OAuthKind) -> usize {
    OAUTH_KINDS.iter().position(|k| *k == kind).unwrap_or(0)
}

pub struct OnboardingView {
    pub focus: gpui::FocusHandle,
    purpose: Purpose,
    state: DesktopState,
    /// Instance being re-set-up (`--onboarding` over an existing one).
    editing: Option<String>,
    step: Step,
    field: Field,
    provider: &'static ProviderInfo,
    dropdown_open: bool,
    /// Typed while the provider dropdown is open.
    filter: String,
    api_key: Secret,
    base_url: String,
    custom_name: String,
    model: String,
    /// Model list for the selected provider (see `models.rs`).
    models: Option<ModelList>,
    models_loading: bool,
    /// Why the live listing failed, if it did.
    models_note: Option<String>,
    /// Bumped per fetch so a slow, stale answer is dropped.
    models_gen: u64,
    model_menu: bool,
    everywhere: bool,
    workspace: String,
    name: String,
    profile: &'static ProfileInfo,
    mode: Mode,
    signed_in: [bool; 3],
    sign_in: SignIn,
    prompt: String,
    test: TestState,
    written: Option<WrittenSetup>,
    error: String,
    cursor_start: Instant,
    on_finish: Option<OnFinish>,
}

impl OnboardingView {
    pub fn new(
        purpose: Purpose,
        cx: &mut Context<Self>,
        on_finish: impl FnOnce(Option<DesktopState>, &mut Window, &mut App) + 'static,
    ) -> Self {
        let state = DesktopState::load().unwrap_or_default();
        let active = state
            .active
            .as_deref()
            .and_then(|id| state.instance(id))
            .or_else(|| state.instances.first())
            .cloned();

        // Re-running first-run setup edits the active instance in place; a
        // new instance starts from its provider but gets its own folder.
        let editing = match purpose {
            Purpose::FirstRun => active.as_ref().map(|i| i.id.clone()),
            Purpose::NewInstance => None,
        };
        let provider = active
            .as_ref()
            .and_then(|i| setup::provider(&i.provider))
            .unwrap_or(&OAUTH_PROVIDERS[0]);
        let model = match (&active, purpose) {
            (Some(i), Purpose::FirstRun) if !i.model.is_empty() => i.model.clone(),
            _ => provider.default_model.to_string(),
        };
        let (everywhere, workspace, name) = match (&active, purpose) {
            (Some(i), Purpose::FirstRun) => (
                i.everywhere,
                setup::display_path(&i.workspace),
                i.name.clone(),
            ),
            _ => {
                let names: Vec<&str> = state.instances.iter().map(|i| i.name.as_str()).collect();
                let name = if names.is_empty() {
                    "apollo".to_string()
                } else {
                    (2..)
                        .map(|n| format!("apollo {n}"))
                        .find(|n| !names.contains(&n.as_str()))
                        .expect("unbounded")
                };
                let folder =
                    setup::default_workspace().with_file_name(setup::instance_id(&name, &[]));
                (false, setup::display_path(&folder), name)
            }
        };
        let profile = active
            .as_ref()
            .and_then(|i| PROFILES.iter().find(|p| p.id == i.permission_profile))
            .unwrap_or(&PROFILES[0]);
        let (base_url, custom_name) = if provider.is_custom() {
            let url = active
                .as_ref()
                .map(|i| setup::read_config(&i.config_path()))
                .and_then(|c| c["provider"]["base_url"].as_str().map(str::to_string))
                .unwrap_or_default();
            let name = active
                .as_ref()
                .and_then(|i| i.provider.strip_prefix("custom-"))
                .unwrap_or_default()
                .to_string();
            (url, name)
        } else {
            (String::new(), String::new())
        };

        // Repaint for the blinking caret, the test spinner and sign-in wait.
        cx.spawn(async move |this, cx: &mut gpui::AsyncApp| loop {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        })
        .detach();

        let mode = state.mode;
        let mut view = Self {
            focus: cx.focus_handle(),
            purpose,
            state,
            editing,
            step: match purpose {
                Purpose::FirstRun => Step::Welcome,
                Purpose::NewInstance => Step::Provider,
            },
            field: Field::None,
            provider,
            dropdown_open: false,
            filter: String::new(),
            api_key: Secret::default(),
            base_url,
            custom_name,
            model,
            models: None,
            models_loading: false,
            models_note: None,
            models_gen: 0,
            model_menu: false,
            everywhere,
            workspace,
            name,
            profile,
            mode,
            signed_in: OAUTH_KINDS.map(OAuthKind::signed_in),
            sign_in: SignIn::Idle,
            prompt: "Say hello and tell me which model you are, in one sentence.".into(),
            test: TestState::Idle,
            written: None,
            error: String::new(),
            cursor_start: Instant::now(),
            on_finish: Some(Box::new(on_finish)),
        };
        view.field = view.default_field();
        view.refresh_models(cx);
        // A flow started before this view (e.g. a cancelled new instance) is
        // still listening: show it and pick up its result.
        if let Auth::OAuth(kind) = view.provider.auth {
            if kind.in_flight() {
                view.start_sign_in(kind, cx);
            }
        }
        view
    }

    fn steps(&self) -> &'static [Step] {
        match self.purpose {
            Purpose::FirstRun => &FIRST_RUN_STEPS,
            Purpose::NewInstance => &NEW_INSTANCE_STEPS,
        }
    }

    fn step_index(&self) -> usize {
        self.steps()
            .iter()
            .position(|s| *s == self.step)
            .unwrap_or(0)
    }

    /// Text fields on the current step, in tab order.
    fn fields(&self) -> Vec<Field> {
        match self.step {
            Step::Provider => match self.provider.auth {
                Auth::OAuth(kind) if matches!(kind.support(), Support::Unsupported(_)) => vec![],
                Auth::OAuth(_) | Auth::Local => vec![Field::Model],
                Auth::ApiKey(_) => vec![Field::ApiKey, Field::Model],
                Auth::Custom => vec![
                    Field::CustomName,
                    Field::BaseUrl,
                    Field::ApiKey,
                    Field::Model,
                ],
            },
            Step::Workspace if self.everywhere => vec![Field::Name],
            Step::Workspace => vec![Field::Workspace, Field::Name],
            Step::Test => vec![Field::Prompt],
            _ => vec![],
        }
    }

    fn default_field(&self) -> Field {
        self.fields().first().copied().unwrap_or(Field::None)
    }

    fn caret(&self) -> &'static str {
        if (self.cursor_start.elapsed().as_millis() / 500).is_multiple_of(2) {
            "\u{258F}"
        } else {
            " "
        }
    }

    fn scope(&self) -> Scope {
        if self.everywhere {
            Scope::Everywhere
        } else {
            Scope::Folder(setup::expand_path(&self.workspace))
        }
    }

    fn instance_id(&self) -> String {
        match &self.editing {
            Some(id) => id.clone(),
            None => setup::instance_id(&self.name, &self.state.ids()),
        }
    }

    fn choices(&self) -> SetupChoices {
        SetupChoices {
            instance_id: self.instance_id(),
            name: self.name.trim().to_string(),
            provider: self.provider,
            api_key: self.api_key.clone(),
            base_url: self.base_url.trim().to_string(),
            custom_name: self.custom_name.trim().to_string(),
            model: self.model.clone(),
            scope: self.scope(),
            profile: self.profile,
        }
    }

    // ── Navigation ─────────────────────────────────────────────────────────

    fn go(&mut self, step: Step, cx: &mut Context<Self>) {
        self.step = step;
        self.error.clear();
        self.dropdown_open = false;
        self.model_menu = false;
        self.field = self.default_field();
        self.cursor_start = Instant::now();
        cx.notify();
    }

    fn fail(&mut self, error: impl Into<String>, field: Option<Field>, cx: &mut Context<Self>) {
        self.error = error.into();
        if let Some(field) = field {
            self.field = field;
        }
        cx.notify();
    }

    /// Validate the current step and advance. Writing the config happens on
    /// leaving Permissions, so the test step exercises the real files.
    fn advance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.step {
            Step::Welcome => self.go(Step::Provider, cx),
            Step::Provider => {
                if let Err((e, field)) = self.check_provider() {
                    return self.fail(e, field, cx);
                }
                if self.model.trim().is_empty() {
                    self.model = self.provider.default_model.to_string();
                }
                self.go(Step::Workspace, cx)
            }
            Step::Workspace => {
                if self.name.trim().is_empty() {
                    return self.fail("give this instance a name", Some(Field::Name), cx);
                }
                if !self.everywhere && self.workspace.trim().is_empty() {
                    return self.fail(
                        "choose a folder, or let apollo work everywhere",
                        Some(Field::Workspace),
                        cx,
                    );
                }
                let dir = self.choices().config_dir();
                let except = self.editing.clone().unwrap_or_default();
                if let Some(owner) = self.state.config_dir_owner(&dir, &except) {
                    let msg = format!(
                        "instance \"{}\" already lives in that folder — pick another",
                        owner.name
                    );
                    return self.fail(msg, Some(Field::Workspace), cx);
                }
                self.go(Step::Permissions, cx)
            }
            Step::Permissions => match setup::write_setup(&self.choices()) {
                Ok(written) => {
                    self.written = Some(written);
                    self.test = TestState::Idle;
                    self.go(Step::Test, cx);
                }
                Err(e) => self.fail(e, None, cx),
            },
            Step::Test => {
                if matches!(self.test, TestState::Running(_)) {
                    return;
                }
                self.go(Step::Done, cx)
            }
            Step::Done => self.finish(window, cx),
        }
    }

    fn check_provider(&self) -> Result<(), (String, Option<Field>)> {
        match self.provider.auth {
            Auth::OAuth(kind) => {
                if let Support::Unsupported(why) = kind.support() {
                    return Err((
                        format!("{} can't be used: {why}", self.provider.label),
                        None,
                    ));
                }
                if !self.signed_in[kind_index(kind)] {
                    let msg = if matches!(self.sign_in, SignIn::Waiting(k, _) if k == kind) {
                        "finish signing in in your browser first".to_string()
                    } else {
                        format!("sign in to {} first", self.provider.label)
                    };
                    return Err((msg, None));
                }
                Ok(())
            }
            Auth::Custom => {
                setup::validate_custom(&self.base_url, &self.model).map_err(|e| {
                    let field = if e.contains("model") {
                        Field::Model
                    } else {
                        Field::BaseUrl
                    };
                    (e, Some(field))
                })?;
                setup::validate_key(self.provider, &self.api_key)
                    .map_err(|e| (e, Some(Field::ApiKey)))
            }
            _ => setup::validate_key(self.provider, &self.api_key)
                .map_err(|e| (e, Some(Field::ApiKey))),
        }
    }

    fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(written) = self.written.clone() else {
            return self.fail("nothing was saved yet — go back to permissions", None, cx);
        };
        let mut state = self.state.clone();
        state.upsert(Instance::from_setup(&self.choices(), &written));
        state.onboarded = true;
        if self.purpose == Purpose::FirstRun {
            state.mode = self.mode;
        }
        if let Err(e) = state.save() {
            return self.fail(e, None, cx);
        }
        // The key is on disk now; drop the in-memory copy.
        self.api_key.clear();
        if let Some(on_finish) = self.on_finish.take() {
            on_finish(Some(state), window, cx);
        }
    }

    fn on_next(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.advance(window, cx);
    }

    fn on_back(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.test, TestState::Running(_)) {
            return;
        }
        let i = self.step_index();
        if i > 0 {
            self.go(self.steps()[i - 1], cx);
        } else if self.purpose == Purpose::NewInstance {
            self.api_key.clear();
            if let Some(on_finish) = self.on_finish.take() {
                on_finish(None, window, cx);
            }
        }
    }

    fn on_browse(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.everywhere = false;
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("use as apollo workspace".into()),
        });
        cx.spawn(async move |this, cx: &mut gpui::AsyncApp| {
            let outcome = picked.await;
            this.update(cx, |view, cx| {
                match outcome {
                    Ok(Ok(Some(paths))) if !paths.is_empty() => {
                        view.workspace = setup::display_path(&paths[0]);
                        view.error.clear();
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => {
                        view.error = format!("no folder picker available ({e}) — type a path");
                        view.field = Field::Workspace;
                    }
                    Err(_) => {
                        view.error = "no folder picker available — type a path".into();
                        view.field = Field::Workspace;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_run_test(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.run_test(cx);
    }

    fn has_credential(&self) -> bool {
        match self.provider.auth {
            Auth::OAuth(kind) => self.signed_in[kind_index(kind)],
            Auth::ApiKey(_) => !self.api_key.is_empty(),
            Auth::Local | Auth::Custom => true,
        }
    }

    fn run_test(&mut self, cx: &mut Context<Self>) {
        if matches!(self.test, TestState::Running(_)) || self.prompt.trim().is_empty() {
            return;
        }
        let Some(dir) = self.written.as_ref().map(|w| w.config_dir.clone()) else {
            return;
        };
        let prompt = self.prompt.trim().to_string();
        let has_credential = self.has_credential();
        self.test = TestState::Running(Instant::now());
        self.field = Field::None;
        cx.notify();

        let (tx, rx) = channel::<ProbeResult>();
        std::thread::spawn(move || {
            let _ = tx.send(probe(&dir, &prompt, has_credential));
        });
        cx.spawn(async move |this, cx: &mut gpui::AsyncApp| loop {
            match rx.try_recv() {
                Ok(result) => {
                    this.update(cx, |view, cx| {
                        view.test = TestState::Finished(result);
                        cx.notify();
                    })
                    .ok();
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            cx.background_executor()
                .timer(Duration::from_millis(60))
                .await;
        })
        .detach();
    }

    fn pick_provider(&mut self, provider: &'static ProviderInfo, cx: &mut Context<Self>) {
        let changed = self.provider.id != provider.id;
        if changed {
            // A key for one provider is never valid for another.
            self.api_key.clear();
            self.model = provider.default_model.to_string();
            self.models = None;
            self.models_note = None;
        }
        self.provider = provider;
        self.dropdown_open = false;
        self.model_menu = false;
        self.filter.clear();
        self.error.clear();
        self.field = self.default_field();
        if changed {
            self.refresh_models(cx);
        }
        cx.notify();
    }

    /// A sign-in card: selecting it *is* signing in. The browser opens
    /// straight away unless this login is already stored (then the card
    /// just selects it, with "sign in again" beside it).
    fn click_card(&mut self, provider: &'static ProviderInfo, cx: &mut Context<Self>) {
        self.pick_provider(provider, cx);
        if let Auth::OAuth(kind) = provider.auth {
            let waiting = matches!(self.sign_in, SignIn::Waiting(k, _) if k == kind);
            if !waiting && !self.signed_in[kind_index(kind)] {
                self.start_sign_in(kind, cx);
            }
        }
    }

    /// Where a live `/models` listing can come from for the current
    /// provider and credential, if anywhere.
    fn live_listing(&self) -> Option<LiveListing> {
        match self.provider.auth {
            Auth::OAuth(kind) if self.signed_in[kind_index(kind)] => Some(LiveListing::OAuth(kind)),
            Auth::OAuth(_) => None,
            Auth::Local => Some(LiveListing::Http(
                "http://localhost:11434/v1".into(),
                models::Auth::None,
            )),
            Auth::Custom => {
                let url = self.base_url.trim();
                if setup::validate_custom(url, "m").is_err() {
                    return None;
                }
                let key = self.api_key.expose().trim().to_string();
                let auth = if key.is_empty() {
                    models::Auth::None
                } else {
                    models::Auth::Bearer(key)
                };
                Some(LiveListing::Http(url.to_string(), auth))
            }
            Auth::ApiKey(_) => {
                let key = self.api_key.expose().trim().to_string();
                if key.is_empty() {
                    return None;
                }
                let spec = rs_ai_providers::catalog::by_id(self.provider.catalog)?;
                let (base, auth) = match spec.id {
                    "anthropic" => (spec.base_url.to_string(), models::Auth::AnthropicKey(key)),
                    // Gemini's OpenAI-compatible surface takes a Bearer key.
                    "google" => (
                        "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
                        models::Auth::Bearer(key),
                    ),
                    _ => (
                        self.provider.base_url.unwrap_or(spec.base_url).to_string(),
                        models::Auth::Bearer(key),
                    ),
                };
                Some(LiveListing::Http(base, auth))
            }
        }
    }

    /// Fetch the model list on a worker thread: live listing when there is
    /// a credential, else models.dev, else the built-in catalog.
    fn refresh_models(&mut self, cx: &mut Context<Self>) {
        let cache_as = match self.provider.auth {
            Auth::Custom => self.choices().provider_name(),
            _ if self.provider.catalog.is_empty() => self.provider.id.to_string(),
            _ => self.provider.catalog.to_string(),
        };
        if self.models.is_none() {
            self.models = models::cached(&cache_as);
        }
        let live = self.live_listing();
        let current = self.model.trim().to_string();
        self.models_gen += 1;
        let gen = self.models_gen;
        self.models_loading = true;
        cx.notify();
        let (tx, rx) = channel::<(ModelList, Option<String>)>();
        std::thread::spawn(move || {
            let live = match live {
                Some(LiveListing::Http(url, auth)) => Some((url, auth)),
                Some(LiveListing::OAuth(kind)) => kind.access_token().map(|t| {
                    let auth = match kind {
                        OAuthKind::Claude => models::Auth::ClaudeLogin(t),
                        _ => models::Auth::OAuthPlan(kind.provider(), t),
                    };
                    ("https://api.anthropic.com/v1".to_string(), auth)
                }),
                None => None,
            };
            let extra: Vec<&str> = [current.as_str()]
                .into_iter()
                .filter(|m| !m.is_empty())
                .collect();
            let out = models::resolve(
                &cache_as,
                live.as_ref().map(|(u, a)| (u.as_str(), a.clone())),
                &extra,
            );
            let _ = tx.send(out);
        });
        cx.spawn(async move |this, cx: &mut gpui::AsyncApp| loop {
            match rx.try_recv() {
                Ok((list, note)) => {
                    this.update(cx, |view, cx| {
                        if view.models_gen == gen {
                            view.models_loading = false;
                            view.models_note = note;
                            if view.model.trim().is_empty() {
                                if let Some(first) = list.models.first() {
                                    view.model = first.clone();
                                }
                            }
                            view.models = Some(list);
                            cx.notify();
                        }
                    })
                    .ok();
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
        })
        .detach();
    }

    /// Providers shown in the open dropdown, narrowed by what was typed.
    fn filtered(&self) -> Vec<(usize, &'static ProviderInfo)> {
        let q = self.filter.trim().to_ascii_lowercase();
        PROVIDERS
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.is_custom())
            .filter(|(_, p)| {
                q.is_empty()
                    || p.label.to_ascii_lowercase().contains(&q)
                    || p.catalog.contains(&q)
                    || p.id.contains(&q)
                    || p.blurb.contains(&q)
            })
            .collect()
    }

    /// Start the browser sign-in for `kind` on a worker thread. The flow
    /// blocks until the redirect arrives or its own 3-minute timeout; the
    /// UI only learns the outcome, never a token.
    fn start_sign_in(&mut self, kind: OAuthKind, cx: &mut Context<Self>) {
        if matches!(kind.support(), Support::Unsupported(_)) {
            return;
        }
        self.sign_in = SignIn::Waiting(kind, Instant::now());
        self.error.clear();
        cx.notify();
        let (tx, rx) = channel::<Result<(), String>>();
        if kind.claim() {
            std::thread::spawn(move || {
                let _ = tx.send(kind.sign_in());
            });
        } else {
            // An earlier attempt is still listening on the callback port;
            // wait for it rather than starting a second listener.
            std::thread::spawn(move || {
                while kind.in_flight() {
                    std::thread::sleep(Duration::from_millis(300));
                }
                let _ = tx.send(if kind.signed_in() {
                    Ok(())
                } else {
                    Err("sign-in did not complete".into())
                });
            });
        }
        cx.spawn(async move |this, cx: &mut gpui::AsyncApp| loop {
            match rx.try_recv() {
                Ok(outcome) => {
                    this.update(cx, |view, cx| view.sign_in_finished(kind, outcome, cx))
                        .ok();
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    this.update(cx, |view, cx| {
                        view.sign_in_finished(kind, Err("sign-in stopped".into()), cx)
                    })
                    .ok();
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
        })
        .detach();
    }

    fn sign_in_finished(
        &mut self,
        kind: OAuthKind,
        outcome: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        let i = kind_index(kind);
        // Whatever the UI showed, the store is the truth.
        self.signed_in[i] = kind.signed_in();
        let watching = matches!(self.sign_in, SignIn::Waiting(k, _) if k == kind);
        if watching {
            self.sign_in = match outcome {
                Ok(()) => SignIn::Idle,
                Err(e) => SignIn::Failed(kind, e),
            };
        }
        // Signed in: the plan's own model list replaces the offline one.
        if self.signed_in[i] && self.provider.auth == Auth::OAuth(kind) {
            self.refresh_models(cx);
        }
        cx.notify();
    }

    fn cancel_sign_in(&mut self, cx: &mut Context<Self>) {
        self.sign_in = SignIn::Idle;
        cx.notify();
    }

    fn pick_profile(&mut self, profile: &'static ProfileInfo, cx: &mut Context<Self>) {
        self.profile = profile;
        cx.notify();
    }

    fn set_everywhere(&mut self, everywhere: bool, cx: &mut Context<Self>) {
        self.everywhere = everywhere;
        self.error.clear();
        self.field = self.default_field();
        cx.notify();
    }

    // ── Keyboard ───────────────────────────────────────────────────────────

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        let key = stroke.key.as_str();

        if stroke.modifiers.platform || stroke.modifiers.control {
            if key == "v" {
                if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    self.insert(text.trim(), cx);
                    // A pasted key or URL is the moment a live listing works.
                    if matches!(self.field, Field::ApiKey | Field::BaseUrl) && !self.dropdown_open {
                        self.refresh_models(cx);
                    }
                }
            }
            return;
        }

        if self.dropdown_open {
            match key {
                "escape" => {
                    self.dropdown_open = false;
                    self.filter.clear();
                }
                "enter" => {
                    if let Some((_, p)) = self.filtered().first().copied() {
                        self.pick_provider(p, cx);
                    }
                }
                "backspace" => {
                    self.filter.pop();
                }
                "space" => self.filter.push(' '),
                _ => {
                    if let Some(ch) = stroke.key_char.as_deref() {
                        if !ch.chars().any(char::is_control) {
                            self.filter.push_str(ch);
                        }
                    }
                }
            }
            cx.notify();
            return;
        }

        match key {
            "enter" => {
                if self.step == Step::Test && self.field == Field::Prompt {
                    self.run_test(cx);
                } else {
                    self.advance(window, cx);
                }
            }
            "tab" => {
                let fields = self.fields();
                if matches!(self.field, Field::ApiKey | Field::BaseUrl) {
                    self.refresh_models(cx);
                }
                if !fields.is_empty() {
                    let at = fields.iter().position(|f| *f == self.field);
                    self.field = fields[at.map(|i| (i + 1) % fields.len()).unwrap_or(0)];
                    self.cursor_start = Instant::now();
                    cx.notify();
                }
            }
            "backspace" => {
                match self.field {
                    Field::ApiKey => self.api_key.pop(),
                    Field::BaseUrl => {
                        self.base_url.pop();
                    }
                    Field::CustomName => {
                        self.custom_name.pop();
                    }
                    Field::Model => {
                        self.model.pop();
                    }
                    Field::Workspace => {
                        self.workspace.pop();
                    }
                    Field::Name => {
                        self.name.pop();
                    }
                    Field::Prompt => {
                        self.prompt.pop();
                    }
                    Field::None => {}
                }
                cx.notify();
            }
            "escape" => {
                match self.field {
                    Field::ApiKey => self.api_key.clear(),
                    Field::BaseUrl => self.base_url.clear(),
                    Field::CustomName => self.custom_name.clear(),
                    Field::Model => self.model.clear(),
                    Field::Workspace => self.workspace.clear(),
                    Field::Name => self.name.clear(),
                    Field::Prompt => self.prompt.clear(),
                    Field::None => {}
                }
                cx.notify();
            }
            "space" => self.insert(" ", cx),
            _ => {
                if let Some(ch) = stroke.key_char.as_deref() {
                    if !ch.is_empty() && !ch.chars().any(char::is_control) {
                        self.insert(ch, cx);
                    }
                }
            }
        }
    }

    fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        let compact = || {
            text.chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
        };
        match self.field {
            // Whitespace never belongs in a key or URL; a paste often carries some.
            Field::ApiKey => self.api_key.push_str(&compact()),
            Field::BaseUrl => self.base_url.push_str(&compact()),
            Field::CustomName => self.custom_name.push_str(&text),
            Field::Model => self.model.push_str(text.trim()),
            Field::Workspace => self.workspace.push_str(&text),
            Field::Name => self.name.push_str(&text),
            Field::Prompt => self.prompt.push_str(&text),
            Field::None => return,
        }
        self.error.clear();
        cx.notify();
    }

    // ── Rust-built pieces handed to the templates ─────────────────────────

    /// A 2px line along the top edge; the only progress indicator.
    fn progress(&self) -> AnyElement {
        let frac = (self.step_index() + 1) as f32 / self.steps().len() as f32;
        div()
            .w_full()
            .h(px(2.))
            .flex_shrink_0()
            .bg(rgb(SURFACE))
            .child(div().h_full().w(relative(frac)).bg(rgb(MUTED)))
            .into_any_element()
    }

    fn text_field(
        &self,
        id: &'static str,
        field: Field,
        shown: String,
        placeholder: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focused = self.field == field;
        let empty = shown.is_empty();
        let text = if empty && !focused {
            placeholder.to_string()
        } else if focused {
            format!("{shown}{}", self.caret())
        } else {
            shown
        };
        div()
            .id(id)
            .w_full()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(if focused { MUTED } else { SURFACE_2 }))
            .bg(rgb(if focused { SURFACE } else { BG }))
            .text_sm()
            .text_color(rgb(if empty && !focused { GHOST } else { TEXT }))
            .cursor_text()
            .child(SharedString::from(text))
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                view.field = field;
                view.dropdown_open = false;
                view.model_menu = false;
                view.cursor_start = Instant::now();
                cx.notify();
            }))
            .into_any_element()
    }

    fn labelled(
        label: impl Into<SharedString>,
        field: AnyElement,
        note: Option<String>,
    ) -> AnyElement {
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(div().text_xs().text_color(rgb(MUTED)).child(label.into()))
            .child(field)
            .children(note.map(|n| {
                div()
                    .text_xs()
                    .text_color(rgb(GHOST))
                    .child(SharedString::from(n))
            }))
            .into_any_element()
    }

    /// Status line under a pinned sign-in card.
    fn oauth_status(&self, kind: OAuthKind) -> (String, u32) {
        let i = kind_index(kind);
        if let SignIn::Waiting(k, _) = self.sign_in {
            if k == kind {
                return ("waiting for browser…".into(), WARN);
            }
        }
        match kind.support() {
            Support::Unsupported(_) => ("not supported by apollo".into(), GHOST),
            Support::NeedsFeature(_) if self.signed_in[i] => {
                ("✓ signed in · needs build flag".into(), WARN)
            }
            Support::NeedsFeature(_) => ("click to sign in · needs build flag".into(), WARN),
            Support::Live if self.signed_in[i] => ("✓ signed in".into(), SUCCESS),
            Support::Live => ("click to sign in".into(), MUTED),
        }
    }

    fn pinned(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .w_full()
            .flex()
            .flex_row()
            .gap_2()
            .children(OAUTH_PROVIDERS.iter().enumerate().map(|(i, p)| {
                let selected = p.id == self.provider.id;
                let Auth::OAuth(kind) = p.auth else {
                    unreachable!("pinned providers are oauth")
                };
                let unsupported = matches!(kind.support(), Support::Unsupported(_));
                let (status, color) = self.oauth_status(kind);
                div()
                    .id(("oauth", i))
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(if selected { MUTED } else { SURFACE_2 }))
                    .bg(rgb(if selected { SURFACE } else { BG }))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(SURFACE)))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(if unsupported {
                                MUTED
                            } else if selected {
                                ACCENT
                            } else {
                                TEXT
                            }))
                            .child(p.label),
                    )
                    .child(div().text_xs().text_color(rgb(GHOST)).child(p.blurb))
                    .child(
                        div()
                            .pt_1()
                            .text_xs()
                            .text_color(rgb(color))
                            .child(SharedString::from(status)),
                    )
                    .on_click(
                        cx.listener(move |view, _: &ClickEvent, _, cx| view.click_card(p, cx)),
                    )
                    .into_any_element()
            }))
            .into_any_element()
    }

    fn separator(text: &'static str) -> AnyElement {
        let line = || div().flex_1().h(px(1.)).bg(rgb(SURFACE_2));
        div()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .child(line())
            .child(div().text_xs().text_color(rgb(GHOST)).child(text))
            .child(line())
            .into_any_element()
    }

    fn provider_row(
        &self,
        id: (&'static str, usize),
        p: &'static ProviderInfo,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let selected = p.id == self.provider.id;
        let right = match p.auth {
            Auth::ApiKey(var) if var == setup::CUSTOM_KEY_VAR => "api key".to_string(),
            Auth::ApiKey(var) => var.to_string(),
            Auth::Local => "no key".into(),
            Auth::Custom => "never oauth".into(),
            Auth::OAuth(_) => String::new(),
        };
        div()
            .id(id)
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_3()
            .py(px(6.))
            .cursor_pointer()
            .bg(rgb(if selected { SURFACE_2 } else { SURFACE }))
            .hover(|s| s.bg(rgb(SURFACE_2)))
            .child(
                div()
                    .w(px(190.))
                    .text_sm()
                    .text_color(rgb(if selected { ACCENT } else { TEXT }))
                    .child(p.label),
            )
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(p.blurb),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(GHOST))
                    .child(SharedString::from(right)),
            )
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| view.pick_provider(p, cx)))
    }

    fn dropdown(&self, cx: &mut Context<Self>) -> AnyElement {
        let chosen = PROVIDERS.iter().find(|p| p.id == self.provider.id);
        let trigger_label = if self.dropdown_open {
            if self.filter.is_empty() {
                format!("type to filter {} providers…", PROVIDERS.len() - 1)
            } else {
                format!("{}{}", self.filter, self.caret())
            }
        } else {
            match chosen {
                Some(p) => format!("{} · {}", p.label, p.blurb),
                None => "api key, local model or custom endpoint".to_string(),
            }
        };
        let trigger = div()
            .id("provider-dropdown")
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(if chosen.is_some() || self.dropdown_open {
                MUTED
            } else {
                SURFACE_2
            }))
            .bg(rgb(if chosen.is_some() { SURFACE } else { BG }))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(SURFACE)))
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(rgb(if self.dropdown_open && !self.filter.is_empty() {
                        TEXT
                    } else if chosen.is_some() && !self.dropdown_open {
                        ACCENT
                    } else {
                        SOFT
                    }))
                    .child(SharedString::from(trigger_label)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(if self.dropdown_open { "▴" } else { "▾" }),
            )
            .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                view.dropdown_open = !view.dropdown_open;
                view.filter.clear();
                view.model_menu = false;
                cx.notify();
            }));

        let list = self.dropdown_open.then(|| {
            let rows = self.filtered();
            let empty = rows.is_empty();
            let custom = PROVIDERS
                .iter()
                .find(|p| p.is_custom())
                .expect("custom endpoint is always offered");
            div()
                .w_full()
                .flex()
                .flex_col()
                .rounded_md()
                .border_1()
                .border_color(rgb(SURFACE_2))
                .bg(rgb(SURFACE))
                .child(
                    div()
                        .id("provider-list")
                        .w_full()
                        .max_h(px(330.))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .py_1()
                        .children(
                            rows.into_iter()
                                .map(|(i, p)| self.provider_row(("provider-option", i), p, cx)),
                        )
                        .when(empty, |d| {
                            d.child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .text_xs()
                                    .text_color(rgb(GHOST))
                                    .child("no provider matches — use a custom endpoint"),
                            )
                        }),
                )
                .child(div().w_full().h(px(1.)).bg(rgb(SURFACE_2)))
                .child(self.provider_row(("provider-custom", 0), custom, cx))
        });

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .child(trigger)
            .children(list)
            .into_any_element()
    }

    fn model_field(&self, cx: &mut Context<Self>) -> AnyElement {
        let placeholder = if self.provider.default_model.is_empty() {
            "pick from the list or type a model id".to_string()
        } else {
            self.provider.default_model.to_string()
        };
        let field = self.text_field("model", Field::Model, self.model.clone(), &placeholder, cx);
        let count = self.models.as_ref().map_or(0, |m| m.models.len());
        let picker_label = if self.models_loading {
            "fetching…".to_string()
        } else if count == 0 {
            "no list".to_string()
        } else {
            format!("{count} models {}", if self.model_menu { "▴" } else { "▾" })
        };
        let picker = div()
            .id("model-list-toggle")
            .flex_shrink_0()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(if self.model_menu { MUTED } else { SURFACE_2 }))
            .text_xs()
            .text_color(rgb(if count > 0 { TEXT } else { GHOST }))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(SURFACE)))
            .child(SharedString::from(picker_label))
            .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                if view.models.as_ref().is_some_and(|m| !m.models.is_empty()) {
                    view.model_menu = !view.model_menu;
                    view.dropdown_open = false;
                }
                cx.notify();
            }));
        let refresh = div()
            .id("model-refresh")
            .flex_shrink_0()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(SURFACE_2))
            .text_xs()
            .text_color(rgb(SOFT))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(SURFACE)))
            .child("↻")
            .on_click(cx.listener(|view, _: &ClickEvent, _, cx| view.refresh_models(cx)));
        let row = div()
            .w_full()
            .flex()
            .flex_row()
            .gap_2()
            .child(div().flex_1().min_w(px(0.)).child(field))
            .child(picker)
            .child(refresh);

        let menu = (self.model_menu && count > 0).then(|| {
            let models = self
                .models
                .as_ref()
                .map(|m| m.models.clone())
                .unwrap_or_default();
            div()
                .id("model-list")
                .w_full()
                .max_h(px(190.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(rgb(SURFACE_2))
                .bg(rgb(SURFACE))
                .children(models.into_iter().enumerate().map(|(i, m)| {
                    let selected = m == self.model.trim();
                    let pick = m.clone();
                    div()
                        .id(("model-option", i))
                        .w_full()
                        .px_3()
                        .py(px(5.))
                        .text_sm()
                        .text_color(rgb(if selected { ACCENT } else { TEXT }))
                        .bg(rgb(if selected { SURFACE_2 } else { SURFACE }))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(SURFACE_2)))
                        .child(SharedString::from(m))
                        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                            view.model = pick.clone();
                            view.model_menu = false;
                            view.error.clear();
                            cx.notify();
                        }))
                }))
        });

        let note = match (&self.models, &self.models_note) {
            _ if self.models_loading => "looking up models…".to_string(),
            (Some(list), Some(why)) => {
                format!(
                    "list from {} · live /models failed: {why}",
                    list.source.label()
                )
            }
            (Some(list), None) => {
                format!("list from {} · or type any model id", list.source.label())
            }
            (None, _) => "type a model id".to_string(),
        };
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(div().text_xs().text_color(rgb(MUTED)).child("model"))
            .child(row)
            .children(menu)
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(GHOST))
                    .child(SharedString::from(note)),
            )
            .into_any_element()
    }

    fn key_field(&self, cx: &mut Context<Self>) -> AnyElement {
        let var = self.provider.env_var().unwrap_or_default();
        let optional = self.provider.is_custom();
        let field = self.text_field(
            "api-key",
            Field::ApiKey,
            self.api_key.masked(),
            if optional {
                "leave empty if the endpoint needs none"
            } else {
                "paste with ctrl+v"
            },
            cx,
        );
        let label = if optional {
            format!("api key · optional · {var}")
        } else {
            format!("api key · {var}")
        };
        Self::labelled(
            label,
            field,
            Some("saved to the instance's .env, owner-only — never apollo.json".into()),
        )
    }

    fn button(
        id: &'static str,
        label: &'static str,
        primary: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        div()
            .id(id)
            .flex_shrink_0()
            .px_4()
            .py_2()
            .rounded_md()
            .text_sm()
            .cursor_pointer()
            .when(primary, |d| {
                d.bg(rgb(ACCENT))
                    .text_color(rgb(BG))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .hover(|s| s.bg(rgb(TEXT)))
            })
            .when(!primary, |d| {
                d.border_1()
                    .border_color(rgb(SURFACE_2))
                    .text_color(rgb(TEXT))
                    .hover(|s| s.bg(rgb(SURFACE)))
            })
            .child(label)
            .on_click(on_click)
            .into_any_element()
    }

    /// Below the chooser: whatever the selected provider needs.
    fn provider_details(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.dropdown_open {
            return div().into_any_element();
        }
        let col = || div().w_full().flex().flex_col().gap_3();
        match self.provider.auth {
            Auth::OAuth(kind) => {
                let i = kind_index(kind);
                let port = kind.callback_port();
                let (line, color) = match (&self.sign_in, kind.support()) {
                    (_, Support::Unsupported(why)) => (why.to_string(), MUTED),
                    (SignIn::Waiting(k, since), _) if *k == kind => (
                        format!(
                            "waiting for the browser · localhost:{port} · {}s / 180s",
                            since.elapsed().as_secs()
                        ),
                        WARN,
                    ),
                    (SignIn::Failed(k, e), _) if *k == kind => (e.clone(), DANGER),
                    (_, Support::NeedsFeature(feature)) if self.signed_in[i] => (
                        format!(
                            "signed in. this apollo build can't run it yet — rebuild with \
                             `--features {feature}`"
                        ),
                        WARN,
                    ),
                    (_, Support::NeedsFeature(feature)) => (
                        format!(
                            "sign-in works and is saved, but apollo needs `--features {feature}` \
                             to use it"
                        ),
                        WARN,
                    ),
                    (_, Support::Live) if self.signed_in[i] => (
                        "signed in — apollo uses this login directly, no key needed".into(),
                        SUCCESS,
                    ),
                    (_, Support::Live) => (
                        format!("opens your browser; the login returns to localhost:{port}"),
                        SOFT,
                    ),
                };
                let waiting = matches!(self.sign_in, SignIn::Waiting(k, _) if k == kind);
                let actions = match kind.support() {
                    Support::Unsupported(_) => None,
                    _ if waiting => Some(Self::button(
                        "oauth-cancel",
                        "cancel",
                        false,
                        cx.listener(|view, _: &ClickEvent, _, cx| view.cancel_sign_in(cx)),
                    )),
                    _ => Some(Self::button(
                        "oauth-sign-in",
                        if self.signed_in[i] {
                            "sign in again"
                        } else {
                            "sign in with browser"
                        },
                        !self.signed_in[i],
                        cx.listener(move |view, _: &ClickEvent, _, cx| {
                            view.start_sign_in(kind, cx)
                        }),
                    )),
                };
                let panel = div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_4()
                    .p_4()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(SURFACE_2))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(GHOST))
                                    .child("~/.config/rs_ai/credentials · shared with apollo"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(color))
                                    .child(SharedString::from(line)),
                            ),
                    )
                    .children(actions);
                let unsupported = matches!(kind.support(), Support::Unsupported(_));
                col()
                    .child(panel)
                    .when(!unsupported, |d| d.child(self.model_field(cx)))
                    .into_any_element()
            }
            Auth::ApiKey(_) => col()
                .child(self.key_field(cx))
                .child(self.model_field(cx))
                .into_any_element(),
            Auth::Local => col()
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(SOFT))
                        .child("no key — apollo talks to ollama at http://localhost:11434"),
                )
                .child(self.model_field(cx))
                .into_any_element(),
            Auth::Custom => {
                let name = self.text_field(
                    "custom-name",
                    Field::CustomName,
                    self.custom_name.clone(),
                    "e.g. work gateway",
                    cx,
                );
                let url = self.text_field(
                    "base-url",
                    Field::BaseUrl,
                    self.base_url.clone(),
                    "https://api.example.com/v1",
                    cx,
                );
                col()
                    .child(Self::labelled(
                        "name",
                        name,
                        Some(format!(
                            "saved as provider \"{}\"",
                            self.choices().provider_name()
                        )),
                    ))
                    .child(Self::labelled(
                        "base url · openai-compatible (/chat/completions, /models)",
                        url,
                        None,
                    ))
                    .child(self.key_field(cx))
                    .child(self.model_field(cx))
                    .into_any_element()
            }
        }
    }

    fn option_card(
        id: &'static str,
        selected: bool,
        title: &'static str,
        detail: String,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        div()
            .id(id)
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .gap_1()
            .p_4()
            .rounded_md()
            .border_1()
            .border_color(rgb(if selected { MUTED } else { SURFACE_2 }))
            .bg(rgb(if selected { SURFACE } else { BG }))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(SURFACE)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(if selected { ACCENT } else { GHOST }))
                            .child(if selected { "●" } else { "○" }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(if selected { ACCENT } else { TEXT }))
                            .child(title),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(SharedString::from(detail)),
            )
            .on_click(on_click)
            .into_any_element()
    }

    fn scope_cards(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .w_full()
            .flex()
            .flex_row()
            .gap_2()
            .child(Self::option_card(
                "scope-folder",
                !self.everywhere,
                "one folder",
                "a project folder. apollo.json and .env live in it, like `apollo init`.".into(),
                cx.listener(|view, _: &ClickEvent, _, cx| view.set_everywhere(false, cx)),
            ))
            .child(Self::option_card(
                "scope-everywhere",
                self.everywhere,
                "works everywhere",
                "no single folder. works from your home directory; config is kept in \
                 ~/.apollo/instances/."
                    .into(),
                cx.listener(|view, _: &ClickEvent, _, cx| view.set_everywhere(true, cx)),
            ))
            .into_any_element()
    }

    fn scope_details(&self) -> Vec<AnyElement> {
        let dir = self.choices().config_dir();
        let shown = setup::display_path(&dir);
        let row = |path: String, what: String| {
            div()
                .w_full()
                .flex()
                .flex_row()
                .gap_4()
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .text_sm()
                        .text_color(rgb(TEXT))
                        .child(SharedString::from(path)),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(SharedString::from(what)),
                )
                .into_any_element()
        };
        let mut rows = vec![div()
            .pb_1()
            .text_xs()
            .text_color(rgb(GHOST))
            .child("apollo will write")
            .into_any_element()];
        rows.push(row(
            format!("{shown}/apollo.json"),
            "provider, model, policy".into(),
        ));
        rows.push(match self.provider.auth {
            Auth::OAuth(_) => row(
                "~/.config/rs_ai/credentials".into(),
                "the sign-in (already saved)".into(),
            ),
            Auth::Local => row(format!("{shown}/.env"), "nothing secret".into()),
            _ => row(
                format!("{shown}/.env"),
                format!("{} · 0600", self.provider.env_var().unwrap_or_default()),
            ),
        });
        if self.everywhere {
            rows.push(row(
                format!(
                    "{}  (workspace)",
                    setup::display_path(&self.choices().workspace())
                ),
                "reads + writes anywhere under it".into(),
            ));
        } else {
            rows.push(row(format!("{shown}/.apollo/"), "memory + sessions".into()));
        }
        rows
    }

    fn profile_list(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .children(PROFILES.iter().enumerate().map(|(i, p)| {
                let selected = p.id == self.profile.id;
                div()
                    .id(("profile", i))
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(if selected { MUTED } else { SURFACE_2 }))
                    .bg(rgb(if selected { SURFACE } else { BG }))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(SURFACE)))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(if selected { ACCENT } else { GHOST }))
                            .child(if selected { "●" } else { "○" }),
                    )
                    .child(
                        div()
                            .w(px(110.))
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(if selected { ACCENT } else { TEXT }))
                            .child(p.label),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(p.detail),
                    )
                    .on_click(
                        cx.listener(move |view, _: &ClickEvent, _, cx| view.pick_profile(p, cx)),
                    )
                    .into_any_element()
            }))
            .into_any_element()
    }

    fn test_transcript(&self) -> AnyElement {
        let line = |who: &'static str, color: u32, text: String| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_xs().text_color(rgb(GHOST)).child(who))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(color))
                        .child(SharedString::from(text)),
                )
        };
        match &self.test {
            TestState::Idle => div()
                .text_sm()
                .text_color(rgb(GHOST))
                .child("press run (or enter) to send the prompt")
                .into_any_element(),
            TestState::Running(start) => {
                const FRAMES: [&str; 10] = [
                    "\u{280B}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283C}", "\u{2834}",
                    "\u{2826}", "\u{2827}", "\u{2807}", "\u{280F}",
                ];
                let frame = FRAMES[(start.elapsed().as_millis() / 100) as usize % FRAMES.len()];
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(line("you", TEXT, self.prompt.clone()))
                    .child(line("apollo", WARN, format!("{frame} thinking…")))
                    .into_any_element()
            }
            TestState::Finished(result) => div()
                .flex()
                .flex_col()
                .gap_3()
                .child(line("you", TEXT, self.prompt.clone()))
                .child(line(
                    if result.ok { "apollo" } else { "error" },
                    if result.ok { ACCENT } else { DANGER },
                    result.text.clone(),
                ))
                .into_any_element(),
        }
    }

    fn summary(&self) -> AnyElement {
        let row = |k: &'static str, v: String| {
            div()
                .flex()
                .flex_row()
                .gap_3()
                .child(div().w(px(110.)).text_xs().text_color(rgb(MUTED)).child(k))
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(rgb(TEXT))
                        .child(SharedString::from(v)),
                )
        };
        let auth = match (self.provider.auth, &self.written) {
            (Auth::OAuth(_), _) => "browser sign-in (shared rs_ai store)".to_string(),
            (Auth::Local, _) => "not needed".to_string(),
            (_, Some(w)) => match (&w.env_path, self.api_key.is_empty()) {
                (Some(p), false) => format!(
                    "{} in {} (0600)",
                    self.provider.env_var().unwrap_or_default(),
                    setup::display_path(p)
                ),
                _ => "none".to_string(),
            },
            (_, None) => "not written yet".to_string(),
        };
        let test = match &self.test {
            TestState::Finished(r) if r.ok => format!("passed · {}", r.route.label()),
            TestState::Finished(r) => format!("failed · {}", r.route.label()),
            _ => "skipped".to_string(),
        };
        let provider = if self.provider.is_custom() {
            format!("custom · {}", self.base_url.trim())
        } else {
            self.provider.label.to_string()
        };
        let scope = if self.everywhere {
            "everywhere (~)".to_string()
        } else {
            setup::display_path(&setup::expand_path(&self.workspace))
        };
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(row("instance", self.name.trim().to_string()))
            .child(row("provider", provider))
            .child(row("model", self.choices().resolved_model()))
            .child(row("credential", auth))
            .child(row("works in", scope))
            .child(row(
                "config",
                self.written
                    .as_ref()
                    .map(|w| setup::display_path(&w.config_path))
                    .unwrap_or_else(|| "not written yet".into()),
            ))
            .child(row("permissions", self.profile.label.to_string()))
            .child(row("test prompt", test))
            .into_any_element()
    }

    fn mode_cards(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.purpose != Purpose::FirstRun {
            return div().into_any_element();
        }
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("how much app do you want?"),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(Self::option_card(
                        "mode-simple",
                        self.mode == Mode::Simple,
                        "simple",
                        "just the chat and an instance switcher.".into(),
                        cx.listener(|view, _: &ClickEvent, _, cx| {
                            view.mode = Mode::Simple;
                            cx.notify();
                        }),
                    ))
                    .child(Self::option_card(
                        "mode-advanced",
                        self.mode == Mode::Advanced,
                        "advanced",
                        "instance roster, tools + permissions, model params and logs.".into(),
                        cx.listener(|view, _: &ClickEvent, _, cx| {
                            view.mode = Mode::Advanced;
                            cx.notify();
                        }),
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(GHOST))
                    .child("switch any time in settings"),
            )
            .into_any_element()
    }
}

impl Render for OnboardingView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let step = self.step;
        let first = self.step_index() == 0;
        let counter = format!("{} / {}", self.step_index() + 1, self.steps().len());
        let progress = self.progress();
        let can_back = step != Step::Done && (!first || self.purpose == Purpose::NewInstance);
        let back_label = if first { "cancel" } else { "back" };
        let error = self.error.clone();
        let running = matches!(self.test, TestState::Running(_));
        let new_instance = self.purpose == Purpose::NewInstance;

        let (title, lede, hint, next_label): (&str, String, &str, &str) = match step {
            Step::Welcome => (
                "meet apollo",
                "a local-first agent on the rotary (rx4) engine. a few screens and it is ready."
                    .into(),
                "enter to continue",
                "get started",
            ),
            Step::Provider => (
                if new_instance {
                    "new instance · model"
                } else {
                    "connect a model"
                },
                "sign in with an account, or pick a key-based provider below.".into(),
                "tab · ctrl+v",
                "continue",
            ),
            Step::Workspace => (
                "where should it work?",
                "one folder keeps apollo's files, memory and tools inside it. \
                 everywhere lets it work across your home directory."
                    .into(),
                "",
                "continue",
            ),
            Step::Permissions => (
                "what may it do on its own?",
                "continuing writes the config. change it later in settings.".into(),
                "",
                "save and continue",
            ),
            Step::Test => (
                "say hello",
                "one prompt through apollo with the config just written. \
                 without a binary or credential it falls back to a marked offline mock."
                    .into(),
                if running { "waiting…" } else { "" },
                match &self.test {
                    TestState::Running(_) => "…",
                    TestState::Finished(r) if r.ok => "continue",
                    TestState::Finished(_) => "continue anyway",
                    _ => "skip",
                },
            ),
            Step::Done => (
                if new_instance {
                    "instance ready"
                } else {
                    "ready"
                },
                "everything below is on disk.".into(),
                "",
                if new_instance {
                    "open it"
                } else {
                    "open apollo"
                },
            ),
        };

        let body: AnyElement = match step {
            Step::Welcome => {
                let engine_line = format!(
                    "apollo-ui {} · crepuscularity + gpui · engine rx4",
                    env!("CARGO_PKG_VERSION")
                );
                view_file!("views/welcome.crepus").into_any_element()
            }
            Step::Provider => {
                let pinned = self.pinned(cx);
                let separator = Self::separator("or");
                let dropdown = self.dropdown(cx);
                let details = self.provider_details(cx);
                view_file!("views/provider.crepus").into_any_element()
            }
            Step::Workspace => {
                let scope_cards = self.scope_cards(cx);
                let folder = (!self.everywhere).then(|| {
                    let ws = setup::expand_path(&self.workspace);
                    let note = if ws.join("apollo.json").is_file() {
                        "has an apollo.json — it will be updated, not replaced"
                    } else if ws.is_dir() {
                        "existing folder"
                    } else {
                        "new folder — created on save"
                    };
                    let field = self.text_field(
                        "workspace",
                        Field::Workspace,
                        self.workspace.clone(),
                        "~/apollo",
                        cx,
                    );
                    let row = div()
                        .w_full()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(div().flex_1().child(field))
                        .child(Self::button(
                            "browse",
                            "browse…",
                            false,
                            cx.listener(Self::on_browse),
                        ))
                        .into_any_element();
                    Self::labelled("folder", row, Some(note.into()))
                });
                let name_field = Self::labelled(
                    "instance name",
                    self.text_field("name", Field::Name, self.name.clone(), "apollo", cx),
                    Some(format!("id: {}", self.instance_id())),
                );
                let preview = div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_4()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(SURFACE_2))
                    .children(self.scope_details())
                    .into_any_element();
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(scope_cards)
                    .children(folder)
                    .child(name_field)
                    .child(preview)
                    .into_any_element()
            }
            Step::Permissions => {
                let profile_list = self.profile_list(cx);
                view_file!("views/permissions.crepus").into_any_element()
            }
            Step::Test => {
                let prompt_field = self.text_field(
                    "prompt",
                    Field::Prompt,
                    self.prompt.clone(),
                    "type a prompt",
                    cx,
                );
                let run_label = if running { "running…" } else { "run" };
                let (route_label, elapsed) = match &self.test {
                    TestState::Idle => ("not run yet".to_string(), String::new()),
                    TestState::Running(start) => (
                        "sending…".to_string(),
                        format!("{:.1}s", start.elapsed().as_secs_f32()),
                    ),
                    TestState::Finished(r) => (r.route.label(), format!("{:.1}s", r.secs)),
                };
                let transcript = self.test_transcript();
                view_file!("views/test.crepus").into_any_element()
            }
            Step::Done => {
                let summary = self.summary();
                let mode_cards = self.mode_cards(cx);
                view_file!("views/done.crepus").into_any_element()
            }
        };

        view_file!("views/onboarding.crepus").track_focus(&self.focus)
    }
}

// ── Test prompt ─────────────────────────────────────────────────────────────

/// Answer the test prompt through the most real path available.
///
/// 1. a running apollo agent (full rx4 turn over its HTTP API);
/// 2. `apollo ask` run in the new workspace, so it loads the apollo.json and
///    .env the onboarding just wrote;
/// 3. an offline mock, labelled as such, when neither is possible.
pub fn probe(workspace: &std::path::Path, prompt: &str, has_credential: bool) -> ProbeResult {
    let start = Instant::now();
    let done = |route, ok, text: String| ProbeResult {
        route,
        ok,
        text,
        secs: start.elapsed().as_secs_f32(),
    };

    if crate::agent::agent_online() {
        let (tx, rx) = channel();
        let prompt_owned = prompt.to_string();
        std::thread::spawn(move || {
            crate::agent::run_turn(&prompt_owned, "desktop-onboarding", &tx)
        });
        let mut text = String::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(120)) {
                Ok(crate::agent::AgentEvent::Delta(d)) => text.push_str(&d),
                Ok(crate::agent::AgentEvent::Done(t)) => {
                    let t = if t.trim().is_empty() { text } else { t };
                    return done(ProbeRoute::AgentServer, true, t.trim().to_string());
                }
                Ok(crate::agent::AgentEvent::Error(e)) => {
                    return done(ProbeRoute::AgentServer, false, e)
                }
                Ok(_) => {}
                Err(_) => {
                    return done(
                        ProbeRoute::AgentServer,
                        false,
                        "agent did not answer in 120s".into(),
                    )
                }
            }
        }
    }

    let Some(apollo) = crate::agent::find_apollo_bin() else {
        return done(
            ProbeRoute::OfflineMock("apollo binary not found".into()),
            true,
            mock_reply(prompt),
        );
    };
    if !has_credential {
        return done(
            ProbeRoute::OfflineMock("no api key entered".into()),
            true,
            mock_reply(prompt),
        );
    }

    // The key reaches the child through the workspace .env apollo loads
    // itself — never argv, never this process's environment.
    let child = std::process::Command::new(apollo)
        .args(["ask", prompt, "--config", "apollo.json"])
        .current_dir(workspace)
        .env_remove("OPENAI_API_KEY")
        .env("RUST_LOG", "error")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let child = match child {
        Ok(c) => c,
        Err(e) => {
            return done(
                ProbeRoute::Cli,
                false,
                format!("could not start apollo: {e}"),
            )
        }
    };
    let pid = child.id();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(Duration::from_secs(90)) {
        Ok(Ok(output)) => {
            let out = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if output.status.success() && !out.is_empty() {
                done(ProbeRoute::Cli, true, out)
            } else {
                let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let detail = if err.is_empty() { out } else { err };
                // Last line is the error; earlier lines are log noise.
                let detail = detail
                    .lines()
                    .last()
                    .unwrap_or("apollo ask failed")
                    .to_string();
                done(ProbeRoute::Cli, false, detail)
            }
        }
        Ok(Err(e)) => done(ProbeRoute::Cli, false, format!("apollo ask failed: {e}")),
        Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {
            kill_process(pid);
            done(
                ProbeRoute::Cli,
                false,
                "apollo ask did not answer within 90s".into(),
            )
        }
    }
}

/// Stop a hung `apollo ask`.
#[cfg(unix)]
fn kill_process(pid: u32) {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: plain syscall on a pid this process spawned.
    unsafe {
        kill(pid as i32, 9);
    }
}

#[cfg(not(unix))]
fn kill_process(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn mock_reply(prompt: &str) -> String {
    let preview: String = prompt.chars().take(60).collect();
    format!(
        "hello — this reply is generated locally, not by a model. \
         your prompt (\"{preview}\") reached the ui's agent path; \
         install apollo and add a key to get a live answer."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_lists_match_purpose() {
        assert_eq!(FIRST_RUN_STEPS[0], Step::Welcome);
        assert_eq!(NEW_INSTANCE_STEPS[0], Step::Provider);
        assert_eq!(*FIRST_RUN_STEPS.last().unwrap(), Step::Done);
        assert_eq!(*NEW_INSTANCE_STEPS.last().unwrap(), Step::Done);
    }

    #[test]
    fn mock_is_labelled_offline() {
        let r = ProbeRoute::OfflineMock("no api key entered".into());
        assert!(r.label().starts_with("offline mock"));
        assert!(mock_reply("hi").contains("not by a model"));
    }
}
