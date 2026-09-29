//! The main window around the chat: simple and advanced layouts, the
//! instance switcher, settings, tools/permissions and the session log.
//!
//! Simple mode has no chrome beyond an instance pill and a settings link in
//! the top-right corner. Advanced mode borrows OpenMausBot's roster (every
//! instance is a row with an initial, name and model; click to switch) and
//! Hermes' settings layout (one column of sections), plus tools, model
//! parameters and logs.

use std::sync::mpsc::{channel, TryRecvError};
use std::time::{Duration, Instant};

use crepuscularity_gpui::prelude::*;
use gpui::{
    ease_out_quint, Animation, AnyElement, ClickEvent, MouseButton, MouseDownEvent, SharedString,
};

use crate::agent::{self, AgentEvent};
use crate::models::{self, ModelList};
use crate::onboarding::Purpose;
use crate::setup::{self, Auth, Mode, ProviderInfo, PROFILES};
use crate::theme::*;
use crate::{blink_cursor, spinner_frame, ApolloView, CHAT_CONTEXT};

const GHOST: u32 = 0x52525b;
const SOFT: u32 = 0xa1a1aa;
/// Between BG and SURFACE, so the roster reads as a separate column.
const SIDEBAR: u32 = 0x0f0f11;

/// Avatar colors offered on the profile panel.
const AVATAR_COLORS: [u32; 6] = [
    0x8b5cf6, // violet
    0x3b82f6, // blue
    0x10b981, // emerald
    0xf59e0b, // amber
    0xef4444, // red
    0xec4899, // pink
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Chat,
    Profile,
    Tools,
    Logs,
    Settings,
}

impl Panel {
    fn label(self) -> &'static str {
        match self {
            Panel::Chat => "chat",
            Panel::Profile => "profile",
            Panel::Tools => "tools",
            Panel::Logs => "logs",
            Panel::Settings => "settings",
        }
    }
}

/// Which profile field the keyboard is editing (see `ApolloView::edit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditField {
    Name,
    Instructions,
}

/// One provider row in the model picker's left rail. `reason` is `None`
/// when a credential exists and the provider is switchable.
pub(crate) struct RailEntry {
    provider: &'static ProviderInfo,
    reason: Option<&'static str>,
}

/// The model picker overlay: providers on the left, their models on the
/// right. Built by `ApolloView::open_picker`.
pub(crate) struct PickerState {
    rail: Vec<RailEntry>,
    highlighted: usize,
    models: Option<ModelList>,
    loading: bool,
    note: Option<String>,
    /// Bumped per fetch so a slow, stale answer is dropped.
    gen: u64,
    /// Typed filter. A non-match is kept as a custom model id.
    pub(crate) query: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    Info,
    Tool,
    Ok,
    Error,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub at: String,
    pub kind: LogKind,
    pub text: String,
}

impl LogLine {
    pub fn new(kind: LogKind, text: impl Into<String>) -> Self {
        Self {
            at: chrono::Local::now().format("%H:%M:%S").to_string(),
            kind,
            text: text.into(),
        }
    }
}

/// Model parameters exposed in advanced settings: `agent.<key>` in
/// apollo.json, with apollo's defaults and a sensible step.
const PARAMS: [(&str, &str, u64, u64); 3] = [
    ("max_rounds", "max tool rounds per turn", 50, 5),
    ("max_history_messages", "history messages sent", 10, 2),
    ("auto_compact_after", "auto-compact after (0 = off)", 0, 10),
];

fn initial(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "a".into())
}

impl ApolloView {
    pub(crate) fn log(&mut self, kind: LogKind, text: impl Into<String>) {
        self.logs.push(LogLine::new(kind, text));
        if self.logs.len() > 500 {
            self.logs.remove(0);
        }
    }

    pub(crate) fn log_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Status(s) => self.log(LogKind::Info, s.clone()),
            AgentEvent::ToolStart { name, hint } => {
                self.log(LogKind::Tool, format!("| {name} {hint}"))
            }
            AgentEvent::ToolEnd { name, ok, secs } => self.log(
                if *ok { LogKind::Ok } else { LogKind::Error },
                format!("| {name} {} {secs}s", if *ok { "ok" } else { "failed" }),
            ),
            AgentEvent::Delta(_) => {}
            AgentEvent::Done(text) => self.log(
                LogKind::Ok,
                format!("turn {} done ({} chars)", self.turns, text.chars().count()),
            ),
            AgentEvent::Error(e) => self.log(LogKind::Error, e.clone()),
        }
    }

    // ── Actions ────────────────────────────────────────────────────────────

    fn show(&mut self, panel: Panel, cx: &mut Context<Self>) {
        self.panel = panel;
        self.switcher_open = false;
        self.roster_menu = None;
        self.edit = None;
        self.edit_buf.clear();
        self.notice.clear();
        cx.notify();
    }

    fn switch_to(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.switcher_open = false;
        self.roster_menu = None;
        if id == self.instance.id || self.busy {
            cx.notify();
            return;
        }
        let mut state = self.state.clone();
        state.active = Some(id);
        if let Err(e) = state.save() {
            self.notice = e;
            cx.notify();
            return;
        }
        crate::open_chat(state, window, cx);
    }

    fn new_instance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        crate::open_onboarding(Purpose::NewInstance, window, cx);
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.state.mode = mode;
        self.notice = match self.state.save() {
            Ok(_) => format!(
                "{} mode saved",
                if mode == Mode::Simple {
                    "simple"
                } else {
                    "advanced"
                }
            ),
            Err(e) => e,
        };
        if mode == Mode::Simple && matches!(self.panel, Panel::Tools | Panel::Logs) {
            self.panel = Panel::Settings;
        }
        cx.notify();
    }

    fn set_profile(&mut self, profile: &'static str, cx: &mut Context<Self>) {
        let path = self.instance.config_path();
        match setup::update_config(&path, |c| setup::apply_profile(c, profile)) {
            Ok(_) => {
                self.instance.permission_profile = profile.to_string();
                self.state.upsert(self.instance.clone());
                let _ = self.state.save();
                self.notice = format!("permission profile → {profile} (next turn)");
                self.log(LogKind::Info, self.notice.clone());
            }
            Err(e) => self.notice = e,
        }
        cx.notify();
    }

    fn param(&self, key: &str, default: u64) -> u64 {
        setup::read_config(&self.instance.config_path())["agent"][key]
            .as_u64()
            .unwrap_or(default)
    }

    fn step_param(&mut self, key: &'static str, default: u64, delta: i64, cx: &mut Context<Self>) {
        let current = self.param(key, default) as i64;
        let min = if key == "auto_compact_after" { 0 } else { 1 };
        let next = (current + delta).max(min) as u64;
        let path = self.instance.config_path();
        let result = setup::update_config(&path, |c| {
            if !c["agent"].is_object() {
                c["agent"] = serde_json::json!({});
            }
            c["agent"][key] = serde_json::json!(next);
        });
        self.notice = match result {
            Ok(_) => {
                let msg = format!("agent.{key} = {next}");
                self.log(LogKind::Info, format!("settings: {msg}"));
                msg
            }
            Err(e) => e,
        };
        cx.notify();
    }

    fn current_effort(&self) -> String {
        setup::read_config(&self.instance.config_path())["agent"]["reasoning_effort"]
            .as_str()
            .and_then(setup::normalize_effort)
            .unwrap_or("medium")
            .to_string()
    }

    fn set_effort(&mut self, effort: &str, cx: &mut Context<Self>) {
        let path = self.instance.config_path();
        let dir = path.parent().map(|p| p.to_path_buf());
        self.notice = match setup::update_config(&path, |c| {
            if !c["agent"].is_object() {
                c["agent"] = serde_json::json!({});
            }
            c["agent"]["reasoning_effort"] = serde_json::json!(effort);
        }) {
            Ok(_) => {
                let _ = setup::write_dial(&path, "main", None, Some(effort));
                let _ = dir;
                format!("effort set to {effort} · next message")
            }
            Err(e) => e,
        };
        cx.notify();
    }

    fn effort_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.current_effort();
        let Some(levels) = models::effort_levels(&self.model) else {
            return div()
                .text_xs()
                .text_color(rgb(GHOST))
                .child("this model has no effort dial")
                .into_any_element();
        };
        let chips = levels.iter().enumerate().map(|(i, level)| {
            let on = current == *level;
            let id_owned = level.clone();
            div()
                .id(("settings-effort", i))
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(rgb(if on { MUTED } else { SURFACE_2 }))
                .bg(rgb(if on { SURFACE } else { BG }))
                .text_xs()
                .text_color(rgb(if on { ACCENT } else { SOFT }))
                .cursor_pointer()
                .hover(|style| style.bg(rgb(SURFACE)))
                .child(SharedString::from(level.clone()))
                .on_click(
                    cx.listener(move |view, _: &ClickEvent, _, cx| view.set_effort(&id_owned, cx)),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().flex().flex_row().gap_2().children(chips))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(GHOST))
                    .child("only the levels this model offers. the next message uses it."),
            )
            .into_any_element()
    }

    // ── Profile panel ──────────────────────────────────────────────────────

    /// The current value of a profile field, straight from where it lives.
    fn field_value(&self, field: EditField) -> String {
        match field {
            EditField::Name => self.instance.name.clone(),
            EditField::Instructions => setup::read_config(&self.instance.config_path())
                ["system_prompt"]
                .as_str()
                .unwrap_or("")
                .to_string(),
        }
    }

    fn start_edit(&mut self, field: EditField, cx: &mut Context<Self>) {
        self.edit = Some(field);
        self.edit_buf = self.field_value(field);
        self.cursor_start = Instant::now();
        cx.notify();
    }

    pub(crate) fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        self.edit = None;
        self.edit_buf.clear();
        cx.notify();
    }

    /// Write the edited field where apollo reads it: the name lives in
    /// ~/.apollo/desktop.json, the instructions in apollo.json's
    /// `system_prompt`.
    pub(crate) fn save_edit(&mut self, cx: &mut Context<Self>) {
        let Some(field) = self.edit.take() else {
            return;
        };
        match field {
            EditField::Name => {
                let name = self.edit_buf.trim().to_string();
                if name.is_empty() {
                    self.notice = "a name cannot be empty".into();
                } else {
                    self.instance.name = name.clone();
                    self.state.upsert(self.instance.clone());
                    self.notice = match self.state.save() {
                        Ok(_) => {
                            self.log(LogKind::Info, format!("profile: name → {name}"));
                            "profile saved · applies next turn".into()
                        }
                        Err(e) => e,
                    };
                }
            }
            EditField::Instructions => {
                let value = self.edit_buf.clone();
                let path = self.instance.config_path();
                self.notice = match setup::update_config(&path, |c| {
                    c["system_prompt"] = serde_json::json!(value);
                }) {
                    Ok(_) => {
                        self.log(LogKind::Info, "profile: instructions updated");
                        "profile saved · applies next turn".into()
                    }
                    Err(e) => e,
                };
            }
        }
        self.edit_buf.clear();
        cx.notify();
    }

    fn set_color(&mut self, color: Option<u32>, cx: &mut Context<Self>) {
        self.instance.color = color;
        self.state.upsert(self.instance.clone());
        self.notice = match self.state.save() {
            Ok(_) => {
                let what = color.map_or("default".to_string(), |c| format!("#{c:06x}"));
                self.log(LogKind::Info, format!("profile: avatar color → {what}"));
                "profile saved · applies next turn".into()
            }
            Err(e) => e,
        };
        cx.notify();
    }

    // ── Roster context menu ────────────────────────────────────────────────

    fn toggle_pin(&mut self, id: String, cx: &mut Context<Self>) {
        self.roster_menu = None;
        let Some(inst) = self.state.instances.iter_mut().find(|i| i.id == id) else {
            return;
        };
        inst.pinned = !inst.pinned;
        let (name, pinned) = (inst.name.clone(), inst.pinned);
        if id == self.instance.id {
            self.instance.pinned = pinned;
        }
        self.notice = match self.state.save() {
            Ok(_) => {
                let verb = if pinned { "pinned to top" } else { "unpinned" };
                self.log(LogKind::Info, format!("roster: {name} {verb}"));
                format!("{name} {verb}")
            }
            Err(e) => e,
        };
        cx.notify();
    }

    fn duplicate_roster(&mut self, id: String, cx: &mut Context<Self>) {
        self.roster_menu = None;
        match setup::duplicate_instance(&self.state, &id) {
            Ok(copy) => {
                let name = copy.name.clone();
                self.state.instances.push(copy);
                self.notice = match self.state.save() {
                    Ok(_) => {
                        self.log(LogKind::Info, format!("roster: created {name}"));
                        format!("duplicated → {name}")
                    }
                    Err(e) => e,
                };
            }
            Err(e) => self.notice = e,
        }
        cx.notify();
    }

    /// Drop an instance from ~/.apollo/desktop.json. Files on disk are
    /// never touched; the active instance cannot leave the list.
    fn remove_roster(&mut self, id: String, cx: &mut Context<Self>) {
        self.roster_menu = None;
        if id == self.instance.id {
            return;
        }
        let Some(name) = self
            .state
            .instances
            .iter()
            .find(|i| i.id == id)
            .map(|i| i.name.clone())
        else {
            return;
        };
        self.state.instances.retain(|i| i.id != id);
        if self.state.active.as_deref() == Some(id.as_str()) {
            self.state.active = self.state.instances.first().map(|i| i.id.clone());
        }
        self.notice = match self.state.save() {
            Ok(_) => {
                self.log(
                    LogKind::Info,
                    format!("roster: removed {name} from the list (files kept)"),
                );
                format!("removed {name} from the list — files kept on disk")
            }
            Err(e) => e,
        };
        cx.notify();
    }

    // ── Model picker ───────────────────────────────────────────────────────

    /// Why this provider has no usable credential, or `None` when it has
    /// one. Only the *presence* of a key is ever read, never its value.
    fn credential_reason(&self, p: &ProviderInfo) -> Option<&'static str> {
        let env = self.instance.env_path();
        match p.auth {
            Auth::OAuth(kind) if !kind.signed_in() => Some("not signed in"),
            Auth::ApiKey(var) if !setup::env_has(&env, var) => Some("no key saved"),
            Auth::Custom if !setup::env_has(&env, setup::CUSTOM_KEY_VAR) => Some("no key saved"),
            _ => None,
        }
    }

    /// Build the picker rail: the current provider, then the sign-in
    /// providers (signed in live, the rest dimmed), then the catalog.
    fn open_picker(&mut self, cx: &mut Context<Self>) {
        let mut rail: Vec<RailEntry> = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        if let Some(p) = setup::provider(&self.instance.provider) {
            seen.push(p.id);
            // In use, so never dimmed — model switching stays possible.
            rail.push(RailEntry {
                provider: p,
                reason: None,
            });
        }
        for p in setup::OAUTH_PROVIDERS {
            if seen.contains(&p.id) {
                continue;
            }
            seen.push(p.id);
            rail.push(RailEntry {
                provider: p,
                reason: self.credential_reason(p),
            });
        }
        for p in setup::PROVIDERS.iter() {
            if p.is_custom() || seen.contains(&p.id) {
                continue;
            }
            seen.push(p.id);
            rail.push(RailEntry {
                provider: p,
                reason: self.credential_reason(p),
            });
        }
        self.picker = Some(PickerState {
            rail,
            highlighted: 0,
            models: None,
            loading: false,
            note: None,
            gen: 0,
            query: String::new(),
        });
        self.refresh_picker_models(cx);
        cx.notify();
    }

    fn highlight_provider(&mut self, i: usize, cx: &mut Context<Self>) {
        let changed = match self.picker.as_mut() {
            Some(picker) if i < picker.rail.len() && picker.highlighted != i => {
                picker.highlighted = i;
                picker.models = None;
                picker.note = None;
                true
            }
            _ => false,
        };
        if changed {
            self.refresh_picker_models(cx);
            cx.notify();
        }
    }

    /// Resolve the highlighted provider's models on a worker thread and
    /// poll the channel back on the foreground, the way onboarding's
    /// `refresh_models` does. Offline sources only — no key is at hand.
    fn refresh_picker_models(&mut self, cx: &mut Context<Self>) {
        let (catalog, fetch) = match self.picker.as_ref() {
            Some(picker) => {
                let entry = &picker.rail[picker.highlighted];
                (entry.provider.catalog.to_string(), entry.reason.is_none())
            }
            None => return,
        };
        let current = self.model.clone();
        let gen = match self.picker.as_mut() {
            Some(picker) => {
                picker.gen += 1;
                picker.loading = fetch;
                picker.gen
            }
            None => return,
        };
        if !fetch {
            return;
        }
        let (tx, rx) = channel::<(ModelList, Option<String>)>();
        std::thread::spawn(move || {
            let _ = tx.send(models::resolve(&catalog, None, &[current.as_str()]));
        });
        cx.spawn(async move |this, cx: &mut gpui::AsyncApp| loop {
            match rx.try_recv() {
                Ok((list, note)) => {
                    this.update(cx, |view, cx| {
                        if let Some(picker) = view.picker.as_mut() {
                            if picker.gen == gen {
                                picker.loading = false;
                                picker.note = note;
                                picker.models = Some(list);
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                    break;
                }
                Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
        })
        .detach();
    }

    /// Write the picked model to apollo.json. Picking on another provider
    /// also switches `provider.name` and `provider.base_url`.
    fn pick_model(&mut self, model: String, cx: &mut Context<Self>) {
        if let Some(role) = self.dial.take() {
            let path = self.instance.config_path();
            let current = setup::read_dials(&path, &self.model)
                .into_iter()
                .nth(match role {
                    "oracle" => 0,
                    "subagents" => 2,
                    _ => 1,
                })
                .map(|dial| dial.effort)
                .unwrap_or_default();
            let effort = models::snap_effort(&model, &current);
            self.notice = match setup::write_dial(&path, role, Some(&model), Some(&effort)) {
                Ok(_) => {
                    if role == "main" {
                        self.model = model.clone();
                        self.instance.model = model.clone();
                        self.state.upsert(self.instance.clone());
                        let _ = self.state.save();
                        format!("main → {model} · next message")
                    } else if role == "oracle" {
                        let dir = path.parent().map(|p| p.to_path_buf());
                        if let Some(dir) = dir {
                            std::thread::spawn(move || {
                                let _ = agent::ensure_daemon(&dir);
                            });
                        }
                        format!("oracle → {model} · fast model, after the agent restarts")
                    } else {
                        format!("subagents → {model} · stored. the engine has no separate subagent runner yet")
                    }
                }
                Err(e) => e,
            };
            self.picker = None;
            cx.notify();
            return;
        }
        let (provider, is_current) = match self.picker.as_ref() {
            Some(picker) => match picker.rail.get(picker.highlighted) {
                // Dimmed providers are not switchable.
                Some(entry) if entry.reason.is_none() => {
                    let is_current = setup::provider(&self.instance.provider)
                        .is_some_and(|cur| cur.id == entry.provider.id);
                    (entry.provider, is_current)
                }
                _ => return,
            },
            None => return,
        };
        let model_write = model.clone();
        let provider_id = provider.id;
        let base_url = provider.base_url;
        let path = self.instance.config_path();
        let result = setup::update_config(&path, move |c| {
            if !is_current {
                if !c["provider"].is_object() {
                    c["provider"] = serde_json::json!({});
                }
                c["provider"]["name"] = serde_json::json!(provider_id);
                c["provider"]["base_url"] =
                    base_url.map_or(serde_json::Value::Null, |u| serde_json::json!(u));
            }
            c["model"] = serde_json::json!(model_write.clone());
            if !c["agent"].is_object() {
                c["agent"] = serde_json::json!({});
            }
            if !c["agent"]["roles"].is_object() {
                c["agent"]["roles"] = serde_json::json!({});
            }
            if !c["agent"]["roles"]["main"].is_object() {
                c["agent"]["roles"]["main"] = serde_json::json!({});
            }
            c["agent"]["roles"]["main"]["model"] = serde_json::json!(model_write);
        });
        match result {
            Ok(_) => {
                if !is_current {
                    self.instance.provider = provider_id.to_string();
                    self.log(LogKind::Info, format!("provider → {provider_id}"));
                }
                self.instance.model = model.clone();
                self.model = model.clone();
                self.state.upsert(self.instance.clone());
                let _ = self.state.save();
                self.notice = format!("model → {model} (next turn)");
                self.log(LogKind::Info, self.notice.clone());
                self.picker = None;
            }
            Err(e) => self.notice = e,
        }
        cx.notify();
    }

    // ── Pieces ─────────────────────────────────────────────────────────────

    fn link(
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        active: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .text_xs()
            .cursor_pointer()
            .text_color(rgb(if active { ACCENT } else { MUTED }))
            .hover(|s| s.text_color(rgb(TEXT)))
            .child(label.into())
            .on_click(on_click)
    }

    fn avatar(name: &str, active: bool, size: f32, color: Option<u32>) -> AnyElement {
        let (bg, fg) = match color {
            Some(c) => (c, BG),
            None if active => (ACCENT, BG),
            None => (SURFACE_2, TEXT),
        };
        div()
            .w(px(size))
            .h(px(size))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .bg(rgb(bg))
            .text_xs()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(rgb(fg))
            .child(SharedString::from(initial(name)))
            .into_any_element()
    }

    fn chat_dock(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.role_dials(cx))
            .child(self.composer(cx))
            .into_any_element()
    }

    fn role_dials(&self, cx: &mut Context<Self>) -> AnyElement {
        let path = self.instance.config_path();
        let dials = setup::read_dials(&path, &self.model);
        let roles = ["oracle", "main", "subagents"];
        let rows = roles.into_iter().enumerate().map(|(i, role)| {
            let dial = &dials[i];
            let levels = models::effort_levels(&dial.model).unwrap_or_default();
            let model_label = if dial.model.is_empty() {
                "model".to_string()
            } else {
                dial.model.clone()
            };
            let chips = levels.into_iter().enumerate().map(|(j, level)| {
                let on = dial.effort == level;
                let level_owned = level.clone();
                div()
                    .id(("dial-effort", i * 8 + j))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(if on { MUTED } else { SURFACE_2 }))
                    .text_xs()
                    .text_color(rgb(if on { ACCENT } else { SOFT }))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(SURFACE)))
                    .child(SharedString::from(level))
                    .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                        let path = view.instance.config_path();
                        view.notice = setup::write_dial(&path, role, None, Some(&level_owned))
                            .map(|_| format!("{role} effort → {level_owned}"))
                            .unwrap_or_else(|e| e);
                        cx.notify();
                    }))
            });
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(72.))
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(role),
                )
                .child(
                    div()
                        .id(("dial-model", i))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(SURFACE_2))
                        .text_xs()
                        .text_color(rgb(TEXT))
                        .cursor_pointer()
                        .hover(|style| style.bg(rgb(SURFACE)))
                        .child(SharedString::from(model_label))
                        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                            view.dial = Some(role);
                            view.open_picker(cx);
                        })),
                )
                .children(chips)
        });
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(GHOST))
                    .child("main is this chat. oracle is the fast model. subagents are stored."),
            )
            .children(rows)
            .with_animation(
                "role-dials",
                Animation::new(Duration::from_millis(180)).with_easing(ease_out_quint()),
                |el, delta| el.opacity(0.4 + 0.6 * delta),
            )
            .into_any_element()
    }

    fn composer(&self, cx: &mut Context<Self>) -> AnyElement {
        let cursor = blink_cursor(self.cursor_start);
        let empty = self.draft.is_empty();
        let text = if empty {
            format!("message {}…{cursor}", self.instance.name)
        } else {
            format!("{}{cursor}", self.draft)
        };
        div()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_4()
            .py_3()
            .rounded_lg()
            .border_1()
            .border_color(rgb(SURFACE_2))
            .bg(rgb(SURFACE))
            .child(div().text_sm().text_color(rgb(MUTED)).child("›"))
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(rgb(if empty { GHOST } else { TEXT }))
                    .child(SharedString::from(text)),
            )
            .child(if self.busy {
                div()
                    .text_sm()
                    .text_color(rgb(WARN))
                    .child(spinner_frame(self.spinner_start))
                    .into_any_element()
            } else {
                div()
                    .id("send")
                    .text_xs()
                    .cursor_pointer()
                    .text_color(rgb(if empty { GHOST } else { ACCENT }))
                    .child("enter ↵")
                    .on_click(cx.listener(ApolloView::submit))
                    .into_any_element()
            })
            .into_any_element()
    }

    fn transcript(&self, cx: &mut Context<Self>) -> AnyElement {
        let cursor = blink_cursor(self.cursor_start);
        div()
            .flex()
            .flex_col()
            .gap_4()
            .children(
                self.entries
                    .iter()
                    .enumerate()
                    .map(|(i, entry)| match entry {
                        crate::Entry::Error(text) => self.error_row(i, text, cx),
                        other => other.view(cursor, i).into_any_element(),
                    }),
            )
            .into_any_element()
    }

    /// The sentence first. The provider body is opt-in, and it is not red.
    fn error_row(&self, index: usize, raw: &str, cx: &mut Context<Self>) -> AnyElement {
        let open = self.error_detail == Some(index);
        let missing_key = matches!(crate::classify_fault(raw), crate::Fault::MissingKey);
        let line = crate::fault_line(raw);
        let detail = crate::fault_detail(raw);
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_xs().text_color(rgb(DANGER)).child("error"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(DANGER))
                            .child(if missing_key {
                                "The API key isn't set."
                            } else {
                                line
                            }),
                    )
                    .when(missing_key, |row| {
                        row.child(
                            div()
                                .id(("error-settings", index))
                                .text_sm()
                                .text_color(rgb(ACCENT))
                                .cursor_pointer()
                                .hover(|s| s.text_color(rgb(TEXT)))
                                .child("Add it in settings.")
                                .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                                    view.show(Panel::Settings, cx)
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .id(("error-detail", index))
                    .text_xs()
                    .text_color(rgb(GHOST))
                    .cursor_pointer()
                    .hover(|s| s.text_color(rgb(TEXT)))
                    .child(if open { "hide details" } else { "details" })
                    .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                        view.error_detail = if view.error_detail == Some(index) {
                            None
                        } else {
                            Some(index)
                        };
                        cx.notify();
                    })),
            )
            .when(open, |row| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(SharedString::from(detail)),
                )
            })
            .into_any_element()
    }

    /// True once someone has spoken. A status line alone is still the empty home.
    fn has_conversation(&self) -> bool {
        self.entries.iter().any(|entry| {
            matches!(
                entry,
                crate::Entry::User(_)
                    | crate::Entry::Agent { .. }
                    | crate::Entry::Tool { .. }
                    | crate::Entry::Error(_)
            )
        })
    }

    /// First screen: a centered column, not a void with controls on the floor.
    /// Chat guidance (a message column, an empty state that says what this
    /// one does, a few starters, composer in a predictable place) puts the
    /// composer mid-column until the first message, then docks it.
    fn home_stage(&self, cx: &mut Context<Self>) -> AnyElement {
        let column = if self.has_conversation() {
            div()
                .w_full()
                .max_w(px(720.))
                .h_full()
                .flex()
                .flex_col()
                .child(self.transcript_pane(cx))
                .child(div().w_full().pt_4().child(self.chat_dock(cx)))
                .into_any_element()
        } else {
            div()
                .w_full()
                .max_w(px(560.))
                .flex()
                .flex_col()
                .gap_5()
                .child(self.empty_intro())
                .child(self.starters(cx))
                .child(self.role_dials(cx))
                .child(self.composer(cx))
                .with_animation(
                    "empty-home",
                    Animation::new(Duration::from_millis(180)).with_easing(ease_out_quint()),
                    |el, delta| el.opacity(0.45 + 0.55 * delta),
                )
                .into_any_element()
        };
        div()
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px_8()
            .when(self.has_conversation(), |d| d.justify_start())
            .child(column)
            .into_any_element()
    }

    fn empty_intro(&self) -> AnyElement {
        let scope = if self.instance.everywhere {
            "anywhere on this machine"
        } else {
            "this workspace"
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_lg()
                    .text_color(rgb(TEXT))
                    .child(SharedString::from(self.instance.name.clone())),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(SOFT))
                    .child(SharedString::from(format!(
                        "It can look at {scope}, run a check, and change files. Pick a start, or write your own."
                    ))),
            )
            .into_any_element()
    }

    fn starters(&self, cx: &mut Context<Self>) -> AnyElement {
        let prompts = [
            (
                "Look around",
                "Look at this workspace and tell me what is here.",
            ),
            ("Run a check", "Run doctor and summarize any issues."),
            (
                "What can you use",
                "List your available tools, grouped by purpose.",
            ),
        ];
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .children(prompts.into_iter().enumerate().map(|(i, (label, prompt))| {
                let prompt = prompt.to_string();
                div()
                    .id(("starter", i))
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(SURFACE_2))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(SURFACE)))
                    .child(div().text_sm().text_color(rgb(TEXT)).child(label))
                    .child(div().text_xs().text_color(rgb(GHOST)).child("↵"))
                    .on_click(cx.listener(move |view, _: &ClickEvent, window, cx| {
                        view.use_prompt(&prompt, window, cx)
                    }))
            }))
            .into_any_element()
    }

    fn transcript_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .overflow_hidden()
            .flex()
            .flex_col()
            .justify_end()
            .child(self.transcript(cx))
            .into_any_element()
    }

    fn instance_pill(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("instance-pill")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .rounded_full()
            .border_1()
            .border_color(rgb(if self.switcher_open { MUTED } else { SURFACE_2 }))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(SURFACE)))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(if self.online { SUCCESS } else { GHOST }))
                    .child("●"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(TEXT))
                    .child(SharedString::from(self.instance.name.clone())),
            )
            .child(div().text_xs().text_color(rgb(MUTED)).child("▾"))
            .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                view.switcher_open = !view.switcher_open;
                cx.notify();
            }))
            .into_any_element()
    }

    /// The dropdown under the instance pill (simple mode).
    fn switcher(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = setup::roster_order(&self.state.instances)
            .into_iter()
            .enumerate()
            .map(|(i, inst)| {
                let active = inst.id == self.instance.id;
                let id = inst.id.clone();
                div()
                    .id(("switch", i))
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .bg(rgb(if active { SURFACE_2 } else { SURFACE }))
                    .hover(|s| s.bg(rgb(SURFACE_2)))
                    .child(Self::avatar(&inst.name, active, 22., inst.color))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(if active { ACCENT } else { TEXT }))
                                    .child(SharedString::from(inst.name.clone())),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child(SharedString::from(inst.model.clone())),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(rgb(GHOST))
                                    .child(SharedString::from(inst.scope_label())),
                            ),
                    )
                    .child(div().text_xs().text_color(rgb(SUCCESS)).child(if active {
                        "✓"
                    } else {
                        ""
                    }))
                    .on_click(cx.listener(move |view, _: &ClickEvent, window, cx| {
                        view.switch_to(id.clone(), window, cx)
                    }))
            });
        div()
            .absolute()
            .top(px(44.))
            .right(px(20.))
            .w(px(300.))
            .flex()
            .flex_col()
            .gap_0p5()
            .p_1()
            .rounded_lg()
            .border_1()
            .border_color(rgb(SURFACE_2))
            .bg(rgb(SURFACE))
            .shadow_lg()
            .child(
                div()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .text_xs()
                    .text_color(rgb(GHOST))
                    .child("instances"),
            )
            .children(rows)
            .child(div().my_1().h(px(1.)).bg(rgb(SURFACE_2)))
            .child(
                div()
                    .id("switch-new")
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .text_sm()
                    .text_color(rgb(SOFT))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(SURFACE_2)).text_color(rgb(ACCENT)))
                    .child("+ new instance")
                    .on_click(cx.listener(|view, _: &ClickEvent, window, cx| {
                        view.new_instance(window, cx)
                    })),
            )
            .with_animation(
                "instance-switcher",
                Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint()),
                |el, delta| el.opacity(0.4 + 0.6 * delta),
            )
            .into_any_element()
    }

    fn section(title: &'static str) -> gpui::Div {
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_xs().text_color(rgb(GHOST)).child(title))
    }

    fn kv(k: &'static str, v: String) -> AnyElement {
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
            .into_any_element()
    }

    /// Like [`Self::kv`], for rows whose label is built at runtime —
    /// credential names and sign-ins on the keys section.
    fn kv_key(k: impl Into<SharedString>, v: &'static str) -> AnyElement {
        div()
            .flex()
            .flex_row()
            .gap_3()
            .child(
                div()
                    .w(px(220.))
                    .min_w(px(0.))
                    .truncate()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(k.into()),
            )
            .child(div().flex_1().text_xs().text_color(rgb(TEXT)).child(v))
            .into_any_element()
    }

    fn segmented(&self, cx: &mut Context<Self>) -> AnyElement {
        let seg = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .px_4()
                .py_1p5()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .bg(rgb(if on { ACCENT } else { SURFACE }))
                .text_color(rgb(if on { BG } else { MUTED }))
                .when(on, |d| d.font_weight(gpui::FontWeight::SEMIBOLD))
                .child(label)
        };
        let simple = self.state.mode == Mode::Simple;
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .p_0p5()
                    .gap_0p5()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(SURFACE_2))
                    .bg(rgb(SURFACE))
                    .child(seg("mode-simple", "simple", simple).on_click(
                        cx.listener(|view, _: &ClickEvent, _, cx| view.set_mode(Mode::Simple, cx)),
                    ))
                    .child(
                        seg("mode-advanced", "advanced", !simple).on_click(cx.listener(
                            |view, _: &ClickEvent, _, cx| view.set_mode(Mode::Advanced, cx),
                        )),
                    ),
            )
            .child(div().text_xs().text_color(rgb(MUTED)).child(if simple {
                "chat and an instance switcher"
            } else {
                "roster, tools, permissions, model params, logs"
            }))
            .into_any_element()
    }

    fn stepper(&self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let (key, label, default, step) = PARAMS[i];
        let value = self.param(key, default);
        let btn = |id: (&'static str, usize), text: &'static str| {
            div()
                .id(id)
                .w(px(24.))
                .h(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .border_1()
                .border_color(rgb(SURFACE_2))
                .text_sm()
                .text_color(rgb(TEXT))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(SURFACE_2)))
                .child(text)
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .child(
                div()
                    .w(px(240.))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(label),
            )
            .child(btn(("param-dec", i), "−").on_click(cx.listener(
                move |view, _: &ClickEvent, _, cx| {
                    view.step_param(key, default, -(step as i64), cx)
                },
            )))
            .child(
                div()
                    .w(px(48.))
                    .flex()
                    .justify_center()
                    .text_sm()
                    .text_color(rgb(ACCENT))
                    .child(SharedString::from(value.to_string())),
            )
            .child(btn(("param-inc", i), "+").on_click(cx.listener(
                move |view, _: &ClickEvent, _, cx| view.step_param(key, default, step as i64, cx),
            )))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(GHOST))
                    .child(SharedString::from(format!("agent.{key}"))),
            )
            .into_any_element()
    }

    // ── Profile panel ──────────────────────────────────────────────────────

    /// One editable profile field. Clicking starts editing; while edited
    /// it shows the buffer with a blinking caret and save/cancel links.
    fn profile_field(
        &self,
        id: &'static str,
        field: EditField,
        placeholder: &str,
        multiline: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shown = self.field_value(field);
        let editing = self.edit == Some(field);
        let empty = shown.is_empty();
        let cursor = blink_cursor(self.cursor_start);
        let text = if editing {
            format!("{}{cursor}", self.edit_buf)
        } else if empty {
            placeholder.to_string()
        } else {
            shown
        };
        let (save_id, cancel_id) = match field {
            EditField::Name => ("profile-name-save", "profile-name-cancel"),
            EditField::Instructions => ("profile-inst-save", "profile-inst-cancel"),
        };
        let input = div()
            .id(id)
            .w_full()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(if editing { MUTED } else { SURFACE_2 }))
            .bg(rgb(if editing { SURFACE } else { BG }))
            .text_sm()
            .text_color(rgb(if !editing && empty { GHOST } else { TEXT }))
            .when(multiline, |d| d.min_h(px(96.)))
            .cursor_text()
            .child(SharedString::from(text))
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                if view.edit != Some(field) {
                    view.start_edit(field, cx);
                }
            }));
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(input)
            .when(editing, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_4()
                        .child(Self::link(
                            save_id,
                            "save",
                            true,
                            cx.listener(|view, _: &ClickEvent, _, cx| view.save_edit(cx)),
                        ))
                        .child(Self::link(
                            cancel_id,
                            "cancel",
                            false,
                            cx.listener(|view, _: &ClickEvent, _, cx| view.cancel_edit(cx)),
                        ))
                        .child(div().text_xs().text_color(rgb(GHOST)).child(if multiline {
                            "enter saves · shift+enter adds a line · esc cancels"
                        } else {
                            "enter saves · esc cancels"
                        })),
                )
            })
            .into_any_element()
    }

    fn profile_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let inst = &self.instance;
        let swatches = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(Self::avatar(&inst.name, true, 34., inst.color))
            .children(AVATAR_COLORS.iter().enumerate().map(|(i, c)| {
                let on = inst.color == Some(*c);
                div()
                    .id(("swatch", i))
                    .w(px(22.))
                    .h(px(22.))
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(if on { TEXT } else { *c }))
                    .bg(rgb(*c))
                    .cursor_pointer()
                    .hover(|s| s.border_color(rgb(TEXT)))
                    .on_click(
                        cx.listener(move |view, _: &ClickEvent, _, cx| {
                            view.set_color(Some(*c), cx)
                        }),
                    )
            }))
            .child(
                div()
                    .id("swatch-none")
                    .w(px(22.))
                    .h(px(22.))
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(if inst.color.is_none() {
                        TEXT
                    } else {
                        SURFACE_2
                    }))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .cursor_pointer()
                    .hover(|s| s.border_color(rgb(MUTED)))
                    .child("·")
                    .on_click(cx.listener(|view, _: &ClickEvent, _, cx| view.set_color(None, cx))),
            );
        div()
            .w_full()
            .max_w(px(640.))
            .flex()
            .flex_col()
            .gap_6()
            .child(
                Self::section("instance")
                    .child(Self::kv("id", inst.id.clone()))
                    .child(Self::kv("works in", inst.scope_label()))
                    .child(Self::kv("provider", inst.provider.clone()))
                    .child(Self::kv("model", self.model.clone())),
            )
            .child(Self::section("name").child(self.profile_field(
                "profile-name",
                EditField::Name,
                "name this instance",
                false,
                cx,
            )))
            .child(Self::section("avatar color").child(swatches))
            .child(
                Self::section("instructions · saved as apollo.json system_prompt").child(
                    self.profile_field(
                        "profile-instructions",
                        EditField::Instructions,
                        "(none — apollo uses its built-in prompt)",
                        true,
                        cx,
                    ),
                ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(GHOST))
                    .child("changes apply from the next turn on"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(SUCCESS))
                    .child(SharedString::from(self.notice.clone())),
            )
            .into_any_element()
    }

    // ── Roster context menu ────────────────────────────────────────────────

    /// The menu that opens under a roster row on right-click.
    fn roster_menu(&self, inst: &setup::Instance, cx: &mut Context<Self>) -> AnyElement {
        let id = inst.id.clone();
        let active = inst.id == self.instance.id;
        let pin_id = id.clone();
        let edit_id = id.clone();
        let dup_id = id.clone();
        let remove_id = id.clone();
        let item = |label: SharedString| {
            div()
                .px_3()
                .py_1p5()
                .mx_0p5()
                .rounded_md()
                .text_xs()
                .text_color(rgb(TEXT))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(SURFACE_2)))
                .child(label)
        };
        let pin = item(if inst.pinned {
            "unpin".into()
        } else {
            "pin to top".into()
        })
        .id("menu-pin")
        .on_click(
            cx.listener(move |view, _: &ClickEvent, _, cx| view.toggle_pin(pin_id.clone(), cx)),
        );
        let edit = item(if active {
            "edit profile".into()
        } else {
            "edit profile · switches first".into()
        })
        .id("menu-edit")
        .on_click(cx.listener(move |view, _: &ClickEvent, window, cx| {
            view.roster_menu = None;
            if active {
                view.show(Panel::Profile, cx);
            } else {
                view.switch_to(edit_id.clone(), window, cx);
            }
        }));
        let duplicate = item("duplicate".into())
            .id("menu-duplicate")
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                view.duplicate_roster(dup_id.clone(), cx)
            }));
        // The active instance cannot leave the list — the window is chatting
        // through its config dir.
        let remove = if active {
            div()
                .px_3()
                .py_1p5()
                .mx_0p5()
                .rounded_md()
                .text_xs()
                .text_color(rgb(GHOST))
                .child("remove from list (active)")
                .into_any_element()
        } else {
            item("remove from list".into())
                .id("menu-remove")
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                    view.remove_roster(remove_id.clone(), cx)
                }))
                .into_any_element()
        };
        div()
            .mx_2()
            .mb_1()
            .flex()
            .flex_col()
            .gap_0p5()
            .p_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(SURFACE_2))
            .bg(rgb(SURFACE))
            .shadow_lg()
            .child(
                div()
                    .px_3()
                    .pt_1()
                    .text_xs()
                    .text_color(rgb(GHOST))
                    .child(SharedString::from(inst.name.clone())),
            )
            .child(pin)
            .child(edit)
            .child(duplicate)
            .child(remove)
            .into_any_element()
    }

    // ── Model picker overlay ───────────────────────────────────────────────

    fn picker_overlay(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(picker) = self.picker.as_ref() else {
            return div().into_any_element();
        };
        let rail = picker.rail.iter().enumerate().map(|(i, entry)| {
            let on = i == picker.highlighted;
            let p = entry.provider;
            let dimmed = entry.reason.is_some();
            let right = entry.reason.map(|r| r.to_string()).unwrap_or_else(|| {
                if i == 0 {
                    "current".into()
                } else {
                    String::new()
                }
            });
            div()
                .id(("picker-provider", i))
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .rounded_md()
                .cursor_pointer()
                .when(on, |d| d.bg(rgb(SURFACE_2)))
                .hover(|s| s.bg(rgb(SURFACE_2)))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(if on { ACCENT } else { GHOST }))
                        .child(if on { "●" } else { "○" }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .text_sm()
                        .text_color(rgb(if dimmed {
                            GHOST
                        } else if on {
                            ACCENT
                        } else {
                            TEXT
                        }))
                        .child(p.label),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(if dimmed { GHOST } else { MUTED }))
                        .child(SharedString::from(right)),
                )
                .on_click(
                    cx.listener(move |view, _: &ClickEvent, _, cx| view.highlight_provider(i, cx)),
                )
        });
        let current_provider = picker.rail[picker.highlighted].provider;
        let dimmed = picker.rail[picker.highlighted].reason.is_some();
        let (models, footer) = if dimmed {
            (
                vec![div()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(rgb(GHOST))
                    .child(SharedString::from(format!(
                        "{} — sign in or add a key to switch here",
                        picker.rail[picker.highlighted].reason.unwrap_or_default()
                    )))
                    .into_any_element()],
                String::new(),
            )
        } else {
            let query = picker.query.clone();
            let matched = models::filter_models(
                &picker
                    .models
                    .as_ref()
                    .map(|m| m.models.clone())
                    .unwrap_or_default(),
                &query,
            );
            let mut models: Vec<AnyElement> = matched
                .into_iter()
                .enumerate()
                .map(|(i, m)| {
                    let selected = m == self.model;
                    let pick = m.clone();
                    div()
                        .id(("picker-model", i))
                        .w_full()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .text_sm()
                        .text_color(rgb(if selected { ACCENT } else { TEXT }))
                        .bg(rgb(if selected { SURFACE } else { BG }))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(SURFACE_2)))
                        .child(SharedString::from(m))
                        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                            view.pick_model(pick.clone(), cx)
                        }))
                        .into_any_element()
                })
                .collect();
            if models.is_empty() && !query.trim().is_empty() {
                let custom = query.trim().to_string();
                models.push(
                    div()
                        .id("picker-custom")
                        .w_full()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .text_sm()
                        .text_color(rgb(ACCENT))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(SURFACE_2)))
                        .child(SharedString::from(format!("use {custom}")))
                        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                            view.pick_model(custom.clone(), cx)
                        }))
                        .into_any_element(),
                );
            }
            let footer = if picker.loading {
                "fetching models…".to_string()
            } else {
                picker
                    .models
                    .as_ref()
                    .map(|m| format!("list from {}", m.source.label()))
                    .unwrap_or_else(|| "no list for this provider".into())
            };
            (models, footer)
        };
        div()
            .absolute()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(BG))
            .opacity(0.88)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _: &MouseDownEvent, _, cx| {
                    view.picker = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .id("picker-panel")
                    .w(px(720.))
                    .max_w(px(720.))
                    .h(px(460.))
                    .max_h(px(460.))
                    .flex()
                    .flex_col()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(SURFACE_2))
                    .bg(rgb(SURFACE))
                    .shadow_lg()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _: &MouseDownEvent, _, cx| {
                            // Keep clicks inside the picker from reaching
                            // the backdrop (which closes on any click).
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_3()
                            .px_4()
                            .py_3()
                            .border_b_1()
                            .border_color(rgb(SURFACE_2))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(TEXT))
                                    .child(SharedString::from(format!(
                                        "model · {}{}",
                                        current_provider.label,
                                        if picker.query.is_empty() {
                                            String::new()
                                        } else {
                                            format!(" · {}", picker.query)
                                        }
                                    ))),
                            )
                            .child(div().flex_1())
                            .child(Self::link(
                                "picker-close",
                                "close",
                                false,
                                cx.listener(|view, _: &ClickEvent, _, cx| {
                                    view.picker = None;
                                    cx.notify();
                                }),
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(0.))
                            .flex()
                            .flex_row()
                            .child(
                                div()
                                    .id("picker-rail")
                                    .w(px(230.))
                                    .flex_shrink_0()
                                    .overflow_y_scroll()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .p_2()
                                    .border_r_1()
                                    .border_color(rgb(SURFACE_2))
                                    .children(rail),
                            )
                            .child(
                                div()
                                    .id("picker-models")
                                    .flex_1()
                                    .min_w(px(0.))
                                    .overflow_y_scroll()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .p_2()
                                    .children(models),
                            ),
                    )
                    .child(
                        div()
                            .px_4()
                            .py_2()
                            .border_t_1()
                            .border_color(rgb(SURFACE_2))
                            .text_xs()
                            .text_color(rgb(GHOST))
                            .child(SharedString::from(format!(
                                "{footer} · written to apollo.json · applies next turn"
                            ))),
                    )
                    .with_animation(
                        "model-picker",
                        Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint()),
                        |el, delta| el.opacity(0.55 + 0.45 * delta),
                    ),
            )
            .into_any_element()
    }

    fn settings_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let advanced = self.state.mode == Mode::Advanced;
        let inst = &self.instance;
        let actions = div()
            .flex()
            .flex_row()
            .gap_4()
            .child(Self::link(
                "settings-new",
                "+ new instance",
                false,
                cx.listener(|view, _: &ClickEvent, window, cx| view.new_instance(window, cx)),
            ))
            .child(Self::link(
                "settings-rerun",
                "re-run setup for this instance",
                false,
                cx.listener(|view, _: &ClickEvent, window, cx| {
                    if !view.busy {
                        crate::open_onboarding(Purpose::FirstRun, window, cx)
                    }
                }),
            ));
        let keys: Vec<AnyElement> = {
            // Names only — the values never leave the .env (see
            // `setup::configured_keys`).
            let mut rows: Vec<AnyElement> = setup::configured_keys(&inst.env_path())
                .into_iter()
                .map(|name| Self::kv_key(name, "••••  set"))
                .collect();
            for p in setup::OAUTH_PROVIDERS {
                if let Auth::OAuth(kind) = p.auth {
                    if kind.signed_in() {
                        rows.push(Self::kv_key(
                            format!("{} · signed in", p.label),
                            "browser login",
                        ));
                    }
                }
            }
            if rows.is_empty() {
                rows.push(
                    div()
                        .text_xs()
                        .text_color(rgb(GHOST))
                        .child("none configured for this instance")
                        .into_any_element(),
                );
            }
            rows
        };
        div()
            .w_full()
            .max_w(px(640.))
            .flex()
            .flex_col()
            .gap_6()
            .child(Self::section("mode").child(self.segmented(cx)))
            .child(
                Self::section("instance")
                    .child(Self::kv("name", inst.name.clone()))
                    .child(Self::kv("provider", inst.provider.clone()))
                    .child(Self::kv("model", self.model.clone()))
                    .child(Self::kv("works in", inst.scope_label()))
                    .child(Self::kv("config", setup::display_path(&inst.config_path())))
                    .child(Self::kv("permissions", inst.permission_profile.clone())),
            )
            .child(Self::section("effort").child(self.effort_row(cx)))
            .child(Self::section("keys & connections").children(keys).child(
                div().text_xs().text_color(rgb(GHOST)).child(
                    "keys are write-only here — edit with `apollo init` or re-run \
                                 onboarding",
                ),
            ))
            .when(advanced, |d| {
                d.child(
                    Self::section("model parameters")
                        .children((0..PARAMS.len()).map(|i| self.stepper(i, cx))),
                )
            })
            .child(actions)
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(SUCCESS))
                    .child(SharedString::from(self.notice.clone())),
            )
            .into_any_element()
    }

    fn tools_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let config = setup::read_config(&self.instance.config_path());
        let flag = |k: &str| match config["policy"][k].as_bool() {
            Some(true) => "on",
            Some(false) => "off",
            None => "default",
        };
        let list = |v: &serde_json::Value| {
            v.as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "all (profile default)".into())
        };
        let profiles = PROFILES.iter().enumerate().map(|(i, p)| {
            let on = p.id == self.instance.permission_profile;
            div()
                .id(("tools-profile", i))
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(rgb(if on { MUTED } else { SURFACE_2 }))
                .bg(rgb(if on { SURFACE } else { BG }))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(SURFACE)))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(if on { ACCENT } else { GHOST }))
                        .child(if on { "●" } else { "○" }),
                )
                .child(
                    div()
                        .w(px(100.))
                        .text_sm()
                        .text_color(rgb(if on { ACCENT } else { TEXT }))
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
                    cx.listener(move |view, _: &ClickEvent, _, cx| view.set_profile(p.id, cx)),
                )
        });
        div()
            .w_full()
            .max_w(px(680.))
            .flex()
            .flex_col()
            .gap_6()
            .child(Self::section("permission profile").children(profiles))
            .child(
                Self::section("policy")
                    .child(Self::kv("shell", flag("allow_shell").into()))
                    .child(Self::kv(
                        "dynamic tools",
                        flag("allow_dynamic_tools").into(),
                    ))
                    .child(Self::kv("computer use", flag("allow_computer_use").into())),
            )
            .child(
                Self::section("toolsets")
                    .child(Self::kv("enabled", list(&config["toolsets"]["enabled"])))
                    .child(Self::kv(
                        "disabled",
                        list(&config["toolsets"]["disabled"])
                            .replace("all (profile default)", "none"),
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(SUCCESS))
                    .child(SharedString::from(self.notice.clone())),
            )
            .into_any_element()
    }

    fn logs_panel(&self) -> AnyElement {
        let rows = self.logs.iter().rev().take(40).map(|l| {
            let color = match l.kind {
                LogKind::Info => SOFT,
                LogKind::Tool => MUTED,
                LogKind::Ok => SUCCESS,
                LogKind::Error => DANGER,
            };
            div()
                .flex()
                .flex_row()
                .gap_3()
                .child(
                    div()
                        .w(px(70.))
                        .text_xs()
                        .text_color(rgb(GHOST))
                        .child(SharedString::from(l.at.clone())),
                )
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(rgb(color))
                        .child(SharedString::from(l.text.clone())),
                )
        });
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(
                div()
                    .pb_2()
                    .text_xs()
                    .text_color(rgb(GHOST))
                    .child("this session · newest first"),
            )
            .children(rows)
            .into_any_element()
    }

    // ── Layouts ────────────────────────────────────────────────────────────

    fn simple_layout(&self, cx: &mut Context<Self>) -> AnyElement {
        let settings_open = self.panel == Panel::Settings;
        let profile_open = self.panel == Panel::Profile;
        let top = div()
            .w_full()
            .h(px(44.))
            .flex_shrink_0()
            .flex()
            .flex_row()
            .items_center()
            .justify_end()
            .gap_4()
            .px_5()
            .child(self.instance_pill(cx))
            .child(Self::link(
                "simple-new-chat",
                "new chat",
                false,
                cx.listener(|view, _: &ClickEvent, _, cx| view.new_chat(cx)),
            ))
            .child(Self::link(
                "simple-profile",
                "profile",
                profile_open,
                cx.listener(move |view, _: &ClickEvent, _, cx| {
                    view.show(
                        if profile_open {
                            Panel::Chat
                        } else {
                            Panel::Profile
                        },
                        cx,
                    )
                }),
            ))
            .child(Self::link(
                "simple-settings",
                if settings_open { "close" } else { "settings" },
                settings_open,
                cx.listener(move |view, _: &ClickEvent, _, cx| {
                    view.show(
                        if settings_open {
                            Panel::Chat
                        } else {
                            Panel::Settings
                        },
                        cx,
                    )
                }),
            ));
        let body = if settings_open {
            div()
                .flex_1()
                .w_full()
                .px_10()
                .pt_6()
                .overflow_hidden()
                .child(self.settings_panel(cx))
                .into_any_element()
        } else if profile_open {
            div()
                .flex_1()
                .w_full()
                .px_10()
                .pt_6()
                .overflow_hidden()
                .child(self.profile_panel(cx))
                .into_any_element()
        } else {
            self.home_stage(cx)
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(top)
            .child(body)
            .child(
                div()
                    .w_full()
                    .px_10()
                    .pt_2()
                    .pb_5()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(0x3f3f46))
                    .child(
                        div()
                            .id("simple-model")
                            .cursor_pointer()
                            .text_color(rgb(MUTED))
                            .hover(|s| s.text_color(rgb(TEXT)))
                            .child(SharedString::from(self.model.clone()))
                            .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                                view.dial = None;
                                view.open_picker(cx);
                            })),
                    )
                    .child(
                        div()
                            .text_color(rgb(0x3f3f46))
                            .child(SharedString::from(format!("· {}", self.status))),
                    )
                    .child(div().flex_1())
                    .child("↑↓ history · ctrl+n new chat · esc clear"),
            )
            .into_any_element()
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        // Pinned rows first (`setup::roster_order`); each row carries a
        // right-click menu, rendered directly under the open row.
        // Collected eagerly so `cx` is free for the nav links below.
        let roster: Vec<AnyElement> = setup::roster_order(&self.state.instances)
            .into_iter()
            .enumerate()
            .flat_map(|(i, inst)| {
                let active = inst.id == self.instance.id;
                let id = inst.id.clone();
                let menu_id = inst.id.clone();
                let menu_open = self.roster_menu.as_deref() == Some(inst.id.as_str());
                let menu = menu_open.then(|| self.roster_menu(inst, cx));
                let row = div()
                    .id(("roster", i))
                    .mx_2()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .px_2()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .when(active, |d| d.bg(rgb(SURFACE)))
                    .hover(|s| s.bg(rgb(SURFACE)))
                    .child(Self::avatar(&inst.name, active, 26., inst.color))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .text_color(rgb(if active { ACCENT } else { TEXT }))
                                    .child(SharedString::from(if inst.pinned {
                                        format!("{} ⌃", inst.name)
                                    } else {
                                        inst.name.clone()
                                    })),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child(SharedString::from(inst.model.clone())),
                            ),
                    )
                    .on_click(cx.listener(move |view, _: &ClickEvent, window, cx| {
                        view.roster_menu = None;
                        view.switch_to(id.clone(), window, cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |view, _: &MouseDownEvent, _, cx| {
                            view.roster_menu = Some(menu_id.clone());
                            cx.notify();
                        }),
                    )
                    .into_any_element();
                let mut out = vec![row];
                if let Some(menu) = menu {
                    out.push(menu);
                }
                out
            })
            .collect();
        let nav = [
            Panel::Chat,
            Panel::Profile,
            Panel::Tools,
            Panel::Logs,
            Panel::Settings,
        ]
        .into_iter()
        .enumerate()
        .map(|(i, panel)| {
            let on = self.panel == panel;
            div()
                .id(("nav", i))
                .mx_2()
                .px_3()
                .py_1p5()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .text_color(rgb(if on { ACCENT } else { MUTED }))
                .when(on, |d| d.bg(rgb(SURFACE)))
                .hover(|s| s.text_color(rgb(TEXT)))
                .child(panel.label())
                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| view.show(panel, cx)))
        });
        div()
            .w(px(220.))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_1()
            .py_4()
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(rgb(SURFACE_2))
            .child(
                div()
                    .px_4()
                    .pb_2()
                    .text_xs()
                    .text_color(rgb(GHOST))
                    .child("instances"),
            )
            .children(roster)
            .child(
                div()
                    .id("roster-new")
                    .mx_2()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(rgb(MUTED))
                    .hover(|s| s.text_color(rgb(ACCENT)))
                    .child("+ new instance")
                    .on_click(cx.listener(|view, _: &ClickEvent, window, cx| {
                        view.new_instance(window, cx)
                    })),
            )
            .child(div().flex_1())
            .children(nav)
            .child(
                div()
                    .px_4()
                    .pt_3()
                    .text_xs()
                    .text_color(rgb(0x3f3f46))
                    .child(SharedString::from(format!(
                        "apollo-ui {} · advanced",
                        env!("CARGO_PKG_VERSION")
                    ))),
            )
            .into_any_element()
    }

    fn advanced_layout(&self, cx: &mut Context<Self>) -> AnyElement {
        let chip = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(rgb(SURFACE_2))
                .text_xs()
                .text_color(rgb(SOFT))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(SURFACE)))
                .child(label)
        };
        let top = div()
            .w_full()
            .h(px(44.))
            .flex_shrink_0()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_6()
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(ACCENT))
                    .child(SharedString::from(self.instance.name.clone())),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(GHOST))
                    .child(SharedString::from(self.instance.scope_label())),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id("top-model")
                    .text_xs()
                    .cursor_pointer()
                    .text_color(rgb(MUTED))
                    .hover(|s| s.text_color(rgb(TEXT)))
                    .child(SharedString::from(self.model.clone()))
                    .on_click(cx.listener(|view, _: &ClickEvent, _, cx| view.open_picker(cx))),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(if self.online { SUCCESS } else { GHOST }))
                    .child(SharedString::from(if self.online {
                        format!("● :{}", agent::http_port())
                    } else {
                        "○ ask".to_string()
                    })),
            );
        let body: AnyElement = match self.panel {
            Panel::Chat => div()
                .flex_1()
                .w_full()
                .min_h(px(0.))
                .flex()
                .flex_col()
                .px_8()
                .child(self.transcript_pane(cx))
                .child(
                    div()
                        .w_full()
                        .pt_3()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(
                            chip("chip-doctor", "doctor")
                                .on_click(cx.listener(ApolloView::prompt_doctor)),
                        )
                        .child(
                            chip("chip-tools", "tools")
                                .on_click(cx.listener(ApolloView::prompt_tools)),
                        )
                        .child(
                            chip("chip-new-chat", "new chat")
                                .on_click(cx.listener(ApolloView::clear_chat)),
                        ),
                )
                .into_any_element(),
            Panel::Tools => div()
                .flex_1()
                .w_full()
                .px_8()
                .pt_4()
                .overflow_hidden()
                .child(self.tools_panel(cx))
                .into_any_element(),
            Panel::Profile => div()
                .flex_1()
                .w_full()
                .px_8()
                .pt_4()
                .overflow_hidden()
                .child(self.profile_panel(cx))
                .into_any_element(),
            Panel::Logs => div()
                .flex_1()
                .w_full()
                .px_8()
                .pt_4()
                .overflow_hidden()
                .child(self.logs_panel())
                .into_any_element(),
            Panel::Settings => div()
                .flex_1()
                .w_full()
                .px_8()
                .pt_4()
                .overflow_hidden()
                .child(self.settings_panel(cx))
                .into_any_element(),
        };
        let composer = (self.panel == Panel::Chat)
            .then(|| div().w_full().px_6().pt_3().child(self.chat_dock(cx)));
        let status = div()
            .w_full()
            .mt_3()
            .pb_5()
            .flex_shrink_0()
            .flex()
            .flex_row()
            .items_center()
            .gap_4()
            .px_6()
            .border_t_1()
            .border_color(rgb(SURFACE_2))
            .text_xs()
            .text_color(rgb(GHOST))
            .child(div().text_color(rgb(MUTED)).child(self.status.clone()))
            .child(div().flex_1())
            .child(SharedString::from(format!("{} turns", self.turns)))
            .child(SharedString::from(self.instance.permission_profile.clone()));
        div()
            .size_full()
            .flex()
            .flex_row()
            .child(self.sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .child(top)
                    .child(body)
                    .children(composer)
                    .child(status),
            )
            .into_any_element()
    }
}

impl Render for ApolloView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = match self.state.mode {
            Mode::Simple => self.simple_layout(cx),
            Mode::Advanced => self.advanced_layout(cx),
        };
        let switcher =
            (self.switcher_open && self.state.mode == Mode::Simple).then(|| self.switcher(cx));
        let picker = self.picker.is_some().then(|| self.picker_overlay(cx));
        div()
            .size_full()
            .relative()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .font_family(FONT)
            .child(layout)
            .children(switcher)
            .children(picker)
            .track_focus(&self.focus)
            .key_context(CHAT_CONTEXT)
            .on_key_down(cx.listener(ApolloView::on_key_down))
            .on_action(cx.listener(ApolloView::submit_action))
            .on_action(cx.listener(ApolloView::clear_action))
    }
}
