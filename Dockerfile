# syntax=docker/dockerfile:1
#
# atelier — the headless pixel-art MCP server, containerized.
# Runs the streamable-HTTP transport (container-native); point an MCP client at
#   http://<host>:8765/mcp
# Persist documents by mounting a volume at /data (ATELIER_HOME).

# ---- build -------------------------------------------------------------------
FROM rust:1-slim-bookworm AS build
# pkg-config/libssl cover any transitive openssl-sys; harmless if unused.
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release -p atelier \
 && strip target/release/atelier

# ---- runtime -----------------------------------------------------------------
FROM debian:bookworm-slim
# Non-root, unprivileged; documents live in a mounted volume it owns.
RUN useradd --system --uid 10001 atelier \
 && mkdir -p /data \
 && chown atelier:atelier /data
COPY --from=build /src/target/release/atelier /usr/local/bin/atelier
USER atelier
ENV ATELIER_HOME=/data
VOLUME ["/data"]
EXPOSE 8765
ENTRYPOINT ["atelier"]
CMD ["--http", "0.0.0.0:8765"]
