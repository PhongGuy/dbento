#!/usr/bin/env bash
# Usage: release-notes.sh <tag> [template]
# Prints the draft release notes for <tag>: the template with {version} filled in and {changes} replaced by
# GitHub's generated notes since the previous v* tag, grouped by PR label through .github/release.yml and ending in
# a Full Changelog link. Runs in the repo's directory and needs gh with a token that can read the repo's contents.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
tag=${1:?usage: release-notes.sh <tag> [template]}
template=${2:-$ROOT/packaging/release-notes.md}

git rev-parse -q --verify "refs/tags/$tag" >/dev/null || { echo "release-notes: no tag $tag" >&2; exit 1; }

# The previous release is the nearest v* tag behind this one; the first release has none and GitHub picks the start.
prev=$(git describe --tags --abbrev=0 --match 'v*' "$tag^" 2>/dev/null || true)
args=(-f "tag_name=$tag")
[[ -z $prev ]] || args+=(-f "previous_tag_name=$prev")
generated=$(gh api 'repos/{owner}/{repo}/releases/generate-notes' "${args[@]}" --jq .body)

# The template owns the What's Changed heading, so GitHub's own heading and config comment go, and its other
# sections (New Contributors) drop a level to stay inside What's Changed. Blank edges are trimmed.
changes=$(awk '
  /^<!-- Release notes generated/ { next }
  /^## What.s Changed$/ && !seen { seen = 1; next }
  { sub(/^## /, "### "); lines[++n] = $0 }
  END {
    for (first = 1; first <= n && lines[first] == ""; first++) {}
    for (last = n; last >= first && lines[last] == ""; last--) {}
    for (i = first; i <= last; i++) print lines[i]
  }' <<<"$generated")
if ! grep -qvE '^(\*\*Full Changelog\*\*:.*)?$' <<<"$changes"; then
  changes="- No pull requests were merged for this release."$'\n\n'$changes
fi

# Fill {version} in the template first, so a PR title that mentions it is printed as written.
CHANGES=$changes awk -v version="${tag#v}" '
  $0 == "{changes}" { print ENVIRON["CHANGES"]; next }
  { gsub(/\{version\}/, version); print }' "$template"
