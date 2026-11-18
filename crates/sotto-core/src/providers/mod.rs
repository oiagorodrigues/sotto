//! LLM providers behind one streaming trait. Raw HTTP with `reqwest` (rustls) for all three:
//! Anthropic has no official Rust SDK, and OpenAI and Ollama need only one endpoint each.
//!
//! Nothing in this module logs keys, prompts or outputs.

use std::fmt;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

pub mod anthropic;
pub mod ollama;
pub mod openai;
mod wire;

pub use anthropic::AnthropicProvider;
pub use ollama::OllamaProvider;
pub use openai::OpenAiProvider;

pub type Result<T, E = ProviderError> = std::result::Result<T, E>;

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum ProviderError {
    #[error("the provider rejected the API key")]
    Unauthorized,
    #[error("rate limited (retry after {retry_after:?})")]
    RateLimited { retry_after: Option<Duration> },
    #[error("provider server error (HTTP {status})")]
    Server { status: u16 },
    #[error("provider returned HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("network error: {0}")]
    Network(String),
    #[error("the model refused the request (category: {category:?})")]
    Refusal { category: Option<String> },
    #[error("provider stream error ({kind}): {message}")]
    Stream { kind: String, message: String },
    #[error("unexpected provider response: {0}")]
    Protocol(String),
    /// The output budget ran out (reasoning used it) before any text was written.
    #[error("the model used its whole output budget before writing any text")]
    NoTextAtTokenLimit,
    #[error("the provider sent nothing for {secs} s")]
    IdleTimeout { secs: u64 },
    #[error("request cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Whether the model may reason before answering. `Adaptive` sends nothing, so each provider's
/// default applies (Haiku 5.5 thinks adaptively when `thinking` is omitted).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Thinking {
    #[default]
    Adaptive,
    /// Anthropic `thinking: {type: "disabled"}` (valid at effort high or below), OpenAI
    /// `reasoning_effort: "minimal"`, Ollama `think: false`.
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Turn {
    pub role: Role,
    pub text: String,
}

// Turn text is conversation content, so Debug omits it.
impl fmt::Debug for Turn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Turn")
            .field("role", &self.role)
            .field("text_len", &self.text.len())
            .finish()
    }
}

impl Turn {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            text: text.into(),
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            text: text.into(),
        }
    }
}

#[derive(Clone)]
pub struct CompletionRequest {
    pub model: String,
    /// Byte-identical across requests so the provider can cache it.
    pub system_prefix: String,
    /// Per-mode context (prompt and files). Stable for a session, so cacheable.
    pub mode_context: String,
    /// Transcript lines that are final and precede every non-final one. They only grow at the
    /// end, so they get their own cache breakpoint.
    pub history: String,
    /// The rest of the transcript (lines that can still be revised). Never cached.
    pub recent: String,
    pub turns: Vec<Turn>,
    pub user_text: String,
    pub image_png: Option<Vec<u8>>,
    pub max_tokens: u32,
    /// Anthropic `output_config.effort`; OpenAI `reasoning_effort` on reasoning models.
    pub effort: Option<Effort>,
    pub thinking: Thinking,
}

impl CompletionRequest {
    /// Mode context and transcript as one text, for providers without content blocks.
    #[must_use]
    pub fn context_text(&self) -> String {
        [
            self.mode_context.as_str(),
            self.history.as_str(),
            self.recent.as_str(),
        ]
        .concat()
    }
}

// Prompts are sensitive, so Debug prints sizes only.
impl fmt::Debug for CompletionRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompletionRequest")
            .field("model", &self.model)
            .field("system_prefix_len", &self.system_prefix.len())
            .field("mode_context_len", &self.mode_context.len())
            .field("history_len", &self.history.len())
            .field("recent_len", &self.recent.len())
            .field("turns", &self.turns.len())
            .field("user_text_len", &self.user_text.len())
            .field("has_image", &self.image_png.is_some())
            .field("max_tokens", &self.max_tokens)
            .field("effort", &self.effort)
            .field("thinking", &self.thinking)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    TextDelta(String),
    /// Tokens billed since the previous `Usage` event of the same stream; the sum of all of
    /// them is the request's usage. Anthropic reports input at the start and output at the end,
    /// so a stream cut in the middle has still reported its input.
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        cache_read_input_tokens: u64,
        cache_creation_input_tokens: u64,
    },
    Done {
        stop_reason: String,
    },
}

impl StreamEvent {
    #[must_use]
    pub fn usage(&self) -> Option<Usage> {
        match *self {
            Self::Usage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
            } => Some(Usage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
            }),
            _ => None,
        }
    }

    pub(crate) fn from_usage(u: Usage) -> Self {
        Self::Usage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_input_tokens: u.cache_read_input_tokens,
            cache_creation_input_tokens: u.cache_creation_input_tokens,
        }
    }
}

pub type EventStream = BoxStream<'static, Result<StreamEvent>>;

/// Latency metric for ADR 0006: records when the first `TextDelta` passed through the stream.
#[derive(Debug, Clone)]
pub struct TtftHandle {
    started: Instant,
    first_token: Arc<OnceLock<Instant>>,
}

impl TtftHandle {
    #[must_use]
    pub fn started_at(&self) -> Instant {
        self.started
    }

    /// The `Instant` the first text delta arrived, if it has.
    #[must_use]
    pub fn first_token_at(&self) -> Option<Instant> {
        self.first_token.get().copied()
    }

    /// Time from the request start to the first text delta.
    #[must_use]
    pub fn time_to_first_token(&self) -> Option<Duration> {
        self.first_token_at()
            .map(|t| t.duration_since(self.started))
    }
}

/// Wraps a stream so the arrival of its first `TextDelta` is recorded.
pub fn track_ttft(stream: EventStream) -> (EventStream, TtftHandle) {
    let handle = TtftHandle {
        started: Instant::now(),
        first_token: Arc::new(OnceLock::new()),
    };
    let slot = Arc::clone(&handle.first_token);
    let wrapped = stream
        .inspect(move |item| {
            if matches!(item, Ok(StreamEvent::TextDelta(_))) {
                let _ = slot.set(Instant::now());
            }
        })
        .boxed();
    (wrapped, handle)
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Starts a streaming completion. Cancelling the token stops reading and drops the
    /// connection; the stream then ends without further items.
    async fn stream(
        &self,
        req: CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream>;

    /// Same as [`stream`](Self::stream), plus a handle that records time to first token.
    async fn stream_timed(
        &self,
        req: CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<(EventStream, TtftHandle)> {
        let started = Instant::now();
        let stream = self.stream(req, cancel).await?;
        let (stream, mut handle) = track_ttft(stream);
        handle.started = started;
        Ok((stream, handle))
    }
}
