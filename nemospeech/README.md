# NeMo Speech Server

A Docker-based speech recognition server using NVIDIA NeMo toolkit with CUDA acceleration.

## Prerequisites

- NVIDIA GPU with drivers installed
- Docker
- Docker Compose

## Installation (Arch Linux)

### 1. Install Required Packages

```bash
# Install Docker if not already installed
sudo pacman -S docker docker-compose

# Install NVIDIA Container Toolkit
sudo pacman -S nvidia-container-toolkit
```

### 2. Configure NVIDIA Runtime for Docker

```bash
# Configure Docker to use NVIDIA runtime
sudo nvidia-ctk runtime configure --runtime=docker

# Restart Docker daemon
sudo systemctl restart docker
```

### 3. Verify NVIDIA GPU Access

```bash
# Check that your NVIDIA drivers are working
nvidia-smi

# Verify Docker daemon configuration
cat /etc/docker/daemon.json
```

The daemon.json should now include NVIDIA runtime configuration.

## Running the Server

### Build and Start with Docker Compose

```bash
docker compose up
```

This will:
- Build the Docker image with CUDA support
- Download the NeMo model on first run (cached in a Docker volume)
- Start the speech server on port 5051
- Provide GPU acceleration for inference

### Run in Background

```bash
docker compose up -d
```

### Stop the Server

```bash
docker compose down
```

## Usage

The server exposes a WebSocket API on port 5051 for real-time speech recognition using NVIDIA's NeMo ASR models.

## Troubleshooting

If you encounter GPU access issues:

1. Verify NVIDIA drivers: `nvidia-smi`
2. Check Docker can access GPU: `docker run --rm --gpus all nvidia/cuda:12.6.0-base-ubuntu22.04 nvidia-smi`
3. Ensure Docker service is running: `sudo systemctl status docker`
4. Review Docker logs: `docker compose logs`
