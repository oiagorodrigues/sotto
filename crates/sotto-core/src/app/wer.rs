//! Word error rate for the transcription check: word-level Levenshtein distance between a
//! normalized reference and a normalized hypothesis.

/// Lowercases, turns every character that is not a letter or digit into a space and drops
/// apostrophes inside words ("don't" becomes "dont"). Accents are kept, so "voce" and "você"
/// differ: the engine gets no credit for losing them. Input is expected in NFC, which is what
/// the speech engine writes.
pub(crate) fn normalize(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            word.extend(c.to_lowercase());
        } else if matches!(c, '\'' | '\u{2019}') && !word.is_empty() {
            continue;
        } else if !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Substitutions, insertions and deletions divided by the reference length, in percent.
/// `None` when the reference has no words, because the rate is undefined.
pub(crate) fn word_error_rate_pct(reference: &str, hypothesis: &str) -> Option<f32> {
    let r = normalize(reference);
    let h = normalize(hypothesis);
    if r.is_empty() {
        return None;
    }
    let distance = edit_distance(&r, &h);
    Some(distance as f32 * 100.0 / r.len() as f32)
}

fn edit_distance(a: &[String], b: &[String]) -> usize {
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, y) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(x != y);
            current.push(substitution.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wer(r: &str, h: &str) -> f32 {
        word_error_rate_pct(r, h).unwrap()
    }

    #[test]
    fn identical_text_is_zero_after_normalization() {
        assert!(
            (wer("So how would you handle it?", "so, how would you handle it") - 0.0).abs() < 1e-6
        );
        assert!((wer("Don't stop", "dont STOP") - 0.0).abs() < 1e-6);
        assert!((wer("Don\u{2019}t stop", "don't stop") - 0.0).abs() < 1e-6);
    }

    #[test]
    fn counts_substitutions_insertions_and_deletions() {
        assert!((wer("a b c d", "a x c d") - 25.0).abs() < 1e-4);
        assert!((wer("a b c d", "a b c d e") - 25.0).abs() < 1e-4);
        assert!((wer("a b c d", "a b d") - 25.0).abs() < 1e-4);
        assert!((wer("a b c d", "") - 100.0).abs() < 1e-4);
    }

    #[test]
    fn can_exceed_one_hundred_percent() {
        assert!((wer("a", "x y z") - 300.0).abs() < 1e-4);
    }

    #[test]
    fn accents_are_kept_and_case_is_not() {
        assert!((wer("Você faria a migração", "você faria a migração") - 0.0).abs() < 1e-6);
        assert!((wer("Você faria", "voce faria") - 50.0).abs() < 1e-4);
        assert_eq!(normalize("ÁGUA, Ação!"), ["água", "ação"]);
    }

    #[test]
    fn hyphens_and_digits_split_or_stay_as_words() {
        assert_eq!(
            normalize("rate-limiting 500 requests"),
            ["rate", "limiting", "500", "requests"]
        );
    }

    #[test]
    fn empty_cases() {
        assert_eq!(word_error_rate_pct("", "anything"), None);
        assert_eq!(word_error_rate_pct("  ... ", ""), None);
        assert!((wer("hello", "") - 100.0).abs() < 1e-4);
    }
}
