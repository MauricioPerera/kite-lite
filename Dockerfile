# Build the engine outside the runtime image.
FROM rust:latest AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

# Small, non-root runtime image.
FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 kite
COPY --from=builder /build/target/release/kite-lite /usr/local/bin/kite-lite
COPY --from=builder /build/target/release/kite-lite-js /usr/local/bin/kite-lite-js
USER kite
WORKDIR /home/kite
ENTRYPOINT ["/usr/local/bin/kite-lite"]

# Resource limits are a `docker run` concern, not something this image can
# declare — measured floor (see README's "Recursos minimos"): Docker itself
# refuses --memory below 6m, and real peak usage against a typical page
# stays under 7 MiB even for PNG/PDF rendering. Recommended, with margin:
#   docker run --rm \
#     --memory=32m --cpus=0.2 --pids-limit=16 \
#     --read-only --tmpfs /tmp:rw,noexec,nosuid,size=16m \
#     --cap-drop=ALL --security-opt=no-new-privileges \
#     kite-lite:dev cdp 0.0.0.0:9222
