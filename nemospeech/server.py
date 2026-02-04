#!/usr/bin/env python3
"""HTTP server for Nemotron ASR transcription - compatible with stompai."""
import os
import tempfile
from contextlib import asynccontextmanager

import nemo.collections.asr as nemo_asr
from fastapi import FastAPI, File, UploadFile
from fastapi.responses import PlainTextResponse

# Global model reference
asr_model = None


@asynccontextmanager
async def lifespan(app: FastAPI):
    """Load the ASR model on startup."""
    global asr_model
    print("Loading Nemotron ASR model...")
    asr_model = nemo_asr.models.ASRModel.from_pretrained(
        "nvidia/nemotron-speech-streaming-en-0.6b"
    )
    print("Nemotron ASR model loaded and ready!")
    yield
    # Cleanup on shutdown
    asr_model = None


app = FastAPI(lifespan=lifespan, title="Nemotron ASR Server")


@app.post("/transcribe/", response_class=PlainTextResponse)
async def transcribe(file: UploadFile = File(...)) -> str:
    """Transcribe an uploaded audio file and return the text."""
    # Save uploaded file to temp location
    suffix = os.path.splitext(file.filename)[1] if file.filename else ".wav"
    with tempfile.NamedTemporaryFile(suffix=suffix, delete=False) as tmp:
        content = await file.read()
        tmp.write(content)
        tmp_path = tmp.name

    try:
        # Transcribe using the model
        result = asr_model.transcribe([tmp_path])
        if result:
            hyp = result[0]
            text = hyp.text if hasattr(hyp, "text") else str(hyp)
            return text.strip()
        return ""
    finally:
        # Clean up temp file
        os.unlink(tmp_path)


@app.get("/health")
async def health():
    """Health check endpoint."""
    return {"status": "ok", "model": "nemotron-speech-streaming-en-0.6b"}


if __name__ == "__main__":
    import uvicorn

    port = int(os.environ.get("NEMOTRON_PORT", "5051"))
    print(f"Starting Nemotron ASR server on port {port}...")
    uvicorn.run(app, host="0.0.0.0", port=port)
