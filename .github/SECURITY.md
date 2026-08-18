# Security Policy

## Reporting a vulnerability

Report security issues **privately** via GitHub's
[Report a vulnerability](https://github.com/marmikshah/atelier/security/advisories/new)
(the repo's Security → Advisories tab). Please do **not** open a public issue for
security problems.

I'll acknowledge as soon as I can and work with you on a fix and disclosure
timeline.

## Status and threat model

- atelier runs **headless, offline** — no API keys, no network calls, no
  telemetry. It reads and writes image documents under `ATELIER_HOME`.
- The HTTP transport binds **loopback by default**. Only expose it beyond
  localhost — on a network you trust — via `ATELIER_ALLOWED_HOSTS`; there is no
  authentication layer, so treat an exposed endpoint as fully trusted.
- Below **v2.0.0** the code is AI-generated and has not been fully reviewed by
  the maintainer. Assume bugs — including security bugs — may exist, and use at
  your own risk (see the README notice).

## Supported versions

Only the latest release receives fixes. There are no backports below the latest
tag while the project is pre-2.0.0. Native support is limited to Ubuntu 22.04 or
newer on x86_64; the Alpine linux/amd64 container is the only other supported
runtime.
