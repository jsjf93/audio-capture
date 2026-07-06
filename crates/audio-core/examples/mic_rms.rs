//! Standalone Phase 1 proof: exercise `MicrophoneSource` with no Tauri, no
//! UI, no button clicks. Run with:
//!
//!     cargo run -p audio-core --example mic_rms
//!
//! macOS will prompt for microphone permission the first time this runs.
//! Speak into your mic during the 10-second window and watch the RMS
//! numbers rise from near-zero; stay quiet and they should stay low.

use audio_core::{AudioBus, AudioSource, MicrophoneSource};
use std::time::Duration;

#[tokio::main]
async fn main() {
    let bus = AudioBus::new(64);
    let mut mic = MicrophoneSource::new();

    println!("Starting microphone capture for 10 seconds — speak into your mic to see RMS rise.");
    mic.start(bus.clone())
        .await
        .expect("failed to start microphone capture");

    let mut rx = bus.subscribe();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);

    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Ok(frame)) => println!("[{:?}] rms = {:.4}", frame.source, frame.rms()),
            Ok(Err(_)) => break,
            Err(_) => println!("(no frames received in the last 500ms)"),
        }
    }

    mic.stop().await.expect("failed to stop microphone capture");
    println!("Stopped cleanly.");
}
