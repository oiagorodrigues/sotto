//! A local stand-in for the OpenAI Realtime endpoint, for tests. It speaks just enough of the
//! protocol: it answers `session.update`, turns each `append` into one scripted word, and on
//! `commit` reports the item and its completed text.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::Message;

#[derive(Debug, Default, Clone)]
pub(crate) struct Recorded {
    pub connections: usize,
    pub auth_headers: Vec<String>,
    pub session_updates: Vec<Value>,
    /// Samples (24 kHz) in each append, in arrival order across connections.
    pub append_samples: Vec<usize>,
    pub commits: usize,
}

impl Recorded {
    pub fn appended_ms(&self) -> u64 {
        self.append_samples.iter().sum::<usize>() as u64 * 1_000 / 24_000
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct FakeConfig {
    /// The words of each turn, one per append; turns are used in order across connections.
    pub turns: Vec<Vec<&'static str>>,
    /// Close each connection (without a close frame) after this many appends.
    pub drop_after_appends: Option<usize>,
    /// Refuse the WebSocket handshake with this status.
    pub reject_with: Option<u16>,
}

pub(crate) struct FakeRealtime {
    pub url: String,
    pub recorded: Arc<Mutex<Recorded>>,
}

impl FakeRealtime {
    pub async fn start(config: FakeConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let recorded = Arc::new(Mutex::new(Recorded::default()));
        let turn = Arc::new(AtomicUsize::new(0));
        let rec = Arc::clone(&recorded);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(serve(
                    stream,
                    config.clone(),
                    Arc::clone(&rec),
                    Arc::clone(&turn),
                ));
            }
        });
        Self { url, recorded }
    }

    pub fn recorded(&self) -> Recorded {
        self.recorded.lock().unwrap().clone()
    }
}

async fn serve(
    stream: tokio::net::TcpStream,
    config: FakeConfig,
    recorded: Arc<Mutex<Recorded>>,
    turn: Arc<AtomicUsize>,
) {
    let callback = Handshake {
        recorded: Arc::clone(&recorded),
        reject: config.reject_with,
    };
    let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(stream, callback).await else {
        return;
    };
    let mut item: Option<String> = None;
    let mut words_in_turn: Vec<&'static str> = Vec::new();
    let mut appends_here = 0usize;
    let mut appends_in_turn = 0usize;
    let mut item_counter = 0usize;
    while let Some(Ok(msg)) = ws.next().await {
        let Message::Text(text) = msg else { continue };
        let v: Value = serde_json::from_str(text.as_str()).unwrap();
        match v["type"].as_str().unwrap_or_default() {
            "session.update" => {
                recorded.lock().unwrap().session_updates.push(v.clone());
                let _ = ws
                    .send(Message::text(
                        json!({"type": "session.updated"}).to_string(),
                    ))
                    .await;
            }
            "input_audio_buffer.append" => {
                let bytes = v["audio"].as_str().unwrap().len() / 4 * 3;
                recorded.lock().unwrap().append_samples.push(bytes / 2);
                if item.is_none() {
                    item_counter += 1;
                    item = Some(format!("item_{item_counter}"));
                    let idx = turn.load(Ordering::SeqCst);
                    words_in_turn = config.turns.get(idx).cloned().unwrap_or_default();
                    appends_in_turn = 0;
                }
                if let Some(word) = words_in_turn.get(appends_in_turn) {
                    let delta = if appends_in_turn == 0 {
                        (*word).to_string()
                    } else {
                        format!(" {word}")
                    };
                    let event = json!({
                        "type": "conversation.item.input_audio_transcription.delta",
                        "item_id": item.clone().unwrap(),
                        "content_index": 0,
                        "delta": delta,
                    });
                    let _ = ws.send(Message::text(event.to_string())).await;
                }
                appends_in_turn += 1;
                appends_here += 1;
                if config.drop_after_appends == Some(appends_here) {
                    return;
                }
            }
            "input_audio_buffer.commit" => {
                recorded.lock().unwrap().commits += 1;
                let Some(id) = item.take() else { continue };
                turn.fetch_add(1, Ordering::SeqCst);
                let _ = ws
                    .send(Message::text(
                        json!({"type": "input_audio_buffer.committed", "item_id": id}).to_string(),
                    ))
                    .await;
                let full = words_in_turn.join(" ");
                let _ = ws
                    .send(Message::text(
                        json!({
                            "type": "conversation.item.input_audio_transcription.completed",
                            "item_id": id,
                            "content_index": 0,
                            "transcript": full,
                        })
                        .to_string(),
                    ))
                    .await;
            }
            _ => {}
        }
    }
}

struct Handshake {
    recorded: Arc<Mutex<Recorded>>,
    reject: Option<u16>,
}

impl Callback for Handshake {
    fn on_request(self, req: &Request, resp: Response) -> Result<Response, ErrorResponse> {
        let auth = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        {
            let mut r = self.recorded.lock().unwrap();
            r.connections += 1;
            r.auth_headers.push(auth);
        }
        match self.reject {
            Some(code) => {
                let mut err = ErrorResponse::new(None);
                *err.status_mut() = StatusCode::from_u16(code).unwrap();
                Err(err)
            }
            None => Ok(resp),
        }
    }
}
