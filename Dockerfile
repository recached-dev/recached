# ── Stage 1: Build ───────────────────────────────────────────────────────────
# Use a recent stable image — edition 2024 and let-chains require Rust ≥ 1.88.
FROM rust:1-slim-bookworm AS builder

# tikv-jemalloc-sys compiles jemalloc from C source, which needs a C toolchain
# (gcc + make). The slim image doesn't ship one.
RUN apt-get update && \
    apt-get install -y --no-install-recommends build-essential && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Cache dependencies separately so they aren't re-downloaded on every code change.
COPY Cargo.toml Cargo.lock ./
COPY core-engine/Cargo.toml   core-engine/Cargo.toml
COPY server-native/Cargo.toml server-native/Cargo.toml
COPY wasm-edge/Cargo.toml     wasm-edge/Cargo.toml

# Dummy source files so the dep-only build can resolve the workspace.
# wasm-edge/src is kept as a stub — Cargo parses it as a workspace member
# even when only building server-native, so lib.rs must exist.
RUN mkdir -p core-engine/src server-native/src wasm-edge/src && \
    echo "fn main() {}" > server-native/src/main.rs && \
    echo "" > core-engine/src/lib.rs && \
    echo "" > wasm-edge/src/lib.rs && \
    cargo build --release --package server-native && \
    rm -rf core-engine/src server-native/src

# Now copy real source and do the real build (only changed crates recompile).
COPY core-engine/src   core-engine/src
COPY server-native/src server-native/src

RUN cargo build --release --package server-native

# ── Stage 2: Runtime ─────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/server-native /usr/local/bin/recached-server

EXPOSE 6379
EXPOSE 6380

CMD ["recached-server"]
