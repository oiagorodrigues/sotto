//! OpenAI Chat Completions streaming.

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

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com";

pub struct OpenAiProvider {
    client: Client,
    api_key: String,
    base_url: String,
}

impl fmt::Debug for OpenAiProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiProvider")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl OpenAiProvider {
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

/// Reasoning models spend hidden reasoning tokens from `max_completion_tokens`; below this a
/// short request (the classifier) can end with no visible text.
pub const MIN_REASONING_COMPLETION_TOKENS: u32 = 512;

/// `gpt-5*` and the `o<digit>` series take `reasoning_effort`; `gpt-4o` rejects it.
#[must_use]
pub fn is_reasoning_model(model: &str) -> bool {
    let mut chars = model.chars();
    model.starts_with("gpt-5")
        || (chars.next() == Some('o') && chars.next().is_some_and(|c| c.is_ascii_digit()))
}

/// OpenAI caches long prompt prefixes automatically, so the stable parts only need to come
/// first and stay byte-identical.
pub(crate) fn build_body(req: &CompletionRequest) -> Value {
    let context = req.context_text();
    let system = [req.system_prefix.as_str(), context.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut messages = Vec::new();
    if !system.is_empty() {
        messages.push(json!({"role": "system", "content": system}));
    }
    for t in &req.turns {
        messages.push(json!({"role": t.role.as_str(), "content": t.text}));
    }
    let user = match &req.image_png {
        None => json!({"role": "user", "content": req.user_text}),
        Some(png) => {
            let url = format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(png)
            );
            json!({"role": "user", "content": [
                {"type": "text", "text": req.user_text},
                {"type": "image_url", "image_url": {"url": url}},
            ]})
        }
    };
    messages.push(user);
    let reasoning = is_reasoning_model(&req.model);
    let max_tokens = if reasoning {
        req.max_tokens.max(MIN_REASONING_COMPLETION_TOKENS)
    } else {
        req.max_tokens
    };
    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "max_completion_tokens": max_tokens,
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if reasoning {
        let effort = match req.thinking {
            Thinking::Disabled => Some("minimal"),
            Thinking::Adaptive => req.effort.map(super::Effort::as_str),
        };
        if let Some(effort) = effort {
            body["reasoning_effort"] = json!(effort);
        }
    }
    body
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn stream(
        &self,
        req: CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream> {
        let request = self
            .client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&build_body(&req));
        let resp = wire::send(request, &cancel).await?;
        tracing::debug!(provider = "openai", model = %req.model, "stream opened");
        Ok(wire::line_stream(resp, cancel, ChunkParser::default()))
    }
}

#[derive(Default)]
struct ChunkParser {
    finish_reason: Option<String>,
    saw_text: bool,
}

impl LineParser for ChunkParser {
    fn line(&mut self, line: &str) -> Parsed {
        let Some(data) = line.strip_prefix("data:") else {
            return Parsed::default();
        };
        let data = data.trim();
        let mut out = Parsed::default();
        if data == "[DONE]" {
            let reason = self.finish_reason.take().unwrap_or_else(|| "stop".into());
            out.items.push(if reason == "length" && !self.saw_text {
                Err(ProviderError::NoTextAtTokenLimit)
            } else {
                Ok(StreamEvent::Done {
                    stop_reason: reason,
                })
            });
            out.finished = true;
            return out;
        }
        let chunk: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(e) => {
                out.items
                    .push(Err(ProviderError::Protocol(format!("bad SSE JSON: {e}"))));
                out.finished = true;
                return out;
            }
        };
        if let Some(err) = chunk.get("error").filter(|e| !e.is_null()) {
            out.items.push(Err(ProviderError::Stream {
                kind: err
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                message: err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars()
                    .take(300)
                    .collect(),
            }));
            out.finished = true;
            return out;
        }
        if let Some(text) = chunk
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
            && !text.is_empty()
        {
            self.saw_text = true;
            out.items.push(Ok(StreamEvent::TextDelta(text.to_string())));
        }
        if let Some(reason) = chunk
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
        {
            self.finish_reason = Some(reason.to_string());
        }
        if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
            let get = |p: &str| u.pointer(p).and_then(Value::as_u64).unwrap_or(0);
            let cached = get("/prompt_tokens_details/cached_tokens");
            out.items.push(Ok(StreamEvent::from_usage(Usage {
                input_tokens: get("/prompt_tokens").saturating_sub(cached),
                output_tokens: get("/completion_tokens"),
                cache_read_input_tokens: cached,
                cache_creation_input_tokens: 0,
            })));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::providers::Turn;

    const BODY: &str = "\
data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}],\"usage\":null}

data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Olá\"},\"finish_reason\":null}],\"usage\":null}

data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" mundo\"},\"finish_reason\":null}],\"usage\":null}

data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":null}

data: {\"id\":\"c1\",\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":7,\"prompt_tokens_details\":{\"cached_tokens\":60}}}

data: [DONE]

";

    fn request(with_image: bool) -> CompletionRequest {
        CompletionRequest {
            model: "gpt-4o".into(),
            system_prefix: "prefix".into(),
            mode_context: "mode".into(),
            history: String::new(),
            recent: String::new(),
            turns: vec![Turn::user("a"), Turn::assistant("b")],
            user_text: "question".into(),
            image_png: with_image.then(|| vec![1, 2, 3]),
            max_tokens: 100,
            effort: None,
            thinking: Thinking::Adaptive,
        }
    }

    #[test]
    fn reasoning_models_get_effort_and_room_for_reasoning() {
        let mut r = request(false);
        assert!(build_body(&r).get("reasoning_effort").is_none());
        assert_eq!(build_body(&r)["max_completion_tokens"], 100);

        r.model = "gpt-5-mini".into();
        r.thinking = Thinking::Disabled;
        r.max_tokens = 256;
        let b = build_body(&r);
        assert_eq!(b["reasoning_effort"], "minimal");
        assert_eq!(b["max_completion_tokens"], MIN_REASONING_COMPLETION_TOKENS);

        r.thinking = Thinking::Adaptive;
        r.effort = Some(crate::providers::Effort::Low);
        r.max_tokens = 1_024;
        let b = build_body(&r);
        assert_eq!(b["reasoning_effort"], "low");
        assert_eq!(b["max_completion_tokens"], 1_024);
        assert!(is_reasoning_model("o3-mini"));
        assert!(!is_reasoning_model("gpt-4o"));
    }

    #[tokio::test]
    async fn length_stop_with_no_text_is_an_error() {
        let server = MockServer::start().await;
        let body = "\
data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"length\"}],\"usage\":null}

data: {\"id\":\"c1\",\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":512}}

data: [DONE]

";
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body.as_bytes().to_vec(), "text/event-stream"),
            )
            .mount(&server)
            .await;
        let p = OpenAiProvider::new("sk-test")
            .unwrap()
            .with_base_url(server.uri());
        let items = p
            .stream(request(false), CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(matches!(
            items[0],
            Ok(StreamEvent::Usage {
                output_tokens: 512,
                ..
            })
        ));
        assert_eq!(items[1], Err(ProviderError::NoTextAtTokenLimit));
    }

    #[test]
    fn body_has_usage_option_and_data_url_image() {
        let b = build_body(&request(true));
        assert_eq!(b["stream"], true);
        assert_eq!(b["stream_options"]["include_usage"], true);
        assert_eq!(b["messages"][0]["content"], "prefix\n\nmode");
        let user = &b["messages"][3]["content"];
        assert_eq!(user[1]["image_url"]["url"], "data:image/png;base64,AQID");
        let plain = build_body(&request(false));
        assert_eq!(plain["messages"][3]["content"], "question");
    }

    #[tokio::test]
    async fn streams_text_usage_and_done() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer sk-test"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(BODY.as_bytes().to_vec(), "text/event-stream"),
            )
            .mount(&server)
            .await;
        let p = OpenAiProvider::new("sk-test")
            .unwrap()
            .with_base_url(server.uri());
        let stream = p
            .stream(request(false), CancellationToken::new())
            .await
            .unwrap();
        let items: Vec<_> = stream
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<_>>()
            .unwrap();
        assert_eq!(
            items,
            vec![
                StreamEvent::TextDelta("Olá".into()),
                StreamEvent::TextDelta(" mundo".into()),
                StreamEvent::Usage {
                    input_tokens: 40,
                    output_tokens: 7,
                    cache_read_input_tokens: 60,
                    cache_creation_input_tokens: 0,
                },
                StreamEvent::Done {
                    stop_reason: "stop".into()
                },
            ]
        );
    }

    #[tokio::test]
    async fn unauthorized_and_rate_limit_are_typed() {
        let server = MockServer::start().await;
        let p = OpenAiProvider::new("bad")
            .unwrap()
            .with_base_url(server.uri());
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        assert_eq!(
            p.stream(request(false), CancellationToken::new())
                .await
                .err(),
            Some(ProviderError::Unauthorized)
        );
        server.reset().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "2"))
            .mount(&server)
            .await;
        assert_eq!(
            p.stream(request(false), CancellationToken::new())
                .await
                .err(),
            Some(ProviderError::RateLimited {
                retry_after: Some(std::time::Duration::from_secs(2))
            })
        );
    }
}
