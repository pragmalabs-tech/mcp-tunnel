# syntax=docker/dockerfile:1.7

FROM rust:1.92-alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /app
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release --bin mcp-tunnel \
 && cp target/release/mcp-tunnel /usr/local/bin/mcp-tunnel

FROM alpine:3.21 AS runtime
RUN apk add --no-cache ca-certificates tini
RUN addgroup -S -g 10001 relay \
 && adduser -S -u 10001 -G relay -h /var/lib/relay -s /sbin/nologin relay

COPY --from=builder /usr/local/bin/mcp-tunnel /usr/local/bin/mcp-tunnel

EXPOSE 8080

USER relay
WORKDIR /var/lib/relay

ENTRYPOINT ["/sbin/tini", "--", "mcp-tunnel"]

LABEL org.opencontainers.image.source="https://github.com/pragmalabs-tech/mcp-tunnel" \
      org.opencontainers.image.description="Self-hosted HTTP tunnel relay server" \
      org.opencontainers.image.licenses="Apache-2.0"
