# Development

[README](../README.md) · [Getting started](getting-started.md) · [Running Atelier](running.md)

Run commands below from the repository root. The [README](../README.md#contributing)
states the contribution policy.

## Repository map

| Path | Responsibility |
| --- | --- |
| `crates/atelier/` | CLI commands, replay, archives, agent skills, and Linux daemon |
| `crates/atelier-mcp/` | Tool schemas and dispatch, plus stdio and HTTP transports |
| `crates/atelier-studio/` | Storage, transactions, journals, checkpoints, and analysis |
| `crates/atelier-core/` | Document model, pixel operations, rendering, and exports |
| `site/` | React + TypeScript showcase website, built with Vite |
| `showcase/` | Frozen briefs, recorded runs, recipes, and GIFs |
| `tools/` | Development checks, showcase runner, and container smoke test |

An edit travels through these layers:

```text
CLI call or MCP request
  → atelier-mcp: validate arguments and dispatch the tool
  → atelier-studio: load, edit, journal, and persist the document
  → atelier-core: operate on pixels, layers, and frames
```

Start with [main.rs](../crates/atelier/src/main.rs) for CLI routing,
[server/mod.rs](../crates/atelier-mcp/src/server/mod.rs) for tool dispatch, and
[params.rs](../crates/atelier-mcp/src/server/params.rs) for input schemas.
The core has no MCP or server dependencies. The website displays recorded
showcase results; it is separate from the editor.

## Build and check the editor

Use Linux or macOS with a native build toolchain and Rust installed through
rustup. [rust-toolchain.toml](../rust-toolchain.toml) pins the development
version and components, which is also the minimum supported version.
Python 3 is needed for showcase tooling.

```sh
cargo build --locked -p atelier
./target/debug/atelier --help
tools/check.sh
python3 -m unittest discover -s tools/tests
python3 tools/showcase.py verify
```

`tools/check.sh` checks formatting, strict Clippy, rustdoc, and Rust tests.
The Python tests cover showcase tooling. The replay verifier rebuilds every
committed showcase GIF offline and checks for byte-identical output; build the
editor first so `target/debug/atelier` exists.

For an optimized binary, an installed build of your checkout, or browsable
Rust API documentation:

```sh
cargo build --release --locked -p atelier
cargo install --locked --path crates/atelier
cargo doc --locked --no-deps --open
```

## Work on the website

Use Node.js 24+:

```sh
npm ci --prefix site
npm --prefix site run dev
```

The dev command prepares showcase data and starts Vite with hot reload. Before
submitting site changes, run:

```sh
npm --prefix site run check
npm --prefix site run build
```

The check covers formatting, strict TypeScript, and comparison tests. The
production build is written to `site/dist`.

## Understand and reproduce the showcase

[showcase/runs.json](../showcase/runs.json) describes the runs and tasks.
Each recorded animation has a recipe under `showcase/replays/` and a GIF
under `showcase/gifs/`. The website prepares its data from these committed files.

To inspect one recipe in an isolated store:

```sh
./target/debug/atelier replay showcase/replays/opus-5.5-xhigh/cat.recipe --home ./replay-store
./target/debug/atelier library --home ./replay-store
```

Use the returned document ID with the tools from [Getting started](getting-started.md),
adding `--home ./replay-store` to each call. To collect new runs, start with
`python3 tools/showcase.py run --help`. Collection drives an authenticated Codex
or Claude Code CLI, chosen with `--client`, and writes to the ignored
`target/showcase/` directory; verifying existing artwork needs neither.

## CI and publishing

CI checks native builds on Ubuntu and macOS and the showcase tooling. A separate workflow verifies replay bytes. Linux container
checks run on pushes to `master` and non-draft PRs targeting `master`. Pull
requests that change [install.sh](../install.sh), the [npm wrapper](../npm),
or [binaries.yml](../.github/workflows/binaries.yml) build the release
binaries and install each one through both installers, on its own platform
and on fresh Ubuntu 22.04, 24.04, and 26.04 images.

The Pages workflow checks and builds website changes, then publishes the showcase
from `master`. See [Running Atelier](running.md#docker-compose) for local
container use.

## Releasing

Releases are never tagged by hand. Merge the version bump to `master`, with
`Cargo.toml`, [server.json](../server.json), and
[npm/package.json](../npm/package.json) naming the same `X.Y.Z`
(`tools/check-version.sh`), then dispatch the Release workflow from the
Actions page on `master`. The run:

1. Verifies it was started on the live `master` commit, that the version is
   consistent, and that no tag, GitHub Release, or image for `vX.Y.Z` exists
   yet (`tools/release-vacancy.sh`).
2. Runs the CI and Showcase workflows as its gate.
3. Builds the native binaries for macOS and Linux on x86-64 and arm64
   ([binaries.yml](../.github/workflows/binaries.yml)), installing,
   using, and uninstalling each through [install.sh](../install.sh) and
   the npm wrapper.
4. Builds and smoke-tests the linux/amd64 image, then pushes it to
   `ghcr.io/marmikshah/atelier` as `X.Y.Z` and `latest`.
5. Creates the `vX.Y.Z` tag and a GitHub Release recording the commit and
   image digest, with the binary archives, `install.sh`, and `SHA256SUMS`
   attached. The installer downloads from the latest release. The tag is
   written last, so a failed run leaves no tag.
6. Publishes [npm/](../npm) as `@marmikshah/atelier`, whose first start
   downloads that release's binary, with npm trusted publishing: no npm
   token is stored, and the package carries provenance. A version that is
   already on npm is skipped.
7. Publishes `server.json` to the MCP Registry, which checks that the npm
   package's `mcpName` names this server.

The `publish`, `npm`, and `registry` jobs run in the `release` environment,
where protection rules such as required reviewers can be added. The registry reads
the image's labels anonymously, so the GHCR package must stay public; a
package pushed by this public repository's workflow is public from its first
release.

npm only accepts trusted publishing for a package that already exists, so
the first release that ships the npm package needs one hand step. Its `npm`
job fails; then, from a checkout of the release commit, signed in to npm as
`marmikshah`:

```sh
cp LICENSE npm/LICENSE
npm publish ./npm
```

On the package's settings page on npmjs.com, add a trusted publisher:
GitHub Actions, repository `marmikshah/atelier`, workflow `release.yml`,
environment `release`. Re-run the failed jobs; the `npm` job finds the
version published and the `registry` job goes ahead.
