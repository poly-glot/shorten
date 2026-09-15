FROM mcr.microsoft.com/devcontainers/rust:1-trixie

RUN apt-get update \
    && apt-get install -y --no-install-recommends awscli python3-pip \
    && pip3 install --break-system-packages cargo-lambda ziglang \
    && rm -rf /var/lib/apt/lists/*
