# Changelog

Format follows [Keep a Changelog](https://keepachangelog.com/). Any release
may contain breaking changes, and only the newest release is ever available.

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
  previous one entirely — its release page, its archives, its tag, and its
  container images — because releases break and maintaining several in parallel
  is out of scope for now. Superseded releases are not archived, so migration
  steps ship with the release that requires them.
- The companion `atelier-site` repository was folded back in and GitHub Pages
  was switched off. The site's logos and demo animation now live in `assets/`,
  its seventy-run model showcase in `showcase/` with a generated
  `showcase/README.md`, and its replay verification in
  `tools/showcase-check.sh`, run by a new on-demand `Showcase` workflow.
- The tool reference is Markdown instead of a published HTML page. `atelier
  tools --markdown` replaces `--html`, `make docs` writes the committed
  `docs/tools.md`, and `make docs-check` now fails when that file drifts from
  the registry rather than only checking that it renders.
- The installer is served from `raw.githubusercontent.com` rather than the
  retired Pages site, so the one-liner in the README changed.
- Every release predating this reset was removed under that policy, along with
  its tag and images. `tools/install.sh` installs whatever the current release
  is.
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
