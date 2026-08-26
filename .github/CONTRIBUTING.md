# Contributing

**Atelier is closed to outside pull requests.** They will be declined without
review, regardless of quality. This is not a judgment of any contribution; it
is a consequence of how the repository is built. Please do not spend your time
on a change here.

Bug reports and questions are still welcome. Open an issue, or follow
[SECURITY.md](SECURITY.md) to disclose a vulnerability privately — never in a
public issue.

The project is MIT licensed. Fork it and take it wherever you like.

## Why pull requests are closed

100% of this repository is AI-generated. The maintainer has written none of it
by hand and has not performed a line-by-line review of any of it.

What the code has had instead is several rounds of AI review and revision,
across models: GPT 5.6 Sol, Fable 5, and Opus 4.8. The majority of the early
code came from Opus; the newer work came from GPT and Fable.

Tests, Clippy, rustdoc, locked dependencies, bounded image operations, and the
release gates all pass, and none of that is a substitute for human review. A
human contribution merged into an unreviewed AI-generated codebase would be
reviewed by nobody who understands the surrounding code — including the
maintainer. Accepting one would be dishonest about what happens to it.

## Building it yourself

Install the Rust toolchain declared in `rust-toolchain.toml`, clone the
repository, and run:

```sh
make check
```

The supported native development environment is Ubuntu 22.04 or newer on
x86_64. The supported container target is Alpine linux/amd64.

That non-mutating gate checks release metadata, formatting, Clippy, rustdoc,
tests, and tool-reference rendering.

Useful commands:

```sh
make fmt          # apply formatting
make test         # complete Rust test suite
make docs         # generate target/atelier-tools.html
make release      # optimized binary
```

The workspace commits `Cargo.lock` because it publishes executable artifacts.
Include its changes when adding or updating dependencies. Rust 1.88 remains the
minimum supported compiler and has a dedicated CI check.

The hooks in `.githooks/` are optional:

```sh
git config core.hooksPath .githooks
```

CI is authoritative even when the hooks are enabled.

---

A friendly note to close on: if any of this is useful to you, the tokens were
worth spending.
