//! The seam between the shell and the application core. `RealCore` (in `real_core.rs`) wraps
//! `sotto-core`; [`DemoCore`], behind the `demo` feature, replays a scripted session so the UI
//! is alive without keys or a sidecar.

use std::sync::Arc;

use sotto_protocol::ui::{AppSnapshot, Intent};

pub type SnapshotSink = Arc<dyn Fn(AppSnapshot) + Send + Sync>;

pub trait CoreHandle: Send + Sync {
    fn dispatch(&self, intent: Intent) -> Result<(), String>;
    /// Registers the callback that receives a snapshot on every state change.
    fn subscribe(&self, sink: SnapshotSink) -> Result<(), String>;
    /// Gives files the user picked in the native panel to a mode.
    fn add_mode_files(
        &self,
        _mode_id: uuid::Uuid,
        _paths: Vec<std::path::PathBuf>,
    ) -> Result<(), String> {
        Ok(())
    }
    /// Ends the session and waits until pending writes are on disk. Called once, on exit.
    fn shutdown(&self) {}
    /// A `sotto://` link the OS delivered. The core decides whether it is a callback it asked for.
    fn deep_link(&self, _url: &str) {}
}

#[cfg(any(feature = "demo", test))]
pub use self::demo::DemoCore;

#[cfg(any(feature = "demo", test))]
mod demo {
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use sotto_protocol::onboarding::OnboardingView;
    use sotto_protocol::sidecar::Speaker;
    use sotto_protocol::ui::{
        AccountView, AnswerOrigin, AnswerState, AnswerView, AppSnapshot, CalendarView, DebugStats,
        EnginePreference, EngineView, Intent, LevelsView, Phase, ProviderId, ProviderView,
        SessionView, SettingsView, TranscriptLine,
    };
    use uuid::Uuid;

    use super::{CoreHandle, SnapshotSink};

    const CYCLE_MS: u64 = 16_000;
    const ELAPSED_AT_START_MS: u64 = 768_000;
    const QUESTION_END_MS: u64 = 4_200;
    const STREAM_START_DELAY_MS: u64 = 1_200;
    const LATENCY_MS: u32 = 640;
    const WORD_MS: u64 = 60;
    const TICK: Duration = Duration::from_millis(50);

    const QUESTION: &str =
        "So how would you handle the migration if their API starts rate limiting us?";
    const ANSWER: &str = "Queue the writes and drain them at the vendor's limit. Batch orders in groups of 500, retry with exponential backoff, and keep a dead-letter queue for anything that fails three times. Monday spikes then become a longer drain, not dropped orders.";
    const MODEL: &str = "claude-haiku-5-5";

    #[derive(Debug, Clone)]
    struct Control {
        listening: bool,
        invisible: bool,
        auto_answer: bool,
        /// Manual ask: (time of the ask in cycle milliseconds, question text).
        manual: Option<(u64, String)>,
        cancelled: bool,
        asks: u32,
    }

    impl Default for Control {
        fn default() -> Self {
            Self {
                listening: true,
                invisible: true,
                auto_answer: true,
                manual: None,
                cancelled: false,
                asks: 0,
            }
        }
    }

    struct Shared {
        control: Mutex<Control>,
        sink: Mutex<Option<SnapshotSink>>,
    }

    pub struct DemoCore {
        shared: Arc<Shared>,
    }

    impl std::fmt::Debug for DemoCore {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("DemoCore").finish_non_exhaustive()
        }
    }

    impl Default for DemoCore {
        fn default() -> Self {
            Self::new()
        }
    }

    fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn unix_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(0))
            .unwrap_or(0)
    }

    impl DemoCore {
        pub fn new() -> Self {
            Self {
                shared: Arc::new(Shared {
                    control: Mutex::new(Control::default()),
                    sink: Mutex::new(None),
                }),
            }
        }
    }

    impl CoreHandle for DemoCore {
        fn dispatch(&self, intent: Intent) -> Result<(), String> {
            intent.validate().map_err(|e| format!("{e:?}"))?;
            let mut c = lock(&self.shared.control);
            match intent {
                Intent::StartSession { .. } => c.listening = true,
                Intent::EndSession {} => c.listening = false,
                Intent::Ask { text, .. } => {
                    c.asks += 1;
                    c.cancelled = false;
                    c.manual = Some((u64::MAX, text));
                }
                Intent::CancelAnswer {} => c.cancelled = true,
                Intent::SetInvisible { invisible } => c.invisible = invisible,
                Intent::SetAutoAnswer { enabled } => c.auto_answer = enabled,
                Intent::OpenSettings {}
                | Intent::SetApiKey { .. }
                | Intent::RemoveApiKey { .. }
                | Intent::SetModels { .. }
                | Intent::SetSpendCap { .. }
                | Intent::SetLocale { .. }
                | Intent::SetEnginePreference { .. }
                | Intent::CalendarSetClient { .. }
                | Intent::CalendarRemoveClient {}
                | Intent::CalendarConnect {}
                | Intent::CalendarDisconnect {}
                | Intent::CalendarRefresh {}
                | Intent::CalendarDismissPrompt { .. }
                | Intent::StartCalendarSession { .. }
                | Intent::AcknowledgeConsent {}
                | Intent::CheckPermissions {}
                | Intent::RequestPermission { .. }
                | Intent::OpenPrivacySettings { .. }
                | Intent::RunTranscriptionCheck {}
                | Intent::TestModel {}
                | Intent::CompleteOnboarding {}
                | Intent::ReopenOnboarding {}
                | Intent::CreateMode { .. }
                | Intent::UpdateMode { .. }
                | Intent::DeleteMode { .. }
                | Intent::SelectMode { .. }
                | Intent::PickModeFiles { .. }
                | Intent::RemoveModeFile { .. }
                | Intent::RetryIndexing { .. }
                | Intent::SignIn {}
                | Intent::SignOut {}
                | Intent::SyncNow {} => {}
            }
            Ok(())
        }

        fn subscribe(&self, sink: SnapshotSink) -> Result<(), String> {
            *lock(&self.shared.sink) = Some(sink);
            let shared = Arc::clone(&self.shared);
            thread::Builder::new()
                .name("demo-core".into())
                .spawn(move || run_script(&shared))
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
    }

    fn run_script(shared: &Shared) {
        let mut cycle_start = unix_ms();
        let mut last: Option<AppSnapshot> = None;
        loop {
            let now = unix_ms();
            if now.saturating_sub(cycle_start) >= CYCLE_MS {
                cycle_start = now;
                lock(&shared.control).cancelled = false;
            }
            let t = now.saturating_sub(cycle_start);
            let snapshot = {
                let mut c = lock(&shared.control);
                if let Some((at, _)) = c.manual.as_mut()
                    && *at == u64::MAX
                {
                    *at = t;
                }
                script_snapshot(t, cycle_start, &c)
            };
            if last.as_ref() != Some(&snapshot) {
                if let Some(sink) = lock(&shared.sink).clone() {
                    sink(snapshot.clone());
                }
                last = Some(snapshot);
            }
            thread::sleep(TICK);
        }
    }

    fn line(
        id: &str,
        speaker: Speaker,
        start: i64,
        end: i64,
        text: &str,
        is_final: bool,
    ) -> TranscriptLine {
        TranscriptLine {
            id: id.into(),
            speaker,
            start_ms: ELAPSED_AT_START_MS.saturating_add_signed(start),
            end_ms: ELAPSED_AT_START_MS.saturating_add_signed(end),
            text: text.into(),
            is_final,
            is_question: false,
        }
    }

    fn prefix_words(text: &str, count: usize) -> String {
        text.split_whitespace()
            .take(count)
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn answer_at(
        dt: u64,
        origin: AnswerOrigin,
        question: &str,
        id: Uuid,
        line_id: Option<String>,
    ) -> AnswerView {
        let words = ANSWER.split_whitespace().count();
        let streamed = if dt < STREAM_START_DELAY_MS {
            0
        } else {
            usize::try_from((dt - STREAM_START_DELAY_MS) / WORD_MS + 1).unwrap_or(words)
        };
        let (state, text, latency_ms) = if dt < STREAM_START_DELAY_MS {
            (AnswerState::Preparing, String::new(), None)
        } else if streamed < words {
            (
                AnswerState::Streaming,
                prefix_words(ANSWER, streamed),
                Some(LATENCY_MS),
            )
        } else {
            (AnswerState::Done, ANSWER.to_string(), Some(LATENCY_MS))
        };
        AnswerView {
            id,
            origin,
            question: question.into(),
            text,
            state,
            model: MODEL.into(),
            latency_ms,
            question_line_id: line_id,
            error: None,
        }
    }

    /// Them speaking the question moves the bars; the rest of the cycle is quiet.
    fn demo_levels(t: u64) -> LevelsView {
        let speaking = (1_000..QUESTION_END_MS).contains(&t);
        LevelsView {
            me: 0,
            them: if speaking {
                u8::try_from(2 + (t / 150) % 4).unwrap_or(2)
            } else {
                0
            },
        }
    }

    /// Builds the snapshot for `t` milliseconds into a cycle. Pure, so tests can pin every state.
    fn script_snapshot(t: u64, cycle_start_unix_ms: u64, c: &Control) -> AppSnapshot {
        let session = SessionView {
            id: Uuid::from_u128(0x5077),
            title: "Roadmap sync \u{b7} Acme".into(),
            mode_name: Some("Sales discovery".into()),
            started_at_unix_ms: i64::try_from(
                cycle_start_unix_ms.saturating_sub(ELAPSED_AT_START_MS),
            )
            .unwrap_or(0),
        };
        if !c.listening {
            return AppSnapshot {
                phase: Phase::Idle,
                session: None,
                transcript: vec![],
                answer: None,
                invisible: c.invisible,
                auto_answer: c.auto_answer,
                engine: EngineView::Local,
                levels: LevelsView::default(),
                debug: DebugStats::default(),
                settings: settings_view(),
                calendar: CalendarView::not_configured(),
                account: AccountView::signed_out(),
                notice: None,
                onboarding: OnboardingView::default(),
                modes: vec![],
                selected_mode_id: None,
            };
        }

        let mut transcript = vec![
            line(
                "them-1",
                Speaker::Them,
                -52_000,
                -47_000,
                "We tried two vendors last year and both broke under load.",
                true,
            ),
            line(
                "me-2",
                Speaker::Me,
                -44_000,
                -41_000,
                "Got it. What volume are we talking about?",
                true,
            ),
            line(
                "them-3",
                Speaker::Them,
                -35_000,
                -29_000,
                "Around forty thousand orders a day, with spikes on Mondays.",
                true,
            ),
        ];
        let q_words = QUESTION.split_whitespace().count();
        if t >= 1_000 {
            let spoken = if t >= QUESTION_END_MS {
                q_words
            } else {
                let progressed =
                    usize::try_from((t - 1_000) * q_words as u64 / 3_200).unwrap_or(q_words);
                progressed.clamp(1, q_words)
            };
            let done = t >= QUESTION_END_MS;
            let mut l = line(
                "them-4",
                Speaker::Them,
                1_000,
                i64::try_from(t.min(QUESTION_END_MS)).unwrap_or(0),
                &prefix_words(QUESTION, spoken),
                done,
            );
            l.is_question = done;
            transcript.push(l);
        }

        let auto = (c.auto_answer && t >= QUESTION_END_MS).then(|| {
            answer_at(
                t - QUESTION_END_MS,
                AnswerOrigin::Auto,
                QUESTION,
                Uuid::from_u128(0xA0),
                Some("them-4".into()),
            )
        });
        let manual = c
            .manual
            .as_ref()
            .filter(|(at, _)| *at != u64::MAX)
            .map(|(at, q)| {
                answer_at(
                    t.saturating_sub(*at),
                    AnswerOrigin::Manual,
                    q,
                    Uuid::from_u128(0xB0 + u128::from(c.asks)),
                    None,
                )
            });
        let answer = if c.cancelled { None } else { manual.or(auto) };
        let last_latency_ms = answer.as_ref().and_then(|a| a.latency_ms);

        AppSnapshot {
            phase: Phase::Listening,
            session: Some(session),
            transcript,
            answer,
            invisible: c.invisible,
            auto_answer: c.auto_answer,
            engine: EngineView::Local,
            levels: demo_levels(t),
            debug: DebugStats {
                speculations_started: 1,
                speculations_revealed: 1,
                spend_micro_usd: 4_200,
                last_latency_ms,
                p50_latency_ms: Some(LATENCY_MS),
                p95_latency_ms: Some(910),
                ..DebugStats::default()
            },
            settings: settings_view(),
            calendar: CalendarView::not_configured(),
            account: AccountView::signed_out(),
            notice: None,
            onboarding: OnboardingView::default(),
            modes: vec![],
            selected_mode_id: None,
        }
    }

    fn settings_view() -> SettingsView {
        SettingsView {
            providers: vec![
                ProviderView {
                    provider: ProviderId::Anthropic,
                    configured: true,
                    masked_key: Some("sk-ant\u{2026}4f2a".into()),
                    models: vec!["claude-haiku-5-5".into(), "claude-sonnet-5-5".into()],
                },
                ProviderView {
                    provider: ProviderId::Openai,
                    configured: false,
                    masked_key: None,
                    models: vec!["gpt-5".into(), "gpt-5-mini".into()],
                },
                ProviderView {
                    provider: ProviderId::Ollama,
                    configured: true,
                    masked_key: None,
                    models: vec!["ollama:qwen3:8b".into()],
                },
            ],
            auto_answer_model: MODEL.into(),
            manual_model: "claude-sonnet-5-5".into(),
            spend_cap_micro_usd: 500_000,
            ollama_running: true,
            locale: "en-US".into(),
            available_locales: vec!["en-US".into()],
            engine_preference: EnginePreference::Local,
            cloud_transcription_available: false,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn at(t: u64) -> AppSnapshot {
            script_snapshot(t, 0, &Control::default())
        }

        #[test]
        fn answer_goes_from_preparing_to_done() {
            assert!(at(3_000).answer.is_none());
            let preparing = at(QUESTION_END_MS + 500).answer;
            assert_eq!(
                preparing.as_ref().map(|a| a.state),
                Some(AnswerState::Preparing)
            );
            let streaming = at(QUESTION_END_MS + STREAM_START_DELAY_MS + 600).answer;
            assert_eq!(
                streaming.as_ref().map(|a| a.state),
                Some(AnswerState::Streaming)
            );
            let done = at(CYCLE_MS - 1).answer;
            assert_eq!(done.as_ref().map(|a| a.text.as_str()), Some(ANSWER));
        }

        #[test]
        fn question_line_is_marked_once_the_turn_ends() {
            let marked = at(QUESTION_END_MS)
                .transcript
                .iter()
                .filter(|l| l.is_question)
                .count();
            assert_eq!(marked, 1);
            assert!(at(2_000).transcript.iter().all(|l| !l.is_question));
        }

        #[test]
        fn dispatch_rejects_empty_ask() {
            let core = DemoCore::new();
            let empty = Intent::Ask {
                text: " ".into(),
                with_screenshot: false,
            };
            assert!(core.dispatch(empty).is_err());
        }

        #[test]
        fn invisible_follows_intent() {
            let core = DemoCore::new();
            assert!(
                core.dispatch(Intent::SetInvisible { invisible: false })
                    .is_ok()
            );
            assert!(!lock(&core.shared.control).invisible);
        }
    }
}
