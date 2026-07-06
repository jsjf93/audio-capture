use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::bus::AudioBus;
use crate::frame::{AudioFrame, SourceKind};
use crate::source::{AudioSource, CaptureError, SourceStatus};

/// Captures the default microphone via `cpal` and publishes frames onto an
/// [`AudioBus`].
///
/// # Why a dedicated OS thread owns the `cpal::Stream`
///
/// `cpal::Stream` is not guaranteed `Send` on every platform/backend — it
/// wraps native audio-unit handles whose thread-safety hasn't been audited
/// uniformly across backends. Rather than fight that, the stream is created,
/// played, and dropped entirely on one dedicated thread; `MicrophoneSource`
/// itself only ever holds `Send`-safe control handles (a stop signal and a
/// `JoinHandle`), never the stream itself. This sidesteps the question
/// entirely instead of relying on cpal's Send behavior staying the same
/// across versions or platforms.
///
/// # The two hops from callback to bus
///
/// cpal invokes its callback on a real-time OS audio thread: it must never
/// block, allocate, or lock, or you risk an audible glitch. So the callback
/// only does one thing — push raw samples into an `rtrb` ring buffer
/// (lock-free, wait-free, allocation-free `try_push`, drop-and-count on
/// full). A plain loop on the same dedicated thread drains that ring buffer
/// in fixed-size chunks and publishes each chunk as an `AudioFrame` onto the
/// `AudioBus`. That publish call is a synchronous, ordinary function call
/// (`broadcast::Sender::send`), so it doesn't need an async runtime here —
/// only subscribers reading from the bus need one.
pub struct MicrophoneSource {
    status: Arc<AtomicU8>,
    handle: Option<CaptureThreadHandle>,
}

struct CaptureThreadHandle {
    stop_tx: std_mpsc::Sender<()>,
    join: std::thread::JoinHandle<()>,
}

impl MicrophoneSource {
    pub fn new() -> Self {
        Self {
            status: Arc::new(AtomicU8::new(SourceStatus::Idle as u8)),
            handle: None,
        }
    }
}

impl Default for MicrophoneSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl AudioSource for MicrophoneSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Microphone
    }

    async fn start(&mut self, bus: AudioBus) -> Result<(), CaptureError> {
        if self.handle.is_some() {
            return Ok(()); // already running; starting twice is a no-op, not an error
        }

        self.status.store(SourceStatus::Starting as u8, Ordering::SeqCst);

        let (ready_tx, ready_rx) = std_mpsc::channel::<Result<(), CaptureError>>();
        let (stop_tx, stop_rx) = std_mpsc::channel::<()>();
        let status = self.status.clone();

        let join = std::thread::Builder::new()
            .name("mic-capture".into())
            .spawn(move || run_capture_thread(bus, ready_tx, stop_rx, status))
            .expect("failed to spawn mic-capture thread");

        // Block (we're off the real-time path here — this runs on whatever
        // called `start`, e.g. a Tauri command handler) until the capture
        // thread confirms the stream actually started, so callers get a
        // real error instead of a false "started fine".
        //
        // This timeout is generous on purpose: on first run, macOS gates
        // `build_input_stream` on the user answering the microphone TCC
        // permission dialog, which can take an arbitrarily long time for a
        // human to notice and click. A tight timeout here would misreport
        // "timed out" for what's actually just a pending permission prompt.
        // A future phase could report a distinct "awaiting permission"
        // status instead of blocking silently, but that requires detecting
        // the pending-authorization state explicitly rather than inferring
        // it from a slow reply.
        match ready_rx.recv_timeout(Duration::from_secs(60)) {
            Ok(Ok(())) => {
                self.handle = Some(CaptureThreadHandle { stop_tx, join });
                Ok(())
            }
            Ok(Err(err)) => {
                let _ = join.join();
                self.status.store(SourceStatus::Failed as u8, Ordering::SeqCst);
                Err(err)
            }
            Err(_) => {
                self.status.store(SourceStatus::Failed as u8, Ordering::SeqCst);
                Err(CaptureError::StartupTimedOut)
            }
        }
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


/// Body of the dedicated capture thread: build + play the cpal stream,
/// confirm startup via `ready_tx`, then loop draining the ring buffer into
/// `AudioFrame`s until `stop_rx` fires.
fn run_capture_thread(
    bus: AudioBus,
    ready_tx: std_mpsc::Sender<Result<(), CaptureError>>,
    stop_rx: std_mpsc::Receiver<()>,
    status: Arc<AtomicU8>,
) {
    eprintln!("[mic] capture thread started, requesting default host");
    let host = cpal::default_host();
    eprintln!("[mic] got host {:?}, looking up default input device", host.id());
    let device = match host.default_input_device() {
        Some(d) => d,
        None => {
            let _ = ready_tx.send(Err(CaptureError::NoDevice));
            return;
        }
    };
    eprintln!("[mic] found input device {device}, querying default config");

    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            let _ = ready_tx.send(Err(CaptureError::UnsupportedConfig(e.to_string())));
            return;
        }
    };
    eprintln!("[mic] default config: {config:?}");

    if config.sample_format() != cpal::SampleFormat::F32 {
        // Most macOS input devices report F32 as their default config; other
        // formats are handled once a real device forces the issue rather
        // than speculatively supporting every cpal::SampleFormat now.
        let _ = ready_tx.send(Err(CaptureError::UnsupportedConfig(format!(
            "sample format {:?} not yet supported (only F32)",
            config.sample_format()
        ))));
        return;
    }

    let sample_rate = config.sample_rate();
    let channels = config.channels();
    let stream_config: cpal::StreamConfig = config.into();

    // ~0.5s of headroom between the real-time callback and the drain loop.
    let ring_capacity = (sample_rate as usize) * (channels as usize) / 2;
    let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(ring_capacity);

    let stream = match device.build_input_stream(
        stream_config,
        move |data: &[f32], _info: &cpal::InputCallbackInfo| {
            for &sample in data {
                // Never block/allocate/log on this thread: on overflow we
                // simply drop the sample. Real health-reporting (drop
                // counters surfaced to the UI) is a Phase 4 supervision
                // concern; today an overflow here would mean the drain loop
                // below is somehow stalled, which Phase 1's manual RMS check
                // would immediately make obvious.
                let _ = producer.push(sample);
            }
        },
        move |err| eprintln!("[mic] stream error: {err}"),
        None,
    ) {
        Ok(s) => {
            eprintln!("[mic] input stream built, calling play()");
            s
        }
        Err(e) => {
            let _ = ready_tx.send(Err(CaptureError::StreamBuildFailed(e.to_string())));
            return;
        }
    };

    if let Err(e) = stream.play() {
        let _ = ready_tx.send(Err(CaptureError::StreamStartFailed(e.to_string())));
        return;
    }

    status.store(SourceStatus::Running as u8, Ordering::SeqCst);
    let _ = ready_tx.send(Ok(()));

    // 20ms frames: small enough for low latency downstream, large enough
    // that publishing isn't dominated by per-call overhead.
    let frame_samples = ((sample_rate as usize) * (channels as usize)) / 50;
    let mut scratch = vec![0f32; frame_samples];
    // `filled` deliberately lives outside the loop and carries a partial
    // fill across iterations. An earlier version of this loop redeclared
    // `filled = 0` inside the loop body, which silently discarded
    // already-popped-but-unpublished samples whenever the ring buffer
    // didn't have a full frame ready in a single drain attempt — real data
    // loss on its own, and (confirmed via instrumentation while
    // investigating a reproducible mic dropout when running alongside
    // system-audio capture) capable of compounding into a permanent stall
    // after tens of seconds under the right timing conditions. Carrying
    // `filled` across iterations, so a partial drain resumes instead of
    // being thrown away, fixed it outright — verified via a producer/consumer
    // sample-count trace showing zero divergence for the run's full duration.
    let mut filled = 0usize;

    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }

        while filled < frame_samples {
            match consumer.pop() {
                Ok(sample) => {
                    scratch[filled] = sample;
                    filled += 1;
                }
                Err(_) => break, // ring buffer temporarily empty; resume from here next loop
            }
        }

        if filled == frame_samples {
            let frame = AudioFrame {
                source: SourceKind::Microphone,
                captured_at: Instant::now(),
                sample_rate,
                channels: channels as u8,
                samples: Arc::from(scratch.as_slice()),
            };
            bus.publish(frame);
            filled = 0;
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    status.store(SourceStatus::Stopped as u8, Ordering::SeqCst);
    // `stream` is dropped here, which stops it.
}
