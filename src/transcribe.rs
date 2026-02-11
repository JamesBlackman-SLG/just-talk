use crate::config::Config;
use anyhow::{Context, Result};
use std::path::Path;
use tracing::{info, warn};
use ureq::unversioned::multipart::{Form, Part};

pub struct Transcriber {
    server_url: String,
}

impl Transcriber {
    pub fn new(server_url: Option<String>) -> Self {
        let server_url = Config::resolve_server_url(server_url);

        // Non-fatal health check — server may not be up yet
        let health_url = format!("{}/health", server_url);
        match ureq::get(&health_url).call() {
            Ok(_) => info!(server = %server_url, "transcriber ready (nemospeech)"),
            Err(_) => warn!(
                server = %server_url,
                "nemospeech not reachable yet — will connect on first use"
            ),
        }

        Self { server_url }
    }

    /// WebSocket URL for streaming transcription.
    pub fn ws_url(&self) -> String {
        let base = self.server_url.replace("http://", "ws://").replace("https://", "wss://");
        format!("{base}/ws/stream")
    }

    /// Transcribe a WAV file by uploading it to the nemospeech server.
    pub fn transcribe(&self, wav_path: &Path) -> Result<String> {
        info!(path = %wav_path.display(), "transcribing via nemospeech");

        let url = format!("{}/transcribe/", self.server_url);

        let form = Form::new()
            .part(
                "file",
                Part::file(wav_path)
                    .context("failed to read WAV file")?
                    .file_name("audio.wav")
                    .mime_str("audio/wav")?,
            );

        let mut response = ureq::post(&url)
            .send(form)
            .context("nemospeech request failed")?;

        let text = response.body_mut().read_to_string()?.trim().to_string();

        info!(text = %text, "transcription complete");
        Ok(text)
    }
}
