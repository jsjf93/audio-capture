//! Phase 2 verification tool: decode a raw capture of the Swift helper's
//! stdout (e.g. `AudioTapHelper > capture.bin`) into a playable WAV file,
//! and print a summary so a corrupted/silent/garbage capture is obvious
//! without needing to open an audio editor.
//!
//! Usage:
//!     cargo run -p audio-core --example decode_tap_capture -- <input.bin> <output.wav>

use audio_core::system_audio::protocol::{read_message, TapMessage};
use std::fs::File;
use std::io::BufReader;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: decode_tap_capture <input.bin> <output.wav>");
        std::process::exit(2);
    }
    let input_path = &args[1];
    let output_path = &args[2];

    let file = File::open(input_path).unwrap_or_else(|e| {
        eprintln!("failed to open {input_path}: {e}");
        std::process::exit(1);
    });
    let mut reader = BufReader::new(file);

    let mut audio_count = 0u64;
    let mut heartbeat_count = 0u64;
    let mut status_count = 0u64;
    let mut sample_rate: Option<u32> = None;
    let mut channels: Option<u8> = None;
    let mut all_samples: Vec<f32> = Vec::new();

    // Coarse per-second RMS so the silence/speech/silence pattern of a
    // manual test is visible at a glance, without needing to actually
    // listen to the decoded file.
    let mut per_second_sum_sq: Vec<f64> = Vec::new();
    let mut per_second_count: Vec<u64> = Vec::new();

    loop {
        match read_message(&mut reader) {
            Ok(None) => break,
            Ok(Some(TapMessage::Heartbeat)) => heartbeat_count += 1,
            Ok(Some(TapMessage::StatusEvent(event))) => {
                status_count += 1;
                println!(
                    "[status] level={} code={} message={}",
                    event.level, event.code, event.message
                );
            }
            Ok(Some(TapMessage::Audio(msg))) => {
                audio_count += 1;
                sample_rate.get_or_insert(msg.sample_rate);
                channels.get_or_insert(msg.channels);

                if sample_rate != Some(msg.sample_rate) || channels != Some(msg.channels) {
                    eprintln!(
                        "warning: format changed mid-stream (was {:?}Hz/{:?}ch, now {}Hz/{}ch) — unexpected, but continuing",
                        sample_rate, channels, msg.sample_rate, msg.channels
                    );
                }

                let ch = msg.channels.max(1) as usize;
                let sr = msg.sample_rate.max(1) as usize;
                for (i, &sample) in msg.samples.iter().enumerate() {
                    let frame_index = i / ch;
                    let global_frame = (all_samples.len() / ch) + frame_index;
                    let second = global_frame / sr;
                    if per_second_sum_sq.len() <= second {
                        per_second_sum_sq.resize(second + 1, 0.0);
                        per_second_count.resize(second + 1, 0);
                    }
                    per_second_sum_sq[second] += (sample as f64) * (sample as f64);
                    per_second_count[second] += 1;
                }

                all_samples.extend_from_slice(&msg.samples);
            }
            Err(e) => {
                eprintln!("fatal protocol error after {audio_count} audio messages: {e}");
                std::process::exit(1);
            }
        }
    }

    let Some(sample_rate) = sample_rate else {
        eprintln!("no audio messages found in {input_path} — capture is empty");
        std::process::exit(1);
    };
    let channels = channels.unwrap_or(1);

    let spec = hound::WavSpec {
        channels: channels as u16,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(output_path, spec).expect("failed to create WAV writer");
    for sample in &all_samples {
        writer.write_sample(*sample).expect("failed to write sample");
    }
    writer.finalize().expect("failed to finalize WAV file");

    let total_frames = all_samples.len() / channels.max(1) as usize;
    let duration_secs = total_frames as f64 / sample_rate as f64;

    println!("--- summary ---");
    println!("audio messages:     {audio_count}");
    println!("heartbeat messages: {heartbeat_count}");
    println!("status messages:    {status_count}");
    println!("format:             {sample_rate} Hz, {channels} channel(s)");
    println!("duration:           {duration_secs:.2}s ({total_frames} frames)");
    println!("wrote:              {output_path}");
    println!("--- per-second RMS (silence should read low, speech should read noticeably higher) ---");
    for (second, (&sum_sq, &count)) in per_second_sum_sq.iter().zip(&per_second_count).enumerate() {
        let rms = if count > 0 { (sum_sq / count as f64).sqrt() } else { 0.0 };
        println!("  t={second:>3}s  rms={rms:.5}");
    }
}
