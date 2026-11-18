//! Question detection and turn tracking on "them" transcript revisions (ADR 0006).

mod guards;
mod score;
mod text;
mod tracker;

pub use guards::{GuardBlock, GuardConfig, Guards};
pub use score::{DEFAULT_QUESTION_THRESHOLD, QuestionScore, meaning_changed, score_question};
pub use tracker::{DetectorConfig, Signal, TranscriptRevision, TurnTracker};
