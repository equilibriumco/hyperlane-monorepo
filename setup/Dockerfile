# syntax=docker/dockerfile:1
#
# Agents for the Cardano <-> Midnight bridge.
#
# Two things make this differ from the plain agent image:
#   1. `--features midnight` — the Midnight chain arms in hyperlane-base are
#      feature-gated, so without it a midnight chain entry cannot be built and
#      the agent dies at config load.
#   2. Node in the runtime image — hyperlane-midnight submits transactions by
#      spawning an external submitter (`toolkitPath`), which is a bash wrapper
#      around `npx tsx` in the midnight-hyperlane repo. The repo itself is bind
#      mounted at run time (see docker-compose.yml); only the interpreter is
#      baked in here.

# Matches rust/main/rust-toolchain, which is copied in below so the two
# cannot drift. The workspace lockfile needs >= 1.95 (sysinfo), so an older
# pinned base fails at dependency resolution rather than at compile time.
FROM rust:1.96.1-bookworm AS builder

RUN apt-get update && \
    apt-get install -y --no-install-recommends musl-tools clang && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/rust/main

# Version information is read from git metadata at build time.
COPY .git ../../.git

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
COPY rust/main/rust-toolchain ./
COPY rust/sealevel ../sealevel

# The registry, git checkouts and target dir are cache mounts, so editing one
# crate recompiles that crate rather than the whole graph. The binaries must be
# copied out inside this same instruction: a cache mount is not part of the
# image, so target/ is gone by the next one.
RUN --mount=type=cache,id=hyperlane-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=hyperlane-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=hyperlane-cargo-target,target=/usr/src/rust/main/target \
    RUSTFLAGS="--cfg tokio_unstable" \
    cargo build --release --features midnight \
        --bin validator --bin relayer --bin scraper --bin init-db && \
    mkdir -p /release && \
    cp target/release/validator target/release/relayer \
       target/release/scraper target/release/init-db /release/

# Debian bookworm, matching the builder's glibc so the agent binaries run
# unchanged. Carries node + npx for the Midnight submitter.
#
# Node 24 to match the host that runs `npm install` in the Midnight repo: those
# node_modules are bind mounted in, and anything native in them is compiled
# against the installing runtime's ABI.
FROM node:24-bookworm-slim
WORKDIR /app

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        gettext-base \
        openssl \
        tini && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /release/validator /release/relayer /release/scraper /release/init-db /app/
COPY setup/config/agent-config.json.tmpl /app/config/
COPY setup/entrypoint.sh /app/
RUN chmod +x /app/entrypoint.sh

# Agent state (validator dbs, relayer db) and the checkpoint directory the
# validators write and the relayer reads.
RUN mkdir -p /data /shared/checkpoints && chown -R 1000 /data /shared

USER 1000
ENV RUST_LOG=info

ENTRYPOINT ["tini", "--", "/app/entrypoint.sh"]
