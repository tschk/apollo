//! apollo desktop app — Crepuscularity + GPUI.
//!
//! Layout and palette follow telekinesis' UI: zinc surfaces with an indigo
//! accent, tool calls rendered as `| tool` with indented detail, a blinking
//! input cursor, a braille spinner while busy, and a status bar carrying
//! model, engine and connection state.

mod agent;

use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use agent::AgentEvent;
use crepuscularity_gpui::prelude::*;
use gpui::{actions, bounds, point, px, size, Application, ClickEvent, KeyDownEvent, SharedString};

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

// ── Palette (tailwind zinc/indigo, matching telekinesis) ────────────────────
// Surface and border tones live as literals in the `view!` template below;
// these are the ones the Rust-built transcript rows need.
const TEXT: u32 = 0xf4f4f5; // zinc-100
const TEXT_FAINT: u32 = 0x71717a; // zinc-500
const TEXT_GHOST: u32 = 0x52525b; // zinc-600
const ACCENT: u32 = 0x818cf8; // indigo-400
const USER: u32 = 0x60a5fa; // blue-400
const OK: u32 = 0x4ade80; // green-400
const ERR: u32 = 0xf87171; // red-400

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

impl Entry {
    fn view(&self, cursor: &'static str) -> impl IntoElement {
        match self {
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
        }
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
    engine: String,
    history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,
    spinner_start: Instant,
    cursor_start: Instant,
    turns: usize,
}

impl ApolloView {
    fn new(cx: &mut Context<Self>) -> Self {
        let (model, engine) = agent::config_summary();
        let online = agent::agent_online();

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

        Self {
            focus: cx.focus_handle(),
            draft: String::new(),
            entries: vec![Entry::Status(if online {
                "connected — streaming tool activity live".into()
            } else {
                "no agent listening. run `apollo chat` in this workspace, then send a message"
                    .into()
            })],
            status: if online {
                "ready".into()
            } else {
                "offline".into()
            },
            busy: false,
            online,
            model,
            engine,
            history: Vec::new(),
            history_index: None,
            history_draft: String::new(),
            spinner_start: Instant::now(),
            cursor_start: Instant::now(),
            turns: 0,
        }
    }

    // ── Input ──────────────────────────────────────────────────────────────

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let stroke = &event.keystroke;
        let key = stroke.key.as_str();

        // Let the platform paste path through rather than swallowing it.
        if stroke.modifiers.platform || stroke.modifiers.control {
            if key == "v" {
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
        self.send(window, cx);
    }

    fn clear_action(&mut self, _: &ClearDraft, _window: &mut Window, cx: &mut Context<Self>) {
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
        self.entries.clear();
        self.status = "cleared".into();
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
        cx.notify();

        // The transport is blocking, so it runs on its own thread and reports
        // back through a channel the UI drains on the foreground.
        let (tx, rx) = channel::<AgentEvent>();
        std::thread::spawn(move || {
            agent::run_turn(&prompt, "desktop", &tx);
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
        match event {
            AgentEvent::Status(message) => {
                self.status = message.into();
                false
            }
            AgentEvent::ToolStart { name, hint } => {
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
                self.busy = false;
                self.online = true;
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

// ── Rendering ───────────────────────────────────────────────────────────────

impl ApolloView {
    fn transcript(&self) -> impl IntoElement {
        let cursor = blink_cursor(self.cursor_start);

        div()
            .flex()
            .flex_col()
            .gap_4()
            .children(self.entries.iter().map(|entry| entry.view(cursor)))
    }
}

impl Render for ApolloView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy;
        let spinner = SharedString::from(if busy {
            spinner_frame(self.spinner_start).to_string()
        } else {
            String::new()
        });
        let cursor = blink_cursor(self.cursor_start);
        let draft_empty = self.draft.is_empty();
        let draft_display = SharedString::from(if draft_empty {
            format!("type a message…{cursor}")
        } else {
            format!("{}{cursor}", self.draft)
        });

        let status = self.status.clone();
        let model = SharedString::from(self.model.clone());
        let engine = SharedString::from(format!("engine: {}", self.engine));
        let turns = SharedString::from(format!("{} turns", self.turns));
        let online = self.online;
        let link = SharedString::from(if online {
            format!("● :{}", agent::http_port())
        } else {
            "○ offline".to_string()
        });
        let transcript = self.transcript();

        view! {r#"
            div w-full h-full bg-[#09090b] text-[#f4f4f5] flex flex-col @keydown=on_key_down

                div h-12 w-full flex flex-row items-center px-5 gap-3 border-b border-[#27272a]
                    span text-lg font-semibold text-[#818cf8]
                        "apollo"
                    span text-xs text-[#52525b]
                        "v0.2.2"
                    if {busy}
                        span text-xs text-[#fbbf24]
                            "{spinner}"
                    span flex-1
                    span text-xs text-[#71717a]
                        "{model}"
                    span text-xs text-[#52525b]
                        "{engine}"
                    if {online}
                        span text-xs text-[#4ade80]
                            "{link}"
                    else
                        span text-xs text-[#52525b]
                            "{link}"

                div flex-1 w-full px-5 py-4 overflow-hidden
                    {transcript}

                div w-full px-5 py-2 flex flex-row gap-2 border-t border-[#27272a]
                    button bg-[#18181b] border border-[#27272a] text-[#a1a1aa] text-xs px-3 py-1 rounded-md @click=prompt_doctor
                        "doctor"
                    button bg-[#18181b] border border-[#27272a] text-[#a1a1aa] text-xs px-3 py-1 rounded-md @click=prompt_tools
                        "tools"
                    button bg-[#18181b] border border-[#27272a] text-[#a1a1aa] text-xs px-3 py-1 rounded-md @click=clear_chat
                        "clear"

                div w-full flex flex-row items-center px-5 py-3 gap-3 border-t border-[#27272a]
                    span text-sm text-[#818cf8]
                        "›"
                    if {draft_empty}
                        span flex-1 text-sm text-[#52525b]
                            "{draft_display}"
                    else
                        span flex-1 text-sm text-[#f4f4f5]
                            "{draft_display}"
                    button bg-[#818cf8] text-[#09090b] text-xs font-semibold px-4 py-2 rounded-md disabled={busy} @click=submit
                        if {busy}
                            "…"
                        else
                            "send"

                div h-7 w-full flex flex-row items-center px-5 gap-3 border-t border-[#27272a] bg-[#18181b]
                    span text-xs text-[#71717a]
                        "{status}"
                    span flex-1
                    span text-xs text-[#52525b]
                        "{turns}"
                    span text-xs text-[#52525b]
                        "↑↓ history · esc clear · enter send"
        "#}
        .track_focus(&self.focus)
        .on_action(cx.listener(Self::submit_action))
        .on_action(cx.listener(Self::clear_action))
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        cx.bind_keys([
            gpui::KeyBinding::new("enter", SubmitMessage, None),
            gpui::KeyBinding::new("escape", ClearDraft, None),
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

        let opened = cx.open_window(window_options, |window, cx| {
            let view = cx.new(ApolloView::new);
            // Without this the root div never receives keystrokes.
            window.focus(&view.read(cx).focus);
            view
        });
        if let Err(e) = opened {
            eprintln!("failed to open apollo ui: {e:?}");
        }
    });
}
