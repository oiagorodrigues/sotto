use sotto_protocol::{SidecarEvent, Speaker};

use crate::score::score_question;
use crate::text::normalize_key;

/// Segments older than this (relative to the newest time seen) are forgotten.
const RETENTION_MS: u64 = 120_000;
/// How far below the threshold a likely question must fall before it is dropped.
const DROP_MARGIN: f32 = 0.25;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptRevision {
    pub speaker: Speaker,
    pub segment_id: u64,
    pub revision: u32,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub is_final: bool,
}

impl TranscriptRevision {
    /// Returns `None` for any event other than `Transcript`.
    #[must_use]
    pub fn from_event(event: &SidecarEvent) -> Option<Self> {
        match event {
            SidecarEvent::Transcript {
                speaker,
                segment_id,
                revision,
                start_ms,
                end_ms,
                text,
                is_final,
            } => Some(Self {
                speaker: *speaker,
                segment_id: *segment_id,
                revision: *revision,
                start_ms: *start_ms,
                end_ms: *end_ms,
                text: text.clone(),
                is_final: *is_final,
            }),
            _ => None,
        }
    }
}

impl TryFrom<&SidecarEvent> for TranscriptRevision {
    type Error = ();

    fn try_from(event: &SidecarEvent) -> Result<Self, ()> {
        Self::from_event(event).ok_or(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetectorConfig {
    pub question_threshold: f32,
    pub stability_ms: u64,
    /// Silence between two "them" segments that starts a new turn.
    pub max_turn_gap_ms: u64,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            question_threshold: 0.5,
            stability_ms: 400,
            max_turn_gap_ms: 1_500,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    /// First time a "them" turn crosses the threshold.
    QuestionLikely {
        turn_id: u64,
        text: String,
        score: f32,
    },
    /// The text of a likely-question turn changed.
    QuestionUpdated {
        turn_id: u64,
        text: String,
        score: f32,
    },
    /// Revoked, or the score fell well below the threshold.
    QuestionDropped { turn_id: u64 },
    /// "Them" text was stable for `stability_ms`.
    TurnEnded {
        turn_id: u64,
        text: String,
        end_ms: u64,
        score: f32,
    },
}

#[derive(Debug, Clone)]
struct Segment {
    speaker: Speaker,
    id: u64,
    revision: u32,
    start_ms: u64,
    end_ms: u64,
    text: String,
}

#[derive(Debug, Clone)]
struct TurnState {
    id: u64,
    first_segment: u64,
    key: String,
    text: String,
    end_ms: u64,
    score: f32,
    last_change_ms: u64,
    ended: bool,
    likely: bool,
}

struct TurnView {
    first_segment: u64,
    text: String,
    scored_text: String,
    end_ms: u64,
}

/// Groups "them" segments into turns and emits question signals as the text evolves.
///
/// A turn keeps the id of its first segment, so ids stay stable while the turn grows. Turn
/// stability is measured on the normalized text, so a late final that only adds punctuation
/// neither re-arms `TurnEnded` nor counts as new speech.
#[derive(Debug)]
pub struct TurnTracker {
    config: DetectorConfig,
    segments: Vec<Segment>,
    turns: Vec<TurnState>,
    next_turn_id: u64,
    clock_ms: u64,
}

impl TurnTracker {
    #[must_use]
    pub fn new(config: DetectorConfig) -> Self {
        Self {
            config,
            segments: Vec::new(),
            turns: Vec::new(),
            next_turn_id: 1,
            clock_ms: 0,
        }
    }

    /// Feed every transcript revision. "Me" revisions only split turns; detection runs on "them".
    pub fn on_revision(&mut self, rev: &TranscriptRevision) -> Vec<Signal> {
        let arrival_ms = self.clock_ms.max(rev.end_ms);
        self.clock_ms = arrival_ms;
        self.apply_revision(rev);
        let cutoff = arrival_ms.saturating_sub(RETENTION_MS);
        self.segments.retain(|s| s.end_ms >= cutoff);
        self.reconcile(arrival_ms)
    }

    /// Call periodically with the session clock (same base as the revisions).
    pub fn tick(&mut self, now_ms: u64) -> Vec<Signal> {
        self.clock_ms = self.clock_ms.max(now_ms);
        let stability = self.config.stability_ms;
        let mut out = Vec::new();
        for turn in &mut self.turns {
            if !turn.ended && now_ms >= turn.last_change_ms.saturating_add(stability) {
                turn.ended = true;
                out.push(Signal::TurnEnded {
                    turn_id: turn.id,
                    text: turn.text.clone(),
                    end_ms: turn.end_ms,
                    score: turn.score,
                });
            }
        }
        out
    }

    fn apply_revision(&mut self, rev: &TranscriptRevision) {
        let existing = self
            .segments
            .iter()
            .position(|s| s.speaker == rev.speaker && s.id == rev.segment_id);
        if let Some(i) = existing {
            if self.segments[i].revision > rev.revision {
                return;
            }
            self.segments.remove(i);
        }
        let text = rev.text.trim();
        if text.is_empty() {
            return;
        }
        let seg = Segment {
            speaker: rev.speaker,
            id: rev.segment_id,
            revision: rev.revision,
            start_ms: rev.start_ms,
            end_ms: rev.end_ms.max(rev.start_ms),
            text: text.to_owned(),
        };
        let at = self
            .segments
            .partition_point(|s| (s.start_ms, s.end_ms) <= (seg.start_ms, seg.end_ms));
        self.segments.insert(at, seg);
    }

    fn build_turns(&self) -> Vec<TurnView> {
        let mut views: Vec<TurnView> = Vec::new();
        let mut open = false;
        for seg in &self.segments {
            if seg.speaker == Speaker::Me {
                open = false;
                continue;
            }
            match views.last_mut() {
                Some(view)
                    if open
                        && seg.start_ms.saturating_sub(view.end_ms)
                            <= self.config.max_turn_gap_ms =>
                {
                    view.text.push(' ');
                    view.text.push_str(&seg.text);
                    view.scored_text.push_str(". ");
                    view.scored_text.push_str(&seg.text);
                    view.end_ms = view.end_ms.max(seg.end_ms);
                }
                _ => {
                    views.push(TurnView {
                        first_segment: seg.id,
                        text: seg.text.clone(),
                        scored_text: seg.text.clone(),
                        end_ms: seg.end_ms,
                    });
                    open = true;
                }
            }
        }
        views
    }

    fn reconcile(&mut self, arrival_ms: u64) -> Vec<Signal> {
        let threshold = self.config.question_threshold;
        let drop_below = (threshold - DROP_MARGIN).max(0.0);
        let mut out = Vec::new();
        let mut next = Vec::new();
        for view in self.build_turns() {
            let mut turn = match self
                .turns
                .iter()
                .position(|t| t.first_segment == view.first_segment)
            {
                Some(i) => self.turns.swap_remove(i),
                None => {
                    let id = self.next_turn_id;
                    self.next_turn_id += 1;
                    TurnState {
                        id,
                        first_segment: view.first_segment,
                        key: String::new(),
                        text: String::new(),
                        end_ms: 0,
                        score: 0.0,
                        last_change_ms: arrival_ms,
                        ended: false,
                        likely: false,
                    }
                }
            };
            let key = normalize_key(&view.text);
            if key != turn.key {
                turn.key = key;
                turn.last_change_ms = arrival_ms;
                turn.ended = false;
                turn.score = score_question(&view.scored_text).score;
                if !turn.likely {
                    if turn.score >= threshold {
                        turn.likely = true;
                        out.push(Signal::QuestionLikely {
                            turn_id: turn.id,
                            text: view.text.clone(),
                            score: turn.score,
                        });
                    }
                } else if turn.score < drop_below {
                    turn.likely = false;
                    out.push(Signal::QuestionDropped { turn_id: turn.id });
                } else {
                    out.push(Signal::QuestionUpdated {
                        turn_id: turn.id,
                        text: view.text.clone(),
                        score: turn.score,
                    });
                }
            }
            turn.text = view.text;
            turn.end_ms = view.end_ms;
            next.push(turn);
        }
        for gone in &self.turns {
            if gone.likely {
                out.push(Signal::QuestionDropped { turn_id: gone.id });
            }
        }
        next.sort_by_key(|t| t.id);
        self.turns = next;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rev(
        speaker: Speaker,
        id: u64,
        revision: u32,
        start: u64,
        end: u64,
        text: &str,
    ) -> TranscriptRevision {
        TranscriptRevision {
            speaker,
            segment_id: id,
            revision,
            start_ms: start,
            end_ms: end,
            text: text.to_owned(),
            is_final: false,
        }
    }

    fn them(id: u64, revision: u32, start: u64, end: u64, text: &str) -> TranscriptRevision {
        rev(Speaker::Them, id, revision, start, end, text)
    }

    fn tracker() -> TurnTracker {
        TurnTracker::new(DetectorConfig::default())
    }

    fn ended(signals: &[Signal]) -> Vec<(u64, &str)> {
        signals
            .iter()
            .filter_map(|s| match s {
                Signal::TurnEnded { turn_id, text, .. } => Some((*turn_id, text.as_str())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn question_likely_fires_once_then_updates() {
        let mut t = tracker();
        assert!(t.on_revision(&them(1, 0, 0, 300, "como")).is_empty());
        let first = t.on_revision(&them(1, 1, 0, 800, "como voce lidaria com a migracao"));
        assert!(matches!(
            first.as_slice(),
            [Signal::QuestionLikely { turn_id: 1, .. }]
        ));
        let second = t.on_revision(&them(
            1,
            2,
            0,
            1_200,
            "como voce lidaria com a migracao de banco",
        ));
        assert!(matches!(
            second.as_slice(),
            [Signal::QuestionUpdated { turn_id: 1, .. }]
        ));
        let same = t.on_revision(&them(
            1,
            3,
            0,
            1_300,
            "Como você lidaria com a migração de banco?",
        ));
        assert!(same.is_empty(), "{same:?}");
    }

    #[test]
    fn revocation_drops_a_likely_question() {
        let mut t = tracker();
        t.on_revision(&them(1, 0, 0, 800, "como voce lidaria com isso"));
        let out = t.on_revision(&them(1, 1, 0, 800, ""));
        assert_eq!(out, vec![Signal::QuestionDropped { turn_id: 1 }]);
        assert!(t.tick(10_000).is_empty());
    }

    #[test]
    fn score_falling_well_below_threshold_drops() {
        let mut t = tracker();
        t.on_revision(&them(1, 0, 0, 800, "como voce lidaria com isso"));
        let out = t.on_revision(&them(
            1,
            1,
            0,
            900,
            "eu acho que a gente deveria lidar com isso depois",
        ));
        assert_eq!(out, vec![Signal::QuestionDropped { turn_id: 1 }]);
    }

    #[test]
    fn later_revision_replaces_earlier_one() {
        let mut t = tracker();
        t.on_revision(&them(1, 0, 0, 500, "so i will handle"));
        t.on_revision(&them(1, 1, 0, 900, "so how would you handle it"));
        let out = t.tick(1_300);
        assert_eq!(ended(&out), vec![(1, "so how would you handle it")]);
    }

    #[test]
    fn stale_revision_is_ignored() {
        let mut t = tracker();
        t.on_revision(&them(1, 5, 0, 900, "como voce faria isso"));
        assert!(t.on_revision(&them(1, 2, 0, 900, "outra coisa")).is_empty());
        assert_eq!(ended(&t.tick(1_300)), vec![(1, "como voce faria isso")]);
    }

    #[test]
    fn me_speech_splits_turns() {
        let mut t = tracker();
        t.on_revision(&them(1, 0, 0, 1_000, "como voce faria"));
        t.on_revision(&rev(Speaker::Me, 100, 0, 1_100, 1_400, "boa pergunta"));
        t.on_revision(&them(2, 0, 1_500, 2_200, "e o prazo"));
        let out = t.tick(3_000);
        assert_eq!(ended(&out), vec![(1, "como voce faria"), (2, "e o prazo")]);
    }

    #[test]
    fn silence_gap_splits_turns_and_short_gap_joins() {
        let mut t = tracker();
        t.on_revision(&them(1, 0, 0, 1_000, "primeira parte"));
        t.on_revision(&them(2, 0, 2_400, 3_000, "segunda parte"));
        t.on_revision(&them(3, 0, 5_000, 5_500, "terceira parte"));
        let out = t.tick(9_000);
        assert_eq!(
            ended(&out),
            vec![(1, "primeira parte segunda parte"), (2, "terceira parte")]
        );
    }

    #[test]
    fn turn_ended_waits_for_stability_without_a_final() {
        let mut t = tracker();
        t.on_revision(&them(1, 0, 0, 1_000, "qual o prazo de entrega"));
        assert!(t.tick(1_399).is_empty());
        let out = t.tick(1_400);
        assert!(matches!(
            out.as_slice(),
            [Signal::TurnEnded {
                turn_id: 1,
                end_ms: 1_000,
                ..
            }]
        ));
        assert!(t.tick(5_000).is_empty());
    }

    #[test]
    fn new_text_postpones_turn_end() {
        let mut t = tracker();
        t.on_revision(&them(1, 0, 0, 1_000, "qual o prazo"));
        t.tick(1_300);
        t.on_revision(&them(1, 1, 0, 1_350, "qual o prazo de entrega"));
        assert!(t.tick(1_700).is_empty());
        assert_eq!(ended(&t.tick(1_750)).len(), 1);
    }

    #[test]
    fn late_final_with_same_words_does_not_rearm() {
        let mut t = tracker();
        t.on_revision(&them(1, 0, 0, 1_000, "qual o prazo de entrega"));
        assert_eq!(ended(&t.tick(1_400)).len(), 1);
        t.on_revision(&them(1, 1, 0, 1_000, "Qual o prazo de entrega?"));
        assert!(t.tick(3_000).is_empty());
    }

    #[test]
    fn me_revisions_alone_emit_nothing() {
        let mut t = tracker();
        assert!(
            t.on_revision(&rev(Speaker::Me, 1, 0, 0, 900, "como voce faria isso"))
                .is_empty()
        );
        assert!(t.tick(5_000).is_empty());
    }

    #[test]
    fn sidecar_event_converts() {
        let ev = SidecarEvent::Transcript {
            speaker: Speaker::Them,
            segment_id: 3,
            revision: 1,
            start_ms: 10,
            end_ms: 20,
            text: "oi".into(),
            is_final: true,
        };
        let r = TranscriptRevision::from_event(&ev);
        assert_eq!(
            r.map(|r| (r.segment_id, r.revision, r.is_final)),
            Some((3, 1, true))
        );
        assert!(TranscriptRevision::from_event(&SidecarEvent::Stopped {}).is_none());
    }

    #[test]
    fn random_revisions_never_panic() {
        let words = [
            "como",
            "voce",
            "if",
            "se",
            "?",
            "",
            "  ",
            "é",
            "ç",
            "e\u{301}",
            "what",
            "a",
            "‽",
            "😀",
            "?!",
            "migration",
            "começar",
        ];
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut t = tracker();
        for _ in 0..5_000 {
            let n = next() % 8;
            let text = (0..n)
                .map(|_| words[(next() % words.len() as u64) as usize])
                .collect::<Vec<_>>()
                .join(" ");
            let start = next() % 100_000;
            let end = next() % 100_000;
            let speaker = if next() % 3 == 0 {
                Speaker::Me
            } else {
                Speaker::Them
            };
            t.on_revision(&rev(
                speaker,
                next() % 6,
                (next() % 4) as u32,
                start,
                end,
                &text,
            ));
            if next() % 2 == 0 {
                t.tick(next() % 200_000);
            }
        }
        t.tick(u64::MAX);
    }
}
