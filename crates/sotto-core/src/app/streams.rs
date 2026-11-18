//! Provider calls run in their own tasks and report to the event loop over one channel. Every
//! message carries the id of the stream that sent it, so the loop can ignore messages from a
//! stream it already cancelled.

use std::sync::Arc;

use base64::Engine as _;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::mode_context::ModeQuery;
use super::models::ProviderFactory;
use crate::prompt::{Verdict, parse_verdict};
use crate::providers::{CompletionRequest, ProviderError, StreamEvent, Usage};
use crate::secrets::SecretStore;

pub(crate) type StreamId = u64;

/// Enough for "yes" or "no" plus noise; the classifier never needs more.
const MAX_VERDICT_CHARS: usize = 32;

#[derive(Debug)]
pub(crate) struct StreamMsg {
    pub id: StreamId,
    pub kind: StreamMsgKind,
}

#[derive(Debug)]
pub(crate) enum StreamMsgKind {
    Delta(String),
    /// Tokens billed since the previous `Usage` of this stream (an estimate when the stream
    /// was cut before the provider reported).
    Usage {
        model: String,
        kind: JobKind,
        usage: Usage,
    },
    Done,
    /// `setup` is true when the request never left the machine (missing key, unknown model).
    Failed {
        message: String,
        setup: bool,
    },
    /// Shown in the answer's error field; the answer goes on.
    Warning(String),
    Verdict(Verdict),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobKind {
    /// A speculative answer (ADR 0006); counts toward the spend cap.
    Auto,
    /// A typed ask; costs money but never blocks automatic answers.
    Manual,
    Classifier,
}

/// About four bytes per token: overestimates for English and Portuguese, which is the safe side
/// for a spend cap.
fn estimate_tokens(bytes: usize) -> u64 {
    u64::try_from(bytes.div_ceil(4)).unwrap_or(u64::MAX)
}

/// The prompt size in tokens, for a stream cut before the provider reported its input.
pub(crate) fn estimate_input_tokens(req: &CompletionRequest) -> u64 {
    let turns: usize = req.turns.iter().map(|t| t.text.len()).sum();
    estimate_tokens(
        req.system_prefix.len()
            + req.mode_context.len()
            + req.history.len()
            + req.recent.len()
            + req.user_text.len()
            + turns,
    )
}

/// What a stream has reported, so a stream that ends without the provider's final usage
/// (cancelled, cut off) can still be billed: Anthropic only reports output at the end, and
/// OpenAI and Ollama report nothing until then.
struct Meter {
    input_estimate: u64,
    reported: Usage,
    text_bytes: usize,
}

impl Meter {
    /// Only what the reported usage does not already cover.
    fn missing(&self) -> Usage {
        let r = &self.reported;
        let input_seen = r.input_tokens + r.cache_read_input_tokens + r.cache_creation_input_tokens;
        Usage {
            input_tokens: if input_seen == 0 {
                self.input_estimate
            } else {
                0
            },
            output_tokens: estimate_tokens(self.text_bytes).saturating_sub(r.output_tokens),
            ..Usage::default()
        }
    }
}

enum End {
    Done,
    Cancelled,
    Failed(String),
}

pub(crate) struct Job {
    pub kind: JobKind,
    /// The configured id (`ollama:qwen3`), used for the provider and the price table. The
    /// request carries the wire name.
    pub model: String,
    pub request: CompletionRequest,
    pub png_base64: Option<String>,
    /// Fills in the mode part of the request; retrieval needs the question and an embedding
    /// call, so it happens here and not on the event loop.
    pub mode: Option<ModeQuery>,
}

/// Cancels its stream when dropped, so replacing or clearing an answer always stops the work.
#[derive(Debug)]
pub(crate) struct Running {
    pub id: StreamId,
    cancel: CancellationToken,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

pub(crate) struct Spawner {
    providers: Arc<dyn ProviderFactory>,
    secrets: Arc<dyn SecretStore>,
    tx: mpsc::UnboundedSender<StreamMsg>,
    next_id: StreamId,
}

impl Spawner {
    pub fn new(
        providers: Arc<dyn ProviderFactory>,
        secrets: Arc<dyn SecretStore>,
        tx: mpsc::UnboundedSender<StreamMsg>,
    ) -> Self {
        Self {
            providers,
            secrets,
            tx,
            next_id: 1,
        }
    }

    pub fn spawn(&mut self, job: Job) -> Running {
        let id = self.next_id;
        self.next_id += 1;
        let cancel = CancellationToken::new();
        tokio::spawn(run(
            id,
            job,
            Arc::clone(&self.providers),
            Arc::clone(&self.secrets),
            self.tx.clone(),
            cancel.clone(),
        ));
        Running { id, cancel }
    }
}

fn user_message(e: &ProviderError) -> String {
    match e {
        ProviderError::Unauthorized => {
            "The provider rejected the API key. Check it in Settings > Models".into()
        }
        ProviderError::NoTextAtTokenLimit => {
            "The model spent its whole output budget reasoning and wrote no answer. Try again or pick another model in Settings > Models".into()
        }
        other => other.to_string(),
    }
}

async fn run(
    id: StreamId,
    job: Job,
    providers: Arc<dyn ProviderFactory>,
    secrets: Arc<dyn SecretStore>,
    tx: mpsc::UnboundedSender<StreamMsg>,
    cancel: CancellationToken,
) {
    let send = |kind: StreamMsgKind| {
        let _ = tx.send(StreamMsg { id, kind });
    };
    let Job {
        kind,
        model,
        mut request,
        png_base64,
        mode,
    } = job;

    let provider = match providers.provider(&model, secrets.as_ref()) {
        Ok(p) => p,
        Err(e) => {
            send(StreamMsgKind::Failed {
                message: e.to_string(),
                setup: true,
            });
            return;
        }
    };
    if let Some(m) = mode {
        let rendered = tokio::select! {
            () = cancel.cancelled() => return,
            text = m.context.render(&m.query) => text,
        };
        request.mode_context = crate::prompt::mode_block(&rendered.stable);
        if !rendered.passages.is_empty() {
            request.user_text = format!("{}\n\n{}", rendered.passages, request.user_text);
        }
    }
    if let Some(b64) = png_base64 {
        match base64::engine::general_purpose::STANDARD.decode(b64.as_bytes()) {
            Ok(png) => request.image_png = Some(png),
            Err(_) => send(StreamMsgKind::Warning(
                "The screenshot could not be read; answered without it".into(),
            )),
        }
    }

    let mut task = Task {
        kind,
        model: &model,
        meter: Meter {
            input_estimate: estimate_input_tokens(&request),
            reported: Usage::default(),
            text_bytes: 0,
        },
        verdict_text: String::new(),
        send: &send,
    };
    let end = task.drive(provider.as_ref(), request, &cancel).await;
    match end {
        Some(End::Done) => match kind {
            JobKind::Auto | JobKind::Manual => send(StreamMsgKind::Done),
            JobKind::Classifier => {
                send(StreamMsgKind::Verdict(parse_verdict(&task.verdict_text)));
            }
        },
        None => {}
        Some(other) => {
            let missing = task.meter.missing();
            if missing != Usage::default() {
                send(StreamMsgKind::Usage {
                    model: model.clone(),
                    kind,
                    usage: missing,
                });
            }
            if let End::Failed(message) = other {
                send(StreamMsgKind::Failed {
                    message,
                    setup: false,
                });
            }
        }
    }
}

struct Task<'a> {
    kind: JobKind,
    model: &'a str,
    meter: Meter,
    verdict_text: String,
    send: &'a (dyn Fn(StreamMsgKind) + Sync),
}

impl Task<'_> {
    /// `None` when the request was refused before anything could be billed (HTTP error).
    async fn drive(
        &mut self,
        provider: &dyn crate::providers::LlmProvider,
        request: CompletionRequest,
        cancel: &CancellationToken,
    ) -> Option<End> {
        let opened = tokio::select! {
            () = cancel.cancelled() => return Some(End::Cancelled),
            r = provider.stream(request, cancel.clone()) => r,
        };
        let mut stream = match opened {
            Ok(s) => s,
            Err(ProviderError::Cancelled) => return Some(End::Cancelled),
            Err(e) => {
                (self.send)(StreamMsgKind::Failed {
                    message: user_message(&e),
                    setup: false,
                });
                return None;
            }
        };
        loop {
            let item = tokio::select! {
                () = cancel.cancelled() => return Some(End::Cancelled),
                item = stream.next() => item,
            };
            match item {
                None => return Some(End::Done),
                Some(Ok(StreamEvent::TextDelta(text))) => self.on_text(text),
                Some(Ok(event @ StreamEvent::Usage { .. })) => {
                    if let Some(usage) = event.usage() {
                        let r = &mut self.meter.reported;
                        r.input_tokens += usage.input_tokens;
                        r.output_tokens += usage.output_tokens;
                        r.cache_read_input_tokens += usage.cache_read_input_tokens;
                        r.cache_creation_input_tokens += usage.cache_creation_input_tokens;
                        (self.send)(StreamMsgKind::Usage {
                            model: self.model.to_string(),
                            kind: self.kind,
                            usage,
                        });
                    }
                }
                Some(Ok(StreamEvent::Done { .. })) => {}
                Some(Err(ProviderError::Cancelled)) => return Some(End::Cancelled),
                Some(Err(e)) => return Some(End::Failed(user_message(&e))),
            }
        }
    }

    fn on_text(&mut self, text: String) {
        self.meter.text_bytes += text.len();
        match self.kind {
            JobKind::Auto | JobKind::Manual => (self.send)(StreamMsgKind::Delta(text)),
            JobKind::Classifier => {
                if self.verdict_text.len() < MAX_VERDICT_CHARS {
                    self.verdict_text.push_str(&text);
                }
            }
        }
    }
}
