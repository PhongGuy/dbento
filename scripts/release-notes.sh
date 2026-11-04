#!/usr/bin/env bash
# Usage: release-notes.sh <tag> [template]
# Prints the draft release notes for <tag>: the template with {version} filled in and {changes} replaced by the
# feat and fix commits since the previous v* tag, ending in a Full Changelog link. Runs in the repo's directory.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
REPO_URL=https://github.com/PhongGuy/dbento
tag=${1:?usage: release-notes.sh <tag> [template]}
template=${2:-$ROOT/packaging/release-notes.md}

git rev-parse -q --verify "refs/tags/$tag" >/dev/null || { echo "release-notes: no tag $tag" >&2; exit 1; }

# The previous release is the nearest v* tag behind this one; the first release has none and lists from the root.
prev=$(git describe --tags --abbrev=0 --match 'v*' "$tag^" 2>/dev/null || true)
if [[ -n $prev ]]; then
  range=$prev..$tag
  link="$REPO_URL/compare/$prev...$tag"
else
  range=$tag
  link="$REPO_URL/commits/$tag"
fi

# Only user-facing Conventional Commits types make the list; merges and housekeeping types stay out.
changes=$(git log --no-merges --topo-order --reverse --format=%s "$range" |
  sed -nE 's/^(feat|fix)(\([^)]*\))?!?: +/- /p')
[[ -n $changes ]] || changes='- No feature or fix commits.'
changes+=$'\n\n'"**Full Changelog**: $link"

# Fill {version} in the template first, so a commit subject that mentions it is printed as written.
CHANGES=$changes awk -v version="${tag#v}" '
  $0 == "{changes}" { print ENVIRON["CHANGES"]; next }
  { gsub(/\{version\}/, version); print }' "$template"
