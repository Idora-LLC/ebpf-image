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
RUN sed -i 's/\r$//' /entrypoint.sh /usr/local/bin/start-ci-tracer \
    && chmod +x /entrypoint.sh /usr/local/bin/start-ci-tracer

ENTRYPOINT ["/entrypoint.sh"]
