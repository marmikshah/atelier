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
  surface. `1.0.0` now marks the point at which breaking changes stop being
  routine, replacing `2.0.0` as the maintainer's manual-review milestone.
- Atelier is closed to outside pull requests. The repository is entirely
  AI-generated with no line-by-line human review, so a merged contribution
  would be reviewed by nobody who understands the code around it. Bug reports,
  questions, and private security disclosures are still welcome, and the MIT
  licence still permits any fork.
- Release pages for `v1.0.0` through `v1.9.1` were removed along with their
  archives. Their git tags remain, so any of those versions can still be
  checked out and built from source; `ATELIER_VERSION` pins to a `v1.x` tag no
  longer resolve, and `tools/install.sh` installs the latest release.
