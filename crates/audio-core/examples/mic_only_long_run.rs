//! Diagnostic tool (not a permanent fixture): run only the microphone for a
//! configurable duration and report how many frames actually arrived vs.
//! how many were expected, to isolate whether a mid-recording mic dropout
//! is specific to running alongside system-audio capture or happens on its
//! own too.
//!
//! Usage:
//!     cargo run -p audio-core --example mic_only_long_run -- <seconds>

use audio_core::{AudioBus, AudioSource, MicrophoneSource};
use std::time::Duration;

#[tokio::main]
async fn main() {
    let seconds: u64 = std::env::args()
        .nth(1)
        .expect("usage: mic_only_long_run <seconds>")
        .parse()
        .expect("seconds must be an integer");

    let bus = AudioBus::new(256);
    let mut rx = bus.subscribe();
    let mut mic = MicrophoneSource::new();

    mic.start(bus.clone()).await.expect("failed to start microphone capture");
    println!("mic-only capture running for {seconds}s...");

    let mut frame_count = 0u64;
    let mut last_frame_at = tokio::time::Instant::now();
    let mut longest_gap = Duration::from_secs(0);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);

    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Ok(_frame)) => {
                frame_count += 1;
                let gap = last_frame_at.elapsed();
                if gap > longest_gap {
                    longest_gap = gap;
                }
                last_frame_at = tokio::time::Instant::now();
            }
            Ok(Err(_)) => break,
            Err(_) => {
                let gap = last_frame_at.elapsed();
                println!("no frame received in the last 500ms (current gap: {gap:?})");
                if gap > longest_gap {
                    longest_gap = gap;
                }
            }
        }
    }

    mic.stop().await.expect("failed to stop microphone capture");

    let expected_frames = (seconds * 1000) / 20; // ~20ms per frame
    println!("--- summary ---");
    println!("frames received:  {frame_count} (expected roughly {expected_frames})");
    println!("longest gap between frames: {longest_gap:?}");
    println!("time since last frame at deadline: {:?}", last_frame_at.elapsed());
}
