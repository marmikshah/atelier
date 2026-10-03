# Contributing

Outside pull requests are currently closed. Bug reports and questions are
welcome; include the checkout commit and a minimal command sequence or journal.

Maintainers should work through issues and reviewable pull requests. Keep each
change focused; stack dependent branches on the preceding PR's branch. Run
`tools/check.sh`, the showcase tooling tests, and the replay verifier for editor
changes. Run the website checks and production build for site changes. The
[README](../README.md) lists these commands; tool `--help` provides details.

Preserve tool schemas, pixels, exports, platform integrations, and recorded
artwork. Retain tests that prove supported behavior, and add regressions for
meaningful changes. Include validation evidence and anything unverified in PRs.

Report vulnerabilities privately through [SECURITY.md](SECURITY.md). Follow the
[Code of Conduct](CODE_OF_CONDUCT.md). The project is [MIT licensed](../LICENSE).
