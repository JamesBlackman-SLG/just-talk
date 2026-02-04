FROM pytorch/pytorch:2.6.0-cuda12.6-cudnn9-runtime

WORKDIR /app

# Install system deps for soundfile
RUN apt-get update && \
    apt-get install -y --no-install-recommends libsndfile1 git && \
    rm -rf /var/lib/apt/lists/*

# Install Python deps
COPY pyproject.toml .
RUN pip install --no-cache-dir \
    "nemo_toolkit[asr] @ git+https://github.com/NVIDIA/NeMo.git@main" \
    fastapi uvicorn websockets numpy soundfile python-multipart

COPY server.py .

EXPOSE 5051

# Download model on first run (cached in volume)
ENV NEMOTRON_PORT=5051

CMD ["python", "server.py"]
