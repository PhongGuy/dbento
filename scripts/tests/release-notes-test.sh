#!/usr/bin/env bash
# Builds throwaway repos with tags and checks the notes release-notes.sh writes from the template, with a stub
# gh on PATH that records its arguments and answers with a canned generated-notes body.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
SCRIPT=$ROOT/scripts/release-notes.sh
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
FAILS=0
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }

# The stub prints $WORK/body as the API's body and logs one argument per line; FAKE_GH_FAIL makes it fail.
mkdir -p "$WORK/bin"
cat >"$WORK/bin/gh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" >"$FAKE_GH_LOG"
[[ -z ${FAKE_GH_FAIL:-} ]] || { echo "gh: HTTP 403" >&2; exit 1; }
cat "$FAKE_GH_BODY"
EOF
chmod +x "$WORK/bin/gh"

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
# notes <repo> <tag> <generated body>: runs the script with the stub answering <generated body>.
notes() {
  printf '%s' "$3" >"$WORK/body"
  rm -f "$WORK/gh-args"
  (cd "$1" && PATH=$WORK/bin:$PATH FAKE_GH_LOG=$WORK/gh-args FAKE_GH_BODY=$WORK/body bash "$SCRIPT" "$2")
}
whats_changed() { awk '/^## What.s Changed$/ { on = 1; next } /^## / { on = 0 } on' <<<"$1"; }

# What GitHub returns with .github/release.yml in place: a config comment, its own heading, categories and the
# New Contributors section, then the Full Changelog link.
GENERATED=$(cat <<'EOF'
<!-- Release notes generated using configuration in .github/release.yml at master -->

## What's Changed
### New features
* feat(app): add the grid by @someone in https://github.com/PhongGuy/dbento/pull/41
### Fixes
* fix: keep {version} NULLs by @someone in https://github.com/PhongGuy/dbento/pull/42

## New Contributors
* @someone made their first contribution in https://github.com/PhongGuy/dbento/pull/41

**Full Changelog**: https://github.com/PhongGuy/dbento/compare/v0.1.0...v0.2.0
EOF
)

test_sections_come_in_the_shared_order() {
  local d=$WORK/order out prev=0 h n
  new_repo "$d" 'feat: first'
  tag "$d" v0.1.0
  out=$(notes "$d" v0.1.0 "$GENERATED") || { fail "script failed"; return 0; }
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

test_whats_changed_is_the_generated_content() {
  local d=$WORK/generated out want
  new_repo "$d" 'feat: one'
  tag "$d" v0.1.0
  new_repo "$d" 'feat: two'
  tag "$d" v0.2.0
  out=$(notes "$d" v0.2.0 "$GENERATED") || { fail "script failed"; return 0; }
  want=$(cat <<'EOF'

### New features
* feat(app): add the grid by @someone in https://github.com/PhongGuy/dbento/pull/41
### Fixes
* fix: keep {version} NULLs by @someone in https://github.com/PhongGuy/dbento/pull/42

### New Contributors
* @someone made their first contribution in https://github.com/PhongGuy/dbento/pull/41

**Full Changelog**: https://github.com/PhongGuy/dbento/compare/v0.1.0...v0.2.0

EOF
)
  [[ $(whats_changed "$out") == "$want" ]] || fail "What's Changed is not the generated content:"$'\n'"$(whats_changed "$out")"
  if grep -qF 'Release notes generated using configuration' <<<"$out"; then fail "the config comment leaked"; fi
}

test_the_api_compares_with_the_previous_v_tag() {
  local d=$WORK/args out
  new_repo "$d" 'feat: one'
  tag "$d" v0.1.0
  new_repo "$d" 'feat: two'
  tag "$d" not-a-release
  new_repo "$d" 'feat: three'
  tag "$d" v0.2.0
  out=$(notes "$d" v0.2.0 "$GENERATED") || { fail "script failed"; return 0; }
  [[ $(cat "$WORK/gh-args") == "$(printf '%s\n' api 'repos/{owner}/{repo}/releases/generate-notes' \
    -f tag_name=v0.2.0 -f previous_tag_name=v0.1.0 --jq .body)" ]] || fail "gh called with:"$'\n'"$(cat "$WORK/gh-args")"
}

test_the_first_tag_lets_github_pick_the_start() {
  local d=$WORK/first out
  new_repo "$d" 'feat: the start'
  tag "$d" v0.1.0
  out=$(notes "$d" v0.1.0 "$GENERATED") || { fail "script failed"; return 0; }
  if grep -q previous_tag_name "$WORK/gh-args"; then fail "the first release names a previous tag"; fi
  grep -qxF 'tag_name=v0.1.0' "$WORK/gh-args" || fail "the first release does not name its tag"
}

test_no_pull_requests_says_so() {
  local d=$WORK/empty out want
  new_repo "$d" 'feat: straight to master'
  tag "$d" v0.1.0
  out=$(notes "$d" v0.1.0 $'**Full Changelog**: https://github.com/PhongGuy/dbento/commits/v0.1.0\n') \
    || { fail "script failed"; return 0; }
  want=$'\n- No pull requests were merged for this release.\n\n**Full Changelog**: https://github.com/PhongGuy/dbento/commits/v0.1.0'
  [[ $(whats_changed "$out") == "$want" ]] || fail "empty What's Changed:"$'\n'"$(whats_changed "$out")"
}

test_version_is_substituted_everywhere() {
  local d=$WORK/version out
  new_repo "$d" 'feat: one'
  tag "$d" v1.2.3
  out=$(notes "$d" v1.2.3 "$GENERATED") || { fail "script failed"; return 0; }
  if grep -n '{version}\|{changes}' <<<"$out" | grep -vF -- '* fix: keep {version} NULLs'; then
    fail "a placeholder survived"
  fi
  grep -qF -- '* fix: keep {version} NULLs' <<<"$out" || fail "a PR title mentioning {version} was rewritten"
  grep -qF 'dbento-app_1.2.3_amd64.deb' <<<"$out" || fail "the downloads do not name 1.2.3"
  grep -qF 'Check a download with `sha256sum -c SHA256SUMS`.' <<<"$out" || fail "no checksum line"
  grep -qF 'xattr -dr com.apple.quarantine /Applications/Dbento.app' <<<"$out" || fail "no macOS quarantine step"
  grep -qF '"More info", then "Run anyway"' <<<"$out" || fail "no Windows SmartScreen step"
}

test_a_missing_tag_fails() {
  local d=$WORK/missing
  new_repo "$d" 'feat: x'
  if notes "$d" v9.9.9 "$GENERATED" >/dev/null 2>&1; then fail "an unknown tag produced notes"; fi
}

test_a_failed_api_call_fails() {
  local d=$WORK/apifail out
  new_repo "$d" 'feat: x'
  tag "$d" v0.1.0
  if out=$(FAKE_GH_FAIL=1 notes "$d" v0.1.0 "$GENERATED" 2>/dev/null); then fail "a failed gh call produced notes"; fi
  [[ -z $out ]] || fail "a failed gh call still printed notes"
}

for t in $(declare -F | awk '{print $3}' | grep '^test_'); do "$t"; done
if ((FAILS > 0)); then echo "$FAILS failure(s)" >&2; exit 1; fi
echo "release notes tests passed"
