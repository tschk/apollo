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

use agent::{AgentEvent, AgentState};
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
/// How many model rows the selector shows at once.
const MODEL_ROWS: usize = 5;

/// Slash commands, in palette order.
///
/// apollo's server-side surface decides what can be here: telekinesis'
/// /scope, /subagent, /todo, /budget, /review, /mcp and /plan have no apollo
/// endpoint to drive, so they are absent rather than inert.
const COMMANDS: [(&str, &str); 9] = [
    ("/help", "keys and commands"),
    ("/model", "switch model — no argument opens the selector"),
    ("/clear", "clear the transcript"),
    ("/cost", "spend and tokens so far"),
    ("/context", "context use against the compaction budget"),
    ("/compact", "ask the agent to compact this chat"),
    ("/project", "project, branch and agent connection"),
    ("/quit", "leave"),
    ("/exit", "leave"),
];

/// Models apollo's own `/models` command advertises.
const ANTHROPIC_MODELS: [&str; 3] = ["claude-sonnet-4-5", "claude-opus-4", "claude-haiku-3-5"];

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

/// One update the UI thread consumes.
///
/// `AgentEvent` is shared with apollo-ui, so state refreshes ride alongside it
/// here rather than being added to that enum.
enum UiEvent {
    Agent(AgentEvent),
    State(AgentState),
}

fn spinner_frame(start: Instant) -> &'static str {
    let index = (start.elapsed().as_millis() / 80) as usize % SPINNER_FRAMES.len();
    SPINNER_FRAMES[index]
}

/// Idle spinner frames would rotate every 80ms and defeat the changed_keys()
/// redraw gate, repainting a still screen at 12Hz — so idle is empty.
fn spinner_text(busy: bool, start: Instant) -> &'static str {
    if busy {
        spinner_frame(start)
    } else {
        ""
    }
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

fn git_branch() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "-".to_string())
}

fn context_color(pct: u8) -> &'static str {
    if pct >= 90 {
        "red-400"
    } else if pct >= 70 {
        "amber-400"
    } else {
        "green-400"
    }
}

fn format_tokens(count: u64) -> String {
    if count < 1000 {
        count.to_string()
    } else if count < 1_000_000 {
        format!("{}k", count / 1000)
    } else {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    }
}

/// Commands whose name starts with what has been typed.
fn palette_matches(input: &str) -> Vec<(&'static str, &'static str)> {
    if !input.starts_with('/') || input.contains(' ') {
        return Vec::new();
    }
    COMMANDS
        .iter()
        .filter(|(name, _)| name.starts_with(input))
        .copied()
        .collect()
}

/// Models the selector can offer, grouped by provider.
///
/// Everything here comes from the machine rather than a guess: the configured
/// provider and models in `apollo.json`, the set apollo's own `/models`
/// command advertises, and an optional `~/.apollo/models.json` map of
/// `{"provider": ["model", …]}` for anyone running something else.
fn model_catalogue() -> Vec<(String, Vec<String>)> {
    let mut catalogue: Vec<(String, Vec<String>)> = Vec::new();
    let mut push = |provider: &str, model: &str| {
        if model.is_empty() {
            return;
        }
        match catalogue.iter_mut().find(|(name, _)| name == provider) {
            Some((_, models)) => {
                if !models.iter().any(|m| m == model) {
                    models.push(model.to_string());
                }
            }
            None => catalogue.push((provider.to_string(), vec![model.to_string()])),
        }
    };

    let config = std::fs::read_to_string("apollo.json")
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    let provider = config
        .as_ref()
        .and_then(|v| v.get("provider"))
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("anthropic")
        .to_string();
    if let Some(config) = &config {
        for key in ["model"] {
            if let Some(model) = config.get(key).and_then(|m| m.as_str()) {
                push(&provider, model);
            }
        }
        for key in ["fast_model", "heavy_model"] {
            if let Some(model) = config
                .get("agent")
                .and_then(|a| a.get(key))
                .and_then(|m| m.as_str())
            {
                push(&provider, model);
            }
        }
    }
    if provider == "anthropic" {
        for model in ANTHROPIC_MODELS {
            push(&provider, model);
        }
    }

    if let Some(home) = std::env::var_os("HOME") {
        let path = std::path::PathBuf::from(home).join(".apollo/models.json");
        if let Some(map) = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|value| value.as_object().cloned())
        {
            for (name, models) in map {
                for model in models.as_array().into_iter().flatten() {
                    if let Some(model) = model.as_str() {
                        push(&name, model);
                    }
                }
            }
        }
    }

    for (_, models) in catalogue.iter_mut() {
        models.sort();
    }
    catalogue
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
    mode: String,
    cost_usd: f64,
    context_pct: u8,
    context_window: u64,
    project: String,
    branch: String,
    online: bool,
    show_header: bool,
    palette_choice: usize,
    selecting_model: bool,
    catalogue: Vec<(String, Vec<String>)>,
    provider_choice: usize,
    model_choice: Option<usize>,
    spinner_start: Instant,
    cursor_start: Instant,
    tx: Sender<UiEvent>,
    rx: Receiver<UiEvent>,
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
            mode: "—".into(),
            cost_usd: 0.0,
            context_pct: 0,
            context_window: 0,
            project: project_name(),
            branch: git_branch(),
            online: agent::agent_online(),
            show_header: false,
            palette_choice: 0,
            selecting_model: false,
            catalogue: model_catalogue(),
            provider_choice: 0,
            model_choice: None,
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

    fn note(&mut self, text: impl Into<String>) {
        self.live.push(Msg {
            kind: Kind::Agent,
            tool_name: String::new(),
            text: text.into(),
        });
    }

    /// Refresh model, engine, mode, cost and context from the running agent.
    fn refresh_state(&self) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            if let Some(state) = agent::fetch_state() {
                let _ = tx.send(UiEvent::State(state));
            }
        });
    }

    fn apply_state(&mut self, state: AgentState) {
        self.model = state.model;
        self.engine = state.engine;
        self.mode = state.mode;
        self.cost_usd = state.cost_usd;
        self.context_pct = state.context_pct;
        self.context_window = state.context_window;
        self.online = true;
    }

    fn submit(&mut self) {
        let prompt = self.input.trim().to_string();
        if prompt.is_empty() || self.busy {
            return;
        }
        self.input.clear();
        self.palette_choice = 0;
        self.history_pos = None;
        if self.history.last().map(String::as_str) != Some(prompt.as_str()) {
            self.history.push(prompt.clone());
            if self.history.len() > MAX_HISTORY {
                self.history.remove(0);
            }
        }

        if prompt.starts_with('/') {
            self.run_command(&prompt);
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
        std::thread::spawn(move || {
            let (relay, events) = channel();
            let forward = std::thread::spawn(move || {
                for event in events {
                    if tx.send(UiEvent::Agent(event)).is_err() {
                        return;
                    }
                }
            });
            agent::run_turn(&prompt, CHAT_ID, &relay);
            drop(relay);
            let _ = forward.join();
        });
    }

    fn run_command(&mut self, prompt: &str) {
        let (command, argument) = match prompt.split_once(' ') {
            Some((command, argument)) => (command, argument.trim()),
            None => (prompt, ""),
        };
        match command {
            "/quit" | "/exit" => self.quit = true,
            "/help" => {
                let mut text = String::from("commands:\n");
                for (name, hint) in COMMANDS {
                    text.push_str(&format!("  {name} — {hint}\n"));
                }
                text.push_str(
                    "keys: f1 header · tab complete · up/down history · \
                     model selector: type to filter, ←/→ provider, ↑/↓ model, \
                     enter apply, esc cancel · ctrl+c quit",
                );
                self.note(text);
            }
            "/model" => {
                if argument.is_empty() {
                    self.open_model_selector();
                } else {
                    self.apply_model(argument.to_string());
                }
            }
            "/clear" => {
                self.live.clear();
                self.scrollback.clear();
                self.status = "transcript cleared".into();
            }
            "/cost" => {
                let text = format!("${:.4} spent this session", self.cost_usd);
                self.note(text);
                self.refresh_state();
            }
            "/context" => {
                let text = format!(
                    "{}% of a {} token compaction budget",
                    self.context_pct,
                    format_tokens(self.context_window)
                );
                self.note(text);
                self.refresh_state();
            }
            "/compact" => {
                let result = agent::post_chat_action("/v1/compact", CHAT_ID);
                match result {
                    Ok(message) => self.note(message),
                    Err(message) => self.note(format!("compact: {message}")),
                }
            }
            "/project" => {
                let text = format!(
                    "{} ({}) · agent {} on port {}",
                    self.project,
                    self.branch,
                    if self.online { "connected" } else { "offline" },
                    agent::http_port()
                );
                self.note(text);
            }
            other => self.note(format!("unknown command: {other} — try /help")),
        }
    }

    fn apply_model(&mut self, model: String) {
        match agent::set_model(&model) {
            Ok(state) => {
                self.apply_state(state);
                self.status = format!("model → {model}");
            }
            Err(e) => {
                self.note(format!("model: {e}"));
                self.status = "model unchanged".into();
            }
        }
    }

    // ── Model selector ────────────────────────────────────────────────────

    fn open_model_selector(&mut self) {
        if self.catalogue.is_empty() {
            self.note("no models configured — add ~/.apollo/models.json");
            return;
        }
        if let Some(index) = self
            .catalogue
            .iter()
            .position(|(_, models)| models.iter().any(|m| *m == self.model))
        {
            self.provider_choice = index;
        }
        self.input.clear();
        self.selecting_model = true;
        self.reset_model_choice();
    }

    fn close_model_selector(&mut self) {
        self.selecting_model = false;
        self.model_choice = None;
        self.input.clear();
    }

    fn filtered_models(&self) -> Vec<&String> {
        let Some((_, models)) = self.catalogue.get(self.provider_choice) else {
            return Vec::new();
        };
        let query = self.input.to_ascii_lowercase();
        models
            .iter()
            .filter(|model| model.to_ascii_lowercase().contains(&query))
            .collect()
    }

    fn reset_model_choice(&mut self) {
        let models = self.filtered_models();
        self.model_choice = models
            .iter()
            .position(|model| **model == self.model)
            .or((!models.is_empty()).then_some(0));
    }

    fn move_provider_choice(&mut self, offset: isize) {
        if !self.selecting_model || self.catalogue.is_empty() {
            return;
        }
        let start = self.provider_choice;
        loop {
            self.provider_choice = (self.provider_choice as isize + offset)
                .rem_euclid(self.catalogue.len() as isize)
                as usize;
            self.reset_model_choice();
            if self.model_choice.is_some() || self.provider_choice == start {
                break;
            }
        }
    }

    fn move_model_choice(&mut self, offset: isize) {
        let Some(index) = self.model_choice else {
            return;
        };
        let len = self.filtered_models().len();
        if len != 0 {
            self.model_choice = Some((index as isize + offset).rem_euclid(len as isize) as usize);
        }
    }

    fn choose_model(&mut self) {
        let Some(index) = self.model_choice else {
            return;
        };
        let Some(model) = self.filtered_models().get(index).map(|m| (*m).clone()) else {
            return;
        };
        self.close_model_selector();
        self.apply_model(model);
    }

    // ── Palette ───────────────────────────────────────────────────────────

    fn palette(&self) -> Vec<(&'static str, &'static str)> {
        if self.selecting_model {
            return Vec::new();
        }
        palette_matches(&self.input)
    }

    fn move_palette_choice(&mut self, offset: isize) -> bool {
        let len = self.palette().len();
        if len == 0 {
            return false;
        }
        self.palette_choice =
            (self.palette_choice as isize + offset).rem_euclid(len as isize) as usize;
        true
    }

    fn complete_palette(&mut self) {
        let palette = self.palette();
        if palette.is_empty() {
            return;
        }
        let index = self.palette_choice.min(palette.len() - 1);
        self.input = palette[index].0.to_string();
        self.palette_choice = 0;
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
                self.refresh_state();
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
        tpl.set("show_header", self.show_header);
        tpl.set("version", env!("CARGO_PKG_VERSION"));
        tpl.set("spinner", spinner_text(self.busy, self.spinner_start));
        tpl.set("status", self.status.clone());
        tpl.set("model", self.model.clone());
        tpl.set("engine", self.engine.clone());
        tpl.set("mode", self.mode.clone());
        tpl.set("cost", format!("{:.3}", self.cost_usd));
        tpl.set("context_pct", self.context_pct.to_string());
        tpl.set("context_window", format_tokens(self.context_window));
        tpl.set("context_color", context_color(self.context_pct));
        tpl.set("conn", if self.online { "connected" } else { "offline" });
        tpl.set(
            "conn_color",
            if self.online { "green-400" } else { "red-400" },
        );
        tpl.set("project", self.project.clone());
        tpl.set("branch", self.branch.clone());

        let palette = self.palette();
        tpl.set("show_palette", !palette.is_empty());
        let rows = palette
            .iter()
            .enumerate()
            .map(|(index, (name, hint))| {
                let mut row = TemplateContext::new();
                row.set("name", name.to_string());
                row.set("hint", hint.to_string());
                row.set(
                    "selected",
                    index == self.palette_choice.min(palette.len() - 1),
                );
                row
            })
            .collect();
        tpl.set("palette", TemplateValue::List(rows));

        tpl.set("selecting_model", self.selecting_model);
        tpl.set(
            "selected_provider",
            self.catalogue
                .get(self.provider_choice)
                .map(|(name, _)| name.clone())
                .unwrap_or_default(),
        );
        let filtered = self.filtered_models();
        let model_rows = filtered
            .iter()
            .enumerate()
            .skip(self.model_choice.unwrap_or_default().saturating_sub(2))
            .take(MODEL_ROWS)
            .map(|(index, model)| {
                let mut row = TemplateContext::new();
                row.set("model_id", (*model).clone());
                row.set("selected", Some(index) == self.model_choice);
                row
            })
            .collect();
        tpl.set("model_rows", TemplateValue::List(model_rows));
    }
}

fn main() -> anyhow::Result<()> {
    let mut app = App::new();
    app.refresh_state();
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
            match event {
                UiEvent::Agent(event) => app.handle_event(event),
                UiEvent::State(state) => app.apply_state(state),
            }
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
                    KeyCode::F(1) => app.show_header = !app.show_header,
                    KeyCode::Esc if app.selecting_model => app.close_model_selector(),
                    KeyCode::Enter if app.selecting_model => app.choose_model(),
                    KeyCode::Left if app.selecting_model => app.move_provider_choice(-1),
                    KeyCode::Right if app.selecting_model => app.move_provider_choice(1),
                    KeyCode::Enter => app.submit(),
                    KeyCode::Tab => app.complete_palette(),
                    KeyCode::Backspace => {
                        app.input.pop();
                        if app.selecting_model {
                            app.reset_model_choice();
                        }
                        app.palette_choice = 0;
                    }
                    KeyCode::Up => {
                        if app.selecting_model {
                            app.move_model_choice(-1);
                        } else if !app.move_palette_choice(-1) {
                            app.history_step(true);
                        }
                    }
                    KeyCode::Down => {
                        if app.selecting_model {
                            app.move_model_choice(1);
                        } else if !app.move_palette_choice(1) {
                            app.history_step(false);
                        }
                    }
                    KeyCode::Char(c) => {
                        app.input.push(c);
                        if app.selecting_model {
                            app.reset_model_choice();
                        }
                        app.palette_choice = 0;
                    }
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

    #[test]
    fn the_palette_only_offers_matching_commands() {
        assert!(palette_matches("hello").is_empty());
        assert!(palette_matches("/model claude").is_empty());
        assert_eq!(palette_matches("/co").len(), 3);
        assert!(palette_matches("/comp")
            .iter()
            .all(|(name, _)| *name == "/compact"));
        assert_eq!(palette_matches("/").len(), COMMANDS.len());
    }

    #[test]
    fn tab_completes_the_selected_command() {
        let mut a = app();
        a.input = "/he".into();
        a.complete_palette();
        assert_eq!(a.input, "/help");
    }

    #[test]
    fn arrow_keys_walk_the_palette_before_the_history() {
        let mut a = app();
        a.history = vec!["earlier".into()];
        a.input = "/c".into();
        assert!(a.move_palette_choice(1));
        assert_eq!(a.input, "/c");
        a.input = "plain".into();
        assert!(!a.move_palette_choice(1));
    }

    #[test]
    fn help_lists_every_command() {
        let mut a = app();
        a.run_command("/help");
        let text = &a.live[0].text;
        for (name, _) in COMMANDS {
            assert!(text.contains(name), "missing {name}");
        }
    }

    #[test]
    fn quit_commands_end_the_session() {
        for command in ["/quit", "/exit"] {
            let mut a = app();
            a.run_command(command);
            assert!(a.quit, "{command} did not quit");
        }
    }

    #[test]
    fn clear_empties_the_transcript() {
        let mut a = app();
        a.note("something");
        a.scrollback.push("old".into());
        a.run_command("/clear");
        assert!(a.live.is_empty());
        assert!(a.scrollback.is_empty());
    }

    #[test]
    fn an_unknown_command_says_so_instead_of_being_sent() {
        let mut a = app();
        a.run_command("/nope");
        assert!(a.live[0].text.contains("unknown command"));
    }

    #[test]
    fn the_selector_filters_models_and_walks_providers() {
        let mut a = app();
        a.catalogue = vec![
            (
                "anthropic".into(),
                vec!["claude-haiku-3-5".into(), "claude-opus-4".into()],
            ),
            ("ollama".into(), vec!["qwen".into()]),
        ];
        a.model = "claude-opus-4".into();
        a.open_model_selector();
        assert!(a.selecting_model);
        assert_eq!(a.model_choice, Some(1));
        a.input = "haiku".into();
        a.reset_model_choice();
        assert_eq!(a.filtered_models().len(), 1);
        assert_eq!(a.model_choice, Some(0));
        a.input.clear();
        a.reset_model_choice();
        a.move_provider_choice(1);
        assert_eq!(a.provider_choice, 1);
        assert_eq!(a.filtered_models(), vec![&"qwen".to_string()]);
        a.close_model_selector();
        assert!(!a.selecting_model);
        assert!(a.model_choice.is_none());
    }

    #[test]
    fn model_choice_wraps_in_both_directions() {
        let mut a = app();
        a.catalogue = vec![("p".into(), vec!["a".into(), "b".into(), "c".into()])];
        a.open_model_selector();
        assert_eq!(a.model_choice, Some(0));
        a.move_model_choice(-1);
        assert_eq!(a.model_choice, Some(2));
        a.move_model_choice(1);
        assert_eq!(a.model_choice, Some(0));
    }

    #[test]
    fn context_colours_follow_the_thresholds() {
        assert_eq!(context_color(0), "green-400");
        assert_eq!(context_color(69), "green-400");
        assert_eq!(context_color(70), "amber-400");
        assert_eq!(context_color(89), "amber-400");
        assert_eq!(context_color(90), "red-400");
        assert_eq!(context_color(100), "red-400");
    }

    #[test]
    fn token_counts_are_abbreviated() {
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(32_000), "32k");
        assert_eq!(format_tokens(1_500_000), "1.5M");
    }

    #[test]
    fn state_from_the_server_replaces_the_config_guess() {
        let mut a = app();
        a.apply_state(AgentState {
            model: "claude-opus-4".into(),
            engine: "rx4".into(),
            mode: "coding".into(),
            cost_usd: 1.5,
            total_tokens: 10,
            call_count: 2,
            context_tokens: 900,
            context_window: 1000,
            context_pct: 90,
        });
        assert_eq!(a.model, "claude-opus-4");
        assert_eq!(a.engine, "rx4");
        assert_eq!(a.mode, "coding");
        assert_eq!(context_color(a.context_pct), "red-400");
        assert!(a.online);
    }

    #[test]
    fn the_spinner_stays_empty_while_idle() {
        let start = Instant::now() - Duration::from_millis(500);
        assert_eq!(spinner_text(false, start), "");
        assert_eq!(spinner_text(false, Instant::now()), "");
        assert!(!spinner_text(true, start).is_empty());
    }
}
