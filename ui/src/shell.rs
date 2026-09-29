//! The main window around the chat: simple and advanced layouts, the
//! instance switcher, settings, tools/permissions and the session log.
//!
//! Simple mode has no chrome beyond an instance pill and a settings link in
//! the top-right corner. Advanced mode borrows OpenMausBot's roster (every
//! instance is a row with an initial, name and model; click to switch) and
//! Hermes' settings layout (one column of sections), plus tools, model
//! parameters and logs.

use crepuscularity_gpui::prelude::*;
use gpui::{AnyElement, ClickEvent, SharedString};

use crate::agent::{self, AgentEvent};
use crate::onboarding::Purpose;
use crate::setup::{self, Mode, PROFILES};
use crate::theme::*;
use crate::{blink_cursor, spinner_frame, ApolloView, CHAT_CONTEXT};

const GHOST: u32 = 0x52525b;
const SOFT: u32 = 0xa1a1aa;
/// Between BG and SURFACE, so the roster reads as a separate column.
const SIDEBAR: u32 = 0x0f0f11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Chat,
    Tools,
    Logs,
    Settings,
}

impl Panel {
    fn label(self) -> &'static str {
        match self {
            Panel::Chat => "chat",
            Panel::Tools => "tools",
            Panel::Logs => "logs",
            Panel::Settings => "settings",
        }
    }
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
        self.notice.clear();
        cx.notify();
    }

    fn switch_to(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.switcher_open = false;
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

    fn avatar(name: &str, active: bool, size: f32) -> AnyElement {
        div()
            .w(px(size))
            .h(px(size))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .bg(rgb(if active { ACCENT } else { SURFACE_2 }))
            .text_xs()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(rgb(if active { BG } else { TEXT }))
            .child(SharedString::from(initial(name)))
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

    fn transcript(&self) -> impl IntoElement {
        let cursor = blink_cursor(self.cursor_start);
        div()
            .flex()
            .flex_col()
            .gap_4()
            .children(self.entries.iter().map(|entry| entry.view(cursor)))
    }

    fn transcript_pane(&self) -> AnyElement {
        div()
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .overflow_hidden()
            .flex()
            .flex_col()
            .justify_end()
            .child(self.transcript())
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
        let rows = self.state.instances.iter().enumerate().map(|(i, inst)| {
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
                .child(Self::avatar(&inst.name, active, 22.))
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
        } else {
            div()
                .flex_1()
                .w_full()
                .min_h(px(0.))
                .flex()
                .flex_col()
                .px_10()
                .child(self.transcript_pane())
                .into_any_element()
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(top)
            .child(body)
            .when(!settings_open, |d| {
                d.child(div().w_full().px_8().pt_3().child(self.composer(cx)))
            })
            .child(
                div()
                    .w_full()
                    .px_10()
                    .py_2()
                    .flex()
                    .flex_row()
                    .text_xs()
                    .text_color(rgb(0x3f3f46))
                    .child(SharedString::from(format!(
                        "{} · {}",
                        self.model, self.status
                    )))
                    .child(div().flex_1())
                    .child("↑↓ history · esc clear"),
            )
            .into_any_element()
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let roster = self.state.instances.iter().enumerate().map(|(i, inst)| {
            let active = inst.id == self.instance.id;
            let id = inst.id.clone();
            div()
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
                .child(Self::avatar(&inst.name, active, 26.))
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
                        ),
                )
                .on_click(cx.listener(move |view, _: &ClickEvent, window, cx| {
                    view.switch_to(id.clone(), window, cx)
                }))
        });
        let nav = [Panel::Chat, Panel::Tools, Panel::Logs, Panel::Settings]
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
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(SharedString::from(self.model.clone())),
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
                .child(self.transcript_pane())
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
                            chip("chip-clear", "clear")
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
            .then(|| div().w_full().px_6().pt_3().child(self.composer(cx)));
        let status = div()
            .w_full()
            .h(px(28.))
            .mt_3()
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
            .child(SharedString::from(format!("engine {}", self.engine)))
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
        div()
            .size_full()
            .relative()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .font_family(FONT)
            .child(layout)
            .children(switcher)
            .track_focus(&self.focus)
            .key_context(CHAT_CONTEXT)
            .on_key_down(cx.listener(ApolloView::on_key_down))
            .on_action(cx.listener(ApolloView::submit_action))
            .on_action(cx.listener(ApolloView::clear_action))
    }
}
