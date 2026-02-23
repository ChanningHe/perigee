FROM rust:1-slim-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
    musl-tools pkg-config git \
    && rm -rf /var/lib/apt/lists/*

RUN case "$(uname -m)" in \
      x86_64)  rustup target add x86_64-unknown-linux-musl ;; \
      aarch64) rustup target add aarch64-unknown-linux-musl ;; \
    esac

WORKDIR /workspace
