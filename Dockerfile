# syntax=docker/dockerfile:1
#
# atelier — the supported Alpine release image for linux/amd64.
# One static musl binary, a non-root account, and no runtime packages.
# Runs the streamable-HTTP transport (container-native); point an MCP client at
#   http://<host>:8765/mcp
# The 0.0.0.0 listener requires a user-supplied ATELIER_HTTP_TOKEN. No token is
# embedded in the image.
# Persist documents by mounting a volume at /data (ATELIER_HOME).

# ---- build -------------------------------------------------------------------
FROM rust:1.97.1-alpine3.22 AS build
# musl is the default target here, so the binary is fully static — no package
# installs at all (the default build links no openssl, no C deps).
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p atelier \
 && strip target/release/atelier

# ---- runtime -----------------------------------------------------------------
FROM alpine:3.24
LABEL org.opencontainers.image.title="atelier" \
      org.opencontainers.image.description="Offline, headless pixel-art editor for CLI and MCP workflows" \
      org.opencontainers.image.licenses="MIT"
# Non-root, unprivileged; documents live in a mounted volume it owns.
RUN addgroup -S atelier \
 && adduser -S -u 10001 -G atelier atelier \
 && mkdir -p /data \
 && chown atelier:atelier /data
COPY --from=build /src/target/release/atelier /usr/local/bin/atelier
COPY --from=build /src/LICENSE /usr/share/licenses/atelier/LICENSE
USER atelier
ENV ATELIER_HOME=/data
VOLUME ["/data"]
EXPOSE 8765
ENTRYPOINT ["atelier"]
CMD ["--http", "0.0.0.0:8765"]
