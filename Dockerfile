# Build stage
FROM rust:1.82-slim AS builder

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    pkg-config libssl-dev protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY . .
RUN cargo build --release && \
    cp target/release/calld /usr/local/bin/calld

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -r -s /bin/false calld
RUN mkdir -p /var/lib/callchain && chown calld:calld /var/lib/callchain

COPY --from=builder /usr/local/bin/calld /usr/local/bin/calld

USER calld

EXPOSE 5005 5006 51235 9090

VOLUME ["/var/lib/callchain"]

ENTRYPOINT ["calld"]
CMD ["--data-dir", "/var/lib/callchain"]
