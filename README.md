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
  +-- Text pasted via wtype
```

## Demo

Hold AltGr and speak. Words appear in a floating panel as you talk, with a speech-bubble tail tracking your cursor. On release, the text animates to the cursor and gets pasted into whatever app has focus.

## Prerequisites

- **Wayland compositor** with wlr-layer-shell support (Hyprland, Sway, etc.)
- **PipeWire** (or PulseAudio) for audio capture
- **wtype** for text injection (`pacman -S wtype`)
- **hyprctl** for cursor position (Hyprland-specific, falls back to screen center otherwise)
- **NVIDIA GPU** with CUDA for the transcription server
- **Python 3.10+** with [uv](https://github.com/astral-sh/uv) for the ASR server
- User must be in the `input` group for evdev key capture: `sudo usermod -aG input $USER`

## Setup

### 1. Build the Rust client

```bash
cargo build --release
```

### 2. Start the transcription server

The `nemospeech/` directory contains a FastAPI server wrapping NVIDIA's Nemotron ASR model.

```bash
cd nemospeech
uv sync
uv run uvicorn server:app --host 0.0.0.0 --port 5051
```

The server downloads `nvidia/nemotron-speech-streaming-en-0.6b` on first run (~600MB).

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
  paste.rs       -- Text injection via wtype

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
| Text paste | wtype |
| Cursor position | hyprctl |

## License

MIT
