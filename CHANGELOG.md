# Changelog

Format follows [Keep a Changelog](https://keepachangelog.com/). Any release
may contain breaking changes.

Releases before this train are archived in
[docs/CHANGELOG-1.x.md](docs/CHANGELOG-1.x.md).

## [0.1.0] — Unreleased

Version reset. The editor, the tool surface, the document format, and the
shipped `linux/amd64` container are unchanged from the retired `1.9.1`; only
the version number and the release policy changed.

### Changed

- Versions restarted at `0.1.0`. The `1.9.1` number claimed a stability the
  project never had; every release in that series was free to break the tool
  surface. `1.0.0` replaces `2.0.0` as the version reserved for an explicit
  maintainer decision; it is not planned.
- Atelier is closed to outside pull requests. The repository is entirely
  AI-generated with no line-by-line human review, so a merged contribution
  would be reviewed by nobody who understands the code around it. Bug reports,
  questions, and private security disclosures are still welcome, and the MIT
  licence still permits any fork.
- Only one release exists at a time. Publishing a new release now removes the
  previous release's page and archives, because releases break and maintaining
  several in parallel is out of scope for now. Git tags always survive, so any
  past version can be checked out and built from source, and migration steps
  ship with the release that requires them.
- Release pages for `v1.0.0` through `v1.9.1` were removed along with their
  archives. Their git tags remain, so any of those versions can still be
  checked out and built from source; `ATELIER_VERSION` pins to a `v1.x` tag no
  longer resolve, and `tools/install.sh` installs the latest release.
- Every `library` subcommand now accepts `--home DIR`, not just `pack` and
  `unpack`. Listing, verifying, and deleting an isolated store previously
  needed `ATELIER_HOME`, so `--home` meant different things depending on the
  subcommand. `atelier library --home DIR` lists that store directly.
- `atelier replay <doc-id> --home DIR` read the document's journal from the
  default store and rebuilt it in `--home`, so a bare document id silently
  replayed the wrong document — or failed with `no file or document` when the
  id existed only in the isolated store. Both the source journal and the
  rebuilt document now come from the same store.
- `.env.example` was removed. It listed variables for three unrelated scopes —
  the binary, `tools/install.sh`, and `docker-compose.yml` — and nothing loaded
  it, so it drifted instead of documenting. Each of those three documents its
  own variables.
- `AGENTS.md` and `CLAUDE.md` were removed. The repository no longer carries
  agent instructions of its own.
- The README was rewritten to roughly a third of its length. It now covers what
  Atelier is, how to install it, a thirty-second example, and the experiment
  behind it; the reference material it used to carry moved to
  [docs/cli.md](docs/cli.md) and [docs/mcp.md](docs/mcp.md) unchanged.
- The Rust toolchain moved from 1.97.1 to 1.98.0 across `rust-toolchain.toml`,
  the `Dockerfile` build stage, and every CI, Pages, and release workflow. The
  tested minimum supported compiler is unchanged at 1.88.
