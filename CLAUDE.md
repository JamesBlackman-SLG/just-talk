# just-talk (justspeak)

Voice-to-text tool for Hyprland/Wayland. Hold Right Alt (AltGr) or press a MIDI foot pedal to record speech, which gets transcribed and typed into the focused window.

## Architecture

Single-binary Rust app. Tokio async runtime for the main event loop; blocking threads for evdev input, MIDI, and Wayland overlay.

### Module Map

- `main.rs` — Event loop: Idle/Recording state machine, streaming WebSocket transcription, HTTP fallback
- `audio.rs` — Microphone capture via `cpal` (16kHz mono f32). `AudioBufferHandle` allows snapshot from other threads
- `blip.rs` — Synthesizes per-character blip tones via `cpal` output. Each blip plays root + perfect fifth (3:2 ratio). Pitch varies by word length: short words ~1650Hz, long words ~990Hz. 18ms per character, ~6ms tone with quadratic decay envelope. Opt-in: only active when `blip_device` is configured. Routes audio to a specific PipeWire sink via `PIPEWIRE_NODE` env var
- `input.rs` — Reads `/dev/input/event*` via `evdev` for AltGr press/release and double-tap detection. Requires `input` group membership
- `midi.rs` — Listens for MIDI CC 85 from FS-1-WL foot pedal via `midir`. Maps to same `KeyEvent` as keyboard
- `overlay.rs` — Wayland layer-shell overlay (smithay-client-toolkit + tiny-skia + cosmic-text). Shows live transcription, cancel button, fly-in animation
- `paste.rs` — Types text into focused window. Detects XWayland vs native Wayland via `hyprctl`. Uses `xdotool type` or `wtype`. With blips enabled: pastes word-by-word, blip audio paces the delay between words. Without: instant bulk paste
- `transcribe.rs` — HTTP and WebSocket client for nemospeech server (configurable URL)
- `config.rs` — TOML config from `~/.config/justspeak/config.toml`. Priority: CLI > env `NEMOSPEECH_URL` > config file > default

### Config File

`~/.config/justspeak/config.toml`:

```toml
# Toggle blip sounds on/off (persisted by double-tap AltGr at runtime)
blips_enabled = true

# Optional: route blip audio to a specific PipeWire sink.
# Use PipeWire node name (from `pw-cli list-objects Node`).
# If omitted, blips are disabled and text pastes instantly.
blip_device = "alsa_output.pci-0000_0d_00.4.analog-stereo"

[server]
url = "http://localhost:5051"
```

The `blip_device` key enables the "Mother from Alien" typing effect — word-by-word paste with synthesized blip tones. Each blip is a root note + perfect fifth interval. Pitch varies by word length (short words ≈ 1650Hz, long words ≈ 990Hz, 18ms per character). Without this key, paste is instant and silent.

Double-tap AltGr to toggle blips on/off at runtime. Plays a two-note confirmation tone (ascending C5→G5 for ON, descending G5→C5 for OFF). State persists to config file. MIDI pedal does NOT trigger the toggle — keyboard-only.

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
