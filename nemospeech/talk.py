#!/usr/bin/env python3
"""Simple voice chat: speak -> transcribe -> LLM -> print response."""
import sounddevice as sd
import soundfile as sf
import tempfile
import requests
import nemo.collections.asr as nemo_asr

SAMPLE_RATE = 16000
RECORD_SECONDS = 5
OLLAMA_URL = "http://localhost:11434/api/generate"
MODEL = "granite4:latest"

print("Loading Nemotron ASR model...")
asr = nemo_asr.models.ASRModel.from_pretrained("nvidia/nemotron-speech-streaming-en-0.6b")
print("Ready!\n")

def record_audio():
    print(f"🎤 Recording for {RECORD_SECONDS}s... (speak now)")
    audio = sd.rec(int(RECORD_SECONDS * SAMPLE_RATE), samplerate=SAMPLE_RATE, channels=1, dtype='float32')
    sd.wait()
    print("Done recording.")
    return audio.flatten()

def transcribe(audio):
    with tempfile.NamedTemporaryFile(suffix='.wav', delete=False) as f:
        sf.write(f.name, audio, SAMPLE_RATE)
        result = asr.transcribe([f.name])
    if result:
        hyp = result[0]
        return hyp.text if hasattr(hyp, 'text') else str(hyp)
    return ""

def ask_llm(text):
    resp = requests.post(OLLAMA_URL, json={"model": MODEL, "prompt": text, "stream": False})
    return resp.json().get("response", "")

if __name__ == "__main__":
    while True:
        input("\nPress Enter to record (Ctrl+C to quit)...")
        audio = record_audio()
        text = transcribe(audio)
        print(f"You said: {text}")
        if text.strip():
            print("Thinking...")
            reply = ask_llm(text)
            print(f"LLM: {reply}")
