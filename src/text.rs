//! Small text utilities shared across tools and providers.

pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_chars() {
        // Basic ASCII
        assert_eq!(truncate_chars("hello world", 5), "hello");

        // Exact length
        assert_eq!(truncate_chars("hello", 5), "hello");

        // Over-truncation (string is shorter than max)
        assert_eq!(truncate_chars("hi", 5), "hi");

        // Zero length
        assert_eq!(truncate_chars("hello", 0), "");

        // Empty string
        assert_eq!(truncate_chars("", 5), "");

        // Multibyte characters (Emojis)
        assert_eq!(truncate_chars("👋🌎", 1), "👋");
        assert_eq!(truncate_chars("👋🌎", 2), "👋🌎");
        assert_eq!(truncate_chars("👋🌎", 3), "👋🌎");

        // Multibyte characters (CJK)
        assert_eq!(truncate_chars("こんにちは", 2), "こん");
        assert_eq!(truncate_chars("안녕하세요", 3), "안녕하");
        assert_eq!(truncate_chars("你好世界", 1), "你");
    }
}
