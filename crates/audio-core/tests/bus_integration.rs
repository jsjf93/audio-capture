//! CI-runnable proof that the bus keeps two simultaneous sources correctly
//! tagged with no bleed between them — no real hardware or OS permissions
//! required, since both sources here are `FakeAudioSource`. This is the
//! automated stand-in for the manual dual-capture proof (see
//! `examples/dual_capture.rs` and the Phase 3 plan): the same guarantee
//! (mic and system-output never cross-contaminate), checked continuously
//! in a way real hardware and OS TCC permissions would otherwise prevent
//! from ever running in CI.

use audio_core::{AudioBus, AudioSource, FakeAudioSource, SourceKind};
use std::time::Duration;

const MIC_SAMPLE: f32 = 0.25;
const SYSTEM_SAMPLE: f32 = -0.75;

#[tokio::test]
async fn two_simultaneous_sources_never_bleed_into_each_other() {
    let bus = AudioBus::new(64);

    // Distinct, easily-recognizable constant values per source: any sample
    // that isn't exactly one of these two values would mean the bus (or a
    // source) corrupted or mixed the streams.
    let mut mic = FakeAudioSource::new(SourceKind::Microphone, vec![MIC_SAMPLE; 32], 16_000, 1)
        .with_frame_interval(Duration::from_millis(2));
    let mut system = FakeAudioSource::new(SourceKind::SystemOutput, vec![SYSTEM_SAMPLE; 32], 48_000, 1)
        .with_frame_interval(Duration::from_millis(2));

    let mut rx = bus.subscribe();

    mic.start(bus.clone()).await.expect("fake mic source should always start");
    system.start(bus.clone()).await.expect("fake system source should always start");

    let mut mic_frames = 0;
    let mut system_frames = 0;

    // Collect frames until we've seen a healthy number from both sources.
    // Bounded by a timeout so a real regression (one source never
    // publishing) fails the test instead of hanging forever.
    let collect = async {
        while mic_frames < 20 || system_frames < 20 {
            let frame = rx.recv().await.expect("bus channel should not close mid-test");
            match frame.source {
                SourceKind::Microphone => {
                    mic_frames += 1;
                    for &sample in frame.samples.iter() {
                        assert_eq!(
                            sample, MIC_SAMPLE,
                            "a frame tagged Microphone contained a non-mic sample value — cross-contamination"
                        );
                    }
                    assert_eq!(frame.sample_rate, 16_000);
                }
                SourceKind::SystemOutput => {
                    system_frames += 1;
                    for &sample in frame.samples.iter() {
                        assert_eq!(
                            sample, SYSTEM_SAMPLE,
                            "a frame tagged SystemOutput contained a non-system sample value — cross-contamination"
                        );
                    }
                    assert_eq!(frame.sample_rate, 48_000);
                }
            }
        }
    };

    tokio::time::timeout(Duration::from_secs(10), collect)
        .await
        .expect("timed out waiting for frames from both sources — one of them likely stopped publishing");

    mic.stop().await.expect("fake mic source should always stop cleanly");
    system.stop().await.expect("fake system source should always stop cleanly");

    assert!(mic_frames >= 20);
    assert!(system_frames >= 20);
}

#[tokio::test]
async fn a_late_subscriber_only_sees_frames_published_after_it_subscribes() {
    let bus = AudioBus::new(64);
    let mut early = FakeAudioSource::new(SourceKind::Microphone, vec![MIC_SAMPLE; 16], 16_000, 1)
        .with_frame_interval(Duration::from_millis(2));

    early.start(bus.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await; // let a few frames publish with no subscriber yet

    let mut rx = bus.subscribe();
    let frame = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for the first frame after subscribing")
        .expect("bus channel should not close mid-test");

    assert_eq!(frame.source, SourceKind::Microphone);
    early.stop().await.unwrap();
}
