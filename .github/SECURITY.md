# Security Policy

## Reporting a vulnerability

Report security issues **privately** via GitHub's
[Report a vulnerability](https://github.com/marmikshah/atelier/security/advisories/new)
(the repo's Security → Advisories tab). Please do **not** open a public issue for
security problems.

I'll acknowledge as soon as I can and work with you on a fix and disclosure
timeline.

## Status and threat model

- atelier runs **headless and without outbound services or telemetry**. It
  reads and writes image documents under `ATELIER_HOME` and can expose an MCP
  HTTP listener when requested.
- The HTTP transport binds **loopback by default**. A non-loopback bind refuses
  to start without `ATELIER_HTTP_TOKEN`; whenever the token is configured,
  every request must send `Authorization: Bearer <token>`. Use TLS at a reverse
  proxy before sending the bearer token across an untrusted network.
- HTTP callers cannot read or write arbitrary paths. External input and output
  are disabled unless `ATELIER_IMPORT_ROOT` and/or `ATELIER_EXPORT_ROOT` are
  configured, and calls must use relative paths confined to those roots.
  CLI and stdio workflows retain normal local filesystem access.
- `ATELIER_ALLOWED_HOSTS` is an additional Host-header/DNS-rebinding guard; it
  is not a substitute for bearer authentication.
- HTTP request bodies are limited to 1 MiB, including chunked transfers; body
  uploads time out after 30 seconds and at most 64 requests run concurrently.
  Persisted document metadata is bounded, and normal store reads refuse
  symlinked document directories, metadata, cels, references, and journals.
- Below **v2.0.0** the code is AI-generated and has not been fully reviewed by
  the maintainer. Assume bugs — including security bugs — may exist, and use at
  your own risk (see the README notice).

## Supported versions

Only the latest release receives fixes. There are no backports below the latest
tag while the project is pre-2.0.0. Native support is limited to Ubuntu 22.04 or
newer on x86_64; the Alpine linux/amd64 container is the only other supported
runtime.
