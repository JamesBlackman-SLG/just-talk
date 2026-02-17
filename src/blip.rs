use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;

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
    let len = len.clamp(1, 10) as f32;
    (1100.0 - (len - 1.0) * 49.0) as u32
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

        // If a blip_device is configured, set PIPEWIRE_NODE so PipeWire routes
        // this stream to the desired sink. This works because cpal opens the
        // default device through PipeWire, and PipeWire checks this env var
        // when a new stream connects.
        if let Some(ref node) = app_config.blip_device {
            // SAFETY: called before any other threads use this env var
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
                                let wave = (std::f32::consts::TAU * hz * t).sin()
                                    + 0.35 * (std::f32::consts::TAU * hz * 2.0 * t).sin()
                                    + 0.15 * (std::f32::consts::TAU * hz * 3.0 * t).sin();
                                *out = wave * env * 0.15;
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

        // Clear so it doesn't affect other audio in this process
        if app_config.blip_device.is_some() {
            // SAFETY: stream is created, env var no longer needed
            unsafe { std::env::remove_var("PIPEWIRE_NODE") };
        }

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
