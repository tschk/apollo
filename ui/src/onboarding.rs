//! First-run onboarding: welcome → provider + key → workspace → permissions
//! → test prompt → done, then the window swaps to the main chat view.
//!
//! Layout lives in `views/*.crepus` (compiled in with `view_file!`); rows
//! whose count or click target depends on state (provider cards, fields,
//! profile list) are built here and handed to the templates as children.

use std::path::PathBuf;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::{Duration, Instant};

use crepuscularity_gpui::prelude::*;
use gpui::{AnyElement, ClickEvent, KeyDownEvent, PathPromptOptions, SharedString};

use crate::setup::{
    self, DesktopState, ProfileInfo, ProviderInfo, Secret, SetupChoices, WrittenSetup, PROFILES,
    PROVIDERS,
};
use crate::theme::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Welcome,
    Provider,
    Workspace,
    Permissions,
    Test,
    Done,
}

const STEPS: [Step; 6] = [
    Step::Welcome,
    Step::Provider,
    Step::Workspace,
    Step::Permissions,
    Step::Test,
    Step::Done,
];

impl Step {
    fn index(self) -> usize {
        STEPS.iter().position(|s| *s == self).unwrap_or(0)
    }
    fn label(self) -> &'static str {
        match self {
            Step::Welcome => "welcome",
            Step::Provider => "provider",
            Step::Workspace => "workspace",
            Step::Permissions => "permissions",
            Step::Test => "test",
            Step::Done => "done",
        }
    }
}

/// Which text field receives typing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    None,
    ApiKey,
    Model,
    Workspace,
    Prompt,
}

/// How the test prompt was answered — shown so a mocked reply is never
/// mistaken for a live one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeRoute {
    /// A running apollo agent (`apollo chat`) answered over its HTTP API,
    /// i.e. a full rx4 agent-loop turn.
    AgentServer,
    /// `apollo ask` in the new workspace: apollo loaded the config and
    /// credential just written and round-tripped the provider.
    Cli,
    /// No apollo binary or no credential — answered locally.
    OfflineMock(String),
}

impl ProbeRoute {
    fn label(&self) -> String {
        match self {
            ProbeRoute::AgentServer => "live · running apollo agent (rx4 loop)".into(),
            ProbeRoute::Cli => "live · apollo ask with the new workspace config".into(),
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

/// Called once with the onboarded workspace; swaps the window to the chat.
type OnFinish = Box<dyn FnOnce(PathBuf, &mut Window, &mut App) + 'static>;

pub struct OnboardingView {
    pub focus: gpui::FocusHandle,
    step: Step,
    field: Field,
    provider: &'static ProviderInfo,
    api_key: Secret,
    model: String,
    workspace: String,
    profile: &'static ProfileInfo,
    prompt: String,
    test: TestState,
    written: Option<WrittenSetup>,
    error: String,
    cursor_start: Instant,
    on_finish: Option<OnFinish>,
}

impl OnboardingView {
    pub fn new(
        cx: &mut Context<Self>,
        on_finish: impl FnOnce(PathBuf, &mut Window, &mut App) + 'static,
    ) -> Self {
        let previous = DesktopState::load().unwrap_or_default();
        let provider = previous
            .provider
            .as_deref()
            .and_then(setup::provider)
            .unwrap_or(&PROVIDERS[0]);
        let model = previous
            .model
            .clone()
            .unwrap_or_else(|| provider.default_model.to_string());
        let workspace = previous
            .workspace
            .clone()
            .unwrap_or_else(setup::default_workspace);
        let profile = previous
            .permission_profile
            .as_deref()
            .and_then(|id| PROFILES.iter().find(|p| p.id == id))
            .unwrap_or(&PROFILES[0]);

        // Repaint for the blinking caret and the test spinner.
        cx.spawn(async move |this, cx: &mut gpui::AsyncApp| loop {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        })
        .detach();

        Self {
            focus: cx.focus_handle(),
            step: Step::Welcome,
            field: Field::None,
            provider,
            api_key: Secret::default(),
            model,
            workspace: setup::display_path(&workspace),
            profile,
            prompt: "Say hello and tell me which model you are, in one sentence.".into(),
            test: TestState::Idle,
            written: None,
            error: String::new(),
            cursor_start: Instant::now(),
            on_finish: Some(Box::new(on_finish)),
        }
    }

    fn default_field(&self) -> Field {
        match self.step {
            Step::Provider if self.provider.env_var.is_some() => Field::ApiKey,
            Step::Provider => Field::Model,
            Step::Workspace => Field::Workspace,
            Step::Test => Field::Prompt,
            _ => Field::None,
        }
    }

    fn caret(&self) -> &'static str {
        if (self.cursor_start.elapsed().as_millis() / 500).is_multiple_of(2) {
            "\u{258F}"
        } else {
            " "
        }
    }

    fn choices(&self) -> SetupChoices {
        SetupChoices {
            provider: self.provider,
            api_key: self.api_key.clone(),
            model: self.model.clone(),
            workspace: setup::expand_path(&self.workspace),
            profile: self.profile,
        }
    }

    // ── Navigation ─────────────────────────────────────────────────────────

    fn go(&mut self, step: Step, cx: &mut Context<Self>) {
        self.step = step;
        self.error.clear();
        self.field = self.default_field();
        self.cursor_start = Instant::now();
        cx.notify();
    }

    /// Validate the current step and advance. Writing the config happens on
    /// leaving Permissions, so the test step exercises the real files.
    fn advance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.step {
            Step::Welcome => self.go(Step::Provider, cx),
            Step::Provider => match setup::validate_key(self.provider, &self.api_key) {
                Ok(()) => {
                    if self.model.trim().is_empty() {
                        self.model = self.provider.default_model.to_string();
                    }
                    self.go(Step::Workspace, cx)
                }
                Err(e) => {
                    self.error = e;
                    self.field = Field::ApiKey;
                    cx.notify();
                }
            },
            Step::Workspace => {
                if self.workspace.trim().is_empty() {
                    self.error = "choose a folder for apollo to work in".into();
                    cx.notify();
                } else {
                    self.go(Step::Permissions, cx)
                }
            }
            Step::Permissions => match setup::write_setup(&self.choices()) {
                Ok(written) => {
                    self.written = Some(written);
                    self.test = TestState::Idle;
                    self.go(Step::Test, cx);
                }
                Err(e) => {
                    self.error = e;
                    cx.notify();
                }
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

    fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let choices = self.choices();
        let workspace = choices
            .workspace
            .canonicalize()
            .unwrap_or(choices.workspace.clone());
        let state = DesktopState {
            onboarded: true,
            workspace: Some(workspace.clone()),
            provider: Some(self.provider.id.to_string()),
            model: Some(self.model.trim().to_string()),
            permission_profile: Some(self.profile.id.to_string()),
        };
        if let Err(e) = state.save() {
            self.error = e;
            cx.notify();
            return;
        }
        // The key is on disk now; drop the in-memory copy.
        self.api_key.clear();
        if let Some(on_finish) = self.on_finish.take() {
            on_finish(workspace, window, cx);
        }
    }

    fn on_next(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.advance(window, cx);
    }

    fn on_back(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let i = self.step.index();
        if i > 0 && !matches!(self.test, TestState::Running(_)) {
            self.go(STEPS[i - 1], cx);
        }
    }

    fn on_browse(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
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

    fn run_test(&mut self, cx: &mut Context<Self>) {
        if matches!(self.test, TestState::Running(_)) || self.prompt.trim().is_empty() {
            return;
        }
        let workspace = setup::expand_path(&self.workspace);
        let prompt = self.prompt.trim().to_string();
        let has_credential = self.provider.env_var.is_none() || !self.api_key.is_empty();
        self.test = TestState::Running(Instant::now());
        self.field = Field::None;
        cx.notify();

        let (tx, rx) = channel::<ProbeResult>();
        std::thread::spawn(move || {
            let _ = tx.send(probe(&workspace, &prompt, has_credential));
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
        if self.provider.id != provider.id {
            // A key for one provider is never valid for another.
            self.api_key.clear();
            self.model = provider.default_model.to_string();
        }
        self.provider = provider;
        self.error.clear();
        self.field = if provider.env_var.is_some() {
            Field::ApiKey
        } else {
            Field::Model
        };
        cx.notify();
    }

    fn pick_profile(&mut self, profile: &'static ProfileInfo, cx: &mut Context<Self>) {
        self.profile = profile;
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
                }
            }
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
                self.field = match (self.step, self.field) {
                    (Step::Provider, Field::ApiKey) => Field::Model,
                    (Step::Provider, _) if self.provider.env_var.is_some() => Field::ApiKey,
                    _ => self.field,
                };
                cx.notify();
            }
            "backspace" => {
                match self.field {
                    Field::ApiKey => self.api_key.pop(),
                    Field::Model => {
                        self.model.pop();
                    }
                    Field::Workspace => {
                        self.workspace.pop();
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
                    Field::Model => self.model.clear(),
                    Field::Workspace => self.workspace.clear(),
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
        match self.field {
            // Whitespace never belongs in a key; a paste often carries some.
            Field::ApiKey => self.api_key.push_str(
                &text
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect::<String>(),
            ),
            Field::Model => self.model.push_str(text.trim()),
            Field::Workspace => self.workspace.push_str(&text),
            Field::Prompt => self.prompt.push_str(&text),
            Field::None => return,
        }
        self.error.clear();
        cx.notify();
    }

    // ── Rust-built pieces handed to the templates ─────────────────────────

    fn rail(&self) -> AnyElement {
        let current = self.step.index();
        div()
            .w_full()
            .flex()
            .flex_row()
            .gap_2()
            .children(STEPS.iter().enumerate().map(|(i, step)| {
                let (bar, label) = if i < current {
                    (MUTED, MUTED)
                } else if i == current {
                    (ACCENT, ACCENT)
                } else {
                    (SURFACE_2, 0x52525b)
                };
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(div().h(px(3.)).w_full().rounded_sm().bg(rgb(bar)))
                    .child(div().text_xs().text_color(rgb(label)).child(format!(
                        "{:02} {}",
                        i + 1,
                        step.label()
                    )))
            }))
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
            .border_color(rgb(if focused { MUTED } else { BORDER }))
            .bg(rgb(if focused { SURFACE } else { BG }))
            .text_sm()
            .text_color(rgb(if empty && !focused { 0x52525b } else { TEXT }))
            .cursor_text()
            .child(SharedString::from(text))
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                view.field = field;
                view.cursor_start = Instant::now();
                cx.notify();
            }))
            .into_any_element()
    }

    fn provider_grid(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows: Vec<AnyElement> = PROVIDERS
            .chunks(4)
            .enumerate()
            .map(|(r, chunk)| {
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .children(chunk.iter().enumerate().map(|(c, p)| {
                        let selected = p.id == self.provider.id;
                        div()
                            .id(("provider", r * 4 + c))
                            .flex_1()
                            // Without this a card's min width is its content,
                            // and the row stops being four equal columns.
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .p_3()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(if selected { ACCENT } else { BORDER }))
                            .bg(rgb(if selected { SURFACE_2 } else { SURFACE }))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(SURFACE_2)))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(rgb(if selected { ACCENT } else { TEXT }))
                                            .child(p.label),
                                    )
                                    .child(div().flex_1())
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(SUCCESS))
                                            .child(if selected { "●" } else { "" }),
                                    ),
                            )
                            .child(div().text_xs().text_color(rgb(MUTED)).child(p.blurb))
                            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                                view.pick_provider(p, cx)
                            }))
                            .into_any_element()
                    }))
                    // Keep the last row's cards the same width as the others.
                    // Same padding and border as a card: with a zero flex basis
                    // those still count, so a bare spacer would come out narrower.
                    .children((chunk.len()..4).map(|_| {
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .p_3()
                            .border_1()
                            .border_color(gpui::transparent_black())
                            .into_any_element()
                    }))
                    .into_any_element()
            })
            .collect();
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .children(rows)
            .into_any_element()
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
                    .border_color(rgb(if selected { ACCENT } else { BORDER }))
                    .bg(rgb(if selected { SURFACE_2 } else { SURFACE }))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(SURFACE_2)))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(if selected { ACCENT } else { MUTED }))
                            .child(if selected { "(•)" } else { "( )" }),
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
                .child(div().text_xs().text_color(rgb(0x52525b)).child(who))
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
                .text_color(rgb(0x52525b))
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
                .child(div().w(px(120.)).text_xs().text_color(rgb(MUTED)).child(k))
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(rgb(TEXT))
                        .child(SharedString::from(v)),
                )
        };
        let ws = setup::expand_path(&self.workspace);
        let key = match (self.provider.env_var, &self.written) {
            (None, _) => "not needed".to_string(),
            (Some(var), Some(w)) => format!(
                "{var} in {} (0600, never shown)",
                w.env_path
                    .as_deref()
                    .map(setup::display_path)
                    .unwrap_or_default()
            ),
            (Some(var), None) => format!("{var} — not written yet"),
        };
        let test = match &self.test {
            TestState::Finished(r) if r.ok => format!("passed · {}", r.route.label()),
            TestState::Finished(r) => format!("failed · {}", r.route.label()),
            _ => "skipped".to_string(),
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(row("provider", self.provider.label.to_string()))
            .child(row("model", self.model.trim().to_string()))
            .child(row("api key", key))
            .child(row("workspace", setup::display_path(&ws)))
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
}

impl Render for OnboardingView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let step = self.step;
        let step_counter = format!("step {} of {}", step.index() + 1, STEPS.len());
        let rail = self.rail();
        let can_back = step.index() > 0 && step != Step::Done;
        let error = self.error.clone();
        let running = matches!(self.test, TestState::Running(_));

        let (eyebrow, title, lede, hint, next_label): (&str, &str, String, &str, &str) = match step
        {
            Step::Welcome => (
                "welcome",
                "meet apollo",
                "a local-first agent host on the rotary (rx4) harness engine. \
                 a few screens and it is ready to work in a folder of your choosing."
                    .into(),
                "enter to continue",
                "get started",
            ),
            Step::Provider => (
                "model provider",
                "pick a provider and paste its key",
                "the key is written to your workspace .env, owner-only. \
                 apollo.json never holds it, and it is never logged."
                    .into(),
                "ctrl+v pastes · tab switches field",
                "continue",
            ),
            Step::Workspace => (
                "workspace",
                "where should apollo work?",
                "apollo reads and writes files, keeps memory and runs tools inside this folder. \
                 it is created if missing."
                    .into(),
                "",
                "continue",
            ),
            Step::Permissions => (
                "permissions",
                "choose policy defaults",
                "how much apollo may do on its own. \
                 continuing writes apollo.json and .env into the workspace."
                    .into(),
                "",
                "save and continue",
            ),
            Step::Test => (
                "test",
                "check that the agent answers",
                "one prompt through apollo using the config just written. \
                 with no apollo binary or key it falls back to a clearly marked offline mock."
                    .into(),
                if running {
                    "waiting for the agent…"
                } else {
                    ""
                },
                match &self.test {
                    TestState::Running(_) => "…",
                    TestState::Finished(r) if r.ok => "continue",
                    TestState::Finished(_) => "continue anyway",
                    _ => "skip",
                },
            ),
            Step::Done => (
                "done",
                "apollo is set up",
                "everything below is on disk. the main window opens next.".into(),
                "",
                "open apollo",
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
                let provider_grid = self.provider_grid(cx);
                let key_label = match self.provider.env_var {
                    Some(var) => format!("api key · {var}"),
                    None => "api key · not needed for ollama".into(),
                };
                let key_field = if self.provider.env_var.is_some() {
                    self.text_field(
                        "api-key",
                        Field::ApiKey,
                        self.api_key.masked(),
                        "paste your key",
                        cx,
                    )
                } else {
                    div()
                        .w_full()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(BORDER))
                        .text_sm()
                        .text_color(rgb(0x52525b))
                        .child("ollama runs locally at http://localhost:11434")
                        .into_any_element()
                };
                // Not even the length is shown back.
                let key_note = if self.api_key.is_empty() {
                    "masked while you type · stored locally only"
                } else {
                    "key captured · masked · stored locally only"
                };
                let model_field = self.text_field(
                    "model",
                    Field::Model,
                    self.model.clone(),
                    self.provider.default_model,
                    cx,
                );
                view_file!("views/provider.crepus").into_any_element()
            }
            Step::Workspace => {
                let workspace_field = self.text_field(
                    "workspace",
                    Field::Workspace,
                    self.workspace.clone(),
                    "~/apollo",
                    cx,
                );
                let ws = setup::expand_path(&self.workspace);
                let workspace_note = if ws.join("apollo.json").is_file() {
                    "this folder already has an apollo.json — it will be updated, not replaced"
                        .to_string()
                } else if ws.is_dir() {
                    "existing folder".to_string()
                } else {
                    "new folder — created on save".to_string()
                };
                let shown = setup::display_path(&ws);
                let config_preview = format!("{shown}/apollo.json   provider, model, policy");
                let env_preview = match self.provider.env_var {
                    Some(var) => format!("{shown}/.env          {var} (0600)"),
                    None => format!("{shown}/.env          OLLAMA_BASE_URL"),
                };
                let memory_preview = format!("{shown}/.apollo/      memory + sessions");
                view_file!("views/workspace.crepus").into_any_element()
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
            #[cfg(unix)]
            unsafe {
                libc_kill(pid as i32);
            }
            done(
                ProbeRoute::Cli,
                false,
                "apollo ask did not answer within 90s".into(),
            )
        }
    }
}

#[cfg(unix)]
unsafe fn libc_kill(pid: i32) {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    kill(pid, 9);
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
    fn steps_are_ordered_and_labelled() {
        assert_eq!(Step::Welcome.index(), 0);
        assert_eq!(Step::Done.index(), STEPS.len() - 1);
        for s in STEPS {
            assert!(!s.label().is_empty());
        }
    }

    #[test]
    fn mock_is_labelled_offline() {
        let r = ProbeRoute::OfflineMock("no api key entered".into());
        assert!(r.label().starts_with("offline mock"));
        assert!(mock_reply("hi").contains("not by a model"));
    }
}
