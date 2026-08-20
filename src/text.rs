//! Small text utilities shared across tools and providers.

pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// Truncate to `max_chars` characters.
///
/// Returns `None` when the text already fits, otherwise the truncated text
/// and the number of characters dropped. The gate and the truncation use the
/// same unit so the reported count is always accurate.
pub fn truncate_chars_counted(text: &str, max_chars: usize) -> Option<(String, usize)> {
    let total = text.chars().count();
    if total <= max_chars {
        return None;
    }
    Some((truncate_chars(text, max_chars), total - max_chars))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_chars() {
        assert_eq!(truncate_chars("hello world", 5), "hello");
        assert_eq!(truncate_chars("hello", 5), "hello");
        assert_eq!(truncate_chars("hi", 5), "hi");
        assert_eq!(truncate_chars("hello", 0), "");
        assert_eq!(truncate_chars("", 5), "");
        assert_eq!(truncate_chars("👋🌎", 1), "👋");
        assert_eq!(truncate_chars("👋🌎", 2), "👋🌎");
        assert_eq!(truncate_chars("こんにちは", 2), "こん");
        assert_eq!(truncate_chars("안녕하세요", 3), "안녕하");
        assert_eq!(truncate_chars("你好世界", 1), "你");
    }

    #[test]
    fn test_truncate_chars_counted() {
        assert_eq!(truncate_chars_counted("hello", 5), None);
        assert_eq!(truncate_chars_counted("hello", 6), None);
        assert_eq!(
            truncate_chars_counted("hello", 4),
            Some(("hell".to_string(), 1))
        );
        assert_eq!(
            truncate_chars_counted("hello", 0),
            Some(("".to_string(), 5))
        );
        assert_eq!(truncate_chars_counted("", 0), None);
        assert_eq!(truncate_chars_counted("", 5), None);
        assert_eq!(truncate_chars_counted("你好世界", 4), None);
        assert_eq!(
            truncate_chars_counted("你好世界", 2),
            Some(("你好".to_string(), 2))
        );
        let cjk = "日".repeat(30_000);
        let (text, dropped) = truncate_chars_counted(&cjk, 20_000).unwrap();
        assert_eq!(text.chars().count(), 20_000);
        assert_eq!(dropped, 10_000);
    }
}
