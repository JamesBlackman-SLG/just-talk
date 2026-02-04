use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleRate, StreamConfig};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{info, warn};

const WHISPER_SAMPLE_RATE: u32 = 16_000;

/// Lightweight, Send+Sync handle to the audio buffer.
/// Can be cloned and sent to other threads for snapshotting.
#[derive(Clone)]
pub struct AudioBufferHandle {
    buffer: Arc<Mutex<Vec<f32>>>,
}

impl AudioBufferHandle {
    /// Clone the current buffer contents without affecting recording.
    pub fn snapshot(&self) -> Vec<f32> {
        self.buffer.lock().unwrap().clone()
    }
}

/// Manages microphone capture. Samples are continuously captured when the stream
/// is running, but only accumulated into the buffer when `recording` is true.
/// Not Send/Sync due to cpal::Stream — lives on the main thread.
pub struct AudioCapture {
    _stream: cpal::Stream,
    buffer: Arc<Mutex<Vec<f32>>>,
    recording: Arc<AtomicBool>,
}

impl AudioCapture {
    pub fn new() -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .context("no input device available")?;

        info!(device = ?device.name(), "using input device");

        let config = StreamConfig {
            channels: 1,
            sample_rate: SampleRate(WHISPER_SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Default,
        };

        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let recording = Arc::new(AtomicBool::new(false));

        let buf_clone = buffer.clone();
        let rec_clone = recording.clone();

        let stream = device
            .build_input_stream(
                &config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if rec_clone.load(Ordering::Relaxed) {
                        if let Ok(mut buf) = buf_clone.lock() {
                            buf.extend_from_slice(data);
                        }
                    }
                },
                move |err| {
                    warn!(error = %err, "audio stream error");
                },
                None,
            )
            .context("failed to build input stream")?;

        stream.play().context("failed to start audio stream")?;

        Ok(Self {
            _stream: stream,
            buffer,
            recording,
        })
    }

    /// Get a Send+Sync handle for snapshotting the buffer from other threads.
    pub fn buffer_handle(&self) -> AudioBufferHandle {
        AudioBufferHandle {
            buffer: self.buffer.clone(),
        }
    }

    /// Start accumulating samples.
    pub fn start_recording(&self) {
        self.buffer.lock().unwrap().clear();
        self.recording.store(true, Ordering::Relaxed);
        info!("recording started");
    }

    /// Stop accumulating and return the buffered samples.
    pub fn stop_recording(&self) -> Vec<f32> {
        self.recording.store(false, Ordering::Relaxed);
        let samples = std::mem::take(&mut *self.buffer.lock().unwrap());
        let duration = samples.len() as f32 / WHISPER_SAMPLE_RATE as f32;
        info!(samples = samples.len(), duration_secs = duration, "recording stopped");
        samples
    }

    /// Write f32 samples to a 16kHz mono WAV file.
    pub fn write_wav(samples: &[f32], path: &std::path::Path) -> Result<()> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: WHISPER_SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec)?;
        for &sample in samples {
            let s = (sample * 32767.0).clamp(-32768.0, 32767.0) as i16;
            writer.write_sample(s)?;
        }
        writer.finalize()?;
        Ok(())
    }
}
