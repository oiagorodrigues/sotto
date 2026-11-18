//! Token estimates without a tokenizer.

/// About four bytes per token. It overestimates English and Portuguese, which is the safe side
/// for a prompt budget, and it matches the estimate the core uses for the spend cap.
#[must_use]
pub fn estimate_tokens(text: &str) -> u32 {
    u32::try_from(text.len().div_ceil(4)).unwrap_or(u32::MAX)
}

/// Cuts `text` to about `max_tokens`, at the last paragraph break, line break or space in the
/// final fifth of the allowed length so words and sentences stay whole, else at a character
/// boundary. Returns the text unchanged when it already fits.
#[must_use]
pub fn truncate_to_tokens(text: &str, max_tokens: u32) -> &str {
    let max_bytes = usize::try_from(max_tokens)
        .unwrap_or(usize::MAX)
        .saturating_mul(4);
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let head = &text[..end];
    let floor = end - end / 5;
    for sep in ["\n\n", "\n", " "] {
        if let Some(at) = head.rfind(sep)
            && at >= floor
        {
            return head[..at].trim_end();
        }
    }
    head
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_round_up() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
        assert_eq!(estimate_tokens("ação"), 2);
    }

    #[test]
    fn short_text_is_unchanged() {
        assert_eq!(truncate_to_tokens("hello world", 10), "hello world");
    }

    #[test]
    fn truncation_prefers_a_paragraph_break() {
        let text = format!("{}\n\n{}", "a".repeat(30), "b".repeat(30));
        assert_eq!(truncate_to_tokens(&text, 9), "a".repeat(30));
    }

    #[test]
    fn truncation_falls_back_to_a_space_then_a_character_boundary() {
        assert_eq!(
            truncate_to_tokens("abcdefghij klm nopqrstuvwxyz", 4),
            "abcdefghij klm"
        );
        let no_space = "ç".repeat(20);
        let cut = truncate_to_tokens(&no_space, 3);
        assert!(cut.len() <= 12);
        assert!(cut.chars().all(|c| c == 'ç'));
    }

    #[test]
    fn zero_budget_yields_nothing() {
        assert_eq!(truncate_to_tokens("anything", 0), "");
    }
}
