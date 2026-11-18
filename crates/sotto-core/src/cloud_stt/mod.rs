//! Cloud streaming transcription (ADR 0003): the opt-in alternative to the on-device engine.
//! The sidecar captures audio and writes raw PCM frames; this module turns them into transcript
//! revisions through the OpenAI Realtime transcription API.

mod credentials;
mod engine;
#[cfg(test)]
pub(crate) mod fake_server;
mod gate;
mod mapper;
mod pcm;
mod resample;
pub mod wire;

pub use credentials::{
    ByokOpenAi, CloudCredentials, Credential, CredentialError, FirstAvailable, HostedOpenAi,
};
pub use engine::{CloudConfig, CloudEngine, CloudEvent, FeedError};
pub use gate::{GateAction, GateConfig, TurnGate};
pub use mapper::Mapper;
pub use pcm::{MAX_FRAME_SAMPLES, PcmError, PcmFrame, encode_frame, pump_frames, read_frame};
pub use resample::{Resampler16To24, resample_16k_to_24k};
