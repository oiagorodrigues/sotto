//! Cleans extracted text so chunking and token estimates see the same shape for every source.

/// Line endings become `\n`, form feeds (PDF page breaks) become a paragraph break, control
/// characters, soft hyphens and zero-width characters are dropped, ligature glyphs are
/// expanded, runs of spaces collapse, and more than one blank line collapses to one.
#[must_use]
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\u{0c}' => out.push_str("\n\n"),
            '\t' | '\u{a0}' | '\u{2009}' | '\u{202f}' => out.push(' '),
            '\u{ad}' | '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{feff}' => {}
            '\u{fb00}' => out.push_str("ff"),
            '\u{fb01}' => out.push_str("fi"),
            '\u{fb02}' => out.push_str("fl"),
            '\u{fb03}' => out.push_str("ffi"),
            '\u{fb04}' => out.push_str("ffl"),
            c if c != '\n' && c.is_control() => {}
            c => out.push(c),
        }
    }
    tidy_lines(&out)
}

fn tidy_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0_u32;
    for line in text.split('\n') {
        let mut cleaned = String::with_capacity(line.len());
        let mut last_space = true;
        for c in line.chars() {
            if c == ' ' {
                if !last_space {
                    cleaned.push(' ');
                }
                last_space = true;
            } else {
                cleaned.push(c);
                last_space = false;
            }
        }
        let cleaned = cleaned.trim_end();
        if cleaned.is_empty() {
            blank_run += 1;
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
            if blank_run > 0 {
                out.push('\n');
            }
        }
        blank_run = 0;
        out.push_str(cleaned);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_endings_and_blank_runs() {
        assert_eq!(normalize("a\r\nb\rc\n\n\n\n\nd"), "a\nb\nc\n\nd");
    }

    #[test]
    fn spaces_collapse_and_trim() {
        assert_eq!(normalize("  a \t  b\u{a0}\u{a0}c  \n  d"), "a b c\nd");
    }

    #[test]
    fn control_characters_and_invisibles_are_dropped() {
        assert_eq!(normalize("a\u{0}b\u{7}c\u{ad}d\u{200b}e\u{feff}"), "abcde");
    }

    #[test]
    fn form_feed_separates_paragraphs() {
        assert_eq!(normalize("page one\u{0c}page two"), "page one\n\npage two");
    }

    #[test]
    fn ligatures_expand() {
        assert_eq!(
            normalize("e\u{fb03}cient \u{fb01}le o\u{fb00}er"),
            "efficient file offer"
        );
    }

    #[test]
    fn accents_survive() {
        assert_eq!(normalize("Ação e coração"), "Ação e coração");
    }

    #[test]
    fn empty_and_blank_input() {
        assert_eq!(normalize(""), "");
        assert_eq!(normalize(" \n\n \t "), "");
    }
}
