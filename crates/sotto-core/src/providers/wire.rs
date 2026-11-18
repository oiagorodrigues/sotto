//! HTTP plumbing shared by the providers: status mapping and the line-oriented stream driver.

use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use super::{EventStream, ProviderError, Result, StreamEvent};

/// A single SSE or NDJSON line may carry a large JSON object, but never this much.
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 16 * 1024;
/// A stalled connection must not hold an answer in Streaming forever. Anthropic sends pings
/// and models stream thinking events, so a healthy stream is never this quiet.
pub(crate) const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) fn build_client() -> Result<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| ProviderError::Network(e.to_string()))
}

/// Sends the request, abandoning it promptly if `cancel` fires, and maps HTTP errors.
pub(crate) async fn send(req: RequestBuilder, cancel: &CancellationToken) -> Result<Response> {
    let resp = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(ProviderError::Cancelled),
        r = req.send() => r.map_err(|e| ProviderError::Network(e.without_url().to_string()))?,
    };
    check_status(resp).await
}

pub(crate) async fn check_status(resp: Response) -> Result<Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let code = status.as_u16();
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(ProviderError::Unauthorized),
        StatusCode::TOO_MANY_REQUESTS => Err(ProviderError::RateLimited { retry_after }),
        s if s.is_server_error() => Err(ProviderError::Server { status: code }),
        _ => {
            let body = resp.bytes().await.unwrap_or_default();
            let body = &body[..body.len().min(MAX_ERROR_BODY_BYTES)];
            Err(ProviderError::Http {
                status: code,
                message: api_error_message(body),
            })
        }
    }
}

fn api_error_message(body: &[u8]) -> String {
    let parsed: Option<serde_json::Value> = serde_json::from_slice(body).ok();
    let msg = parsed.as_ref().and_then(|v| {
        v.pointer("/error/message")
            .or_else(|| v.get("error"))
            .and_then(|m| m.as_str())
            .map(str::to_owned)
    });
    let msg = msg.unwrap_or_else(|| "no error message".to_string());
    msg.chars().take(300).collect()
}

pub(crate) type Item = Result<StreamEvent>;

#[derive(Default)]
pub(crate) struct Parsed {
    pub items: Vec<Item>,
    /// No further input is needed; the driver closes the connection.
    pub finished: bool,
}

pub(crate) trait LineParser: Send + 'static {
    fn line(&mut self, line: &str) -> Parsed;
}

/// Reads the response body line by line on a background task and yields parsed events.
/// Cancelling the token, dropping the returned stream, or a finished parser all drop the
/// response, which closes the connection.
pub(crate) fn line_stream<P: LineParser>(
    resp: Response,
    cancel: CancellationToken,
    parser: P,
) -> EventStream {
    line_stream_with_idle(resp, cancel, parser, STREAM_IDLE_TIMEOUT)
}

/// [`line_stream`] with the gap allowed between two chunks of the body.
pub(crate) fn line_stream_with_idle<P: LineParser>(
    resp: Response,
    cancel: CancellationToken,
    mut parser: P,
    idle: Duration,
) -> EventStream {
    let (tx, rx) = mpsc::channel::<Item>(64);
    tokio::spawn(async move {
        let mut body: BoxStream<'_, reqwest::Result<bytes::Bytes>> = resp.bytes_stream().boxed();
        let mut buf: Vec<u8> = Vec::new();
        loop {
            let chunk = tokio::select! {
                biased;
                () = cancel.cancelled() => return,
                () = tx.closed() => return,
                c = tokio::time::timeout(idle, body.next()) => c,
            };
            let Ok(chunk) = chunk else {
                let _ = tx
                    .send(Err(ProviderError::IdleTimeout {
                        secs: idle.as_secs(),
                    }))
                    .await;
                return;
            };
            match chunk {
                None => {
                    let _ = tx
                        .send(Err(ProviderError::Protocol(
                            "stream ended before completion".into(),
                        )))
                        .await;
                    return;
                }
                Some(Err(e)) => {
                    let _ = tx
                        .send(Err(ProviderError::Network(e.without_url().to_string())))
                        .await;
                    return;
                }
                Some(Ok(bytes)) => buf.extend_from_slice(&bytes),
            }
            while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                let raw: Vec<u8> = buf.drain(..=pos).collect();
                let text = String::from_utf8_lossy(&raw);
                let parsed = parser.line(text.trim_end_matches(['\r', '\n']));
                for item in parsed.items {
                    if tx.send(item).await.is_err() {
                        return;
                    }
                }
                if parsed.finished {
                    return;
                }
            }
            if buf.len() > MAX_LINE_BYTES {
                let _ = tx
                    .send(Err(ProviderError::Protocol("stream line too long".into())))
                    .await;
                return;
            }
        }
    });
    ReceiverStream::new(rx).boxed()
}
