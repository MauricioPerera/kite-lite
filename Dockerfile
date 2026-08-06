# Build the engine outside the runtime image.
FROM rust:latest AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

# Small, non-root runtime image.
FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 kite
COPY --from=builder /build/target/release/kite-lite /usr/local/bin/kite-lite
USER kite
WORKDIR /home/kite
ENTRYPOINT ["/usr/local/bin/kite-lite"]
