# just-talk (justspeak)

Voice-to-text tool for Hyprland/Wayland. Hold Right Alt (AltGr) or press a MIDI foot pedal to record speech, which gets transcribed and typed into the focused window.

## Architecture

Single-binary Rust app. Tokio async runtime for the main event loop; blocking threads for evdev input, MIDI, and Wayland overlay.

### Module Map

- `main.rs` — Event loop: Idle/Recording state machine, streaming WebSocket transcription, HTTP fallback
- `audio.rs` — Microphone capture via `cpal` (16kHz mono f32). `AudioBufferHandle` allows snapshot from other threads
- `blip.rs` — Synthesizes per-character blip tones (880Hz + harmonics, ~10ms) and plays via `cpal` output. Used during paste for the "Mother from Alien" typing effect
- `input.rs` — Reads `/dev/input/event*` via `evdev` for AltGr press/release. Requires `input` group membership
- `midi.rs` — Listens for MIDI CC 85 from FS-1-WL foot pedal via `midir`. Maps to same `KeyEvent` as keyboard
- `overlay.rs` — Wayland layer-shell overlay (smithay-client-toolkit + tiny-skia + cosmic-text). Shows live transcription, cancel button, fly-in animation
- `paste.rs` — Types text into focused window. Detects XWayland vs native Wayland via `hyprctl`. Uses `xdotool type` or `wtype` with per-character delay synced to blip audio
- `transcribe.rs` — HTTP and WebSocket client for nemospeech server (configurable URL)
- `config.rs` — TOML config from `~/.config/justspeak/config.toml`. Priority: CLI > env `NEMOSPEECH_URL` > config file > default

### External Dependencies (runtime)

- `wtype` — Virtual keyboard for native Wayland windows
- `xdotool` — Keystroke injection for XWayland windows
- `wl-copy` — Clipboard backup
- `hyprctl` — Detect XWayland vs Wayland, cursor position

### Transcription Server

Nemospeech (separate service). See `nemospeech/` directory and `compose.yml`. Supports:
- HTTP POST `/transcribe` with WAV multipart (fallback)
- WebSocket `/ws/transcribe` for streaming (primary)

## Conventions

- Rust 2024 edition
- `anyhow::Result` for error handling throughout
- `tracing` for structured logging (info/warn/error/debug)
- No unit tests currently
- Blocking I/O runs on std threads; async only for the main event loop and WebSocket streaming
