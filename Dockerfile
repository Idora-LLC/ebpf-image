FROM rust:latest AS builder

RUN apt-get update && apt-get install -y \
    clang \
    llvm \
    libelf-dev \
    build-essential \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

RUN rustup install nightly \
    && rustup component add rust-src --toolchain nightly \
    && cargo install bpf-linker

WORKDIR /build
COPY . .

RUN cargo xtask build-ebpf --profile release
RUN cargo build --package ci-tracer --release

# ── Runtime ──────────────────────────────────────────────────────────────────

FROM ubuntu:latest

RUN apt-get update && apt-get install -y --no-install-recommends \
    libelf1 \
    zlib1g \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/ci-tracer /usr/local/bin/ci-tracer
COPY entrypoint.sh /entrypoint.sh
COPY start-ci-tracer.sh /usr/local/bin/start-ci-tracer
COPY sh-wrapper.sh /usr/local/bin/sh-wrapper.sh

# Fix CRLF, set permissions, install sh wrapper.
# The wrapper intercepts `docker exec sh -c "..."` calls from GitHub Actions
# and auto-starts the tracer on first invocation.
RUN sed -i 's/\r$//' /entrypoint.sh /usr/local/bin/start-ci-tracer /usr/local/bin/sh-wrapper.sh \
    && chmod +x /entrypoint.sh /usr/local/bin/start-ci-tracer /usr/local/bin/sh-wrapper.sh \
    && cp /usr/bin/dash /usr/bin/sh.real \
    && cp /usr/local/bin/sh-wrapper.sh /usr/bin/sh

ENTRYPOINT ["/entrypoint.sh"]
