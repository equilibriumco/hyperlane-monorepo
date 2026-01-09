# Dockerfile for Cardano E2E testing agents
# Builds on the hyperlane-agent base image

# -------- Build Stage --------
# Build the Rust agents from source
FROM rust:1.88.0 AS builder

# Install build dependencies
RUN apt-get update && \
    apt-get install -y --no-install-recommends musl-tools clang && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/rust/main

# Copy git metadata for version information
COPY .git ../../.git

# Copy all main workspace crates
COPY rust/main/agents ./agents
COPY rust/main/applications ./applications
COPY rust/main/chains ./chains
COPY rust/main/ethers-prometheus ./ethers-prometheus
COPY rust/main/hyperlane-base ./hyperlane-base
COPY rust/main/hyperlane-core ./hyperlane-core
COPY rust/main/hyperlane-metric ./hyperlane-metric
COPY rust/main/hyperlane-test ./hyperlane-test
COPY rust/main/lander ./lander
COPY rust/main/utils ./utils
COPY rust/main/Cargo.toml ./
COPY rust/main/Cargo.lock ./

# Copy sealevel workspace
COPY rust/sealevel ../sealevel

# Build release binaries
RUN RUSTFLAGS="--cfg tokio_unstable" \
    cargo build --release --bin validator --bin relayer && \
    mkdir -p /release && \
    cp target/release/validator /release && \
    cp target/release/relayer /release

# -------- Runtime Image --------
FROM ubuntu:22.04
WORKDIR /app

# Install runtime dependencies including envsubst and curl
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        openssl \
        ca-certificates \
        tini \
        libcurl4 \
        curl \
        gettext-base && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/* && \
    mkdir -p /data && chown -R 1000 /data && \
    mkdir -p /app/config && chown -R 1000 /app/config && \
    mkdir -p /shared/checkpoints && chown -R 1000 /shared

# Copy binaries from builder
COPY --from=builder /release/validator /app/
COPY --from=builder /release/relayer /app/

# Copy entrypoint script
COPY cardano/e2e-docker/entrypoint.sh /app/
RUN chmod +x /app/entrypoint.sh

# Copy config templates to a templates directory (not the config dir the agent loads from)
COPY cardano/e2e-docker/config/*.json /app/config-templates/

# Create empty config directory (required by agent loader)
RUN mkdir -p /app/config

# Run as non-root user
USER 1000

# Default environment variables
ENV AGENT_TYPE=validator
ENV RUST_LOG=info

# Use tini as init and our entrypoint for config processing
ENTRYPOINT ["tini", "--", "/app/entrypoint.sh"]
