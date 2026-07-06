//! The core proof for the whole project so far: capture the microphone and
//! system output *simultaneously*, demux by the bus's `source` tag into two
//! separate WAV files, and let a human (or a later automated check) confirm
//! there's no bleed either direction — e.g. speak a sentence into the mic
//! while a different, distinguishable clip plays through the speakers, then
//! listen to both resulting files and confirm each contains only what it
//! should.
//!
//! Usage:
//!     cargo run -p audio-core --example dual_capture -- <seconds> <mic_out.wav> <system_out.wav>

use audio_core::{AudioBus, AudioSource, MicrophoneSource, SourceKind, SystemOutputSource};
use std::time::Duration;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: dual_capture <seconds> <mic_out.wav> <system_out.wav>");
        std::process::exit(2);
    }
    let seconds: u64 = args[1].parse().expect("seconds must be an integer");
    let mic_path = args[2].clone();
    let system_path = args[3].clone();

    let bus = AudioBus::new(256);
    let mut rx = bus.subscribe();

    let mut mic = MicrophoneSource::new();
    let mut system = SystemOutputSource::new();

    println!("starting microphone capture...");
    mic.start(bus.clone()).await.expect("failed to start microphone capture");
    println!("starting system-audio capture...");
    system
        .start(bus.clone())
        .await
        .expect("failed to start system-audio capture");

    println!(
        "capturing both streams for {seconds}s — speak into the mic and/or play something \
         through your speakers now"
    );

    let mut mic_writer: Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>> = None;
    let mut system_writer: Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>> = None;
    let mut mic_frame_count = 0u64;
    let mut system_frame_count = 0u64;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Ok(frame)) => {
                let (writer_slot, path, count) = match frame.source {
                    SourceKind::Microphone => (&mut mic_writer, &mic_path, &mut mic_frame_count),
                    SourceKind::SystemOutput => (&mut system_writer, &system_path, &mut system_frame_count),
                };

                let writer = writer_slot.get_or_insert_with(|| {
                    let spec = hound::WavSpec {
                        channels: frame.channels as u16,
                        sample_rate: frame.sample_rate,
                        bits_per_sample: 32,
                        sample_format: hound::SampleFormat::Float,
                    };
                    println!(
                        "first frame from {:?}: {} Hz, {} channel(s) -> {}",
                        frame.source, frame.sample_rate, frame.channels, path
                    );
                    hound::WavWriter::create(path, spec).expect("failed to create WAV writer")
                });

                for &sample in frame.samples.iter() {
                    writer.write_sample(sample).expect("failed to write sample");
                }
                *count += 1;
            }
            Ok(Err(_)) => break, // bus closed
            Err(_) => {
                // no frame in the last 500ms from either source — fine,
                // just keep waiting for the deadline
            }
        }
    }

    mic.stop().await.expect("failed to stop microphone capture");
    system.stop().await.expect("failed to stop system-audio capture");

    if let Some(writer) = mic_writer {
        writer.finalize().expect("failed to finalize mic WAV");
    } else {
        eprintln!("warning: no microphone frames were ever received — {mic_path} was not written");
    }
    if let Some(writer) = system_writer {
        writer.finalize().expect("failed to finalize system-audio WAV");
    } else {
        eprintln!("warning: no system-audio frames were ever received — {system_path} was not written");
    }

    println!("--- summary ---");
    println!("mic frames:    {mic_frame_count} -> {mic_path}");
    println!("system frames: {system_frame_count} -> {system_path}");
    println!("done. Listen to both files and confirm: {mic_path} contains only your voice, {system_path} contains only what was playing through the speakers.");
}
