use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

fn patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            r"(?i)\b(?:sk|sk-ant|ghp_|gho_|github_pat_|xoxb-|xapp-)[A-Za-z0-9_\-]{16,}\b",
            r"(?i)(?:authorization|x-api-key|api[_-]?key|access[_-]?token|refresh[_-]?token|password|secret)\s*[:=]\s*(?:bearer\s+)?[^\s,;]+",
            r"(?i)https?://[^\s/@:]+:[^\s/@]+@[^\s]+",
            r"(?im)\b[A-Z][A-Z0-9_]*(?:KEY|TOKEN|SECRET|PASSWORD|CREDENTIALS?)[A-Z0-9_]*\s*=\s*[^\s]+",
            r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----[\s\S]*?-----END (?:RSA |EC |OPENSSH )?PRIVATE KEY-----",
        ]
        .into_iter()
        .map(|pattern| Regex::new(pattern).expect("valid redaction regex"))
        .collect()
    })
}

pub fn redact_text(input: &str) -> String {
    let mut output = input.to_string();
    for pattern in patterns() {
        output = pattern.replace_all(&output, "[REDACTED]").into_owned();
    }
    for name in [
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "GEMINI_API_KEY",
        "DISCORD_TOKEN",
        "TELEGRAM_BOT_TOKEN",
        "SLACK_BOT_TOKEN",
        "SLACK_APP_TOKEN",
        "GITHUB_TOKEN",
    ] {
        if let Ok(secret) = std::env::var(name) {
            if secret.len() >= 8 {
                output = output.replace(&secret, "[REDACTED]");
            }
        }
    }
    output
}

pub fn redact_tool_payload(tool: &str, input: &str) -> String {
    if tool != "praefectus" {
        return redact_text(input);
    }
    let Ok(mut value) = serde_json::from_str::<Value>(input) else {
        return "[REDACTED]".to_string();
    };
    redact_praefectus_value(&mut value);
    serde_json::to_string(&value).unwrap_or_else(|_| "[REDACTED]".to_string())
}

fn redact_praefectus_value(value: &mut Value) {
    match value {
        Value::Array(values) => values.iter_mut().for_each(redact_praefectus_value),
        Value::Object(values) => {
            for key in [
                "after",
                "authority",
                "before",
                "evidence",
                "fallback_chain",
                "name",
                "signature",
                "target",
                "text",
                "value",
                "warnings",
            ] {
                if values.contains_key(key) {
                    values.insert(key.to_string(), Value::String("[REDACTED]".to_string()));
                }
            }
            values.values_mut().for_each(redact_praefectus_value);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_common_tokens() {
        let output = redact_text(
            "token sk-ant-abcdefghijklmnopqrstuvwxyz and ghp_abcdefghijklmnopqrstuvwxyz1234567890",
        );
        assert!(!output.contains("abcdefghijklmnopqrstuvwxyz"));
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn redacts_environment_assignments() {
        let output = redact_text("AWS_SECRET_ACCESS_KEY=top-secret\nCUSTOM_TOKEN=value");
        assert!(!output.contains("top-secret"));
        assert!(!output.contains("CUSTOM_TOKEN=value"));
    }

    #[test]
    fn redacts_headers_and_private_keys() {
        let output = redact_text("Authorization: Bearer secret-token\n-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----");
        assert!(!output.contains("secret-token"));
        assert!(!output.contains("abc"));
    }

    #[test]
    fn redacts_praefectus_targets_and_values_from_persistent_payloads() {
        let output = redact_tool_payload(
            "praefectus",
            r#"{"desktop_action":{"kind":"set_value","value":"private"},"target":{"element_id":"selector"}}"#,
        );
        assert!(!output.contains("private"));
        assert!(!output.contains("selector"));
        assert!(output.contains("[REDACTED]"));
    }
}
