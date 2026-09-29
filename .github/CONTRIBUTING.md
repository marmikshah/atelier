# Contributing

First, thank you for wanting to. It genuinely means something that you got far
enough into this project to consider improving it.

**That said, Atelier is closed to outside pull requests.** They will be
declined without review, regardless of how good they are. That is not a
judgment of your work — it is a consequence of what this project is, and I would
rather say so plainly here than let you find out after writing the patch.

Bug reports and questions are very welcome. Open an issue, or follow
[SECURITY.md](SECURITY.md) to disclose a vulnerability privately — never in a
public issue. And the project is MIT licensed, so fork it and take it wherever
you like; you do not need my permission for any of it.

## Why pull requests are closed

Atelier is an experiment. I wanted to find out how far a real, useful piece of
software could get when it is written entirely by AI models, with a human
steering the direction but never the code.

So: 100% of this repository is AI-generated. I have written none of it by hand
and have not reviewed a line of it. What it has had instead is several rounds
of AI review and revision, across models — GPT 5.6 Sol, Fable 5, and Opus 4.8.
The majority of the early code came from Opus; the newer work came from GPT and
Fable. My part was deciding what should exist, what should not, and when
something was not good enough yet.

Tests, Clippy, rustdoc, locked dependencies, and bounded image operations all
pass, and none of that is the same as a person having read the code. Which is where your pull request would land: merged into a codebase
nobody understands well enough to review it against, me included. You would
deserve a better review than I could honestly give, and accepting the change
anyway would quietly break the one rule the experiment runs on.

## Building it yourself

Install the Rust toolchain declared in `rust-toolchain.toml`, clone the
repository, and run:

```sh
make check
```

The supported native development environment is Ubuntu 22.04 or newer on
x86_64. The supported container target is Alpine linux/amd64.

That non-mutating gate checks formatting, Clippy, rustdoc, tests, and the
committed tool reference.

Useful commands:

```sh
make fmt          # apply formatting
make test         # complete Rust test suite
make docs         # regenerate docs/tools.md
make showcase-check  # replay all 80 showcase recipes (~10 min)
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
