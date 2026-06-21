//! Memory brief — time windows + open loops (Vellum-style, KV-backed).

use std::sync::Arc;

use crate::memory::MemoryBackend;

pub const BRIEF_NS: &str = "brief";
pub const OPEN_LOOPS_KEY: &str = "open_loops";
pub const TIME_CONTEXTS_KEY: &str = "time_contexts";

pub fn parse_bullet_list(value: &str) -> Vec<String> {
    value
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            l.trim_start_matches("- ")
                .trim_start_matches("* ")
                .to_string()
        })
        .filter(|l| !l.is_empty())
        .collect()
}

pub async fn load_brief(memory: &Arc<dyn MemoryBackend>) -> (Vec<String>, Vec<String>) {
    let loops = memory
        .recall(BRIEF_NS, OPEN_LOOPS_KEY)
        .await
        .ok()
        .flatten()
        .map(|e| parse_bullet_list(&e.value))
        .unwrap_or_default();
    let contexts = memory
        .recall(BRIEF_NS, TIME_CONTEXTS_KEY)
        .await
        .ok()
        .flatten()
        .map(|e| parse_bullet_list(&e.value))
        .unwrap_or_default();
    (contexts, loops)
}

pub fn format_brief_xml(time_contexts: &[String], open_loops: &[String]) -> Option<String> {
    if time_contexts.is_empty() && open_loops.is_empty() {
        return None;
    }
    let mut out = String::from("<memory_brief>\n");
    if !time_contexts.is_empty() {
        out.push_str("  <time_contexts>\n");
        for line in time_contexts {
            out.push_str("    - ");
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("  </time_contexts>\n");
    }
    if !open_loops.is_empty() {
        out.push_str("  <open_loops>\n");
        for line in open_loops {
            out.push_str("    - ");
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("  </open_loops>\n");
    }
    out.push_str("</memory_brief>");
    Some(out)
}

#[cfg(test)]
fn demo_brief() {
    let xml = format_brief_xml(
        &["this week: ship memory".into()],
        &["follow up on graph vendor".into()],
    )
    .unwrap();
    assert!(xml.contains("open_loops"));
    assert!(xml.contains("graph vendor"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bullets() {
        let v = "- a\n* b\n\n# c\n- d";
        assert_eq!(parse_bullet_list(v), vec!["a", "b", "d"]);
    }

    #[test]
    fn brief_xml() {
        demo_brief();
    }
}
