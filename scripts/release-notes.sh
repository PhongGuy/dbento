#!/usr/bin/env bash
# Usage: release-notes.sh <tag> [template]
#        release-notes.sh generate <tag>
#        release-notes.sh markdown <tag> <generated-file> [template]
#        release-notes.sh json <tag> <generated-file>
# The old form prints the draft release notes for <tag>: the template with {version} filled in and {changes}
# replaced by GitHub's generated notes since the previous v* tag, grouped by PR label through .github/release.yml.
# `generate` prints only GitHub's body, `markdown` splices a saved body into the template, and `json` prints
# notes.json from that draft plus every published release. Needs gh with a token that can read the repo's contents.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
TEMPLATE=$ROOT/packaging/release-notes.md

generate() {
  local tag=$1 prev
  git rev-parse -q --verify "refs/tags/$tag" >/dev/null || { echo "release-notes: no tag $tag" >&2; exit 1; }
  # The previous release is the nearest v* tag behind this one; the first release has none and GitHub picks the start.
  prev=$(git describe --tags --abbrev=0 --match 'v*' "$tag^" 2>/dev/null || true)
  local args=(-f "tag_name=$tag")
  [[ -z $prev ]] || args+=(-f "previous_tag_name=$prev")
  gh api 'repos/{owner}/{repo}/releases/generate-notes' "${args[@]}" --jq .body
}

# markdown_of <tag> <generated body> <template>: the draft body.
markdown_of() {
  local tag=$1 generated=$2 template=$3 changes
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
}

# json_of <tag> <generated-file>: notes.json, built only once every input is in hand so a failure prints nothing.
json_of() {
  local tag=$1 draft listing out
  draft=$(markdown_of "$tag" "$(<"$2")" "$TEMPLATE")
  listing=$(gh api --paginate --slurp 'repos/{owner}/{repo}/releases')
  out=$(jq -c -L "$ROOT/scripts" --arg tag "$tag" --arg date "${NOTES_DATE:-$(date -u +%F)}" --arg body "$draft" \
    'include "release-notes"; notes($tag; $date; $body)' <<<"$listing")
  printf '%s\n' "$out"
}

case ${1:-} in
  generate) generate "${2:?usage: release-notes.sh generate <tag>}" ;;
  markdown)
    [[ $# -ge 3 ]] || { echo "usage: release-notes.sh markdown <tag> <generated-file> [template]" >&2; exit 2; }
    markdown_of "$2" "$(<"$3")" "${4:-$TEMPLATE}"
    ;;
  json)
    [[ $# -ge 3 ]] || { echo "usage: release-notes.sh json <tag> <generated-file>" >&2; exit 2; }
    json_of "$2" "$3"
    ;;
  *)
    tag=${1:?usage: release-notes.sh <tag> [template]}
    generated=$(generate "$tag")
    markdown_of "$tag" "$generated" "${2:-$TEMPLATE}"
    ;;
esac
