//! Desktop automation via [rs_peekaboo](https://crates.io/crates/rs_peekaboo).

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use super::traits::*;
use rs_peekaboo::automation::{parse_point, split_keys, Target};
use rs_peekaboo::{Direction, ImageMode, Peekaboo};

#[derive(Deserialize)]
struct PeekabooArgs {
    action: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    coords: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    keys: Option<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    app: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    direction: Option<String>,
    #[serde(default)]
    amount: Option<u32>,
}

pub struct PeekabooTool;

#[async_trait]
impl Tool for PeekabooTool {
    fn name(&self) -> &str {
        "peekaboo"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "peekaboo".to_string(),
            description: "Cross-platform desktop automation (screenshot, UI tree, click, type, hotkeys, clipboard, shell). Uses rs_peekaboo.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": [
                            "image", "see", "list_apps", "list_windows", "click", "type",
                            "press", "hotkey", "paste", "scroll", "shell",
                            "clipboard_read", "clipboard_write", "tools"
                        ],
                        "description": "Operation to run"
                    },
                    "path": { "type": "string", "description": "Output image path (image/see)" },
                    "coords": { "type": "string", "description": "x,y point (click/move)" },
                    "text": { "type": "string", "description": "Text for type/paste/clipboard_write" },
                    "keys": { "type": "string", "description": "Hotkey combo e.g. cmd,l" },
                    "key": { "type": "string", "description": "Single key for press" },
                    "query": { "type": "string", "description": "UI element query for click" },
                    "app": { "type": "string", "description": "App name filter" },
                    "command": { "type": "string", "description": "Shell command (shell action)" },
                    "direction": { "type": "string", "enum": ["up", "down", "left", "right"] },
                    "amount": { "type": "integer", "description": "Scroll amount" }
                },
                "required": ["action"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: PeekabooArgs = serde_json::from_str(arguments)?;
        let action = args.action.to_ascii_lowercase();

        let out = tokio::task::spawn_blocking(move || run_sync(&action, args))
            .await
            .map_err(|e| anyhow::anyhow!("peekaboo task join: {e}"))??;

        Ok(ToolResult::success(out))
    }
}

fn run_sync(action: &str, args: PeekabooArgs) -> anyhow::Result<String> {
    let pb = Peekaboo::new();

    let value: Value = match action {
        "tools" => {
            return Ok("peekaboo actions: image, see, list_apps, list_windows, click, type, press, hotkey, paste, scroll, shell, clipboard_read, clipboard_write".into());
        }
        "image" => {
            let path = args.path.as_deref().map(PathBuf::from);
            let cap = pb.image(ImageMode::Screen, path, false)?;
            serde_json::to_value(cap)?
        }
        "see" => {
            let path = args.path.as_deref().map(PathBuf::from);
            let snap = pb.see(args.app.as_deref(), ImageMode::Screen, path, false)?;
            serde_json::to_value(snap)?
        }
        "list_apps" => pb.list_apps()?,
        "list_windows" => pb.list_windows()?,
        "click" => {
            let target = target_from_args(&pb, &args)?;
            pb.click(target, "left", 1)?
        }
        "type" => {
            let text = args
                .text
                .ok_or_else(|| anyhow::anyhow!("type requires text"))?;
            pb.type_text(&text, false, true, None, args.app.as_deref())?
        }
        "press" => {
            let key = args
                .key
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("press requires key"))?;
            pb.press(key, 1, None)?
        }
        "hotkey" => {
            let keys = args
                .keys
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("hotkey requires keys"))?;
            let parts: Vec<&str> = split_keys(keys);
            pb.hotkey(&parts)?
        }
        "paste" => {
            let text = args
                .text
                .ok_or_else(|| anyhow::anyhow!("paste requires text"))?;
            pb.paste(&text)?
        }
        "scroll" => {
            let dir = match args.direction.as_deref().unwrap_or("down") {
                "up" => Direction::Up,
                "left" => Direction::Left,
                "right" => Direction::Right,
                _ => Direction::Down,
            };
            pb.scroll(dir, args.amount.unwrap_or(3))?
        }
        "shell" => {
            let cmd = args
                .command
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("shell requires command"))?;
            let shell_out = pb.shell(cmd, None)?;
            serde_json::to_value(shell_out)?
        }
        "clipboard_read" => Value::String(pb.clipboard_read()?),
        "clipboard_write" => {
            let text = args
                .text
                .ok_or_else(|| anyhow::anyhow!("clipboard_write requires text"))?;
            pb.clipboard_write(&text)?
        }
        other => anyhow::bail!("unknown peekaboo action: {other}"),
    };

    Ok(serde_json::to_string_pretty(&value)?)
}

fn target_from_args(pb: &Peekaboo, args: &PeekabooArgs) -> anyhow::Result<Target> {
    if let Some(q) = args.query.as_deref() {
        let node = pb.resolve_selector(q, None)?;
        return Ok(Target::Element(node));
    }
    if let Some(c) = args.coords.as_deref() {
        let p = parse_point(c)?;
        return Ok(Target::Point(p));
    }
    anyhow::bail!("click requires coords or query")
}
