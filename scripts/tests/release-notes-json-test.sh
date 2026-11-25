#!/usr/bin/env bash
# Checks the notes.json that release-notes.sh json writes, from canned generated notes and a canned
# release listing served by a stub gh that logs every call; GitHub is never reached.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
SCRIPT=$ROOT/scripts/release-notes.sh
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
FAILS=0
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }

# The listing call answers $FAKE_GH_RELEASES, generate-notes answers $FAKE_GH_BODY, anything else fails.
mkdir -p "$WORK/bin"
cat >"$WORK/bin/gh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$FAKE_GH_LOG"
if [[ "$*" == "api --paginate --slurp repos/{owner}/{repo}/releases" ]]; then cat "$FAKE_GH_RELEASES"; exit 0; fi
if [[ "$*" == "api repos/{owner}/{repo}/releases/generate-notes "* ]]; then cat "$FAKE_GH_BODY"; exit 0; fi
echo "gh: unexpected call: $*" >&2
exit 1
EOF
chmod +x "$WORK/bin/gh"

# rel <tag> <date> <body> [draft] [prerelease]: one release object as the REST API lists it.
rel() {
  jq -cn --arg t "$1" --arg d "$2" --arg b "$3" --argjson dr "${4:-false}" --argjson pre "${5:-false}" \
    '{tag_name: $t, draft: $dr, prerelease: $pre, published_at: ($d + "T10:00:00Z"), body: $b}'
}
# listing: release objects on stdin become the one-page --slurp answer.
listing() { jq -cs '[.]' >"$WORK/releases"; }
# run_json <tag> <generated>: notes.json on stdout; the exit status is the script's.
run_json() {
  printf '%s' "$2" >"$WORK/generated"
  : >"$WORK/gh-log"
  [[ -f $WORK/releases ]] || printf '' | listing
  PATH=$WORK/bin:$PATH FAKE_GH_LOG=$WORK/gh-log FAKE_GH_RELEASES=$WORK/releases FAKE_GH_BODY=$WORK/generated \
    NOTES_DATE=2026-12-01 bash "$SCRIPT" json "$1" "$WORK/generated"
}

GENERATED=$(cat <<'EOF'
<!-- Release notes generated using configuration in .github/release.yml at master -->

## What's Changed
### New features
* feat(app): show **release notes** in the [banner](https://example.com) by @someone in https://github.com/PhongGuy/dbento/pull/370
* add `snake_case` names by @dependabot[bot] in https://github.com/PhongGuy/dbento/pull/371
### Fixes
* fix: keep `NULL`s in a\_b, see <https://example.com/x> and __this__ by @someone in https://github.com/PhongGuy/dbento/pull/42
### Something new
* odd title by @someone in https://github.com/PhongGuy/dbento/pull/43

## New Contributors
* @someone made their first contribution in https://github.com/PhongGuy/dbento/pull/41

**Full Changelog**: https://github.com/PhongGuy/dbento/compare/v0.2.0...v0.3.0
EOF
)
BODY_020=$(cat <<'EOF'
<!-- intro: left in by mistake -->
Two headline changes in one sentence.

## What's Changed

### New features
* feat(grid): sort columns by @someone in https://github.com/PhongGuy/dbento/pull/200
### Usability and accessibility
* ux: bigger hit targets by @someone in https://github.com/PhongGuy/dbento/pull/201

### New Contributors
* @someone made their first contribution in https://github.com/PhongGuy/dbento/pull/200

**Full Changelog**: https://github.com/PhongGuy/dbento/compare/v0.1.0...v0.2.0

## Downloads

- **macOS:** a bullet outside the window
EOF
)
BODY_010=$'First release.\n\n## Downloads\n\n- a download bullet\n'

two_published() {
  { rel v0.2.0 2026-11-20 "$BODY_020"; rel v0.1.0 2026-11-01 "$BODY_010"; } | listing
}

test_the_newest_entry_comes_from_the_generated_notes() {
  two_published
  local out
  out=$(run_json v0.3.0 "$GENERATED") || { fail "json failed"; return 0; }
  [[ $(jq -r '.schema' <<<"$out") == 1 ]] || fail "schema: $out"
  [[ $(jq -c '.releases[0] | [.version, .previous, .date, has("intro")]' <<<"$out") == '["0.3.0","0.2.0","2026-12-01",false]' ]] \
    || fail "newest head: $(jq -c '.releases[0]' <<<"$out")"
  [[ $(jq -c '[.releases[0].groups[] | [.kind, .title]]' <<<"$out") == '[["features","New features"],["fixes","Fixes"],["other","Something new"]]' ]] \
    || fail "groups: $(jq -c '.releases[0].groups' <<<"$out")"
  [[ $(jq -c '[.releases[0].groups[].items[]]' <<<"$out") == \
    '[{"text":"feat(app): show release notes in the banner","pr":370},{"text":"add snake_case names","pr":371},{"text":"fix: keep NULLs in a_b, see and this","pr":42},{"text":"odd title","pr":43}]' ]] \
    || fail "items: $(jq -c '[.releases[0].groups[].items[]]' <<<"$out")"
  if grep -qE 'Full Changelog|first contribution|@someone' <<<"$out"; then fail "contributors or changelog leaked: $out"; fi
}

test_a_published_body_gives_its_intro_and_only_its_window() {
  two_published
  local out e
  out=$(run_json v0.3.0 "$GENERATED") || { fail "json failed"; return 0; }
  e=$(jq -c '.releases[1]' <<<"$out")
  [[ $(jq -c '[.version, .previous, .date, .intro]' <<<"$e") == '["0.2.0","0.1.0","2026-11-20","Two headline changes in one sentence."]' ]] \
    || fail "v0.2.0 head: $e"
  [[ $(jq -c '[.groups[] | .kind]' <<<"$e") == '["features","usability"]' ]] || fail "v0.2.0 kinds: $e"
  if grep -q 'outside the window' <<<"$e"; then fail "a bullet after Downloads was parsed"; fi
}

test_a_body_without_whats_changed_has_no_groups_and_does_not_fail() {
  two_published
  local out
  out=$(run_json v0.3.0 "$GENERATED") || { fail "json failed on the v0.1.0 shape"; return 0; }
  [[ $(jq -c '.releases[2] | [.version, .previous, .intro, .groups]' <<<"$out") == '["0.1.0",null,"First release.",[]]' ]] \
    || fail "v0.1.0: $(jq -c '.releases[2]' <<<"$out")"
}

test_every_release_yml_title_has_its_kind() {
  local title kind out want
  rm -f "$WORK/releases"
  while IFS= read -r title; do
    case $title in
      'New features') want=features ;; Fixes) want=fixes ;; 'Usability and accessibility') want=usability ;;
      Performance) want=performance ;; 'Other changes') want=other ;;
      *) fail "release.yml title '$title' has no kind in the test table"; continue ;;
    esac
    out=$(run_json v0.3.0 "## What's Changed"$'\n'"### $title"$'\n'"* x by @a in https://github.com/PhongGuy/dbento/pull/1"$'\n') \
      || { fail "json failed for $title"; continue; }
    kind=$(jq -r '.releases[0].groups[0].kind' <<<"$out")
    [[ $kind == "$want" ]] || fail "$title maps to $kind, want $want"
  done < <(sed -n 's/^    - title: //p' "$ROOT/.github/release.yml")
}

test_no_pull_requests_gives_no_groups() {
  rm -f "$WORK/releases"
  local out
  out=$(run_json v0.3.0 $'**Full Changelog**: https://github.com/PhongGuy/dbento/commits/v0.3.0\n') || { fail "json failed"; return 0; }
  [[ $(jq -c '.releases[0].groups' <<<"$out") == '[]' ]] || fail "sentinel parsed: $out"
}

test_bullets_that_parse_to_nothing_fail_the_job() {
  { rel v0.2.0 2026-11-20 $'## What\'s Changed\n### Fixes\n* [](https://example.com/x)\n\n## Downloads\n'; } | listing
  local out status=0
  out=$(run_json v0.3.0 "$GENERATED" 2>/dev/null) || status=$?
  [[ $status != 0 ]] || fail "an empty parse shipped notes"
  [[ -z $out ]] || fail "a failed run printed: $out"
}

test_the_old_form_still_prints_the_draft_body() {
  grep -qE '^ *(generate|markdown|json|files)\)' "$SCRIPT" || fail "release-notes.sh has no subcommands"
  bash "$ROOT/scripts/tests/release-notes-test.sh" >/dev/null 2>&1 || fail "the markdown tests no longer pass"
}

for t in $(declare -F | awk '{print $3}' | grep '^test_'); do "$t"; done
if ((FAILS > 0)); then echo "$FAILS failure(s)" >&2; exit 1; fi
echo "release notes json tests passed"
