#!/usr/bin/env bash
# Builds throwaway repos with tags and mixed commits and checks the notes release-notes.sh writes from the template.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
SCRIPT=$ROOT/scripts/release-notes.sh
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
FAILS=0
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }

# A repo whose commits are the given subjects, one empty commit each. Identity and signing are pinned, and the
# scratch repo ignores the machine's global hooks, which would judge its fake identity.
new_repo() {
  local dir=$1 s
  shift
  [[ -d $dir/.git ]] || { git init -q "$dir"; git -C "$dir" config core.hooksPath /dev/null; }
  for s in "$@"; do
    git -C "$dir" -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q --allow-empty -m "$s"
  done
}
tag() { git -C "$1" -c tag.gpgsign=false tag "$2"; }
notes() { (cd "$1" && bash "$SCRIPT" "$2"); }

test_sections_come_in_the_shared_order() {
  local d=$WORK/order out prev=0 h n
  new_repo "$d" 'feat: first'
  tag "$d" v0.1.0
  out=$(notes "$d" v0.1.0) || { fail "script failed"; return 0; }
  n=$(grep -nF '<!-- intro:' <<<"$out" | head -1 | cut -d: -f1)
  [[ $n == 1 ]] || fail "the intro placeholder is not the first line (line ${n:-none})"
  for h in "## What's Changed" '## Downloads' '## Before you install' '## Updates'; do
    n=$(grep -nxF "$h" <<<"$out" | cut -d: -f1)
    [[ -n $n && $n -gt $prev ]] || { fail "$h is missing or out of order"; continue; }
    prev=$n
  done
  [[ $(grep -c '^## ' <<<"$out") == 4 ]] || fail "want exactly four sections: $(grep '^## ' <<<"$out")"
  if grep -q '^# ' <<<"$out"; then fail "the notes carry a title heading"; fi
}

test_only_feat_and_fix_since_the_previous_tag_are_listed() {
  local d=$WORK/mixed out list
  new_repo "$d" 'feat: before the last tag'
  tag "$d" v0.1.0
  new_repo "$d" 'feat(app): add the grid' 'fix: keep NULLs' 'test: cover the grid' 'ci: pin actions' \
    'chore(release): bump to 0.2.0' 'docs: note it' 'refactor(core): split' 'feat!: drop the old format' \
    'fix(cli)!: rename a flag' 'feature: not a type' 'fixup: not a type either'
  git -C "$d" checkout -q -b side
  git -C "$d" -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q --allow-empty -m 'fix: on a side branch'
  git -C "$d" checkout -q -
  git -C "$d" -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false merge -q --no-ff --no-edit -m "feat: merge branch 'side'" side
  tag "$d" v0.2.0
  out=$(notes "$d" v0.2.0) || { fail "script failed"; return 0; }
  list=$(awk '/^## What.s Changed$/ { on = 1; next } /^## / { on = 0 } on && /^- /' <<<"$out")
  [[ $list == "$(printf '%s\n' '- add the grid' '- keep NULLs' '- drop the old format' '- rename a flag' '- on a side branch')" ]] \
    || fail "What's Changed lists the wrong commits:"$'\n'"$list"
  grep -qxF '**Full Changelog**: https://github.com/PhongGuy/dbento/compare/v0.1.0...v0.2.0' <<<"$out" \
    || fail "Full Changelog does not compare v0.1.0...v0.2.0"
}

test_the_first_tag_lists_from_the_root() {
  local d=$WORK/first out
  new_repo "$d" 'feat: the start' 'chore: tidy' 'fix: a bug'
  tag "$d" v0.1.0
  out=$(notes "$d" v0.1.0) || { fail "script failed"; return 0; }
  grep -qxF -- '- the start' <<<"$out" || fail "the root commit is not listed"
  grep -qxF -- '- a bug' <<<"$out" || fail "the fix is not listed"
  grep -qxF '**Full Changelog**: https://github.com/PhongGuy/dbento/commits/v0.1.0' <<<"$out" \
    || fail "the first release does not link commits/v0.1.0"
}

test_version_is_substituted_everywhere() {
  local d=$WORK/version out
  new_repo "$d" 'feat: keep {version} in a subject'
  tag "$d" v1.2.3
  out=$(notes "$d" v1.2.3) || { fail "script failed"; return 0; }
  if grep -n '{version}\|{changes}' <<<"$out" | grep -vF -- '- keep {version} in a subject'; then
    fail "a placeholder survived"
  fi
  grep -qF 'dbento-app_1.2.3_amd64.deb' <<<"$out" || fail "the downloads do not name 1.2.3"
  grep -qF 'Check a download with `sha256sum -c SHA256SUMS`.' <<<"$out" || fail "no checksum line"
  grep -qF 'xattr -dr com.apple.quarantine /Applications/Dbento.app' <<<"$out" || fail "no macOS quarantine step"
  grep -qF '"More info", then "Run anyway"' <<<"$out" || fail "no Windows SmartScreen step"
}

test_a_missing_tag_fails() {
  local d=$WORK/missing
  new_repo "$d" 'feat: x'
  if notes "$d" v9.9.9 >/dev/null 2>&1; then fail "an unknown tag produced notes"; fi
}

for t in $(declare -F | awk '{print $3}' | grep '^test_'); do "$t"; done
if ((FAILS > 0)); then echo "$FAILS failure(s)" >&2; exit 1; fi
echo "release notes tests passed"
