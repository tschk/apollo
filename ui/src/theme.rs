//! Telekinesis portal tokens (tschk/telekinesis apps/desktop
//! `src/styles/tokens.css`, dark theme) and the Chivo Mono face.
//!
//! The `.crepus` views use the same values as literals (`bg-[#09090b]`);
//! these constants are for rows built in Rust.

use std::borrow::Cow;

pub const BG: u32 = 0x09090b; // --tk-bg (zinc-950)
pub const SURFACE: u32 = 0x18181b; // --tk-surface
pub const SURFACE_2: u32 = 0x27272a; // --tk-surface-2
#[allow(dead_code)] // part of the token set; the templates use it literally
pub const BORDER: u32 = 0x3f3f46; // --tk-border
pub const TEXT: u32 = 0xd4d4d8; // --tk-text
pub const MUTED: u32 = 0x71717a; // --tk-muted
pub const ACCENT: u32 = 0xfafafa; // --tk-accent
pub const SUCCESS: u32 = 0x34d399; // --tk-success
pub const WARN: u32 = 0xfbbf24; // --tk-warn
pub const DANGER: u32 = 0xf87171; // --tk-danger

pub const FONT: &str = "Chivo Mono";

/// Register the bundled Chivo Mono faces (SIL OFL 1.1, see
/// `assets/fonts/OFL.txt`) so the UI does not depend on it being installed.
pub fn load_fonts(cx: &mut gpui::App) {
    let faces: Vec<Cow<'static, [u8]>> = vec![
        Cow::Borrowed(include_bytes!("../assets/fonts/ChivoMono-Regular.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/ChivoMono-SemiBold.ttf")),
    ];
    if let Err(e) = cx.text_system().add_fonts(faces) {
        eprintln!("apollo-ui: could not load Chivo Mono ({e}); using the system monospace face");
    }
}
