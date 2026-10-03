FROM rust:1.86-slim AS builder

WORKDIR /app

# Cache dependency build (build.rs + proto are needed to compile build deps
# and the gRPC stubs; copying them here keeps the dependency layer cacheable)
COPY Cargo.toml Cargo.lock build.rs ./
COPY proto/ proto/
RUN mkdir src && echo "fn main() {}" > src/main.rs && touch src/lib.rs && \
    cargo build --release --locked && \
    rm -rf src

# Build the real binary (lib + bin)
COPY src/ src/
RUN touch src/main.rs src/lib.rs && cargo build --release --locked

FROM debian:bookworm-slim AS runtime

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/rustybin /app/rustybin

# Demo PKI: on first start Rustybin generates a demo CA (certs/ca.crt +
# ca.key), a server certificate for the HTTPS listener (certs/server.crt +
# server.key, RUSTYBIN_TLS_CERT / RUSTYBIN_TLS_KEY) and a client certificate
# for mTLS demos, all in /app/certs. Mount a volume there to keep them across
# container restarts (docker-compose.yml does). To use your own server
# certificate, put server.crt and server.key in that directory.
RUN mkdir -p /app/certs

ENV RUSTYBIN_LOG_LEVEL=info
# HTTP, HTTPS, gRPC
EXPOSE 80 443 50051

ENTRYPOINT ["/app/rustybin"]
