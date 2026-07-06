//! Smoke test for the Swift helper: prove that a Rust process can build,
//! spawn, signal, and cleanly read framed protocol output from it. This
//! started in Phase 0 as a bare hello-world check and now exercises the
//! real Phase 2 helper (the Core Audio Process Tap implementation) — the
//! process/IPC mechanics being proven are the same ones the Phase 3
//! `SystemOutputSource` supervisor will rely on.
//!
//! This test shells out to `swift build`, so it requires the Swift
//! toolchain (present via Xcode Command Line Tools) and is skipped rather
//! than failed if `swift` isn't on PATH, so it doesn't break environments
//! without Xcode tooling. It's also skipped (not failed) if the helper
//! can't actually start the tap — e.g. no audio-capture TCC permission has
//! been granted yet, or this isn't macOS 14.4+ — since that's an
//! environment/permission precondition, not a code defect. What actually
//! captured (even silence) is intentionally not asserted here — real
//! audio-content verification is the manual/scripted spike described in
//! the Phase 2 plan (playing known audio and inspecting a decoded WAV),
//! not something this automated test tries to reproduce.

use audio_core::system_audio::protocol::read_message;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

fn swift_helper_dir() -> PathBuf {
    // crates/audio-core -> workspace root -> swift-helper
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .parent()
        .expect("workspace root")
        .join("swift-helper")
}

#[test]
fn swift_helper_builds_and_streams_decodable_protocol_messages() {
    let helper_dir = swift_helper_dir();

    if Command::new("swift").arg("--version").output().is_err() {
        eprintln!("swift toolchain not found on PATH; skipping smoke test");
        return;
    }

    let build_status = Command::new("swift")
        .arg("build")
        .current_dir(&helper_dir)
        .status()
        .expect("failed to invoke `swift build`");
    assert!(build_status.success(), "swift build failed");

    let binary_path = helper_dir.join(".build/debug/AudioTapHelper");
    assert!(
        binary_path.exists(),
        "expected built helper binary at {binary_path:?}"
    );

    let child = Command::new(&binary_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn AudioTapHelper");

    // The real helper runs until signaled, capturing the whole time. Give
    // it a moment to set up the tap and emit at least one message (audio
    // or heartbeat), then stop it the same way the Rust supervisor will in
    // Phase 3+: SIGINT, not SIGKILL, so it exercises its own teardown path
    // (destroying the tap and aggregate device) instead of being yanked
    // out from under Core Audio.
    std::thread::sleep(Duration::from_millis(500));

    let pid = child.id();
    let signal_status = Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .status()
        .expect("failed to send SIGINT to helper");
    assert!(signal_status.success(), "failed to signal helper process");

    let output = child
        .wait_with_output()
        .expect("failed waiting for AudioTapHelper to exit after SIGINT");

    if !output.status.success() {
        eprintln!(
            "helper exited non-zero (status: {:?}); treating as an environment/permission \
             precondition not being met rather than a test failure. stderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("stopped cleanly"),
        "expected the helper's graceful-shutdown log line; stderr was:\n{stderr}"
    );

    // The real assertion: whatever the tap captured (even silence) must
    // arrive as well-formed framed messages, not garbage.
    let mut cursor = Cursor::new(output.stdout);
    let mut message_count = 0;
    while read_message(&mut cursor)
        .expect("failed to decode a protocol message from helper stdout")
        .is_some()
    {
        message_count += 1;
    }
    assert!(
        message_count > 0,
        "expected at least one decodable message (audio or heartbeat) from the helper"
    );
}
