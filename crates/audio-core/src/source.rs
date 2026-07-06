use crate::bus::AudioBus;
use crate::frame::SourceKind;

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("no input device available")]
    NoDevice,
    #[error("unsupported stream configuration: {0}")]
    UnsupportedConfig(String),
    #[error("failed to build audio stream: {0}")]
    StreamBuildFailed(String),
    #[error("failed to start audio stream: {0}")]
    StreamStartFailed(String),
    #[error("capture thread did not confirm startup in time")]
    StartupTimedOut,
    #[error("system-audio helper is unavailable: {0}")]
    HelperUnavailable(String),
}

/// Coarse lifecycle state for a source, surfaced to the UI as a status dot.
/// `Failed` carries no auto-retry semantics here — that policy belongs to
/// whatever supervises the source (relevant once Phase 4 adds real
/// supervision), not to the source itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceStatus {
    Idle,
    Starting,
    Running,
    Stopped,
    Failed,
}

impl SourceStatus {
    /// Shared by every `AudioSource` implementation that tracks its status
    /// via an `AtomicU8` (all of them, as of Phase 3) so the encoding lives
    /// in one place rather than being duplicated per source.
    pub(crate) fn from_u8(value: u8) -> Self {
        match value {
            0 => SourceStatus::Idle,
            1 => SourceStatus::Starting,
            2 => SourceStatus::Running,
            3 => SourceStatus::Stopped,
            _ => SourceStatus::Failed,
        }
    }
}

/// A producer of [`AudioFrame`](crate::frame::AudioFrame)s.
///
/// This is the boundary that keeps capture ignorant of everything
/// downstream: `start` is only ever given a place to publish frames
/// (`AudioBus`), never a reference to whoever ends up subscribing to them.
/// Adding a transcription consumer later means adding a new subscriber to
/// the bus — zero changes here.
///
/// `start`/`stop` are async (via `async_trait`, since native async-fn-in-trait
/// doesn't yet support `dyn` dispatch on stable Rust) so that `MicrophoneSource`,
/// the future `SystemOutputSource` (which needs to await child-process
/// spawning), and `FakeAudioSource` (test fixture playback) all share one
/// trait and can be called uniformly from async Tauri command handlers.
#[async_trait::async_trait]
pub trait AudioSource: Send {
    fn kind(&self) -> SourceKind;
    async fn start(&mut self, bus: AudioBus) -> Result<(), CaptureError>;
    async fn stop(&mut self) -> Result<(), CaptureError>;
    fn status(&self) -> SourceStatus;
}
