//! Cuts the continuous PCM stream of one speaker into turns with an energy gate.
//!
//! `gpt-live-transcribe` takes no server-side voice activity detection (it needs
//! `turn_detection: null`) and returns its final text only when the client commits the buffer,
//! so the client decides where a turn ends. The same decision keeps silence off the wire: the
//! API bills per minute of audio, and a meeting is mostly one side listening.
//!
//! A turn starts a little before the first loud window (pre-roll, so a soft onset is not cut)
//! and ends after a stretch of quiet. Every audio window sent belongs to exactly one turn, and
//! the turn's start and end are on the sidecar clock, which is what the transcript needs.

use std::collections::VecDeque;

use sotto_protocol::sidecar::PCM_SAMPLE_RATE_HZ;

const WINDOW_MS: u64 = 20;
const WINDOW_SAMPLES: usize = (PCM_SAMPLE_RATE_HZ as usize * WINDOW_MS as usize) / 1_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateConfig {
    /// A window above this RMS level counts as speech.
    pub threshold_dbfs: f32,
    /// Consecutive loud windows that open a turn, so a click does not.
    pub onset_windows: u32,
    pub preroll_ms: u64,
    /// Quiet that closes a turn.
    pub hangover_ms: u64,
    /// A longer turn is committed and a new one opened, so deltas keep a bounded latency to
    /// their final text and one item never grows without limit.
    pub max_turn_ms: u64,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            threshold_dbfs: -42.0,
            onset_windows: 3,
            preroll_ms: 300,
            hangover_ms: 700,
            max_turn_ms: 25_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum GateAction {
    /// A turn opened; its first audio follows. `start_ms` is the first sample that will be sent.
    TurnStarted { seq: u64, start_ms: u64 },
    /// Audio of the open turn, contiguous with what was sent before.
    Audio { start_ms: u64, samples: Vec<i16> },
    /// The turn is over; `end_ms` is the end of the last sample sent.
    TurnEnded { seq: u64, end_ms: u64 },
}

#[derive(Debug)]
struct Window {
    start_ms: u64,
    samples: Vec<i16>,
}

impl Window {
    fn end_ms(&self) -> u64 {
        self.start_ms + samples_to_ms(self.samples.len())
    }
}

fn samples_to_ms(n: usize) -> u64 {
    n as u64 * 1_000 / u64::from(PCM_SAMPLE_RATE_HZ)
}

#[must_use]
pub fn rms_dbfs(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return -120.0;
    }
    let sum: f64 = samples
        .iter()
        .map(|&s| {
            let v = f64::from(s) / 32_768.0;
            v * v
        })
        .sum();
    let mean = sum / samples.len() as f64;
    if mean <= 1e-12 {
        return -120.0;
    }
    (10.0 * mean.log10()) as f32
}

#[derive(Debug)]
pub struct TurnGate {
    config: GateConfig,
    next_seq: u64,
    /// Windows seen while idle, newest last; the turn start takes the pre-roll from here.
    preroll: VecDeque<Window>,
    loud_run: u32,
    open: Option<OpenTurn>,
}

#[derive(Debug)]
struct OpenTurn {
    seq: u64,
    start_ms: u64,
    end_ms: u64,
    quiet_ms: u64,
}

impl TurnGate {
    #[must_use]
    pub fn new(config: GateConfig) -> Self {
        Self {
            config,
            next_seq: 0,
            preroll: VecDeque::new(),
            loud_run: 0,
            open: None,
        }
    }

    /// End of the audio sent for the open turn, if one is open.
    #[must_use]
    pub fn open_end_ms(&self) -> Option<u64> {
        self.open.as_ref().map(|t| t.end_ms)
    }

    /// Feeds one PCM frame and returns what to send. Frames may have any length.
    pub fn push(&mut self, start_ms: u64, samples: &[i16]) -> Vec<GateAction> {
        let mut actions = Vec::new();
        let mut audio: Vec<i16> = Vec::new();
        let mut audio_start = 0;
        for (i, chunk) in samples.chunks(WINDOW_SAMPLES).enumerate() {
            let window = Window {
                start_ms: start_ms + samples_to_ms(i * WINDOW_SAMPLES),
                samples: chunk.to_vec(),
            };
            self.window(window, &mut actions, &mut audio, &mut audio_start);
        }
        flush_audio(&mut actions, &mut audio, audio_start);
        actions
    }

    /// Closes the open turn at the end of the stream.
    pub fn finish(&mut self) -> Vec<GateAction> {
        self.preroll.clear();
        self.loud_run = 0;
        self.open
            .take()
            .map(|t| GateAction::TurnEnded {
                seq: t.seq,
                end_ms: t.end_ms,
            })
            .into_iter()
            .collect()
    }

    fn window(
        &mut self,
        window: Window,
        actions: &mut Vec<GateAction>,
        audio: &mut Vec<i16>,
        audio_start: &mut u64,
    ) {
        let loud = rms_dbfs(&window.samples) > self.config.threshold_dbfs;
        let window_ms = samples_to_ms(window.samples.len());
        if let Some(turn) = self.open.as_mut() {
            if audio.is_empty() {
                *audio_start = window.start_ms;
            }
            audio.extend_from_slice(&window.samples);
            turn.end_ms = window.end_ms();
            turn.quiet_ms = if loud { 0 } else { turn.quiet_ms + window_ms };
            let too_long = turn.end_ms - turn.start_ms >= self.config.max_turn_ms;
            if turn.quiet_ms >= self.config.hangover_ms || too_long {
                flush_audio(actions, audio, *audio_start);
                actions.push(GateAction::TurnEnded {
                    seq: turn.seq,
                    end_ms: turn.end_ms,
                });
                self.open = None;
                self.loud_run = 0;
                self.preroll.clear();
                if too_long && loud {
                    // The speaker is still talking: continue in a new turn from the next window.
                    self.loud_run = self.config.onset_windows;
                }
            }
            return;
        }
        self.loud_run = if loud { self.loud_run + 1 } else { 0 };
        self.preroll.push_back(window);
        let keep = (self.config.preroll_ms / WINDOW_MS).max(u64::from(self.config.onset_windows));
        while self.preroll.len() as u64 > keep {
            self.preroll.pop_front();
        }
        if self.loud_run < self.config.onset_windows {
            return;
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        let start_ms = self.preroll.front().map_or(0, |w| w.start_ms);
        let mut end_ms = start_ms;
        flush_audio(actions, audio, *audio_start);
        actions.push(GateAction::TurnStarted { seq, start_ms });
        *audio_start = start_ms;
        for w in self.preroll.drain(..) {
            end_ms = w.end_ms();
            audio.extend_from_slice(&w.samples);
        }
        self.open = Some(OpenTurn {
            seq,
            start_ms,
            end_ms,
            quiet_ms: 0,
        });
    }
}

fn flush_audio(actions: &mut Vec<GateAction>, audio: &mut Vec<i16>, start_ms: u64) {
    if !audio.is_empty() {
        actions.push(GateAction::Audio {
            start_ms,
            samples: std::mem::take(audio),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOUD: i16 = 6_000;
    const QUIET: i16 = 20;

    fn ms(n: u64, level: i16) -> Vec<i16> {
        vec![level; (n * u64::from(PCM_SAMPLE_RATE_HZ) / 1_000) as usize]
    }

    fn gate() -> TurnGate {
        TurnGate::new(GateConfig::default())
    }

    fn audio_ms(actions: &[GateAction]) -> u64 {
        actions
            .iter()
            .map(|a| match a {
                GateAction::Audio { samples, .. } => samples_to_ms(samples.len()),
                _ => 0,
            })
            .sum()
    }

    #[test]
    fn silence_sends_nothing() {
        let mut g = gate();
        for i in 0..100 {
            assert!(g.push(i * 100, &ms(100, QUIET)).is_empty());
        }
        assert!(g.finish().is_empty());
    }

    #[test]
    fn a_turn_starts_with_pre_roll_and_ends_after_the_hangover() {
        let mut g = gate();
        for i in 0..10 {
            assert!(g.push(i * 100, &ms(100, QUIET)).is_empty());
        }
        // Speech from 1000 ms to 1500 ms.
        let started = g.push(1_000, &ms(500, LOUD));
        // The pre-roll keeps 15 windows of 20 ms ending at the third loud window (1040 ms).
        assert_eq!(
            started[0],
            GateAction::TurnStarted {
                seq: 0,
                start_ms: 760
            }
        );
        assert_eq!(audio_ms(&started), 1_500 - 760);
        let mut ended = Vec::new();
        let mut t = 1_500;
        while !ended
            .iter()
            .any(|a| matches!(a, GateAction::TurnEnded { .. }))
        {
            ended = g.push(t, &ms(100, QUIET));
            t += 100;
        }
        match ended.last() {
            Some(GateAction::TurnEnded { seq: 0, end_ms }) => {
                assert!(
                    (2_180..=2_300).contains(end_ms),
                    "ends ~700 ms after the last loud window, got {end_ms}"
                );
            }
            other => panic!("expected TurnEnded, got {other:?}"),
        }
        assert_eq!(g.open_end_ms(), None);
    }

    #[test]
    fn a_short_click_does_not_open_a_turn() {
        let mut g = gate();
        let mut samples = ms(40, LOUD);
        samples.extend(ms(500, QUIET));
        assert!(g.push(0, &samples).is_empty());
    }

    #[test]
    fn audio_is_contiguous_and_no_sample_is_lost_inside_a_turn() {
        let mut g = gate();
        let mut sent: Vec<(u64, usize)> = Vec::new();
        let mut t = 0;
        let mut feed = |g: &mut TurnGate, level: i16, dur: u64, t: &mut u64| {
            for a in g.push(*t, &ms(dur, level)) {
                if let GateAction::Audio { start_ms, samples } = a {
                    sent.push((start_ms, samples.len()));
                }
            }
            *t += dur;
        };
        feed(&mut g, QUIET, 400, &mut t);
        for _ in 0..20 {
            feed(&mut g, LOUD, 80, &mut t);
            feed(&mut g, QUIET, 130, &mut t);
        }
        for pair in sent.windows(2) {
            let end = pair[0].0 + samples_to_ms(pair[0].1);
            assert_eq!(pair[1].0, end, "gap or overlap in {pair:?}");
        }
        // Pauses shorter than the hangover keep one turn open.
        assert!(g.open_end_ms().is_some());
    }

    #[test]
    fn long_speech_is_split_into_turns() {
        let mut g = gate();
        let mut starts = 0;
        let mut ends = 0;
        for i in 0..60 {
            for a in g.push(i * 1_000, &ms(1_000, LOUD)) {
                match a {
                    GateAction::TurnStarted { .. } => starts += 1,
                    GateAction::TurnEnded { .. } => ends += 1,
                    GateAction::Audio { .. } => {}
                }
            }
        }
        assert!((2..=3).contains(&starts), "{starts}");
        assert_eq!(ends, starts - 1);
    }

    #[test]
    fn finish_closes_the_open_turn_once() {
        let mut g = gate();
        g.push(0, &ms(200, LOUD));
        assert!(matches!(
            g.finish().as_slice(),
            [GateAction::TurnEnded {
                seq: 0,
                end_ms: 200
            }]
        ));
        assert!(g.finish().is_empty());
    }

    #[test]
    fn frames_that_are_not_multiples_of_a_window_are_handled() {
        let mut g = gate();
        let all = ms(300, LOUD);
        let mut total = 0;
        let mut t = 0;
        for chunk in all.chunks(137) {
            let actions = g.push(t, chunk);
            total += audio_ms(&actions);
            t += samples_to_ms(chunk.len());
        }
        assert!(total > 0);
    }

    #[test]
    fn rms_levels() {
        assert!((rms_dbfs(&[i16::MAX; 100]) - 0.0).abs() < 0.01);
        assert_eq!(rms_dbfs(&[0; 100]), -120.0);
        assert_eq!(rms_dbfs(&[]), -120.0);
        assert!((rms_dbfs(&[16_384; 100]) + 6.02).abs() < 0.05);
    }
}
