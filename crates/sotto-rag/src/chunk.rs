//! Splits normalized text into overlapping windows that end at paragraph or sentence
//! boundaries where possible. Sizes are token estimates (`tokens::estimate_tokens`).

use crate::tokens::estimate_tokens;

/// Window and overlap sizes in estimated tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkConfig {
    pub max_tokens: u32,
    pub overlap_tokens: u32,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            max_tokens: 320,
            overlap_tokens: 48,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub ordinal: u32,
    pub text: String,
}

struct Unit<'a> {
    text: &'a str,
    /// What joined this unit to the previous one in the source.
    sep: &'a str,
}

fn bytes_for(tokens: u32) -> usize {
    usize::try_from(tokens)
        .unwrap_or(usize::MAX)
        .saturating_mul(4)
}

/// Splits one paragraph that is larger than the window: sentences first, then words, then
/// characters, so a unit never exceeds `max_bytes`.
fn split_oversized<'a>(
    para: &'a str,
    max_bytes: usize,
    out: &mut Vec<Unit<'a>>,
    first_sep: &'a str,
) {
    let mut sep = first_sep;
    for sentence in sentences(para) {
        if sentence.len() <= max_bytes {
            out.push(Unit {
                text: sentence,
                sep,
            });
            sep = " ";
            continue;
        }
        for word in sentence.split(' ').filter(|w| !w.is_empty()) {
            let mut rest = word;
            while rest.len() > max_bytes {
                let mut cut = max_bytes;
                while !rest.is_char_boundary(cut) {
                    cut -= 1;
                }
                out.push(Unit {
                    text: &rest[..cut],
                    sep,
                });
                sep = "";
                rest = &rest[cut..];
            }
            out.push(Unit { text: rest, sep });
            sep = " ";
        }
    }
}

/// Sentence ends are `.`, `!`, `?` or `…` followed by whitespace, and single line breaks.
fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut prev_end = false;
    for (i, c) in text.char_indices() {
        if c == '\n' || (c.is_whitespace() && prev_end) {
            let piece = text[start..i].trim();
            if !piece.is_empty() {
                out.push(piece);
            }
            start = i + c.len_utf8();
            prev_end = false;
        } else {
            prev_end = matches!(c, '.' | '!' | '?' | '…');
        }
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

fn units(text: &str, max_bytes: usize) -> Vec<Unit<'_>> {
    let mut out = Vec::new();
    let mut sep = "";
    for para in text.split("\n\n") {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        if para.len() <= max_bytes {
            out.push(Unit { text: para, sep });
        } else {
            split_oversized(para, max_bytes, &mut out, sep);
        }
        sep = "\n\n";
    }
    out
}

/// Packs units into windows of at most `max_tokens`. Each window after the first starts with the
/// last units of the previous one, up to `overlap_tokens`, so a sentence cut by a boundary is
/// whole in one of the two.
#[must_use]
pub fn chunk(text: &str, config: &ChunkConfig) -> Vec<Chunk> {
    let max_tokens = config.max_tokens.max(1);
    let max_bytes = bytes_for(max_tokens);
    let overlap_bytes = bytes_for(config.overlap_tokens.min(max_tokens.saturating_sub(1)));
    let units = units(text, max_bytes);
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < units.len() {
        let mut end = start;
        let mut size = 0;
        while end < units.len() {
            let add = units[end].text.len()
                + if end == start {
                    0
                } else {
                    units[end].sep.len()
                };
            if end > start && size + add > max_bytes {
                break;
            }
            size += add;
            end += 1;
        }
        let mut text = String::with_capacity(size);
        for (i, u) in units[start..end].iter().enumerate() {
            if i > 0 {
                text.push_str(u.sep);
            }
            text.push_str(u.text);
        }
        chunks.push(Chunk {
            ordinal: u32::try_from(chunks.len()).unwrap_or(u32::MAX),
            text,
        });
        if end == units.len() {
            break;
        }
        let mut next = end;
        let mut overlap = 0;
        while next > start + 1 {
            let u = &units[next - 1];
            let cost = u.text.len() + u.sep.len();
            if overlap + cost > overlap_bytes {
                break;
            }
            overlap += cost;
            next -= 1;
        }
        start = next;
    }
    debug_assert!(
        chunks
            .iter()
            .all(|c| estimate_tokens(&c.text) <= max_tokens)
    );
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(max: u32, overlap: u32) -> ChunkConfig {
        ChunkConfig {
            max_tokens: max,
            overlap_tokens: overlap,
        }
    }

    fn paragraphs(n: usize) -> String {
        (0..n)
            .map(|i| format!("Paragraph {i} says something about topic {i}."))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    #[test]
    fn empty_text_has_no_chunks() {
        assert!(chunk("", &ChunkConfig::default()).is_empty());
    }

    #[test]
    fn small_text_is_one_chunk() {
        let c = chunk("Hello there.", &ChunkConfig::default());
        assert_eq!(
            c,
            vec![Chunk {
                ordinal: 0,
                text: "Hello there.".into()
            }]
        );
    }

    #[test]
    fn chunks_respect_the_window_and_number_in_order() {
        let text = paragraphs(60);
        let chunks = chunk(&text, &cfg(40, 10));
        assert!(chunks.len() > 5);
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.ordinal as usize, i);
            assert!(
                estimate_tokens(&c.text) <= 40,
                "chunk {i}: {}",
                c.text.len()
            );
        }
    }

    #[test]
    fn every_paragraph_appears_whole_in_some_chunk() {
        let text = paragraphs(60);
        let chunks = chunk(&text, &cfg(40, 10));
        for i in 0..60 {
            let needle = format!("Paragraph {i} says something about topic {i}.");
            assert!(chunks.iter().any(|c| c.text.contains(&needle)), "{needle}");
        }
    }

    #[test]
    fn consecutive_chunks_overlap_by_whole_units() {
        let text = paragraphs(60);
        let chunks = chunk(&text, &cfg(40, 12));
        for pair in chunks.windows(2) {
            let last = pair[0].text.rsplit("\n\n").next().unwrap_or_default();
            assert!(
                pair[1].text.starts_with(last),
                "{last:?} not at the start of next"
            );
        }
    }

    #[test]
    fn no_overlap_when_the_overlap_is_zero() {
        let text = paragraphs(30);
        let chunks = chunk(&text, &cfg(40, 0));
        let joined = chunks
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        assert_eq!(joined, text);
    }

    #[test]
    fn a_long_paragraph_splits_at_sentences() {
        let text = (0..40)
            .map(|i| format!("Sentence number {i} is here."))
            .collect::<Vec<_>>()
            .join(" ");
        let chunks = chunk(&text, &cfg(30, 0));
        assert!(chunks.len() > 3);
        for c in &chunks {
            assert!(c.text.ends_with('.'), "{}", c.text);
            assert!(c.text.starts_with("Sentence"), "{}", c.text);
        }
    }

    #[test]
    fn a_word_longer_than_the_window_is_cut_on_a_character_boundary() {
        let text = "ç".repeat(500);
        let chunks = chunk(&text, &cfg(10, 0));
        assert!(chunks.len() > 10);
        assert!(chunks.iter().all(|c| estimate_tokens(&c.text) <= 10));
        let total: usize = chunks.iter().map(|c| c.text.chars().count()).sum();
        assert_eq!(total, 500);
    }

    #[test]
    fn overlap_larger_than_the_window_still_terminates() {
        let text = paragraphs(20);
        let chunks = chunk(&text, &cfg(20, 500));
        assert!(!chunks.is_empty());
        assert!(chunks.len() < 200);
    }

    #[test]
    fn a_window_smaller_than_one_character_still_terminates() {
        let chunks = chunk("ççç", &cfg(0, 0));
        assert!(!chunks.is_empty());
    }

    #[test]
    fn chunking_is_deterministic() {
        let text = paragraphs(25);
        assert_eq!(chunk(&text, &cfg(40, 10)), chunk(&text, &cfg(40, 10)));
    }

    #[test]
    fn default_window_on_realistic_text() {
        let text = paragraphs(400);
        let chunks = chunk(&text, &ChunkConfig::default());
        assert!(chunks.iter().all(|c| estimate_tokens(&c.text) <= 320));
        assert!(chunks.len() > 10);
    }
}
