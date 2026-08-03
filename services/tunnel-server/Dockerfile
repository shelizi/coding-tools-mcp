FROM rust:1.88-alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY crates/tunnel-protocol crates/tunnel-protocol
COPY services/tunnel-server services/tunnel-server
RUN cargo build --manifest-path services/tunnel-server/Cargo.toml --release

FROM alpine:3.22
# wget is required for HEALTHCHECK / Compose health probes against /health.
RUN apk add --no-cache wget \
    && addgroup -S tunnel && adduser -S -G tunnel tunnel \
    && mkdir -p /data/logs \
    && chown -R tunnel:tunnel /data
COPY --from=builder /src/services/tunnel-server/target/release/coding-tools-tunnel-server /usr/local/bin/coding-tools-tunnel-server
ENV CODING_TOOLS_TUNNEL_DB=/data/tunnel.db \
    CODING_TOOLS_TUNNEL_LOG_DIR=/data/logs
VOLUME ["/data"]
USER tunnel
EXPOSE 8088
EXPOSE 8089
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD wget -qO- http://127.0.0.1:8088/health || exit 1
ENTRYPOINT ["/usr/local/bin/coding-tools-tunnel-server"]
