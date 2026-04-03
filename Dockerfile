# ── Stage 1: Build the Rust binary ───────────────────────────────────────────
FROM rust:1.78-alpine AS builder

RUN apk add --no-cache musl-dev pkgconfig openssl-dev

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

# Pre-fetch dependencies (cache layer — invalidated only on Cargo.toml changes).
RUN cargo fetch --locked

RUN cargo build --release --locked --bin agentgate

# ── Stage 2: Optional dashboard build ────────────────────────────────────────
FROM node:20-alpine AS dashboard-builder

WORKDIR /dashboard
COPY dashboard/package*.json ./
RUN npm ci --prefer-offline
COPY dashboard/ ./
RUN npm run build

# ── Stage 3: Minimal runtime image ───────────────────────────────────────────
FROM alpine:3.20 AS runtime

RUN apk add --no-cache ca-certificates tzdata

# Create a non-root user for running the gateway.
RUN addgroup -S agentgate && adduser -S -G agentgate agentgate

COPY --from=builder /build/target/release/agentgate /usr/local/bin/agentgate
COPY --from=dashboard-builder /dashboard/out /app/dashboard/out

# Default data directory — mount a volume here for persistence.
RUN mkdir -p /data && chown agentgate:agentgate /data

USER agentgate
WORKDIR /app

# Dashboard UI + REST API
EXPOSE 7070
# Prometheus metrics (stdio mode only; optional)
EXPOSE 9090

ENV HOME=/data

ENTRYPOINT ["agentgate"]
CMD ["--help"]
