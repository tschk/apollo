//! apollo desktop app — Crepuscularity + GPUI.
//!
//! First launch runs the onboarding (`onboarding.rs`, `views/*.crepus`):
//! sign-in or provider, a folder or "everywhere", permissions, a test prompt
//! and simple-vs-advanced, written where the `apollo` CLI reads them. Later
//! launches open the main window on the active instance; `--onboarding`
//! runs the setup again.
//!
//! The app manages several apollo *instances* (each its own config dir,
//! provider, model and permissions). Simple mode is the chat plus an
//! instance pill; advanced mode adds a roster sidebar, tools/permissions,
//! model parameters and a session log (`shell.rs`).
//!
//! Palette and type follow the Telekinesis portal tokens (`theme.rs`):
//! zinc-950 surfaces, Chivo Mono.

mod agent;
mod catalog;
mod models;
mod oauth;
mod onboarding;
mod setup;
mod shell;
mod theme;

use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use agent::AgentEvent;
use crepuscularity_gpui::prelude::*;
use gpui::{
    actions, bounds, ease_out_quint, point, px, size, Animation, Application, ClickEvent,
    KeyDownEvent, SharedString,
};

actions!(
    apollo_ui,
    [SubmitMessage, ClearDraft, HistoryPrev, HistoryNext]
);

/// Braille spinner, as used by the telekinesis TUI.
const SPINNER_FRAMES: [&str; 10] = [
    "\u{280B}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283C}", "\u{2834}", "\u{2826}", "\u{2827}",
    "\u{2807}", "\u{280F}",
];

const MAX_HISTORY: usize = 100;

// ── Palette (Telekinesis portal tokens, see theme.rs) ───────────────────────
// Surface and border tones live as literals in the `view!` template below;
// these are the ones the Rust-built transcript rows need.
const TEXT: u32 = theme::TEXT;
const TEXT_FAINT: u32 = theme::MUTED;
const TEXT_GHOST: u32 = 0x52525b; // zinc-600
const ACCENT: u32 = theme::ACCENT;
const USER: u32 = 0xa1a1aa; // zinc-400
const OK: u32 = theme::SUCCESS;
const ERR: u32 = theme::DANGER;

fn spinner_frame(start: Instant) -> &'static str {
    let idx = ((start.elapsed().as_millis() / 100) % SPINNER_FRAMES.len() as u128) as usize;
    SPINNER_FRAMES[idx]
}

fn blink_cursor(start: Instant) -> &'static str {
    if (start.elapsed().as_millis() / 500).is_multiple_of(2) {
        "\u{258F}"
    } else {
        " "
    }
}

/// One rendered row in the transcript.
#[derive(Clone)]
enum Entry {
    User(String),
    Agent {
        text: String,
        streaming: bool,
    },
    /// A tool call: `| name` plus its hint, and its outcome once known.
    Tool {
        name: String,
        hint: String,
        outcome: Option<(bool, u64)>,
    },
    Status(String),
    Error(String),
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

impl Entry {
    fn view(&self, cursor: &'static str, index: usize) -> impl IntoElement {
        let row = match self {
            Entry::User(text) => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_xs().text_color(rgb(TEXT_GHOST)).child("you"))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(USER))
                        .child(SharedString::from(text.clone())),
                ),

            Entry::Agent { text, streaming } => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_xs().text_color(rgb(ACCENT)).child("apollo"))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(TEXT))
                        .child(SharedString::from(if *streaming {
                            format!("{text}{cursor}")
                        } else {
                            text.clone()
                        })),
                ),

            Entry::Tool {
                name,
                hint,
                outcome,
            } => {
                let (mark, color) = match outcome {
                    None => ("…".to_string(), TEXT_FAINT),
                    Some((true, secs)) => (format!("✓ {secs}s"), OK),
                    Some((false, secs)) => (format!("✗ {secs}s"), ERR),
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(TEXT_FAINT))
                                    .child(SharedString::from(format!("| {name}"))),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(color))
                                    .child(SharedString::from(mark)),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(TEXT_GHOST))
                            .child(SharedString::from(format!("  {hint}"))),
                    )
            }

            Entry::Status(text) => div()
                .text_xs()
                .text_color(rgb(TEXT_GHOST))
                .child(SharedString::from(text.clone())),

            Entry::Error(text) => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_xs().text_color(rgb(ERR)).child("error"))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(ERR))
                        .child(SharedString::from(text.clone())),
                ),
        };
        row.with_animation(
            ("entry", index),
            Animation::new(Duration::from_millis(180)).with_easing(ease_out_quint()),
            |el, delta| el.opacity(0.45 + 0.55 * delta),
        )
    }
}

struct ApolloView {
    /// GPUI only routes key events to the focused element, so the root div
    /// tracks this handle and the window focuses it on open.
    focus: gpui::FocusHandle,
    draft: String,
    entries: Vec<Entry>,
    status: SharedString,
    busy: bool,
    online: bool,
    model: String,
    history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,
    spinner_start: Instant,
    cursor_start: Instant,
    turns: usize,
    /// Everything the app knows: instances, active one, mode.
    state: setup::DesktopState,
    /// The instance this window is chatting with.
    instance: setup::Instance,
    panel: shell::Panel,
    switcher_open: bool,
    /// Session log for the advanced logs panel.
    logs: Vec<shell::LogLine>,
    /// Last settings write, shown under the settings controls.
    notice: String,
    /// Which profile field is being edited, if any. While set, typing goes
    /// to `edit_buf` instead of the chat draft.
    edit: Option<shell::EditField>,
    edit_buf: String,
    /// Instance id whose roster context menu is open.
    roster_menu: Option<String>,
    /// Model picker overlay state, when open.
    picker: Option<shell::PickerState>,
    /// Which role dial the picker is writing, if it was opened from one.
    dial: Option<&'static str>,
    /// Agent session. A new chat mints a new id so the next turn does not
    /// continue the previous conversation.
    chat_id: String,
}

impl ApolloView {
    fn new(state: setup::DesktopState, instance: setup::Instance, cx: &mut Context<Self>) -> Self {
        let model = agent::config_model();
        let online = agent::agent_online();
        let config_dir = instance.config_dir.clone();

        // Repaint on a timer so the spinner animates and the cursor blinks.
        cx.spawn(async move |this, cx: &mut gpui::AsyncApp| loop {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        })
        .detach();

        let mut view = Self {
            focus: cx.focus_handle(),
            draft: String::new(),
            entries: vec![Entry::Status(if online {
                "agent ready".into()
            } else {
                "starting the agent…".into()
            })],
            status: if online {
                "ready".into()
            } else {
                "starting…".into()
            },
            busy: false,
            online,
            model,
            history: Vec::new(),
            history_index: None,
            history_draft: String::new(),
            spinner_start: Instant::now(),
            cursor_start: Instant::now(),
            turns: 0,
            logs: vec![shell::LogLine::new(
                shell::LogKind::Info,
                format!(
                    "opened {} · {} · {}",
                    instance.name,
                    instance.provider,
                    if online {
                        "agent ready"
                    } else {
                        "starting the agent"
                    }
                ),
            )],
            state,
            instance,
            panel: shell::Panel::Chat,
            switcher_open: false,
            notice: String::new(),
            edit: None,
            edit_buf: String::new(),
            roster_menu: None,
            picker: None,
            dial: None,
            chat_id: format!("desktop-{}", now_millis()),
        };
        view.supervise_agent(config_dir, cx);
        view
    }

    /// Start the instance's agent in the background and flip the banner when
    /// it is ready. Messages wait on this process; they are not sent as a
    /// bare completion.
    fn supervise_agent(&mut self, config_dir: std::path::PathBuf, cx: &mut Context<Self>) {
        let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
        std::thread::spawn(move || {
            let _ = tx.send(agent::ensure_daemon(&config_dir));
        });
        cx.spawn(async move |this, cx: &mut gpui::AsyncApp| loop {
            match rx.try_recv() {
                Ok(Ok(())) => {
                    this.update(cx, |view, cx| {
                        view.online = true;
                        view.status = "ready".into();
                        if let Some(state) = agent::fetch_state() {
                            if !state.model.is_empty() && state.model != "—" {
                                view.model = state.model.clone();
                                view.instance.model = state.model;
                            }
                        }
                        if let Some(Entry::Status(text)) = view.entries.first_mut() {
                            if text.starts_with("starting") || text.starts_with("agent ") {
                                *text = "agent ready — tools and streaming are on".into();
                            }
                        }
                        view.log(shell::LogKind::Ok, "agent ready");
                        cx.notify();
                    })
                    .ok();
                    break;
                }
                Ok(Err(error)) => {
                    this.update(cx, |view, cx| {
                        view.online = false;
                        view.status = "agent failed".into();
                        if let Some(Entry::Status(text)) = view.entries.first_mut() {
                            *text = format!("the agent did not start. {error}");
                        }
                        view.log(shell::LogKind::Error, error);
                        cx.notify();
                    })
                    .ok();
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
        })
        .detach();
    }

    // ── Input ──────────────────────────────────────────────────────────────

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let stroke = &event.keystroke;
        let key = stroke.key.as_str();

        // The model picker is modal: escape closes it, nothing else types.
        if self.picker.is_some() {
            if key == "escape" {
                self.picker = None;
                self.dial = None;
            } else if key == "backspace" {
                if let Some(picker) = self.picker.as_mut() {
                    picker.query.pop();
                }
            } else if let Some(ch) = stroke.key_char.as_deref() {
                if !ch.is_empty() && !ch.chars().any(char::is_control) {
                    if let Some(picker) = self.picker.as_mut() {
                        picker.query.push_str(ch);
                    }
                }
            } else if key == "space" {
                if let Some(picker) = self.picker.as_mut() {
                    picker.query.push(' ');
                }
            }
            cx.notify();
            return;
        }

        // A profile field being edited takes over the keyboard.
        if self.edit.is_some() {
            self.edit_key_down(event, cx);
            return;
        }

        // Let the platform paste path through rather than swallowing it.
        if stroke.modifiers.platform || stroke.modifiers.control {
            if key == "n" {
                self.new_chat(cx);
            } else if key == "v" {
                if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    self.draft.push_str(text.trim_end_matches('\n'));
                    cx.notify();
                }
            }
            return;
        }

        match key {
            "enter" => self.send(window, cx),
            "backspace" => {
                self.draft.pop();
                cx.notify();
            }
            "escape" => {
                self.draft.clear();
                cx.notify();
            }
            "space" => {
                self.draft.push(' ');
                cx.notify();
            }
            "up" => self.history_step(-1, cx),
            "down" => self.history_step(1, cx),
            _ => {
                // `key_char` carries the shifted/composed character, so capitals
                // and punctuation survive — matching on `key` alone loses them.
                if let Some(ch) = stroke.key_char.as_deref() {
                    if !ch.is_empty() && !ch.chars().any(char::is_control) {
                        self.draft.push_str(ch);
                        cx.notify();
                    }
                }
            }
        }
    }

    /// Keys while a profile field is being edited: chars/space append,
    /// backspace pops, paste pastes, escape cancels, enter saves (and
    /// shift+enter puts a newline in the instructions field).
    fn edit_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        let key = stroke.key.as_str();

        if stroke.modifiers.platform || stroke.modifiers.control {
            if key == "v" {
                if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    self.edit_buf.push_str(text.trim_end_matches('\n'));
                    cx.notify();
                }
            }
            return;
        }

        let multiline = matches!(self.edit, Some(shell::EditField::Instructions));
        match key {
            "enter" if stroke.modifiers.shift && multiline => {
                self.edit_buf.push('\n');
                cx.notify();
            }
            "enter" => self.save_edit(cx),
            "backspace" => {
                self.edit_buf.pop();
                cx.notify();
            }
            "escape" => self.cancel_edit(cx),
            "space" => {
                self.edit_buf.push(' ');
                cx.notify();
            }
            _ => {
                if let Some(ch) = stroke.key_char.as_deref() {
                    if !ch.is_empty() && !ch.chars().any(char::is_control) {
                        self.edit_buf.push_str(ch);
                        cx.notify();
                    }
                }
            }
        }
    }

    /// Walk the input history, keeping the in-progress draft parked at the end.
    fn history_step(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.history.is_empty() {
            return;
        }
        let next = match (self.history_index, delta) {
            (None, -1) => {
                self.history_draft = self.draft.clone();
                Some(self.history.len() - 1)
            }
            (Some(0), -1) => Some(0),
            (Some(i), -1) => Some(i - 1),
            (Some(i), 1) if i + 1 < self.history.len() => Some(i + 1),
            (Some(_), 1) => None,
            (None, _) => None,
            _ => self.history_index,
        };
        self.history_index = next;
        self.draft = match next {
            Some(i) => self.history[i].clone(),
            None => std::mem::take(&mut self.history_draft),
        };
        cx.notify();
    }

    fn submit(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.send(window, cx);
    }

    fn submit_action(&mut self, _: &SubmitMessage, window: &mut Window, cx: &mut Context<Self>) {
        // While the picker or a profile field is open, enter belongs to it,
        // not to the chat draft.
        if self.picker.is_some() || self.edit.is_some() {
            return;
        }
        self.send(window, cx);
    }

    fn clear_action(&mut self, _: &ClearDraft, _window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.is_some() || self.edit.is_some() {
            return;
        }
        self.draft.clear();
        cx.notify();
    }

    fn use_prompt(&mut self, prompt: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.draft = prompt.to_string();
        self.send(window, cx);
    }

    fn prompt_doctor(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.use_prompt("Run doctor and summarize any issues.", window, cx);
    }

    fn prompt_tools(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.use_prompt("List your available tools, grouped by purpose.", window, cx);
    }

    fn clear_chat(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.new_chat(cx);
    }

    /// Drop the transcript and start a fresh agent session. The unsent draft
    /// stays. Hermes does this with Ctrl+N; the previous chat id is abandoned
    /// so the running agent does not keep that history on the next turn.
    fn new_chat(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.chat_id = format!("desktop-{}", now_millis());
        self.entries.clear();
        self.entries.push(Entry::Status("new chat".into()));
        self.turns = 0;
        self.status = "new chat".into();
        self.log(shell::LogKind::Info, "new chat");
        cx.notify();
    }

    // ── Turn execution ─────────────────────────────────────────────────────

    fn send(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.draft.trim().to_string();
        if prompt.is_empty() || self.busy {
            return;
        }

        self.entries.push(Entry::User(prompt.clone()));
        self.history.push(prompt.clone());
        if self.history.len() > MAX_HISTORY {
            self.history.remove(0);
        }
        self.history_index = None;
        self.draft.clear();
        self.busy = true;
        self.turns += 1;
        self.spinner_start = Instant::now();
        self.status = "thinking…".into();
        self.log(
            shell::LogKind::Info,
            format!(
                "turn {} sent ({} chars)",
                self.turns,
                prompt.chars().count()
            ),
        );
        cx.notify();

        // The transport is blocking, so it runs on its own thread and reports
        // back through a channel the UI drains on the foreground.
        let (tx, rx) = channel::<AgentEvent>();
        let chat_id = self.chat_id.clone();
        std::thread::spawn(move || {
            agent::run_turn(&prompt, &chat_id, &tx);
        });

        cx.spawn(async move |this, cx: &mut gpui::AsyncApp| {
            let rx: Receiver<AgentEvent> = rx;
            loop {
                let mut finished = false;
                let mut batch: Vec<AgentEvent> = rx.try_iter().collect();
                let mut disconnected = false;
                if batch.is_empty() {
                    // Distinguish "nothing yet" from "sender gone" without
                    // discarding an event that arrived since the drain above.
                    match rx.try_recv() {
                        Ok(event) => batch.push(event),
                        Err(std::sync::mpsc::TryRecvError::Empty) => {}
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => disconnected = true,
                    }
                }

                if !batch.is_empty()
                    && this
                        .update(cx, |view, cx| {
                            for event in batch {
                                if view.apply(event) {
                                    finished = true;
                                }
                            }
                            cx.notify();
                        })
                        .is_err()
                {
                    break;
                }

                if finished {
                    break;
                }
                if disconnected {
                    // Transport ended without a terminal event.
                    this.update(cx, |view, cx| {
                        if view.busy {
                            view.busy = false;
                            view.status = "connection lost".into();
                            view.entries
                                .push(Entry::Error("agent connection closed unexpectedly".into()));
                        }
                        cx.notify();
                    })
                    .ok();
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(30))
                    .await;
            }
        })
        .detach();
    }

    /// Fold one event into the transcript. Returns true when the turn is over.
    fn apply(&mut self, event: AgentEvent) -> bool {
        self.log_event(&event);
        match event {
            AgentEvent::Status(message) => {
                self.status = message.into();
                false
            }
            AgentEvent::ToolStart { name, hint } => {
                // Tool and delta events only arrive over the server's stream.
                self.online = true;
                self.entries.push(Entry::Tool {
                    name: name.clone(),
                    hint,
                    outcome: None,
                });
                self.status = format!("running {name}…").into();
                false
            }
            AgentEvent::ToolEnd { name, ok, secs } => {
                // Attach to the most recent unfinished call with this name.
                if let Some(entry) =
                    self.entries.iter_mut().rev().find(
                        |e| matches!(e, Entry::Tool { name: n, outcome: None, .. } if *n == name),
                    )
                {
                    if let Entry::Tool { outcome, .. } = entry {
                        *outcome = Some((ok, secs));
                    }
                } else {
                    self.entries.push(Entry::Tool {
                        name,
                        hint: String::new(),
                        outcome: Some((ok, secs)),
                    });
                }
                false
            }
            AgentEvent::Delta(text) => {
                self.online = true;
                match self.entries.last_mut() {
                    Some(Entry::Agent {
                        text: existing,
                        streaming: true,
                    }) => existing.push_str(&text),
                    _ => self.entries.push(Entry::Agent {
                        text,
                        streaming: true,
                    }),
                }
                false
            }
            AgentEvent::Done(response) => {
                // Deltas may already have built the reply; replace it so the
                // final text wins and the streaming cursor stops.
                match self.entries.last_mut() {
                    Some(Entry::Agent { text, streaming }) if *streaming => {
                        if !response.trim().is_empty() {
                            *text = response;
                        }
                        *streaming = false;
                    }
                    _ if !response.trim().is_empty() => self.entries.push(Entry::Agent {
                        text: response,
                        streaming: false,
                    }),
                    _ => {}
                }
                // The reply came from the already-running agent. A lost
                // socket is an error event, not a second completion.
                self.busy = false;
                self.status = "ready".into();
                true
            }
            AgentEvent::Error(message) => {
                self.entries.push(Entry::Error(message));
                self.busy = false;
                self.status = "error".into();
                true
            }
        }
    }
}

/// Key context of the chat view, so its enter/escape bindings do not fire
/// while the onboarding has focus.
const CHAT_CONTEXT: &str = "ApolloChat";

/// Show the main window for the state's active instance. The chat view
/// and status bar resolve `apollo.json` relative to the working directory,
/// so that becomes the instance's config dir. The agent process is started
/// once for that config and reused for every message.
pub(crate) fn open_chat(state: setup::DesktopState, window: &mut Window, cx: &mut App) {
    let Some(instance) = state.ready().cloned() else {
        return open_onboarding(onboarding::Purpose::FirstRun, window, cx);
    };
    if let Err(e) = std::env::set_current_dir(&instance.config_dir) {
        eprintln!(
            "apollo-ui: cannot enter {}: {e}",
            instance.config_dir.display()
        );
    }
    let view = window.replace_root(cx, |_, cx| ApolloView::new(state, instance, cx));
    window.focus(&view.read(cx).focus);
}

/// Swap the window to the onboarding. A cancelled new-instance flow goes
/// back to the main window unchanged.
pub(crate) fn open_onboarding(purpose: onboarding::Purpose, window: &mut Window, cx: &mut App) {
    let view = window.replace_root(cx, move |_, cx| {
        onboarding::OnboardingView::new(purpose, cx, |state, window, cx| {
            let state = state.or_else(setup::DesktopState::load).unwrap_or_default();
            open_chat(state, window, cx)
        })
    });
    window.focus(&view.read(cx).focus);
}

fn main() {
    let force_onboarding = std::env::args().skip(1).any(|a| a == "--onboarding");
    let simple = std::env::args().skip(1).any(|a| a == "--simple");
    let advanced = std::env::args().skip(1).any(|a| a == "--advanced");
    if std::env::args().skip(1).any(|a| a == "--help" || a == "-h") {
        println!(
            "apollo-ui — desktop app for apollo\n\n\
             usage: apollo-ui [--onboarding] [--simple | --advanced]\n\n\
             first launch walks through setup; later launches open the main window\n\
             on the active instance. --onboarding runs setup again; --simple and\n\
             --advanced switch the mode (saved).\n\
             state: ~/.apollo/desktop.json (no secrets)"
        );
        return;
    }
    let mut state = setup::DesktopState::load().unwrap_or_default();
    if simple || advanced {
        state.mode = if advanced {
            setup::Mode::Advanced
        } else {
            setup::Mode::Simple
        };
        let _ = state.save();
    }
    let ready = state.ready().cloned().filter(|_| !force_onboarding);

    Application::new().run(move |cx: &mut App| {
        theme::load_fonts(cx);
        cx.bind_keys([
            gpui::KeyBinding::new("enter", SubmitMessage, Some(CHAT_CONTEXT)),
            gpui::KeyBinding::new("escape", ClearDraft, Some(CHAT_CONTEXT)),
        ]);

        let window_options = gpui_window_options(
            "apollo.ui",
            "apollo",
            Some(gpui::WindowBounds::Windowed(bounds(
                point(px(80.), px(60.)),
                size(px(960.), px(760.)),
            ))),
            Some(size(px(640.), px(480.))),
        );

        let opened = cx.open_window(window_options, move |window, cx| {
            let root = match ready {
                Some(instance) => {
                    let _ = std::env::set_current_dir(&instance.config_dir);
                    let view = cx.new(|cx| ApolloView::new(state, instance, cx));
                    window.focus(&view.read(cx).focus);
                    gpui::AnyView::from(view)
                }
                None => {
                    let view = cx.new(|cx| {
                        onboarding::OnboardingView::new(
                            onboarding::Purpose::FirstRun,
                            cx,
                            |state, window, cx| {
                                let state =
                                    state.or_else(setup::DesktopState::load).unwrap_or_default();
                                open_chat(state, window, cx)
                            },
                        )
                    });
                    window.focus(&view.read(cx).focus);
                    gpui::AnyView::from(view)
                }
            };
            cx.new(|_| Root(root))
        });
        if let Err(e) = opened {
            eprintln!("failed to open apollo ui: {e:?}");
        }
    });
}

/// Window root holding whichever view launch picked; `replace_root` swaps
/// it for the chat view when onboarding finishes.
struct Root(gpui::AnyView);

impl Render for Root {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .font_family(theme::FONT)
            .child(self.0.clone())
    }
}
