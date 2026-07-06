//! The text counterpart of audio-core's `AudioBus`: transcription publishes
//! finished `TranscriptSegment`s here, and any number of downstream
//! consumers (a cue agent, a transcript view, persistence) subscribe
//! without transcription knowing they exist — the same decoupling move,
//! one pipeline stage later.

use crate::TranscriptSegment;
use tokio::sync::broadcast;

/// Same delivery contract as `AudioBus`: prompt best-effort fan-out, a
/// lagging subscriber gets `Lagged(n)` and resumes rather than
/// backpressuring the pipeline. Text segments arrive at human speech pace
/// (a few per minute), so lagging is far less likely than on the audio bus
/// — but the contract is identical on purpose, so consumers written
/// against one bus behave correctly against the other.
#[derive(Clone)]
pub struct TranscriptBus {
    tx: broadcast::Sender<TranscriptSegment>,
}

impl TranscriptBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TranscriptSegment> {
        self.tx.subscribe()
    }

    /// Never blocks; zero subscribers is a healthy state, not an error.
    pub fn publish(&self, segment: TranscriptSegment) {
        let _ = self.tx.send(segment);
    }
}
