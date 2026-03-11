use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tracing::warn;

/// Serialize access to PIPEWIRE_NODE env var — prevents races between
/// BlipPlayer::new() and play_toggle_sound() running on different threads.
static PIPEWIRE_ENV_LOCK: Mutex<()> = Mutex::new(());

const SAMPLE_RATE: u32 = 44_100;

/// Per-character interval in milliseconds.
const CHAR_INTERVAL_MS: u64 = 18;

/// Samples per character interval.
const INTERVAL_SAMPLES: usize = (SAMPLE_RATE as usize * CHAR_INTERVAL_MS as usize) / 1000;

/// Blip duration in samples (~6 ms — fits the 18ms interval).
const BLIP_SAMPLES: usize = 265;

/// Map word length to blip frequency.
/// Short words → high pitch, long words → low pitch.
/// 1 char ≈ 1100 Hz, 5 chars ≈ 900 Hz, 10+ chars ≈ 660 Hz.
fn freq_for_word_len(len: usize) -> u32 {
    // Base range raised half an octave: ~1320–1650 Hz
    let len = len.clamp(1, 10) as f32;
    (1650.0 - (len - 1.0) * 73.0) as u32
}

/// Keeps a cpal output stream open for the lifetime of a paste.
/// Queue blips with `blip_word()`, wait for them to play with `wait_for_drain()`.
pub struct BlipPlayer {
    _stream: cpal::Stream,
    queued: Arc<AtomicUsize>,
    played: Arc<AtomicUsize>,
    freq: Arc<AtomicU32>,
}

impl BlipPlayer {
    pub fn new() -> Result<Self> {
        let app_config = crate::config::Config::load();

        // Hold lock around env var set → device open → env var clear
        // to prevent races with play_toggle_sound on another thread
        let _env_guard = PIPEWIRE_ENV_LOCK.lock().unwrap();

        if let Some(ref node) = app_config.blip_device {
            // SAFETY: serialized by PIPEWIRE_ENV_LOCK
            unsafe { std::env::set_var("PIPEWIRE_NODE", node) };
            tracing::info!(node, "routing blip audio via PIPEWIRE_NODE");
        }

        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .context("no audio output device")?;

        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Default,
        };

        let queued = Arc::new(AtomicUsize::new(0));
        let played = Arc::new(AtomicUsize::new(0));
        let freq = Arc::new(AtomicU32::new(880));

        let q = queued.clone();
        let p = played.clone();
        let f = freq.clone();

        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let limit = q.load(Ordering::Acquire) * INTERVAL_SAMPLES;
                    let hz = f.load(Ordering::Relaxed) as f32;
                    let mut i = p.load(Ordering::Relaxed);
                    for out in data.iter_mut() {
                        if i < limit {
                            let within = i % INTERVAL_SAMPLES;
                            if within < BLIP_SAMPLES {
                                let t = within as f32 / SAMPLE_RATE as f32;
                                let env = if within < BLIP_SAMPLES / 10 {
                                    within as f32 / (BLIP_SAMPLES / 10) as f32
                                } else {
                                    let pos = (within - BLIP_SAMPLES / 10) as f32
                                        / (BLIP_SAMPLES * 9 / 10) as f32;
                                    (1.0 - pos) * (1.0 - pos)
                                };
                                let root = (std::f32::consts::TAU * hz * t).sin();
                                let fifth = (std::f32::consts::TAU * hz * 1.5 * t).sin();
                                *out = (root + 0.7 * fifth) * env * 0.15;
                            } else {
                                *out = 0.0;
                            }
                            i += 1;
                        } else {
                            *out = 0.0;
                        }
                    }
                    p.store(i, Ordering::Release);
                },
                |err| tracing::warn!(error = %err, "blip playback error"),
                None,
            )
            .context("failed to build output stream for blips")?;

        stream.play().context("failed to start blip playback")?;

        if app_config.blip_device.is_some() {
            // SAFETY: serialized by PIPEWIRE_ENV_LOCK
            unsafe { std::env::remove_var("PIPEWIRE_NODE") };
        }

        drop(_env_guard);

        Ok(Self {
            _stream: stream,
            queued,
            played,
            freq,
        })
    }

    /// Queue blips for a word: sets pitch from word length, queues n blips.
    pub fn blip_word(&self, char_count: usize) {
        self.freq
            .store(freq_for_word_len(char_count), Ordering::Relaxed);
        self.queued.fetch_add(char_count, Ordering::Release);
    }

    /// Block until all queued blips have been output.
    pub fn wait_for_drain(&self) {
        let target = self.queued.load(Ordering::Acquire) * INTERVAL_SAMPLES;
        while self.played.load(Ordering::Acquire) < target {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// Play a two-note confirmation tone for blip toggle.
/// Ascending (C5→G5) for enabled, descending (G5→C5) for disabled.
/// ~200ms total. Uses same PIPEWIRE_NODE routing as BlipPlayer.
pub fn play_toggle_sound(enabled: bool) {
    std::thread::spawn(move || {
        if let Err(e) = play_toggle_sound_inner(enabled) {
            warn!(error = %e, "failed to play toggle sound");
        }
    });
}

fn play_toggle_sound_inner(enabled: bool) -> Result<()> {
    let app_config = crate::config::Config::load();

    // Hold lock around env var set → device open → env var clear
    let _env_guard = PIPEWIRE_ENV_LOCK.lock().unwrap();

    if let Some(ref node) = app_config.blip_device {
        // SAFETY: serialized by PIPEWIRE_ENV_LOCK
        unsafe { std::env::set_var("PIPEWIRE_NODE", node) };
    }

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .context("no audio output device")?;

    let config = cpal::StreamConfig {
        channels: 1,
        sample_rate: cpal::SampleRate(SAMPLE_RATE),
        buffer_size: cpal::BufferSize::Default,
    };

    // C5 ≈ 523 Hz, G5 ≈ 784 Hz
    let (freq1, freq2): (f32, f32) = if enabled {
        (523.0, 784.0)
    } else {
        (784.0, 523.0)
    };

    let note_samples = SAMPLE_RATE as usize / 10; // 100ms per note
    let total_samples = note_samples * 2;
    let played = Arc::new(AtomicUsize::new(0));
    let p = played.clone();

    let stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let mut i = p.load(Ordering::Relaxed);
            for out in data.iter_mut() {
                if i < total_samples {
                    let (hz, note_pos) = if i < note_samples {
                        (freq1, i)
                    } else {
                        (freq2, i - note_samples)
                    };
                    let t = note_pos as f32 / SAMPLE_RATE as f32;
                    // Envelope: quick attack, quadratic decay
                    let env = if note_pos < note_samples / 10 {
                        note_pos as f32 / (note_samples / 10) as f32
                    } else {
                        let pos = (note_pos - note_samples / 10) as f32
                            / (note_samples * 9 / 10) as f32;
                        (1.0 - pos) * (1.0 - pos)
                    };
                    let root = (std::f32::consts::TAU * hz * t).sin();
                    let fifth = (std::f32::consts::TAU * hz * 1.5 * t).sin();
                    *out = (root + 0.7 * fifth) * env * 0.15;
                    i += 1;
                } else {
                    *out = 0.0;
                }
            }
            p.store(i, Ordering::Release);
        },
        |err| warn!(error = %err, "toggle sound playback error"),
        None,
    ).context("failed to build toggle sound stream")?;

    stream.play().context("failed to play toggle sound")?;

    if app_config.blip_device.is_some() {
        // SAFETY: serialized by PIPEWIRE_ENV_LOCK
        unsafe { std::env::remove_var("PIPEWIRE_NODE") };
    }

    drop(_env_guard);

    // Wait for playback to finish
    while played.load(Ordering::Acquire) < total_samples {
        std::thread::sleep(Duration::from_millis(5));
    }
    // Small tail for audio to flush
    std::thread::sleep(Duration::from_millis(20));

    Ok(())
}
