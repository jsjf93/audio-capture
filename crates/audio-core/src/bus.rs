use crate::frame::AudioFrame;
use tokio::sync::broadcast;

/// The internal event bus: every capture source publishes onto it, and any
/// number of independent consumers subscribe to it, without either side
/// knowing about the other.
///
/// Built on `tokio::sync::broadcast` specifically because it's a true
/// multi-consumer channel — every subscriber sees every frame — unlike e.g.
/// `crossbeam-channel`, which is multi-producer/multi-consumer but delivers
/// each message to exactly *one* receiver. A subscriber that falls behind
/// gets `RecvError::Lagged(n)` on its next `recv()` and resumes from the
/// next available frame, rather than backpressuring the publisher or
/// growing memory without bound. That's a deliberate contract: this bus
/// promises prompt, best-effort delivery, not lossless delivery for every
/// possible future subscriber. A consumer that genuinely cannot tolerate
/// drops (a future transcription client, say) is responsible for its own
/// buffering — that's not this bus's job.
#[derive(Clone)]
pub struct AudioBus {
    tx: broadcast::Sender<AudioFrame>,
}

impl AudioBus {
    /// `capacity` is the number of frames retained for a lagging subscriber
    /// to catch up on before it starts missing frames. It is not a queue
    /// depth for backpressure — `publish` never blocks regardless of this
    /// value.
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AudioFrame> {
        self.tx.subscribe()
    }

    /// Publish a frame to every current subscriber. Never blocks. Returns
    /// with no error even if there are currently zero subscribers — that's
    /// an expected, healthy state (e.g. between the source starting and the
    /// UI subscribing), not a failure.
    pub fn publish(&self, frame: AudioFrame) {
        let _ = self.tx.send(frame);
    }
}
