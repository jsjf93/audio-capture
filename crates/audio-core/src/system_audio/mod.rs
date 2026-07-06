//! System-audio capture: the Swift Process Tap helper, the wire protocol
//! used to talk to it, and `SystemOutputSource` — the `AudioSource` that
//! spawns and reads from the helper.

pub mod protocol;
mod source;

pub use protocol::{AudioMessage, ProtocolError, StatusEvent, TapMessage};
pub use source::SystemOutputSource;
