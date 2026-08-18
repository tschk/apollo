//! Outgoing message formatting per channel.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatTarget {
    Telegram,
    Discord,
    WhatsApp,
    Plain,
}

pub fn format_outgoing_text(target: FormatTarget, text: &str) -> String {
    match target {
        FormatTarget::Telegram => sanitize_telegram_markdown(text),
        FormatTarget::Discord | FormatTarget::Plain => text.to_string(),
        FormatTarget::WhatsApp => sanitize_whatsapp_text(text),
    }
}

pub fn chunk_outgoing_text(target: FormatTarget, text: &str, max_len: usize) -> Vec<String> {
    match target {
        FormatTarget::Telegram => chunk_telegram_message(text, max_len),
        _ => vec![text.to_string()],
    }
}

fn sanitize_telegram_markdown(text: &str) -> String {
    let mut result = String::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut in_code_block = false;
    let mut in_table = false;

    for line in lines {
        let trimmed = line.trim();

        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            if in_code_block && trimmed == "```" {
                result.push_str("```text\n");
                continue;
            }
        }

        if in_code_block {
            result.push_str(line);
            result.push('\n');
            continue;
        }

        if trimmed.starts_with('#') {
            let header_text = trimmed.trim_start_matches('#').trim();
            if !header_text.is_empty() {
                result.push_str(&format!("*{}*\n", header_text));
            }
            continue;
        }

        if trimmed.contains('|') && !trimmed.is_empty() {
            let cells: Vec<&str> = trimmed
                .split('|')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();

            if cells
                .iter()
                .all(|c| c.chars().all(|ch| ch == '-' || ch == ':' || ch == ' '))
            {
                continue;
            }

            if !cells.is_empty() {
                in_table = true;
                result.push_str(&format!("• {}\n", cells.join(" | ")));
                continue;
            }
        } else if in_table && !trimmed.is_empty() {
            in_table = false;
        }

        result.push_str(line);
        result.push('\n');
    }

    result.trim_end().to_string()
}

fn sanitize_whatsapp_text(text: &str) -> String {
    let mut result = String::new();
    let mut in_code_block = false;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }

        if in_code_block {
            result.push_str(line);
            result.push('\n');
            continue;
        }

        if trimmed.starts_with('#') {
            let header_text = trimmed.trim_start_matches('#').trim();
            if !header_text.is_empty() {
                result.push_str(header_text);
                result.push('\n');
            }
            continue;
        }

        if trimmed.contains('|') && !trimmed.is_empty() {
            let cells: Vec<&str> = trimmed
                .split('|')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();

            if cells
                .iter()
                .all(|c| c.chars().all(|ch| ch == '-' || ch == ':' || ch == ' '))
            {
                continue;
            }

            if !cells.is_empty() {
                result.push_str("- ");
                result.push_str(&strip_inline_markdown(&cells.join(" - ")));
                result.push('\n');
                continue;
            }
        }

        result.push_str(&strip_inline_markdown(line));
        result.push('\n');
    }

    result.trim_end().to_string()
}

fn strip_inline_markdown(line: &str) -> String {
    line.replace("**", "")
        .replace("__", "")
        .replace(['`', '*', '_'], "")
}

fn chunk_telegram_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    for block in split_blocks(text) {
        if block.trim().is_empty() {
            continue;
        }

        if block.starts_with("```") {
            chunks.extend(chunk_code_block(&block, max_len));
        } else {
            chunks.extend(chunk_prose_block(&block, max_len));
        }
    }

    chunks
}

fn split_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut in_code_block = false;

    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if !current.trim().is_empty() && !in_code_block {
                blocks.push(trimmed_block(&current));
                current.clear();
            }

            current.push_str(line);
            current.push('\n');
            in_code_block = !in_code_block;

            if !in_code_block {
                blocks.push(trimmed_block(&current));
                current.clear();
            }
            continue;
        }

        current.push_str(line);
        current.push('\n');
    }

    if !current.trim().is_empty() {
        blocks.push(trimmed_block(&current));
    }

    blocks
}

/// A block is sent as its own message, so the blank line that separated it
/// from the previous block is not content — it renders as an empty first line
/// in Telegram. Leading indentation on the first line is kept.
fn trimmed_block(block: &str) -> String {
    block.trim_matches('\n').trim_end().to_string()
}

fn chunk_prose_block(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for para in text.split("\n\n") {
        if para.is_empty() {
            continue;
        }

        if current.len() + para.len() + 2 > max_len {
            if !current.is_empty() {
                chunks.push(current.clone());
                current.clear();
            }

            if para.len() > max_len {
                for sentence in split_sentences(para) {
                    if current.len() + sentence.len() > max_len {
                        if !current.is_empty() {
                            chunks.push(current.clone());
                            current.clear();
                        }
                        if sentence.len() > max_len {
                            chunks.extend(hard_split_text(&sentence, max_len));
                        } else {
                            current = sentence;
                        }
                    } else {
                        current.push_str(&sentence);
                    }
                }
            } else {
                current = para.to_string();
            }
        } else {
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(para);
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

fn split_sentences(para: &str) -> Vec<String> {
    let mut parts = Vec::new();
    for (index, sentence) in para.split(". ").enumerate() {
        if sentence.is_empty() {
            continue;
        }
        if index < para.matches(". ").count() {
            parts.push(format!("{}. ", sentence));
        } else {
            parts.push(sentence.to_string());
        }
    }
    if parts.is_empty() {
        vec![para.to_string()]
    } else {
        parts
    }
}

fn hard_split_text(text: &str, max_len: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if current.len() + ch.len_utf8() > max_len && !current.is_empty() {
            chunks.push(current);
            current = String::new();
        }
        current.push(ch);
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

fn chunk_code_block(block: &str, max_len: usize) -> Vec<String> {
    if block.len() <= max_len {
        return vec![block.to_string()];
    }

    let mut lines = block.lines();
    let opening = lines.next().unwrap_or("```text");
    let language = opening.trim_start_matches("```");
    let closing = "```";
    let content_lines: Vec<&str> = lines.filter(|line| *line != closing).collect();

    let wrapper_len = opening.len() + closing.len() + 2;
    let available = max_len.saturating_sub(wrapper_len).max(1);

    let mut chunks = Vec::new();
    let mut current = String::new();

    for line in content_lines {
        let candidate_len = if current.is_empty() {
            line.len()
        } else {
            current.len() + 1 + line.len()
        };

        if candidate_len > available && !current.is_empty() {
            chunks.push(wrap_code_chunk(language, &current));
            current.clear();
        }

        if line.len() > available {
            for part in hard_split_text(line, available) {
                if !current.is_empty() {
                    chunks.push(wrap_code_chunk(language, &current));
                    current.clear();
                }
                chunks.push(wrap_code_chunk(language, &part));
            }
            continue;
        }

        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }

    if !current.is_empty() {
        chunks.push(wrap_code_chunk(language, &current));
    }

    if chunks.is_empty() {
        chunks.push(wrap_code_chunk(language, ""));
    }

    chunks
}

fn wrap_code_chunk(language: &str, body: &str) -> String {
    if language.is_empty() {
        format!("```\n{}\n```", body)
    } else {
        format!("```{}\n{}\n```", language, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telegram_tables_become_bullets() {
        let input = "| Name | Value |\n| --- | --- |\n| A | B |";
        let output = format_outgoing_text(FormatTarget::Telegram, input);
        assert!(output.contains("• Name | Value"));
        assert!(output.contains("• A | B"));
    }

    #[test]
    fn whatsapp_strips_markdown_syntax() {
        let input = "# Header\n**bold** and `code`\n| A | B |";
        let output = format_outgoing_text(FormatTarget::WhatsApp, input);
        assert!(output.contains("Header"));
        assert!(output.contains("bold and code"));
        assert!(output.contains("- A - B"));
        assert!(!output.contains("**"));
        assert!(!output.contains("```"));
    }

    #[test]
    fn telegram_chunker_rewraps_code_blocks() {
        let input =
            "```rust\nfn main() {\n    println!(\"hello\");\n    println!(\"world\");\n}\n```";
        let chunks = chunk_outgoing_text(FormatTarget::Telegram, input, 30);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.starts_with("```rust\n")));
        assert!(chunks.iter().all(|chunk| chunk.ends_with("\n```")));
    }
}
