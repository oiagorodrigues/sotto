//! The cloud transcription engine as the event loop sees it (ADR 0003): the opt-in rules, the
//! engine's lifetime inside a session, and the mapping of its events onto the transcript.

use std::sync::Arc;
use std::time::Duration;

use sotto_detect::TranscriptRevision;
use sotto_protocol::Speaker;
use sotto_protocol::sidecar::{EngineKind, SidecarEvent};
use sotto_protocol::ui::{EnginePreference, EngineView, Phase};
use tokio::time::Instant;

use super::{Core, unix_ms};
use crate::app::{NoticeKind, cloud_settings};
use crate::cloud_stt::{CloudConfig, CloudEngine, CloudEvent, FeedError, PcmFrame};

/// How often the spend shown in the debug stats catches up with the audio sent.
const SPEND_REFRESH: Duration = Duration::from_secs(1);

pub(super) struct CloudSession {
    engine: CloudEngine,
    model: String,
    /// Frames the engine's queue refused, per speaker (`Me`, `Them`).
    dropped: [u64; 2],
    counted_micro_usd: u64,
    last_refresh: Instant,
}

fn slot(speaker: Speaker) -> usize {
    usize::from(speaker == Speaker::Them)
}

impl Core {
    pub(super) fn set_engine_preference(&mut self, engine: EnginePreference) {
        self.settings.engine_preference = engine;
        let (key, value) = cloud_settings::engine_row(engine);
        self.store.set_setting(key, value, unix_ms());
        self.refresh_settings_view();
    }

    /// Whether a session may start on the chosen engine. Cloud is never a fallback: it runs
    /// only when the user chose it, and only with a key to use.
    pub(super) fn cloud_precheck(&mut self) -> bool {
        let cloud = self.settings.engine_preference == EnginePreference::Cloud;
        if cloud && self.providers.openai_mask.is_none() {
            self.notice(
                NoticeKind::Cloud,
                "Cloud transcription is on but no OpenAI key is saved. Add one in Settings > Models, or switch back to on-device transcription in Settings > Transcription",
            );
            return false;
        }
        self.session_cloud = cloud;
        true
    }

    pub(super) fn session_engine_view(&self) -> EngineView {
        if self.session_cloud {
            EngineView::Cloud
        } else {
            EngineView::Local
        }
    }

    pub(super) fn session_engine_kind(&self) -> EngineKind {
        if self.session_cloud {
            EngineKind::Raw
        } else {
            EngineKind::Local
        }
    }

    pub(super) fn start_locale(&self) -> String {
        self.providers.locale(&self.settings)
    }

    /// Runs when the helper is ready for `Start`. Returns `false` when the session was ended
    /// instead.
    pub(super) fn prepare_engine(&mut self) -> bool {
        if self.session_cloud {
            self.start_cloud();
            return true;
        }
        let installed = &self.providers.installed_locales;
        let missing = self
            .settings
            .locale
            .as_deref()
            .filter(|saved| {
                !installed.is_empty() && !installed.iter().any(|l| l.eq_ignore_ascii_case(saved))
            })
            .map(str::to_string);
        if let Some(tag) = missing {
            self.notice(
                NoticeKind::Locale,
                format!(
                    "On-device transcription does not have {tag}. Pick another language, or choose cloud transcription in Settings > Transcription (the audio of the call is then sent to OpenAI)"
                ),
            );
            self.abort_session();
            return false;
        }
        true
    }

    /// The session ends before it captured anything, so the helper has nothing to stop.
    fn abort_session(&mut self) {
        if let Some(s) = self.session.take() {
            self.store.end_session(s.id, unix_ms());
        }
        self.snap.phase = Phase::Idle;
        self.snap.session = None;
        self.snap.engine = EngineView::Local;
        self.dirty = true;
    }

    fn start_cloud(&mut self) {
        let locale = self.start_locale();
        let mut config = CloudConfig::new(locale);
        if let Some(url) = &self.settings.cloud_stt_url {
            config.url.clone_from(url);
        }
        let model = config.model.clone();
        let (engine, events) = CloudEngine::start(
            &config,
            Arc::clone(&self.cloud_credentials),
            &[Speaker::Me, Speaker::Them],
        );
        self.cloud = Some(CloudSession {
            engine,
            model,
            dropped: [0; 2],
            counted_micro_usd: 0,
            last_refresh: Instant::now(),
        });
        self.cloud_rx = Some(events);
    }

    pub(super) fn stop_cloud(&mut self) {
        self.refresh_cloud_counters(true);
        self.cloud = None;
    }

    pub(super) fn on_pcm(&mut self, frame: Option<PcmFrame>) {
        let Some(frame) = frame else {
            self.sidecar_pcm = None;
            return;
        };
        let Some(cloud) = self.cloud.as_mut() else {
            return;
        };
        let speaker = frame.speaker;
        if cloud.engine.try_feed(frame) == Err(FeedError::Full) {
            cloud.dropped[slot(speaker)] += 1;
        }
    }

    pub(super) fn on_cloud(&mut self, event: Option<CloudEvent>) {
        let Some(event) = event else {
            self.cloud_rx = None;
            return;
        };
        if self.cloud.is_none() {
            return;
        }
        match event {
            CloudEvent::Transcript(event @ SidecarEvent::Transcript { .. }) => {
                if let Some(rev) = TranscriptRevision::from_event(&event) {
                    self.on_transcript(rev);
                }
            }
            CloudEvent::Transcript(_) => {}
            CloudEvent::Notice(message) => self.notice(NoticeKind::Cloud, message),
            CloudEvent::Unavailable(message) => {
                self.stop_cloud();
                if self.session.is_some() {
                    self.snap.engine = EngineView::Unavailable;
                    self.dirty = true;
                }
                self.notice(NoticeKind::Cloud, message);
            }
        }
    }

    /// Keeps the debug counters in step with the engine: audio billed and frames refused.
    pub(super) fn cloud_tick(&mut self) {
        self.refresh_cloud_counters(false);
    }

    fn refresh_cloud_counters(&mut self, force: bool) {
        let Some(cloud) = self.cloud.as_mut() else {
            return;
        };
        if !force && cloud.last_refresh.elapsed() < SPEND_REFRESH {
            return;
        }
        cloud.last_refresh = Instant::now();
        let total = self
            .prices
            .transcription_cost_micro_usd(&cloud.model, cloud.engine.billed_ms());
        let delta = total.saturating_sub(cloud.counted_micro_usd);
        cloud.counted_micro_usd = total;
        let cloud_dropped = cloud.dropped[0].saturating_add(cloud.dropped[1]);
        let sidecar_dropped = self
            .session
            .as_ref()
            .map_or(0, |s| s.dropped[0].saturating_add(s.dropped[1]));
        let dropped = sidecar_dropped.saturating_add(cloud_dropped);
        if delta > 0 || dropped != self.snap.debug.dropped_buffers {
            self.snap.debug.spend_micro_usd = self.snap.debug.spend_micro_usd.saturating_add(delta);
            self.snap.debug.dropped_buffers = dropped;
            self.dirty = true;
        }
    }
}
