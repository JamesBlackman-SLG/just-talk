#!/bin/bash
# Update nemospeech to use the latest model and NeMo toolkit.
#
# The model (nvidia/nemotron-speech-streaming-en-0.6b) is downloaded at
# container startup by NeMo's from_pretrained() and cached in a Docker
# volume (nemo-cache). This means "docker compose build" alone won't
# pull a newer model — the old cached weights persist in the volume.
#
# Similarly, the NeMo toolkit is installed from git main at build time,
# so Docker's layer cache can serve a stale version.
#
# This script:
#   1. Stops the running container
#   2. Removes the model cache volume so from_pretrained() re-downloads
#   3. Rebuilds the image with --no-cache so pip fetches latest NeMo
#   4. Starts the container (model downloads on first startup)

set -euo pipefail
cd "$(dirname "$0")"

echo "Stopping nemospeech..."
docker compose down

echo "Removing model cache volume..."
docker volume rm nemospeech_nemo-cache 2>/dev/null || true

echo "Rebuilding image (no cache, pulls latest NeMo toolkit)..."
docker compose build --no-cache

echo "Starting nemospeech..."
docker compose up -d

echo "Done. Model will download on first startup — watch logs with:"
echo "  docker compose logs -f"
