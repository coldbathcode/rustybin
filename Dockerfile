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
# Commit reported by /_rustybin/version and the metrics (the build context has
# no .git): docker build --build-arg RUSTYBIN_GIT_SHA=<commit> . Declared here,
# after the dependency layer, so a new commit does not invalidate that cache.
ARG RUSTYBIN_GIT_SHA=""
# Touch build.rs too: COPY keeps old mtimes, so without it Cargo would not
# rerun the build script and the console (ui/) would be embedded empty.
RUN touch src/main.rs src/lib.rs build.rs && \
    RUSTYBIN_GIT_SHA="$RUSTYBIN_GIT_SHA" cargo build --release --locked

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
# certificate, put server.crt and server.key in that directory. A ca.crt /
# ca.key that is not the demo CA is never overwritten: the demo CA then uses
# rustybin-demo-ca.crt / rustybin-demo-ca.key.
#
# The process runs as an unprivileged user (uid/gid 10001) and writes only to
# /app/certs (demo PKI) and RUSTYBIN_USAGE_FILE, so the image also runs with a
# read-only root filesystem when /app/certs is a writable volume:
#   docker run --read-only -v rustybin-certs:/app/certs \
#     -e RUSTYBIN_USAGE_FILE=/app/certs/usage.json -p 8080:80 rustybin
# Docker lets unprivileged processes bind ports below 1024 in a container
# (net.ipv4.ip_unprivileged_port_start=0); where a runtime does not, set
# RUSTYBIN_HTTP_PORT=8080 and RUSTYBIN_HTTPS_PORT=8443.
RUN groupadd --system --gid 10001 rustybin && \
    useradd --system --uid 10001 --gid 10001 --home-dir /app --no-create-home \
      --shell /usr/sbin/nologin rustybin && \
    mkdir -p /app/certs && chown 10001:10001 /app/certs

ENV RUSTYBIN_LOG_LEVEL=info
# HTTP, HTTPS, gRPC
EXPOSE 80 443 50051

USER 10001:10001

ENTRYPOINT ["/app/rustybin"]
