# syntax=docker/dockerfile:1.7
# Multi-stage build: cargo-chef for dependency caching, alpine runtime.

FROM rust:1.92-alpine AS planner
RUN apk add --no-cache musl-dev
RUN cargo install cargo-chef
WORKDIR /app
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM rust:1.92-alpine AS cacher
RUN apk add --no-cache musl-dev
RUN cargo install cargo-chef
WORKDIR /app
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

FROM rust:1.92-alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /app
COPY . .
COPY --from=cacher /app/target target
COPY --from=cacher /usr/local/cargo /usr/local/cargo
RUN cargo build --release

FROM alpine:3.21 AS runtime
RUN apk add --no-cache ca-certificates tini
RUN addgroup -S -g 10001 relay \
 && adduser -S -u 10001 -G relay -h /var/lib/relay -s /sbin/nologin relay

COPY --from=builder /app/target/release/mcp-tunnel /usr/local/bin/mcp-tunnel

ENV MCPR_RELAY_PORT=8080

EXPOSE 8080

USER relay
WORKDIR /var/lib/relay

ENTRYPOINT ["/sbin/tini", "--", "mcp-tunnel"]
