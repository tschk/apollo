//! Global open/focus hotkey.
//!
//! Default is Cmd+Alt+A on macOS (Control+Alt+A elsewhere). The chord is
//! stored in `~/.apollo/desktop.json` as `hotkey` and can be changed in
//! settings. An empty string turns the hotkey off.
//!
//! The listener runs on a background thread and flips a flag; the UI timer
//! picks it up and activates the window. Tokens and credentials are never
//! involved.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

static PENDING: AtomicBool = AtomicBool::new(false);
static MANAGER: Mutex<Option<(GlobalHotKeyManager, HotKey)>> = Mutex::new(None);

/// True after a global press until the UI consumes it.
pub fn take_pending() -> bool {
    PENDING.swap(false, Ordering::SeqCst)
}

/// Register (or replace) the global chord. Empty string clears it.
pub fn install(spec: &str) {
    let mut slot = MANAGER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((manager, previous)) = slot.take() {
        let _ = manager.unregister(previous);
        // Drop manager after unregister.
        drop(manager);
    }
    let trimmed = spec.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("off") {
        return;
    }
    let Some(hotkey) = parse(trimmed) else {
        tracing_warn(&format!("unrecognized hotkey: {trimmed}"));
        return;
    };
    let Ok(manager) = GlobalHotKeyManager::new() else {
        tracing_warn("could not create global hotkey manager");
        return;
    };
    if manager.register(hotkey).is_err() {
        tracing_warn(&format!("could not register hotkey {trimmed}"));
        return;
    }
    *slot = Some((manager, hotkey));
    ensure_listener();
}

fn ensure_listener() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("apollo-hotkey".into())
        .spawn(|| {
            let receiver = GlobalHotKeyEvent::receiver();
            while let Ok(event) = receiver.recv() {
                if event.state == HotKeyState::Pressed {
                    PENDING.store(true, Ordering::SeqCst);
                }
            }
        })
        .ok();
}

fn tracing_warn(msg: &str) {
    eprintln!("apollo-ui: {msg}");
}

/// Parse chords like `Cmd+Alt+A`, `Ctrl+Cmd+Space`, `Command+Option+A`.
pub fn parse(spec: &str) -> Option<HotKey> {
    let mut mods = Modifiers::empty();
    let mut code: Option<Code> = None;
    for part in spec.split('+') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        let lower = p.to_ascii_lowercase();
        match lower.as_str() {
            "cmd" | "command" | "super" | "meta" | "win" => mods |= Modifiers::SUPER,
            "alt" | "option" | "opt" => mods |= Modifiers::ALT,
            "ctrl" | "control" | "controlkey" => mods |= Modifiers::CONTROL,
            "shift" => mods |= Modifiers::SHIFT,
            "space" => code = Some(Code::Space),
            "a" => code = Some(Code::KeyA),
            "b" => code = Some(Code::KeyB),
            "c" => code = Some(Code::KeyC),
            "d" => code = Some(Code::KeyD),
            "e" => code = Some(Code::KeyE),
            "f" => code = Some(Code::KeyF),
            "g" => code = Some(Code::KeyG),
            "h" => code = Some(Code::KeyH),
            "i" => code = Some(Code::KeyI),
            "j" => code = Some(Code::KeyJ),
            "k" => code = Some(Code::KeyK),
            "l" => code = Some(Code::KeyL),
            "m" => code = Some(Code::KeyM),
            "n" => code = Some(Code::KeyN),
            "o" => code = Some(Code::KeyO),
            "p" => code = Some(Code::KeyP),
            "q" => code = Some(Code::KeyQ),
            "r" => code = Some(Code::KeyR),
            "s" => code = Some(Code::KeyS),
            "t" => code = Some(Code::KeyT),
            "u" => code = Some(Code::KeyU),
            "v" => code = Some(Code::KeyV),
            "w" => code = Some(Code::KeyW),
            "x" => code = Some(Code::KeyX),
            "y" => code = Some(Code::KeyY),
            "z" => code = Some(Code::KeyZ),
            _ => return None,
        }
    }
    Some(HotKey::new(Some(mods), code?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_chord() {
        assert!(parse("Cmd+Alt+A").is_some());
        assert!(parse("Ctrl+Cmd+Space").is_some());
    }

    #[test]
    fn rejects_empty() {
        assert!(parse("").is_none());
        assert!(parse("Cmd+").is_none());
    }
}
