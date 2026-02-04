mod audio;
mod input;
mod overlay;
mod paste;
mod transcribe;

use anyhow::Result;
use clap::Parser;
use input::KeyEvent;
use overlay::OverlayCommand;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(name = "justspeak", about = "Voice transcription for Wayland")]
struct Args {
    /// Nemospeech server URL (default: http://localhost:5051)
    #[arg(short, long)]
    server: Option<String>,

    /// Disable the fly-in overlay animation
    #[arg(long)]
    no_overlay: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Recording,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    // Preflight checks
    paste::check_wtype()?;
    let transcriber = Arc::new(transcribe::Transcriber::new(args.server)?);
    let audio = audio::AudioCapture::new()?;
    let audio_handle = audio.buffer_handle();

    info!("justspeak ready - hold Right Alt (AltGr) to speak");

    // Key event channel
    let (tx, mut rx) = mpsc::unbounded_channel();
    input::spawn_listener(tx)?;

    let mut state = State::Idle;

    while let Some(event) = rx.recv().await {
        match (state, event) {
            (State::Idle, KeyEvent::AltGrPressed) => {
                audio.start_recording();

                if !args.no_overlay {
                    // Spawn overlay thread
                    let overlay_handle = match overlay::spawn_overlay() {
                        Ok(h) => h,
                        Err(e) => {
                            warn!(error = %e, "failed to spawn overlay");
                            state = State::Recording;
                            continue;
                        }
                    };

                    // Spawn periodic transcription loop
                    let stop_flag = Arc::new(AtomicBool::new(false));
                    let stop_clone = stop_flag.clone();
                    let audio_handle_clone = audio_handle.clone();
                    let transcriber_clone = transcriber.clone();
                    let overlay_tx = overlay_handle.tx.clone();

                    std::thread::spawn(move || {
                        periodic_transcription_loop(
                            stop_clone,
                            audio_handle_clone,
                            transcriber_clone,
                            overlay_tx,
                        );
                    });

                    // Wait for AltGr release
                    loop {
                        match rx.recv().await {
                            Some(KeyEvent::AltGrReleased) => break,
                            Some(KeyEvent::AltGrPressed) => continue, // repeat
                            None => return Ok(()),
                        }
                    }

                    // Stop periodic loop
                    stop_flag.store(true, Ordering::Relaxed);

                    let samples = audio.stop_recording();
                    let duration = samples.len() as f32 / 16_000.0;

                    if duration < 0.3 {
                        warn!(duration, "recording too short, ignoring");
                        overlay_handle.send(OverlayCommand::Close);
                        overlay_handle.join();
                        state = State::Idle;
                        continue;
                    }

                    // Final transcription
                    let final_text = {
                        let tmp = tempfile::Builder::new().suffix(".wav").tempfile()?;
                        let wav_path = tmp.path().to_path_buf();
                        audio::AudioCapture::write_wav(&samples, &wav_path)?;
                        transcriber.transcribe(&wav_path)?
                    };

                    if final_text.is_empty() {
                        warn!("final transcription returned empty text");
                        overlay_handle.send(OverlayCommand::Close);
                        overlay_handle.join();
                        state = State::Idle;
                        continue;
                    }

                    let (cx, cy) = get_cursor_position();
                    overlay_handle.send(OverlayCommand::Finish(final_text.clone(), cx, cy));
                    overlay_handle.join();

                    if let Err(e) = paste::paste_text(&final_text) {
                        error!(error = %e, "failed to paste");
                    }

                    state = State::Idle;
                } else {
                    // --no-overlay mode: just record and transcribe
                    state = State::Recording;
                }
            }

            (State::Recording, KeyEvent::AltGrReleased) if args.no_overlay => {
                let samples = audio.stop_recording();
                let duration = samples.len() as f32 / 16_000.0;

                if duration < 0.3 {
                    warn!(duration, "recording too short, ignoring");
                    state = State::Idle;
                    continue;
                }

                let tmp = tempfile::Builder::new().suffix(".wav").tempfile()?;
                let wav_path = tmp.path().to_path_buf();
                audio::AudioCapture::write_wav(&samples, &wav_path)?;

                match transcriber.transcribe(&wav_path) {
                    Ok(text) if text.is_empty() => {
                        warn!("transcription returned empty text");
                    }
                    Ok(text) => {
                        if let Err(e) = paste::paste_text(&text) {
                            error!(error = %e, "failed to paste");
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "transcription failed");
                    }
                }

                state = State::Idle;
            }

            // Ignore spurious events
            (State::Idle, KeyEvent::AltGrReleased) => {}
            (State::Recording, KeyEvent::AltGrPressed) => {} // repeat
            (State::Recording, KeyEvent::AltGrReleased) => {} // handled in overlay branch above
        }
    }

    Ok(())
}

/// Runs on a background thread. Every ~1 second, snapshots the audio buffer,
/// transcribes it, and sends the result to the overlay.
fn periodic_transcription_loop(
    stop: Arc<AtomicBool>,
    audio_handle: audio::AudioBufferHandle,
    transcriber: Arc<transcribe::Transcriber>,
    overlay_tx: std::sync::mpsc::Sender<OverlayCommand>,
) {
    let interval = std::time::Duration::from_secs(1);

    // Wait initial 1s before first transcription attempt
    for _ in 0..10 {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    while !stop.load(Ordering::Relaxed) {
        let samples = audio_handle.snapshot();
        let duration = samples.len() as f32 / 16_000.0;

        if duration >= 0.5 {
            // Write to temp file and transcribe
            let tmp = match tempfile::Builder::new().suffix(".wav").tempfile() {
                Ok(t) => t,
                Err(e) => {
                    warn!(error = %e, "periodic: failed to create temp file");
                    break;
                }
            };
            let wav_path = tmp.path().to_path_buf();
            if let Err(e) = audio::AudioCapture::write_wav(&samples, &wav_path) {
                warn!(error = %e, "periodic: failed to write WAV");
            } else {
                match transcriber.transcribe(&wav_path) {
                    Ok(text) if !text.is_empty() => {
                        info!(text = %text, "periodic transcription");
                        if overlay_tx.send(OverlayCommand::UpdateText(text)).is_err() {
                            return; // overlay closed
                        }
                    }
                    Ok(_) => {} // empty, skip
                    Err(e) => {
                        warn!(error = %e, "periodic transcription failed");
                    }
                }
            }
        }

        // Sleep in small increments so we can check the stop flag
        let sleep_start = std::time::Instant::now();
        while sleep_start.elapsed() < interval && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

/// Get cursor position from Hyprland via hyprctl.
/// Falls back to screen center if unavailable.
fn get_cursor_position() -> (f32, f32) {
    if let Ok(output) = std::process::Command::new("hyprctl")
        .args(["cursorpos", "-j"])
        .output()
    {
        if let Ok(text) = String::from_utf8(output.stdout) {
            let x = extract_json_number(&text, "x");
            let y = extract_json_number(&text, "y");
            if let (Some(x), Some(y)) = (x, y) {
                return (x, y);
            }
        }
    }
    (960.0, 800.0)
}

fn extract_json_number(json: &str, key: &str) -> Option<f32> {
    let pattern = format!("\"{}\":", key);
    let start = json.find(&pattern)? + pattern.len();
    let rest = json[start..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')?;
    rest[..end].parse().ok()
}
