//! First-run flow: recording consent, permission states, the local transcription check and the
//! model test. The event loop owns one [`Onboarding`] and copies its view into the snapshot.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sotto_protocol::onboarding::{
    CheckStatus, ClipOutcome, ClipResultView, ModelTestStatus, ModelTestView, OnboardingView,
};
use sotto_protocol::sidecar::{PermissionState, SidecarCommand};
use sotto_protocol::ui::{Intent, Phase};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::SidecarFactory;
use super::check;
use super::models::{ProviderFactory, wire_model};
use super::settings::BgMsg;
use super::store::StoreHandle;
use crate::providers::{CompletionRequest, ProviderError, StreamEvent, Thinking};
use crate::secrets::SecretStore;
use crate::storage::Db;

const KEY_COMPLETED: &str = "onboarding_completed";
const KEY_CONSENT: &str = "recording_consent_at";
const KEY_CHECK: &str = "transcription_check";
const SW_VERS: &str = "/usr/bin/sw_vers";

const MODEL_TEST_TIMEOUT: Duration = Duration::from_secs(20);
const MODEL_TEST_PROMPT: &str = "You are a connectivity check. Reply with the single word: ok";
const MODEL_TEST_MAX_TOKENS: u32 = 32;

/// What the database says about onboarding, read once before the database moves to its thread.
#[derive(Debug, Default)]
pub(crate) struct Saved {
    completed: bool,
    consent: bool,
    check: Option<StoredCheck>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StoredCheck {
    macos_version: Option<String>,
    checked_at_unix_ms: i64,
    results: Vec<ClipResultView>,
}

pub(crate) fn load(db: &Db) -> Saved {
    let get = |key| db.get_setting(key).ok().flatten();
    Saved {
        completed: get(KEY_COMPLETED)
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        consent: get(KEY_CONSENT).and_then(|v| v.as_i64()).is_some(),
        check: get(KEY_CHECK).and_then(|v| serde_json::from_value(v).ok()),
    }
}

/// Where the app starts: onboarding until it was completed once.
pub(crate) fn initial_phase(require: bool, saved: &Saved) -> Phase {
    if require && !saved.completed {
        Phase::Onboarding
    } else {
        Phase::Idle
    }
}

/// The OS version and build, for example `27.0 (26A428)`, read from `sw_vers` by absolute path.
pub(crate) fn detect_macos_version() -> Option<String> {
    let run = |arg: &str| {
        let out = std::process::Command::new(SW_VERS).arg(arg).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let version = run("-productVersion").filter(|v| !v.is_empty())?;
    Some(match run("-buildVersion").filter(|b| !b.is_empty()) {
        Some(build) => format!("{version} ({build})"),
        None => version,
    })
}

/// Results measured on another macOS version no longer describe this machine. Unknown current
/// version means nothing can be said, so nothing is stale.
pub(crate) fn is_stale(measured_on: Option<&str>, current: Option<&str>) -> bool {
    current.is_some_and(|c| measured_on != Some(c))
}

pub(crate) enum Msg {
    CheckProgress {
        done: u32,
        total: u32,
        locale: Option<String>,
    },
    CheckDone(Vec<ClipResultView>),
    ModelTest(ModelTestView),
}

pub(crate) struct Parts {
    pub sidecars: Arc<dyn SidecarFactory>,
    pub providers: Arc<dyn ProviderFactory>,
    pub secrets: Arc<dyn SecretStore>,
    pub bg: mpsc::UnboundedSender<BgMsg>,
    pub clips_dir: Option<PathBuf>,
    pub macos_version: Option<String>,
}

/// What an intent needs from the event loop.
pub(crate) struct Cx<'a> {
    pub store: &'a StoreHandle,
    pub phase: &'a mut Phase,
    pub locale: String,
    pub manual_model: String,
    pub now_ms: i64,
}

/// What the event loop must do after an intent.
#[derive(Debug, PartialEq)]
pub(crate) enum Effect {
    None,
    Sidecar(SidecarCommand),
    Notice(&'static str),
    /// Fresh provider status for the Models step.
    ProbeProviders,
}

pub(crate) struct Onboarding {
    view: OnboardingView,
    parts: Parts,
}

impl Onboarding {
    pub fn new(parts: Parts, saved: &Saved) -> Self {
        let mut view = OnboardingView {
            consent_acknowledged: saved.consent,
            ..OnboardingView::default()
        };
        if let Some(stored) = &saved.check {
            view.check.status = CheckStatus::Done;
            view.check.stale = is_stale(
                stored.macos_version.as_deref(),
                parts.macos_version.as_deref(),
            );
            view.check.macos_version.clone_from(&stored.macos_version);
            view.check.checked_at_unix_ms = Some(stored.checked_at_unix_ms);
            view.check.results.clone_from(&stored.results);
        }
        Self { view, parts }
    }

    pub fn view(&self) -> &OnboardingView {
        &self.view
    }

    pub fn set_permissions(&mut self, states: Vec<PermissionState>) {
        self.view.permissions = states;
    }

    pub fn on_intent(&mut self, intent: Intent, cx: &mut Cx<'_>) -> Effect {
        match intent {
            Intent::AcknowledgeConsent {} => {
                if !self.view.consent_acknowledged {
                    self.view.consent_acknowledged = true;
                    cx.store
                        .set_setting(KEY_CONSENT, cx.now_ms.into(), cx.now_ms);
                }
                Effect::None
            }
            Intent::CheckPermissions {} => Effect::Sidecar(SidecarCommand::CheckPermissions {}),
            Intent::RequestPermission { permission } => {
                Effect::Sidecar(SidecarCommand::RequestPermission { permission })
            }
            Intent::RunTranscriptionCheck {} => {
                self.start_check(&cx.locale);
                Effect::None
            }
            Intent::TestModel {} => {
                self.start_model_test(&cx.manual_model);
                Effect::None
            }
            Intent::CompleteOnboarding {} => {
                if !self.view.consent_acknowledged {
                    return Effect::Notice("Acknowledge the recording notice to finish setup");
                }
                cx.store.set_setting(KEY_COMPLETED, true.into(), cx.now_ms);
                if *cx.phase == Phase::Onboarding {
                    *cx.phase = Phase::Idle;
                }
                Effect::None
            }
            Intent::ReopenOnboarding {} => Effect::ProbeProviders,
            _ => Effect::None,
        }
    }

    pub fn on_msg(&mut self, msg: Msg, store: &StoreHandle, now_ms: i64) {
        match msg {
            Msg::CheckProgress {
                done,
                total,
                locale,
            } => {
                let c = &mut self.view.check;
                c.clips_done = done;
                c.clips_total = total;
                c.running_locale = locale;
            }
            Msg::CheckDone(results) => self.finish_check(results, store, now_ms),
            Msg::ModelTest(view) => self.view.model_test = view,
        }
    }

    fn finish_check(&mut self, results: Vec<ClipResultView>, store: &StoreHandle, now_ms: i64) {
        let c = &mut self.view.check;
        c.running_locale = None;
        c.clips_done = c.clips_total;
        let measured = results.iter().any(|r| r.outcome == ClipOutcome::Measured);
        if measured {
            c.status = CheckStatus::Done;
            c.error = None;
            c.stale = false;
            c.macos_version.clone_from(&self.parts.macos_version);
            c.checked_at_unix_ms = Some(now_ms);
            let stored = StoredCheck {
                macos_version: self.parts.macos_version.clone(),
                checked_at_unix_ms: now_ms,
                results: results.clone(),
            };
            match serde_json::to_value(&stored) {
                Ok(value) => store.set_setting(KEY_CHECK, value, now_ms),
                Err(e) => tracing::warn!(error = %e, "could not encode the check result"),
            }
        } else {
            c.status = CheckStatus::Failed;
            c.error = Some(failure_summary(&results));
        }
        c.results = results;
    }

    fn start_check(&mut self, locale: &str) {
        if self.view.check.status == CheckStatus::Running {
            return;
        }
        let planned = check::plan(locale, &check::bundled_clips());
        let c = &mut self.view.check;
        c.status = CheckStatus::Running;
        c.clips_done = 0;
        c.clips_total = u32::try_from(planned.len()).unwrap_or(u32::MAX);
        c.running_locale = planned.first().map(|(l, _)| l.clone());
        c.error = None;
        c.results.clear();

        let sidecars = Arc::clone(&self.parts.sidecars);
        let clips_dir = self.parts.clips_dir.clone();
        let bg = self.parts.bg.clone();
        let locale = locale.to_string();
        tokio::spawn(async move {
            let progress_tx = bg.clone();
            let results =
                check::run_check(sidecars.as_ref(), clips_dir, &locale, |done, total, l| {
                    let _ = progress_tx.send(BgMsg::Onboarding(Msg::CheckProgress {
                        done,
                        total,
                        locale: l.map(str::to_string),
                    }));
                })
                .await;
            tracing::info!(clips = results.len(), "transcription check finished");
            let _ = bg.send(BgMsg::Onboarding(Msg::CheckDone(results)));
        });
    }

    fn start_model_test(&mut self, model: &str) {
        if self.view.model_test.status == ModelTestStatus::Running {
            return;
        }
        self.view.model_test = ModelTestView {
            status: ModelTestStatus::Running,
            model: Some(model.to_string()),
            latency_ms: None,
            error: None,
        };
        let providers = Arc::clone(&self.parts.providers);
        let secrets = Arc::clone(&self.parts.secrets);
        let bg = self.parts.bg.clone();
        let model = model.to_string();
        tokio::spawn(async move {
            let view = run_model_test(providers, secrets, model).await;
            let _ = bg.send(BgMsg::Onboarding(Msg::ModelTest(view)));
        });
    }
}

fn failure_summary(results: &[ClipResultView]) -> String {
    let all_missing = !results.is_empty()
        && results.iter().all(|r| {
            matches!(
                r.outcome,
                ClipOutcome::LocaleNotInstalled | ClipOutcome::NoSample
            )
        });
    if all_missing {
        return "The on-device model for this language is not installed".into();
    }
    results
        .iter()
        .find(|r| r.outcome == ClipOutcome::EngineFailed)
        .and_then(|r| r.message.clone())
        .unwrap_or_else(|| "The on-device engine could not transcribe the sample clips".into())
}

fn model_test_failure(model: &str, error: String) -> ModelTestView {
    ModelTestView {
        status: ModelTestStatus::Failed,
        model: Some(model.to_string()),
        latency_ms: None,
        error: Some(error),
    }
}

/// One tiny completion on the chosen model. Runs only because the user clicked Test.
async fn run_model_test(
    providers: Arc<dyn ProviderFactory>,
    secrets: Arc<dyn SecretStore>,
    model: String,
) -> ModelTestView {
    let lookup = {
        let model = model.clone();
        tokio::task::spawn_blocking(move || providers.provider(&model, secrets.as_ref())).await
    };
    let provider = match lookup {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => return model_test_failure(&model, e.to_string()),
        Err(_) => return model_test_failure(&model, "The key lookup stopped unexpectedly".into()),
    };
    let request = CompletionRequest {
        model: wire_model(&model).to_string(),
        system_prefix: MODEL_TEST_PROMPT.into(),
        mode_context: String::new(),
        history: String::new(),
        recent: String::new(),
        turns: Vec::new(),
        user_text: "ping".into(),
        image_png: None,
        max_tokens: MODEL_TEST_MAX_TOKENS,
        effort: None,
        thinking: Thinking::Disabled,
    };
    let cancel = CancellationToken::new();
    let started = Instant::now();
    let run = async {
        let mut stream = provider.stream(request, cancel.clone()).await?;
        while let Some(event) = stream.next().await {
            if let StreamEvent::TextDelta(_) = event? {
                return Ok(started.elapsed());
            }
        }
        Err(ProviderError::NoTextAtTokenLimit)
    };
    let outcome = tokio::time::timeout(MODEL_TEST_TIMEOUT, run).await;
    cancel.cancel();
    match outcome {
        Ok(Ok(elapsed)) => ModelTestView {
            status: ModelTestStatus::Passed,
            model: Some(model),
            latency_ms: Some(u32::try_from(elapsed.as_millis()).unwrap_or(u32::MAX)),
            error: None,
        },
        Ok(Err(ProviderError::Unauthorized)) => model_test_failure(
            &model,
            "The provider rejected the API key. Check it in Settings > Models".into(),
        ),
        Ok(Err(e)) => model_test_failure(&model, e.to_string()),
        Err(_) => model_test_failure(&model, "The provider did not answer in time".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_only_when_both_versions_are_known_and_differ() {
        assert!(!is_stale(Some("27.0 (26A428)"), Some("27.0 (26A428)")));
        assert!(is_stale(Some("27.0 (26A428)"), Some("27.1 (26B100)")));
        assert!(is_stale(Some("27.0 (26A428)"), Some("27.0 (26A500)")));
        assert!(is_stale(None, Some("27.0 (26A428)")));
        assert!(!is_stale(Some("27.0"), None));
        assert!(!is_stale(None, None));
    }

    #[test]
    fn onboarding_is_required_until_completed() {
        let done = Saved {
            completed: true,
            ..Saved::default()
        };
        assert_eq!(initial_phase(true, &Saved::default()), Phase::Onboarding);
        assert_eq!(initial_phase(true, &done), Phase::Idle);
        assert_eq!(initial_phase(false, &Saved::default()), Phase::Idle);
    }

    #[test]
    fn a_failed_run_explains_whether_the_model_is_missing_or_the_engine_broke() {
        let r = |outcome, message: Option<&str>| ClipResultView {
            locale: "pt-BR".into(),
            outcome,
            first_partial_ms: None,
            final_ms: None,
            error_rate_pct: None,
            message: message.map(str::to_string),
        };
        assert_eq!(
            failure_summary(&[
                r(ClipOutcome::LocaleNotInstalled, None),
                r(ClipOutcome::NoSample, None)
            ]),
            "The on-device model for this language is not installed"
        );
        assert_eq!(
            failure_summary(&[r(ClipOutcome::EngineFailed, Some("analyzer failed"))]),
            "analyzer failed"
        );
        assert_eq!(
            failure_summary(&[]),
            "The on-device engine could not transcribe the sample clips"
        );
    }
}
