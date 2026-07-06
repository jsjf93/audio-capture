#!/usr/bin/env bash
# Downloads a ggml Whisper model for local transcription into models/
# (gitignored — models are hundreds of MB and re-downloadable).
#
# Usage:
#   bash scripts/download-model.sh            # base.en (~142MB) — good default
#   bash scripts/download-model.sh small.en   # (~466MB) — noticeably more accurate, slower
#   bash scripts/download-model.sh tiny.en    # (~75MB)  — fastest, roughest
set -euo pipefail

MODEL="${1:-base.en}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST_DIR="$REPO_ROOT/models"
DEST="$DEST_DIR/ggml-${MODEL}.bin"
URL="https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-${MODEL}.bin"

if [[ -f "$DEST" ]]; then
  echo "already present: $DEST"
  exit 0
fi

mkdir -p "$DEST_DIR"
echo "downloading ggml-${MODEL}.bin …"
curl -L --fail --progress-bar -o "$DEST.partial" "$URL"
mv "$DEST.partial" "$DEST"
echo "saved to $DEST"
