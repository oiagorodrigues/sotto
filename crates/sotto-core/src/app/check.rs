//! The local transcription check: plays the bundled clips through the on-device engine in
//! file mode at real-time pace and measures what a user would feel in a call. Time to first
//! partial runs from the first spoken sound, time to final from the end of speech, and the
//! word error rate compares the final text with the clip's reference text.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sotto_protocol::onboarding::{ClipOutcome, ClipResultView};
use sotto_protocol::sidecar::{SidecarErrorCode, SidecarEvent};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::locale::FALLBACK_LOCALE;
use super::wer;
use super::{SIDECAR_SHUTDOWN_GRACE, SidecarFactory, SidecarLink};
use crate::sidecar::{SidecarError, SidecarOutput, SidecarProcess};

/// Reference texts live next to the audio so the clips and their text change together.
const MANIFEST: &str = include_str!("../../../../fixtures/audio/clips.json");

/// Window for the speech detector.
const WINDOW_MS: u64 = 20;
/// Frames quieter than this are silence. TTS clips have a noise floor far below it.
const SPEECH_FLOOR_DBFS: f32 = -45.0;
/// Time the helper may spend loading the model before the clip starts playing.
const SETUP_ALLOWANCE: Duration = Duration::from_secs(30);
/// After the audio ends the engine still has to finalize.
const FINALIZE_ALLOWANCE: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Clip {
    pub locale: String,
    pub file: String,
    pub text: String,
}

#[derive(Deserialize)]
struct Manifest {
    clips: Vec<Clip>,
}

pub(crate) fn bundled_clips() -> Vec<Clip> {
    serde_json::from_str::<Manifest>(MANIFEST)
        .map(|m| m.clips)
        .unwrap_or_default()
}

/// The locales to check: the user's, then `en-US` when it differs, each with its clip when
/// one is bundled.
pub(crate) fn plan(user_locale: &str, clips: &[Clip]) -> Vec<(String, Option<Clip>)> {
    let mut locales = vec![user_locale.to_string()];
    if !user_locale.eq_ignore_ascii_case(FALLBACK_LOCALE) {
        locales.push(FALLBACK_LOCALE.to_string());
    }
    locales
        .into_iter()
        .map(|locale| {
            let clip = clips
                .iter()
                .find(|c| c.locale.eq_ignore_ascii_case(&locale))
                .cloned();
            (locale, clip)
        })
        .collect()
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum WavError {
    #[error("not a RIFF/WAVE file")]
    NotWav,
    #[error("unsupported audio format: {0}")]
    Unsupported(&'static str),
    #[error("the file ends early")]
    Truncated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Audio {
    pub duration_ms: u64,
    /// First and last spoken millisecond, or `None` for a silent clip.
    pub speech: Option<(u64, u64)>,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Reads a mono 16-bit PCM WAV and finds where the speech starts and ends.
pub(crate) fn analyze_wav(bytes: &[u8]) -> Result<Audio, WavError> {
    if bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err(WavError::NotWav);
    }
    let mut at = 12;
    let mut rate = None;
    let mut samples: Option<&[u8]> = None;
    while let (Some(id), Some(size)) = (bytes.get(at..at + 4), u32_at(bytes, at + 4)) {
        let body = at + 8;
        let size = size as usize;
        match id {
            b"fmt " => {
                let tag = u16_at(bytes, body).ok_or(WavError::Truncated)?;
                let channels = u16_at(bytes, body + 2).ok_or(WavError::Truncated)?;
                let hz = u32_at(bytes, body + 4).ok_or(WavError::Truncated)?;
                let bits = u16_at(bytes, body + 14).ok_or(WavError::Truncated)?;
                if tag != 1 {
                    return Err(WavError::Unsupported("not integer PCM"));
                }
                if channels != 1 {
                    return Err(WavError::Unsupported("not mono"));
                }
                if bits != 16 {
                    return Err(WavError::Unsupported("not 16-bit"));
                }
                rate = Some(hz);
            }
            b"data" => {
                let end = body.saturating_add(size).min(bytes.len());
                samples = bytes.get(body..end);
            }
            _ => {}
        }
        at = body.saturating_add(size).saturating_add(size & 1);
    }
    let rate = u64::from(rate.ok_or(WavError::Truncated)?);
    let data = samples.ok_or(WavError::Truncated)?;
    if rate == 0 {
        return Err(WavError::Unsupported("zero sample rate"));
    }
    let pcm: Vec<i16> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| i16::from_le_bytes(*p))
        .collect();
    let duration_ms = pcm.len() as u64 * 1_000 / rate;
    let window = usize::try_from(rate * WINDOW_MS / 1_000)
        .unwrap_or(1)
        .max(1);
    let loud: Vec<bool> = pcm
        .chunks(window)
        .map(|w| {
            let energy: f64 = w.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / w.len() as f64;
            let dbfs = 10.0 * (energy / (32_768.0_f64 * 32_768.0)).max(1e-12).log10();
            dbfs as f32 > SPEECH_FLOOR_DBFS
        })
        .collect();
    let speech = loud.iter().position(|l| *l).and_then(|first| {
        let last = loud.iter().rposition(|l| *l)?;
        Some((
            first as u64 * WINDOW_MS,
            ((last as u64 + 1) * WINDOW_MS).min(duration_ms),
        ))
    });
    Ok(Audio {
        duration_ms,
        speech,
    })
}

/// A sidecar event and when it arrived, in milliseconds after the helper reported `Started`
/// (which is also when it begins feeding the clip).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Timed {
    pub at_ms: u64,
    pub event: SidecarEvent,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Measurement {
    pub first_partial_ms: u64,
    pub final_ms: Option<u64>,
    /// The latest revision of every segment, in order. Transcript text: never log it.
    pub hypothesis: String,
}

/// `None` when the engine produced no text at all.
pub(crate) fn measure(
    events: &[Timed],
    speech_start_ms: u64,
    speech_end_ms: u64,
) -> Option<Measurement> {
    let mut first_text = None;
    let mut last_final = None;
    let mut segments: BTreeMap<u64, (u32, &str)> = BTreeMap::new();
    for t in events {
        let SidecarEvent::Transcript {
            segment_id,
            revision,
            text,
            is_final,
            ..
        } = &t.event
        else {
            continue;
        };
        let has_text = !text.trim().is_empty();
        if has_text {
            first_text.get_or_insert(t.at_ms);
            if *is_final {
                last_final = Some(t.at_ms);
            }
        }
        let newer = segments.get(segment_id).is_none_or(|(r, _)| revision >= r);
        if newer {
            segments.insert(*segment_id, (*revision, text.as_str()));
        }
    }
    let first_text = first_text?;
    let hypothesis = segments
        .values()
        .map(|(_, text)| text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Some(Measurement {
        first_partial_ms: first_text.saturating_sub(speech_start_ms),
        final_ms: last_final.map(|at| at.saturating_sub(speech_end_ms)),
        hypothesis,
    })
}

/// Spawns the sidecar in real-time file mode and wires its stdin like a normal link: dropping
/// the link shuts the process down.
pub(super) fn file_mode_link(
    path: &Path,
    wav: &Path,
    locale: &str,
) -> Result<SidecarLink, SidecarError> {
    let (process, outputs) = SidecarProcess::spawn_file_mode_with(path, wav, locale, true)?;
    let (commands, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while rx.recv().await.is_some() {}
        process.shutdown(SIDECAR_SHUTDOWN_GRACE).await;
    });
    Ok(SidecarLink {
        commands,
        outputs,
        pcm: None,
    })
}

enum Capture {
    Events(Vec<Timed>),
    Failed(ClipOutcome, String),
}

fn failed(outcome: ClipOutcome, message: impl Into<String>) -> Capture {
    Capture::Failed(outcome, message.into())
}

async fn capture(
    factory: &dyn SidecarFactory,
    wav: &Path,
    locale: &str,
    duration_ms: u64,
) -> Capture {
    let mut link = match factory.spawn_file_mode(wav, locale) {
        Ok(link) => link,
        Err(e) => {
            tracing::warn!(error = %e, "transcription check: the audio helper did not start");
            return failed(ClipOutcome::EngineFailed, "The audio helper did not start");
        }
    };
    let deadline =
        Instant::now() + SETUP_ALLOWANCE + Duration::from_millis(duration_ms) + FINALIZE_ALLOWANCE;
    let mut zero: Option<Instant> = None;
    let mut events = Vec::new();
    loop {
        let out = match tokio::time::timeout_at(deadline, link.outputs.recv()).await {
            Ok(Some(out)) => out,
            Ok(None) => break,
            Err(_) => return failed(ClipOutcome::EngineFailed, "The engine took too long"),
        };
        match out {
            SidecarOutput::Event(SidecarEvent::Ready {
                installed_locales, ..
            }) => {
                if !installed_locales
                    .iter()
                    .any(|l| l.eq_ignore_ascii_case(locale))
                {
                    return failed(
                        ClipOutcome::LocaleNotInstalled,
                        format!("The on-device model for {locale} is not installed"),
                    );
                }
            }
            SidecarOutput::Event(SidecarEvent::Started { .. }) => zero = Some(Instant::now()),
            SidecarOutput::Event(event @ SidecarEvent::Transcript { .. }) => {
                let at_ms = zero.map_or(0, |z| {
                    u64::try_from(z.elapsed().as_millis()).unwrap_or(u64::MAX)
                });
                events.push(Timed { at_ms, event });
            }
            SidecarOutput::Event(SidecarEvent::Error {
                code,
                message,
                fatal,
            }) => {
                tracing::warn!(?code, fatal, "transcription check: sidecar error");
                match code {
                    SidecarErrorCode::UnsupportedLocale | SidecarErrorCode::AssetsUnavailable => {
                        return failed(
                            ClipOutcome::LocaleNotInstalled,
                            format!("The on-device model for {locale} is not available"),
                        );
                    }
                    _ if fatal => return failed(ClipOutcome::EngineFailed, message),
                    _ => {}
                }
            }
            SidecarOutput::Event(SidecarEvent::Stopped {}) => break,
            SidecarOutput::Exited { success, .. } => {
                if !success && events.is_empty() {
                    return failed(
                        ClipOutcome::EngineFailed,
                        "The audio helper stopped unexpectedly",
                    );
                }
                break;
            }
            SidecarOutput::Event(_) | SidecarOutput::BadLine { .. } => {}
        }
    }
    Capture::Events(events)
}

fn result(locale: &str, outcome: ClipOutcome, message: Option<String>) -> ClipResultView {
    ClipResultView {
        locale: locale.to_string(),
        outcome,
        first_partial_ms: None,
        final_ms: None,
        error_rate_pct: None,
        message,
    }
}

fn clamp_ms(ms: u64) -> u32 {
    u32::try_from(ms).unwrap_or(u32::MAX)
}

pub(crate) async fn run_clip(
    factory: &dyn SidecarFactory,
    clips_dir: Option<&Path>,
    locale: &str,
    clip: Option<&Clip>,
) -> ClipResultView {
    let Some(clip) = clip else {
        return result(
            locale,
            ClipOutcome::NoSample,
            Some(format!("There is no sample clip for {locale}")),
        );
    };
    let Some(dir) = clips_dir else {
        return result(
            locale,
            ClipOutcome::EngineFailed,
            Some("The sample clips are missing from this build".into()),
        );
    };
    let wav = dir.join(&clip.file);
    let audio = match tokio::fs::read(&wav).await {
        Ok(bytes) => analyze_wav(&bytes),
        Err(_) => {
            return result(
                locale,
                ClipOutcome::EngineFailed,
                Some("The sample clip could not be read".into()),
            );
        }
    };
    let (audio, (start, end)) = match audio {
        Ok(a) => match a.speech {
            Some(span) => (a, span),
            None => {
                return result(
                    locale,
                    ClipOutcome::EngineFailed,
                    Some("The sample clip is silent".into()),
                );
            }
        },
        Err(e) => {
            return result(
                locale,
                ClipOutcome::EngineFailed,
                Some(format!("The sample clip is not usable: {e}")),
            );
        }
    };
    match capture(factory, &wav, locale, audio.duration_ms).await {
        Capture::Failed(outcome, message) => result(locale, outcome, Some(message)),
        Capture::Events(events) => score(locale, &clip.text, &events, start, end),
    }
}

fn score(
    locale: &str,
    reference: &str,
    events: &[Timed],
    speech_start_ms: u64,
    speech_end_ms: u64,
) -> ClipResultView {
    let Some(m) = measure(events, speech_start_ms, speech_end_ms) else {
        return result(
            locale,
            ClipOutcome::EngineFailed,
            Some("The engine produced no text".into()),
        );
    };
    ClipResultView {
        locale: locale.to_string(),
        outcome: ClipOutcome::Measured,
        first_partial_ms: Some(clamp_ms(m.first_partial_ms)),
        final_ms: m.final_ms.map(clamp_ms),
        error_rate_pct: wer::word_error_rate_pct(reference, &m.hypothesis),
        message: None,
    }
}

/// Runs every planned clip one after another. `progress(done, total, locale)` is called before
/// each clip starts and once more when all are done.
pub(crate) async fn run_check(
    factory: &dyn SidecarFactory,
    clips_dir: Option<PathBuf>,
    user_locale: &str,
    mut progress: impl FnMut(u32, u32, Option<&str>),
) -> Vec<ClipResultView> {
    let planned = plan(user_locale, &bundled_clips());
    let total = u32::try_from(planned.len()).unwrap_or(u32::MAX);
    let mut results = Vec::new();
    for (done, (locale, clip)) in (0_u32..).zip(&planned) {
        progress(done, total, Some(locale));
        results.push(run_clip(factory, clips_dir.as_deref(), locale, clip.as_ref()).await);
    }
    progress(total, total, None);
    results
}

#[cfg(test)]
mod tests {
    use sotto_protocol::sidecar::Speaker;

    use super::*;

    const REAL_RUN: &str = include_str!("testdata/check-en-US.jsonl");

    fn wav(samples: &[i16], rate: u32) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16_u32.to_le_bytes());
        out.extend_from_slice(&1_u16.to_le_bytes());
        out.extend_from_slice(&1_u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2_u16.to_le_bytes());
        out.extend_from_slice(&16_u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    fn tone(ms: usize, amplitude: i16) -> Vec<i16> {
        (0..ms * 16)
            .map(|i| if i % 2 == 0 { amplitude } else { -amplitude })
            .collect()
    }

    fn transcript(at_ms: u64, seg: u64, rev: u32, text: &str, is_final: bool) -> Timed {
        Timed {
            at_ms,
            event: SidecarEvent::Transcript {
                speaker: Speaker::Them,
                segment_id: seg,
                revision: rev,
                start_ms: 0,
                end_ms: 0,
                text: text.into(),
                is_final,
            },
        }
    }

    #[test]
    fn the_manifest_lists_both_clips_with_reference_text() {
        let clips = bundled_clips();
        let locales: Vec<_> = clips.iter().map(|c| c.locale.as_str()).collect();
        assert_eq!(locales, ["pt-BR", "en-US"]);
        assert!(clips.iter().all(|c| !c.text.trim().is_empty()));
    }

    #[test]
    fn the_bundled_wavs_match_the_documented_format() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio");
        for clip in bundled_clips() {
            let bytes = std::fs::read(dir.join(&clip.file)).unwrap();
            let audio = analyze_wav(&bytes).unwrap();
            let (start, end) = audio.speech.unwrap();
            assert!(start < 1_000, "{}: speech starts at {start}", clip.file);
            assert!(end > 5_000 && end <= audio.duration_ms, "{}", clip.file);
        }
    }

    #[test]
    fn plan_checks_the_user_locale_then_english() {
        let clips = bundled_clips();
        let p = plan("pt-br", &clips);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].1.as_ref().map(|c| c.locale.as_str()), Some("pt-BR"));
        assert_eq!(p[1].0, "en-US");
        let p = plan("fr-FR", &clips);
        assert!(p[0].1.is_none());
        assert!(p[1].1.is_some());
        assert_eq!(plan("en-US", &clips).len(), 1);
    }

    #[test]
    fn wav_analysis_finds_speech_between_silences() {
        let mut samples = vec![0_i16; 16 * 300];
        samples.extend(tone(1_000, 3_000));
        samples.extend(vec![0_i16; 16 * 500]);
        let audio = analyze_wav(&wav(&samples, 16_000)).unwrap();
        assert_eq!(audio.duration_ms, 1_800);
        assert_eq!(audio.speech, Some((300, 1_300)));
    }

    #[test]
    fn a_silent_clip_has_no_speech() {
        let audio = analyze_wav(&wav(&vec![0_i16; 16_000], 16_000)).unwrap();
        assert_eq!(audio.speech, None);
        let quiet = analyze_wav(&wav(&tone(500, 20), 16_000)).unwrap();
        assert_eq!(quiet.speech, None);
    }

    #[test]
    fn wav_errors() {
        assert_eq!(analyze_wav(b"nope"), Err(WavError::NotWav));
        let mut stereo = wav(&[0; 100], 16_000);
        stereo[22] = 2;
        assert_eq!(analyze_wav(&stereo), Err(WavError::Unsupported("not mono")));
        assert_eq!(
            analyze_wav(&wav(&[0; 100], 16_000)[..30]),
            Err(WavError::Truncated)
        );
    }

    #[test]
    fn wav_chunks_other_than_fmt_and_data_are_skipped() {
        let plain = wav(&tone(200, 3_000), 16_000);
        let mut with_list = plain[..12].to_vec();
        with_list.extend_from_slice(b"LIST\x03\0\0\0abc\0");
        with_list.extend_from_slice(&plain[12..]);
        assert_eq!(
            analyze_wav(&with_list).unwrap(),
            analyze_wav(&plain).unwrap()
        );
    }

    #[test]
    fn measures_from_speech_start_and_end() {
        let events = vec![
            transcript(900, 1, 0, "we tried", false),
            transcript(1_500, 1, 1, "we tried two vendors", false),
            transcript(4_300, 1, 2, "We tried two vendors.", true),
            transcript(5_000, 2, 0, "so how", false),
            transcript(8_900, 2, 1, "So how would you handle it?", true),
        ];
        let m = measure(&events, 120, 8_800).unwrap();
        assert_eq!(m.first_partial_ms, 780);
        assert_eq!(m.final_ms, Some(100));
        assert_eq!(
            m.hypothesis,
            "We tried two vendors. So how would you handle it?"
        );
    }

    #[test]
    fn revoked_segments_and_stale_revisions_do_not_count() {
        let events = vec![
            transcript(500, 1, 0, "", false),
            transcript(900, 1, 1, "hello", false),
            transcript(1_000, 1, 2, "hello there", true),
            transcript(1_100, 1, 1, "late old revision", false),
            transcript(1_200, 2, 0, "ghost", false),
            transcript(1_300, 2, 1, "", false),
        ];
        let m = measure(&events, 0, 2_000).unwrap();
        assert_eq!(m.first_partial_ms, 900);
        assert_eq!(m.final_ms, Some(0));
        assert_eq!(m.hypothesis, "hello there");
    }

    #[test]
    fn no_text_means_no_measurement() {
        assert!(measure(&[], 0, 1).is_none());
        assert!(measure(&[transcript(10, 1, 0, "  ", false)], 0, 1).is_none());
    }

    #[test]
    fn a_run_without_a_final_leaves_final_time_empty() {
        let m = measure(&[transcript(700, 1, 0, "hi", false)], 0, 1_000).unwrap();
        assert_eq!(m.final_ms, None);
    }

    #[test]
    fn scores_a_recorded_real_run() {
        let events: Vec<Timed> = REAL_RUN
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let en = bundled_clips()
            .into_iter()
            .find(|c| c.locale == "en-US")
            .unwrap();
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio");
        let audio = analyze_wav(&std::fs::read(dir.join(&en.file)).unwrap()).unwrap();
        let (start, end) = audio.speech.unwrap();
        let view = score("en-US", &en.text, &events, start, end);
        assert_eq!(view.outcome, ClipOutcome::Measured);
        assert!(view.first_partial_ms.unwrap() < 3_000);
        assert!(view.final_ms.unwrap() < 3_000);
        let wer = view.error_rate_pct.unwrap();
        assert!((0.0..60.0).contains(&wer), "{wer}");
    }

    struct Scripted(std::sync::Mutex<Vec<SidecarOutput>>);

    impl SidecarFactory for Scripted {
        fn spawn(&self) -> Result<SidecarLink, SidecarError> {
            Err(SidecarError::StdinClosed)
        }

        fn spawn_file_mode(&self, _wav: &Path, _locale: &str) -> Result<SidecarLink, SidecarError> {
            let (commands, _rx) = mpsc::unbounded_channel();
            let (tx, outputs) = mpsc::channel(64);
            for out in self.0.lock().unwrap().drain(..) {
                tx.try_send(out).unwrap();
            }
            Ok(SidecarLink {
                commands,
                outputs,
                pcm: None,
            })
        }
    }

    fn ready(locales: &[&str]) -> SidecarOutput {
        SidecarOutput::Event(SidecarEvent::Ready {
            protocol_version: 1,
            sidecar_version: "test".into(),
            installed_locales: locales.iter().map(|l| (*l).to_string()).collect(),
        })
    }

    fn clip_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio")
    }

    async fn run(outputs: Vec<SidecarOutput>, locale: &str) -> ClipResultView {
        let factory = Scripted(std::sync::Mutex::new(outputs));
        let clips = bundled_clips();
        let clip = clips.iter().find(|c| c.locale == locale);
        run_clip(&factory, Some(&clip_dir()), locale, clip).await
    }

    #[tokio::test]
    async fn a_missing_locale_is_reported_without_playing_the_clip() {
        let r = run(vec![ready(&["en-US"])], "pt-BR").await;
        assert_eq!(r.outcome, ClipOutcome::LocaleNotInstalled);
        assert!(r.first_partial_ms.is_none());
    }

    #[tokio::test]
    async fn an_assets_error_means_the_locale_is_not_available() {
        let r = run(
            vec![
                ready(&["en-US"]),
                SidecarOutput::Event(SidecarEvent::Error {
                    code: SidecarErrorCode::AssetsUnavailable,
                    message: "x".into(),
                    fatal: true,
                }),
            ],
            "en-US",
        )
        .await;
        assert_eq!(r.outcome, ClipOutcome::LocaleNotInstalled);
    }

    #[tokio::test]
    async fn a_fatal_engine_error_is_an_engine_failure_with_its_message() {
        let r = run(
            vec![
                ready(&["en-US"]),
                SidecarOutput::Event(SidecarEvent::Error {
                    code: SidecarErrorCode::Analyzer,
                    message: "analyzer failed".into(),
                    fatal: true,
                }),
            ],
            "en-US",
        )
        .await;
        assert_eq!(r.outcome, ClipOutcome::EngineFailed);
        assert_eq!(r.message.as_deref(), Some("analyzer failed"));
    }

    #[tokio::test]
    async fn a_helper_that_exits_without_text_is_a_failure() {
        let r = run(
            vec![
                ready(&["en-US"]),
                SidecarOutput::Event(SidecarEvent::Stopped {}),
            ],
            "en-US",
        )
        .await;
        assert_eq!(r.outcome, ClipOutcome::EngineFailed);
        let r = run(
            vec![
                ready(&["en-US"]),
                SidecarOutput::Exited {
                    code: Some(133),
                    success: false,
                },
            ],
            "en-US",
        )
        .await;
        assert_eq!(
            r.message.as_deref(),
            Some("The audio helper stopped unexpectedly")
        );
    }

    #[tokio::test]
    async fn a_helper_that_does_not_start_is_a_failure() {
        let r = run_clip(
            &Scripted(std::sync::Mutex::new(Vec::new())),
            None,
            "en-US",
            bundled_clips().first(),
        )
        .await;
        assert_eq!(r.outcome, ClipOutcome::EngineFailed);
    }

    #[tokio::test]
    async fn a_locale_without_a_clip_has_no_sample() {
        let r = run_clip(
            &Scripted(std::sync::Mutex::new(Vec::new())),
            Some(&clip_dir()),
            "fr-FR",
            None,
        )
        .await;
        assert_eq!(r.outcome, ClipOutcome::NoSample);
    }

    #[tokio::test]
    async fn a_scripted_run_produces_a_measured_result() {
        let en = bundled_clips()
            .into_iter()
            .find(|c| c.locale == "en-US")
            .unwrap();
        let text = |at, rev, t: &str, f| {
            let Timed { event, .. } = transcript(at, 1, rev, t, f);
            SidecarOutput::Event(event)
        };
        let r = run(
            vec![
                ready(&["en-US"]),
                SidecarOutput::Event(SidecarEvent::Started {
                    t0_host_ns: 1,
                    emitted_at_ms: 0,
                }),
                text(0, 0, "we tried", false),
                text(0, 1, &en.text, true),
                SidecarOutput::Event(SidecarEvent::Stopped {}),
            ],
            "en-US",
        )
        .await;
        assert_eq!(r.outcome, ClipOutcome::Measured);
        assert_eq!(r.first_partial_ms, Some(0));
        assert_eq!(r.error_rate_pct, Some(0.0));
    }

    /// Needs the built sidecar and the installed speech models. Run by hand to calibrate:
    /// `SOTTO_SIDECAR=<path> cargo test -p sotto-core real_sidecar -- --ignored --nocapture`.
    /// With `SOTTO_CHECK_FIXTURE=<file>` it also records the en-US run for the unit test above.
    #[tokio::test]
    #[ignore = "plays the bundled clips through the real on-device engine"]
    async fn real_sidecar_run() {
        let path = PathBuf::from(std::env::var("SOTTO_SIDECAR").unwrap());
        let factory = crate::app::ProcessSidecarFactory::new(path);
        for clip in bundled_clips() {
            let wav = clip_dir().join(&clip.file);
            let audio = analyze_wav(&std::fs::read(&wav).unwrap()).unwrap();
            let Capture::Events(events) =
                capture(&factory, &wav, &clip.locale, audio.duration_ms).await
            else {
                panic!("{} did not run", clip.locale);
            };
            let (start, end) = audio.speech.unwrap();
            let view = score(&clip.locale, &clip.text, &events, start, end);
            eprintln!("{view:?}");
            if clip.locale == "en-US"
                && let Ok(out) = std::env::var("SOTTO_CHECK_FIXTURE")
            {
                let lines: Vec<String> = events
                    .iter()
                    .map(|e| serde_json::to_string(e).unwrap())
                    .collect();
                std::fs::write(out, lines.join("\n") + "\n").unwrap();
            }
        }
    }
}
