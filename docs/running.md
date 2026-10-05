# Running Atelier

[README](../README.md) · [Getting started](getting-started.md) · [Development](development.md)

## Choose an interface

| Interface | How to use it | Best for |
| --- | --- | --- |
| CLI | `atelier call TOOL '<json>'` | Shell scripts and agents with shell access |
| MCP over stdio | Have your client launch `atelier` with no arguments | Clients that manage their own server process |
| MCP over HTTP | Run `atelier --http`, then connect to `http://127.0.0.1:8765/mcp` | Clients connecting to a running server |

All three use the same tools and document format. CLI calls run in-process and
need no server. Running bare `atelier` in a terminal waits for MCP input; use
`atelier --help` for interactive command help.

For a stdio client, set its server command to `atelier` (or the absolute path to
the binary) and leave its arguments empty. Set `ATELIER_HOME` in the client's
server environment to an absolute directory so its working directory does not
change which store it uses. Client configuration formats vary.

## Agent skills

The bundled skills describe sprite creation, scene composition, and review:

```sh
atelier skills                        # list the bundled skills
atelier skills show sprite            # read a guide without installing it
atelier skills install --for codex
```

The installer also accepts `claude`, `kimi`, `cursor`, or `all`. It writes the
bundled skills to the selected agent's user skill directory, replacing existing
copies. Use `--dir PATH` instead of `--for` to choose a destination.

## Document storage and recovery

Store selection follows this order:

1. `--home DIR` on commands that support it (`call`, `library`, and `replay`).
2. The `ATELIER_HOME` environment variable.
3. `./.atelier`, if it exists in the current working directory.
4. `~/.atelier`.

`atelier init` creates the directory-local store. It does not override
`ATELIER_HOME`, and Atelier does not search parent directories for a store.
Documents use UUIDs; names are labels rather than identifiers.

```sh
atelier library
atelier library verify               # inspect saved data without changing it
atelier library pack <doc-id> --out sprite.atelierpack
atelier library unpack sprite.atelierpack
atelier replay recipe.jsonl --home ./replay-store
```

Replace `<doc-id>` with a UUID from the library. Archives preserve UUIDs and
refuse to overwrite existing documents unless `--replace --yes` is supplied.
Replays rebuild documents from recorded tool calls; use a separate store to
inspect a recipe without affecting your working library.

Normal tool access can reset unsupported or corrupt documents to fresh data,
keeping the UUID and valid name/dimensions. Use `library verify` first when
investigating damaged data, and keep backups of important work.

## HTTP server and Linux daemon

Run a foreground server:

```sh
atelier --http                       # 127.0.0.1:8765, endpoint /mcp
```

For a background server on Linux with `systemd --user`:

```sh
atelier install --port 8765
atelier status
atelier uninstall
```

Use `atelier install --port 8765 --home /absolute/path/to/store` to select the
daemon's store explicitly. Without it, the daemon uses `ATELIER_HOME` or
`~/.atelier`, rather than the directory-local store. `atelier status` reports
daemon state and log locations.

HTTP binds to loopback by default. A non-loopback bind requires
`ATELIER_HTTP_TOKEN`; whenever it is set, clients must send
`Authorization: Bearer <token>`. See the [security policy](../.github/SECURITY.md)
for network and file-access boundaries.

| Environment variable | Purpose |
| --- | --- |
| `ATELIER_HOME` | Document store directory |
| `ATELIER_HTTP` | HTTP bind address; alternative to `--http` |
| `ATELIER_HTTP_TOKEN` | HTTP bearer token |
| `ATELIER_ALLOWED_HOSTS` | Comma-separated additional allowed Host headers |
| `ATELIER_IMPORT_ROOT` | HTTP root for relative reference-image paths |
| `ATELIER_EXPORT_ROOT` | HTTP root for relative output paths |
| `ATELIER_LOG` | Log filter, using `RUST_LOG` syntax |

HTTP external file access is disabled unless the corresponding roots are set.
CLI and stdio calls use normal local filesystem access.

## Docker Compose

From the repository root, set a token and build the local image:

```sh
export ATELIER_HTTP_TOKEN='replace-with-your-own-secret'
docker compose up -d --build
```

Connect to `http://127.0.0.1:8765/mcp` with that bearer token. Compose binds the
host port to localhost and stores documents in the `atelier-data` named volume.
`docker compose down` stops the service and retains that volume.

The [Compose file](../docker-compose.yml) documents platform selection and
optional mounts for HTTP imports and exports. To check a built image, run
`tools/container-smoke.sh atelier:local`; use `--help` for options.
