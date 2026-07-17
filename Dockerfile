# Multi-stage build for Chat Transfer
FROM rust:1.87-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY static ./static
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -r -u 10001 -m chatxfer
WORKDIR /app
COPY --from=builder /app/target/release/chat-transfer /app/chat-transfer
COPY --from=builder /app/static /app/static
RUN mkdir -p /app/data/uploads && chown -R chatxfer:chatxfer /app
USER chatxfer
ENV CHAT_BIND=0.0.0.0:8080 \
    CHAT_DATA_DIR=/app/data \
    RUST_LOG=chat_transfer=info
EXPOSE 8080
VOLUME ["/app/data"]
ENTRYPOINT ["/app/chat-transfer"]
