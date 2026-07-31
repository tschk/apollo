//! apollo terminal UI — crepuscularity-tui over the agent's HTTP/WS API.
//!
//! The agent runs in a separate process (`apollo serve`); this is a thin
//! client. Finished messages are pushed into the terminal's own scrollback so
//! the transcript scrolls natively, and only in-flight output plus the input
//! line live in the inline viewport.

#[path = "../../src/agent.rs"]
mod agent;

use std::io::{stdout, Write};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use agent::AgentEvent;
use crepuscularity_tui::ratatui::backend::CrosstermBackend;
use crepuscularity_tui::ratatui::text::Line;
use crepuscularity_tui::{Template, TemplateContext, TemplateValue};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

const SPINNER_FRAMES: [&str; 10] = [
    "\u{280B}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283C}", "\u{2834}", "\u{2826}", "\u{2827}",
    "\u{2807}", "\u{280F}",
];

const MAX_HISTORY: usize = 100;
const VIEWPORT_ROWS: u16 = 12;
const CHAT_ID: &str = "tui";

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    User,
    Agent,
    Tool,
}

struct Msg {
    kind: Kind,
    tool_name: String,
    text: String,
}

fn spinner_frame(start: Instant) -> &'static str {
    let index = (start.elapsed().as_millis() / 80) as usize % SPINNER_FRAMES.len();
    SPINNER_FRAMES[index]
}

fn blink_cursor(start: Instant) -> &'static str {
    if (start.elapsed().as_millis() / 500).is_multiple_of(2) {
        "\u{2588}"
    } else {
        " "
    }
}

fn project_name() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|path| path.file_name().map(|n| n.to_string_lossy().into_owned()))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "-".to_string())
}

struct App {
    live: Vec<Msg>,
    scrollback: Vec<String>,
    input: String,
    history: Vec<String>,
    history_pos: Option<usize>,
    busy: bool,
    status: String,
    model: String,
    engine: String,
    online: bool,
    spinner_start: Instant,
    cursor_start: Instant,
    tx: Sender<AgentEvent>,
    rx: Receiver<AgentEvent>,
    quit: bool,
}

impl App {
    fn new() -> Self {
        let (tx, rx) = channel();
        let (model, engine) = agent::config_summary();
        Self {
            live: Vec::new(),
            scrollback: Vec::new(),
            input: String::new(),
            history: Vec::new(),
            history_pos: None,
            busy: false,
            status: "ready".into(),
            model,
            engine,
            online: agent::agent_online(),
            spinner_start: Instant::now(),
            cursor_start: Instant::now(),
            tx,
            rx,
            quit: false,
        }
    }

    /// Move every finished message out of the viewport and into scrollback.
    ///
    /// Called once a turn ends, so the inline viewport only ever holds the
    /// current exchange and the terminal keeps its own scroll history.
    fn retire_live(&mut self) {
        for msg in self.live.drain(..) {
            let prefix = match msg.kind {
                Kind::User => "› ",
                Kind::Agent => "",
                Kind::Tool => "  ",
            };
            if msg.kind == Kind::Tool {
                self.scrollback.push(format!("| {}", msg.tool_name));
            }
            for line in msg.text.lines() {
                self.scrollback.push(format!("{prefix}{line}"));
            }
            self.scrollback.push(String::new());
        }
    }

    fn take_scrollback(&mut self) -> Vec<Line<'static>> {
        self.scrollback.drain(..).map(Line::from).collect()
    }

    fn submit(&mut self) {
        let prompt = self.input.trim().to_string();
        if prompt.is_empty() || self.busy {
            return;
        }
        self.input.clear();
        self.history_pos = None;
        if self.history.last().map(String::as_str) != Some(prompt.as_str()) {
            self.history.push(prompt.clone());
            if self.history.len() > MAX_HISTORY {
                self.history.remove(0);
            }
        }

        if prompt == "/quit" || prompt == "/exit" {
            self.quit = true;
            return;
        }

        self.retire_live();
        self.live.push(Msg {
            kind: Kind::User,
            tool_name: String::new(),
            text: prompt.clone(),
        });
        self.busy = true;
        self.status = "thinking…".into();
        self.spinner_start = Instant::now();

        // ponytail: one thread per turn. Turns are serialized by `busy`, so a
        // pool buys nothing here.
        let tx = self.tx.clone();
        std::thread::spawn(move || agent::run_turn(&prompt, CHAT_ID, &tx));
    }

    fn append_delta(&mut self, text: &str) {
        match self.live.last_mut() {
            Some(msg) if msg.kind == Kind::Agent => msg.text.push_str(text),
            _ => self.live.push(Msg {
                kind: Kind::Agent,
                tool_name: String::new(),
                text: text.to_string(),
            }),
        }
    }

    fn handle_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Status(message) => self.status = message,
            AgentEvent::ToolStart { name, hint } => self.live.push(Msg {
                kind: Kind::Tool,
                tool_name: name,
                text: hint,
            }),
            AgentEvent::ToolEnd { name, ok, secs } => {
                self.status = if ok {
                    format!("{name} ok · {secs}s")
                } else {
                    format!("{name} failed · {secs}s")
                };
            }
            AgentEvent::Delta(text) => self.append_delta(&text),
            AgentEvent::Done(text) => {
                // Streaming already produced the body; only use `response`
                // when nothing was streamed (the HTTP/CLI fallback path).
                let streamed = self
                    .live
                    .last()
                    .map(|m| m.kind == Kind::Agent)
                    .unwrap_or(false);
                if !streamed && !text.is_empty() {
                    self.append_delta(&text);
                }
                self.busy = false;
                self.status = "ready".into();
                self.online = true;
            }
            AgentEvent::Error(message) => {
                self.append_delta(&format!("\nerror: {message}"));
                self.busy = false;
                self.status = "error".into();
                self.online = agent::agent_online();
            }
        }
    }

    fn history_step(&mut self, back: bool) {
        if self.history.is_empty() {
            return;
        }
        let last = self.history.len() - 1;
        self.history_pos = match (self.history_pos, back) {
            (None, true) => Some(last),
            (None, false) => return,
            (Some(0), true) => Some(0),
            (Some(index), true) => Some(index - 1),
            (Some(index), false) if index >= last => {
                self.history_pos = None;
                self.input.clear();
                return;
            }
            (Some(index), false) => Some(index + 1),
        };
        if let Some(index) = self.history_pos {
            self.input = self.history[index].clone();
        }
    }

    fn update_template(&self, tpl: &mut Template) {
        let live = self
            .live
            .iter()
            .map(|msg| {
                let mut row = TemplateContext::new();
                row.set("is_tool", msg.kind == Kind::Tool);
                row.set("is_user", msg.kind == Kind::User);
                row.set("tool_name", msg.tool_name.clone());
                let lines = msg
                    .text
                    .lines()
                    .map(|text| {
                        let mut line = TemplateContext::new();
                        line.set("text", text.to_string());
                        line
                    })
                    .collect();
                row.set("lines", TemplateValue::List(lines));
                row
            })
            .collect();
        tpl.set("live", TemplateValue::List(live));
        tpl.set("input", self.input.clone());
        tpl.set(
            "input_color",
            if self.busy { "zinc-600" } else { "zinc-100" },
        );
        tpl.set("cursor", blink_cursor(self.cursor_start));
        tpl.set("busy", self.busy);
        tpl.set("spinner", spinner_frame(self.spinner_start));
        tpl.set("status", self.status.clone());
        tpl.set("model", self.model.clone());
        tpl.set("engine", self.engine.clone());
        tpl.set("conn", if self.online { "connected" } else { "offline" });
        tpl.set(
            "conn_color",
            if self.online { "green-400" } else { "red-400" },
        );
        tpl.set("project", project_name());
    }
}

fn main() -> anyhow::Result<()> {
    let mut app = App::new();
    let mut tpl = Template::from_source(include_str!("../shell.crepus"));

    enable_raw_mode()?;
    let mut out = stdout();
    out.flush()?;
    let mut terminal = crepuscularity_tui::ratatui::Terminal::with_options(
        CrosstermBackend::new(out),
        crepuscularity_tui::ratatui::TerminalOptions {
            viewport: crepuscularity_tui::ratatui::Viewport::Inline(VIEWPORT_ROWS),
        },
    )?;

    let result = run(&mut app, &mut tpl, &mut terminal);

    disable_raw_mode()?;
    let _ = terminal.clear();
    result
}

type Term = crepuscularity_tui::ratatui::Terminal<CrosstermBackend<std::io::Stdout>>;

/// Push any retired transcript lines into the terminal's native scrollback.
fn flush_scrollback(app: &mut App, terminal: &mut Term) -> anyhow::Result<()> {
    let scrollback = app.take_scrollback();
    if scrollback.is_empty() {
        return Ok(());
    }
    let width = terminal.size()?.width;
    terminal.insert_before(scrollback.len() as u16, |buffer| {
        for (index, line) in scrollback.iter().enumerate() {
            buffer.set_line(0, index as u16, line, width);
        }
    })?;
    Ok(())
}

fn run(app: &mut App, tpl: &mut Template, terminal: &mut Term) -> anyhow::Result<()> {
    while !app.quit {
        while let Ok(event) = app.rx.try_recv() {
            app.handle_event(event);
        }

        flush_scrollback(app, terminal)?;

        app.update_template(tpl);
        if !tpl.changed_keys().is_empty() {
            terminal.draw(|f| {
                if let Err(e) = tpl.draw(f, f.area()) {
                    use crepuscularity_tui::ratatui::style::{Color, Style};
                    use crepuscularity_tui::ratatui::widgets::Paragraph;
                    let p = Paragraph::new(format!("Template error: {e}"))
                        .style(Style::default().fg(Color::Red));
                    f.render_widget(p, f.area());
                }
            })?;
            tpl.mark_rendered();
        }

        if crossterm::event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = crossterm::event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('c') | KeyCode::Char('d')
                        if key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        app.quit = true;
                    }
                    KeyCode::Enter => app.submit(),
                    KeyCode::Backspace => {
                        app.input.pop();
                    }
                    KeyCode::Up => app.history_step(true),
                    KeyCode::Down => app.history_step(false),
                    KeyCode::Char(c) => app.input.push(c),
                    _ => {}
                }
            }
        }
    }

    app.retire_live();
    flush_scrollback(app, terminal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App::new()
    }

    #[test]
    fn deltas_coalesce_into_one_agent_message() {
        let mut a = app();
        a.handle_event(AgentEvent::Delta("hel".into()));
        a.handle_event(AgentEvent::Delta("lo".into()));
        assert_eq!(a.live.len(), 1);
        assert_eq!(a.live[0].text, "hello");
    }

    #[test]
    fn done_does_not_duplicate_streamed_text() {
        let mut a = app();
        a.handle_event(AgentEvent::Delta("hello".into()));
        a.handle_event(AgentEvent::Done("hello".into()));
        assert_eq!(a.live.len(), 1);
        assert_eq!(a.live[0].text, "hello");
        assert!(!a.busy);
    }

    #[test]
    fn done_without_stream_uses_the_response_body() {
        let mut a = app();
        a.handle_event(AgentEvent::Done("fallback".into()));
        assert_eq!(a.live.len(), 1);
        assert_eq!(a.live[0].text, "fallback");
    }

    #[test]
    fn a_tool_call_breaks_the_agent_message_run() {
        let mut a = app();
        a.handle_event(AgentEvent::Delta("before".into()));
        a.handle_event(AgentEvent::ToolStart {
            name: "shell".into(),
            hint: "ls".into(),
        });
        a.handle_event(AgentEvent::Delta("after".into()));
        assert_eq!(a.live.len(), 3);
        assert_eq!(a.live[2].text, "after");
    }

    #[test]
    fn retiring_moves_the_transcript_to_scrollback() {
        let mut a = app();
        a.live.push(Msg {
            kind: Kind::User,
            tool_name: String::new(),
            text: "hi".into(),
        });
        a.retire_live();
        assert!(a.live.is_empty());
        assert_eq!(a.scrollback[0], "› hi");
    }

    #[test]
    fn history_walks_back_and_forward() {
        let mut a = app();
        a.history = vec!["one".into(), "two".into()];
        a.history_step(true);
        assert_eq!(a.input, "two");
        a.history_step(true);
        assert_eq!(a.input, "one");
        a.history_step(false);
        assert_eq!(a.input, "two");
        a.history_step(false);
        assert_eq!(a.input, "");
    }
}
