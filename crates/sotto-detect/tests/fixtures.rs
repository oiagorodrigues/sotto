use serde::Deserialize;
use sotto_detect::{DetectorConfig, Signal, TranscriptRevision, TurnTracker, score_question};
use sotto_protocol::Speaker;

#[derive(Deserialize)]
struct Fixtures {
    note: String,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    lang: String,
    label: bool,
    revisions: Vec<String>,
}

#[derive(Default, Debug)]
struct Confusion {
    tp: u32,
    fp: u32,
    tn: u32,
    fn_: u32,
}

impl Confusion {
    fn add(&mut self, label: bool, predicted: bool) {
        match (label, predicted) {
            (true, true) => self.tp += 1,
            (false, true) => self.fp += 1,
            (false, false) => self.tn += 1,
            (true, false) => self.fn_ += 1,
        }
    }

    fn precision(&self) -> f64 {
        f64::from(self.tp) / f64::from((self.tp + self.fp).max(1))
    }

    fn recall(&self) -> f64 {
        f64::from(self.tp) / f64::from((self.tp + self.fn_).max(1))
    }

    fn report(&self, name: &str) {
        eprintln!(
            "{name}: tp={} fp={} tn={} fn={} precision={:.3} recall={:.3}",
            self.tp,
            self.fp,
            self.tn,
            self.fn_,
            self.precision(),
            self.recall()
        );
    }
}

fn load() -> Result<Fixtures, Box<dyn std::error::Error>> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/questions.json");
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

#[test]
fn fixtures_meet_precision_and_recall_targets() -> Result<(), Box<dyn std::error::Error>> {
    let fixtures = load()?;
    assert!(fixtures.note.contains("Synthetic"));
    assert_eq!(fixtures.cases.len(), 60);
    let config = DetectorConfig::default();
    let mut scorer = Confusion::default();
    let mut tracker_cm = Confusion::default();

    for case in &fixtures.cases {
        assert!((2..=6).contains(&case.revisions.len()), "{}", case.id);
        let last = case.revisions.last().ok_or("no revisions")?;
        scorer.add(
            case.label,
            score_question(last).score >= config.question_threshold,
        );

        let mut tracker = TurnTracker::new(config);
        let mut signals = Vec::new();
        let mut end = 0;
        for (i, text) in case.revisions.iter().enumerate() {
            end = 600 * (i as u64 + 1);
            signals.extend(tracker.on_revision(&TranscriptRevision {
                speaker: Speaker::Them,
                segment_id: 1,
                revision: i as u32,
                start_ms: 0,
                end_ms: end,
                text: text.clone(),
                is_final: false,
            }));
        }
        signals.extend(tracker.tick(end + config.stability_ms));
        let likely = signals.iter().fold(false, |acc, s| match s {
            Signal::QuestionLikely { .. } => true,
            Signal::QuestionDropped { .. } => false,
            _ => acc,
        });
        let ended = signals
            .iter()
            .any(|s| matches!(s, Signal::TurnEnded { .. }));
        assert!(ended, "{} never ended", case.id);
        tracker_cm.add(case.label, likely);
        if likely != case.label {
            eprintln!(
                "miss {} ({}): label={} text={last:?}",
                case.id, case.lang, case.label
            );
        }
    }

    scorer.report("score_question on final revision");
    tracker_cm.report("TurnTracker (net QuestionLikely at turn end)");
    for cm in [&scorer, &tracker_cm] {
        assert!(cm.precision() >= 0.9, "precision {:.3}", cm.precision());
        assert!(cm.recall() >= 0.8, "recall {:.3}", cm.recall());
    }
    Ok(())
}
