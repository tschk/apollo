use crepuscularity_gpui::prelude::*;
use gpui::{actions, bounds, point, px, size, Application, ClickEvent, KeyDownEvent, SharedString};

actions!(apollo_ui, [SubmitMessage, ClearDraft]);

#[derive(Clone)]
struct ChatMessage {
    role: SharedString,
    text: SharedString,
}

struct ApolloView {
    draft: String,
    messages: Vec<ChatMessage>,
    status: SharedString,
    busy: bool,
}

impl ApolloView {
    fn new(_cx: &mut Context<Self>) -> Self {
        let status = if std::path::Path::new("apollo.json").exists() {
            "ready · apollo.json found"
        } else {
            "run `apollo init` / `apollo setup` to configure"
        };
        Self {
            draft: String::new(),
            messages: vec![ChatMessage {
                role: "apollo".into(),
                text: "Local-first agent runtime. Type a message and press Enter (or Send).".into(),
            }],
            status: status.into(),
            busy: false,
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let key = event.keystroke.key.as_str();
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
            _ if key.len() == 1
                && !event.keystroke.modifiers.control
                && !event.keystroke.modifiers.platform =>
            {
                self.draft.push_str(key);
                cx.notify();
            }
            "space" => {
                self.draft.push(' ');
                cx.notify();
            }
            _ => {}
        }
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

    fn prompt_status(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.use_prompt(
            "What is your current status and configured model?",
            window,
            cx,
        );
    }

    fn send(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let trimmed = self.draft.trim().to_string();
        if trimmed.is_empty() || self.busy {
            return;
        }

        self.messages.push(ChatMessage {
            role: "you".into(),
            text: trimmed.clone().into(),
        });
        self.draft.clear();
        self.busy = true;
        self.status = "thinking…".into();
        cx.notify();

        let prompt = trimmed;
        cx.spawn(async move |this, cx: &mut gpui::AsyncApp| {
            let reply = cx
                .background_executor()
                .spawn(async move { ask_apollo(&prompt) })
                .await;

            this.update(cx, |view, cx| {
                view.busy = false;
                match reply {
                    Ok(text) => {
                        view.messages.push(ChatMessage {
                            role: "apollo".into(),
                            text: text.into(),
                        });
                        view.status = "ready".into();
                    }
                    Err(err) => {
                        view.messages.push(ChatMessage {
                            role: "system".into(),
                            text: err.into(),
                        });
                        view.status = "error — see message".into();
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

fn ask_apollo(prompt: &str) -> Result<String, String> {
    if let Ok(text) = ask_via_http(prompt) {
        return Ok(text);
    }
    ask_via_cli(prompt)
}

fn ask_via_http(prompt: &str) -> Result<String, String> {
    let port = std::env::var("APOLLO_HTTP_PORT").unwrap_or_else(|_| "31338".into());
    let url = format!("http://127.0.0.1:{port}/v1/chat");
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())?;
    let body = serde_json::json!({ "message": prompt });
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}: agent HTTP not ready", resp.status()));
    }
    let value: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    value
        .get("response")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "unexpected /v1/chat response".into())
}

fn ask_via_cli(prompt: &str) -> Result<String, String> {
    let apollo = find_apollo_bin().ok_or_else(|| {
        "apollo binary not found. Install with `cargo install apollo-agent` or `./scripts/install.sh`, start chat with `apollo chat`, or set APOLLO_HTTP_PORT."
            .to_string()
    })?;
    let output = std::process::Command::new(apollo)
        .args(["ask", prompt, "--config", "apollo.json"])
        .output()
        .map_err(|e| format!("failed to run apollo: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let out = String::from_utf8_lossy(&output.stdout);
        let detail = if !err.trim().is_empty() { err } else { out };
        return Err(format!(
            "apollo ask failed. Try `apollo init` / `apollo doctor`.\n{detail}"
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn find_apollo_bin() -> Option<std::path::PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("apollo");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    which("apollo")
}

fn which(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

impl Render for ApolloView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let draft_display = if self.draft.is_empty() {
            SharedString::from("Type a message…")
        } else {
            SharedString::from(self.draft.clone())
        };
        let draft_empty = self.draft.is_empty();
        let status = self.status.clone();
        let busy = self.busy;

        let message_list = div()
            .flex()
            .flex_col()
            .gap_5()
            .children(self.messages.iter().map(|msg| {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x6d7f92))
                            .child(msg.role.clone()),
                    )
                    .child(
                        div()
                            .text_base()
                            .text_color(rgb(0xd7e0ea))
                            .child(msg.text.clone()),
                    )
            }));

        view! {r#"
            div w-full h-full bg-[#0c1117] text-[#e8eef4] flex flex-col @keydown=on_key_down

                div px-8 pt-7 pb-4 border-b border-[#1c2630] flex items-end justify-between gap-6
                    div flex flex-col gap-1
                        div text-5xl font-bold tracking-tight text-[#f4f7fa] leading-none
                            "apollo"
                        div text-sm text-[#8b9aab]
                            "local-first agent · crepuscularity + gpui"
                    div text-xs tracking-wide uppercase text-[#6d7f92]
                        "{status}"

                div flex-1 px-8 py-6
                    {message_list}

                div px-8 pb-3 flex gap-3
                    button bg-[#121a22] border border-[#243040] text-[#c5d0dc] text-sm px-3 py-2 rounded-lg @click=prompt_doctor
                        "Doctor"
                    button bg-[#121a22] border border-[#243040] text-[#c5d0dc] text-sm px-3 py-2 rounded-lg @click=prompt_status
                        "Status"

                div px-8 pb-7 pt-2 border-t border-[#1c2630] flex gap-3 items-center
                    div flex-1 bg-[#121a22] border border-[#243040] rounded-lg px-4 py-3 text-base
                        if {draft_empty}
                            div text-[#5f7084]
                                "{draft_display}"
                        else
                            div text-[#e8eef4]
                                "{draft_display}"
                    button bg-[#d9772c] hover:bg-[#e5893d] text-[#0c1117] font-semibold px-5 py-3 rounded-lg disabled={busy} @click=submit
                        if {busy}
                            "…"
                        else
                            "Send"
        "#}
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
                size(px(920.), px(720.)),
            ))),
            Some(size(px(640.), px(480.))),
        );

        if let Err(e) = cx.open_window(window_options, |_window, cx| cx.new(ApolloView::new)) {
            eprintln!("failed to open apollo ui: {e:?}");
        }
    });
}
