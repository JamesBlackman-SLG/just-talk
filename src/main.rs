mod audio;
mod input;
mod overlay;
mod paste;
mod transcribe;

use anyhow::{Context, Result};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use input::KeyEvent;
use overlay::OverlayCommand;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
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
    let transcriber = Arc::new(transcribe::Transcriber::new(args.server));
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

                    // Spawn streaming transcription task
                    let stop_flag = Arc::new(AtomicBool::new(false));
                    let stop_clone = stop_flag.clone();
                    let audio_handle_clone = audio_handle.clone();
                    let overlay_tx = overlay_handle.tx.clone();
                    let ws_url = transcriber.ws_url();

                    let stream_task = tokio::spawn(async move {
                        streaming_transcription(
                            stop_clone,
                            audio_handle_clone,
                            ws_url,
                            overlay_tx,
                        )
                        .await
                    });

                    // Wait for AltGr release
                    loop {
                        match rx.recv().await {
                            Some(KeyEvent::AltGrReleased) => break,
                            Some(KeyEvent::AltGrPressed) => continue, // repeat
                            None => return Ok(()),
                        }
                    }

                    // Signal streaming to finish (it will send final chunk + "done")
                    stop_flag.store(true, Ordering::Relaxed);

                    // Wait for streaming task to get final result from server
                    let stream_result = tokio::time::timeout(
                        std::time::Duration::from_secs(15),
                        stream_task,
                    )
                    .await;

                    // Now stop recording
                    let samples = audio.stop_recording();
                    let duration = samples.len() as f32 / 16_000.0;

                    if duration < 0.3 {
                        warn!(duration, "recording too short, ignoring");
                        overlay_handle.send(OverlayCommand::Close);
                        overlay_handle.join();
                        state = State::Idle;
                        continue;
                    }

                    // Extract final text from streaming, fall back to HTTP
                    let final_text = match stream_result {
                        Ok(Ok(Ok(text))) if !text.is_empty() => {
                            info!(text = %text, "streaming transcription complete");
                            text
                        }
                        other => {
                            match &other {
                                Err(_) => warn!("streaming transcription timed out"),
                                Ok(Err(e)) => warn!(error = %e, "streaming task panicked"),
                                Ok(Ok(Err(e))) => {
                                    warn!(error = %e, "streaming transcription failed")
                                }
                                _ => warn!("streaming returned empty text"),
                            }
                            info!("falling back to HTTP transcription");

                            let tmp =
                                tempfile::Builder::new().suffix(".wav").tempfile()?;
                            let wav_path = tmp.path().to_path_buf();
                            audio::AudioCapture::write_wav(&samples, &wav_path)?;
                            match transcriber.transcribe(&wav_path) {
                                Ok(text) => text,
                                Err(e) => {
                                    warn!(error = %e, "fallback transcription failed");
                                    overlay_handle.send(OverlayCommand::UpdateText(
                                        "Transcription server unreachable".into(),
                                    ));
                                    tokio::time::sleep(std::time::Duration::from_secs(2))
                                        .await;
                                    overlay_handle.send(OverlayCommand::Close);
                                    overlay_handle.join();
                                    state = State::Idle;
                                    continue;
                                }
                            }
                        }
                    };

                    if final_text.is_empty() {
                        warn!("final transcription returned empty text");
                        overlay_handle.send(OverlayCommand::Close);
                        overlay_handle.join();
                        state = State::Idle;
                        continue;
                    }

                    let (cx, cy) = get_cursor_position();
                    overlay_handle
                        .send(OverlayCommand::Finish(final_text.clone(), cx, cy));
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

/// Stream audio to the nemospeech server over WebSocket, receiving partial
/// transcription results in real time. Returns the final transcription text.
async fn streaming_transcription(
    stop: Arc<AtomicBool>,
    audio_handle: audio::AudioBufferHandle,
    ws_url: String,
    overlay_tx: std::sync::mpsc::Sender<OverlayCommand>,
) -> Result<String> {
    let (ws_stream, _) =
        tokio_tungstenite::connect_async(&ws_url)
            .await
            .context("failed to connect to nemospeech WebSocket")?;

    info!(url = %ws_url, "WebSocket connected for streaming transcription");

    let (mut write, mut read) = ws_stream.split();

    // Spawn receiver task — forwards partial results to overlay, captures final text
    let overlay_tx_clone = overlay_tx;
    let recv_task = tokio::spawn(async move {
        let mut final_text = String::new();
        while let Some(msg) = read.next().await {
            let msg = match msg {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "WebSocket read error");
                    break;
                }
            };
            if let Message::Text(text) = msg {
                let data: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match data["type"].as_str() {
                    Some("partial") => {
                        if let Some(t) = data["text"].as_str() {
                            info!(text = %t, "streaming partial");
                            let _ = overlay_tx_clone
                                .send(OverlayCommand::UpdateText(t.to_string()));
                        }
                    }
                    Some("final") => {
                        if let Some(t) = data["text"].as_str() {
                            final_text = t.to_string();
                        }
                        break;
                    }
                    _ => {}
                }
            }
        }
        final_text
    });

    // Send audio chunks — only new samples since last send
    let mut last_sent = 0;
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));

    loop {
        interval.tick().await;

        if stop.load(Ordering::Relaxed) {
            // Send any remaining audio before signalling done
            let samples = audio_handle.snapshot();
            if samples.len() > last_sent {
                let bytes = samples_to_s16le(&samples[last_sent..]);
                let _ = write.send(Message::Binary(bytes.into())).await;
            }
            // Signal end of audio
            let _ = write
                .send(Message::Text(r#"{"type":"done"}"#.into()))
                .await;
            break;
        }

        let samples = audio_handle.snapshot();
        if samples.len() > last_sent {
            let bytes = samples_to_s16le(&samples[last_sent..]);
            if write.send(Message::Binary(bytes.into())).await.is_err() {
                warn!("WebSocket send failed");
                break;
            }
            last_sent = samples.len();
        }
    }

    // Wait for final transcription from server
    let final_text = match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        recv_task,
    )
    .await
    {
        Ok(Ok(text)) => text,
        Ok(Err(e)) => {
            warn!(error = %e, "recv task failed");
            String::new()
        }
        Err(_) => {
            warn!("timed out waiting for final transcription");
            String::new()
        }
    };

    Ok(final_text)
}

/// Convert f32 samples to s16le byte buffer for WebSocket transmission.
fn samples_to_s16le(samples: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let i = (s * 32767.0).clamp(-32768.0, 32767.0) as i16;
        bytes.extend_from_slice(&i.to_le_bytes());
    }
    bytes
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
