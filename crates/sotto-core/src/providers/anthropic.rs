//! Anthropic Messages API over raw HTTP with SSE streaming.

use std::fmt;

use async_trait::async_trait;
use base64::Engine as _;
use reqwest::Client;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::wire::{self, LineParser, Parsed};
use super::{
    CompletionRequest, EventStream, LlmProvider, ProviderError, Result, StreamEvent, Thinking,
    Usage,
};

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
pub const API_VERSION: &str = "2026-10-21";

pub struct AnthropicProvider {
    client: Client,
    api_key: String,
    base_url: String,
}

impl fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        Ok(Self {
            client: wire::build_client()?,
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
        })
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }
}

/// The shortest prefix the API caches for the current models (512 tokens for the 5.5 models,
/// per the API docs cached on 2026-10-06). Unknown models get the largest documented minimum.
#[must_use]
pub fn min_cacheable_tokens(model: &str) -> usize {
    match model {
        "claude-haiku-5-5" | "claude-sonnet-5-5" | "claude-opus-5-5" => 512,
        _ => 4_096,
    }
}

/// Rough token count for a cache decision: about four bytes per token, so it overestimates
/// for Portuguese and English text, which keeps a breakpoint off a prefix too short to cache.
fn estimated_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// No `temperature`, `top_p`, `budget_tokens` or assistant prefill: the 5.5 models reject them.
/// System blocks are the system prefix, the mode context, the final transcript lines and the
/// rest. Breakpoints go on the last stable block before the transcript and on the final-lines
/// block, each only when the prefix up to it reaches the model's cache minimum, so a speculation
/// restarted within a turn reuses the cached transcript.
pub(crate) fn build_body(req: &CompletionRequest) -> Value {
    let min = min_cacheable_tokens(&req.model);
    let mut system = Vec::new();
    let mut prefix_tokens = 0;
    let mut stable_breakpoint = None;
    for text in [&req.system_prefix, &req.mode_context] {
        if !text.is_empty() {
            prefix_tokens += estimated_tokens(text);
            system.push(json!({"type": "text", "text": text}));
            stable_breakpoint = Some(system.len() - 1);
        }
    }
    let mut breakpoints = Vec::new();
    if prefix_tokens >= min {
        breakpoints.extend(stable_breakpoint);
    }
    if !req.history.is_empty() {
        prefix_tokens += estimated_tokens(&req.history);
        system.push(json!({"type": "text", "text": req.history}));
        if prefix_tokens >= min {
            breakpoints.push(system.len() - 1);
        }
    }
    if !req.recent.is_empty() {
        system.push(json!({"type": "text", "text": req.recent}));
    }
    for i in breakpoints {
        system[i]["cache_control"] = json!({"type": "ephemeral"});
    }

    let mut messages: Vec<Value> = req
        .turns
        .iter()
        .map(|t| json!({"role": t.role.as_str(), "content": t.text}))
        .collect();
    let mut content = Vec::new();
    if let Some(png) = &req.image_png {
        content.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": base64::engine::general_purpose::STANDARD.encode(png),
            }
        }));
    }
    content.push(json!({"type": "text", "text": req.user_text}));
    messages.push(json!({"role": "user", "content": content}));

    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "stream": true,
        "system": system,
        "messages": messages,
    });
    if let Some(effort) = req.effort {
        body["output_config"] = json!({"effort": effort.as_str()});
    }
    if req.thinking == Thinking::Disabled {
        body["thinking"] = json!({"type": "disabled"});
    }
    body
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn stream(
        &self,
        req: CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream> {
        let request = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&build_body(&req));
        let resp = wire::send(request, &cancel).await?;
        tracing::debug!(provider = "anthropic", model = %req.model, "stream opened");
        Ok(wire::line_stream(resp, cancel, SseParser::default()))
    }
}

#[derive(Default)]
pub(crate) struct SseParser {
    /// The latest totals the API reported.
    usage: Usage,
    /// What earlier `Usage` events already carried.
    reported: Usage,
    saw_text: bool,
}

impl SseParser {
    fn push_usage(&mut self, out: &mut Parsed) {
        let delta = Usage {
            input_tokens: self
                .usage
                .input_tokens
                .saturating_sub(self.reported.input_tokens),
            output_tokens: self
                .usage
                .output_tokens
                .saturating_sub(self.reported.output_tokens),
            cache_read_input_tokens: self
                .usage
                .cache_read_input_tokens
                .saturating_sub(self.reported.cache_read_input_tokens),
            cache_creation_input_tokens: self
                .usage
                .cache_creation_input_tokens
                .saturating_sub(self.reported.cache_creation_input_tokens),
        };
        if delta != Usage::default() {
            self.reported = self.usage;
            out.items.push(Ok(StreamEvent::from_usage(delta)));
        }
    }

    fn read_usage(&mut self, u: &Value) {
        let get = |k: &str| u.get(k).and_then(Value::as_u64);
        if let Some(v) = get("input_tokens") {
            self.usage.input_tokens = v;
        }
        if let Some(v) = get("output_tokens") {
            self.usage.output_tokens = v;
        }
        if let Some(v) = get("cache_read_input_tokens") {
            self.usage.cache_read_input_tokens = v;
        }
        if let Some(v) = get("cache_creation_input_tokens") {
            self.usage.cache_creation_input_tokens = v;
        }
    }
}

impl LineParser for SseParser {
    fn line(&mut self, line: &str) -> Parsed {
        let Some(data) = line.strip_prefix("data:") else {
            return Parsed::default();
        };
        let event: Value = match serde_json::from_str(data.trim()) {
            Ok(v) => v,
            Err(e) => {
                return Parsed {
                    items: vec![Err(ProviderError::Protocol(format!("bad SSE JSON: {e}")))],
                    finished: true,
                };
            }
        };
        let mut out = Parsed::default();
        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(u) = event.pointer("/message/usage") {
                    self.read_usage(u);
                    self.push_usage(&mut out);
                }
            }
            Some("content_block_delta") => {
                if event.pointer("/delta/type").and_then(Value::as_str) == Some("text_delta")
                    && let Some(t) = event.pointer("/delta/text").and_then(Value::as_str)
                    && !t.is_empty()
                {
                    self.saw_text = true;
                    out.items.push(Ok(StreamEvent::TextDelta(t.to_string())));
                }
            }
            Some("message_delta") => {
                if let Some(u) = event.get("usage") {
                    self.read_usage(u);
                }
                if let Some(reason) = event.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.push_usage(&mut out);
                    if reason == "max_tokens" && !self.saw_text {
                        out.items.push(Err(ProviderError::NoTextAtTokenLimit));
                        out.finished = true;
                    } else if reason == "refusal" {
                        let category = event
                            .pointer("/delta/stop_details/category")
                            .or_else(|| event.pointer("/stop_details/category"))
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        out.items.push(Err(ProviderError::Refusal { category }));
                        out.finished = true;
                    } else {
                        out.items.push(Ok(StreamEvent::Done {
                            stop_reason: reason.to_string(),
                        }));
                    }
                }
            }
            Some("message_stop") => out.finished = true,
            Some("error") => {
                let kind = event
                    .pointer("/error/type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let message = event
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                out.items.push(Err(ProviderError::Stream {
                    kind: kind.to_string(),
                    message: message.chars().take(300).collect(),
                }));
                out.finished = true;
            }
            _ => {}
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use futures::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::providers::{Effort, Turn};

    /// Long enough to pass the 512-token cache minimum on its own.
    fn long(prefix: &str) -> String {
        format!("{prefix} {}", "word ".repeat(600))
    }

    const OK_BODY: &str = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":12,\"cache_read_input_tokens\":300,\"cache_creation_input_tokens\":40,\"output_tokens\":1}}}

event: content_block_start
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hmm\"}}

event: content_block_start
data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}

event: content_block_stop
data: {\"type\":\"content_block_stop\",\"index\":1}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":9}}

event: message_stop
data: {\"type\":\"message_stop\"}

";

    const REFUSAL_BODY: &str = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\",\"stop_details\":{\"category\":\"cyber\"}},\"usage\":{\"output_tokens\":0}}

event: message_stop
data: {\"type\":\"message_stop\"}

";

    fn request() -> CompletionRequest {
        CompletionRequest {
            model: "claude-haiku-5-5".into(),
            system_prefix: "You are Sotto.".into(),
            mode_context: long("Interview mode."),
            history: String::new(),
            recent: String::new(),
            turns: vec![Turn::user("earlier"), Turn::assistant("reply")],
            user_text: "Why Rust?".into(),
            image_png: Some(vec![1, 2, 3]),
            max_tokens: 512,
            effort: Some(Effort::Low),
            thinking: Thinking::Adaptive,
        }
    }

    fn provider(server: &MockServer) -> AnthropicProvider {
        AnthropicProvider::new("sk-ant-test")
            .unwrap()
            .with_base_url(server.uri())
    }

    async fn collect(mut s: EventStream) -> Vec<Result<StreamEvent>> {
        let mut out = Vec::new();
        while let Some(i) = s.next().await {
            out.push(i);
        }
        out
    }

    fn sse(body: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "text/event-stream")
    }

    #[test]
    fn body_shape_follows_the_documented_rules() {
        let b = build_body(&request());
        assert_eq!(b["stream"], true);
        assert_eq!(b["output_config"], json!({"effort": "low"}));
        // Omitted thinking means adaptive on Haiku 5.5, which the answers want.
        for banned in ["temperature", "top_p", "thinking", "top_k"] {
            assert!(b.get(banned).is_none());
        }
        assert!(!b.to_string().contains("budget_tokens"));
        let system = b["system"].as_array().unwrap();
        assert!(system[0].get("cache_control").is_none());
        assert_eq!(system[1]["cache_control"], json!({"type": "ephemeral"}));
        let msgs = b["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"][0]["type"], "image");
        assert_eq!(msgs[2]["content"][0]["source"]["data"], "AQID");
        assert_eq!(msgs[2]["content"][1]["text"], "Why Rust?");
    }

    #[test]
    fn classifier_disables_thinking_without_a_budget() {
        let mut r = request();
        r.thinking = Thinking::Disabled;
        r.max_tokens = 256;
        let b = build_body(&r);
        assert_eq!(b["thinking"], json!({"type": "disabled"}));
        assert_eq!(b["output_config"], json!({"effort": "low"}));
        assert_eq!(b["max_tokens"], 256);
        assert!(!b.to_string().contains("budget_tokens"));
    }

    #[test]
    fn final_transcript_lines_get_their_own_breakpoint() {
        let mut r = request();
        r.mode_context.clear();
        r.history = long("Transcript so far:");
        r.recent = "Them: como voce".into();
        let b = build_body(&r);
        let system = b["system"].as_array().unwrap();
        assert_eq!(system.len(), 3);
        // The system prefix alone is under the minimum, so only the history is marked.
        assert!(system[0].get("cache_control").is_none());
        assert_eq!(system[1]["cache_control"], json!({"type": "ephemeral"}));
        assert_eq!(system[2]["text"], "Them: como voce");
        assert!(system[2].get("cache_control").is_none());

        r.system_prefix = long("You are Sotto.");
        let b = build_body(&r);
        assert_eq!(
            b["system"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert_eq!(
            b["system"][1]["cache_control"],
            json!({"type": "ephemeral"})
        );
    }

    #[test]
    fn short_prompts_are_not_marked_for_caching() {
        let mut r = request();
        r.mode_context = "Interview mode.".into();
        r.history = "Transcript so far:\nThem: oi\n".into();
        let b = build_body(&r);
        assert!(!b.to_string().contains("cache_control"));
        assert_eq!(min_cacheable_tokens("claude-haiku-5-5"), 512);
        assert_eq!(min_cacheable_tokens("claude-unknown"), 4_096);
    }

    #[test]
    fn cache_breakpoint_falls_back_to_system_prefix() {
        let mut r = request();
        r.mode_context.clear();
        r.system_prefix = long("You are Sotto.");
        r.effort = None;
        let b = build_body(&r);
        assert_eq!(b["system"].as_array().unwrap().len(), 1);
        assert_eq!(
            b["system"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert!(b.get("output_config").is_none());
    }

    #[tokio::test]
    async fn streams_text_usage_and_done() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "sk-ant-test"))
            .and(header("anthropic-version", API_VERSION))
            .respond_with(sse(OK_BODY))
            .mount(&server)
            .await;
        let (stream, ttft) = provider(&server)
            .stream_timed(request(), CancellationToken::new())
            .await
            .unwrap();
        let items: Vec<_> = collect(stream)
            .await
            .into_iter()
            .collect::<Result<_>>()
            .unwrap();
        // Input is reported at message_start, so a stream cut after it has still billed it.
        assert_eq!(
            items,
            vec![
                StreamEvent::Usage {
                    input_tokens: 12,
                    output_tokens: 1,
                    cache_read_input_tokens: 300,
                    cache_creation_input_tokens: 40,
                },
                StreamEvent::TextDelta("Hello".into()),
                StreamEvent::TextDelta(" world".into()),
                StreamEvent::Usage {
                    input_tokens: 0,
                    output_tokens: 8,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                },
                StreamEvent::Done {
                    stop_reason: "end_turn".into()
                },
            ]
        );
        assert!(ttft.time_to_first_token().is_some());
    }

    #[tokio::test]
    async fn refusal_becomes_an_error_with_category() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(sse(REFUSAL_BODY))
            .mount(&server)
            .await;
        let stream = provider(&server)
            .stream(request(), CancellationToken::new())
            .await
            .unwrap();
        let items = collect(stream).await;
        assert!(
            matches!(items.last(), Some(Err(ProviderError::Refusal { category })) if category.as_deref() == Some("cyber"))
        );
        assert!(
            items
                .iter()
                .all(|i| !matches!(i, Ok(StreamEvent::Done { .. })))
        );
    }

    #[tokio::test]
    async fn max_tokens_with_no_text_is_an_error() {
        let server = MockServer::start().await;
        let body = "\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":50,\"output_tokens\":1}}}

data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"\"}}

data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"output_tokens\":256}}

data: {\"type\":\"message_stop\"}

";
        Mock::given(method("POST"))
            .respond_with(sse(body))
            .mount(&server)
            .await;
        let stream = provider(&server)
            .stream(request(), CancellationToken::new())
            .await
            .unwrap();
        let items = collect(stream).await;
        assert_eq!(items.last(), Some(&Err(ProviderError::NoTextAtTokenLimit)));
        assert!(
            !items
                .iter()
                .any(|i| matches!(i, Ok(StreamEvent::Done { .. })))
        );
        let billed: u64 = items
            .iter()
            .filter_map(|i| i.as_ref().ok().and_then(StreamEvent::usage))
            .map(|u| u.output_tokens)
            .sum();
        assert_eq!(billed, 256);
    }

    #[tokio::test]
    async fn a_stalled_stream_ends_with_an_idle_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0u8; 65536];
            let _ = sock.read(&mut buf).await;
            let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
            let first = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n";
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock
                .write_all(format!("{:x}\r\n{first}\r\n", first.len()).as_bytes())
                .await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/v1/messages"))
            .send()
            .await
            .unwrap();
        let t = Instant::now();
        let stream = wire::line_stream_with_idle(
            resp,
            CancellationToken::new(),
            SseParser::default(),
            Duration::from_millis(200),
        );
        let items = collect(stream).await;
        assert_eq!(items[0], Ok(StreamEvent::TextDelta("Hi".into())));
        assert!(matches!(items[1], Err(ProviderError::IdleTimeout { .. })));
        assert!(t.elapsed() < Duration::from_secs(5));
        assert_eq!(wire::STREAM_IDLE_TIMEOUT, Duration::from_secs(30));
    }

    #[tokio::test]
    async fn mid_stream_error_event_is_reported() {
        let server = MockServer::start().await;
        let body = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        Mock::given(method("POST"))
            .respond_with(sse(body))
            .mount(&server)
            .await;
        let stream = provider(&server)
            .stream(request(), CancellationToken::new())
            .await
            .unwrap();
        let items = collect(stream).await;
        assert!(
            matches!(&items[0], Err(ProviderError::Stream { kind, .. }) if kind == "overloaded_error")
        );
    }

    #[tokio::test]
    async fn truncated_stream_is_an_error() {
        let server = MockServer::start().await;
        let body = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n";
        Mock::given(method("POST"))
            .respond_with(sse(body))
            .mount(&server)
            .await;
        let stream = provider(&server)
            .stream(request(), CancellationToken::new())
            .await
            .unwrap();
        let items = collect(stream).await;
        assert_eq!(items.len(), 2);
        assert!(matches!(items[1], Err(ProviderError::Protocol(_))));
    }

    #[tokio::test]
    async fn http_errors_are_typed() {
        let server = MockServer::start().await;
        let p = provider(&server);
        for (status, retry) in [(401, None), (429, Some("7")), (503, None), (400, None)] {
            server.reset().await;
            let mut resp = ResponseTemplate::new(status).set_body_string(
                "{\"type\":\"error\",\"error\":{\"type\":\"x\",\"message\":\"bad\"}}",
            );
            if let Some(r) = retry {
                resp = resp.insert_header("retry-after", r);
            }
            Mock::given(method("POST"))
                .respond_with(resp)
                .mount(&server)
                .await;
            let err = p
                .stream(request(), CancellationToken::new())
                .await
                .err()
                .unwrap();
            match status {
                401 => assert_eq!(err, ProviderError::Unauthorized),
                429 => assert_eq!(
                    err,
                    ProviderError::RateLimited {
                        retry_after: Some(Duration::from_secs(7))
                    }
                ),
                503 => assert_eq!(err, ProviderError::Server { status: 503 }),
                _ => assert_eq!(
                    err,
                    ProviderError::Http {
                        status: 400,
                        message: "bad".into()
                    }
                ),
            }
        }
    }

    #[tokio::test]
    async fn cancellation_stops_reading_and_drops_the_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0u8; 65536];
            let _ = sock.read(&mut buf).await;
            let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
            let first = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n";
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock
                .write_all(format!("{:x}\r\n{first}\r\n", first.len()).as_bytes())
                .await;
            // Never send more; wait for the client to hang up.
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let _ = closed_tx.send(n == 0);
        });

        let p = AnthropicProvider::new("k")
            .unwrap()
            .with_base_url(format!("http://{addr}"));
        let cancel = CancellationToken::new();
        let mut stream = p.stream(request(), cancel.clone()).await.unwrap();
        assert_eq!(
            stream.next().await.unwrap().unwrap(),
            StreamEvent::TextDelta("Hi".into())
        );

        let t = Instant::now();
        cancel.cancel();
        assert!(stream.next().await.is_none());
        assert!(t.elapsed() < Duration::from_millis(500));
        let closed = tokio::time::timeout(Duration::from_secs(2), closed_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(closed, "server should see the connection closed");
    }

    #[tokio::test]
    async fn cancelling_before_the_response_headers_returns_cancelled() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(sse(OK_BODY).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            c2.cancel();
        });
        let t = Instant::now();
        let err = provider(&server)
            .stream(request(), cancel)
            .await
            .err()
            .unwrap();
        assert_eq!(err, ProviderError::Cancelled);
        assert!(t.elapsed() < Duration::from_secs(1));
    }
}
