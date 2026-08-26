# Releasing atelier

Only the maintainer creates version tags. An agent may prepare and review the
version-change pull request, but pushing the tag is the human approval that
starts publication.

`1.0.0` must never be prepared, tagged, or described as planned without
explicit maintainer instruction.

Only one release exists at a time. Publishing a new one removes the release
before it completely — its page, its archives, its tag, and its container
images — because releases break and maintaining several in parallel is out of
scope for now. Nothing of a superseded release is kept, so ship migration steps
in the release that requires them.

The release workflow accepts only a `vMAJOR.MINOR.PATCH` tag such as
`v0.1.0`, with no pre-release or build suffix. It must
be annotated or signed, match every release package and a dated changelog
heading, and point to a commit on `master`. A version-preparation PR keeps its
changelog heading marked `Unreleased`; dating it is a separate, explicit part
of the maintainer's release approval.

## 1. Prepare and merge the version pull request

The pull request must:

- set `workspace.package.version` and the three internal dependency requirements
  in `Cargo.toml`;
- add the `CHANGELOG.md` section as `## [VERSION] — Unreleased`;
- refresh and commit `Cargo.lock`;
- pass CI and generated-doc checks.

After changing versions, refresh the lockfile deliberately, then validate it:

```sh
cargo check --locked
tools/release-check.sh --current
make check
```

This validates the version already declared by the workspace while allowing its
changelog section to remain unreleased. Merge the pull request before creating
the tag.

## 2. Create the tag by hand

As the final release change, replace `Unreleased` in the version heading with
the actual release date (`YYYY-MM-DD`), commit it, and get that commit onto
`master`. An explicit version check deliberately fails while the heading still
says `Unreleased`.

Then start from a clean, current `master`:

```sh
git checkout master
git pull --ff-only origin master
git status --short
ATELIER_RELEASE_TAG=v0.1.0
tools/release-check.sh "$ATELIER_RELEASE_TAG"
```

`git status --short` must print nothing. Run the final local gate:

```sh
make check
```

Create a signed annotated tag when signing is configured:

```sh
git tag -s "$ATELIER_RELEASE_TAG" \
  -m "atelier $ATELIER_RELEASE_TAG"
git show --show-signature "$ATELIER_RELEASE_TAG"
```

If Git tag signing is not configured, create an ordinary annotated tag instead:

```sh
git tag -a "$ATELIER_RELEASE_TAG" \
  -m "atelier $ATELIER_RELEASE_TAG"
git show "$ATELIER_RELEASE_TAG"
```

Inspect the tagged commit carefully. Then push that one tag—never use
`git push --tags` for a release:

```sh
git push origin "refs/tags/$ATELIER_RELEASE_TAG"
```

That tag push is the normal publication trigger.

## 3. Watch publication

The workflow validates the tag and reruns the production gate before building.
It creates the GitHub Release only after both Ubuntu 22.04+ archives (x86_64
and aarch64), their SHA-256 sidecars, and the smoke-tested Alpine linux/amd64
image have succeeded. Each native archive is built on Ubuntu 22.04, natively on
a runner of its own architecture, so its glibc baseline is deliberate; it
contains the executable, `README.md`, and `LICENSE` and is smoke-tested only
after extraction.

```sh
gh run list --workflow Release --limit 3
ATELIER_RELEASE_RUN_ID="$(gh run list --workflow Release --limit 1 --json databaseId --jq '.[0].databaseId')"
gh run watch "$ATELIER_RELEASE_RUN_ID" --exit-status
gh release view "$ATELIER_RELEASE_TAG"
```

The installer requires a checksum beside every archive. The release should
contain `atelier-$ATELIER_RELEASE_TAG-ubuntu-x86_64.tar.gz` and
`atelier-$ATELIER_RELEASE_TAG-ubuntu-aarch64.tar.gz`, each with its matching
`.sha256` file, plus the GHCR image tags. Docker still publishes linux/amd64
only.

## 4. Remove the previous release

Once the new release is published and verified, delete the one it replaces —
its page, its archives, and its tag:

```sh
gh release delete "$ATELIER_PREVIOUS_TAG" --cleanup-tag --yes
git ls-remote --tags origin "$ATELIER_PREVIOUS_TAG"   # must print nothing
```

Then prune that version's container images, which `gh release delete` does not
touch:

```sh
gh api --paginate "user/packages/container/atelier/versions?per_page=100" \
  --jq ".[] | select(.metadata.container.tags | index(\"$ATELIER_PREVIOUS_TAG\")) | .id"
# then DELETE each id under user/packages/container/atelier/versions/<id>
```

Never prune the version holding `latest` before the new release carries it.

Do this after verification, not before: until the new archives are confirmed
downloadable, the previous release is the only installable version, and
`tools/install.sh` resolves whatever `/releases/latest` returns.

## 5. Retry an orchestration failure

Treat a pushed tag as immutable. A plain GitHub rerun uses the workflow from the
tagged commit, so it cannot pick up a workflow-only repair. After merging such a
repair to `master`, retry the exact immutable tag through the current workflow:

```sh
gh workflow run Release --ref master \
  -f release_tag="$ATELIER_RELEASE_TAG"
```

Watch and verify that run exactly as above. It checks out and builds the tagged
source; only the orchestration comes from current `master`. If code or release
metadata must change, prepare a new patch version instead of moving the tag.
