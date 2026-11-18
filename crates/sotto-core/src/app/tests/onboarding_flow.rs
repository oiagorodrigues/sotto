use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sotto_protocol::onboarding::{CheckStatus, ClipOutcome, ModelTestStatus};
use sotto_protocol::sidecar::{PermissionState, PermissionStatus};

use super::*;
use crate::app::check::bundled_clips;
use crate::app::onboarding::{self as flow, Onboarding};

fn clip_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio")
}

fn onboarding_required(s: &mut Settings) {
    s.require_onboarding = true;
    s.clips_dir = Some(clip_dir());
    s.macos_version = Some("27.0 (A1)".into());
}

fn required() -> Opts {
    Opts {
        tweak_settings: Some(onboarding_required),
        ..Opts::default()
    }
}

/// Only file mode works: each locale plays its own script.
struct FileModeSidecars(Mutex<HashMap<String, Vec<SidecarOutput>>>);

impl SidecarFactory for FileModeSidecars {
    fn spawn(&self) -> Result<SidecarLink, crate::sidecar::SidecarError> {
        Err(crate::sidecar::SidecarError::StdinClosed)
    }

    fn spawn_file_mode(
        &self,
        _wav: &Path,
        locale: &str,
    ) -> Result<SidecarLink, crate::sidecar::SidecarError> {
        let (commands, _rx) = mpsc::unbounded_channel();
        let (tx, outputs) = mpsc::channel(64);
        let script = self.0.lock().unwrap().remove(locale).unwrap_or_default();
        for out in script {
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

/// A run that transcribes the reference text perfectly.
fn perfect(locale: &str, installed: &[&str]) -> Vec<SidecarOutput> {
    let text = bundled_clips()
        .into_iter()
        .find(|c| c.locale == locale)
        .unwrap()
        .text;
    vec![
        ready(installed),
        SidecarOutput::Event(SidecarEvent::Started {
            t0_host_ns: 1,
            emitted_at_ms: 0,
        }),
        SidecarOutput::Event(SidecarEvent::Transcript {
            speaker: Speaker::Them,
            segment_id: 1,
            revision: 1,
            start_ms: 0,
            end_ms: 1,
            text,
            is_final: true,
        }),
        SidecarOutput::Event(SidecarEvent::Stopped {}),
    ]
}

fn scripted(scripts: Vec<(&str, Vec<SidecarOutput>)>) -> Arc<dyn SidecarFactory> {
    Arc::new(FileModeSidecars(Mutex::new(
        scripts
            .into_iter()
            .map(|(l, s)| (l.to_string(), s))
            .collect(),
    )))
}

#[tokio::test(start_paused = true)]
async fn starts_in_onboarding_and_leaves_it_only_after_consent() {
    let mut h = harness(required());
    let s = h.wait(|s| s.phase == Phase::Onboarding).await;
    assert!(!s.onboarding.consent_acknowledged);

    h.app
        .dispatch(Intent::StartSession { mode_id: None })
        .unwrap();
    h.at(200).await;
    assert_eq!(h.snaps.borrow().phase, Phase::Onboarding);
    assert!(h.sidecars.try_recv().is_err(), "no sidecar before setup");

    h.app.dispatch(Intent::CompleteOnboarding {}).unwrap();
    let s = h.wait(|s| s.notice.is_some()).await;
    assert_eq!(s.phase, Phase::Onboarding);

    h.app.dispatch(Intent::AcknowledgeConsent {}).unwrap();
    h.wait(|s| s.onboarding.consent_acknowledged).await;
    h.app.dispatch(Intent::CompleteOnboarding {}).unwrap();
    h.wait(|s| s.phase == Phase::Idle).await;
    assert!(h.manual.calls().is_empty() && h.answer.calls().is_empty());

    h.app.shutdown().await;
    let db = Db::open(&h.db_path, KEY).unwrap();
    let saved = flow::load(&db);
    assert_eq!(flow::initial_phase(true, &saved), Phase::Idle);
    assert!(Onboarding::new(parts(), &saved).view().consent_acknowledged);
}

#[tokio::test(start_paused = true)]
async fn the_core_without_a_window_starts_idle() {
    let mut h = harness(Opts::default());
    let s = h.wait(|s| s.phase == Phase::Idle).await;
    assert!(!s.onboarding.consent_acknowledged);
}

#[tokio::test(start_paused = true)]
async fn permission_states_come_from_the_helper() {
    let mut h = harness(required());
    h.app.dispatch(Intent::CheckPermissions {}).unwrap();
    let mut sc = h.sidecar().await;
    assert_eq!(sc.command().await, SidecarCommand::CheckPermissions {});

    h.app
        .dispatch(Intent::RequestPermission {
            permission: PermissionKind::SystemAudio,
        })
        .unwrap();
    assert_eq!(
        sc.command().await,
        SidecarCommand::RequestPermission {
            permission: PermissionKind::SystemAudio
        }
    );

    let states = vec![
        PermissionState {
            permission: PermissionKind::Microphone,
            status: PermissionStatus::Granted,
        },
        PermissionState {
            permission: PermissionKind::SystemAudio,
            status: PermissionStatus::NotDetermined,
        },
    ];
    sc.send(SidecarEvent::Permissions {
        states: states.clone(),
    })
    .await;
    let s = h.wait(|s| !s.onboarding.permissions.is_empty()).await;
    assert_eq!(s.onboarding.permissions, states);
}

#[tokio::test(start_paused = true)]
async fn the_check_measures_both_clips_and_stores_the_result() {
    let mut h = harness(Opts {
        sidecar_override: Some(scripted(vec![
            ("pt-BR", perfect("pt-BR", &["pt-BR", "en-US"])),
            ("en-US", perfect("en-US", &["pt-BR", "en-US"])),
        ])),
        ..required()
    });
    h.app.dispatch(Intent::RunTranscriptionCheck {}).unwrap();
    let s = h
        .wait(|s| s.onboarding.check.status != CheckStatus::NotRun)
        .await;
    assert_eq!(s.onboarding.check.clips_total, 2);
    let s = h
        .wait(|s| s.onboarding.check.status == CheckStatus::Done)
        .await;
    let check = &s.onboarding.check;
    assert_eq!(check.results.len(), 2);
    assert!(
        check
            .results
            .iter()
            .all(|r| r.outcome == ClipOutcome::Measured)
    );
    assert!(check.results.iter().all(|r| r.error_rate_pct == Some(0.0)));
    assert_eq!(check.macos_version.as_deref(), Some("27.0 (A1)"));
    assert!(!check.stale);
    assert!(check.checked_at_unix_ms.is_some());

    h.app.shutdown().await;
    let db = Db::open(&h.db_path, KEY).unwrap();
    let saved = flow::load(&db);
    let same = Onboarding::new(
        flow::Parts {
            macos_version: Some("27.0 (A1)".into()),
            ..parts()
        },
        &saved,
    );
    assert_eq!(same.view().check.results.len(), 2);
    assert!(!same.view().check.stale);
    let updated = Onboarding::new(
        flow::Parts {
            macos_version: Some("27.1 (B2)".into()),
            ..parts()
        },
        &saved,
    );
    assert_eq!(updated.view().check.status, CheckStatus::Done);
    assert!(updated.view().check.stale);
}

#[tokio::test(start_paused = true)]
async fn a_missing_locale_is_reported_while_the_other_clip_still_runs() {
    let mut h = harness(Opts {
        sidecar_override: Some(scripted(vec![
            ("pt-BR", vec![ready(&["en-US"])]),
            ("en-US", perfect("en-US", &["en-US"])),
        ])),
        ..required()
    });
    h.app.dispatch(Intent::RunTranscriptionCheck {}).unwrap();
    let s = h
        .wait(|s| s.onboarding.check.status == CheckStatus::Done)
        .await;
    let outcomes: Vec<_> = s
        .onboarding
        .check
        .results
        .iter()
        .map(|r| r.outcome)
        .collect();
    assert_eq!(
        outcomes,
        [ClipOutcome::LocaleNotInstalled, ClipOutcome::Measured]
    );
}

#[tokio::test(start_paused = true)]
async fn a_check_that_measures_nothing_fails_and_keeps_nothing() {
    let mut h = harness(required());
    h.app.dispatch(Intent::RunTranscriptionCheck {}).unwrap();
    let s = h
        .wait(|s| s.onboarding.check.status == CheckStatus::Failed)
        .await;
    assert!(s.onboarding.check.error.is_some());
    assert!(
        s.onboarding
            .check
            .results
            .iter()
            .all(|r| r.outcome == ClipOutcome::EngineFailed)
    );
    assert!(s.onboarding.check.checked_at_unix_ms.is_none());
    h.app.shutdown().await;
    let db = Db::open(&h.db_path, KEY).unwrap();
    assert!(db.get_setting("transcription_check").unwrap().is_none());
}

#[tokio::test(start_paused = true)]
async fn the_model_test_runs_only_when_asked_and_only_on_the_manual_model() {
    let mut h = harness(required());
    h.app.dispatch(Intent::AcknowledgeConsent {}).unwrap();
    h.wait(|s| s.onboarding.consent_acknowledged).await;
    h.at(500).await;
    assert!(h.manual.calls().is_empty());
    assert_eq!(
        h.snaps.borrow().onboarding.model_test.status,
        ModelTestStatus::Idle
    );

    h.app.dispatch(Intent::TestModel {}).unwrap();
    let s = h
        .wait(|s| s.onboarding.model_test.status == ModelTestStatus::Passed)
        .await;
    assert_eq!(
        s.onboarding.model_test.model.as_deref(),
        Some("fake-manual")
    );
    assert!(s.onboarding.model_test.latency_ms.is_some());
    assert_eq!(h.manual.calls().len(), 1);
    assert!(h.answer.calls().is_empty() && h.classifier.calls().is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_model_test_reports_a_model_without_a_provider() {
    let mut h = harness(required());
    h.app
        .dispatch(Intent::SetModels {
            auto_answer_model: "gpt-5".into(),
            manual_model: "gpt-4o".into(),
        })
        .unwrap();
    h.wait(|s| s.settings.manual_model == "gpt-4o").await;
    h.app.dispatch(Intent::TestModel {}).unwrap();
    let s = h
        .wait(|s| s.onboarding.model_test.status == ModelTestStatus::Failed)
        .await;
    assert!(s.onboarding.model_test.error.is_some());
}

fn parts() -> flow::Parts {
    let (bg, _rx) = mpsc::unbounded_channel();
    flow::Parts {
        sidecars: scripted(Vec::new()),
        providers: Arc::new(FakeProviders(HashMap::new())),
        secrets: Arc::new(MemoryStore::new()),
        bg,
        clips_dir: None,
        macos_version: None,
    }
}
