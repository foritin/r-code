# syntax=docker/dockerfile:1
# r-code-relay — multi-stage, non-root, minimal (R20).
FROM rust:1-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/r-code-relay ./crates/r-code-relay
COPY crates/r-code-harness-protocol ./crates/r-code-harness-protocol
COPY vendor/agent-contracts ./vendor/agent-contracts
# 增量缓存：先空壳 build 让依赖落缓存
RUN mkdir -p crates/r-code-relay/src && \
    echo "fn main() {}" > crates/r-code-relay/src/main.rs && \
    echo "" > crates/r-code-relay/src/lib.rs && \
    cargo build --release -p r-code-relay --bin r-code-relay || true
COPY crates/r-code-relay ./crates/r-code-relay
RUN cargo build --release -p r-code-relay --bin r-code-relay

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY --from=build /build/target/release/r-code-relay /usr/local/bin/r-code-relay
USER 65534:65534
ENV R_CODE_RELAY_BIND=0.0.0.0:8787
# R_CODE_RELAY_CODES 必须由部署方注入（compose secrets / systemd env）。
EXPOSE 8787
ENTRYPOINT ["/usr/local/bin/r-code-relay"]
