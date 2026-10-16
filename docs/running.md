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
change which store it uses. Client configuration formats vary; many accept
this shape:

```json
{
  "mcpServers": {
    "atelier": {
      "command": "atelier",
      "env": { "ATELIER_HOME": "/absolute/path/to/your/art" }
    }
  }
}
```

## Agent skills

The bundled skills describe sprite creation, scene composition, and review:

```sh
atelier skills                        # list the bundled skills
atelier skills show sprite            # read a guide without installing it
atelier skills install --for codex
```

The installer also accepts `claude`, `cursor`, or `all`. It writes the
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
atelier replay sprite.recipe --home ./replay-store
```

Replace `<doc-id>` with a UUID from the library. Archives preserve UUIDs and
refuse to overwrite existing documents unless `--replace --yes` is supplied.
Replays rebuild documents from recorded tool calls; use a separate store to
inspect a recipe without affecting your working library.

Tools never discard saved work. A call on an unsupported or corrupt document
returns an error and leaves its files unchanged. Use `library verify` to see
what is wrong, and copy the document's directory before changing anything.
`atelier library reset <doc-id>` replaces a damaged document with a blank one,
keeping its UUID and any readable name and dimensions; it asks for
confirmation and does nothing to a healthy document.

A document saved in a format this build does not read is not damaged. Tools
say so, and `library reset` leaves it alone unless `--discard-unsupported` is
passed; open it with the Atelier version that wrote it instead.

## Recipes

Every document keeps a recipe, `doc.recipe`, beside its pixels: the calls
that changed it, one per line, exactly as the tools received them. `atelier
replay` rebuilds the document from that file alone.

```text
{"name":"doc_new","arguments":{"doc_id":"48080aae-72aa-4f03-94ec-70b69e2791b0","height":32,"name":"potion-flask","width":32}}
{"name":"doc_palette","arguments":{"colors":["#1a1630","#f0b07a","#5ad8f8"],"doc_id":"48080aae-72aa-4f03-94ec-70b69e2791b0","op":"set"}}
{"name":"doc_layer","arguments":{"doc_id":"48080aae-72aa-4f03-94ec-70b69e2791b0","index":0,"name":"glass back","op":"rename"}}
{"name":"doc_paint_grid","arguments":{"doc_id":"48080aae-72aa-4f03-94ec-70b69e2791b0","frame":0,"layer":3,"legend":{"C":"#5ad8f8","K":0},"rows":[".KKKKKK.","KCCCCCCK","KCCCCCCK",".KKKKKK."],"x":12,"y":2}}
```

- Each line is one MCP `tools/call`: the tool `name` and its `arguments`,
  as JSON. Argument keys are written in sorted order; nothing else is
  changed, abbreviated, or implied.
- Only calls that change the document are recorded. Reads, exports, and
  failed calls are not.
- Blank lines and lines that start with `#` are ignored, so a recipe can carry
  notes.

A recipe records calls, not pixels, so what it draws depends on how the tools
behave. When a fix changes what a tool draws, the recipes that depend on it
are re-recorded rather than edited; the showcase's CI job replays every recipe
and compares the result with the committed animation, so such a change cannot
slip through unnoticed.

To see what a recipe draws without saving anything, add `--show`. Each step is
printed with its layer name and the pixels it changed, in terminal colour:

```sh
atelier replay sprite.recipe --show | less -R
```

## HTTP server and Linux daemon

Run a foreground server:

```sh
atelier --http                       # 127.0.0.1:8765, endpoint /mcp
```

For a background server on Linux with `systemd --user`:

```sh
atelier install --port 8765
atelier status
```

Use `atelier install --port 8765 --home /absolute/path/to/store` to select the
daemon's store explicitly. Without it, the daemon uses `ATELIER_HOME` or
`~/.atelier`, rather than the directory-local store. `atelier status` reports
daemon state and log locations. `atelier uninstall` stops and removes the
daemon as part of [removing Atelier](#uninstall).

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

## Uninstall

```sh
atelier uninstall                 # shows what will go, then asks
atelier uninstall --documents -y  # also delete ~/.atelier, without asking
```

The command removes everything Atelier placed on the machine: the Linux
daemon and its unit file, the skills that `atelier skills install` wrote for
Claude Code, Codex, and Cursor, and the `atelier` binary itself. A binary
that `cargo install` placed is removed through `cargo uninstall`, so cargo's
record of it goes too. It prints the plan and waits for a typed `yes`
unless `--yes` is passed; without a terminal, `--yes` is required.

Your documents are kept. The global store, `ATELIER_HOME` or `~/.atelier`, is
deleted only with `--documents`. Directory-local `./.atelier` stores made with
`atelier init` are never touched, and neither is an MCP client's registration;
in Claude Code, run `claude mcp remove atelier`.

## Container image

MCP clients can launch the published linux/amd64 image over stdio. Keep the
volume mount, or documents are lost when the container exits:

```json
{
  "mcpServers": {
    "atelier": {
      "command": "docker",
      "args": [
        "run", "-i", "--rm",
        "--mount", "type=volume,src=atelier-data,dst=/data",
        "ghcr.io/marmikshah/atelier:latest"
      ]
    }
  }
}
```

Files a tool writes with `out_path` stay inside the container unless you add
a bind mount for them. With no arguments the image serves stdio; pass
`--http 0.0.0.0:8765` and `ATELIER_HTTP_TOKEN` to serve HTTP instead.

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
