//! Cloud transcription through the whole core: opt-in rules, the raw engine start, PCM in,
//! transcript lines out, and the detector on top. Real sockets and real time (paused time would
//! fire the engine's timeouts while it waits on the loopback).

use sotto_protocol::sidecar::EngineKind;
use sotto_protocol::ui::EnginePreference;

use super::*;
use crate::cloud_stt::PcmFrame;
use crate::cloud_stt::fake_server::{FakeConfig, FakeRealtime};

const OPENAI_KEY: &str = "sk-test-openai-key-123456";
const QUESTION_WORDS: [&str; 8] = [
    "como", "voce", "lidaria", "com", "a", "migracao", "de", "banco",
];

fn cloud_opts(url: &str) -> Opts {
    Opts {
        cloud_url: Some(url.to_string()),
        ..Opts::default()
    }
}

async fn opt_in_to_cloud(h: &mut Harness) {
    h.app
        .dispatch(Intent::SetApiKey {
            provider: ProviderId::Openai,
            key: ApiKey::new(OPENAI_KEY),
        })
        .unwrap();
    h.app
        .dispatch(Intent::SetEnginePreference {
            engine: EnginePreference::Cloud,
        })
        .unwrap();
    h.wait(|s| {
        s.settings.cloud_transcription_available
            && s.settings.engine_preference == EnginePreference::Cloud
    })
    .await;
}

/// Starts a session on the cloud engine; session time 3,000 ms is "now" so audio at 0 to 1,500 ms
/// is already in the past for the turn tracker.
async fn start_cloud_session(h: &mut Harness) -> FakeSidecar {
    h.app
        .dispatch(Intent::StartSession { mode_id: None })
        .unwrap();
    let mut sc = h.sidecar().await;
    sc.ready().await;
    match sc.command().await {
        SidecarCommand::Start {
            engine, speakers, ..
        } => {
            assert_eq!(engine, EngineKind::Raw);
            assert_eq!(speakers, vec![Speaker::Me, Speaker::Them]);
        }
        other => panic!("expected start, got {other:?}"),
    }
    sc.send(SidecarEvent::Started {
        t0_host_ns: 1,
        emitted_at_ms: 3_000,
    })
    .await;
    h.wait(|s| s.phase == Phase::Listening).await;
    sc
}

async fn speak(sc: &FakeSidecar, speaker: Speaker, start_ms: u64) {
    let frame = |t: u64, level: i16| PcmFrame {
        speaker,
        start_ms: t,
        samples: vec![level; 1_600],
    };
    let mut t = start_ms;
    for _ in 0..8 {
        sc.pcm.send(frame(t, 6_000)).await.unwrap();
        t += 100;
    }
    for _ in 0..10 {
        sc.pcm.send(frame(t, 10)).await.unwrap();
        t += 100;
    }
}

#[tokio::test]
async fn a_cloud_session_turns_pcm_into_transcript_lines_and_an_answer() {
    let server = FakeRealtime::start(FakeConfig {
        turns: vec![QUESTION_WORDS.to_vec()],
        ..FakeConfig::default()
    })
    .await;
    let mut h = harness(cloud_opts(&server.url));
    opt_in_to_cloud(&mut h).await;
    let sc = start_cloud_session(&mut h).await;
    assert_eq!(h.snaps.borrow().engine, EngineView::Cloud);

    speak(&sc, Speaker::Them, 0).await;

    let s = h
        .wait(|s| s.transcript.first().is_some_and(|l| l.is_final))
        .await;
    let line = &s.transcript[0];
    assert_eq!(line.id, "them-0");
    assert_eq!(line.speaker, Speaker::Them);
    assert_eq!(line.text, QUESTION_WORDS.join(" "));
    assert_eq!((line.start_ms, line.end_ms), (0, 1_500));

    // The detector, the classifier and the answer flow run on top of the cloud transcript.
    let s = h.wait(|s| state(s) == Some(AnswerState::Done)).await;
    let answer = s.answer.unwrap();
    assert_eq!(answer.origin, AnswerOrigin::Auto);
    assert_eq!(answer.question, QUESTION_WORDS.join(" "));

    let rec = server.recorded();
    assert_eq!(rec.connections, 2, "one connection per speaker");
    assert!(
        rec.auth_headers
            .iter()
            .all(|a| a == &format!("Bearer {OPENAI_KEY}"))
    );
    let langs = &rec.session_updates[0]["session"]["audio"]["input"]["transcription"]["languages"];
    assert_eq!(langs, &serde_json::json!(["pt"]));

    h.app.dispatch(Intent::EndSession {}).unwrap();
    h.wait(|s| s.phase == Phase::Idle).await;
    h.app.shutdown().await;
    let db = Db::open(&h.db_path, KEY).unwrap();
    let session = db.list_sessions(10).unwrap().remove(0);
    let segments = db.list_segments(session.id).unwrap();
    assert_eq!(segments.len(), 1);
    assert!(segments[0].is_final);
    assert_eq!(segments[0].text, QUESTION_WORDS.join(" "));
}

#[tokio::test]
async fn cloud_is_never_used_unless_the_user_chose_it() {
    let server = FakeRealtime::start(FakeConfig::default()).await;
    let mut h = harness(cloud_opts(&server.url));
    // A key exists, but the preference is still the default.
    h.app
        .dispatch(Intent::SetApiKey {
            provider: ProviderId::Openai,
            key: ApiKey::new(OPENAI_KEY),
        })
        .unwrap();
    let s = h.wait(|s| s.settings.cloud_transcription_available).await;
    assert_eq!(s.settings.engine_preference, EnginePreference::Local);

    let sc = h.start().await;
    assert_eq!(h.snaps.borrow().engine, EngineView::Local);
    drop(sc);
    assert_eq!(server.recorded().connections, 0);
}

#[tokio::test]
async fn choosing_cloud_without_a_key_explains_instead_of_starting() {
    let mut h = harness(Opts::default());
    h.app
        .dispatch(Intent::SetEnginePreference {
            engine: EnginePreference::Cloud,
        })
        .unwrap();
    let s = h
        .wait(|s| s.settings.engine_preference == EnginePreference::Cloud)
        .await;
    assert!(!s.settings.cloud_transcription_available);
    h.app
        .dispatch(Intent::StartSession { mode_id: None })
        .unwrap();
    let s = h.wait(|s| s.notice.is_some()).await;
    let notice = s.notice.unwrap();
    assert!(notice.contains("no OpenAI key"), "{notice}");
    assert!(notice.contains("Settings > Models"), "{notice}");
    assert_eq!(s.phase, Phase::Idle);
    assert!(h.sidecars.try_recv().is_err(), "no helper was started");
}

#[tokio::test]
async fn a_locale_the_device_lacks_does_not_start_without_the_cloud_opt_in() {
    let mut h = harness(Opts {
        locale: Some("fr-FR"),
        ..Opts::default()
    });
    h.app
        .dispatch(Intent::StartSession { mode_id: None })
        .unwrap();
    let mut sc = h.sidecar().await;
    sc.ready().await;
    let s = h.wait(|s| s.notice.is_some()).await;
    let notice = s.notice.unwrap();
    assert!(notice.contains("does not have fr-FR"), "{notice}");
    assert!(
        notice.contains("cloud transcription in Settings > Transcription"),
        "{notice}"
    );
    assert_eq!(s.phase, Phase::Idle);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !sc.drain()
            .iter()
            .any(|c| matches!(c, SidecarCommand::Start { .. })),
        "the helper was never told to start"
    );
}

#[tokio::test]
async fn with_the_opt_in_a_locale_the_device_lacks_runs_in_the_cloud() {
    let server = FakeRealtime::start(FakeConfig::default()).await;
    let mut h = harness(Opts {
        locale: Some("fr-FR"),
        ..cloud_opts(&server.url)
    });
    opt_in_to_cloud(&mut h).await;
    h.app
        .dispatch(Intent::StartSession { mode_id: None })
        .unwrap();
    let mut sc = h.sidecar().await;
    sc.ready().await;
    match sc.command().await {
        SidecarCommand::Start { locale, engine, .. } => {
            assert_eq!(locale, "fr-FR");
            assert_eq!(engine, EngineKind::Raw);
        }
        other => panic!("expected start, got {other:?}"),
    }
    h.wait(|s| s.phase == Phase::Listening && s.engine == EngineView::Cloud)
        .await;
}

#[tokio::test]
async fn a_rejected_key_ends_the_engine_with_a_notice() {
    let server = FakeRealtime::start(FakeConfig {
        reject_with: Some(401),
        ..FakeConfig::default()
    })
    .await;
    let mut h = harness(cloud_opts(&server.url));
    opt_in_to_cloud(&mut h).await;
    let _sc = start_cloud_session(&mut h).await;
    let s = h.wait(|s| s.engine == EngineView::Unavailable).await;
    let notice = s.notice.unwrap();
    assert!(notice.contains("rejected the API key"), "{notice}");
    assert!(!notice.contains(OPENAI_KEY));
}

#[tokio::test]
async fn frames_the_engine_cannot_take_are_counted() {
    // A server that accepts the TCP connection and never answers the handshake keeps the
    // engine's queue from draining.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    let mut h = harness(cloud_opts(&url));
    opt_in_to_cloud(&mut h).await;
    let sc = start_cloud_session(&mut h).await;
    for i in 0..900u64 {
        sc.pcm
            .send(PcmFrame {
                speaker: Speaker::Them,
                start_ms: i * 20,
                samples: vec![0; 320],
            })
            .await
            .unwrap();
    }
    // The engine queue holds 512 frames; the rest are refused and counted.
    let s = h.wait(|s| s.debug.dropped_buffers >= 300).await;
    assert!(
        s.debug.dropped_buffers <= 900 - 400,
        "{}",
        s.debug.dropped_buffers
    );
}

#[tokio::test]
async fn the_engine_preference_is_saved_and_restored() {
    let mut h = harness(Opts::default());
    h.app
        .dispatch(Intent::SetEnginePreference {
            engine: EnginePreference::Cloud,
        })
        .unwrap();
    h.wait(|s| s.settings.engine_preference == EnginePreference::Cloud)
        .await;
    h.app.shutdown().await;
    let db = Db::open(&h.db_path, KEY).unwrap();
    assert_eq!(
        db.get_setting("engine_preference").unwrap(),
        Some(serde_json::json!("cloud"))
    );

    let h2 = harness(Opts {
        seed: Some(|db| {
            db.set_setting("engine_preference", &serde_json::json!("cloud"), 1)
                .unwrap();
        }),
        ..Opts::default()
    });
    let s = h2.snaps.borrow().clone();
    // The first snapshot is built from the saved settings before any event.
    assert_eq!(s.settings.engine_preference, EnginePreference::Cloud);
}

#[tokio::test]
async fn the_audio_sent_to_the_cloud_counts_toward_the_session_spend() {
    let server = FakeRealtime::start(FakeConfig {
        turns: vec![QUESTION_WORDS.to_vec()],
        ..FakeConfig::default()
    })
    .await;
    let mut h = harness(cloud_opts(&server.url));
    opt_in_to_cloud(&mut h).await;
    h.app
        .dispatch(Intent::SetAutoAnswer { enabled: false })
        .unwrap();
    let sc = start_cloud_session(&mut h).await;
    speak(&sc, Speaker::Them, 0).await;
    h.wait(|s| s.transcript.first().is_some_and(|l| l.is_final))
        .await;
    // Ending the session settles the counter: 800 ms of speech plus the 700 ms hangover is
    // 1.5 s of audio, which is 425 micro-USD at $0.017 a minute.
    h.app.dispatch(Intent::EndSession {}).unwrap();
    let s = h.wait(|s| s.phase == Phase::Idle).await;
    let sent = server.recorded().appended_ms();
    assert!((1_450..=1_550).contains(&sent), "{sent}");
    assert!(
        (420..=430).contains(&s.debug.spend_micro_usd),
        "{}",
        s.debug.spend_micro_usd
    );
}

#[tokio::test]
async fn the_cloud_accepts_a_language_the_device_lacks_and_local_does_not() {
    let mut h = harness(Opts::default());
    h.app.dispatch(Intent::OpenSettings {}).unwrap();
    let sc = h.sidecar().await;
    sc.ready().await;
    h.wait(|s| !s.settings.available_locales.is_empty()).await;
    h.app
        .dispatch(Intent::SetLocale {
            locale: "fr-FR".into(),
        })
        .unwrap();
    let s = h.wait(|s| s.notice.is_some()).await;
    assert!(s.notice.unwrap().contains("fr-FR is not installed"));
    assert_eq!(h.snaps.borrow().settings.locale, "pt-BR");

    h.app
        .dispatch(Intent::SetEnginePreference {
            engine: EnginePreference::Cloud,
        })
        .unwrap();
    h.app
        .dispatch(Intent::SetLocale {
            locale: "fr-FR".into(),
        })
        .unwrap();
    h.wait(|s| s.settings.locale == "fr-FR").await;
    drop(sc);
}
