# syntax=docker/dockerfile:1

FROM rust:1-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --bin trust-router-sidecar

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home trust-router \
    && mkdir -p /audit \
    && chown -R trust-router:trust-router /audit

COPY --from=builder /app/target/release/trust-router-sidecar /usr/local/bin/trust-router-sidecar

USER trust-router
EXPOSE 7878
VOLUME ["/audit"]

ENTRYPOINT ["/usr/local/bin/trust-router-sidecar"]
CMD ["0.0.0.0:7878", "/audit/sidecar-audit.jsonl"]
