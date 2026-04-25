# Build stage
FROM rust:1.94-slim AS builder

# Install build dependencies in stages to avoid OOM during image build.
# `libllvm19` + `clang` are very large; installing them together with
# everything else can exhaust the Docker builder memory limit.
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    ca-certificates git pkg-config libssl-dev protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    libclang-dev clang \
    && rm -rf /var/lib/apt/lists/*

ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

WORKDIR /build
COPY . .
RUN cargo build --release && \
    cp target/release/calld /usr/local/bin/calld

# Runtime stage
FROM debian:trixie-slim

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
