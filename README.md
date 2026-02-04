# JustTalk

Voice-to-text for Wayland. Hold a key, speak, release -- your words appear at the cursor.

JustTalk captures microphone audio while you hold **Right Alt (AltGr)**, streams it to a local [NVIDIA NeMo](https://github.com/NVIDIA/NeMo) ASR server for transcription, displays a live animated overlay with the transcribed text, then pastes the result at your cursor position when you release the key.

Built for **Hyprland** on Wayland with CUDA-accelerated transcription.

## How it works

```
AltGr held:
  +-- Mic recording starts (16kHz mono via PipeWire/cpal)
  +-- Overlay appears with speech-bubble tail pointing at cursor
  +-- Every ~1s: audio buffer snapshot -> nemospeech server -> overlay text update
  +-- New characters animate in with a per-letter grow effect

AltGr released:
  +-- Final transcription of complete audio
  +-- Text flies toward cursor with bezier spiral animation
  +-- Text pasted at cursor (wtype for Wayland apps, xdotool for XWayland/Electron)
```

## Demo

Hold AltGr and speak. Words appear in a floating panel as you talk, with a speech-bubble tail tracking your cursor. On release, the text animates to the cursor and gets pasted into whatever app has focus.

## Prerequisites

### Hardware

- **NVIDIA GPU** with CUDA support (tested on RTX 4090; the Nemotron 0.6B model uses ~2 GB VRAM)

### System

- **Linux** with a **Wayland compositor** supporting wlr-layer-shell (Hyprland, Sway, etc.)
- **Hyprland** recommended — `hyprctl` is used for cursor position and XWayland window detection
- **PipeWire** (or PulseAudio) for audio capture
- User must be in the `input` group for evdev key capture:
  ```bash
  sudo usermod -aG input $USER
  ```
  Log out and back in after adding the group.

### Client packages

Install via pacman (Arch/CachyOS) or your distro's package manager:

```bash
sudo pacman -S wtype wl-clipboard xdotool
```

| Package | Purpose |
|---------|---------|
| **wtype** | Text injection for native Wayland apps |
| **wl-clipboard** | Clipboard access (`wl-copy`) — text is always copied to clipboard as a backup |
| **xdotool** | Text injection for XWayland apps (Electron/Chromium: WhatsApp Web, Cursor, VS Code, etc.) |

### Rust toolchain

- **Rust 1.85+** (edition 2024) — install via [rustup](https://rustup.rs/)

### Transcription server (choose one)

**Option A: Docker (recommended)**

Requires [Docker](https://docs.docker.com/engine/install/) and [NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/install-guide.html):

```bash
sudo pacman -S docker nvidia-container-toolkit
sudo systemctl enable --now docker
```

**Option B: Native Python**

- **Python 3.10+**
- **[uv](https://github.com/astral-sh/uv)** package manager
- **CUDA toolkit** and **cuDNN** matching your driver version
- **libsndfile** (`sudo pacman -S libsndfile`)

## Setup

### 1. Build the Rust client

```bash
cargo build --release
```

### 2. Start the transcription server

The `nemospeech/` directory contains a FastAPI server wrapping NVIDIA's Nemotron ASR model (`nvidia/nemotron-speech-streaming-en-0.6b`, ~600 MB downloaded on first run).

**Option A: Docker**

```bash
cd nemospeech
docker build -t nemospeech .
docker run --gpus all -p 5051:5051 -v nemospeech-cache:/root/.cache nemospeech
```

The `-v nemospeech-cache:/root/.cache` volume caches the model between container restarts.

**Option B: Native Python**

```bash
cd nemospeech
uv sync
uv run python server.py
```

### 3. Run JustTalk

```bash
./target/release/justspeak
```

Options:

| Flag | Description |
|------|-------------|
| `--server URL` | Nemospeech server URL (default: `http://localhost:5051`, or `NEMOSPEECH_URL` env var) |
| `--no-overlay` | Disable the visual overlay, just paste the final transcription |

## Architecture

```
src/
  main.rs        -- State machine: key events, periodic transcription loop, orchestration
  input.rs       -- evdev listener for AltGr on dedicated threads
  audio.rs       -- cpal mic capture, 16kHz mono, WAV encoding via hound
  transcribe.rs  -- HTTP client posting WAV to nemospeech server (ureq multipart)
  overlay.rs     -- SCTK 0.19 + wlr-layer-shell overlay with cosmic-text rendering
  paste.rs       -- Text injection (wtype for Wayland, xdotool for XWayland)

nemospeech/
  server.py      -- FastAPI server wrapping NVIDIA NeMo ASR
  talk.py        -- Standalone voice chat demo (record -> transcribe -> LLM)
```

### Overlay features

- Dark rounded panel with border, positioned at upper-third of screen
- Speech-bubble tail dynamically tracks cursor position (all four directions)
- Per-character grow-in animation as new words arrive from transcription
- Fly-out animation on release: quadratic bezier path with spiral oscillation and comet trail
- Pulsing red recording indicator dot

### Threading model

- **Main thread** (tokio): key event handling, orchestration
- **Overlay thread**: Wayland event loop (`blocking_dispatch`), receives commands via `std::sync::mpsc`
- **Transcription threads**: `std::thread::spawn` for each periodic/final transcription
- **evdev threads**: dedicated threads per input device for key capture

## Tech stack

| Component | Technology |
|-----------|-----------|
| Language | Rust (edition 2024) |
| Audio capture | cpal + hound |
| Key capture | evdev |
| Wayland overlay | smithay-client-toolkit 0.19 + wlr-layer-shell |
| Text rendering | cosmic-text + tiny-skia pixel ops |
| ASR | NVIDIA NeMo (Nemotron 0.6B streaming model) |
| HTTP client | ureq 3 (multipart) |
| Text paste | wtype (Wayland) + xdotool (XWayland) + wl-clipboard (backup) |
| Cursor position | hyprctl |

## License

MIT
