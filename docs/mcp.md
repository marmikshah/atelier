# Running atelier as an MCP server

The CLI needs none of this. `atelier call` works out of the box with no server,
daemon, or registration. This page is for clients that only speak MCP.

## The daemon

One command sets up a shared background server (`systemd --user`):

```sh
atelier install
# MCP HTTP port [8765]:
```

The prompt appears on both first install and reinstall; reinstall defaults to
the currently configured port. For scripts, use `atelier install --port 9123`.

Atelier deliberately does not rewrite third-party client configuration. Point
your MCP client at the endpoint printed by `atelier status`:

```text
http://127.0.0.1:8765/mcp
```

Or configure a stdio MCP server whose command is simply `atelier`. Stdio needs
no daemon: each client starts its own process, while all processes still
resolve the same global or directory-local document store.

Keep your client's normal approval prompts enabled: Atelier can write exports
and delete documents.

## Network and authentication

The installed systemd service is intentionally loopback-only. To serve another
interface, run an authenticated foreground process or the supported container:

```sh
export ATELIER_HTTP_TOKEN="$(openssl rand -hex 32)"
ATELIER_HTTP=0.0.0.0:9123 atelier
```

Non-loopback HTTP refuses to start without `ATELIER_HTTP_TOKEN`. Whenever that
variable is set, including on loopback, clients must send
`Authorization: Bearer <token>`. Use a TLS reverse proxy on untrusted networks;
the built-in listener is plain HTTP. `ATELIER_ALLOWED_HOSTS` adds Host-header
validation but does not replace authentication.

HTTP external file access is off by default. Set `ATELIER_IMPORT_ROOT` and/or
`ATELIER_EXPORT_ROOT` to existing directories to enable it, then pass only
relative `path`/`out_path` values beneath those roots. Absolute paths, parent
traversal, and symlink escapes are rejected. Direct CLI and stdio calls retain
normal local filesystem access. Request bodies are capped at 1 MiB with a
30-second upload deadline, and at most 64 requests run concurrently.

## Caller identity

Every call — CLI, replay, stdio, or HTTP — travels one dispatch path and logs
one line to stderr: tool name, op, target document, caller, duration, and the
error text when a call fails. The Ubuntu daemon collects this in the user
journal; tune verbosity with `ATELIER_LOG` (`RUST_LOG` syntax, default `info`).

When several agents share the daemon, every call logs a `caller=` identity: by
default the TCP peer address. Set an `X-Atelier-Caller` header in a client's
MCP config, or attach per-call `session` metadata, when the name must stay
stable across reconnects. The CLI and replay log as `cli` / `replay`.

MCP callers may attach one optional stable name for logs. It never supplies or
changes tool arguments:

```json
{
  "_meta": {
    "io.github.marmikshah.atelier/session": "sprite-pass"
  }
}
```

The server retains no request context. Journals record the exact arguments that
ran, so a replay never depends on a live session or another caller's state.

## Docker

The second supported release is a small Alpine image with a static musl binary
and no runtime packages. Because native installation is Ubuntu x86_64 only, the
container is also the practical way to run Atelier on macOS, Windows, and other
Linux distributions. It serves the same HTTP MCP endpoint:

```sh
export ATELIER_HTTP_TOKEN="$(openssl rand -hex 32)"
docker run -d --platform linux/amd64 \
  -p 127.0.0.1:9123:8765 \
  -v atelier-data:/data \
  -e ATELIER_HTTP_TOKEN \
  ghcr.io/marmikshah/atelier:latest
```

Documents persist in the `atelier-data` volume, so they survive restarts. Here
`9123` is the configurable host port; the Alpine container keeps its internal
endpoint on `8765`. Configure the same bearer token in the MCP client's HTTP
headers. The image contains no default token and refuses to start its network
listener without one. Optional import/export directories must be mounted and
enabled with the corresponding root variables.

There's a [`docker-compose.yml`](../docker-compose.yml) if you'd rather keep it
declarative.

### Building the image on another architecture

Published images are `linux/amd64` only, which is why the `docker run` above
pins that platform; elsewhere Docker runs them under emulation. Building from a
clone avoids that. Nothing in the `Dockerfile` is architecture-specific, so a
plain build produces a native image — this is the route for an Apple Silicon
Mac, a Graviton instance, or a Raspberry Pi 5:

```sh
docker build -t atelier:local .
docker run -d \
  -p 127.0.0.1:9123:8765 \
  -v atelier-data:/data \
  -e ATELIER_HTTP_TOKEN \
  atelier:local
```

Docker builds for the host platform by default; add `--platform` to pick a
different one (`docker build --platform linux/amd64 .` reproduces the released
image, emulated and therefore slow). With Compose, uncomment `build: .` and set
`ATELIER_PLATFORM` to your own platform so the service is not pinned to amd64:

```sh
ATELIER_PLATFORM=linux/arm64 docker compose up -d --build
```

Two things bound which architectures work. The image needs a Rust Alpine base,
published for `amd64`, `arm64`, and `ppc64le`; and the document store publishes
each generation with a `renameat2` syscall whose number differs per
architecture, so the build refuses any target Atelier has no number for rather
than falling back to a non-atomic rename. `linux/amd64` is built and
smoke-tested in CI and is the only released image, `linux/arm64` is verified by
hand with `tools/container-smoke.sh`, and anything else is yours to build and
test. Non-Linux targets do not compile at all: the store's atomicity and the
daemon both depend on Linux.

## Troubleshooting

- **Your client shows 0 tools** — restart it after registering; MCP clients read
  their server list at session start.
- **stdio or daemon?** The daemon is one shared server and store, and it
  survives reboots; stdio means each client spawns its own `atelier` process.
  Both use the same resolved store. The CLI needs neither transport.
- **The selected port is already in use** — rerun `atelier install` and choose
  another port (`--port PORT` in scripts). `atelier status` prints the installed
  endpoint; `atelier uninstall` stops the daemon.
- **Where are the logs?** Use `journalctl --user -u atelier -f` for the Ubuntu
  daemon. Verbosity is controlled by `ATELIER_LOG` (`RUST_LOG` syntax). In
  stdio mode the same log goes to the spawning client's stderr.
- **Uninstall everything** — `install.sh uninstall` (or `atelier uninstall` for
  just the daemon). Your documents in `~/.atelier` are kept; delete that
  directory too if you want them gone.
