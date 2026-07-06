//! Offline transcription of a WAV file — hardware-free verification of the
//! whisper integration, and the seed of the future replay harness (feed a
//! recorded real call through the pipeline deterministically).
//!
//! Usage:
//!   cargo run -p transcribe --example transcribe_wav -- <input.wav> [model_path]

use transcribe::{resample, Transcriber, WhisperTranscriber, WHISPER_SAMPLE_RATE};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let wav_path = args.next().unwrap_or_else(|| {
        eprintln!("usage: transcribe_wav <input.wav> [model_path]");
        std::process::exit(2);
    });
    let model_path = args.next().unwrap_or_else(|| "models/ggml-base.en.bin".to_string());

    let mut reader = hound::WavReader::open(&wav_path)?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|s| s as f32 / scale))
                .collect::<Result<_, _>>()?
        }
    };

    // Downmix interleaved channels to mono, then bring to Whisper's rate.
    let mono: Vec<f32> = if spec.channels > 1 {
        samples
            .chunks(spec.channels as usize)
            .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
            .collect()
    } else {
        samples
    };
    let samples_16k = resample(&mono, spec.sample_rate, WHISPER_SAMPLE_RATE);

    eprintln!(
        "{}: {:.1}s of audio at {} Hz, {} channel(s); loading model {model_path} …",
        wav_path,
        mono.len() as f32 / spec.sample_rate as f32,
        spec.sample_rate,
        spec.channels,
    );
    let mut transcriber = WhisperTranscriber::new(model_path.as_ref())?;

    let started = std::time::Instant::now();
    let text = transcriber.transcribe(&samples_16k)?;
    eprintln!("transcribed in {:.2}s:", started.elapsed().as_secs_f32());
    println!("{text}");
    Ok(())
}
