FROM rust:1.86-slim AS builder

WORKDIR /app

# Cache dependency build (build.rs + proto are needed to compile build deps
# and the gRPC stubs; copying them here keeps the dependency layer cacheable)
COPY Cargo.toml Cargo.lock build.rs ./
COPY proto/ proto/
RUN mkdir src && echo "fn main() {}" > src/main.rs && touch src/lib.rs && \
    cargo build --release --locked && \
    rm -rf src

# Build the real binary (lib + bin); build.rs embeds the web console from ui/
COPY ui/ ui/
COPY src/ src/
RUN touch src/main.rs src/lib.rs && cargo build --release --locked

FROM debian:bookworm-slim AS runtime

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates openssl && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/rustybin /app/rustybin
COPY certs/generate-certs.sh /app/certs/generate-certs.sh
RUN chmod +x /app/certs/generate-certs.sh

COPY <<'ENTRYPOINT' /app/entrypoint.sh
#!/usr/bin/env bash
set -e
if [ ! -f /app/certs/server.crt ] || [ ! -f /app/certs/server.key ]; then
    echo "Generating self-signed TLS certificates..."
    /app/certs/generate-certs.sh
fi
exec /app/rustybin "$@"
ENTRYPOINT
RUN chmod +x /app/entrypoint.sh

ENV RUST_LOG=info
EXPOSE 80 443 50051

ENTRYPOINT ["/app/entrypoint.sh"]
