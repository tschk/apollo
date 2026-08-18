//! Telegram outbound formatting: the markdown sanitizer and the chunker.
//!
//! Both run on every message the bot sends, and both have a hard contract
//! Telegram enforces: a chunk over the length limit is rejected outright, and
//! malformed markdown makes `sendMessage` fail with `parse_mode` set. The
//! channel retries without markdown, so a sanitizer bug degrades quietly to
//! unformatted text — which is exactly the kind of failure that ships.
//!
//! These are invariant tests rather than golden strings: whatever the
//! sanitizer decides to do with a heading, it must not touch the inside of a
//! code block, and whatever the chunker decides to split on, no chunk may
//! exceed the limit and no content may be lost.

use apollo::channels::formatting::{chunk_outgoing_text, format_outgoing_text, FormatTarget};

const TELEGRAM_MAX_LEN: usize = 4096;

fn telegram(text: &str) -> String {
    format_outgoing_text(FormatTarget::Telegram, text)
}

fn chunks(text: &str, max_len: usize) -> Vec<String> {
    chunk_outgoing_text(FormatTarget::Telegram, text, max_len)
}

/// Content that survives chunking, ignoring the whitespace and fences the
/// chunker is allowed to rearrange.
fn significant(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

// ───────────────────────── sanitizer ─────────────────────────

#[test]
fn a_code_block_is_passed_through_untouched() {
    // Every construct the sanitizer rewrites outside a fence appears inside
    // this one: a heading, a table row, a separator row, and bare asterisks.
    let text = "before\n\n```rust\n# not a heading\n| a | b |\n|---|---|\nlet x = 2 * 3 * 4;\n```\n\nafter";
    let out = telegram(text);

    assert!(out.contains("# not a heading"), "{out}");
    assert!(out.contains("| a | b |"), "{out}");
    assert!(out.contains("|---|---|"), "{out}");
    assert!(out.contains("let x = 2 * 3 * 4;"), "{out}");
    assert!(!out.contains("• a | b"), "table rewriting leaked into code: {out}");
}

#[test]
fn an_unclosed_code_fence_does_not_swallow_the_rest_of_the_message() {
    let out = telegram("```\nunclosed\n# heading");
    assert!(out.contains("unclosed"), "{out}");
    assert!(out.contains("heading"), "{out}");
}

#[test]
fn headings_become_bold_and_separator_rows_disappear() {
    let out = telegram("## Menu\n\n| dish | price |\n| --- | ---: |\n| chicken | 268 |");
    assert!(out.contains("*Menu*"), "{out}");
    assert!(!out.contains("---"), "separator row must not reach Telegram: {out}");
    assert!(out.contains("• dish | price"), "{out}");
    assert!(out.contains("• chicken | 268"), "{out}");
}

#[test]
fn an_empty_heading_is_dropped_rather_than_emitted_as_stray_markup() {
    let out = telegram("#\n\ntext");
    assert!(!out.contains("**"), "{out}");
    assert!(out.contains("text"), "{out}");
}

#[test]
fn multibyte_text_is_never_split_mid_character() {
    // The sanitizer walks lines; a byte-index slip here panics the send path.
    let out = telegram("# 生記飯店\n\n訂枱按金 HK$500 — 靠窗大圓枱");
    assert!(out.contains("*生記飯店*"), "{out}");
    assert!(out.contains("訂枱按金 HK$500 — 靠窗大圓枱"), "{out}");
}

// ───────────────────────── chunking ─────────────────────────

#[test]
fn short_text_is_one_chunk() {
    assert_eq!(chunks("hello", TELEGRAM_MAX_LEN), vec!["hello".to_string()]);
}

#[test]
fn no_chunk_exceeds_the_limit_and_nothing_is_lost() {
    let para = "The deposit is refunded against the final bill. ".repeat(30);
    let text = std::iter::repeat(para.as_str())
        .take(8)
        .collect::<Vec<_>>()
        .join("\n\n");
    assert!(text.len() > TELEGRAM_MAX_LEN);

    let parts = chunks(&text, TELEGRAM_MAX_LEN);
    assert!(parts.len() > 1, "long text must be split");
    for part in &parts {
        assert!(
            part.len() <= TELEGRAM_MAX_LEN,
            "chunk of {} bytes exceeds the Telegram limit",
            part.len()
        );
    }
    assert_eq!(
        significant(&parts.concat()),
        significant(&text),
        "chunking must not drop or reorder content"
    );
}

#[test]
fn a_single_oversized_word_is_hard_split_without_panicking() {
    let text = "字".repeat(4000); // 12 000 bytes, no whitespace to break on
    let parts = chunks(&text, 100);
    assert!(parts.len() > 1);
    for part in &parts {
        assert!(part.len() <= 100, "chunk of {} bytes", part.len());
    }
    assert_eq!(parts.concat(), text, "hard split must be lossless");
}

#[test]
fn every_chunk_of_a_long_code_block_is_a_closed_fence() {
    let body = (0..400)
        .map(|i| format!("println!(\"line {i}\");"))
        .collect::<Vec<_>>()
        .join("\n");
    let text = format!("```rust\n{body}\n```");
    assert!(text.len() > TELEGRAM_MAX_LEN);

    let parts = chunks(&text, TELEGRAM_MAX_LEN);
    assert!(parts.len() > 1);
    for part in &parts {
        assert!(part.len() <= TELEGRAM_MAX_LEN, "chunk of {} bytes", part.len());
        assert!(part.starts_with("```rust\n"), "chunk lost its language: {part}");
        assert!(part.ends_with("\n```"), "chunk left a fence open: {part}");
        assert_eq!(
            part.matches("```").count(),
            2,
            "each chunk is exactly one fenced block: {part}"
        );
    }
    let rejoined = parts
        .iter()
        .map(|p| {
            p.trim_start_matches("```rust\n")
                .trim_end_matches("```")
                .trim_end()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(significant(&rejoined), significant(&body));
}

#[test]
fn prose_around_a_code_block_stays_in_reading_order() {
    let text = format!(
        "intro paragraph\n\n```sh\n{}\n```\n\nclosing paragraph",
        "echo hello\n".repeat(500)
    );
    let parts = chunks(&text, 1000);
    assert!(parts.first().unwrap().starts_with("intro"), "{:?}", parts.first());
    assert!(
        parts.last().unwrap().starts_with("closing"),
        "{:?}",
        parts.last()
    );
    for part in &parts {
        assert!(part.len() <= 1000, "chunk of {} bytes", part.len());
    }
}

#[test]
fn multibyte_paragraphs_chunk_on_character_boundaries() {
    let para = "訂枱按金會喺埋單嗰陣全數扣返，準時到就當錢使。".repeat(20);
    let text = std::iter::repeat(para.as_str())
        .take(10)
        .collect::<Vec<_>>()
        .join("\n\n");

    let parts = chunks(&text, 512);
    assert!(parts.len() > 1);
    for part in &parts {
        assert!(part.len() <= 512, "chunk of {} bytes", part.len());
        // A chunk that split a character would not be valid UTF-8 at all, so
        // reaching this line already proves it; assert the content survived.
        assert!(!part.is_empty());
    }
    assert_eq!(significant(&parts.concat()), significant(&text));
}

#[test]
fn only_telegram_gets_chunked() {
    let long = "x".repeat(10_000);
    assert_eq!(
        chunk_outgoing_text(FormatTarget::Discord, &long, TELEGRAM_MAX_LEN).len(),
        1,
        "chunking is Telegram's limit, not everyone's"
    );
}
