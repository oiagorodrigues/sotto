use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use futures::StreamExt;
use sotto_detect::GuardConfig;
use sotto_protocol::Speaker;
use sotto_protocol::sidecar::{PermissionKind, SidecarCommand, SidecarErrorCode, SidecarEvent};
use sotto_protocol::ui::{
    AnswerOrigin, AnswerState, ApiKey, AppSnapshot, EngineView, Intent, IntentError, LevelsView,
    Phase, ProviderId, ProviderView,
};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::streams::{JobKind, StreamMsg, StreamMsgKind};
use super::*;
use crate::pricing::PriceTable;
use crate::providers::{
    CompletionRequest, Effort, EventStream, LlmProvider, ProviderError, Result as PResult,
    StreamEvent, Thinking, Usage,
};
use crate::secrets::{Account, MemoryStore, SecretStore};
use crate::sidecar::SidecarOutput;
use crate::storage::Db;

mod cloud;

const KEY: [u8; 32] = [9; 32];
const WAIT_LIMIT: Duration = Duration::from_secs(120);

const PRICES: &str = r#"
as_of = "test"
[models."fake-answer"]
input = 1.0
output = 10.0
cache_read = 0.0
cache_write = 0.0
[models."fake-classifier"]
input = 1.0
output = 1.0
cache_read = 0.0
cache_write = 0.0
[models."fake-manual"]
input = 1.0
output = 1.0
cache_read = 0.0
cache_write = 0.0
"#;

const QUESTION: &str = "como voce lidaria com a migracao de banco";

#[derive(Debug, Clone)]
enum Step {
    Wait(u64),
    Text(&'static str),
    Usage(u64, u64),
    Fail(ProviderError),
}

/// Plays one script per call (the last one repeats). Ignores the cancel token on purpose: the
/// core must stop on its own.
struct FakeProvider {
    scripts: Vec<Vec<Step>>,
    calls: Mutex<Vec<CompletionRequest>>,
}

impl FakeProvider {
    fn new(scripts: Vec<Vec<Step>>) -> Arc<Self> {
        Arc::new(Self {
            scripts,
            calls: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<CompletionRequest> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl LlmProvider for FakeProvider {
    async fn stream(
        &self,
        req: CompletionRequest,
        _cancel: CancellationToken,
    ) -> PResult<EventStream> {
        let steps = {
            let mut calls = self.calls.lock().unwrap();
            let i = calls.len().min(self.scripts.len() - 1);
            calls.push(req);
            self.scripts[i].clone()
        };
        Ok(futures::stream::iter(steps)
            .then(|step| async move {
                match step {
                    Step::Wait(ms) => {
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                        None
                    }
                    Step::Text(t) => Some(Ok(StreamEvent::TextDelta(t.into()))),
                    Step::Usage(i, o) => Some(Ok(StreamEvent::Usage {
                        input_tokens: i,
                        output_tokens: o,
                        cache_read_input_tokens: 0,
                        cache_creation_input_tokens: 0,
                    })),
                    Step::Fail(e) => Some(Err(e)),
                }
            })
            .filter_map(|x| async move { x })
            .boxed())
    }
}

struct FakeProviders(HashMap<String, Arc<FakeProvider>>);

impl ProviderFactory for FakeProviders {
    /// Reads the Anthropic key like the real factory does, so Keychain reads can be counted.
    fn provider(
        &self,
        model: &str,
        secrets: &dyn SecretStore,
    ) -> Result<Arc<dyn LlmProvider>, ProviderSetupError> {
        let _ = secrets.get(Account::ProviderAnthropic);
        self.0
            .get(model)
            .map(|p| Arc::clone(p) as Arc<dyn LlmProvider>)
            .ok_or_else(|| ProviderSetupError::UnknownModel(model.into()))
    }
}

/// Answers `/api/tags` from a script: one reply per call, the last one repeats.
struct FakeLocal {
    replies: Vec<(Duration, Option<Vec<String>>)>,
    calls: Mutex<usize>,
}

impl FakeLocal {
    fn new(replies: Vec<(Duration, Option<Vec<String>>)>) -> Arc<Self> {
        Arc::new(Self {
            replies,
            calls: Mutex::new(0),
        })
    }

    fn down() -> Arc<Self> {
        Self::new(vec![(Duration::ZERO, None)])
    }

    fn calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

#[async_trait]
impl LocalModels for FakeLocal {
    async fn ollama_models(&self) -> Option<Vec<String>> {
        let (delay, reply) = {
            let mut calls = self.calls.lock().unwrap();
            let i = (*calls).min(self.replies.len() - 1);
            *calls += 1;
            self.replies[i].clone()
        };
        tokio::time::sleep(delay).await;
        reply
    }
}

/// Counts reads that reach the backing store.
struct CountingStore {
    inner: Arc<MemoryStore>,
    reads: Arc<AtomicUsize>,
}

impl SecretStore for CountingStore {
    fn get(&self, a: Account) -> Result<Option<String>, crate::secrets::SecretsError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.get(a)
    }
    fn set(&self, a: Account, v: &str) -> Result<(), crate::secrets::SecretsError> {
        self.inner.set(a, v)
    }
    fn delete(&self, a: Account) -> Result<(), crate::secrets::SecretsError> {
        self.inner.delete(a)
    }
}

/// A Keychain that refuses every write and delete, quoting the value it was given the way a
/// careless backend might.
struct FailingStore;

impl SecretStore for FailingStore {
    fn get(
        &self,
        _: crate::secrets::Account,
    ) -> Result<Option<String>, crate::secrets::SecretsError> {
        Ok(None)
    }
    fn set(
        &self,
        _: crate::secrets::Account,
        value: &str,
    ) -> Result<(), crate::secrets::SecretsError> {
        Err(crate::secrets::SecretsError::Keychain(format!(
            "access denied for {value}"
        )))
    }
    fn delete(&self, _: crate::secrets::Account) -> Result<(), crate::secrets::SecretsError> {
        Err(crate::secrets::SecretsError::Keychain(
            "access denied".into(),
        ))
    }
}

struct FakeSidecar {
    commands: mpsc::UnboundedReceiver<SidecarCommand>,
    events: mpsc::Sender<SidecarOutput>,
    /// The fd 3 pipe of a raw-engine session.
    pcm: mpsc::Sender<crate::cloud_stt::PcmFrame>,
}

impl FakeSidecar {
    async fn send(&self, event: SidecarEvent) {
        self.events.send(SidecarOutput::Event(event)).await.unwrap();
    }

    async fn line(&self, speaker: Speaker, seg: u64, rev: u32, start: u64, end: u64, text: &str) {
        self.send(SidecarEvent::Transcript {
            speaker,
            segment_id: seg,
            revision: rev,
            start_ms: start,
            end_ms: end,
            text: text.into(),
            is_final: false,
        })
        .await;
    }

    async fn them(&self, seg: u64, rev: u32, start: u64, end: u64, text: &str) {
        self.line(Speaker::Them, seg, rev, start, end, text).await;
    }

    async fn ready(&self) {
        self.send(SidecarEvent::Ready {
            protocol_version: 1,
            sidecar_version: "test".into(),
            installed_locales: INSTALLED.iter().map(|l| (*l).to_string()).collect(),
        })
        .await;
    }

    async fn command(&mut self) -> SidecarCommand {
        tokio::time::timeout(WAIT_LIMIT, self.commands.recv())
            .await
            .expect("no sidecar command")
            .expect("command channel closed")
    }

    fn drain(&mut self) -> Vec<SidecarCommand> {
        let mut out = Vec::new();
        while let Ok(c) = self.commands.try_recv() {
            out.push(c);
        }
        out
    }
}

struct FakeSidecars(mpsc::UnboundedSender<FakeSidecar>);

impl SidecarFactory for FakeSidecars {
    fn spawn(&self) -> Result<SidecarLink, crate::sidecar::SidecarError> {
        let (commands, commands_rx) = mpsc::unbounded_channel();
        let (events, outputs) = mpsc::channel(64);
        let (pcm_tx, pcm_rx) = mpsc::channel(512);
        let _ = self.0.send(FakeSidecar {
            commands: commands_rx,
            events,
            pcm: pcm_tx,
        });
        Ok(SidecarLink {
            commands,
            outputs,
            pcm: Some(pcm_rx),
        })
    }
}

struct Opts {
    answer: Vec<Vec<Step>>,
    classifier: Vec<Vec<Step>>,
    manual: Vec<Vec<Step>>,
    guards: GuardConfig,
    transcript_lines: usize,
    real_providers: bool,
    process_sidecar: Option<ProcessSidecarFactory>,
    local: Option<Arc<FakeLocal>>,
    failing_keychain: bool,
    auto_answer_model: &'static str,
    system_locale: Option<&'static str>,
    locale: Option<&'static str>,
    /// Written to `<data dir>/pricing.toml` before start.
    pricing_override: Option<&'static str>,
    seed: Option<fn(&Db)>,
    /// Replaces the fake sidecar factory.
    sidecar_override: Option<Arc<dyn SidecarFactory>>,
    tweak_settings: Option<fn(&mut Settings)>,
    seed_mut: Option<fn(&mut Db)>,
    embedder: Option<Arc<dyn sotto_rag::Embedder>>,
    account: Option<crate::account::AccountConfig>,
    refresh_token: Option<&'static str>,
    /// Endpoint of a fake cloud transcription server.
    cloud_url: Option<String>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            answer: vec![vec![
                Step::Wait(300),
                Step::Text("Eu "),
                Step::Wait(200),
                Step::Text("faria em etapas."),
                Step::Usage(1_000, 20),
            ]],
            classifier: vec![vec![
                Step::Wait(200),
                Step::Text("yes"),
                Step::Usage(100, 1),
            ]],
            manual: vec![vec![
                Step::Wait(100),
                Step::Text("Manual "),
                Step::Text("answer."),
                Step::Usage(10, 2),
            ]],
            guards: GuardConfig::default(),
            transcript_lines: 200,
            real_providers: false,
            process_sidecar: None,
            local: None,
            failing_keychain: false,
            auto_answer_model: "fake-answer",
            system_locale: Some("en-US"),
            locale: Some("pt-BR"),
            pricing_override: None,
            seed: None,
            sidecar_override: None,
            tweak_settings: None,
            seed_mut: None,
            embedder: None,
            account: None,
            refresh_token: None,
            cloud_url: None,
        }
    }
}

const INSTALLED: [&str; 2] = ["en-US", "pt-BR"];

struct Harness {
    app: AppHandle,
    snaps: watch::Receiver<AppSnapshot>,
    sidecars: mpsc::UnboundedReceiver<FakeSidecar>,
    answer: Arc<FakeProvider>,
    classifier: Arc<FakeProvider>,
    manual: Arc<FakeProvider>,
    alt: Arc<FakeProvider>,
    secrets: Arc<MemoryStore>,
    secret_reads: Arc<AtomicUsize>,
    origin: Instant,
    _dir: tempfile::TempDir,
    db_path: PathBuf,
}

fn harness(opts: Opts) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("sotto.db");
    let mut db = Db::open(&db_path, KEY).unwrap();
    if let Some(seed) = opts.seed {
        seed(&db);
    }
    if let Some(seed) = opts.seed_mut {
        seed(&mut db);
    }
    if let Some(text) = opts.pricing_override {
        std::fs::write(dir.path().join(crate::pricing::OVERRIDE_FILE), text).unwrap();
    }
    let answer = FakeProvider::new(opts.answer);
    let classifier = FakeProvider::new(opts.classifier);
    let manual = FakeProvider::new(opts.manual);
    let alt = FakeProvider::new(vec![vec![Step::Text("Alt answer."), Step::Usage(10, 2)]]);
    let memory = Arc::new(MemoryStore::new());
    if let Some(token) = opts.refresh_token {
        memory.set(Account::AuthRefresh, token).unwrap();
    }
    let local = opts.local.unwrap_or_else(FakeLocal::down);
    let (sidecar_tx, sidecars) = mpsc::unbounded_channel();
    let mut settings = Settings {
        locale: opts.locale.map(str::to_string),
        system_locale: opts.system_locale.map(str::to_string),
        auto_answer_model: opts.auto_answer_model.into(),
        classifier_model: "fake-classifier".into(),
        manual_model: "fake-manual".into(),
        guards: opts.guards,
        transcript_lines: opts.transcript_lines,
        cloud_stt_url: opts.cloud_url,
        ..Settings::default()
    };
    let providers: Arc<dyn ProviderFactory> = if opts.real_providers {
        settings.auto_answer_model = "claude-haiku-5-5".into();
        settings.classifier_model = "claude-haiku-5-5".into();
        settings.manual_model = "claude-sonnet-5-5".into();
        Arc::new(DefaultProviderFactory::new())
    } else {
        Arc::new(FakeProviders(HashMap::from([
            ("fake-answer".to_string(), Arc::clone(&answer)),
            ("fake-classifier".to_string(), Arc::clone(&classifier)),
            ("fake-manual".to_string(), Arc::clone(&manual)),
            ("gpt-5".to_string(), Arc::clone(&alt)),
            ("gpt-5-mini".to_string(), Arc::clone(&classifier)),
        ])))
    };
    let mut prices = PriceTable::bundled().unwrap();
    prices.merge(PriceTable::parse(PRICES).unwrap());
    let secret_reads = Arc::new(AtomicUsize::new(0));
    if let Some(tweak) = opts.tweak_settings {
        tweak(&mut settings);
    }
    let app = App::start(AppConfig {
        sidecars: match (opts.process_sidecar, opts.sidecar_override) {
            (Some(real), _) => Arc::new(real),
            (None, Some(other)) => other,
            (None, None) => Arc::new(FakeSidecars(sidecar_tx)),
        },
        secrets: if opts.failing_keychain {
            Arc::new(FailingStore)
        } else {
            Arc::new(CountingStore {
                inner: Arc::clone(&memory),
                reads: Arc::clone(&secret_reads),
            })
        },
        db,
        prices,
        providers,
        local_models: Arc::clone(&local) as Arc<dyn LocalModels>,
        settings,
        data_dir: Some(dir.path().to_path_buf()),
        calendar: crate::calendar::CalendarConfig::disabled(),
        embedder: opts.embedder,
        account: opts
            .account
            .unwrap_or_else(crate::account::AccountConfig::disabled),
    })
    .unwrap();
    Harness {
        snaps: app.snapshots(),
        app,
        sidecars,
        answer,
        classifier,
        manual,
        alt,
        secrets: memory,
        secret_reads,
        origin: Instant::now(),
        _dir: dir,
        db_path,
    }
}

impl Harness {
    async fn at(&self, ms: u64) {
        tokio::time::sleep_until(self.origin + Duration::from_millis(ms)).await;
    }

    async fn wait(&mut self, pred: impl FnMut(&AppSnapshot) -> bool) -> AppSnapshot {
        tokio::time::timeout(WAIT_LIMIT, self.snaps.wait_for(pred))
            .await
            .expect("timed out waiting for a snapshot")
            .expect("core stopped")
            .clone()
    }

    async fn sidecar(&mut self) -> FakeSidecar {
        tokio::time::timeout(WAIT_LIMIT, self.sidecars.recv())
            .await
            .expect("sidecar not spawned")
            .expect("factory gone")
    }

    /// Starts a session at time 0 of the session clock.
    async fn start(&mut self) -> FakeSidecar {
        self.start_with_setup(0).await
    }

    /// Starts a session whose sidecar spent `setup_ms` between taking t0 and sending `Started`,
    /// so session time `setup_ms` is now.
    async fn start_with_setup(&mut self, setup_ms: u64) -> FakeSidecar {
        let sc = self.start_until_command("pt-BR").await;
        sc.send(SidecarEvent::Started {
            t0_host_ns: 1,
            emitted_at_ms: setup_ms,
        })
        .await;
        self.wait(|s| s.phase == Phase::Listening).await;
        sc
    }

    /// A new sidecar sends `Ready`; the core answers with `Start` in `locale`.
    async fn start_until_command(&mut self, locale: &str) -> FakeSidecar {
        self.app
            .dispatch(Intent::StartSession { mode_id: None })
            .unwrap();
        let mut sc = self.sidecar().await;
        sc.ready().await;
        match sc.command().await {
            SidecarCommand::Start {
                locale: sent,
                engine,
                speakers,
            } => {
                assert_eq!(sent, locale);
                assert_eq!(engine, sotto_protocol::sidecar::EngineKind::Local);
                assert_eq!(speakers, vec![Speaker::Me, Speaker::Them]);
            }
            other => panic!("expected start, got {other:?}"),
        }
        sc
    }
}

fn state(s: &AppSnapshot) -> Option<AnswerState> {
    s.answer.as_ref().map(|a| a.state)
}

/// A partial that is not yet a question at 500 ms, then a likely question at 1,000 ms.
async fn ask_question(h: &Harness, sc: &FakeSidecar) {
    h.at(500).await;
    sc.them(1, 0, 0, 500, "como voce").await;
    h.at(1_000).await;
    sc.them(1, 1, 0, 1_000, QUESTION).await;
}

/// A new turn (split by "me" speech) with another likely question at 4,000 ms.
async fn ask_second_question(h: &Harness, sc: &FakeSidecar) {
    h.at(1_800).await;
    sc.line(Speaker::Me, 1, 0, 1_600, 1_800, "boa pergunta")
        .await;
    h.at(4_000).await;
    sc.them(2, 0, 3_000, 4_000, "qual o prazo de entrega do projeto")
        .await;
}

#[tokio::test(start_paused = true)]
async fn full_auto_flow_reveals_at_turn_end_with_latency() {
    let mut h = harness(Opts::default());
    let mut sc = h.start().await;
    ask_question(&h, &sc).await;

    let s = h.wait(|s| state(s) == Some(AnswerState::Preparing)).await;
    let a = s.answer.clone().unwrap();
    assert_eq!(a.origin, AnswerOrigin::Auto);
    assert_eq!(a.text, "");
    assert_eq!(a.model, "fake-answer");
    assert_eq!(a.question_line_id.as_deref(), Some("them-1"));
    assert!(s.transcript[0].is_question);
    assert_eq!(s.debug.speculations_started, 1);

    // Tokens arrive at 1,300 ms but stay hidden until the turn ends at 1,400 ms.
    let s = h.wait(|s| state(s) == Some(AnswerState::Streaming)).await;
    let a = s.answer.clone().unwrap();
    assert_eq!(a.text, "Eu ");
    assert_eq!(a.latency_ms, Some(400));

    let s = h.wait(|s| state(s) == Some(AnswerState::Done)).await;
    let a = s.answer.clone().unwrap();
    assert_eq!(a.text, "Eu faria em etapas.");
    assert_eq!(a.question, QUESTION);
    assert_eq!(s.debug.speculations_revealed, 1);
    assert_eq!(s.debug.speculations_cancelled, 0);
    assert_eq!(s.debug.last_latency_ms, Some(400));
    assert_eq!(s.debug.p50_latency_ms, Some(400));
    assert_eq!(s.debug.p95_latency_ms, Some(400));
    // 1,000 in + 20 out on the answer model, 100 in + 1 out on the classifier.
    h.wait(|s| s.debug.spend_micro_usd == 1_000 + 200 + 100 + 1)
        .await;

    assert!(sc.drain().contains(&SidecarCommand::Finalize {
        speaker: Speaker::Them,
        through_ms: 1_000,
    }));
    let answer_calls = h.answer.calls();
    assert_eq!(answer_calls.len(), 1);
    assert!(answer_calls[0].user_text.contains(QUESTION));
    assert_eq!(answer_calls[0].effort, Some(Effort::Low));
    let classifier_calls = h.classifier.calls();
    assert_eq!(classifier_calls.len(), 1);
    assert_eq!(classifier_calls[0].max_tokens, 256);
    assert_eq!(classifier_calls[0].thinking, Thinking::Disabled);
    assert_eq!(answer_calls[0].thinking, Thinking::Adaptive);
    assert!(answer_calls[0].max_tokens >= 1_024);

    h.app.shutdown().await;
    let db = Db::open(&h.db_path, KEY).unwrap();
    let sessions = db.list_sessions(10).unwrap();
    assert_eq!(sessions.len(), 1);
    assert!(sessions[0].ended_at.is_some());
    let answers = db.list_answers(sessions[0].id).unwrap();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].id, a.id);
    assert_eq!(answers[0].state, AnswerState::Done);
    assert_eq!(answers[0].latency_ms, Some(400));
    assert_eq!(answers[0].text, "Eu faria em etapas.");
    assert_eq!(answers[0].usage.map(|u| u.output_tokens), Some(20));
    let segments = db.list_segments(sessions[0].id).unwrap();
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].revision, 1);
    assert_eq!(segments[0].text, QUESTION);
}

#[tokio::test(start_paused = true)]
async fn classifier_no_cancels_the_speculation() {
    let mut h = harness(Opts {
        classifier: vec![vec![Step::Wait(100), Step::Text("no")]],
        ..Opts::default()
    });
    let sc = h.start().await;
    ask_question(&h, &sc).await;
    h.wait(|s| state(s) == Some(AnswerState::Preparing)).await;
    let s = h.wait(|s| s.answer.is_none()).await;
    assert_eq!(s.debug.speculations_cancelled, 1);
    assert!(!s.transcript[0].is_question);
    h.at(3_000).await;
    let s = h.snaps.borrow().clone();
    assert!(s.answer.is_none());
    assert_eq!(s.debug.speculations_revealed, 0);
}

#[tokio::test(start_paused = true)]
async fn question_dropped_cancels_the_speculation() {
    let mut h = harness(Opts::default());
    let sc = h.start().await;
    ask_question(&h, &sc).await;
    h.wait(|s| state(s) == Some(AnswerState::Preparing)).await;
    sc.them(1, 2, 0, 1_000, "").await;
    let s = h.wait(|s| s.answer.is_none()).await;
    assert_eq!(s.debug.speculations_cancelled, 1);
    assert!(s.transcript.is_empty());
    h.at(3_000).await;
    assert!(h.snaps.borrow().answer.is_none());
}

#[tokio::test(start_paused = true)]
async fn meaning_change_restarts_the_speculation() {
    let mut h = harness(Opts {
        answer: vec![
            vec![Step::Wait(5_000), Step::Text("old")],
            vec![
                Step::Wait(100),
                Step::Text("new answer"),
                Step::Usage(10, 1),
            ],
        ],
        ..Opts::default()
    });
    let sc = h.start().await;
    ask_question(&h, &sc).await;
    h.wait(|s| state(s) == Some(AnswerState::Preparing)).await;
    h.at(1_200).await;
    sc.them(
        1,
        2,
        0,
        1_200,
        "qual o prazo para o lancamento do produto novo",
    )
    .await;
    let s = h.wait(|s| state(s) == Some(AnswerState::Done)).await;
    let a = s.answer.clone().unwrap();
    assert_eq!(a.text, "new answer");
    assert!(a.question.contains("prazo"));
    assert_eq!(s.debug.speculations_started, 2);
    assert_eq!(s.debug.speculations_cancelled, 1);
    assert_eq!(s.debug.speculations_revealed, 1);
    let calls = h.answer.calls();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].user_text.contains("prazo"));
    assert_eq!(h.classifier.calls().len(), 2);
    h.at(7_000).await;
    assert_eq!(
        h.snaps.borrow().answer.as_ref().map(|a| a.text.clone()),
        Some("new answer".to_string())
    );
}

#[tokio::test(start_paused = true)]
async fn cooldown_blocks_the_next_automatic_answer() {
    let mut h = harness(Opts::default());
    let sc = h.start().await;
    ask_question(&h, &sc).await;
    let first = h.wait(|s| state(s) == Some(AnswerState::Done)).await;
    ask_second_question(&h, &sc).await;
    h.at(5_000).await;
    let s = h.snaps.borrow().clone();
    assert_eq!(s.debug.speculations_started, 1);
    assert_eq!(s.answer.map(|a| a.id), first.answer.map(|a| a.id));
    assert_eq!(h.answer.calls().len(), 1);
    let block = s.debug.last_guard_block.unwrap_or_default();
    assert!(block.starts_with("cooldown"), "{block}");
}

/// Control for the guard tests: with permissive guards the second question does start.
#[tokio::test(start_paused = true)]
async fn second_question_starts_when_guards_allow() {
    let mut h = harness(Opts {
        guards: GuardConfig {
            cooldown_ms: 0,
            ..GuardConfig::default()
        },
        ..Opts::default()
    });
    let sc = h.start().await;
    ask_question(&h, &sc).await;
    h.wait(|s| state(s) == Some(AnswerState::Done)).await;
    ask_second_question(&h, &sc).await;
    let s = h.wait(|s| s.debug.speculations_revealed == 2).await;
    assert_eq!(s.debug.speculations_started, 2);
    assert_eq!(
        s.answer.map(|a| a.question_line_id),
        Some(Some("them-2".into()))
    );
    assert_eq!(h.answer.calls().len(), 2);
    // The second prompt carries the first turn as cacheable transcript, not as the question.
    let second = &h.answer.calls()[1];
    let context = second.context_text();
    assert!(context.contains(&format!("Them: {QUESTION}")));
    assert!(context.contains("Me: boa pergunta"));
    assert!(!context.contains("prazo"));
}

#[tokio::test(start_paused = true)]
async fn rate_limit_blocks_the_next_speculation() {
    let mut h = harness(Opts {
        classifier: vec![vec![Step::Wait(100), Step::Text("no")]],
        guards: GuardConfig {
            cooldown_ms: 0,
            max_starts_per_minute: 1,
            ..GuardConfig::default()
        },
        ..Opts::default()
    });
    let sc = h.start().await;
    ask_question(&h, &sc).await;
    h.wait(|s| s.debug.speculations_cancelled == 1).await;
    ask_second_question(&h, &sc).await;
    h.at(5_000).await;
    let s = h.snaps.borrow().clone();
    assert_eq!(s.debug.speculations_started, 1);
    assert!(s.answer.is_none());
}

#[tokio::test(start_paused = true)]
async fn spend_cap_blocks_the_next_speculation() {
    let mut h = harness(Opts {
        guards: GuardConfig {
            cooldown_ms: 0,
            spend_cap_micro_usd: 1_000,
            ..GuardConfig::default()
        },
        ..Opts::default()
    });
    let sc = h.start().await;
    ask_question(&h, &sc).await;
    h.wait(|s| state(s) == Some(AnswerState::Done) && s.debug.spend_micro_usd == 1_301)
        .await;
    ask_second_question(&h, &sc).await;
    h.at(5_000).await;
    let s = h.snaps.borrow().clone();
    assert_eq!(s.debug.speculations_started, 1);
    assert_eq!(h.answer.calls().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn manual_ask_with_screenshot() {
    let mut h = harness(Opts::default());
    let mut sc = h.start().await;
    h.app
        .dispatch(Intent::Ask {
            text: "What should I say?".into(),
            with_screenshot: true,
        })
        .unwrap();
    assert_eq!(
        sc.command().await,
        SidecarCommand::RequestPermission {
            permission: PermissionKind::ScreenRecording
        }
    );
    let request_id = match sc.command().await {
        SidecarCommand::CaptureScreenshot {
            request_id,
            max_long_side_px,
        } => {
            assert_eq!(max_long_side_px, 1_920);
            request_id
        }
        other => panic!("expected a screenshot request, got {other:?}"),
    };
    let s = h.snaps.borrow().clone();
    assert_eq!(state(&s), Some(AnswerState::Streaming));
    assert!(h.manual.calls().is_empty());
    h.at(200).await;
    sc.send(SidecarEvent::Screenshot {
        request_id,
        png_base64: base64::engine::general_purpose::STANDARD.encode(b"png-bytes"),
        width_px: 1,
        height_px: 1,
    })
    .await;
    let s = h.wait(|s| state(s) == Some(AnswerState::Done)).await;
    let a = s.answer.unwrap();
    assert_eq!(a.origin, AnswerOrigin::Manual);
    assert_eq!(a.text, "Manual answer.");
    assert_eq!(a.model, "fake-manual");
    assert_eq!(a.error, None);
    // Time to first token from the intent: 200 ms screenshot plus 100 ms to the first token.
    assert_eq!(a.latency_ms, Some(300));
    // Manual asks stay out of the speculative latency stats.
    assert_eq!(s.debug.last_latency_ms, None);
    let calls = h.manual.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].image_png.as_deref(), Some(&b"png-bytes"[..]));
    assert!(calls[0].user_text.starts_with("What should I say?"));
}

#[tokio::test(start_paused = true)]
async fn manual_ask_continues_without_screenshot_after_timeout() {
    let mut h = harness(Opts::default());
    let mut sc = h.start().await;
    h.app
        .dispatch(Intent::Ask {
            text: "Summarise".into(),
            with_screenshot: true,
        })
        .unwrap();
    assert!(matches!(
        sc.command().await,
        SidecarCommand::RequestPermission { .. }
    ));
    let request_id = match sc.command().await {
        SidecarCommand::CaptureScreenshot { request_id, .. } => request_id,
        other => panic!("expected a screenshot request, got {other:?}"),
    };
    let s = h.wait(|s| state(s) == Some(AnswerState::Done)).await;
    let a = s.answer.unwrap();
    assert_eq!(a.text, "Manual answer.");
    assert!(a.error.as_deref().unwrap_or_default().contains("timed out"));
    let latency = a.latency_ms.unwrap();
    assert!((3_100..=3_150).contains(&latency), "{latency}");
    let calls = h.manual.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].image_png, None);
    assert!(!calls[0].user_text.contains("screenshot"));

    sc.send(SidecarEvent::Screenshot {
        request_id,
        png_base64: "AAAA".into(),
        width_px: 1,
        height_px: 1,
    })
    .await;
    h.at(8_000).await;
    assert_eq!(h.manual.calls().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn stale_events_from_a_cancelled_stream_are_ignored() {
    let mut h = harness(Opts {
        manual: vec![
            vec![Step::Wait(10_000), Step::Text("never")],
            vec![
                Step::Wait(100),
                Step::Text("fresh"),
                Step::Wait(100),
                Step::Usage(10, 2),
            ],
        ],
        ..Opts::default()
    });
    let _sc = h.start().await;
    h.app
        .dispatch(Intent::Ask {
            text: "one".into(),
            with_screenshot: false,
        })
        .unwrap();
    h.wait(|s| state(s) == Some(AnswerState::Streaming)).await;
    h.app.dispatch(Intent::CancelAnswer {}).unwrap();
    h.wait(|s| state(s) == Some(AnswerState::Cancelled)).await;
    h.app
        .dispatch(Intent::Ask {
            text: "two".into(),
            with_screenshot: false,
        })
        .unwrap();
    h.wait(|s| {
        s.answer
            .as_ref()
            .is_some_and(|a| a.question == "two" && a.state == AnswerState::Streaming)
    })
    .await;
    // The first ask was stream 1. Its late output must not touch the second answer, but the
    // tokens it was billed for still count (the injected 5 + 5 on top of the estimate the
    // cancel already counted).
    for kind in [
        StreamMsgKind::Delta("stale".into()),
        StreamMsgKind::Usage {
            model: "fake-manual".into(),
            kind: JobKind::Manual,
            usage: Usage {
                input_tokens: 5,
                output_tokens: 5,
                ..Usage::default()
            },
        },
        StreamMsgKind::Done,
    ] {
        h.app.stream_tx.send(StreamMsg { id: 1, kind }).unwrap();
    }
    let s = h.wait(|s| state(s) == Some(AnswerState::Done)).await;
    let a = s.answer.unwrap();
    assert_eq!(a.question, "two");
    assert_eq!(a.text, "fresh");
    let cancelled = super::streams::estimate_input_tokens(&h.manual.calls()[0]);
    h.wait(|s| s.debug.spend_micro_usd == cancelled + 10 + 12)
        .await;
}

#[tokio::test(start_paused = true)]
async fn missing_key_fails_the_answer_with_a_clear_message() {
    let mut h = harness(Opts {
        real_providers: true,
        ..Opts::default()
    });
    let sc = h.start().await;
    ask_question(&h, &sc).await;
    let s = h.wait(|s| state(s) == Some(AnswerState::Failed)).await;
    let a = s.answer.unwrap();
    assert_eq!(a.origin, AnswerOrigin::Auto);
    assert_eq!(
        a.error.as_deref(),
        Some("Add an Anthropic key in Settings > Models")
    );

    h.app
        .dispatch(Intent::Ask {
            text: "hello".into(),
            with_screenshot: false,
        })
        .unwrap();
    let s = h
        .wait(|s| {
            s.answer
                .as_ref()
                .is_some_and(|a| a.origin == AnswerOrigin::Manual && a.state == AnswerState::Failed)
        })
        .await;
    assert_eq!(
        s.answer.unwrap().error.as_deref(),
        Some("Add an Anthropic key in Settings > Models")
    );
}

#[tokio::test(start_paused = true)]
async fn transcript_revisions_revocation_and_line_limit() {
    let mut h = harness(Opts {
        transcript_lines: 2,
        ..Opts::default()
    });
    let sc = h.start().await;
    sc.them(1, 0, 0, 400, "bom dia").await;
    sc.line(Speaker::Me, 1, 0, 500, 900, "hello there").await;
    let s = h.wait(|s| s.transcript.len() == 2).await;
    let ids: Vec<_> = s.transcript.iter().map(|l| l.id.as_str()).collect();
    assert_eq!(ids, ["them-1", "me-1"]);

    sc.them(1, 2, 0, 450, "bom dia pessoal").await;
    sc.them(1, 1, 0, 420, "stale").await;
    sc.line(Speaker::Me, 1, 1, 500, 900, "").await;
    let s = h.wait(|s| s.transcript.len() == 1).await;
    assert_eq!(s.transcript[0].id, "them-1");
    assert_eq!(s.transcript[0].text, "bom dia pessoal");

    sc.them(2, 0, 3_000, 3_400, "segunda").await;
    sc.them(3, 0, 6_000, 6_400, "terceira").await;
    let s = h
        .wait(|s| s.transcript.last().is_some_and(|l| l.id == "them-3"))
        .await;
    let ids: Vec<_> = s.transcript.iter().map(|l| l.id.as_str()).collect();
    assert_eq!(ids, ["them-2", "them-3"]);
}

#[tokio::test(start_paused = true)]
async fn sidecar_exit_mid_session_keeps_the_loop_alive() {
    let mut h = harness(Opts::default());
    let mut notices = h.app.notices();
    let sc = h.start().await;
    for (speaker, n) in [(Speaker::Them, 3), (Speaker::Me, 2), (Speaker::Them, 5)] {
        sc.send(SidecarEvent::Overflow {
            speaker,
            dropped_buffers: n,
        })
        .await;
    }
    h.wait(|s| s.debug.dropped_buffers == 7).await;

    sc.events
        .send(SidecarOutput::Exited {
            code: Some(1),
            success: false,
        })
        .await
        .unwrap();
    let s = h.wait(|s| s.engine == EngineView::Unavailable).await;
    assert_eq!(s.phase, Phase::Listening);
    let notice = tokio::time::timeout(WAIT_LIMIT, notices.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notice.kind, NoticeKind::SidecarUnavailable);

    h.app
        .dispatch(Intent::Ask {
            text: "still there?".into(),
            with_screenshot: false,
        })
        .unwrap();
    h.wait(|s| state(s) == Some(AnswerState::Done)).await;

    h.app.dispatch(Intent::EndSession {}).unwrap();
    h.wait(|s| s.phase == Phase::Idle).await;
    let _sc2 = h.start().await;
    let s = h.snaps.borrow().clone();
    assert_eq!(s.engine, EngineView::Local);
    assert!(s.transcript.is_empty());
}

#[tokio::test(start_paused = true)]
async fn levels_reach_the_snapshot_quantized_and_only_when_a_step_changes() {
    let mut h = harness(Opts::default());
    let sc = h.start().await;
    let before = h.snaps.borrow().clone();
    assert_eq!(before.levels, LevelsView::default());

    sc.send(SidecarEvent::Level {
        speaker: Speaker::Them,
        dbfs: -30.0,
    })
    .await;
    let s = h.wait(|s| s.levels.them == 3).await;
    assert_eq!(s.levels.me, 0);

    // Same step again: nothing new is published.
    let mut rx = h.snaps.clone();
    rx.borrow_and_update();
    for dbfs in [-29.0, -31.0, -30.5] {
        sc.send(SidecarEvent::Level {
            speaker: Speaker::Them,
            dbfs,
        })
        .await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!rx.has_changed().unwrap());

    h.app.dispatch(Intent::EndSession {}).unwrap();
    let s = h.wait(|s| s.phase == Phase::Idle).await;
    h.wait(|s| s.levels == LevelsView::default()).await;
    assert_eq!(s.phase, Phase::Idle);
}

#[tokio::test(start_paused = true)]
async fn dispatch_validates_and_simple_intents_update_the_snapshot() {
    let mut h = harness(Opts::default());
    assert_eq!(
        h.app.dispatch(Intent::Ask {
            text: "   ".into(),
            with_screenshot: false,
        }),
        Err(DispatchError::Invalid(IntentError::AskEmpty))
    );
    h.app
        .dispatch(Intent::SetInvisible { invisible: false })
        .unwrap();
    h.app
        .dispatch(Intent::SetAutoAnswer { enabled: false })
        .unwrap();
    h.app.dispatch(Intent::OpenSettings {}).unwrap();
    let s = h.wait(|s| !s.invisible && !s.auto_answer).await;
    assert_eq!(s.phase, Phase::Idle);

    let sc = h.start().await;
    ask_question(&h, &sc).await;
    h.at(3_000).await;
    let s = h.snaps.borrow().clone();
    assert_eq!(s.debug.speculations_started, 0);
    assert!(s.answer.is_none());

    h.app.shutdown().await;
    assert_eq!(
        h.app.dispatch(Intent::EndSession {}),
        Err(DispatchError::Stopped)
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the Swift sidecar: run `swift build` in native/sotto-audio"]
async fn real_sidecar_file_mode_answers_the_clip_question() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let wav = root.join("fixtures/audio/pt-BR-question.wav");
    let mut h = harness(Opts {
        process_sidecar: Some(ProcessSidecarFactory {
            path: root.join("native/sotto-audio/.build/debug/sotto-audio"),
            args: vec![
                "--file".into(),
                wav.to_string_lossy().into_owned(),
                "--speaker".into(),
                "them".into(),
                "--locale".into(),
                "pt-BR".into(),
                "--realtime".into(),
            ],
        }),
        ..Opts::default()
    });
    h.app
        .dispatch(Intent::StartSession { mode_id: None })
        .unwrap();
    h.wait(|s| s.phase == Phase::Listening).await;
    // File mode exits after the clip, which the core reports as an unavailable engine.
    let s = h.wait(|s| s.engine == EngineView::Unavailable).await;
    assert!(s.transcript.iter().any(|l| l.speaker == Speaker::Them));
    let s = h.wait(|s| state(s) == Some(AnswerState::Done)).await;
    assert_eq!(s.answer.map(|a| a.origin), Some(AnswerOrigin::Auto));
    assert!(s.debug.last_latency_ms.is_some());
}

const ANT_KEY: &str = "sk-ant-api03-SECRETSECRETSECRET-4f2a";
const OAI_KEY: &str = "sk-proj-SECRETSECRETSECRETSECRET-9z8y";

fn provider(s: &AppSnapshot, id: ProviderId) -> &ProviderView {
    s.settings
        .providers
        .iter()
        .find(|p| p.provider == id)
        .expect("provider listed")
}

fn set_key(h: &Harness, provider: ProviderId, key: &str) {
    h.app
        .dispatch(Intent::SetApiKey {
            provider,
            key: ApiKey::new(key),
        })
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn keys_go_to_the_secret_store_and_the_snapshot_only_has_the_mask() {
    let mut h = harness(Opts::default());
    let s = h.wait(|s| !s.settings.providers.is_empty()).await;
    assert!(!provider(&s, ProviderId::Anthropic).configured);

    set_key(&h, ProviderId::Anthropic, ANT_KEY);
    let s = h
        .wait(|s| provider(s, ProviderId::Anthropic).configured)
        .await;
    let p = provider(&s, ProviderId::Anthropic);
    assert_eq!(p.masked_key.as_deref(), Some("sk-ant…4f2a"));
    assert_eq!(
        h.secrets
            .get(Account::ProviderAnthropic)
            .unwrap()
            .as_deref(),
        Some(ANT_KEY)
    );
    assert!(!provider(&s, ProviderId::Openai).configured);
    assert!(!provider(&s, ProviderId::Ollama).configured);

    set_key(&h, ProviderId::Openai, OAI_KEY);
    let s = h.wait(|s| provider(s, ProviderId::Openai).configured).await;
    assert_eq!(
        provider(&s, ProviderId::Openai).masked_key.as_deref(),
        Some("sk-pro…9z8y")
    );

    let replacement = "sk-ant-api03-OTHEROTHEROTHEROTHER-1111";
    set_key(&h, ProviderId::Anthropic, replacement);
    let s = h
        .wait(|s| provider(s, ProviderId::Anthropic).masked_key.as_deref() == Some("sk-ant…1111"))
        .await;
    assert_eq!(
        h.secrets
            .get(Account::ProviderAnthropic)
            .unwrap()
            .as_deref(),
        Some(replacement)
    );

    h.app
        .dispatch(Intent::RemoveApiKey {
            provider: ProviderId::Anthropic,
        })
        .unwrap();
    let s2 = h
        .wait(|s| !provider(s, ProviderId::Anthropic).configured)
        .await;
    assert_eq!(h.secrets.get(Account::ProviderAnthropic).unwrap(), None);
    assert!(provider(&s2, ProviderId::Openai).configured);

    for snap in [&s, &s2] {
        let dump = format!("{snap:?}{}", serde_json::to_string(snap).unwrap());
        for key in [ANT_KEY, OAI_KEY, replacement] {
            assert!(!dump.contains(key));
            assert!(!dump.contains(&key[10..]));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_keychain_failure_becomes_a_notice_without_the_key() {
    let mut h = harness(Opts {
        failing_keychain: true,
        ..Opts::default()
    });
    let mut notices = h.app.notices();
    set_key(&h, ProviderId::Anthropic, ANT_KEY);
    let s = h.wait(|s| s.notice.is_some()).await;
    let message = s.notice.clone().unwrap();
    assert!(message.contains("Anthropic"), "{message}");
    assert!(!message.contains("SECRET"));
    let broadcast = notices.recv().await.unwrap();
    assert_eq!(broadcast.kind, NoticeKind::Keychain);
    assert!(!format!("{broadcast:?}").contains("SECRET"));
    assert!(message.contains("access denied"), "{message}");
    assert!(!provider(&s, ProviderId::Anthropic).configured);

    h.start().await;
    let s = h.wait(|s| s.phase == Phase::Listening).await;
    assert_eq!(s.notice, None);
}

#[tokio::test(start_paused = true)]
async fn models_apply_to_new_answers_and_survive_a_restart() {
    let mut h = harness(Opts::default());
    h.app
        .dispatch(Intent::SetModels {
            auto_answer_model: "claude-haiku-5-5".into(),
            manual_model: "gpt-5".into(),
        })
        .unwrap();
    let s = h.wait(|s| s.settings.manual_model == "gpt-5").await;
    assert_eq!(s.settings.auto_answer_model, "claude-haiku-5-5");

    h.app
        .dispatch(Intent::Ask {
            text: "hello".into(),
            with_screenshot: false,
        })
        .unwrap();
    let s = h.wait(|s| state(s) == Some(AnswerState::Done)).await;
    assert_eq!(s.answer.unwrap().model, "gpt-5");
    assert_eq!(h.alt.calls().len(), 1);
    assert!(h.manual.calls().is_empty());

    h.app
        .dispatch(Intent::SetSpendCap {
            micro_usd: 2_000_000,
        })
        .unwrap();
    h.wait(|s| s.settings.spend_cap_micro_usd == 2_000_000)
        .await;
    h.app
        .dispatch(Intent::SetAutoAnswer { enabled: false })
        .unwrap();
    h.wait(|s| !s.auto_answer).await;

    h.app.shutdown().await;
    let db = Db::open(&h.db_path, KEY).unwrap();
    let mut restarted = Settings::default();
    settings::apply_saved(&mut restarted, &db);
    assert!(!restarted.auto_answer);
    assert!(!event_loop::initial_snapshot(&restarted).auto_answer);
    assert_eq!(restarted.manual_model, "gpt-5");
    assert_eq!(restarted.auto_answer_model, "claude-haiku-5-5");
    assert_eq!(restarted.guards.spend_cap_micro_usd, 2_000_000);
    assert_eq!(
        event_loop::initial_snapshot(&restarted)
            .settings
            .manual_model,
        "gpt-5"
    );
}

#[tokio::test(start_paused = true)]
async fn the_spend_cap_applies_to_the_session_in_progress() {
    let mut h = harness(Opts {
        guards: GuardConfig {
            cooldown_ms: 0,
            ..GuardConfig::default()
        },
        ..Opts::default()
    });
    let sc = h.start().await;
    ask_question(&h, &sc).await;
    h.wait(|s| state(s) == Some(AnswerState::Done) && s.debug.spend_micro_usd == 1_301)
        .await;
    h.app
        .dispatch(Intent::SetSpendCap { micro_usd: 1_000 })
        .unwrap();
    h.wait(|s| s.settings.spend_cap_micro_usd == 1_000).await;
    ask_second_question(&h, &sc).await;
    h.at(5_000).await;
    assert_eq!(h.snaps.borrow().debug.speculations_started, 1);
    assert_eq!(h.answer.calls().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn ollama_is_listed_when_it_answers_and_refreshed_when_settings_open() {
    let local = FakeLocal::new(vec![
        (Duration::ZERO, None),
        (Duration::from_millis(50), Some(vec!["qwen3:8b".into()])),
    ]);
    let mut h = harness(Opts {
        local: Some(Arc::clone(&local)),
        ..Opts::default()
    });
    let s = h.wait(|s| !s.settings.providers.is_empty()).await;
    h.at(100).await;
    assert!(!s.settings.ollama_running);
    assert!(!provider(&h.snaps.borrow(), ProviderId::Ollama).configured);

    h.app.dispatch(Intent::OpenSettings {}).unwrap();
    let s = h.wait(|s| s.settings.ollama_running).await;
    let p = provider(&s, ProviderId::Ollama);
    assert!(p.configured);
    assert_eq!(p.masked_key, None);
    assert_eq!(p.models, vec!["ollama:qwen3:8b".to_string()]);
    assert_eq!(local.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_slow_ollama_times_out_and_never_blocks_the_loop() {
    let mut h = harness(Opts {
        local: Some(FakeLocal::new(vec![(
            Duration::from_secs(3_600),
            Some(vec!["qwen3:8b".into()]),
        )])),
        ..Opts::default()
    });
    h.app
        .dispatch(Intent::SetInvisible { invisible: false })
        .unwrap();
    h.wait(|s| !s.invisible).await;
    h.at(settings::OLLAMA_PROBE_TIMEOUT.as_millis() as u64 + 100)
        .await;
    h.app
        .dispatch(Intent::SetAutoAnswer { enabled: false })
        .unwrap();
    let s = h.wait(|s| !s.auto_answer).await;
    assert!(!s.settings.ollama_running);
}

#[test]
fn classifier_follows_the_provider_of_the_auto_answer_model() {
    assert_eq!(settings::classifier_for("gpt-5", "x"), "gpt-5-mini");
    assert_eq!(
        settings::classifier_for("claude-opus-5-5", "x"),
        "claude-haiku-5-5"
    );
    assert_eq!(
        settings::classifier_for("ollama:qwen3", "x"),
        "ollama:qwen3"
    );
    assert_eq!(settings::classifier_for("mystery", "keep"), "keep");
}

// ---- slice 1 review fixes (docs/reviews/slice-1.md) ----

/// Item 1: tokens of a speculation cancelled before the provider reported usage still count,
/// so the spend cap cannot be bypassed by cancellations.
#[tokio::test(start_paused = true)]
async fn a_cancelled_speculation_still_counts_toward_the_cap() {
    let mut h = harness(Opts {
        classifier: vec![vec![Step::Wait(100), Step::Text("no")]],
        guards: GuardConfig {
            cooldown_ms: 0,
            spend_cap_micro_usd: 1,
            ..GuardConfig::default()
        },
        ..Opts::default()
    });
    let sc = h.start().await;
    ask_question(&h, &sc).await;
    h.wait(|s| s.debug.speculations_cancelled == 1).await;
    let input = super::streams::estimate_input_tokens(&h.answer.calls()[0]);
    assert!(input > 0);
    // fake-answer input costs 1 micro-USD per token; the classifier reported nothing.
    h.wait(|s| s.debug.spend_micro_usd == input).await;
    ask_second_question(&h, &sc).await;
    h.at(5_000).await;
    let s = h.snaps.borrow().clone();
    assert_eq!(s.debug.speculations_started, 1);
    assert_eq!(
        s.debug.last_guard_block.as_deref(),
        Some("spend cap reached")
    );
}

/// Item 2: a model without a price cannot be capped, so automatic answers are blocked with a
/// visible reason; manual asks still work.
#[tokio::test(start_paused = true)]
async fn automatic_answers_need_a_price_for_the_model() {
    let mut h = harness(Opts {
        auto_answer_model: "gpt-5",
        ..Opts::default()
    });
    let mut notices = h.app.notices();
    let sc = h.start().await;
    ask_question(&h, &sc).await;
    let s = h.wait(|s| s.notice.is_some()).await;
    assert_eq!(
        s.notice.as_deref(),
        Some("Set a price for gpt-5 to use automatic answers")
    );
    assert_eq!(
        s.debug.last_guard_block.as_deref(),
        Some("no price for gpt-5")
    );
    assert_eq!(notices.recv().await.unwrap().kind, NoticeKind::Pricing);
    h.at(3_000).await;
    assert_eq!(h.snaps.borrow().debug.speculations_started, 0);
    assert!(h.alt.calls().is_empty());

    h.app
        .dispatch(Intent::Ask {
            text: "hello".into(),
            with_screenshot: false,
        })
        .unwrap();
    let s = h.wait(|s| state(s) == Some(AnswerState::Done)).await;
    assert_eq!(s.answer.unwrap().origin, AnswerOrigin::Manual);
}

#[tokio::test(start_paused = true)]
async fn a_price_override_in_the_data_dir_unblocks_a_model() {
    let mut h = harness(Opts {
        auto_answer_model: "gpt-5",
        pricing_override: Some(
            "[models.\"gpt-5\"]\ninput = 1.0\noutput = 10.0\ncache_read = 0.1\ncache_write = 0.0\n",
        ),
        ..Opts::default()
    });
    let sc = h.start().await;
    ask_question(&h, &sc).await;
    let s = h.wait(|s| state(s) == Some(AnswerState::Done)).await;
    assert_eq!(s.answer.unwrap().text, "Alt answer.");
    assert_eq!(h.alt.calls().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn an_invalid_price_override_is_reported_and_ignored() {
    let mut h = harness(Opts {
        pricing_override: Some("[models.x]\ninput = 1.0\n"),
        ..Opts::default()
    });
    let s = h.wait(|s| s.notice.is_some()).await;
    assert!(s.notice.unwrap().contains("pricing.toml"));
    let sc = h.start().await;
    ask_question(&h, &sc).await;
    h.wait(|s| state(s) == Some(AnswerState::Done)).await;
}

/// Item 3: the sidecar spent 700 ms on setup before `Started`; the turn still ends 400 ms
/// after the speech did, not 700 ms later.
#[tokio::test(start_paused = true)]
async fn sidecar_setup_time_does_not_delay_the_turn_end() {
    let mut h = harness(Opts::default());
    let sc = h.start_with_setup(700).await;
    // Session time t happens at h.at(t - 700).
    sc.them(1, 0, 0, 500, "como voce").await;
    h.at(300).await;
    sc.them(1, 1, 0, 1_000, QUESTION).await;
    let s = h.wait(|s| state(s) == Some(AnswerState::Streaming)).await;
    let revealed_at = Instant::now() - h.origin;
    assert!(
        revealed_at <= Duration::from_millis(750),
        "revealed at {revealed_at:?}"
    );
    assert_eq!(s.answer.unwrap().latency_ms, Some(400));
    let mut sc = sc;
    assert!(sc.drain().contains(&SidecarCommand::Finalize {
        speaker: Speaker::Them,
        through_ms: 1_000,
    }));
}

/// Item 4: an answer that spent its budget reasoning fails with a clear message.
#[tokio::test(start_paused = true)]
async fn an_answer_with_no_text_at_the_token_limit_fails_clearly() {
    let mut h = harness(Opts {
        manual: vec![vec![
            Step::Usage(10, 4_096),
            Step::Fail(ProviderError::NoTextAtTokenLimit),
        ]],
        ..Opts::default()
    });
    h.app
        .dispatch(Intent::Ask {
            text: "hello".into(),
            with_screenshot: false,
        })
        .unwrap();
    let s = h.wait(|s| state(s) == Some(AnswerState::Failed)).await;
    let error = s.answer.unwrap().error.unwrap_or_default();
    assert!(error.contains("output budget"), "{error}");
}

/// Item 5: a fatal error before `Started` ends the session instead of showing Listening.
#[tokio::test(start_paused = true)]
async fn a_fatal_start_error_ends_the_session() {
    let mut h = harness(Opts::default());
    let mut notices = h.app.notices();
    let sc = h.start_until_command("pt-BR").await;
    sc.send(SidecarEvent::Error {
        code: SidecarErrorCode::AssetsUnavailable,
        message: "Speech assets for pt-BR could not be installed".into(),
        fatal: true,
    })
    .await;
    let s = h.wait(|s| s.phase == Phase::Idle).await;
    assert_eq!(s.engine, EngineView::Unavailable);
    assert_eq!(s.session, None);
    let n = notices.recv().await.unwrap();
    assert_eq!(n.kind, NoticeKind::SidecarUnavailable);
    assert!(n.message.contains("assets"));
}

#[tokio::test(start_paused = true)]
async fn a_session_ends_when_started_never_arrives() {
    let mut h = harness(Opts::default());
    let _sc = h.start_until_command("pt-BR").await;
    let s = h.snaps.borrow().clone();
    assert_eq!(s.phase, Phase::Listening);
    let s = h.wait(|s| s.phase == Phase::Idle).await;
    let waited = Instant::now() - h.origin;
    assert!(
        (Duration::from_secs(10)..Duration::from_millis(10_200)).contains(&waited),
        "{waited:?}"
    );
    assert_eq!(s.engine, EngineView::Unavailable);
    assert!(s.notice.unwrap_or_default().contains("10 s"));
    // The wedged helper is dropped, so the next start spawns a new one.
    let _sc2 = h.start().await;
    assert_eq!(h.snaps.borrow().engine, EngineView::Local);
}

/// Item 6: a denied Screen Recording permission answers at once with a note, and the
/// permission is requested on the first screenshot only.
#[tokio::test(start_paused = true)]
async fn screen_recording_denial_falls_back_at_once() {
    let mut h = harness(Opts::default());
    let mut sc = h.start().await;
    let ask = || Intent::Ask {
        text: "What is on my screen?".into(),
        with_screenshot: true,
    };
    h.app.dispatch(ask()).unwrap();
    assert_eq!(
        sc.command().await,
        SidecarCommand::RequestPermission {
            permission: PermissionKind::ScreenRecording
        }
    );
    assert!(matches!(
        sc.command().await,
        SidecarCommand::CaptureScreenshot { .. }
    ));
    h.at(100).await;
    sc.send(SidecarEvent::Error {
        code: SidecarErrorCode::PermissionDenied,
        message: "Screen Recording is not granted".into(),
        fatal: false,
    })
    .await;
    let s = h.wait(|s| state(s) == Some(AnswerState::Done)).await;
    let a = s.answer.unwrap();
    assert!(
        a.error
            .as_deref()
            .unwrap_or_default()
            .contains("Screen Recording"),
        "{:?}",
        a.error
    );
    assert_eq!(a.latency_ms, Some(200));
    assert_eq!(s.engine, EngineView::Local);

    h.app.dispatch(ask()).unwrap();
    assert!(matches!(
        sc.command().await,
        SidecarCommand::CaptureScreenshot { .. }
    ));
}

const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn seed_sessions(db: &Db) {
    db.create_session("old", None, now_ms() - 40 * DAY_MS)
        .unwrap();
    db.create_session("recent", None, now_ms() - DAY_MS)
        .unwrap();
}

/// Polls the database from a second connection while letting the core run.
async fn wait_for_titles(path: &std::path::Path, want: &[&str]) {
    let db = Db::open(path, KEY).unwrap();
    for _ in 0..500 {
        let mut titles: Vec<String> = db
            .list_sessions(10)
            .unwrap()
            .into_iter()
            .map(|s| s.title)
            .collect();
        titles.sort();
        if titles == want {
            return;
        }
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("sessions never became {want:?}");
}

/// Item 7: sessions past the 30-day retention are purged at start and once a day.
#[tokio::test(start_paused = true)]
async fn retention_purges_at_start_and_daily() {
    let h = harness(Opts {
        seed: Some(seed_sessions),
        ..Opts::default()
    });
    wait_for_titles(&h.db_path, &["recent"]).await;
    Db::open(&h.db_path, KEY)
        .unwrap()
        .create_session("aged", None, now_ms() - 31 * DAY_MS)
        .unwrap();
    wait_for_titles(&h.db_path, &["aged", "recent"]).await;
    tokio::time::advance(event_loop::PURGE_EVERY).await;
    wait_for_titles(&h.db_path, &["recent"]).await;
}

/// Item 8: finals the sidecar flushes for the old session after a quick End then Start do
/// not land in the new session.
#[tokio::test(start_paused = true)]
async fn finals_from_the_previous_session_stay_out_of_the_next_one() {
    let mut h = harness(Opts::default());
    let mut sc = h.start().await;
    sc.them(1, 0, 0, 400, "old session line").await;
    h.wait(|s| s.transcript.len() == 1).await;
    h.app.dispatch(Intent::EndSession {}).unwrap();
    h.wait(|s| s.phase == Phase::Idle).await;
    h.app
        .dispatch(Intent::StartSession { mode_id: None })
        .unwrap();
    loop {
        if matches!(sc.command().await, SidecarCommand::Start { .. }) {
            break;
        }
    }
    h.wait(|s| s.phase == Phase::Listening).await;
    sc.send(SidecarEvent::Transcript {
        speaker: Speaker::Them,
        segment_id: 1,
        revision: 1,
        start_ms: 0,
        end_ms: 600,
        text: "old session line, final".into(),
        is_final: true,
    })
    .await;
    sc.send(SidecarEvent::Stopped {}).await;
    sc.send(SidecarEvent::Started {
        t0_host_ns: 2,
        emitted_at_ms: 0,
    })
    .await;
    sc.them(1, 0, 0, 300, "new line").await;
    let s = h.wait(|s| !s.transcript.is_empty()).await;
    let texts: Vec<_> = s.transcript.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, ["new line"]);
}

/// Item 10: what manual asks cost does not use up the automatic-answer cap.
#[tokio::test(start_paused = true)]
async fn manual_spend_does_not_use_up_the_automatic_answer_cap() {
    let mut h = harness(Opts {
        guards: GuardConfig {
            spend_cap_micro_usd: 10,
            ..GuardConfig::default()
        },
        ..Opts::default()
    });
    let sc = h.start().await;
    h.app
        .dispatch(Intent::Ask {
            text: "hello".into(),
            with_screenshot: false,
        })
        .unwrap();
    // 10 in + 2 out at 1 micro-USD each: above the cap of 10.
    h.wait(|s| state(s) == Some(AnswerState::Done) && s.debug.spend_micro_usd == 12)
        .await;
    ask_question(&h, &sc).await;
    let s = h.wait(|s| s.debug.speculations_started == 1).await;
    assert_eq!(s.debug.last_guard_block, None);
}

/// Item 11: each key is read from the Keychain once, then served from memory.
#[tokio::test(start_paused = true)]
async fn the_keychain_is_read_once_per_key() {
    let mut h = harness(Opts::default());
    for text in ["one", "two"] {
        h.app
            .dispatch(Intent::Ask {
                text: text.into(),
                with_screenshot: false,
            })
            .unwrap();
        h.wait(|s| {
            s.answer
                .as_ref()
                .is_some_and(|a| a.question == text && a.state == AnswerState::Done)
        })
        .await;
    }
    // The start-up mask load reads both providers once; the answers read nothing.
    assert_eq!(h.secret_reads.load(Ordering::SeqCst), 2);
    set_key(&h, ProviderId::Anthropic, ANT_KEY);
    h.wait(|s| provider(s, ProviderId::Anthropic).configured)
        .await;
    assert_eq!(h.secret_reads.load(Ordering::SeqCst), 2);
}

/// Item 12: the system locale is used when the engine has it, else en-US.
#[tokio::test(start_paused = true)]
async fn the_system_locale_is_used_when_installed() {
    let mut h = harness(Opts {
        locale: None,
        system_locale: Some("pt-BR"),
        ..Opts::default()
    });
    let _sc = h.start_until_command("pt-BR").await;
    let s = h.wait(|s| !s.settings.available_locales.is_empty()).await;
    assert_eq!(s.settings.locale, "pt-BR");
    assert_eq!(s.settings.available_locales, INSTALLED);
}

#[tokio::test(start_paused = true)]
async fn an_uninstalled_system_locale_falls_back_to_en_us() {
    let mut h = harness(Opts {
        locale: None,
        system_locale: Some("fr-FR"),
        ..Opts::default()
    });
    let _sc = h.start_until_command("en-US").await;
}

#[tokio::test(start_paused = true)]
async fn set_locale_is_checked_saved_and_used_by_the_next_session() {
    let mut h = harness(Opts {
        locale: None,
        ..Opts::default()
    });
    assert_eq!(
        h.app.dispatch(Intent::SetLocale {
            locale: "pt_BR".into()
        }),
        Err(DispatchError::Invalid(IntentError::BadLocale))
    );
    // Opening settings starts the helper, whose Ready lists the installed locales.
    h.app.dispatch(Intent::OpenSettings {}).unwrap();
    let mut sc = h.sidecar().await;
    sc.ready().await;
    let s = h.wait(|s| s.settings.available_locales.len() == 2).await;
    assert_eq!(s.settings.locale, "en-US");

    h.app
        .dispatch(Intent::SetLocale {
            locale: "fr-FR".into(),
        })
        .unwrap();
    let s = h.wait(|s| s.notice.is_some()).await;
    assert!(s.notice.unwrap().contains("not installed"));
    assert_eq!(s.settings.locale, "en-US");

    h.app
        .dispatch(Intent::SetLocale {
            locale: "pt-BR".into(),
        })
        .unwrap();
    h.wait(|s| s.settings.locale == "pt-BR").await;
    h.app
        .dispatch(Intent::StartSession { mode_id: None })
        .unwrap();
    match sc.command().await {
        SidecarCommand::Start { locale, .. } => assert_eq!(locale, "pt-BR"),
        other => panic!("expected start, got {other:?}"),
    }

    h.app.shutdown().await;
    let db = Db::open(&h.db_path, KEY).unwrap();
    let mut restarted = Settings::default();
    settings::apply_saved(&mut restarted, &db);
    assert_eq!(restarted.locale.as_deref(), Some("pt-BR"));
}

#[tokio::test(start_paused = true)]
async fn a_calendar_session_without_a_known_event_still_starts_as_a_meeting() {
    let mut h = harness(Opts::default());
    h.app
        .dispatch(Intent::StartCalendarSession {
            event_id: "gone".into(),
        })
        .unwrap();
    let sc = h.sidecar().await;
    sc.ready().await;
    let snap = h.wait(|s| s.session.is_some()).await;
    assert_eq!(snap.session.unwrap().title, "Meeting");
    assert_eq!(
        snap.calendar.connection,
        sotto_protocol::ui::CalendarConnection::NotConfigured
    );
}
mod modes_tests;
mod onboarding_flow;

#[derive(Default)]
struct FakeBrowser(Mutex<Vec<String>>);

impl crate::calendar::UrlOpener for FakeBrowser {
    fn open(&self, url: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(url.to_string());
        Ok(())
    }
}

fn account_config(
    server: &wiremock::MockServer,
    browser: &Arc<FakeBrowser>,
) -> crate::account::AccountConfig {
    crate::account::AccountConfig {
        push_debounce: Duration::from_millis(50),
        ..crate::account::AccountConfig::enabled(
            &server.uri(),
            Arc::clone(browser) as Arc<dyn crate::calendar::UrlOpener>,
        )
        .unwrap()
    }
}

async fn mount_signed_in_backend(server: &wiremock::MockServer, remote: serde_json::Value) {
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "a1", "refresh_token": "r2", "token_type": "Bearer", "expires_in": 900
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "u1", "email": "me@example.com", "created_at": "2026-01-01T00:00:00Z"
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/preferences"))
        .respond_with(ResponseTemplate::new(200).set_body_json(remote))
        .mount(server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1/preferences"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "version": 9, "updated_at": "2026-11-06T12:00:00Z",
            "preferences": null
        })))
        .mount(server)
        .await;
}

async fn put_bodies(server: &wiremock::MockServer) -> Vec<serde_json::Value> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.method.as_str() == "PUT")
        .filter_map(|r| serde_json::from_slice(&r.body).ok())
        .collect()
}

#[tokio::test]
async fn synced_settings_reach_the_snapshot_and_local_changes_are_pushed() {
    let server = wiremock::MockServer::start().await;
    let remote = serde_json::json!({
        "version": 3,
        "updated_at": "2026-11-06T10:00:00Z",
        "preferences": {
            "settings": {
                "auto_answer_model": "claude-haiku-5-5",
                "manual_model": "gpt-5",
                "spend_cap_micro_usd": 3_000_000,
                "locale": "pt-BR",
                "auto_answer": false,
                "invisible": true,
                "engine_preference": "local"
            },
            "shortcuts": {},
            "modes": []
        }
    });
    mount_signed_in_backend(&server, remote).await;
    let browser = Arc::new(FakeBrowser::default());
    let mut h = harness(Opts {
        account: Some(account_config(&server, &browser)),
        refresh_token: Some("r1"),
        ..Opts::default()
    });

    let snap = h
        .wait(|s| {
            s.account.state == sotto_protocol::ui::AccountState::SignedIn
                && s.account.last_synced_ms.is_some()
        })
        .await;
    assert_eq!(snap.account.email.as_deref(), Some("me@example.com"));
    let snap = h
        .wait(|s| s.settings.spend_cap_micro_usd == 3_000_000)
        .await;
    assert_eq!(snap.settings.manual_model, "gpt-5");
    assert!(!snap.auto_answer);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        put_bodies(&server).await.is_empty(),
        "a pull must not push back"
    );

    h.app
        .dispatch(Intent::SetSpendCap {
            micro_usd: 1_000_000,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let puts = put_bodies(&server).await;
        if let Some(body) = puts.first() {
            assert_eq!(body["base_version"], 3);
            assert_eq!(
                body["preferences"]["settings"]["spend_cap_micro_usd"],
                1_000_000
            );
            assert_eq!(body["preferences"]["settings"]["manual_model"], "gpt-5");
            break;
        }
        assert!(Instant::now() < deadline, "the change was not pushed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn sign_in_intent_opens_the_browser_and_the_callback_signs_in() {
    use sotto_protocol::ui::AccountState;
    let server = wiremock::MockServer::start().await;
    mount_signed_in_backend(
        &server,
        serde_json::json!({"version": 0, "preferences": null, "updated_at": null}),
    )
    .await;
    let browser = Arc::new(FakeBrowser::default());
    let mut h = harness(Opts {
        account: Some(account_config(&server, &browser)),
        ..Opts::default()
    });

    h.app.dispatch(Intent::SignIn {}).unwrap();
    h.wait(|s| s.account.state == AccountState::WaitingForBrowser)
        .await;
    let url = url::Url::parse(browser.0.lock().unwrap().last().unwrap()).unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .unwrap();

    h.app.deep_link("sotto://auth/callback?code=c&state=wrong");
    h.app
        .deep_link(&format!("sotto://auth/callback?code=c&state={state}"));
    let snap = h.wait(|s| s.account.state == AccountState::SignedIn).await;
    assert_eq!(
        h.secrets.get(Account::AuthRefresh).unwrap().as_deref(),
        Some("r2")
    );
    assert!(snap.account.error.is_none());

    h.app.dispatch(Intent::SignOut {}).unwrap();
    h.wait(|s| s.account.state == AccountState::SignedOut).await;
    assert_eq!(h.secrets.get(Account::AuthRefresh).unwrap(), None);
}
