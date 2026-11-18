use crate::text::{fold, normalize_key};

/// Threshold `meaning_changed` uses to decide a final text is no longer a question.
pub const DEFAULT_QUESTION_THRESHOLD: f32 = 0.5;

const SIMILARITY_FLOOR: f32 = 0.6;

const W_OPENER: f32 = 0.6;
const W_REQUEST_VERB: f32 = 0.5;
const W_STRONG_ANYWHERE: f32 = 0.45;
const W_INVERSION: f32 = 0.15;
const W_TAG: f32 = 0.35;
const W_CONDITIONAL: f32 = 0.3;
const W_APPROACH: f32 = 0.25;
const W_SECOND_PERSON: f32 = 0.2;
const W_QUESTION_MARK: f32 = 0.25;
const W_EMBEDDED: f32 = 0.25;

const FILLERS: &[&str] = &[
    "so", "entao", "ok", "okay", "e", "mas", "bom", "bem", "agora", "now", "then", "and", "but",
    "well", "olha", "look", "ta", "tipo", "alright", "also", "tambem", "ai", "please", "hey", "oi",
    "hi", "pois", "anyway",
];

/// Interrogative phrases with their weight. `when`/`where` are weaker because they also open
/// subordinate clauses ("when the api fails we retry").
const WH_PHRASES: &[(&str, f32)] = &[
    ("por que", 0.55),
    ("pra que", 0.5),
    ("para que", 0.5),
    ("o que", 0.55),
    ("how come", 0.55),
    ("porque", 0.35),
    ("como", 0.55),
    ("qual", 0.55),
    ("quais", 0.55),
    ("quanto", 0.55),
    ("quantos", 0.55),
    ("quanta", 0.55),
    ("quantas", 0.55),
    ("quem", 0.55),
    ("cade", 0.5),
    ("onde", 0.4),
    ("aonde", 0.4),
    ("quando", 0.4),
    ("what", 0.55),
    ("whats", 0.55),
    ("how", 0.55),
    ("hows", 0.55),
    ("why", 0.55),
    ("which", 0.55),
    ("who", 0.55),
    ("whos", 0.55),
    ("whom", 0.55),
    ("when", 0.4),
    ("where", 0.4),
    ("wheres", 0.4),
];

/// Starts that look interrogative but are discourse markers or exclamations.
const EXCLUDED_STARTS: &[&str] = &[
    "como eu disse",
    "como disse",
    "como mencionei",
    "como falei",
    "como voce sabe",
    "como voce pode ver",
    "como voce viu",
    "como combinamos",
    "como a gente combinou",
    "como dito",
    "como sempre",
    "o que eu quero dizer",
    "o que eu disse",
    "quanto a",
    "quanto ao",
    "quanto aos",
    "quanto as",
    "which means",
    "which is why",
    "what i mean",
    "what i said",
    "what a",
    "what an",
    "what matters",
];

/// Subject pronouns right after an English wh-word mark an embedded clause ("how we handle
/// retries is documented"), not an inverted question ("how do we handle retries").
const EMBEDDED_SUBJECTS: &[&str] = &["we", "i", "they", "he", "she"];

const AUX_AFTER_WH: &[&str] = &[
    "would", "do", "does", "did", "is", "are", "was", "were", "can", "could", "should", "will",
    "has", "have", "you", "voce", "voces", "vc", "vcs", "seria", "e", "era", "foi", "sao", "pode",
    "poderia", "devo", "devemos", "vai", "vamos", "faria", "fariam", "tem", "temos", "tinha",
    "esta", "estao", "da", "dava", "precisa",
];

const OPENERS: &[&str] = &[
    "voce poderia",
    "voces poderiam",
    "voce pode",
    "voces podem",
    "voce consegue",
    "voces conseguem",
    "poderia",
    "poderiam",
    "consegue",
    "conseguem",
    "da pra",
    "dava pra",
    "tem como",
    "me explica",
    "me explique",
    "me conta",
    "me fala",
    "me diz",
    "me fale",
    "fale sobre",
    "fala sobre",
    "fala mais",
    "sera que",
    "e se",
    "queria saber",
    "gostaria de saber",
    "tenho uma pergunta",
    "uma pergunta",
    "could you",
    "can you",
    "would you",
    "will you",
    "do you",
    "did you",
    "have you",
    "is there",
    "are there",
    "are you",
    "tell me",
    "show me",
    "walk me through",
    "what if",
    "let me ask",
    "help me understand",
    "quick question",
    "i have a question",
    "i was wondering",
    "i wanna know",
    "i would like to know",
];

/// Second-person phrases that signal a question even when the clause start is lost.
const STRONG_ANYWHERE: &[&str] = &[
    "could you",
    "can you",
    "would you",
    "will you",
    "do you",
    "did you",
    "have you",
    "are you",
    "how would you",
    "how do you",
    "how did you",
    "what would you",
    "what do you",
    "why did you",
    "voce poderia",
    "voce consegue",
    "voce pode",
    "voces podem",
    "voces conseguem",
    "como voce",
    "como voces",
    "por que voce",
    "porque voce",
    "quando voce",
    "onde voce",
];

const REQUEST_VERBS: &[&str] = &[
    "explica",
    "explique",
    "descreve",
    "descreva",
    "compare",
    "compara",
    "mostra",
    "mostre",
    "liste",
    "resuma",
    "detalha",
    "detalhe",
    "describe",
    "explain",
    "list",
    "summarize",
    "outline",
    "clarify",
    "elaborate",
    "define",
    "contrast",
];

const TAGS: &[&str] = &[
    "ne", "certo", "nao e", "right", "isnt it", "correct", "verdade",
];

const SECOND_PERSON: &[&str] = &["voce", "voces", "vc", "vcs", "you", "your", "seu", "sua"];

const CONDITIONALS: &[&str] = &["se", "if"];

const PROBLEM_TRIGGERS: &[&str] = &[
    "comecar",
    "comeca",
    "comecasse",
    "comecarem",
    "falhar",
    "falha",
    "falhasse",
    "cair",
    "cai",
    "quebrar",
    "quebra",
    "crescer",
    "mudar",
    "atrasar",
    "limitar",
    "travar",
    "start",
    "starts",
    "fail",
    "fails",
    "break",
    "breaks",
    "grows",
    "changes",
    "hits",
    "stops",
    "slows",
    "crashes",
    "drops",
    "limiting",
    "outage",
];

const APPROACH_PREFIXES: &[&str] = &[
    "handl", "lid", "deal", "approach", "abord", "mitig", "recover", "recuper", "migra", "scal",
    "escal", "design", "projet",
];

const CONDITIONAL_WINDOW: usize = 12;

#[derive(Debug, Clone, PartialEq)]
pub struct QuestionScore {
    pub score: f32,
    pub features: Vec<&'static str>,
}

fn starts_with_phrase(tokens: &[&str], phrase: &str) -> bool {
    let mut it = tokens.iter();
    phrase
        .split(' ')
        .all(|w| it.next().is_some_and(|t| *t == w))
}

fn phrase_len(phrase: &str) -> usize {
    phrase.split(' ').count()
}

fn skip_fillers(tokens: &[&str]) -> usize {
    tokens
        .iter()
        .take(3)
        .take_while(|t| FILLERS.contains(t))
        .count()
}

fn score_clause(tokens: &[&str]) -> (f32, Vec<&'static str>) {
    let mut features = Vec::new();
    let start = skip_fillers(tokens);
    let rest = &tokens[start.min(tokens.len())..];
    if rest.is_empty() {
        return (0.0, features);
    }
    let mut score = 0.0;
    let excluded = EXCLUDED_STARTS.iter().any(|p| starts_with_phrase(rest, p));

    let with_e = if start > 0 && tokens[start - 1] == "e" {
        &tokens[start - 1..]
    } else {
        rest
    };
    let opener = OPENERS
        .iter()
        .any(|p| starts_with_phrase(rest, p) || starts_with_phrase(with_e, p));
    let wh = WH_PHRASES
        .iter()
        .find(|(p, _)| starts_with_phrase(rest, p))
        .map(|&(p, w)| (phrase_len(p), w));

    if opener {
        score += W_OPENER;
        features.push("opener");
    } else if let (Some((len, weight)), false) = (wh, excluded) {
        score += weight;
        features.push("wh_start");
        if rest.get(len).is_some_and(|t| AUX_AFTER_WH.contains(t)) {
            score += W_INVERSION;
            features.push("inversion");
        } else if rest.get(len).is_some_and(|t| EMBEDDED_SUBJECTS.contains(t)) {
            score -= W_EMBEDDED;
            features.push("embedded_clause");
        }
    } else if !excluded {
        let verb_at = usize::from(matches!(rest.first(), Some(&"me" | &"nos")));
        if rest.get(verb_at).is_some_and(|t| REQUEST_VERBS.contains(t)) {
            score += W_REQUEST_VERB;
            features.push("request_verb");
        } else if (1..rest.len()).any(|i| {
            STRONG_ANYWHERE
                .iter()
                .any(|p| starts_with_phrase(&rest[i..], p))
        }) {
            score += W_STRONG_ANYWHERE;
            features.push("second_person_phrase");
        }
    }

    if tokens.len() >= 3 {
        if TAGS.iter().any(|t| {
            let n = phrase_len(t);
            tokens.len() >= n + 2 && starts_with_phrase(&tokens[tokens.len() - n..], t)
        }) {
            score += W_TAG;
            features.push("tag");
        }
        if rest.first().is_some_and(|t| SECOND_PERSON.contains(t)) {
            score += W_SECOND_PERSON;
            features.push("second_person_start");
        }
    }

    if let Some(i) = rest.iter().position(|t| CONDITIONALS.contains(t)) {
        let after = &rest[i + 1..];
        let window = &after[..after.len().min(CONDITIONAL_WINDOW)];
        if window.iter().any(|t| PROBLEM_TRIGGERS.contains(t)) {
            score += W_CONDITIONAL;
            features.push("conditional");
            if rest
                .iter()
                .any(|t| APPROACH_PREFIXES.iter().any(|p| t.starts_with(p)))
            {
                score += W_APPROACH;
                features.push("approach_verb");
            }
        }
    }
    (score, features)
}

/// Scores how likely `text` is a question or request, from 0 to 1. Works on unpunctuated ASR
/// output; a `?` only adds a bonus.
#[must_use]
pub fn score_question(text: &str) -> QuestionScore {
    let (folded, has_question_mark) = fold(text);
    let mut total_tokens = 0;
    let mut best: (f32, Vec<&'static str>) = (0.0, Vec::new());
    for clause in folded.split('|') {
        let tokens: Vec<&str> = clause.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        total_tokens += tokens.len();
        let (score, features) = score_clause(&tokens);
        if score > best.0 {
            best = (score, features);
        }
    }
    let (mut score, mut features) = best;
    if has_question_mark && total_tokens > 0 {
        score += W_QUESTION_MARK;
        features.push("question_mark");
    }
    match total_tokens {
        0 => score = 0.0,
        1 | 2 => score *= 0.4,
        3 => score -= 0.05,
        _ => {}
    }
    QuestionScore {
        score: score.clamp(0.0, 1.0),
        features,
    }
}

fn token_set(key: &str) -> Vec<&str> {
    let mut v: Vec<&str> = key.split(' ').filter(|t| !t.is_empty()).collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn jaccard(a: &[&str], b: &[&str]) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let (mut i, mut j, mut shared) = (0, 0, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                shared += 1;
                i += 1;
                j += 1;
            }
        }
    }
    let union = a.len() + b.len() - shared;
    shared as f32 / union as f32
}

/// True when a speculation started on `speculated` should be restarted because `final_text`
/// differs too much or no longer reads as a question.
#[must_use]
pub fn meaning_changed(speculated: &str, final_text: &str) -> bool {
    let (ka, kb) = (normalize_key(speculated), normalize_key(final_text));
    let similarity = jaccard(&token_set(&ka), &token_set(&kb));
    similarity < SIMILARITY_FLOOR || score_question(final_text).score < DEFAULT_QUESTION_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: &str) -> f32 {
        score_question(t).score
    }

    #[test]
    fn interrogatives_without_punctuation_score_high() {
        assert!(s("como voce lidaria com uma migracao de banco") >= 0.5);
        assert!(s("Você poderia explicar o garbage collector") >= 0.5);
        assert!(s("what's the difference between a mutex and a semaphore") >= 0.5);
    }

    #[test]
    fn asr_damage_is_rescued_by_conditional_framing() {
        let damaged = "so i will handle the migration if their api starts rate limiting us";
        let q = score_question(damaged);
        assert!(q.score >= 0.5, "{q:?}");
        assert!(q.features.contains(&"conditional"));
    }

    #[test]
    fn statements_with_question_words_stay_low() {
        for t in [
            "eu nao sei como isso funcionou",
            "that's how we did it",
            "what a week",
            "como eu disse antes o deploy sai na sexta",
            "",
            "   ",
        ] {
            assert!(s(t) < 0.5, "{t}");
        }
    }

    #[test]
    fn short_texts_are_penalised() {
        assert!(s("como") < 0.5);
        assert!(s("what is") < 0.5);
    }

    #[test]
    fn question_mark_is_only_a_bonus() {
        assert!(s("tudo certo?") < 0.5);
        assert!(s("como funciona?") > s("como funciona"));
    }

    #[test]
    fn meaning_changed_cases() {
        assert!(!meaning_changed(
            "como voce lidaria com a migracao",
            "Como você lidaria com a migração?"
        ));
        assert!(meaning_changed(
            "como voce lidaria com a migracao",
            "qual o prazo para o lancamento do produto"
        ));
        assert!(meaning_changed(
            "como voce lidaria com a migracao de banco",
            "como voce lidaria com a migracao de banco mas isso e outra historia que ja passou faz tempo"
        ));
        assert!(meaning_changed(
            "how would you scale it",
            "now we scale it quickly"
        ));
        assert!(!meaning_changed(
            "how would you scale this",
            "how would you scale this service"
        ));
    }
}
