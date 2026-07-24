# Releasing atelier

Only the maintainer creates version tags. An agent may prepare and review the
version-change pull request, but pushing the tag is the human approval that
starts publication.

The release workflow accepts only a stable SemVer tag such as `v1.8.0`. It must
be annotated or signed, match every release package and the dated changelog
heading, and point to a commit on `master`.

## 1. Prepare and merge the version pull request

The pull request must:

- set `workspace.package.version` and the three internal dependency requirements
  in `Cargo.toml`;
- add the dated `CHANGELOG.md` section;
- refresh and commit `Cargo.lock`;
- pass CI and generated-doc checks.

After changing versions, refresh the lockfile deliberately, then validate it:

```sh
cargo check --locked
tools/release-check.sh v1.8.0
make check
```

Replace `v1.8.0` with the version being prepared. Merge the pull request before
creating the tag.

## 2. Create the tag by hand

Start from a clean, current `master`:

```sh
git checkout master
git pull --ff-only origin master
git status --short
ATELIER_RELEASE_TAG=v1.8.0
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

That push is the only publication trigger.

## 3. Watch publication

The workflow validates the tag and reruns the production gate before building.
It creates the GitHub Release only after all three platform archives, their
SHA-256 sidecars, and the multi-architecture container have succeeded.

```sh
gh run list --workflow Release --limit 3
ATELIER_RELEASE_RUN_ID="$(gh run list --workflow Release --limit 1 --json databaseId --jq '.[0].databaseId')"
gh run watch "$ATELIER_RELEASE_RUN_ID" --exit-status
gh release view "$ATELIER_RELEASE_TAG"
```

The installer requires checksums for v1.8.0 and later. The release should contain
three archives, three matching `.sha256` files, and the GHCR image tags.

Treat a pushed tag as immutable. Rerun a workflow when only infrastructure
failed; if code or release metadata must change, prepare a new patch version
instead of moving a published tag.
