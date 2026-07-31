//! Escaping for the three text formats apollo generates from paths it does not
//! control: launchd plists, systemd units, and a generated shell script.
//!
//! A workspace path is whatever directory the user pointed apollo at, and on
//! Unix a directory name may legally contain `<`, `"`, `$`, a space, or a
//! newline. Rejecting those characters outright is wrong — a macOS path with a
//! space in it is entirely ordinary — so each value is escaped for the format
//! it lands in. Only a newline, which no format can represent inside a value,
//! is refused.

/// Escape a value for use as XML character data, as in a plist `<string>`.
pub fn xml_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Quote a value as one argument in a systemd unit directive.
///
/// systemd splits a command line on whitespace unless the word is quoted, and
/// inside a double-quoted word it honours C-style backslash escapes — so both
/// `\` and `"` must be escaped. Specifier expansion (`%h`, `%i`, …) happens
/// regardless of quoting, so `%` is doubled to keep it literal.
pub fn systemd_argument(value: &str) -> anyhow::Result<String> {
    reject_newlines(value, "systemd unit")?;
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' | '"' => {
                out.push('\\');
                out.push(ch);
            }
            '%' => out.push_str("%%"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    Ok(out)
}

/// Quote a value for a POSIX shell.
///
/// Single quotes suppress every expansion, so only the closing quote itself
/// needs handling.
pub fn shell_argument(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn reject_newlines(value: &str, format: &str) -> anyhow::Result<()> {
    if value.contains(['\n', '\r']) {
        anyhow::bail!(
            "refusing to write a {}: the path contains a line break, which no unit file can \
             represent. Move apollo to a path without one.",
            format
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory named to close a `<string>` element and append a new key is
    /// legal on Unix, and would otherwise gain boot-persistent execution.
    #[test]
    fn xml_neutralises_a_hostile_path() {
        let hostile = "/tmp/ws</string><key>RunAtLoad</key><true/><string>";
        let escaped = xml_text(hostile);
        assert!(!escaped.contains('<'), "{escaped}");
        assert!(!escaped.contains('>'), "{escaped}");
        assert!(escaped.contains("&lt;/string&gt;"), "{escaped}");
    }

    #[test]
    fn xml_escapes_ampersands_first_so_output_is_not_double_decoded() {
        assert_eq!(xml_text("a & b"), "a &amp; b");
        assert_eq!(xml_text("&lt;"), "&amp;lt;");
    }

    #[test]
    fn xml_leaves_an_ordinary_path_with_a_space_alone() {
        assert_eq!(
            xml_text("/Users/me/Library/Application Support/ws"),
            "/Users/me/Library/Application Support/ws"
        );
    }

    #[test]
    fn systemd_quoting_keeps_a_hostile_path_in_one_argument() {
        let hostile = "/tmp/ws\nExecStartPost=/bin/sh -c evil";
        assert!(
            systemd_argument(hostile).is_err(),
            "newline must be refused"
        );

        let quoted = systemd_argument("/tmp/ws \" --evil").unwrap();
        assert_eq!(quoted, r#""/tmp/ws \" --evil""#);
        let backslash = systemd_argument(r"/tmp/ws\x").unwrap();
        assert_eq!(backslash, r#""/tmp/ws\\x""#);
    }

    /// Specifiers expand inside quotes too, so `%h` must not become the home
    /// directory.
    #[test]
    fn systemd_quoting_keeps_specifiers_literal() {
        assert_eq!(
            systemd_argument("%h/evil.json").unwrap(),
            r#""%%h/evil.json""#
        );
    }

    #[test]
    fn systemd_quoting_accepts_an_ordinary_path_with_a_space() {
        assert_eq!(
            systemd_argument("/home/me/my workspace").unwrap(),
            r#""/home/me/my workspace""#
        );
    }

    #[test]
    fn shell_quoting_survives_quotes_and_expansions() {
        assert_eq!(shell_argument("/tmp/my ws"), "'/tmp/my ws'");
        assert_eq!(shell_argument("/tmp/$(id)"), "'/tmp/$(id)'");
        assert_eq!(shell_argument("/tmp/it's"), r"'/tmp/it'\''s'");
    }
}
