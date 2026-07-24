# syntax=docker/dockerfile:1
#
# atelier — the headless pixel-art MCP server, containerized.
# One image, one flavour: a static musl binary on Alpine (~15 MB total).
# Runs the streamable-HTTP transport (container-native); point an MCP client at
#   http://<host>:8765/mcp
# Persist documents by mounting a volume at /data (ATELIER_HOME).

# ---- build -------------------------------------------------------------------
FROM rust:1.96.0-alpine3.22 AS build
# musl is the default target here, so the binary is fully static — no package
# installs at all (the default build links no openssl, no C deps).
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p atelier \
 && strip target/release/atelier

# ---- runtime -----------------------------------------------------------------
FROM alpine:3.22
# Non-root, unprivileged; documents live in a mounted volume it owns.
RUN addgroup -S atelier \
 && adduser -S -u 10001 -G atelier atelier \
 && mkdir -p /data \
 && chown atelier:atelier /data
COPY --from=build /src/target/release/atelier /usr/local/bin/atelier
USER atelier
ENV ATELIER_HOME=/data
VOLUME ["/data"]
EXPOSE 8765
ENTRYPOINT ["atelier"]
CMD ["--http", "0.0.0.0:8765"]
