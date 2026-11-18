//! The cloud transcription engine: one WebSocket connection per speaker to the OpenAI Realtime
//! transcription endpoint. Each speaker's task owns a turn gate, a resampler and a transcript
//! mapper, so everything about one connection lives in one place and needs no locks.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use sotto_protocol::Speaker;
use sotto_protocol::sidecar::SidecarEvent;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;

use super::credentials::{CloudCredentials, Credential, CredentialError};
use super::gate::{GateAction, GateConfig, TurnGate};
use super::mapper::Mapper;
use super::pcm::PcmFrame;
use super::resample::Resampler16To24;
use super::wire::{self, ServerEvent};

/// Frames queued per speaker between the core and the connection task. At the sidecar's buffer
/// sizes this is tens of seconds, which covers a reconnect.
const PCM_QUEUE_FRAMES: usize = 512;
const EVENT_QUEUE: usize = 256;
/// Largest `input_audio_buffer.append` payload we build, in 24 kHz samples (one second).
const MAX_APPEND_SAMPLES: usize = 24_000;
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct CloudConfig {
    pub url: String,
    pub model: String,
    /// BCP 47 locale of the meeting; only its language is sent, as a hint.
    pub locale: String,
    pub gate: GateConfig,
    pub connect_timeout: Duration,
    /// How long to wait for `session.updated` before sending audio anyway.
    pub ready_timeout: Duration,
    pub reconnect_delay: Duration,
    /// A connection that lived this long earns back its reconnect.
    pub healthy_after: Duration,
}

impl CloudConfig {
    #[must_use]
    pub fn new(locale: impl Into<String>) -> Self {
        Self {
            url: wire::DEFAULT_URL.into(),
            model: wire::DEFAULT_MODEL.into(),
            locale: locale.into(),
            gate: GateConfig::default(),
            connect_timeout: Duration::from_secs(10),
            ready_timeout: Duration::from_secs(3),
            reconnect_delay: Duration::from_millis(500),
            healthy_after: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CloudEvent {
    Transcript(SidecarEvent),
    /// Something the user should read; never contains transcript text or keys.
    Notice(String),
    /// A connection could not be kept: the engine for this session is gone.
    Unavailable(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedError {
    /// The queue is full; the frame was not accepted and the caller counts it.
    Full,
    Closed,
}

#[derive(Debug)]
pub struct CloudEngine {
    senders: [Option<mpsc::Sender<PcmFrame>>; 2],
    billed_ms: [Arc<AtomicU64>; 2],
    cancel: CancellationToken,
}

fn slot(speaker: Speaker) -> usize {
    usize::from(speaker == Speaker::Them)
}

impl CloudEngine {
    /// Starts one connection task per speaker. Must run inside a tokio runtime.
    #[must_use]
    pub fn start(
        config: &CloudConfig,
        credentials: Arc<dyn CloudCredentials>,
        speakers: &[Speaker],
    ) -> (Self, mpsc::Receiver<CloudEvent>) {
        let (events_tx, events_rx) = mpsc::channel(EVENT_QUEUE);
        let cancel = CancellationToken::new();
        let billed_ms = [Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0))];
        let mut senders = [None, None];
        for &speaker in speakers {
            let (tx, rx) = mpsc::channel(PCM_QUEUE_FRAMES);
            senders[slot(speaker)] = Some(tx);
            let task = SpeakerTask {
                speaker,
                config: config.clone(),
                credentials: Arc::clone(&credentials),
                events: events_tx.clone(),
                billed_ms: Arc::clone(&billed_ms[slot(speaker)]),
                cancel: cancel.clone(),
                gate: TurnGate::new(config.gate),
                mapper: Mapper::new(speaker),
                resampler: Resampler16To24::new(),
            };
            tokio::spawn(task.run(rx));
        }
        (
            Self {
                senders,
                billed_ms,
                cancel,
            },
            events_rx,
        )
    }

    /// Never waits. A full queue is reported so the caller can count the overflow.
    pub fn try_feed(&self, frame: PcmFrame) -> Result<(), FeedError> {
        let Some(tx) = self.senders[slot(frame.speaker)].as_ref() else {
            return Err(FeedError::Closed);
        };
        tx.try_send(frame).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => FeedError::Full,
            mpsc::error::TrySendError::Closed(_) => FeedError::Closed,
        })
    }

    /// Milliseconds of audio sent to the provider so far, which is what the provider bills.
    #[must_use]
    pub fn billed_ms(&self) -> u64 {
        self.billed_ms
            .iter()
            .map(|b| b.load(Ordering::Relaxed))
            .sum()
    }
}

impl Drop for CloudEngine {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug)]
enum ConnectError {
    /// The server refused the credentials; retrying cannot help.
    Rejected(String),
    Failed(String),
}

#[derive(Debug)]
enum Exit {
    Cancelled,
    /// The socket died or the server closed it.
    Dropped(String),
    /// The server refused our session configuration; retrying cannot help.
    Rejected(String),
}

struct SpeakerTask {
    speaker: Speaker,
    config: CloudConfig,
    credentials: Arc<dyn CloudCredentials>,
    events: mpsc::Sender<CloudEvent>,
    billed_ms: Arc<AtomicU64>,
    cancel: CancellationToken,
    gate: TurnGate,
    mapper: Mapper,
    resampler: Resampler16To24,
}

fn speaker_name(speaker: Speaker) -> &'static str {
    match speaker {
        Speaker::Me => "your microphone",
        Speaker::Them => "the other participants",
    }
}

impl SpeakerTask {
    async fn emit(&self, event: CloudEvent) -> bool {
        self.events.send(event).await.is_ok()
    }

    async fn run(mut self, mut pcm: mpsc::Receiver<PcmFrame>) {
        let mut reconnects_left = 1u8;
        loop {
            let credential = match self.credentials.credential().await {
                Ok(c) => c,
                Err(e) => {
                    let message = match e {
                        CredentialError::Missing => {
                            "No OpenAI key is saved. Add one in Settings > Models".to_string()
                        }
                        other => other.to_string(),
                    };
                    self.emit(CloudEvent::Unavailable(message)).await;
                    return;
                }
            };
            let started = Instant::now();
            let exit = match self.connect(&credential).await {
                Ok(ws) => self.connection(ws, &mut pcm).await,
                Err(ConnectError::Rejected(m)) => Exit::Rejected(m),
                Err(ConnectError::Failed(m)) => Exit::Dropped(m),
            };
            match exit {
                Exit::Cancelled => return,
                Exit::Rejected(reason) => {
                    self.emit(CloudEvent::Unavailable(format!(
                        "Cloud transcription for {} stopped: {reason}",
                        speaker_name(self.speaker)
                    )))
                    .await;
                    return;
                }
                Exit::Dropped(reason) => {
                    tracing::warn!(speaker = ?self.speaker, %reason, "cloud transcription connection lost");
                    let mut open = true;
                    for ev in self.mapper.flush() {
                        open &= self.emit(CloudEvent::Transcript(ev)).await;
                    }
                    self.gate.finish();
                    self.resampler.reset();
                    if !open {
                        return;
                    }
                    if started.elapsed() >= self.config.healthy_after {
                        reconnects_left = 1;
                    }
                    if reconnects_left == 0 {
                        self.emit(CloudEvent::Unavailable(format!(
                            "Cloud transcription for {} lost its connection ({reason}). End the session and start a new one to retry",
                            speaker_name(self.speaker)
                        )))
                        .await;
                        return;
                    }
                    reconnects_left -= 1;
                    tokio::select! {
                        () = self.cancel.cancelled() => return,
                        () = tokio::time::sleep(self.config.reconnect_delay) => {}
                    }
                }
            }
        }
    }

    async fn connect(&self, credential: &Credential) -> Result<Ws, ConnectError> {
        let mut request = self
            .config
            .url
            .as_str()
            .into_client_request()
            .map_err(|e| ConnectError::Rejected(format!("bad endpoint: {e}")))?;
        let mut value =
            HeaderValue::from_str(&format!("Bearer {}", credential.bearer())).map_err(|_| {
                ConnectError::Rejected("the key has characters a header cannot hold".into())
            })?;
        value.set_sensitive(true);
        request.headers_mut().insert("Authorization", value);
        let attempt = timeout(
            self.config.connect_timeout,
            tokio_tungstenite::connect_async(request),
        );
        match attempt.await {
            Err(_) => Err(ConnectError::Failed("connecting timed out".into())),
            Ok(Ok((ws, _))) => Ok(ws),
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(resp))) => {
                let status = resp.status();
                if matches!(status.as_u16(), 401 | 403) {
                    Err(ConnectError::Rejected("OpenAI rejected the API key".into()))
                } else if status.as_u16() == 429 {
                    Err(ConnectError::Rejected(
                        "OpenAI rate limit or quota reached".into(),
                    ))
                } else {
                    Err(ConnectError::Failed(format!("HTTP {status}")))
                }
            }
            Ok(Err(e)) => Err(ConnectError::Failed(e.to_string())),
        }
    }

    async fn send(
        sink: &mut futures::stream::SplitSink<Ws, Message>,
        text: String,
    ) -> Result<(), String> {
        match timeout(SEND_TIMEOUT, sink.send(Message::text(text))).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("sending timed out".into()),
        }
    }

    async fn connection(&mut self, ws: Ws, pcm: &mut mpsc::Receiver<PcmFrame>) -> Exit {
        let (mut sink, mut stream) = ws.split();
        let language = wire::language_hint(&self.config.locale);
        if let Err(e) = Self::send(
            &mut sink,
            wire::session_update(&self.config.model, language.as_deref()),
        )
        .await
        {
            return Exit::Dropped(e);
        }
        if let Some(exit) = self.wait_until_ready(&mut stream).await {
            return exit;
        }
        let mut reported_error = false;
        loop {
            tokio::select! {
                () = self.cancel.cancelled() => return Exit::Cancelled,
                frame = pcm.recv() => {
                    let actions = match frame {
                        Some(f) => self.gate.push(f.start_ms, &f.samples),
                        None => {
                            let actions = self.gate.finish();
                            let _ = self.apply(&mut sink, actions).await;
                            return Exit::Cancelled;
                        }
                    };
                    if let Err(e) = self.apply(&mut sink, actions).await {
                        return Exit::Dropped(e);
                    }
                }
                msg = stream.next() => match msg {
                    Some(Ok(Message::Text(text))) => {
                        let event = wire::parse_server_event(text.as_str());
                        if let ServerEvent::Error { code } = &event {
                            tracing::warn!(speaker = ?self.speaker, %code, "cloud transcription error event");
                            if !reported_error {
                                reported_error = true;
                                let note = format!(
                                    "OpenAI reported an error for {} ({code})",
                                    speaker_name(self.speaker)
                                );
                                if !self.emit(CloudEvent::Notice(note)).await {
                                    return Exit::Cancelled;
                                }
                            }
                        }
                        for ev in self.mapper.on_event(&event) {
                            if !self.emit(CloudEvent::Transcript(ev)).await {
                                return Exit::Cancelled;
                            }
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if let Err(e) = sink.send(Message::Pong(payload)).await {
                            return Exit::Dropped(e.to_string());
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        return Exit::Dropped("the server closed the connection".into());
                    }
                    Some(Err(e)) => return Exit::Dropped(e.to_string()),
                    Some(Ok(_)) => {}
                },
            }
        }
    }

    /// Waits for `session.updated`. A refused configuration is final; a silent server is given
    /// the benefit of the doubt after `ready_timeout`.
    async fn wait_until_ready(
        &self,
        stream: &mut futures::stream::SplitStream<Ws>,
    ) -> Option<Exit> {
        let wait = async {
            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => match wire::parse_server_event(text.as_str()) {
                        ServerEvent::SessionUpdated => return None,
                        ServerEvent::Error { code } => {
                            return Some(Exit::Rejected(format!(
                                "OpenAI refused the session ({code})"
                            )));
                        }
                        _ => {}
                    },
                    Ok(Message::Close(_)) | Err(_) => {
                        return Some(Exit::Dropped("the server closed the connection".into()));
                    }
                    Ok(_) => {}
                }
            }
            Some(Exit::Dropped("the server closed the connection".into()))
        };
        tokio::select! {
            () = self.cancel.cancelled() => Some(Exit::Cancelled),
            r = timeout(self.config.ready_timeout, wait) => r.unwrap_or(None),
        }
    }

    async fn apply(
        &mut self,
        sink: &mut futures::stream::SplitSink<Ws, Message>,
        actions: Vec<GateAction>,
    ) -> Result<(), String> {
        for action in actions {
            match action {
                GateAction::TurnStarted { seq, start_ms } => {
                    self.mapper.turn_started(seq, start_ms)
                }
                GateAction::Audio { start_ms, samples } => {
                    let end_ms = start_ms + samples.len() as u64 * 1_000 / 16_000;
                    let mut converted = Vec::with_capacity(samples.len() * 3 / 2 + 2);
                    self.resampler.push(&samples, &mut converted);
                    for part in converted.chunks(MAX_APPEND_SAMPLES) {
                        Self::send(sink, wire::append(part)).await?;
                        self.billed_ms.fetch_add(
                            part.len() as u64 * 1_000 / u64::from(wire::API_SAMPLE_RATE_HZ),
                            Ordering::Relaxed,
                        );
                    }
                    self.mapper.audio_sent(end_ms);
                }
                GateAction::TurnEnded { seq, end_ms } => {
                    Self::send(sink, wire::commit()).await?;
                    self.mapper.turn_ended(seq, end_ms);
                    self.resampler.reset();
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::cloud_stt::fake_server::{FakeConfig, FakeRealtime};

    #[derive(Debug)]
    struct Fixed(Result<&'static str, CredentialError>);

    #[async_trait]
    impl CloudCredentials for Fixed {
        async fn credential(&self) -> Result<Credential, CredentialError> {
            self.0.clone().map(|t| Credential::new(t, None))
        }
    }

    const KEY: &str = "sk-test-secret-key-value";

    fn config(url: &str) -> CloudConfig {
        CloudConfig {
            url: url.into(),
            reconnect_delay: Duration::from_millis(20),
            ..CloudConfig::new("pt-BR")
        }
    }

    fn frame(speaker: Speaker, start_ms: u64, loud: bool, ms: u64) -> PcmFrame {
        let level = if loud { 6_000 } else { 10 };
        PcmFrame {
            speaker,
            start_ms,
            samples: vec![level; (ms * 16) as usize],
        }
    }

    /// 800 ms of speech followed by enough quiet to end the turn.
    fn feed_one_turn(engine: &CloudEngine, speaker: Speaker, start_ms: u64) {
        let mut t = start_ms;
        for _ in 0..8 {
            engine.try_feed(frame(speaker, t, true, 100)).unwrap();
            t += 100;
        }
        for _ in 0..10 {
            engine.try_feed(frame(speaker, t, false, 100)).unwrap();
            t += 100;
        }
    }

    async fn next(rx: &mut mpsc::Receiver<CloudEvent>) -> CloudEvent {
        timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("timed out waiting for a cloud event")
            .expect("engine stopped")
    }

    fn transcript(e: CloudEvent) -> (u64, u32, u64, u64, String, bool) {
        match e {
            CloudEvent::Transcript(SidecarEvent::Transcript {
                segment_id,
                revision,
                start_ms,
                end_ms,
                text,
                is_final,
                ..
            }) => (segment_id, revision, start_ms, end_ms, text, is_final),
            other => panic!("expected a transcript, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_turn_goes_out_and_comes_back_as_revisions() {
        let server = FakeRealtime::start(FakeConfig {
            turns: vec![vec!["como", "voce", "lidaria"]],
            ..FakeConfig::default()
        })
        .await;
        let (engine, mut rx) = CloudEngine::start(
            &config(&server.url),
            Arc::new(Fixed(Ok(KEY))),
            &[Speaker::Them],
        );
        feed_one_turn(&engine, Speaker::Them, 1_000);

        let mut seen = Vec::new();
        loop {
            let t = transcript(next(&mut rx).await);
            let done = t.5;
            seen.push(t);
            if done {
                break;
            }
        }
        let texts: Vec<&str> = seen.iter().map(|t| t.4.as_str()).collect();
        assert_eq!(
            texts,
            [
                "como",
                "como voce",
                "como voce lidaria",
                "como voce lidaria"
            ]
        );
        assert_eq!(
            seen.iter().map(|t| t.1).collect::<Vec<_>>(),
            [1, 2, 3, 4],
            "revisions grow"
        );
        assert!(seen.iter().all(|t| t.0 == 0 && t.2 == 1_000));
        assert!(!seen[2].5 && seen[3].5);
        // 800 ms speech + 700 ms hangover on the sidecar clock.
        assert_eq!(seen[3].3, 2_500);

        let rec = server.recorded();
        assert_eq!(rec.connections, 1);
        assert_eq!(rec.auth_headers, [format!("Bearer {KEY}")]);
        let input = &rec.session_updates[0]["session"]["audio"]["input"];
        assert_eq!(input["transcription"]["languages"], json!(["pt"]));
        assert_eq!(input["format"]["rate"], 24_000);
        assert!(input["turn_detection"].is_null());
        assert_eq!(rec.commits, 1);
        // Only the turn is sent, not the silence before or after it.
        assert!(
            (1_450..=1_550).contains(&rec.appended_ms()),
            "{}",
            rec.appended_ms()
        );
        assert!(engine.billed_ms().abs_diff(rec.appended_ms()) <= 2);
    }

    #[tokio::test]
    async fn silence_alone_sends_no_audio_and_costs_nothing() {
        let server = FakeRealtime::start(FakeConfig {
            turns: vec![vec!["marker"]],
            ..FakeConfig::default()
        })
        .await;
        let (engine, mut rx) = CloudEngine::start(
            &config(&server.url),
            Arc::new(Fixed(Ok(KEY))),
            &[Speaker::Me, Speaker::Them],
        );
        for i in 0..50 {
            engine
                .try_feed(frame(Speaker::Me, i * 100, false, 100))
                .unwrap();
        }
        // The queue drains once the connection is up; wait for it by feeding a marker turn.
        feed_one_turn(&engine, Speaker::Them, 10_000);
        let _ = next(&mut rx).await;
        let rec = server.recorded();
        assert_eq!(rec.connections, 2, "one connection per speaker");
        assert!(rec.appended_ms() < 1_600, "{}", rec.appended_ms());
    }

    #[tokio::test]
    async fn a_dropped_socket_reconnects_once_then_gives_up() {
        let server = FakeRealtime::start(FakeConfig {
            turns: vec![vec!["one", "two"]],
            drop_after_appends: Some(1),
            ..FakeConfig::default()
        })
        .await;
        let (engine, mut rx) = CloudEngine::start(
            &config(&server.url),
            Arc::new(Fixed(Ok(KEY))),
            &[Speaker::Them],
        );
        // One loud frame is one append, so the server drops the socket right after its delta.
        let one_frame = |start_ms| frame(Speaker::Them, start_ms, true, 100);
        engine.try_feed(one_frame(0)).unwrap();
        let partial = transcript(next(&mut rx).await);
        assert_eq!((partial.4.as_str(), partial.5), ("one", false));
        // The dead connection's partial text is finalized so nothing stays partial forever.
        let flushed = transcript(next(&mut rx).await);
        assert_eq!((flushed.4.as_str(), flushed.5), ("one", true));
        // The reconnected socket takes the next audio and dies the same way.
        engine.try_feed(one_frame(5_000)).unwrap();
        let mut unavailable = None;
        while unavailable.is_none() {
            if let CloudEvent::Unavailable(m) = next(&mut rx).await {
                unavailable = Some(m);
            }
        }
        let message = unavailable.unwrap_or_default();
        assert!(message.contains("lost its connection"), "{message}");
        assert!(!message.contains(KEY));
        assert_eq!(server.recorded().connections, 2);
    }

    #[tokio::test]
    async fn a_rejected_key_is_reported_without_retrying() {
        let server = FakeRealtime::start(FakeConfig {
            reject_with: Some(401),
            ..FakeConfig::default()
        })
        .await;
        let (_engine, mut rx) = CloudEngine::start(
            &config(&server.url),
            Arc::new(Fixed(Ok(KEY))),
            &[Speaker::Them],
        );
        match next(&mut rx).await {
            CloudEvent::Unavailable(m) => {
                assert!(m.contains("rejected the API key"), "{m}");
                assert!(!m.contains(KEY));
            }
            other => panic!("expected unavailable, got {other:?}"),
        }
        assert_eq!(server.recorded().connections, 1);
    }

    #[tokio::test]
    async fn a_missing_credential_never_connects() {
        let server = FakeRealtime::start(FakeConfig::default()).await;
        let (_engine, mut rx) = CloudEngine::start(
            &config(&server.url),
            Arc::new(Fixed(Err(CredentialError::Missing))),
            &[Speaker::Me],
        );
        match next(&mut rx).await {
            CloudEvent::Unavailable(m) => assert!(m.contains("No OpenAI key"), "{m}"),
            other => panic!("expected unavailable, got {other:?}"),
        }
        assert_eq!(server.recorded().connections, 0);
    }

    #[tokio::test]
    async fn a_wss_endpoint_gets_a_tls_client_and_a_failed_handshake_ends_in_a_notice() {
        // The peer answers the TCP connection with nothing and hangs up, so the TLS client
        // config must be built (no crypto provider panic) and the handshake must fail cleanly.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("wss://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                drop(stream);
            }
        });
        let (_engine, mut rx) =
            CloudEngine::start(&config(&url), Arc::new(Fixed(Ok(KEY))), &[Speaker::Them]);
        match next(&mut rx).await {
            CloudEvent::Unavailable(m) => assert!(m.contains("lost its connection"), "{m}"),
            other => panic!("expected unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_full_queue_is_reported_not_dropped_silently() {
        let (tx, _rx) = mpsc::channel::<PcmFrame>(1);
        let engine = CloudEngine {
            senders: [Some(tx), None],
            billed_ms: [Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0))],
            cancel: CancellationToken::new(),
        };
        let f = || frame(Speaker::Me, 0, false, 20);
        assert_eq!(engine.try_feed(f()), Ok(()));
        assert_eq!(engine.try_feed(f()), Err(FeedError::Full));
        assert_eq!(
            engine.try_feed(frame(Speaker::Them, 0, false, 20)),
            Err(FeedError::Closed)
        );
    }
}
