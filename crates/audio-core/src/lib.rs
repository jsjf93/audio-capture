//! Platform-agnostic audio capture pipeline.
//!
//! This crate deliberately has no dependency on Tauri: it should be possible
//! to build, run, and test the capture pipeline (sources, the internal bus,
//! the system-audio helper protocol) entirely on its own. The `src-tauri`
//! crate is just one consumer of this crate, responsible for IPC to the
//! frontend and nothing else.
//!
//! - [`frame`] — the [`AudioFrame`] and [`SourceKind`] data types that flow
//!   through the whole pipeline, tagged and never merged.
//! - [`source`] — the [`AudioSource`] trait that every capture backend
//!   implements, keeping capture ignorant of whoever consumes its frames.
//! - [`bus`] — the internal multi-consumer event bus.
//! - [`mic`] — microphone capture via `cpal`.
//! - [`system_audio`] — the Swift Process Tap helper's wire protocol and
//!   `SystemOutputSource`, which spawns and reads from it.
//! - [`fake`] — `FakeAudioSource`, a hardware-free `AudioSource` for tests.
//! - [`health`] — `StalenessWatcher` and `RestartPolicy`, the building
//!   blocks Phase 4 supervision is built from.

mod bus;
mod fake;
mod frame;
pub mod health;
mod mic;
mod source;
pub mod system_audio;

pub use bus::AudioBus;
pub use fake::FakeAudioSource;
pub use frame::{AudioFrame, SourceKind};
pub use health::{HealthState, RestartPolicy, StalenessWatcher};
pub use mic::MicrophoneSource;
pub use source::{AudioSource, CaptureError, SourceStatus};
pub use system_audio::SystemOutputSource;
