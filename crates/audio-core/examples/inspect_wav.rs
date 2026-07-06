//! Reusable manual-verification tool: print duration and per-second RMS for
//! any WAV file, so a capture's content is inspectable at a glance without
//! opening an audio editor. Used to verify `dual_capture`'s output, and
//! generally useful for any future manual capture verification (e.g. Phase
//! 4's chaos testing).
//!
//! Usage:
//!     cargo run -p audio-core --example inspect_wav -- <file.wav>

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("usage: inspect_wav <file.wav>");
        std::process::exit(2);
    }

    let mut reader = hound::WavReader::open(&args[1]).unwrap_or_else(|e| {
        eprintln!("failed to open {}: {e}", args[1]);
        std::process::exit(1);
    });
    let spec = reader.spec();
    let sample_rate = spec.sample_rate as usize;
    let channels = spec.channels.max(1) as usize;

    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap()).collect(),
        hound::SampleFormat::Int => reader
            .samples::<i32>()
            .map(|s| s.unwrap() as f32 / i32::MAX as f32)
            .collect(),
    };

    let total_frames = samples.len() / channels;
    let duration = total_frames as f64 / sample_rate as f64;

    println!("file:        {}", args[1]);
    println!("format:      {} Hz, {} channel(s), {:?}", spec.sample_rate, spec.channels, spec.sample_format);
    println!("duration:    {duration:.2}s ({total_frames} frames)");
    println!("--- per-second RMS ---");

    let mut second = 0;
    while second * sample_rate < total_frames {
        let start_frame = second * sample_rate;
        let end_frame = ((second + 1) * sample_rate).min(total_frames);
        let start = start_frame * channels;
        let end = end_frame * channels;
        let slice = &samples[start..end];
        let rms = if slice.is_empty() {
            0.0
        } else {
            (slice.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / slice.len() as f64).sqrt()
        };
        println!("  t={second:>3}s  rms={rms:.5}");
        second += 1;
    }
}
