#!/usr/bin/env bash
# Usage: release-notes.sh <tag> [template]
#        release-notes.sh generate <tag>
#        release-notes.sh markdown <tag> <generated-file> [template]
#        release-notes.sh json <tag> <generated-file>
#        release-notes.sh files <tag> <outdir>
# The old form prints the draft release notes for <tag>: the template with {version} filled in and {changes}
# replaced by GitHub's generated notes since the previous v* tag, grouped by PR label through .github/release.yml.
# `generate` prints only GitHub's body, `markdown` splices a saved body into the template, and `json` prints
# notes.json (at most 20 releases, capped at $NOTES_CAP bytes) from that draft plus the published releases.
# `files` writes notes.md and notes.json into <outdir> from one generate call. Needs gh with a token that can
# read the repo's contents.
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
  # Fill {version} in the template first, so a PR title that mentions it is printed as written. The changes
  # arrive as a file, since a long release overflows the size limit of one argument or environment variable.
  awk -v version="${tag#v}" '
    FNR == NR { changes = changes (FNR > 1 ? "\n" : "") $0; next }
    $0 == "{changes}" { print changes; next }
    { gsub(/\{version\}/, version); print }' <(printf '%s\n' "$changes") "$template"
}

# jq_notes <filter> [jq args...]: runs a release-notes.jq filter, compact, with the module loaded.
jq_notes() {
  local filter=$1
  shift
  jq -c -L "$ROOT/scripts" "$@" "include \"release-notes\"; $filter"
}

# json_of <tag> <generated-file>: notes.json, built only once every input is in hand so a failure prints nothing.
json_of() {
  local tag=$1 cap=${NOTES_CAP:-49152} draft listing full out keep cut lo hi total
  # Plain assignments, so a failing gh or template step stops the script; the body goes by file, being too
  # long for one argument on a large release.
  draft=$(markdown_of "$tag" "$(<"$2")" "$TEMPLATE")
  listing=$(gh api --paginate --slurp 'repos/{owner}/{repo}/releases')
  full=$(jq_notes 'notes($tag; $date; $body)' --arg tag "$tag" --arg date "${NOTES_DATE:-$(date -u +%F)}" \
    --rawfile body <(printf '%s' "$draft") <<<"$listing")
  # candidate <keep> <cut> prints that cut of the document; size_of is its exact byte count, newline included.
  candidate() { jq_notes 'shrink($keep; $cut)' --argjson keep "$1" --argjson cut "$2" <<<"$full"; }
  size_of() { candidate "$1" "$2" | wc -c; }
  # Drop the oldest entries first; only a newest entry that is too big on its own loses items.
  keep=$(jq '.releases | length' <<<"$full")
  while ((keep > 1)) && (($(size_of "$keep" 0) > cap)); do keep=$((keep - 1)); done
  cut=0
  if (($(size_of "$keep" 0) > cap)); then
    # Binary-search the fewest items to cut, since a large release has too many to try one at a time.
    total=$(jq '[.releases[0].groups[].items[]] | length' <<<"$full")
    lo=1 hi=$total
    while ((lo < hi)); do
      cut=$(((lo + hi) / 2))
      if (($(size_of "$keep" "$cut") > cap)); then lo=$((cut + 1)); else hi=$cut; fi
    done
    cut=$lo
  fi
  out=$(candidate "$keep" "$cut")
  if (($(printf '%s\n' "$out" | wc -c) > cap)); then
    echo "release-notes: the newest release does not fit in $cap bytes" >&2
    exit 1
  fi
  printf '%s\n' "$out"
}

# files_of <tag> <outdir>: the draft body and notes.json from one generate call.
files_of() {
  local tag=$1 dir=$2
  mkdir -p "$dir"
  generate "$tag" >"$dir/generated.md"
  markdown_of "$tag" "$(<"$dir/generated.md")" "$TEMPLATE" >"$dir/notes.md"
  json_of "$tag" "$dir/generated.md" >"$dir/notes.json"
  rm -f "$dir/generated.md"
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
  files)
    [[ $# -ge 3 ]] || { echo "usage: release-notes.sh files <tag> <outdir>" >&2; exit 2; }
    files_of "$2" "$3"
    ;;
  *)
    tag=${1:?usage: release-notes.sh <tag> [template]}
    generated=$(generate "$tag")
    markdown_of "$tag" "$generated" "${2:-$TEMPLATE}"
    ;;
esac
