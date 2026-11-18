//! Maps provider events onto the transcript revisions the on-device engine produces, so the
//! question detector (ADR 0006) and the transcript view work unchanged.
//!
//! One turn is one provider item and one transcript segment: the segment id is the turn's
//! sequence number, every delta is a new revision of it, and the completed text is the final
//! one. The time range comes from the audio the client sent, on the sidecar clock, because the
//! live model reports no offsets.

use std::collections::VecDeque;

use sotto_protocol::Speaker;
use sotto_protocol::sidecar::SidecarEvent;

use super::wire::ServerEvent;

/// A turn the provider never answers must not block the ones behind it forever.
const MAX_TRACKED_TURNS: usize = 256;

#[derive(Debug)]
struct Turn {
    seq: u64,
    start_ms: u64,
    end_ms: u64,
    /// Audio is still being sent for this turn.
    open: bool,
    item_id: Option<String>,
    text: String,
    revision: u32,
    /// A non-empty revision was emitted, so an empty completion has something to revoke.
    shown: bool,
    done: bool,
}

#[derive(Debug)]
pub struct Mapper {
    speaker: Speaker,
    turns: VecDeque<Turn>,
}

impl Mapper {
    #[must_use]
    pub fn new(speaker: Speaker) -> Self {
        Self {
            speaker,
            turns: VecDeque::new(),
        }
    }

    pub fn turn_started(&mut self, seq: u64, start_ms: u64) {
        self.turns.push_back(Turn {
            seq,
            start_ms,
            end_ms: start_ms,
            open: true,
            item_id: None,
            text: String::new(),
            revision: 0,
            shown: false,
            done: false,
        });
        while self.turns.len() > MAX_TRACKED_TURNS {
            self.turns.pop_front();
        }
    }

    /// Audio of the open turn reached `end_ms`.
    pub fn audio_sent(&mut self, end_ms: u64) {
        if let Some(t) = self.turns.iter_mut().rev().find(|t| t.open) {
            t.end_ms = t.end_ms.max(end_ms);
        }
    }

    pub fn turn_ended(&mut self, seq: u64, end_ms: u64) {
        if let Some(t) = self.turns.iter_mut().find(|t| t.seq == seq) {
            t.open = false;
            t.end_ms = t.end_ms.max(end_ms);
        }
    }

    /// Items arrive in the order the turns were committed, so an item id seen for the first
    /// time belongs to the oldest turn that has none yet.
    fn resolve(&mut self, item_id: &str) -> Option<usize> {
        if let Some(i) = self
            .turns
            .iter()
            .position(|t| t.item_id.as_deref() == Some(item_id))
        {
            return Some(i);
        }
        let i = self
            .turns
            .iter()
            .position(|t| t.item_id.is_none() && !t.done)?;
        self.turns[i].item_id = Some(item_id.to_string());
        Some(i)
    }

    fn revision_event(&mut self, i: usize, is_final: bool) -> Option<SidecarEvent> {
        let t = &mut self.turns[i];
        let empty = t.text.trim().is_empty();
        if empty && !(is_final && t.shown) {
            return None;
        }
        t.revision += 1;
        t.shown = !empty;
        let text = if empty { String::new() } else { t.text.clone() };
        Some(SidecarEvent::Transcript {
            speaker: self.speaker,
            segment_id: t.seq,
            revision: t.revision,
            start_ms: t.start_ms,
            end_ms: t.end_ms.max(t.start_ms),
            text,
            is_final,
        })
    }

    fn drop_finished(&mut self) {
        while self.turns.front().is_some_and(|t| t.done && !t.open) {
            self.turns.pop_front();
        }
    }

    pub fn on_event(&mut self, event: &ServerEvent) -> Vec<SidecarEvent> {
        let mut out = Vec::new();
        match event {
            ServerEvent::Committed { item_id } => {
                self.resolve(item_id);
            }
            ServerEvent::Delta { item_id, text } => {
                if let Some(i) = self.resolve(item_id).filter(|&i| !self.turns[i].done) {
                    self.turns[i].text.push_str(text);
                    out.extend(self.revision_event(i, false));
                }
            }
            ServerEvent::Completed { item_id, text } => {
                if let Some(i) = self.resolve(item_id).filter(|&i| !self.turns[i].done) {
                    self.turns[i].text.clone_from(text);
                    self.turns[i].done = true;
                    out.extend(self.revision_event(i, true));
                }
            }
            ServerEvent::Failed { item_id } => {
                if let Some(i) = item_id
                    .as_deref()
                    .and_then(|id| self.resolve(id))
                    .filter(|&i| !self.turns[i].done)
                {
                    self.turns[i].text.clear();
                    self.turns[i].done = true;
                    out.extend(self.revision_event(i, true));
                }
            }
            ServerEvent::SessionUpdated | ServerEvent::Error { .. } | ServerEvent::Other => {}
        }
        self.drop_finished();
        out
    }

    /// The connection is gone: whatever text a turn has becomes final so nothing stays
    /// partial forever, and the turns are forgotten.
    pub fn flush(&mut self) -> Vec<SidecarEvent> {
        let mut out = Vec::new();
        for i in 0..self.turns.len() {
            if !self.turns[i].done {
                self.turns[i].done = true;
                self.turns[i].open = false;
                out.extend(self.revision_event(i, true));
            }
        }
        self.turns.clear();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(id: &str, t: &str) -> ServerEvent {
        ServerEvent::Delta {
            item_id: id.into(),
            text: t.into(),
        }
    }

    fn completed(id: &str, t: &str) -> ServerEvent {
        ServerEvent::Completed {
            item_id: id.into(),
            text: t.into(),
        }
    }

    fn parts(e: &SidecarEvent) -> (u64, u32, u64, u64, &str, bool) {
        match e {
            SidecarEvent::Transcript {
                segment_id,
                revision,
                start_ms,
                end_ms,
                text,
                is_final,
                ..
            } => (*segment_id, *revision, *start_ms, *end_ms, text, *is_final),
            other => panic!("not a transcript: {other:?}"),
        }
    }

    #[test]
    fn deltas_become_increasing_revisions_and_completed_is_final() {
        let mut m = Mapper::new(Speaker::Them);
        m.turn_started(0, 1_000);
        m.audio_sent(1_400);
        let a = m.on_event(&delta("i1", "como"));
        m.audio_sent(1_800);
        let b = m.on_event(&delta("i1", " você"));
        m.turn_ended(0, 2_500);
        m.on_event(&ServerEvent::Committed {
            item_id: "i1".into(),
        });
        let c = m.on_event(&completed("i1", "Como você faria?"));
        assert_eq!(parts(&a[0]), (0, 1, 1_000, 1_400, "como", false));
        assert_eq!(parts(&b[0]), (0, 2, 1_000, 1_800, "como você", false));
        assert_eq!(parts(&c[0]), (0, 3, 1_000, 2_500, "Como você faria?", true));
    }

    #[test]
    fn each_turn_gets_its_own_stable_segment_in_commit_order() {
        let mut m = Mapper::new(Speaker::Me);
        m.turn_started(0, 0);
        m.turn_ended(0, 500);
        m.turn_started(1, 2_000);
        m.turn_ended(1, 2_600);
        // Item ids are opaque and unrelated to the turn numbers.
        let first = m.on_event(&delta("zzz", "one"));
        let second = m.on_event(&delta("aaa", "two"));
        assert_eq!(parts(&first[0]).0, 0);
        assert_eq!(parts(&second[0]).0, 1);
        let later = m.on_event(&delta("aaa", " more"));
        assert_eq!(parts(&later[0]), (1, 2, 2_000, 2_600, "two more", false));
    }

    #[test]
    fn a_committed_turn_without_text_keeps_the_queue_aligned() {
        let mut m = Mapper::new(Speaker::Them);
        m.turn_started(0, 0);
        m.turn_ended(0, 300);
        m.turn_started(1, 1_000);
        m.turn_ended(1, 1_500);
        m.on_event(&ServerEvent::Committed {
            item_id: "noise".into(),
        });
        assert!(m.on_event(&completed("noise", "")).is_empty());
        m.on_event(&ServerEvent::Committed {
            item_id: "real".into(),
        });
        let out = m.on_event(&completed("real", "Hello."));
        assert_eq!(parts(&out[0]), (1, 1, 1_000, 1_500, "Hello.", true));
    }

    #[test]
    fn an_empty_completion_revokes_what_was_shown() {
        let mut m = Mapper::new(Speaker::Them);
        m.turn_started(0, 0);
        m.turn_ended(0, 800);
        m.on_event(&delta("i", "uh"));
        let out = m.on_event(&completed("i", "  "));
        assert_eq!(parts(&out[0]), (0, 2, 0, 800, "", true));
    }

    #[test]
    fn whitespace_only_deltas_emit_nothing() {
        let mut m = Mapper::new(Speaker::Them);
        m.turn_started(0, 0);
        assert!(m.on_event(&delta("i", " ")).is_empty());
    }

    #[test]
    fn events_after_completion_and_for_unknown_items_are_ignored() {
        let mut m = Mapper::new(Speaker::Them);
        m.turn_started(0, 0);
        m.turn_ended(0, 100);
        m.on_event(&completed("i", "Done."));
        assert!(m.on_event(&delta("i", " late")).is_empty());
        assert!(m.on_event(&completed("other", "stray")).is_empty());
    }

    #[test]
    fn failure_revokes_a_shown_turn() {
        let mut m = Mapper::new(Speaker::Them);
        m.turn_started(0, 0);
        m.turn_ended(0, 100);
        m.on_event(&delta("i", "hm"));
        let out = m.on_event(&ServerEvent::Failed {
            item_id: Some("i".into()),
        });
        assert_eq!(parts(&out[0]).4, "");
    }

    #[test]
    fn flush_finalizes_partial_text_and_forgets_turns() {
        let mut m = Mapper::new(Speaker::Them);
        m.turn_started(0, 0);
        m.audio_sent(400);
        m.on_event(&delta("i", "partial text"));
        m.turn_started(1, 900);
        let out = m.flush();
        assert_eq!(out.len(), 1);
        assert_eq!(parts(&out[0]), (0, 2, 0, 400, "partial text", true));
        assert!(m.flush().is_empty());
    }

    #[test]
    fn tracked_turns_are_bounded() {
        let mut m = Mapper::new(Speaker::Them);
        for seq in 0..(MAX_TRACKED_TURNS as u64 + 10) {
            m.turn_started(seq, seq * 10);
        }
        assert_eq!(m.turns.len(), MAX_TRACKED_TURNS);
    }
}
