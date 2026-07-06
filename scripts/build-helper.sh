#!/usr/bin/env bash
# Builds the Swift Process Tap helper in release mode for this machine's
# native architecture, plus a best-effort cross-build for the other of
# arm64/x86_64 via Rosetta 2 emulation, and copies both into
# src-tauri/binaries/ under the target-triple-suffixed names Tauri's
# `externalBin` sidecar mechanism expects (see tauri.conf.json's
# bundle.externalBin). Building both lets `tauri build --target
# universal-apple-darwin` produce one binary that runs on either Mac
# architecture — Intel support is a confirmed project requirement, even
# though no physical Intel Mac is available for local testing; Rosetta
# lets us at least build and functionally smoke-test that binary here.
#
# Run automatically by `tauri build` via beforeBuildCommand; safe to run
# by hand too when you just want a fresh release build of the helper(s).
set -euo pipefail

cd "$(dirname "$0")/.."
mkdir -p src-tauri/binaries

HOST_ARCH="$(uname -m)"
case "$HOST_ARCH" in
  arm64) NATIVE_TRIPLE="aarch64-apple-darwin"; OTHER_TRIPLE="x86_64-apple-darwin"; OTHER_ARCH_FLAG="x86_64" ;;
  x86_64) NATIVE_TRIPLE="x86_64-apple-darwin"; OTHER_TRIPLE="aarch64-apple-darwin"; OTHER_ARCH_FLAG="arm64" ;;
  *) echo "error: unrecognized host architecture '$HOST_ARCH'" >&2; exit 1 ;;
esac

echo "==> Building AudioTapHelper for $NATIVE_TRIPLE (native, release)"
(cd swift-helper && swift build -c release)
cp "swift-helper/.build/release/AudioTapHelper" "src-tauri/binaries/audio-tap-helper-${NATIVE_TRIPLE}"
chmod +x "src-tauri/binaries/audio-tap-helper-${NATIVE_TRIPLE}"
echo "==> Copied native helper to src-tauri/binaries/audio-tap-helper-${NATIVE_TRIPLE}"

SCRATCH_DIR=".build-${OTHER_ARCH_FLAG}"
echo "==> Attempting cross-build for $OTHER_TRIPLE via Rosetta ($OTHER_ARCH_FLAG emulation)"
if (cd swift-helper && arch -"$OTHER_ARCH_FLAG" swift build -c release --scratch-path "$SCRATCH_DIR") 2>/tmp/build-helper-cross.log; then
  OTHER_BINARY="$(find "swift-helper/$SCRATCH_DIR" -name AudioTapHelper -type f -perm +111 | head -1)"
  if [ -n "$OTHER_BINARY" ]; then
    cp "$OTHER_BINARY" "src-tauri/binaries/audio-tap-helper-${OTHER_TRIPLE}"
    chmod +x "src-tauri/binaries/audio-tap-helper-${OTHER_TRIPLE}"
    echo "==> Copied cross-built helper to src-tauri/binaries/audio-tap-helper-${OTHER_TRIPLE}"
  else
    echo "warning: cross-build reported success but the binary wasn't found; skipping ${OTHER_TRIPLE}" >&2
  fi
else
  echo "warning: could not cross-build for ${OTHER_TRIPLE} (see /tmp/build-helper-cross.log — Rosetta may not be installed). Skipping." >&2
  echo "         Only ${NATIVE_TRIPLE} will be available; a universal build won't be possible until this succeeds." >&2
fi

# `tauri build --target universal-apple-darwin` lipo-merges the main Rust
# binary automatically, but expects sidecars to *already* be a single
# universal binary named with the `-universal-apple-darwin` suffix — it
# won't merge our two arch-specific sidecar files itself.
if [ -f "src-tauri/binaries/audio-tap-helper-aarch64-apple-darwin" ] && [ -f "src-tauri/binaries/audio-tap-helper-x86_64-apple-darwin" ]; then
  echo "==> Both architectures present — creating universal sidecar via lipo"
  lipo -create \
    -output "src-tauri/binaries/audio-tap-helper-universal-apple-darwin" \
    "src-tauri/binaries/audio-tap-helper-aarch64-apple-darwin" \
    "src-tauri/binaries/audio-tap-helper-x86_64-apple-darwin"
  chmod +x "src-tauri/binaries/audio-tap-helper-universal-apple-darwin"
  echo "==> Copied universal helper to src-tauri/binaries/audio-tap-helper-universal-apple-darwin"
fi
