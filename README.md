<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-wordmark-dark.png">
    <img src="assets/logo-wordmark.png" width="384" alt="atelier">
  </picture>
</p>

Atelier is an offline, headless pixel-art editor for command-line tools and AI
agents. Create layered sprites and animations, inspect pixels, and export PNGs,
spritesheets, GIFs, APNGs, and TrueType pixel fonts. Documents stay local.

Use it directly from a shell or connect an agent through MCP (Model Context
Protocol). Both interfaces expose the same editing tools; no graphical editor
or running server is needed for CLI calls.

## Showcase

- [Animation showcase](https://marmikshah.github.io/atelier/) — compare models and briefs, view animations, and download replays.
- [Frayed](https://apps.apple.com/app/id6806894761)

## Get started

Build from source on **Linux or macOS** with Rust 1.88+ and a native build
toolchain. The checkout pins the Rust version used for development. No binaries
or container images are published.

```sh
git clone https://github.com/marmikshah/atelier.git
cd atelier
cargo install --locked --path crates/atelier
atelier --help
```

Then follow [your first sprite](docs/getting-started.md) to create a document,
draw pixels, and save an image.

| I want to… | Start here |
| --- | --- |
| Make art from the terminal | [Getting started](docs/getting-started.md) |
| Connect an agent, run a server, or choose where art is stored | [Running Atelier](docs/running.md) |
| Find a tool's arguments | `atelier tools --markdown` or `atelier tools --schema NAME` |
| Understand the code or work on the showcase | [Development guide](docs/development.md) |
| Report a bug or security issue | [Contributing](.github/CONTRIBUTING.md) · [Security](.github/SECURITY.md) |

## About this project

**100% of this code was written by AI.** Atelier is a personal experiment in
whether agents, using only tool calls, can make art useful in games. My part
has been giving direction, and I hope the result helps you make something of
your own.

> [!NOTE]
> **None of the code has had line-by-line human review.** Automated checks
> and AI reviews are part of the process; bugs, security issues, and breaking
> changes are still possible. Please review the code and work with copies of
> important data before using it in production.

[MIT](LICENSE) © Marmik Shah
