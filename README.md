<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-wordmark-dark.png">
    <img src="assets/logo-wordmark.png" width="384" alt="atelier">
  </picture>
</p>

Atelier is an offline, headless pixel-art editor for command-line tools and AI
agents. Create layered sprites and animations, inspect pixels, and export PNGs,
spritesheets, GIFs, APNGs, and TrueType pixel fonts. Documents stay local.

Use it directly from a shell, or connect an agent such as Claude Code or
Codex through MCP (Model Context Protocol). Both interfaces expose the same
editing tools; no graphical editor or running server is needed for CLI calls.

<p align="center">
  <img src="showcase/gifs/opus-5.5-xhigh/cat.gif" width="128" height="128" alt="Animated pixel-art cat">
  <img src="showcase/gifs/opus-5.5-xhigh/torch.gif" width="128" height="128" alt="Animated pixel-art torch">
  <img src="showcase/gifs/opus-5.5-xhigh/potion.gif" width="128" height="128" alt="Animated pixel-art potion">
  <img src="showcase/gifs/opus-5.5-xhigh/explosion.gif" width="128" height="128" alt="Animated pixel-art explosion">
</p>

<p align="center">
  Drawn by an agent through Atelier's tools. Each is rebuilt byte for byte from
  the <a href="showcase/replays/opus-5.5-xhigh">recipe beside it</a> in CI.
</p>

## Showcase

- [Animation showcase](https://marmikshah.github.io/atelier/) — compare models and briefs, view animations, and download replays.
- [Frayed](https://apps.apple.com/app/id6806894761)

## Install

On macOS, Ubuntu, or any other Linux, paste this into a terminal:

```sh
curl -fsSL https://github.com/marmikshah/atelier/releases/latest/download/install.sh | sh
```

Ubuntu Desktop comes without curl; use wget instead:

```sh
wget -qO- https://github.com/marmikshah/atelier/releases/latest/download/install.sh | sh
```

It downloads Atelier for your machine, checks it, puts it in `~/.local/bin`,
and adds that to your `PATH`. If Claude Code or Codex is installed, it also
connects Atelier to them: they get its drawing tools in every project and its
guides for drawing sprites and scenes and reviewing art. Nothing else is
needed, not even Rust.

Then start a new agent session and ask for something:

> Use atelier to draw a 32×32 animated torch and save it as torch.gif.

Your art is kept in `~/.atelier`. To keep a project's art inside the project
instead, run `atelier init` in its folder before you start the agent.

- **Update:** run the install command again.
- **Installed Claude Code or Codex afterwards?** Run `atelier connect`.
- **A specific version or folder, or no profile changes:** set
  `ATELIER_VERSION`, `ATELIER_INSTALL_DIR`, or `ATELIER_NO_MODIFY_PATH=1`;
  [install.sh](install.sh) lists every option.

### Cursor, VS Code, and other MCP clients

No install step: the client starts Atelier through npx, which needs Node.js
18 or later.

[<img src="https://cursor.com/deeplink/mcp-install-dark.svg" alt="Add atelier to Cursor" height="32">](https://cursor.com/en/install-mcp?name=atelier&config=eyJjb21tYW5kIjoibnB4IiwiYXJncyI6WyIteSIsIkBtYXJtaWtzaGFoL2F0ZWxpZXIiXX0%3D)
[<img src="https://img.shields.io/badge/VS_Code-Install_atelier-0098FF?style=flat-square" alt="Install atelier in VS Code" height="32">](https://insiders.vscode.dev/redirect?url=vscode%3Amcp%2Finstall%3F%257B%2522name%2522%253A%2522atelier%2522%252C%2522command%2522%253A%2522npx%2522%252C%2522args%2522%253A%255B%2522-y%2522%252C%2522%2540marmikshah%252Fatelier%2522%255D%257D)

For any other client, add this to its MCP configuration:

```json
{
  "mcpServers": {
    "atelier": { "command": "npx", "args": ["-y", "@marmikshah/atelier"] }
  }
}
```

The first start downloads Atelier for your machine and caches it. To give
the client Atelier's drawing guides too, install it as above and run
`atelier skills install --for cursor`, or `--dir PATH` for another skills
folder.

## Uninstall

```sh
atelier uninstall
```

It lists what it will remove and asks before deleting anything: Atelier's
registration in Claude Code and Codex, its skills, the `PATH` line it added,
the `atelier` program, cached downloads, and the Linux background server if
you set one up. Your art in `~/.atelier` is kept; add `--documents` to delete
it as well. Folders set up with `atelier init` are never touched.

If you only used npx, remove the entry from your client and run
`npx -y @marmikshah/atelier uninstall` to delete the cached download.

## Other ways to run it

- **From the terminal.** Every tool is one command:
  `atelier call TOOL '<json>'`. [Your first sprite](docs/getting-started.md)
  walks through creating, drawing, and saving an image.
- **In Docker.** A [container image](docs/running.md#container-image) is
  published for MCP clients that can launch Docker.
- **As an HTTP server or background service.** See
  [Running Atelier](docs/running.md).

### Build from source

On other platforms, or to build it yourself, install
[Rust](https://rustup.rs) 1.98 or later, then:

```sh
cargo install --locked --git https://github.com/marmikshah/atelier atelier
atelier connect
```

`atelier uninstall` removes a build installed this way too, through
`cargo uninstall`.

| I want to… | Start here |
| --- | --- |
| Make art from the terminal | [Getting started](docs/getting-started.md) |
| Run a server, use Docker, or choose where art is stored | [Running Atelier](docs/running.md) |
| Find a tool's arguments | `atelier tools --markdown` or `atelier tools --schema NAME` |
| Build from a checkout, understand the code, or work on the showcase | [Development guide](docs/development.md) |
| Report a bug or security issue | [Issues](https://github.com/marmikshah/atelier/issues) · [Security](.github/SECURITY.md) |

## Stability

Atelier is at 0.1. `master` is the current source; releases are dispatched
from it and publish the macOS and Linux binaries, the npm package, the
container image, a `vX.Y.Z` tag, and the MCP Registry entry.

- **Your documents are not discarded.** Edits are journaled and committed
  atomically. A document that is damaged, or saved by a build with a different
  document schema, is refused with an error and left unchanged on disk.
- **The document format is versioned, with no migration.** If an update
  changes it, documents saved before it are refused, stay on disk untouched,
  and open again with the commit that wrote them.
- **A recipe is the plain log of the calls that built a document**, one JSON
  line each. It has no version of its own: what it draws depends on how the
  tools behave, so a recipe from one commit replays exactly on commits whose
  tools draw the same way.
- **Tool names and arguments can still change** between commits. Pin a commit
  if you script against them, and read the current surface from
  `atelier tools --markdown`.

## How it is built

**100% of this code was written by AI.** Atelier began as an experiment in
whether agents, using only tool calls, can make art useful in games. My part
has been giving direction, and I hope the result helps you make something of
your own.

CI checks formatting, strict Clippy, and the Rust test suite on Linux and
macOS, smoke-tests the container, and replays every showcase recipe, which
must reproduce its GIF byte for byte. The [security policy](.github/SECURITY.md)
describes the network and file-access boundaries.

> [!NOTE]
> **None of the code has had line-by-line human review.** Bugs, security
> issues, and breaking changes are still possible. Please review the code and
> work with copies of important data before using it in production.

## Contributing

Bug reports and questions are welcome as issues; include the checkout commit and
a minimal command sequence or recipe file. Pull requests from outside the
project are not accepted. Report vulnerabilities privately through the
[security policy](.github/SECURITY.md).

[MIT](LICENSE) © Marmik Shah
