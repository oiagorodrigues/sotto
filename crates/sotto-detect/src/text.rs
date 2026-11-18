/// Lowercases, folds accents and drops apostrophes. Clause boundaries (`. , ; : ! ?`) become
/// `|` and every other non-alphanumeric character becomes a space. The flag reports a `?`.
pub(crate) fn fold(text: &str) -> (String, bool) {
    let mut out = String::with_capacity(text.len());
    let mut has_question_mark = false;
    for ch in text.chars() {
        for c in ch.to_lowercase() {
            match c {
                '\u{300}'..='\u{36f}' | '\'' | '\u{2019}' | '\u{2018}' | '`' => {}
                'á' | 'à' | 'â' | 'ã' | 'ä' | 'å' => out.push('a'),
                'é' | 'è' | 'ê' | 'ë' => out.push('e'),
                'í' | 'ì' | 'î' | 'ï' => out.push('i'),
                'ó' | 'ò' | 'ô' | 'õ' | 'ö' => out.push('o'),
                'ú' | 'ù' | 'û' | 'ü' => out.push('u'),
                'ç' => out.push('c'),
                'ñ' => out.push('n'),
                '?' => {
                    has_question_mark = true;
                    out.push('|');
                }
                '.' | ',' | ';' | ':' | '!' | '\u{2026}' | '\n' => out.push('|'),
                c if c.is_alphanumeric() => out.push(c),
                _ => out.push(' '),
            }
        }
    }
    (out, has_question_mark)
}

/// Lowercase, accent-folded, punctuation-free text with single spaces.
pub(crate) fn normalize_key(text: &str) -> String {
    let (folded, _) = fold(text);
    let mut out = String::with_capacity(folded.len());
    for word in folded.split(|c: char| c == '|' || c.is_whitespace()) {
        if word.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}
