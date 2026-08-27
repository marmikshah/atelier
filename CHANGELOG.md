# Changelog

Format follows [Keep a Changelog](https://keepachangelog.com/).

Atelier does not publish releases. There are no binaries, archives, or
container images — clone the repository and build it. Every change lands on
`master` through a pull request, so `master` is the version. This file records
what changed and why.

## Unreleased

### Changed

- Atelier is closed to outside pull requests. The repository is entirely
  AI-generated with no line-by-line human review, so a merged contribution
  would be reviewed by nobody who understands the code around it. Bug reports,
  questions, and private security disclosures are still welcome, and the MIT
  licence still permits any fork.
- Releasing is switched off entirely. The release and container-publishing
  workflows, `tools/install.sh`, `tools/release-check.sh`, and
  `docs/RELEASING.md` are all removed. Building from a clone is the only way to
  run Atelier, which is honest about where the project is: one user, maybe
  two.
- The companion `atelier-site` repository was folded back in and GitHub Pages
  was switched off. The site's logos and demo animation now live in `assets/`,
  its seventy-run model showcase in `showcase/` with a generated
  `showcase/README.md`, and its replay verification in
  `tools/showcase-check.sh`, run by a new on-demand `Showcase` workflow.
- The tool reference is Markdown instead of a published HTML page. `atelier
  tools --markdown` replaces `--html`, `make docs` writes the committed
  `docs/tools.md`, and `make docs-check` now fails when that file drifts from
  the registry rather than only checking that it renders.
- GitHub Pages is off and its deployments and environment are deleted, so the
  installer's one-liner had nowhere to be served from — another reason the
  installer went rather than being repointed.
- Every `library` subcommand now accepts `--home DIR`, not just `pack` and
  `unpack`. Listing, verifying, and deleting an isolated store previously
  needed `ATELIER_HOME`, so `--home` meant different things depending on the
  subcommand. `atelier library --home DIR` lists that store directly.
- `atelier replay <doc-id> --home DIR` read the document's journal from the
  default store and rebuilt it in `--home`, so a bare document id silently
  replayed the wrong document — or failed with `no file or document` when the
  id existed only in the isolated store. Both the source journal and the
  rebuilt document now come from the same store.
- `.env.example` was removed. It listed variables for three unrelated scopes,
  and nothing loaded it, so it drifted instead of documenting. The binary's own
  `ENVIRONMENT` help and `docker-compose.yml` document theirs.
- `AGENTS.md` and `CLAUDE.md` were removed. The repository no longer carries
  agent instructions of its own.
- The README was rewritten to roughly a third of its length. It now covers what
  Atelier is, how to install it, a thirty-second example, and the experiment
  behind it; the reference material it used to carry moved to
  [docs/cli.md](docs/cli.md) and [docs/mcp.md](docs/mcp.md) unchanged.
- The Rust toolchain moved from 1.97.1 to 1.98.0 across `rust-toolchain.toml`,
  the `Dockerfile` build stage, and every CI, Pages, and release workflow. The
  tested minimum supported compiler is unchanged at 1.88.
