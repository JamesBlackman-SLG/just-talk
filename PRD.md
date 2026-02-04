# JustSpeak - Voice Transcription Service for Wayland

## Overview

A Rust application that captures voice input while the user holds AltGr (Right Alt), transcribes it locally via whisper.cpp (CUDA-accelerated), and pastes the result at the cursor. During transcription, words appear as a large animated overlay that fly/shrink towards the cursor position.

## Environment

| Component | Status |
|---|---|
| OS | CachyOS (Arch), Linux 6.18.8, Wayland |
| Rust | 1.93.0 |
| whisper.cpp | 1.8.2 CUDA, `whisper-cli` at `/usr/bin/whisper-cli` |
| libwhisper | `/usr/lib/libwhisper.so` with pkg-config |
| Models | `~/.cache/whisper/ggml-base.bin`, `~/code/whisper.cpp/models/ggml-base.en.bin` |
| Audio | PipeWire + PulseAudio compat layer |
| Display | Wayland (`wayland-1`) |

---

## Architecture

```
┌─────────────┐    ┌──────────────┐    ┌─────────────┐    ┌──────────┐    ┌────────────┐
│ evdev input │───>│ Audio capture │───>│ whisper.cpp  │───>│ Overlay  │───>│ Text paste │
│ (AltGr key) │    │ (PipeWire)   │    │ transcribe   │    │ animate  │    │ (wtype)    │
└─────────────┘    └──────────────┘    └─────────────┘    └──────────┘    └────────────┘
```

### Key Design Decisions

- **Global key capture**: Use evdev (`/dev/input/event*`) to detect AltGr hold/release globally regardless of focused window. Requires `input` group membership or running as root.
- **Audio**: Use PipeWire (via `pipewire` or `libpulse` crate) to capture microphone audio into a WAV buffer in memory.
- **Transcription**: Shell out to `whisper-cli` for v1 simplicity. The audio buffer is written to a temp file, `whisper-cli` processes it, output is captured. (Future: FFI to libwhisper for streaming.)
- **Text pasting**: Use `wtype` (Wayland typing tool) to type the transcribed text at the cursor. Fallback: `wl-copy` + `wtype`.
- **Overlay**: Use `wlr-layer-shell` protocol via `smithay-client-toolkit` to render a transparent fullscreen overlay. Text rendered with `tiny-skia` or `cosmic-text`. Animation ticked at ~60fps.

---

## Tasks

### Phase 1: Project Scaffolding
- [ ] **1.1** Initialize Cargo project with workspace structure
- [ ] **1.2** Add core dependencies to `Cargo.toml` (evdev, pipewire/cpal, wayland-client, smithay-client-toolkit, tiny-skia, cosmic-text)
- [ ] **1.3** Create module structure: `input`, `audio`, `transcribe`, `overlay`, `paste`, `main`
- [ ] **1.4** Add config struct with defaults (model path, AltGr keycode, audio sample rate)

### Phase 2: Global Key Detection (evdev)
- [ ] **2.1** Enumerate `/dev/input/event*` devices and find keyboards
- [ ] **2.2** Listen for AltGr (KEY_RIGHTALT, code 100) press and release events
- [ ] **2.3** Implement state machine: `Idle -> Recording -> Transcribing -> Animating -> Pasting -> Idle`
- [ ] **2.4** Run input listener on dedicated thread, communicate state via channels

### Phase 3: Audio Capture (PipeWire)
- [ ] **3.1** Open default microphone source via `cpal` (which supports PipeWire backend)
- [ ] **3.2** On AltGr press: start buffering audio samples (16kHz mono f32, what whisper expects)
- [ ] **3.3** On AltGr release: stop buffering, write samples to WAV in `/tmp`
- [ ] **3.4** Handle edge cases: very short press (<0.3s) ignored, very long press (>30s) auto-stops

### Phase 4: Transcription (whisper-cli)
- [ ] **4.1** Invoke `whisper-cli` as subprocess with temp WAV file
- [ ] **4.2** Use flags: `-m ~/.cache/whisper/ggml-base.bin --no-timestamps -l en -otxt`
- [ ] **4.3** Parse text output, strip timestamps and whitespace artifacts
- [ ] **4.4** Handle errors: empty audio, whisper failure, timeout (10s max)
- [ ] **4.5** Clean up temp files after transcription

### Phase 5: Text Pasting (wtype)
- [ ] **5.1** Check for `wtype` availability at startup, warn if missing
- [ ] **5.2** After transcription + animation complete, invoke `wtype` with the transcribed text
- [ ] **5.3** Handle special characters and newlines properly in wtype invocation
- [ ] **5.4** Add small delay before typing to ensure focus is on target window

### Phase 6: Overlay Animation (wlr-layer-shell)
- [ ] **6.1** Create transparent fullscreen overlay surface using `wlr-layer-shell-unstable-v1` via smithay-client-toolkit
- [ ] **6.2** Set layer to `overlay` (topmost), exclusive zone -1 (don't take input), transparent background
- [ ] **6.3** Render transcribed words in large font (48-72px) centered on screen using `tiny-skia` + `cosmic-text`
- [ ] **6.4** Animate: words start large and centered, then shrink and translate toward cursor position over ~1s
- [ ] **6.5** Get cursor position via compositor (wlr-foreign-toplevel or pointer position from Wayland seat)
- [ ] **6.6** Easing function for smooth fly-in (ease-out-cubic or similar)
- [ ] **6.7** Destroy overlay surface after animation completes

### Phase 7: Integration & State Machine
- [ ] **7.1** Wire all modules together in `main.rs` with async event loop (tokio)
- [ ] **7.2** Implement full lifecycle: key-down starts recording, key-up triggers transcribe, transcription triggers overlay + paste
- [ ] **7.3** Visual feedback during recording: small pulsing indicator via overlay (e.g., red dot)
- [ ] **7.4** Visual feedback during transcription: loading spinner or "Transcribing..." text on overlay
- [ ] **7.5** Graceful shutdown on SIGINT/SIGTERM

### Phase 8: Polish & Edge Cases
- [ ] **8.1** Handle multiple rapid key presses (debounce, queue, or ignore while busy)
- [ ] **8.2** Configurable model path via CLI arg or env var (`JUSTSPEAK_MODEL`)
- [ ] **8.3** Configurable key (default AltGr) via CLI arg
- [ ] **8.4** Log output with `tracing` crate for debugging
- [ ] **8.5** Systemd user service file for autostart
- [ ] **8.6** Error messages shown on overlay instead of silent failures

---

## Dependencies (Planned)

| Crate | Purpose |
|---|---|
| `evdev` | Read keyboard events from /dev/input |
| `cpal` | Cross-platform audio capture (PipeWire via ALSA backend) |
| `hound` | Write WAV files |
| `tokio` | Async runtime |
| `smithay-client-toolkit` | Wayland client protocols including wlr-layer-shell |
| `wayland-client` | Low-level Wayland protocol |
| `tiny-skia` | 2D software rendering for overlay |
| `cosmic-text` | Text shaping and layout |
| `tracing` / `tracing-subscriber` | Structured logging |
| `clap` | CLI argument parsing |

## External Tools Required

| Tool | Purpose | Install |
|---|---|---|
| `wtype` | Type text into focused Wayland window | `pacman -S wtype` |
| `whisper-cli` | Transcribe audio (already installed) | `pacman -S whisper.cpp-cuda` |

---

## Non-Goals (for v1)

- Streaming/real-time transcription (words appearing as you speak) -- future v2
- Multiple language support -- English only for now
- GUI settings panel
- Custom wake word
- Cloud-based transcription
- X11/XWayland support

## Notes

- The user must be in the `input` group to read evdev without root: `sudo usermod -aG input $USER`
- whisper.cpp with CUDA will use the GPU for fast transcription; base model should complete in <1s for typical utterances
- `wtype` must be installed separately for text injection on Wayland
- The overlay uses wlr-layer-shell which is supported on wlroots-based compositors (Sway, Hyprland, etc.) and KDE Plasma 6+
