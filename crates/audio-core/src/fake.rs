use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::bus::AudioBus;
use crate::frame::{AudioFrame, SourceKind};
use crate::source::{AudioSource, CaptureError, SourceStatus};

/// Replays a fixed sample buffer as a real `AudioSource`, looping
/// indefinitely (wrapping back to the start) until stopped.
///
/// This exists so the bus/pipeline logic — tagging, fan-out, the
/// `AudioSource` contract itself — can be tested in CI with no real
/// hardware and no OS permissions, exercised through a dedicated thread
/// the same way `MicrophoneSource` and `SystemOutputSource` are, rather
/// than by calling `bus.publish` directly from a test and skipping the
/// "real source on a real thread" shape entirely.
pub struct FakeAudioSource {
    kind: SourceKind,
    samples: Arc<[f32]>,
    sample_rate: u32,
    channels: u8,
    frame_interval: Duration,
    status: Arc<AtomicU8>,
    handle: Option<CaptureThreadHandle>,
}

struct CaptureThreadHandle {
    stop_tx: std_mpsc::Sender<()>,
    join: std::thread::JoinHandle<()>,
}

impl FakeAudioSource {
    /// `samples` is interleaved PCM, looped from the start once exhausted.
    /// Paces frames out roughly every 20ms by default, matching the real
    /// sources' frame cadence — use `with_frame_interval` in tests that
    /// want to run faster than real time.
    pub fn new(kind: SourceKind, samples: Vec<f32>, sample_rate: u32, channels: u8) -> Self {
        assert!(
            !samples.is_empty(),
            "FakeAudioSource needs at least one sample to loop"
        );
        Self {
            kind,
            samples: Arc::from(samples),
            sample_rate,
            channels,
            frame_interval: Duration::from_millis(20),
            status: Arc::new(AtomicU8::new(SourceStatus::Idle as u8)),
            handle: None,
        }
    }

    pub fn with_frame_interval(mut self, interval: Duration) -> Self {
        self.frame_interval = interval;
        self
    }
}

#[async_trait::async_trait]
impl AudioSource for FakeAudioSource {
    fn kind(&self) -> SourceKind {
        self.kind
    }

    async fn start(&mut self, bus: AudioBus) -> Result<(), CaptureError> {
        if self.handle.is_some() {
            return Ok(());
        }
        self.status.store(SourceStatus::Starting as u8, Ordering::SeqCst);

        let (stop_tx, stop_rx) = std_mpsc::channel::<()>();
        let kind = self.kind;
        let samples = self.samples.clone();
        let sample_rate = self.sample_rate;
        let channels = self.channels;
        let frame_interval = self.frame_interval;

        let join = std::thread::Builder::new()
            .name("fake-audio-source".into())
            .spawn(move || {
                run_playback_thread(kind, samples, sample_rate, channels, frame_interval, bus, stop_rx)
            })
            .expect("failed to spawn fake-audio-source thread");

        self.status.store(SourceStatus::Running as u8, Ordering::SeqCst);
        self.handle = Some(CaptureThreadHandle { stop_tx, join });
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), CaptureError> {
        if let Some(handle) = self.handle.take() {
            let _ = handle.stop_tx.send(());
            let _ = handle.join.join();
            self.status.store(SourceStatus::Stopped as u8, Ordering::SeqCst);
        }
        Ok(())
    }

    fn status(&self) -> SourceStatus {
        SourceStatus::from_u8(self.status.load(Ordering::SeqCst))
    }
}

fn run_playback_thread(
    kind: SourceKind,
    samples: Arc<[f32]>,
    sample_rate: u32,
    channels: u8,
    frame_interval: Duration,
    bus: AudioBus,
    stop_rx: std_mpsc::Receiver<()>,
) {
    let frame_samples = (((sample_rate as usize) * (channels as usize)) / 50).max(channels as usize);
    let mut cursor = 0usize;

    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }

        let mut chunk = Vec::with_capacity(frame_samples);
        while chunk.len() < frame_samples {
            chunk.push(samples[cursor]);
            cursor = (cursor + 1) % samples.len();
        }

        bus.publish(AudioFrame {
            source: kind,
            captured_at: Instant::now(),
            sample_rate,
            channels,
            samples: Arc::from(chunk.as_slice()),
        });

        std::thread::sleep(frame_interval);
    }
}
