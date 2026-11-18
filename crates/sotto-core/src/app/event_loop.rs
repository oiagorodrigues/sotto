//! The single task that owns all state. Handlers never await: slow work (provider calls,
//! sidecar writes, SQLite) runs elsewhere and reports back over channels.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::cloud_stt::{CloudCredentials, CloudEvent, PcmFrame};
use sotto_detect::{GuardBlock, Guards, Signal, TranscriptRevision, TurnTracker, meaning_changed};
use sotto_protocol::Speaker;
use sotto_protocol::onboarding::OnboardingView;
use sotto_protocol::sidecar::{PermissionKind, SidecarCommand, SidecarErrorCode, SidecarEvent};
use sotto_protocol::ui::{
    AccountView, AnswerOrigin, AnswerState, AnswerView, AppSnapshot, CalendarView, DebugStats,
    EngineView, Intent, LevelsView, Phase, SessionView, TranscriptLine,
};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior};
use uuid::Uuid;

use self::account::{AccountEvent, AccountLink};
use super::calendar::CalendarLink;
use super::models::{ModelFamily, wire_model};
use super::modes::ModesState;
use super::onboarding::{self, Onboarding};
use super::settings::{self, BgMsg, LocalModels, ProviderState, SecretJob};
use super::store::StoreHandle;
use super::streams::{Job, JobKind, Running, Spawner, StreamId, StreamMsg, StreamMsgKind};
use super::{Control, Notice, NoticeKind, Settings, SidecarFactory, price_key};
use crate::account::{AccountHandle, Applies, Command as AccountCommand};
use crate::calendar::{CalendarHandle, Command as CalendarCommand};
use crate::pricing::PriceTable;
use crate::prompt::{self, Verdict};
use crate::providers::Usage;
use crate::sidecar::SidecarOutput;
use crate::storage::{AnswerUpdate, NewAnswer, SegmentRow};

mod account;
mod cloud;

const TICK: Duration = Duration::from_millis(50);
pub(crate) const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(3);
/// From `StartSession` to the sidecar's `Started`, including asset installation.
pub(crate) const START_TIMEOUT: Duration = Duration::from_secs(10);
/// The first purge runs when the loop starts.
pub(crate) const PURGE_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
pub(crate) const SCREEN_PERMISSION_NOTE: &str = "Screenshots need Screen Recording access. Grant it to Sotto in System Settings > Privacy & Security > Screen Recording; answered without it";
const SCREENSHOT_LONG_SIDE_PX: u32 = 1_920;
const SESSION_TITLE: &str = "Meeting";

pub(crate) fn initial_snapshot(settings: &Settings) -> AppSnapshot {
    AppSnapshot {
        phase: Phase::Idle,
        session: None,
        transcript: Vec::new(),
        answer: None,
        invisible: settings.invisible,
        auto_answer: settings.auto_answer,
        engine: EngineView::Local,
        levels: LevelsView::default(),
        debug: DebugStats::default(),
        settings: ProviderState::default().view(settings),
        calendar: CalendarView::not_configured(),
        account: AccountView::signed_out(),
        notice: None,
        onboarding: OnboardingView::default(),
        modes: Vec::new(),
        selected_mode_id: None,
    }
}

pub(super) fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn duration_ms_u32(d: Duration) -> u32 {
    u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
}

fn speaker_key(speaker: Speaker) -> &'static str {
    match speaker {
        Speaker::Me => "me",
        Speaker::Them => "them",
    }
}

fn line_id(speaker: Speaker, segment_id: u64) -> String {
    format!("{}-{segment_id}", speaker_key(speaker))
}

fn describe_block(block: GuardBlock) -> String {
    match block {
        GuardBlock::Cooldown { remaining_ms } => format!("cooldown ({remaining_ms} ms left)"),
        GuardBlock::RateLimited => "rate limit (too many starts this minute)".into(),
        GuardBlock::SpendCap => "spend cap reached".into(),
    }
}

fn add_usage(total: &mut Usage, more: &Usage) {
    total.input_tokens += more.input_tokens;
    total.output_tokens += more.output_tokens;
    total.cache_read_input_tokens += more.cache_read_input_tokens;
    total.cache_creation_input_tokens += more.cache_creation_input_tokens;
}

/// Nearest-rank percentile of a sorted, non-empty slice.
fn percentile(sorted: &[u32], q: f64) -> Option<u32> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (q * sorted.len() as f64).ceil() as usize;
    sorted.get(rank.clamp(1, sorted.len()) - 1).copied()
}

/// Index where the question turn starts (the trailing run of "them" lines that the tracker
/// would join into one turn) and the id of its last line.
fn current_turn(lines: &[TranscriptLine], max_gap_ms: u64) -> (usize, Option<String>) {
    let Some(last) = lines.iter().rposition(|l| l.speaker == Speaker::Them) else {
        return (lines.len(), None);
    };
    let mut first = last;
    while first > 0 {
        let prev = &lines[first - 1];
        if prev.speaker != Speaker::Them
            || lines[first].start_ms.saturating_sub(prev.end_ms) > max_gap_ms
        {
            break;
        }
        first -= 1;
    }
    (first, Some(lines[last].id.clone()))
}

struct Session {
    id: Uuid,
    /// Where the sidecar's t0 falls on this clock; every `*_ms` from the sidecar is relative to
    /// it. `None` until `Started`, and transcript events before then belong to an earlier
    /// session.
    t0: Option<Instant>,
    /// `Start` waits for the sidecar's `Ready` so the locale can be checked against its list.
    start_sent: bool,
    start_deadline: Instant,
    /// The unpriced model already announced in a notice this session.
    unpriced_noticed: Option<String>,
    tracker: TurnTracker,
    guards: Guards,
    /// The whole session in time order; the snapshot shows the newest ones.
    lines: Vec<TranscriptLine>,
    revisions: HashMap<(Speaker, u64), u32>,
    dropped: [u64; 2],
}

impl Session {
    fn now_ms(&self) -> u64 {
        self.t0.map_or(0, |t0| {
            u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX)
        })
    }

    fn set_question(&mut self, id: &str, flag: bool) {
        if let Some(line) = self.lines.iter_mut().find(|l| l.id == id) {
            line.is_question = flag;
        }
    }
}

struct Auto {
    turn_id: u64,
    /// The text the running stream was started with.
    speculated: String,
    confirmed: bool,
    classifier: Option<Running>,
    turn_end_ms: Option<u64>,
    revealed: bool,
}

/// The one answer in flight. Finished answers move to `Core::shown`.
struct Live {
    view: AnswerView,
    session_id: Option<Uuid>,
    persisted: bool,
    stream: Option<Running>,
    stream_done: bool,
    /// Tokens received while the answer is hidden.
    buffered: String,
    hidden_error: Option<String>,
    /// Latency is measured from here to the first rendered token.
    latency_base: Option<Instant>,
    usage: Usage,
    auto: Option<Auto>,
}

impl Live {
    fn hidden(&self) -> bool {
        self.auto.as_ref().is_some_and(|a| !a.revealed)
    }

    fn hidden_turn(&self, turn_id: u64) -> bool {
        self.auto
            .as_ref()
            .is_some_and(|a| !a.revealed && a.turn_id == turn_id)
    }

    fn owns_stream(&self, id: StreamId) -> bool {
        self.stream.as_ref().is_some_and(|r| r.id == id)
    }

    fn owns_classifier(&self, id: StreamId) -> bool {
        self.auto
            .as_ref()
            .and_then(|a| a.classifier.as_ref())
            .is_some_and(|r| r.id == id)
    }
}

struct PendingShot {
    request_id: Uuid,
    deadline: Instant,
}

pub(crate) struct CoreParts {
    pub sidecars: Arc<dyn SidecarFactory>,
    pub prices: PriceTable,
    pub settings: Settings,
    pub spawner: Spawner,
    pub local_models: Arc<dyn LocalModels>,
    pub secret_jobs: mpsc::UnboundedSender<SecretJob>,
    pub bg_tx: mpsc::UnboundedSender<BgMsg>,
    pub modes: ModesState,
    pub store: StoreHandle,
    pub notices: broadcast::Sender<Notice>,
    pub onboarding: Onboarding,
    pub snapshot: AppSnapshot,
    pub snap_tx: watch::Sender<AppSnapshot>,
    pub calendar: CalendarHandle,
    pub account: (AccountHandle, Applies),
    pub cloud_credentials: Arc<dyn CloudCredentials>,
}

enum Event {
    Control(Option<Control>),
    Sidecar(Option<SidecarOutput>),
    Stream(Option<StreamMsg>),
    Background(Option<BgMsg>),
    Calendar,
    Account(AccountEvent),
    Pcm(Option<PcmFrame>),
    Cloud(Option<CloudEvent>),
    Notice(Result<Notice, RecvError>),
    Tick,
    Purge,
}

pub(crate) struct Core {
    sidecars: Arc<dyn SidecarFactory>,
    prices: PriceTable,
    settings: Settings,
    spawner: Spawner,
    local_models: Arc<dyn LocalModels>,
    secret_jobs: mpsc::UnboundedSender<SecretJob>,
    bg_tx: mpsc::UnboundedSender<BgMsg>,
    providers: ProviderState,
    ollama_probing: bool,
    modes: ModesState,
    store: StoreHandle,
    notices: broadcast::Sender<Notice>,
    notice_rx: broadcast::Receiver<Notice>,
    snap: AppSnapshot,
    snap_tx: watch::Sender<AppSnapshot>,
    onboarding: Onboarding,
    dirty: bool,
    transcript_dirty: bool,
    /// Latest quantized input levels. Copied into the snapshot on the tick, and only when they
    /// changed, so level reports (about 20 a second) do not each publish a snapshot.
    pending_levels: LevelsView,
    sidecar_tx: Option<mpsc::UnboundedSender<SidecarCommand>>,
    sidecar_rx: Option<mpsc::Receiver<SidecarOutput>>,
    sidecar_pcm: Option<mpsc::Receiver<PcmFrame>>,
    cloud_credentials: Arc<dyn CloudCredentials>,
    cloud: Option<cloud::CloudSession>,
    cloud_rx: Option<mpsc::Receiver<CloudEvent>>,
    /// The session in progress was started on the cloud engine.
    session_cloud: bool,
    /// The running sidecar has sent `Ready`.
    sidecar_ready: bool,
    screen_permission_requested: bool,
    session: Option<Session>,
    live: Option<Live>,
    shown: Option<AnswerView>,
    shot: Option<PendingShot>,
    latencies: Vec<u32>,
    calendar: CalendarLink,
    account: AccountLink,
}

async fn recv_opt<T>(rx: &mut Option<mpsc::Receiver<T>>) -> Option<T> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

impl Core {
    pub fn new(parts: CoreParts) -> Self {
        Self {
            sidecars: parts.sidecars,
            prices: parts.prices,
            settings: parts.settings,
            spawner: parts.spawner,
            local_models: parts.local_models,
            secret_jobs: parts.secret_jobs,
            bg_tx: parts.bg_tx,
            providers: ProviderState::default(),
            ollama_probing: false,
            modes: parts.modes,
            store: parts.store,
            notice_rx: parts.notices.subscribe(),
            notices: parts.notices,
            snap: parts.snapshot,
            snap_tx: parts.snap_tx,
            onboarding: parts.onboarding,
            dirty: false,
            transcript_dirty: false,
            pending_levels: LevelsView::default(),
            sidecar_tx: None,
            sidecar_rx: None,
            sidecar_pcm: None,
            cloud_credentials: parts.cloud_credentials,
            cloud: None,
            cloud_rx: None,
            session_cloud: false,
            sidecar_ready: false,
            screen_permission_requested: false,
            session: None,
            live: None,
            shown: None,
            shot: None,
            latencies: Vec::new(),
            calendar: CalendarLink::new(parts.calendar),
            account: AccountLink::new(parts.account.0, parts.account.1),
        }
    }

    pub async fn run(
        mut self,
        mut intents: mpsc::Receiver<Control>,
        mut streams: mpsc::UnboundedReceiver<StreamMsg>,
        mut background: mpsc::UnboundedReceiver<BgMsg>,
    ) {
        let _ = self.secret_jobs.send(SecretJob::Load);
        self.probe_ollama();
        self.modes.resume();
        self.report_account_settings();
        self.refresh_account();
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut purge = tokio::time::interval(PURGE_EVERY);
        purge.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            let event = tokio::select! {
                c = intents.recv() => Event::Control(c),
                out = recv_opt(&mut self.sidecar_rx) => Event::Sidecar(out),
                frame = recv_opt(&mut self.sidecar_pcm) => Event::Pcm(frame),
                ev = recv_opt(&mut self.cloud_rx) => Event::Cloud(ev),
                msg = streams.recv() => Event::Stream(msg),
                msg = background.recv() => Event::Background(msg),
                () = self.calendar.changed() => Event::Calendar,
                ev = self.account.next() => Event::Account(ev),
                n = self.notice_rx.recv() => Event::Notice(n),
                _ = tick.tick() => Event::Tick,
                _ = purge.tick() => Event::Purge,
            };
            let refresh_calendar =
                matches!(event, Event::Control(_) | Event::Calendar | Event::Tick);
            match event {
                Event::Control(Some(Control::Intent(intent))) => {
                    self.on_intent(intent);
                    self.report_account_settings();
                }
                Event::Control(Some(Control::Shutdown(done))) => {
                    self.shutdown().await;
                    let _ = done.send(());
                    return;
                }
                Event::Control(None) => {
                    self.shutdown().await;
                    return;
                }
                Event::Sidecar(out) => self.on_sidecar(out),
                Event::Pcm(frame) => self.on_pcm(frame),
                Event::Cloud(ev) => self.on_cloud(ev),
                Event::Stream(Some(msg)) => self.on_stream(msg),
                // The loop holds a sender in its spawner, so the channel cannot close.
                Event::Stream(None) => {}
                Event::Background(Some(msg)) => self.on_background(msg),
                // `bg_tx` lives in the core, so the channel cannot close.
                Event::Background(None) => {}
                Event::Notice(Ok(n)) => {
                    self.snap.notice = Some(n.message);
                    self.dirty = true;
                }
                Event::Notice(Err(RecvError::Lagged(_)) | Err(RecvError::Closed)) => {}
                Event::Calendar => {}
                Event::Account(ev) => self.on_account(ev),
                Event::Tick => self.on_tick(),
                Event::Purge => {
                    if let Some(days) = self.settings.retention_days {
                        self.store.purge(days, unix_ms());
                    }
                }
            }
            if refresh_calendar {
                self.refresh_calendar();
            }
            self.publish();
        }
    }

    async fn shutdown(&mut self) {
        self.modes.shutdown();
        self.end_session();
        self.publish();
        self.sidecar_tx = None;
        self.sidecar_rx = None;
        self.store.close().await;
    }

    fn publish(&mut self) {
        if !self.dirty && !self.transcript_dirty {
            return;
        }
        if self.transcript_dirty
            && let Some(s) = &self.session
        {
            let keep = self.settings.transcript_lines;
            let from = s.lines.len().saturating_sub(keep);
            self.snap.transcript = s.lines[from..].to_vec();
        }
        self.snap.answer = self
            .live
            .as_ref()
            .map(|l| l.view.clone())
            .or_else(|| self.shown.clone());
        self.dirty = false;
        self.transcript_dirty = false;
        self.snap_tx.send_replace(self.snap.clone());
    }

    fn notice(&self, kind: NoticeKind, message: impl Into<String>) {
        let _ = self.notices.send(Notice {
            kind,
            message: message.into(),
        });
    }

    fn now_ms(&self) -> u64 {
        self.session.as_ref().map_or(0, Session::now_ms)
    }

    // ---- intents ----

    fn on_intent(&mut self, intent: Intent) {
        if self.modes.on_intent(&intent) {
            self.sync_modes();
            return;
        }
        match intent {
            Intent::StartSession { mode_id } => self.start_session(mode_id),
            Intent::EndSession {} => self.end_session(),
            Intent::Ask {
                text,
                with_screenshot,
            } => self.manual_ask(text, with_screenshot),
            Intent::CancelAnswer {} => self.cancel_live(),
            Intent::SetInvisible { invisible } => {
                self.snap.invisible = invisible;
                self.dirty = true;
            }
            Intent::SetAutoAnswer { enabled } => {
                self.snap.auto_answer = enabled;
                self.settings.auto_answer = enabled;
                if !enabled {
                    self.discard_hidden();
                }
                let (key, value) = settings::auto_answer_row(enabled);
                self.store.set_setting(key, value, unix_ms());
                self.dirty = true;
            }
            Intent::OpenSettings {} => {
                self.probe_ollama();
                // The helper's `Ready` lists the installed locales for the language picker.
                self.ensure_sidecar();
            }
            Intent::SetLocale { locale } => self.set_locale(locale),
            Intent::CalendarSetClient {
                client_id,
                client_secret,
            } => self.calendar.send(CalendarCommand::SetClient {
                client_id,
                client_secret,
            }),
            Intent::CalendarRemoveClient {} => self.calendar.send(CalendarCommand::RemoveClient),
            Intent::CalendarConnect {} => self.calendar.send(CalendarCommand::Connect),
            Intent::CalendarDisconnect {} => self.calendar.send(CalendarCommand::Disconnect),
            Intent::CalendarRefresh {} => self.calendar.send(CalendarCommand::Refresh),
            Intent::CalendarDismissPrompt { event_id } => self.calendar.dismiss(event_id),
            Intent::StartCalendarSession { event_id } => self.start_calendar_session(event_id),
            i @ (Intent::AcknowledgeConsent {}
            | Intent::CheckPermissions {}
            | Intent::RequestPermission { .. }
            | Intent::RunTranscriptionCheck {}
            | Intent::TestModel {}
            | Intent::CompleteOnboarding {}
            | Intent::ReopenOnboarding {}) => self.on_onboarding_intent(i),
            // The shell opens the System Settings pane.
            Intent::OpenPrivacySettings { .. } => {}
            Intent::SignIn {} => self.account.send(AccountCommand::SignIn),
            Intent::SignOut {} => self.account.send(AccountCommand::SignOut),
            Intent::SyncNow {} => self.account.send(AccountCommand::SyncNow),
            Intent::SetEnginePreference { engine } => self.set_engine_preference(engine),
            Intent::SetApiKey { provider, key } => {
                let _ = self.secret_jobs.send(SecretJob::Set(provider, key));
            }
            Intent::RemoveApiKey { provider } => {
                let _ = self.secret_jobs.send(SecretJob::Remove(provider));
            }
            Intent::SetModels {
                auto_answer_model,
                manual_model,
            } => {
                self.settings.classifier_model =
                    settings::classifier_for(&auto_answer_model, &self.settings.classifier_model);
                self.settings.auto_answer_model = auto_answer_model;
                self.settings.manual_model = manual_model;
                let now = unix_ms();
                for (key, value) in settings::models_rows(
                    &self.settings.auto_answer_model,
                    &self.settings.manual_model,
                ) {
                    self.store.set_setting(key, value, now);
                }
                self.refresh_settings_view();
            }
            Intent::CreateMode { .. }
            | Intent::UpdateMode { .. }
            | Intent::DeleteMode { .. }
            | Intent::SelectMode { .. }
            | Intent::PickModeFiles { .. }
            | Intent::RemoveModeFile { .. }
            | Intent::RetryIndexing { .. } => {}
            Intent::SetSpendCap { micro_usd } => {
                self.settings.guards.spend_cap_micro_usd = micro_usd;
                if let Some(s) = self.session.as_mut() {
                    s.guards.set_spend_cap(micro_usd);
                }
                let (key, value) = settings::spend_cap_row(micro_usd);
                self.store.set_setting(key, value, unix_ms());
                self.refresh_settings_view();
            }
        }
    }

    fn on_onboarding_intent(&mut self, intent: Intent) {
        let mut cx = onboarding::Cx {
            store: &self.store,
            phase: &mut self.snap.phase,
            locale: self.providers.locale(&self.settings),
            manual_model: self.settings.manual_model.clone(),
            now_ms: unix_ms(),
        };
        match self.onboarding.on_intent(intent, &mut cx) {
            onboarding::Effect::None => {}
            onboarding::Effect::Sidecar(cmd) => {
                if self.ensure_sidecar() {
                    self.send_sidecar(cmd);
                }
            }
            onboarding::Effect::Notice(message) => self.notice(NoticeKind::Onboarding, message),
            onboarding::Effect::ProbeProviders => self.probe_ollama(),
        }
        self.snap.onboarding = self.onboarding.view().clone();
        self.dirty = true;
    }

    fn set_locale(&mut self, tag: String) {
        let list = &self.providers.installed_locales;
        let cloud = self.settings.engine_preference == sotto_protocol::ui::EnginePreference::Cloud;
        if !cloud && !list.is_empty() && !list.iter().any(|l| l.eq_ignore_ascii_case(&tag)) {
            self.notice(
                NoticeKind::Locale,
                format!("{tag} is not installed for on-device transcription"),
            );
            return;
        }
        let (key, value) = settings::locale_row(&tag);
        self.store.set_setting(key, value, unix_ms());
        self.settings.locale = Some(tag);
        self.refresh_settings_view();
    }

    fn sync_modes(&mut self) {
        if self.modes.take_changed() {
            let (modes, selected) = self.modes.views();
            self.snap.modes = modes;
            self.snap.selected_mode_id = selected;
            self.dirty = true;
        }
    }

    fn refresh_settings_view(&mut self) {
        self.snap.settings = self.providers.view(&self.settings);
        self.snap.settings.cloud_transcription_available |= self.signed_in();
        self.dirty = true;
    }

    fn probe_ollama(&mut self) {
        if !self.ollama_probing {
            self.ollama_probing = true;
            settings::spawn_ollama_probe(Arc::clone(&self.local_models), self.bg_tx.clone());
        }
    }

    fn on_background(&mut self, msg: BgMsg) {
        match msg {
            BgMsg::Mask(provider, mask) => self.providers.set_mask(provider, mask),
            BgMsg::Ollama(models) => {
                self.ollama_probing = false;
                self.providers.ollama_models = models;
            }
            BgMsg::Notice(n) => {
                self.notice(n.kind, n.message);
                return;
            }
            BgMsg::KeySaved => self.snap.notice = None,
            BgMsg::Onboarding(m) => {
                self.onboarding.on_msg(m, &self.store, unix_ms());
                self.snap.onboarding = self.onboarding.view().clone();
                self.dirty = true;
                return;
            }
            BgMsg::Modes(m) => {
                self.modes.on_msg(m);
                self.sync_modes();
                return;
            }
        }
        self.refresh_settings_view();
    }

    fn ensure_sidecar(&mut self) -> bool {
        if self.sidecar_tx.is_some() {
            return true;
        }
        match self.sidecars.spawn() {
            Ok(link) => {
                self.sidecar_tx = Some(link.commands);
                self.sidecar_rx = Some(link.outputs);
                self.sidecar_pcm = link.pcm;
                self.sidecar_ready = false;
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "sidecar did not start");
                self.notice(
                    NoticeKind::SidecarUnavailable,
                    format!("The audio helper did not start: {e}"),
                );
                false
            }
        }
    }

    fn send_sidecar(&self, cmd: SidecarCommand) -> bool {
        self.sidecar_tx
            .as_ref()
            .is_some_and(|tx| tx.send(cmd).is_ok())
    }

    fn start_calendar_session(&mut self, event_id: String) {
        let title = self.calendar.session_title(&event_id);
        self.calendar.dismiss(event_id);
        self.start_session_titled(None, title.unwrap_or_else(|| SESSION_TITLE.into()));
    }

    fn refresh_calendar(&mut self) {
        let view = self.calendar.view(unix_ms(), self.session.is_some());
        if view != self.snap.calendar {
            self.snap.calendar = view;
            self.dirty = true;
        }
    }

    fn start_session(&mut self, mode_id: Option<Uuid>) {
        self.start_session_titled(mode_id, SESSION_TITLE.into());
    }

    fn start_session_titled(&mut self, mode_id: Option<Uuid>, title: String) {
        if self.session.is_some() || self.snap.phase == Phase::Onboarding {
            return;
        }
        self.dirty = true;
        if !self.cloud_precheck() {
            return;
        }
        if !self.ensure_sidecar() {
            self.snap.engine = EngineView::Unavailable;
            return;
        }
        let mode = self.modes.begin_session(mode_id);
        let id = Uuid::now_v7();
        let now = unix_ms();
        self.store
            .create_session(id, &title, mode.as_ref().map(|m| m.0), now);
        self.session = Some(Session {
            id,
            t0: None,
            start_sent: false,
            start_deadline: Instant::now() + START_TIMEOUT,
            unpriced_noticed: None,
            tracker: TurnTracker::new(self.settings.detector),
            guards: Guards::new(self.settings.guards),
            lines: Vec::new(),
            revisions: HashMap::new(),
            dropped: [0; 2],
        });
        self.snap.phase = Phase::Listening;
        self.snap.session = Some(SessionView {
            id,
            title,
            mode_name: mode.map(|m| m.1),
            started_at_unix_ms: now,
        });
        self.snap.engine = self.session_engine_view();
        self.snap.notice = None;
        self.snap.transcript.clear();
        self.snap.debug = DebugStats::default();
        self.latencies.clear();
        if self.live.is_none() {
            self.shown = None;
        }
        self.transcript_dirty = true;
        if self.sidecar_ready {
            self.send_start();
        }
    }

    fn send_start(&mut self) {
        if !self.prepare_engine() {
            return;
        }
        let locale = self.start_locale();
        let sent = self.send_sidecar(SidecarCommand::Start {
            locale,
            engine: self.session_engine_kind(),
            speakers: vec![Speaker::Me, Speaker::Them],
        });
        if let Some(s) = self.session.as_mut() {
            s.start_sent = sent;
        }
    }

    /// The session never started capturing: end it so the UI does not show Listening with
    /// nothing transcribing.
    fn fail_start(&mut self, message: String) {
        tracing::warn!("sidecar did not start capturing");
        self.notice(NoticeKind::SidecarUnavailable, message);
        self.end_session();
        self.snap.engine = EngineView::Unavailable;
        self.dirty = true;
    }

    fn end_session(&mut self) {
        if self.session.is_none() {
            return;
        }
        self.send_sidecar(SidecarCommand::Stop {});
        self.stop_cloud();
        self.cancel_live();
        if let Some(s) = self.session.take() {
            self.store.end_session(s.id, unix_ms());
        }
        self.modes.end_session();
        self.snap.phase = Phase::Idle;
        self.snap.session = None;
        self.dirty = true;
    }

    fn cancel_live(&mut self) {
        self.shot = None;
        if self.live.as_ref().is_some_and(Live::hidden) {
            self.discard_hidden();
        } else if self.live.is_some() {
            self.finish(AnswerState::Cancelled, None);
        }
    }

    fn manual_ask(&mut self, text: String, with_screenshot: bool) {
        self.cancel_live();
        let session_id = self.session.as_ref().map(|s| s.id);
        let mut live = Live {
            view: AnswerView {
                id: Uuid::now_v7(),
                origin: AnswerOrigin::Manual,
                question: text,
                text: String::new(),
                state: AnswerState::Streaming,
                model: self.settings.manual_model.clone(),
                latency_ms: None,
                question_line_id: None,
                error: None,
            },
            session_id,
            persisted: false,
            stream: None,
            stream_done: false,
            buffered: String::new(),
            hidden_error: None,
            latency_base: Some(Instant::now()),
            usage: Usage::default(),
            auto: None,
        };
        persist_insert(&self.store, &mut live);
        self.live = Some(live);
        self.dirty = true;
        if with_screenshot {
            let request_id = Uuid::now_v7();
            if self.ensure_sidecar() && !self.screen_permission_requested {
                // Shows the system prompt the first time; later captures fail fast if denied.
                self.screen_permission_requested =
                    self.send_sidecar(SidecarCommand::RequestPermission {
                        permission: PermissionKind::ScreenRecording,
                    });
            }
            if self.ensure_sidecar()
                && self.send_sidecar(SidecarCommand::CaptureScreenshot {
                    request_id,
                    max_long_side_px: SCREENSHOT_LONG_SIDE_PX,
                })
            {
                self.shot = Some(PendingShot {
                    request_id,
                    deadline: Instant::now() + SCREENSHOT_TIMEOUT,
                });
                return;
            }
            self.start_manual_stream(
                None,
                Some("The screenshot is unavailable because the audio helper is not running; answered without it"),
            );
            return;
        }
        self.start_manual_stream(None, None);
    }

    fn start_manual_stream(&mut self, png_base64: Option<String>, note: Option<&str>) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let lines = self.session.as_ref().map_or(&[][..], |s| &s.lines[..]);
        let model = live.view.model.clone();
        let request = prompt::manual(
            wire_model(&model),
            "",
            lines,
            &live.view.question,
            png_base64.is_some(),
            self.settings.manual_effort,
        );
        let running = self.spawner.spawn(Job {
            kind: JobKind::Manual,
            model,
            request,
            png_base64,
            mode: self.modes.query(&live.view.question),
        });
        if let Some(live) = self.live.as_mut() {
            live.stream = Some(running);
            if let Some(note) = note {
                live.view.error = Some(note.to_string());
            }
        }
        self.dirty = true;
    }

    fn screenshot_fallback(&mut self, note: &str) {
        if self.shot.take().is_some() {
            self.start_manual_stream(None, Some(note));
        }
    }

    // ---- sidecar ----

    fn on_sidecar(&mut self, out: Option<SidecarOutput>) {
        match out {
            None | Some(SidecarOutput::Exited { .. }) => self.sidecar_gone(),
            // serde messages can quote field values, so the reason is not logged.
            Some(SidecarOutput::BadLine { .. }) => {
                tracing::debug!("sidecar wrote a line that is not a valid event");
            }
            Some(SidecarOutput::Event(event)) => self.on_sidecar_event(event),
        }
    }

    fn sidecar_gone(&mut self) {
        self.sidecar_tx = None;
        self.sidecar_rx = None;
        self.sidecar_pcm = None;
        self.sidecar_ready = false;
        if self.session.as_ref().is_some_and(|s| s.t0.is_none()) {
            self.screenshot_fallback(
                "The screenshot is unavailable because the audio helper stopped; answered without it",
            );
            self.fail_start("The audio helper stopped before transcription started".into());
            return;
        }
        if self.session.is_some() {
            self.snap.engine = EngineView::Unavailable;
            self.notice(
                NoticeKind::SidecarUnavailable,
                "The audio helper stopped. Transcription is paused; end the session and start a new one to retry",
            );
            self.dirty = true;
        }
        self.screenshot_fallback(
            "The screenshot is unavailable because the audio helper stopped; answered without it",
        );
    }

    fn on_sidecar_event(&mut self, event: SidecarEvent) {
        match event {
            SidecarEvent::Ready {
                installed_locales, ..
            } => {
                self.sidecar_ready = true;
                self.providers.installed_locales = installed_locales;
                self.refresh_settings_view();
                if self.session.as_ref().is_some_and(|s| !s.start_sent) {
                    self.send_start();
                }
            }
            SidecarEvent::Started { emitted_at_ms, .. } => {
                if let Some(s) = self.session.as_mut().filter(|s| s.t0.is_none()) {
                    // The sidecar took t0 before its setup, then sent this event.
                    let now = Instant::now();
                    s.t0 = Some(
                        now.checked_sub(Duration::from_millis(emitted_at_ms))
                            .unwrap_or(now),
                    );
                }
            }
            SidecarEvent::Transcript { .. } => {
                if let Some(rev) = TranscriptRevision::from_event(&event) {
                    self.on_transcript(rev);
                }
            }
            SidecarEvent::Screenshot {
                request_id,
                png_base64,
                ..
            } => {
                if self
                    .shot
                    .as_ref()
                    .is_some_and(|s| s.request_id == request_id)
                {
                    self.shot = None;
                    self.start_manual_stream(Some(png_base64), None);
                }
            }
            SidecarEvent::Overflow {
                speaker,
                dropped_buffers,
            } => {
                if let Some(s) = self.session.as_mut() {
                    let slot = &mut s.dropped[usize::from(speaker == Speaker::Them)];
                    *slot = (*slot).max(dropped_buffers);
                    self.snap.debug.dropped_buffers = s.dropped[0].saturating_add(s.dropped[1]);
                    self.dirty = true;
                }
            }
            SidecarEvent::Error {
                code,
                message,
                fatal,
            } => {
                tracing::warn!(?code, fatal, "sidecar error");
                if self.shot.is_some() {
                    // Screen Recording is the only permission a pending screenshot can lack.
                    match code {
                        SidecarErrorCode::PermissionDenied => {
                            self.screenshot_fallback(SCREEN_PERMISSION_NOTE);
                        }
                        SidecarErrorCode::Screenshot => {
                            self.screenshot_fallback("The screenshot failed; answered without it");
                        }
                        _ => {}
                    }
                    if !fatal
                        && matches!(
                            code,
                            SidecarErrorCode::PermissionDenied | SidecarErrorCode::Screenshot
                        )
                    {
                        return;
                    }
                }
                if fatal && self.session.as_ref().is_some_and(|s| s.t0.is_none()) {
                    self.fail_start(message);
                    return;
                }
                if fatal && self.session.is_some() {
                    self.snap.engine = EngineView::Unavailable;
                    self.dirty = true;
                }
                self.notice(NoticeKind::SidecarError, message);
            }
            SidecarEvent::Permissions { states } => {
                self.onboarding.set_permissions(states);
                self.snap.onboarding = self.onboarding.view().clone();
                self.dirty = true;
            }
            SidecarEvent::Level { speaker, dbfs } => {
                let step = LevelsView::quantize(dbfs);
                match speaker {
                    Speaker::Me => self.pending_levels.me = step,
                    Speaker::Them => self.pending_levels.them = step,
                }
            }
            SidecarEvent::Stopped {} => {}
        }
    }

    fn on_transcript(&mut self, rev: TranscriptRevision) {
        let Some(s) = self.session.as_mut().filter(|s| s.t0.is_some()) else {
            return;
        };
        let key = (rev.speaker, rev.segment_id);
        if s.revisions.get(&key).is_some_and(|&r| rev.revision < r) {
            return;
        }
        s.revisions.insert(key, rev.revision);
        self.store.segment(
            s.id,
            SegmentRow {
                speaker: rev.speaker,
                segment_id: rev.segment_id,
                revision: rev.revision,
                start_ms: rev.start_ms,
                end_ms: rev.end_ms,
                text: rev.text.clone(),
                is_final: rev.is_final,
            },
            unix_ms(),
        );
        let id = line_id(rev.speaker, rev.segment_id);
        let was_question = match s.lines.iter().position(|l| l.id == id) {
            Some(i) => s.lines.remove(i).is_question,
            None => false,
        };
        let text = rev.text.trim();
        if !text.is_empty() {
            let line = TranscriptLine {
                id,
                speaker: rev.speaker,
                start_ms: rev.start_ms,
                end_ms: rev.end_ms.max(rev.start_ms),
                text: text.to_string(),
                is_final: rev.is_final,
                is_question: was_question,
            };
            let at = s
                .lines
                .partition_point(|l| (l.start_ms, l.end_ms) <= (line.start_ms, line.end_ms));
            s.lines.insert(at, line);
        }
        self.transcript_dirty = true;
        let signals = s.tracker.on_revision(&rev);
        for signal in signals {
            self.on_signal(signal);
        }
    }

    fn on_tick(&mut self) {
        self.cloud_tick();
        if self.session.is_none() {
            self.pending_levels = LevelsView::default();
        }
        if self.snap.levels != self.pending_levels {
            self.snap.levels = self.pending_levels;
            self.dirty = true;
        }
        if self
            .shot
            .as_ref()
            .is_some_and(|s| Instant::now() >= s.deadline)
        {
            self.screenshot_fallback("The screenshot timed out after 3 s; answered without it");
        }
        let Some(s) = self.session.as_mut() else {
            return;
        };
        if s.t0.is_none() {
            if Instant::now() >= s.start_deadline {
                self.fail_start(format!(
                    "The audio helper did not start transcribing within {} s. Start the session again",
                    START_TIMEOUT.as_secs()
                ));
                // A wedged helper is replaced on the next start.
                self.sidecar_tx = None;
                self.sidecar_rx = None;
                self.sidecar_ready = false;
            }
            return;
        }
        let now_ms = s.now_ms();
        let signals = s.tracker.tick(now_ms);
        for signal in signals {
            self.on_signal(signal);
        }
    }

    // ---- automatic answers (ADR 0006) ----

    fn on_signal(&mut self, signal: Signal) {
        match signal {
            Signal::QuestionLikely { turn_id, text, .. } => self.on_question_likely(turn_id, text),
            Signal::QuestionUpdated { turn_id, text, .. } => {
                self.on_question_updated(turn_id, text);
            }
            Signal::QuestionDropped { turn_id } => {
                if self.live.as_ref().is_some_and(|l| l.hidden_turn(turn_id)) {
                    self.discard_hidden();
                }
            }
            Signal::TurnEnded {
                turn_id,
                text,
                end_ms,
                ..
            } => self.on_turn_ended(turn_id, text, end_ms),
        }
    }

    fn on_question_likely(&mut self, turn_id: u64, text: String) {
        if !self.snap.auto_answer || self.session.is_none() {
            return;
        }
        if let Some(live) = &self.live {
            if !live.hidden() {
                // Never replace an answer the user is reading or asked for.
                return;
            }
            self.discard_hidden();
        }
        self.start_speculation(turn_id, text);
    }

    /// Without a positive price the spend cap would count this model as free.
    fn unpriced(&self, model: &str) -> bool {
        ModelFamily::of(model) != Some(ModelFamily::Ollama)
            && !self.prices.is_priced(price_key(model))
    }

    /// Starts a hidden answer stream and the classifier, if the guards allow it.
    fn start_speculation(&mut self, turn_id: u64, text: String) {
        let model = self.settings.auto_answer_model.clone();
        let classifier_model = self.settings.classifier_model.clone();
        let max_gap = self.settings.detector.max_turn_gap_ms;
        let unpriced = [&model, &classifier_model]
            .into_iter()
            .find(|m| self.unpriced(m))
            .cloned();
        let Some(s) = self.session.as_mut() else {
            return;
        };
        if let Some(m) = unpriced {
            self.snap.debug.last_guard_block = Some(format!("no price for {m}"));
            self.dirty = true;
            let first = s.unpriced_noticed.as_deref() != Some(m.as_str());
            if first {
                s.unpriced_noticed = Some(m.clone());
            }
            if first {
                self.notice(
                    NoticeKind::Pricing,
                    format!("Set a price for {m} to use automatic answers"),
                );
            }
            return;
        }
        let now_ms = s.now_ms();
        if let Err(block) = s.guards.check(now_ms) {
            tracing::debug!(?block, "automatic answer blocked by a guard");
            self.snap.debug.last_guard_block = Some(describe_block(block));
            self.dirty = true;
            return;
        }
        s.guards.record_start(now_ms);
        let (turn_start, question_line_id) = current_turn(&s.lines, max_gap);
        let prefix = &s.lines[..turn_start];
        let answer_req = prompt::auto_answer(wire_model(&model), "", prefix, &text);
        let classifier_req = prompt::classifier(wire_model(&classifier_model), prefix, &text);
        if let Some(id) = &question_line_id {
            s.set_question(id, true);
        }
        let session_id = s.id;
        let stream = self.spawner.spawn(Job {
            kind: JobKind::Auto,
            model: model.clone(),
            request: answer_req,
            png_base64: None,
            mode: self.modes.query(&text),
        });
        let classifier = self.spawner.spawn(Job {
            kind: JobKind::Classifier,
            model: classifier_model,
            request: classifier_req,
            png_base64: None,
            mode: None,
        });
        self.snap.debug.speculations_started += 1;
        self.live = Some(Live {
            view: AnswerView {
                id: Uuid::now_v7(),
                origin: AnswerOrigin::Auto,
                question: text.clone(),
                text: String::new(),
                state: AnswerState::Preparing,
                model,
                latency_ms: None,
                question_line_id,
                error: None,
            },
            session_id: Some(session_id),
            persisted: false,
            stream: Some(stream),
            stream_done: false,
            buffered: String::new(),
            hidden_error: None,
            latency_base: None,
            usage: Usage::default(),
            auto: Some(Auto {
                turn_id,
                speculated: text,
                confirmed: false,
                classifier: Some(classifier),
                turn_end_ms: None,
                revealed: false,
            }),
        });
        self.dirty = true;
        self.transcript_dirty = true;
    }

    /// Cancels the hidden speculation and starts over on `text`. A guard block leaves nothing.
    fn respeculate(&mut self, turn_id: u64, text: String) {
        self.discard_hidden();
        self.start_speculation(turn_id, text);
    }

    fn speculated(&self, turn_id: u64) -> Option<String> {
        self.live
            .as_ref()
            .filter(|l| l.hidden_turn(turn_id))
            .and_then(|l| l.auto.as_ref())
            .map(|a| a.speculated.clone())
    }

    fn on_question_updated(&mut self, turn_id: u64, text: String) {
        let Some(speculated) = self.speculated(turn_id) else {
            return;
        };
        if meaning_changed(&speculated, &text) {
            self.respeculate(turn_id, text);
            return;
        }
        let max_gap = self.settings.detector.max_turn_gap_ms;
        let line = self
            .session
            .as_ref()
            .and_then(|s| current_turn(&s.lines, max_gap).1);
        if let Some(live) = self.live.as_mut() {
            if let Some(a) = live.auto.as_mut() {
                a.turn_end_ms = None;
            }
            live.view.question = text;
            if line.is_some() && line != live.view.question_line_id {
                let old = std::mem::replace(&mut live.view.question_line_id, line.clone());
                if let Some(s) = self.session.as_mut() {
                    if let Some(old) = old {
                        s.set_question(&old, false);
                    }
                    if let Some(new) = &line {
                        s.set_question(new, true);
                    }
                }
                self.transcript_dirty = true;
            }
            self.dirty = true;
        }
    }

    fn on_turn_ended(&mut self, turn_id: u64, text: String, end_ms: u64) {
        if self.session.is_none() {
            return;
        }
        self.send_sidecar(SidecarCommand::Finalize {
            speaker: Speaker::Them,
            through_ms: end_ms,
        });
        let Some(speculated) = self.speculated(turn_id) else {
            return;
        };
        if meaning_changed(&speculated, &text) {
            self.respeculate(turn_id, text.clone());
        }
        let base = self
            .session
            .as_ref()
            .and_then(|s| s.t0)
            .map(|t0| t0 + Duration::from_millis(end_ms));
        let Some(live) = self.live.as_mut().filter(|l| l.hidden_turn(turn_id)) else {
            return;
        };
        if let Some(a) = live.auto.as_mut() {
            a.turn_end_ms = Some(end_ms);
        }
        live.view.question = text;
        live.latency_base = base;
        self.dirty = true;
        self.try_reveal();
    }

    /// Reveals the hidden answer once the turn has ended and the classifier said yes.
    fn try_reveal(&mut self) {
        let now_ms = self.now_ms();
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Some(auto) = live.auto.as_mut() else {
            return;
        };
        if auto.revealed || !auto.confirmed || auto.turn_end_ms.is_none() {
            return;
        }
        auto.revealed = true;
        auto.classifier = None;
        if let Some(s) = self.session.as_mut() {
            s.guards.record_reveal(now_ms);
        }
        self.snap.debug.speculations_revealed += 1;
        self.dirty = true;
        if let Some(error) = live.hidden_error.take() {
            self.finish(AnswerState::Failed, Some(error));
            return;
        }
        live.view.state = AnswerState::Streaming;
        live.view.text = std::mem::take(&mut live.buffered);
        persist_insert(&self.store, live);
        if !live.view.text.is_empty() {
            self.mark_first_render();
        }
        if self.live.as_ref().is_some_and(|l| l.stream_done) {
            self.finish(AnswerState::Done, None);
        }
    }

    /// Hidden speculation is thrown away: cancelled streams, previous answer shown again.
    fn discard_hidden(&mut self) {
        if !self.live.as_ref().is_some_and(Live::hidden) {
            return;
        }
        let Some(live) = self.live.take() else {
            return;
        };
        if let (Some(id), Some(s)) = (&live.view.question_line_id, self.session.as_mut()) {
            s.set_question(id, false);
        }
        self.snap.debug.speculations_cancelled += 1;
        self.dirty = true;
        self.transcript_dirty = true;
    }

    fn finish(&mut self, state: AnswerState, error: Option<String>) {
        let Some(mut live) = self.live.take() else {
            return;
        };
        live.stream = None;
        if let Some(a) = live.auto.as_mut() {
            a.classifier = None;
        }
        live.view.state = state;
        if error.is_some() {
            live.view.error = error;
        }
        persist_insert(&self.store, &mut live);
        if live.persisted {
            self.store.update_answer(
                live.view.id,
                AnswerUpdate {
                    text: live.view.text.clone(),
                    state,
                    latency_ms: live.view.latency_ms,
                    usage: (live.usage != Usage::default()).then_some(live.usage),
                },
                unix_ms(),
            );
        }
        self.shown = Some(live.view);
        self.dirty = true;
    }

    fn mark_first_render(&mut self) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if live.view.latency_ms.is_some() {
            return;
        }
        let Some(base) = live.latency_base else {
            return;
        };
        let ms = duration_ms_u32(Instant::now().saturating_duration_since(base));
        live.view.latency_ms = Some(ms);
        // Manual asks are cold answers; ADR 0006 reports them apart from the speculative ones.
        if live.view.origin == AnswerOrigin::Auto {
            self.latencies.push(ms);
            let mut sorted = self.latencies.clone();
            sorted.sort_unstable();
            let d = &mut self.snap.debug;
            d.last_latency_ms = Some(ms);
            d.p50_latency_ms = percentile(&sorted, 0.50);
            d.p95_latency_ms = percentile(&sorted, 0.95);
        }
    }

    // ---- provider streams ----

    fn on_stream(&mut self, msg: StreamMsg) {
        let StreamMsg { id, kind } = msg;
        match kind {
            StreamMsgKind::Usage { model, kind, usage } => {
                self.on_usage(id, &model, kind, &usage);
            }
            StreamMsgKind::Delta(text) => {
                let Some(live) = self.live.as_mut().filter(|l| l.owns_stream(id)) else {
                    return;
                };
                if live.hidden() {
                    live.buffered.push_str(&text);
                    return;
                }
                live.view.text.push_str(&text);
                self.mark_first_render();
                self.dirty = true;
            }
            StreamMsgKind::Done => {
                let Some(live) = self.live.as_mut().filter(|l| l.owns_stream(id)) else {
                    return;
                };
                live.stream = None;
                live.stream_done = true;
                if !live.hidden() {
                    self.finish(AnswerState::Done, None);
                }
            }
            StreamMsgKind::Failed { message, setup } => {
                let Some(live) = self.live.as_mut() else {
                    return;
                };
                if live.owns_stream(id) {
                    if live.hidden() && !setup {
                        live.stream = None;
                        live.stream_done = true;
                        live.hidden_error = Some(message);
                    } else {
                        self.finish(AnswerState::Failed, Some(message));
                    }
                } else if live.owns_classifier(id) && setup {
                    // A missing key or unknown model will fail every answer; say so now.
                    self.finish(AnswerState::Failed, Some(message));
                } else if live.owns_classifier(id) {
                    tracing::debug!("classifier failed; speculation cancelled");
                    self.notice(
                        NoticeKind::Classifier,
                        format!("Could not check whether that was a question: {message}"),
                    );
                    self.discard_hidden();
                }
            }
            StreamMsgKind::Warning(note) => {
                if let Some(live) = self.live.as_mut().filter(|l| l.owns_stream(id)) {
                    live.view.error = Some(note);
                    self.dirty = true;
                }
            }
            StreamMsgKind::Verdict(verdict) => {
                let Some(live) = self.live.as_mut().filter(|l| l.owns_classifier(id)) else {
                    return;
                };
                if verdict == Verdict::Yes {
                    if let Some(a) = live.auto.as_mut() {
                        a.confirmed = true;
                        a.classifier = None;
                    }
                    self.try_reveal();
                } else {
                    self.discard_hidden();
                }
            }
        }
    }

    /// Spend counts even for cancelled streams: those tokens were billed. The debug total
    /// includes manual asks; the cap only counts what automatic answering spent.
    fn on_usage(&mut self, id: StreamId, model: &str, kind: JobKind, usage: &Usage) {
        let cost = self.prices.cost_micro_usd(price_key(model), usage);
        self.snap.debug.spend_micro_usd = self.snap.debug.spend_micro_usd.saturating_add(cost);
        if kind != JobKind::Manual
            && let Some(s) = self.session.as_mut()
        {
            s.guards.record_spend(cost);
        }
        if let Some(live) = self.live.as_mut().filter(|l| l.owns_stream(id)) {
            add_usage(&mut live.usage, usage);
        }
        self.dirty = true;
    }
}

fn persist_insert(store: &StoreHandle, live: &mut Live) {
    let Some(session_id) = live.session_id else {
        return;
    };
    if live.persisted {
        return;
    }
    store.insert_answer(
        live.view.id,
        NewAnswer {
            session_id,
            origin: live.view.origin,
            question: live.view.question.clone(),
            model: live.view.model.clone(),
            state: live.view.state,
        },
        unix_ms(),
    );
    live.persisted = true;
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn percentiles_use_nearest_rank() {
        assert_eq!(percentile(&[], 0.5), None);
        assert_eq!(percentile(&[400], 0.5), Some(400));
        assert_eq!(percentile(&[100, 200, 300, 400], 0.5), Some(200));
        let v: Vec<u32> = (1..=20).collect();
        assert_eq!(percentile(&v, 0.95), Some(19));
    }

    fn line(speaker: Speaker, id: u64, start: u64, end: u64) -> TranscriptLine {
        TranscriptLine {
            id: line_id(speaker, id),
            speaker,
            start_ms: start,
            end_ms: end,
            text: "x".into(),
            is_final: false,
            is_question: false,
        }
    }

    #[test]
    fn current_turn_is_the_trailing_them_run() {
        let lines = vec![
            line(Speaker::Them, 1, 0, 900),
            line(Speaker::Me, 1, 1_000, 1_500),
            line(Speaker::Them, 2, 2_000, 3_000),
            line(Speaker::Them, 3, 3_500, 4_000),
        ];
        assert_eq!(current_turn(&lines, 1_500), (2, Some("them-3".into())));
        assert_eq!(current_turn(&lines, 100), (3, Some("them-3".into())));
        assert_eq!(current_turn(&lines[..2], 1_500), (0, Some("them-1".into())));
        assert_eq!(current_turn(&[], 1_500), (0, None));
    }
}
