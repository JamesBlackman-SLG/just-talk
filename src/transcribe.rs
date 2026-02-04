use anyhow::{Context, Result};
use std::path::Path;
use tracing::info;
use ureq::unversioned::multipart::{Form, Part};

const DEFAULT_SERVER: &str = "http://localhost:5051";

pub struct Transcriber {
    server_url: String,
}

impl Transcriber {
    pub fn new(server_url: Option<String>) -> Result<Self> {
        let server_url = server_url.unwrap_or_else(|| {
            std::env::var("NEMOSPEECH_URL").unwrap_or_else(|_| DEFAULT_SERVER.to_string())
        });

        // Health check
        let health_url = format!("{}/health", server_url);
        ureq::get(&health_url)
            .call()
            .context(format!(
                "nemospeech server not reachable at {server_url} - is it running?"
            ))?;

        info!(server = %server_url, "transcriber ready (nemospeech)");
        Ok(Self { server_url })
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
