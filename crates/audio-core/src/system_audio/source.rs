use std::io::BufReader;
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::protocol::{read_message, TapMessage};
use crate::bus::AudioBus;
use crate::frame::{AudioFrame, SourceKind};
use crate::source::{AudioSource, CaptureError, SourceStatus};

/// Spawns the Swift Process Tap helper as a child process, decodes its
/// framed stdout (see `docs/audio-tap-protocol.md`), and publishes each
/// audio message onto the shared `AudioBus` tagged as `SourceKind::SystemOutput`.
///
/// Structurally this mirrors `MicrophoneSource`: a dedicated thread owns
/// the real I/O (there, a `cpal::Stream`; here, a child process) so
/// `SystemOutputSource` itself only ever holds `Send`-safe control handles.
/// The realtime-safety concern is different, though — there's no audio
/// callback on the Rust side to keep non-blocking, since that discipline
/// already happened inside the helper (its IOProc → RingBuffer → stdout
/// writer). The Rust reader thread does ordinary blocking I/O on a pipe,
/// which is fine on an ordinary thread.
pub struct SystemOutputSource {
    status: Arc<AtomicU8>,
    handle: Option<CaptureThreadHandle>,
}

struct CaptureThreadHandle {
    child_pid: u32,
    reader_join: std::thread::JoinHandle<()>,
}

impl SystemOutputSource {
    pub fn new() -> Self {
        Self {
            status: Arc::new(AtomicU8::new(SourceStatus::Idle as u8)),
            handle: None,
        }
    }
}

impl Default for SystemOutputSource {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolves the helper binary's path. `AUDIO_TAP_HELPER_PATH` is an escape
/// hatch (e.g. for pointing at a specific build); otherwise this resolves
/// to the Swift package's debug build, sibling to this crate in the
/// workspace. Phase 4 replaces the default path with proper Tauri sidecar
/// resolution (`tauri-plugin-shell`'s `sidecar()`, which looks inside the
/// packaged `.app` instead of the source tree) — this dev-time default is
/// intentionally not that yet.
fn helper_binary_path() -> PathBuf {
    if let Ok(path) = std::env::var("AUDIO_TAP_HELPER_PATH") {
        return PathBuf::from(path);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .parent()
        .expect("workspace root")
        .join("swift-helper/.build/debug/AudioTapHelper")
}

#[async_trait::async_trait]
impl AudioSource for SystemOutputSource {
    fn kind(&self) -> SourceKind {
        SourceKind::SystemOutput
    }

    async fn start(&mut self, bus: AudioBus) -> Result<(), CaptureError> {
        if self.handle.is_some() {
            return Ok(()); // already running; starting twice is a no-op
        }

        self.status.store(SourceStatus::Starting as u8, Ordering::SeqCst);

        let binary_path = helper_binary_path();
        if !binary_path.exists() {
            self.status.store(SourceStatus::Failed as u8, Ordering::SeqCst);
            return Err(CaptureError::HelperUnavailable(format!(
                "helper binary not found at {}; run `swift build` in swift-helper/ first",
                binary_path.display()
            )));
        }

        let mut child = Command::new(&binary_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| CaptureError::HelperUnavailable(format!("failed to spawn helper: {e}")))?;

        let pid = child.id();
        let stdout = child.stdout.take().expect("helper spawned with piped stdout");
        let stderr = child.stderr.take().expect("helper spawned with piped stderr");

        std::thread::Builder::new()
            .name("system-audio-stderr".into())
            .spawn(move || drain_stderr(stderr))
            .expect("failed to spawn stderr-drain thread");

        let (ready_tx, ready_rx) = std_mpsc::channel::<Result<(), CaptureError>>();
        let status = self.status.clone();

        let reader_join = std::thread::Builder::new()
            .name("system-audio-reader".into())
            .spawn(move || run_reader_thread(stdout, child, bus, ready_tx, status))
            .expect("failed to spawn system-audio reader thread");

        // Generous timeout: Phase 2 testing showed the helper's own
        // aggregate-device-readiness polling can itself take up to ~2s in
        // the worst case, on top of ordinary process startup.
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => {
                self.handle = Some(CaptureThreadHandle {
                    child_pid: pid,
                    reader_join,
                });
                Ok(())
            }
            Ok(Err(err)) => {
                let _ = reader_join.join();
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
            // SIGINT, not SIGKILL: this is what lets the helper tear down
            // the tap and aggregate device cleanly on its side (see
            // main.swift's signal handlers) instead of leaking Core Audio
            // objects that outlive the process being yanked out.
            let _ = Command::new("kill")
                .args(["-INT", &handle.child_pid.to_string()])
                .status();
            let _ = handle.reader_join.join();
            self.status.store(SourceStatus::Stopped as u8, Ordering::SeqCst);
        }
        Ok(())
    }

    fn status(&self) -> SourceStatus {
        SourceStatus::from_u8(self.status.load(Ordering::SeqCst))
    }
}

/// Runs on a dedicated thread for the lifetime of the helper process:
/// decode messages from its stdout and publish audio frames onto the bus.
/// Reports readiness (or a startup failure) exactly once, via `ready_tx`,
/// then keeps running until the stream ends or a protocol error occurs.
fn run_reader_thread(
    stdout: ChildStdout,
    mut child: Child,
    bus: AudioBus,
    ready_tx: std_mpsc::Sender<Result<(), CaptureError>>,
    status: Arc<AtomicU8>,
) {
    let mut reader = BufReader::new(stdout);
    let mut announced_ready = false;

    loop {
        match read_message(&mut reader) {
            Ok(Some(TapMessage::Audio(msg))) => {
                if !announced_ready {
                    status.store(SourceStatus::Running as u8, Ordering::SeqCst);
                    let _ = ready_tx.send(Ok(()));
                    announced_ready = true;
                }

                // `captured_at` is stamped on arrival in this process, not
                // reconstructed from the helper's own `timestamp_ns` — that
                // value is monotonic only relative to the *helper's* start
                // (see docs/audio-tap-protocol.md's clock-domain note), and
                // there's no established offset yet to map it onto this
                // process's `Instant` clock. Using arrival time keeps this
                // field meaning the same thing across every `AudioSource`
                // impl (mic, system-output, fake) for now; this is a
                // deliberate simplification, not an oversight, and will need
                // revisiting once a future phase needs true cross-stream
                // sample-accurate alignment.
                let frame = AudioFrame {
                    source: SourceKind::SystemOutput,
                    captured_at: Instant::now(),
                    sample_rate: msg.sample_rate,
                    channels: msg.channels,
                    samples: Arc::from(msg.samples.as_slice()),
                };
                bus.publish(frame);
            }
            Ok(Some(TapMessage::Heartbeat)) => {
                if !announced_ready {
                    status.store(SourceStatus::Running as u8, Ordering::SeqCst);
                    let _ = ready_tx.send(Ok(()));
                    announced_ready = true;
                }
            }
            Ok(Some(TapMessage::StatusEvent(event))) => {
                eprintln!(
                    "[system-audio] helper status: level={} code={} message={}",
                    event.level, event.code, event.message
                );
                if event.level == "error" && !announced_ready {
                    let _ = ready_tx.send(Err(CaptureError::StreamStartFailed(event.message)));
                    announced_ready = true;
                }
            }
            Ok(None) => break, // clean EOF: helper exited
            Err(e) => {
                eprintln!("[system-audio] protocol error, stopping reader: {e}");
                break;
            }
        }
    }

    let _ = child.wait(); // reap the process, whatever state it's in

    if !announced_ready {
        let _ = ready_tx.send(Err(CaptureError::StreamStartFailed(
            "helper process exited before producing any output".into(),
        )));
    }
    status.store(SourceStatus::Stopped as u8, Ordering::SeqCst);
}

/// Drains the helper's stderr (its human-readable logs) on its own thread
/// so a full pipe buffer there can never back-pressure the helper's own
/// logging calls, and so those logs are visible in ours.
fn drain_stderr(stderr: ChildStderr) {
    use std::io::{BufRead, BufReader as StdBufReader};
    let reader = StdBufReader::new(stderr);
    for line in reader.lines().map_while(Result::ok) {
        eprintln!("[system-audio-helper] {line}");
    }
}
