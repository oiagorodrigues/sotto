//! Every prompt the core sends. Ordering is cache-friendly: the stable system prompt first, then
//! the mode context and the transcript so far (the provider puts its cache breakpoint after
//! them), then the question or the typed request, which is the only part that changes between a
//! speculation and its restart.

use sotto_protocol::Speaker;
use sotto_protocol::ui::TranscriptLine;

use crate::providers::{CompletionRequest, Effort, Thinking};

pub const ANSWER_SYSTEM_PROMPT: &str = "\
You are helping the user during a live meeting. You see a live transcript of the call: \
\"Them:\" lines are the other participants and \"Me:\" lines are the user. The transcript comes \
from speech recognition, so expect missing punctuation and misheard words.

When the other person asks a question or makes a request, write what the user should say back. \
Write it in the first person, as the user would say it aloud: two to four short sentences, in \
the same language as the question. Start with the answer itself. No preamble, no markdown \
headings, no lists unless the question asks for steps. If the question is cut off, answer the \
most likely complete question.

When the user types a request to you directly, do what it asks, briefly, in the language the \
user wrote in.";

pub const CLASSIFIER_SYSTEM_PROMPT: &str = "\
You label lines from a live meeting transcript. The text comes from speech recognition and has \
no reliable punctuation. Reply with exactly one word: yes or no.";

/// Automatic answers keep adaptive thinking at low effort, and thinking tokens count against
/// `max_tokens`, so the budget must leave room for the two to four sentences after it.
pub const AUTO_ANSWER_MAX_TOKENS: u32 = 1_024;
pub const MANUAL_MAX_TOKENS: u32 = 4_096;
/// The classifier runs with thinking off; this is headroom for "yes" or "no", not a target.
pub const CLASSIFIER_MAX_TOKENS: u32 = 256;

/// Lines of earlier transcript the classifier sees besides the question.
const CLASSIFIER_CONTEXT_LINES: usize = 4;

fn label(speaker: Speaker) -> &'static str {
    match speaker {
        Speaker::Me => "Me",
        Speaker::Them => "Them",
    }
}

fn push_lines(out: &mut String, lines: &[TranscriptLine]) {
    for line in lines {
        out.push_str(label(line.speaker));
        out.push_str(": ");
        out.push_str(line.text.trim());
        out.push('\n');
    }
}

/// The cacheable and volatile parts of the context, in prompt order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Context {
    /// `Meeting context:` and the mode text.
    pub mode: String,
    /// The transcript header and the lines that are final and come before any non-final one.
    pub history: String,
    /// The remaining lines, which can still change.
    pub recent: String,
}

/// The mode text as the section that precedes the transcript, or nothing when it is blank.
#[must_use]
pub fn mode_block(mode_context: &str) -> String {
    let mode_context = mode_context.trim();
    if mode_context.is_empty() {
        return String::new();
    }
    format!("Meeting context:\n{mode_context}\n\n")
}

/// Splits the transcript where the first non-final line starts: everything before it only
/// grows at the end, so consecutive requests (a speculation and its restart) share it byte for
/// byte and the provider can cache it.
#[must_use]
pub fn split_context(mode_context: &str, transcript: &[TranscriptLine]) -> Context {
    let mut out = Context {
        mode: mode_block(mode_context),
        ..Context::default()
    };
    if !transcript.is_empty() {
        let stable = transcript
            .iter()
            .position(|l| !l.is_final)
            .unwrap_or(transcript.len());
        out.history.push_str("Transcript so far:\n");
        push_lines(&mut out.history, &transcript[..stable]);
        push_lines(&mut out.recent, &transcript[stable..]);
    }
    out
}

/// Mode context, then the transcript as `Them:` / `Me:` lines, as one text.
#[must_use]
pub fn context(mode_context: &str, transcript: &[TranscriptLine]) -> String {
    let c = split_context(mode_context, transcript);
    [c.mode, c.history, c.recent].concat()
}

fn request(model: &str, system_prefix: &str, context: Context) -> CompletionRequest {
    CompletionRequest {
        model: model.to_string(),
        system_prefix: system_prefix.to_string(),
        mode_context: context.mode,
        history: context.history,
        recent: context.recent,
        turns: Vec::new(),
        user_text: String::new(),
        image_png: None,
        max_tokens: 0,
        effort: None,
        thinking: Thinking::Adaptive,
    }
}

/// Speculative or final automatic answer. `transcript` excludes the question turn itself.
#[must_use]
pub fn auto_answer(
    model: &str,
    mode_context: &str,
    transcript: &[TranscriptLine],
    question: &str,
) -> CompletionRequest {
    CompletionRequest {
        user_text: format!(
            "The other person is asking (the transcript may be cut off):\nThem: {}\n\n\
             Write what I say back.",
            question.trim()
        ),
        max_tokens: AUTO_ANSWER_MAX_TOKENS,
        effort: Some(Effort::Low),
        ..request(
            model,
            ANSWER_SYSTEM_PROMPT,
            split_context(mode_context, transcript),
        )
    }
}

/// Typed ask. The screenshot bytes are attached later by the stream task, which decodes them
/// off the event loop; `with_screenshot` only adjusts the text.
#[must_use]
pub fn manual(
    model: &str,
    mode_context: &str,
    transcript: &[TranscriptLine],
    text: &str,
    with_screenshot: bool,
    effort: Option<Effort>,
) -> CompletionRequest {
    let mut user_text = text.trim().to_string();
    if with_screenshot {
        user_text.push_str("\n\n(A screenshot of my screen is attached.)");
    }
    CompletionRequest {
        user_text,
        max_tokens: MANUAL_MAX_TOKENS,
        effort,
        ..request(
            model,
            ANSWER_SYSTEM_PROMPT,
            split_context(mode_context, transcript),
        )
    }
}

#[must_use]
pub fn classifier(model: &str, transcript: &[TranscriptLine], question: &str) -> CompletionRequest {
    let recent = &transcript[transcript.len().saturating_sub(CLASSIFIER_CONTEXT_LINES)..];
    let mut user_text = String::new();
    if !recent.is_empty() {
        user_text.push_str("Earlier lines:\n");
        push_lines(&mut user_text, recent);
        user_text.push('\n');
    }
    user_text.push_str("Latest from the other person:\nThem: ");
    user_text.push_str(question.trim());
    user_text.push_str(
        "\n\nDid the other person ask a question or make a request that I should answer? \
         Reply yes or no.",
    );
    // Thinking off is allowed on Haiku 5.5 only at effort high or below, hence `Low`.
    CompletionRequest {
        user_text,
        max_tokens: CLASSIFIER_MAX_TOKENS,
        effort: Some(Effort::Low),
        thinking: Thinking::Disabled,
        ..request(model, CLASSIFIER_SYSTEM_PROMPT, Context::default())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Yes,
    No,
    Unclear,
}

#[must_use]
pub fn parse_verdict(text: &str) -> Verdict {
    let lower = text.to_lowercase();
    let first = lower
        .split(|c: char| !c.is_alphanumeric())
        .find(|w| !w.is_empty())
        .unwrap_or_default();
    match first {
        "yes" => Verdict::Yes,
        "no" => Verdict::No,
        _ => Verdict::Unclear,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(speaker: Speaker, id: u64, text: &str) -> TranscriptLine {
        TranscriptLine {
            id: format!("x-{id}"),
            speaker,
            start_ms: id * 1_000,
            end_ms: id * 1_000 + 900,
            text: text.into(),
            is_final: true,
            is_question: false,
        }
    }

    #[test]
    fn prefix_is_stable_and_question_goes_last() {
        let lines = vec![
            line(Speaker::Them, 1, "bom dia"),
            line(Speaker::Me, 2, " oi tudo bem "),
        ];
        let a = auto_answer("m", "", &lines, "como voce faria");
        let b = auto_answer("m", "", &lines, "como voce faria a migracao");
        assert_eq!(a.system_prefix, b.system_prefix);
        assert_eq!(a.context_text(), b.context_text());
        assert_eq!(
            a.context_text(),
            "Transcript so far:\nThem: bom dia\nMe: oi tudo bem\n"
        );
        assert!(a.user_text.contains("Them: como voce faria\n"));
        assert_eq!(a.effort, Some(Effort::Low));
        assert_eq!(a.thinking, Thinking::Adaptive);
        assert_eq!(a.max_tokens, 1_024);
        let m = manual("m", "", &lines, "summarise", true, None);
        assert_eq!(m.system_prefix, a.system_prefix);
        assert_eq!(m.context_text(), a.context_text());
        assert_eq!(m.max_tokens, 4_096);
        assert!(m.user_text.starts_with("summarise"));
        assert!(m.user_text.contains("screenshot"));
    }

    #[test]
    fn mode_context_precedes_transcript() {
        let lines = vec![line(Speaker::Them, 1, "hi")];
        assert_eq!(
            context(" Sales call ", &lines),
            "Meeting context:\nSales call\n\nTranscript so far:\nThem: hi\n"
        );
        assert_eq!(context("", &[]), "");
    }

    #[test]
    fn final_lines_before_the_first_volatile_one_form_the_history() {
        let mut lines = vec![
            line(Speaker::Them, 1, "bom dia"),
            line(Speaker::Me, 2, "oi"),
            line(Speaker::Them, 3, "como voce"),
            line(Speaker::Me, 4, "hm"),
        ];
        lines[2].is_final = false;
        let a = split_context(" Sales ", &lines);
        assert_eq!(a.mode, "Meeting context:\nSales\n\n");
        assert_eq!(a.history, "Transcript so far:\nThem: bom dia\nMe: oi\n");
        assert_eq!(a.recent, "Them: como voce\nMe: hm\n");
        // A revision of the volatile line leaves the cacheable part byte-identical.
        lines[2].text = "como voce faria a migracao".into();
        let b = split_context(" Sales ", &lines);
        assert_eq!(a.history, b.history);
        assert_ne!(a.recent, b.recent);
    }

    #[test]
    fn classifier_is_small() {
        let lines: Vec<_> = (0..10).map(|i| line(Speaker::Me, i, "x")).collect();
        let c = classifier("m", &lines, "qual o prazo");
        assert_eq!(c.max_tokens, 256);
        assert_eq!(c.thinking, Thinking::Disabled);
        assert_eq!(c.effort, Some(Effort::Low));
        assert_eq!(
            c.user_text.matches("Me: x").count(),
            CLASSIFIER_CONTEXT_LINES
        );
        assert!(c.user_text.contains("Them: qual o prazo"));
    }

    #[test]
    fn verdicts() {
        assert_eq!(parse_verdict("yes"), Verdict::Yes);
        assert_eq!(parse_verdict(" Yes."), Verdict::Yes);
        assert_eq!(parse_verdict("NO"), Verdict::No);
        assert_eq!(parse_verdict("nope"), Verdict::Unclear);
        assert_eq!(parse_verdict(""), Verdict::Unclear);
    }
}
