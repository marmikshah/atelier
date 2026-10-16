#!/usr/bin/env bash
# Read-only check that a release tag has no Git tag and no GitHub Release yet.
set -euo pipefail
if [[ ${1:-} == --help ]]; then
  echo "Usage: tools/release-vacancy.sh OWNER/REPO vX.Y.Z"
  echo "Fail when the tag or a GitHub Release for it already exists."
  echo "Reads through gh with its existing authentication; changes nothing."
  exit 0
fi
if [[ $# -ne 2 ]]; then
  echo "Usage: tools/release-vacancy.sh OWNER/REPO vX.Y.Z | --help" >&2
  exit 2
fi
repo=$1
tag=$2
[[ $repo =~ ^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$ ]] || {
  echo "release vacancy: expected OWNER/REPO, got '$repo'" >&2
  exit 2
}
[[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "release vacancy: expected a vX.Y.Z tag, got '$tag'" >&2
  exit 2
}
# The GraphQL variables are for the API, not the shell.
# shellcheck disable=SC2016
response=$(gh api graphql \
  -f owner="${repo%%/*}" -f name="${repo#*/}" \
  -f qualified="refs/tags/$tag" -f tag="$tag" \
  -f query='query($owner:String!,$name:String!,$qualified:String!,$tag:String!){
    repository(owner:$owner,name:$name){
      ref(qualifiedName:$qualified){id}
      release(tagName:$tag){id}
    }
  }') || {
  echo "release vacancy: could not query $repo for $tag" >&2
  exit 1
}
occupants=$(jq -er '
  if ((.errors // []) | length) > 0 or (.data.repository | type) != "object"
  then error("incomplete response")
  else [.data.repository.ref, .data.repository.release]
    | map(select(. != null)) | length
  end' <<<"$response") || {
  echo "release vacancy: unreadable response for $repo $tag" >&2
  exit 1
}
if [[ $occupants != 0 ]]; then
  echo "release vacancy: $repo already has a tag or release named $tag" >&2
  exit 1
fi
echo "release vacancy: $repo $tag is free"
