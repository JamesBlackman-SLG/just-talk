#!/usr/bin/env python3
"""HTTP + WebSocket server for Nemotron ASR transcription."""
import asyncio
import json
import os
import tempfile
import threading

import numpy as np
import soundfile as sf
import nemo.collections.asr as nemo_asr
from fastapi import FastAPI, File, UploadFile, WebSocket, WebSocketDisconnect
from fastapi.responses import PlainTextResponse

# Global model reference
asr_model = None

# Serialize all access to the NeMo model — CUDA is not thread-safe,
# so concurrent transcriptions from overlapping sessions must queue.
_model_lock = threading.Lock()


from contextlib import asynccontextmanager


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


def _transcribe_samples(samples: np.ndarray) -> str:
    """Transcribe numpy float32 audio samples via temp WAV file (blocking).

    Serialized via _model_lock to prevent concurrent GPU access.
    """
    with _model_lock:
        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as tmp:
            tmp_path = tmp.name
        try:
            sf.write(tmp_path, samples, 16000)
            result = asr_model.transcribe([tmp_path])
            if result:
                hyp = result[0]
                text = hyp.text if hasattr(hyp, "text") else str(hyp)
                return text.strip()
            return ""
        finally:
            os.unlink(tmp_path)


# ---- HTTP endpoints (unchanged) ----


@app.post("/transcribe/", response_class=PlainTextResponse)
async def transcribe(file: UploadFile = File(...)) -> str:
    """Transcribe an uploaded audio file and return the text."""
    suffix = os.path.splitext(file.filename)[1] if file.filename else ".wav"
    with tempfile.NamedTemporaryFile(suffix=suffix, delete=False) as tmp:
        content = await file.read()
        tmp.write(content)
        tmp_path = tmp.name

    try:
        result = asr_model.transcribe([tmp_path])
        if result:
            hyp = result[0]
            text = hyp.text if hasattr(hyp, "text") else str(hyp)
            return text.strip()
        return ""
    finally:
        os.unlink(tmp_path)


@app.get("/health")
async def health():
    """Health check endpoint."""
    return {
        "status": "ok",
        "model": "nemotron-speech-streaming-en-0.6b",
        "streaming": True,
    }


# ---- WebSocket streaming endpoint ----


@app.websocket("/ws/stream")
async def stream_transcribe(websocket: WebSocket):
    """Stream audio over WebSocket with periodic partial transcription.

    Protocol:
      Client sends:
        - Binary frames: raw s16le PCM audio at 16kHz mono
        - Text frame: {"type": "done"} to signal end of audio

      Server sends:
        - Text frame: {"type": "partial", "text": "..."} during streaming
        - Text frame: {"type": "final", "text": "..."} after "done" received
    """
    await websocket.accept()

    audio_lock = asyncio.Lock()
    audio_chunks: list[np.ndarray] = []
    running = True

    async def transcription_worker():
        """Periodically transcribe accumulated audio and push partial results."""
        nonlocal running
        last_text = ""

        # Initial wait before first transcription
        await asyncio.sleep(0.5)

        while running:
            # Snapshot current audio under lock (brief hold, no sleeping)
            async with audio_lock:
                if not audio_chunks:
                    samples = None
                else:
                    samples = np.concatenate(audio_chunks)

            # Sleep outside the lock so audio can still be appended
            if samples is None:
                await asyncio.sleep(0.3)
                continue

            duration = len(samples) / 16000.0
            if duration < 0.5:
                await asyncio.sleep(0.3)
                continue

            # Run blocking NeMo transcription in thread pool
            # _model_lock inside _transcribe_samples serializes GPU access
            try:
                text = await asyncio.to_thread(_transcribe_samples, samples)
            except Exception as e:
                print(f"[ws] transcription error: {e}")
                await asyncio.sleep(0.5)
                continue

            if text and text != last_text:
                last_text = text
                try:
                    await websocket.send_json({"type": "partial", "text": text})
                except Exception:
                    break

            # Wait before next transcription cycle
            await asyncio.sleep(0.3)

    worker = asyncio.create_task(transcription_worker())

    try:
        while True:
            message = await websocket.receive()
            msg_type = message.get("type", "")

            if msg_type == "websocket.disconnect":
                break

            if "bytes" in message and message["bytes"]:
                # Raw s16le PCM audio chunk
                raw = message["bytes"]
                chunk = (
                    np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0
                )
                async with audio_lock:
                    audio_chunks.append(chunk)

            elif "text" in message and message["text"]:
                try:
                    data = json.loads(message["text"])
                except (json.JSONDecodeError, TypeError):
                    continue
                if data.get("type") == "done":
                    break

    except WebSocketDisconnect:
        pass
    finally:
        running = False
        worker.cancel()
        try:
            await worker
        except asyncio.CancelledError:
            pass

    # Final transcription of all accumulated audio
    async with audio_lock:
        if audio_chunks:
            all_samples = np.concatenate(audio_chunks)
        else:
            all_samples = np.array([], dtype=np.float32)

    final_text = ""
    if len(all_samples) / 16000.0 >= 0.3:
        try:
            final_text = await asyncio.to_thread(_transcribe_samples, all_samples)
        except Exception as e:
            print(f"[ws] final transcription error: {e}")

    try:
        await websocket.send_json({"type": "final", "text": final_text})
    except Exception:
        pass

    try:
        await websocket.close()
    except Exception:
        pass


if __name__ == "__main__":
    import uvicorn

    port = int(os.environ.get("NEMOTRON_PORT", "5051"))
    print(f"Starting Nemotron ASR server on port {port}...")
    uvicorn.run(app, host="0.0.0.0", port=port)
