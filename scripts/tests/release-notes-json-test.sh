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

# many <from> <to> <items> <width>: published releases v0.0.<from>..v0.0.<to>, each with <items> items of <width> chars.
many() {
  local i n body
  for ((i = $1; i <= $2; i++)); do
    body="## What's Changed"$'\n'"### Fixes"$'\n'
    for ((n = 1; n <= $3; n++)); do body+="* $(printf "r$i-n$n-%0$4d" 0) by @a in https://github.com/PhongGuy/dbento/pull/$n"$'\n'; done
    body+=$'\n## Downloads\n'
    rel "v0.0.$i" "2026-01-$(printf %02d $(((i % 28) + 1)))" "$body"
  done
}
generated_items() { # <count> <width>
  local n out="## What's Changed"$'\n'"### New features"$'\n'
  for ((n = 1; n <= $1; n++)); do out+="* $(printf "new$n-%0$2d" 0) by @a in https://github.com/PhongGuy/dbento/pull/$n"$'\n'; done
  printf '%s' "$out"
}

test_only_published_stable_older_releases_are_listed_once() {
  { rel v0.2.0 2026-11-20 "$BODY_020"; rel v0.1.0 2026-11-01 "$BODY_010"
    rel v0.2.5 2026-11-21 "$BODY_020" true; rel v0.2.6-rc.1 2026-11-22 "$BODY_020" false true
    rel nightly 2026-11-23 "$BODY_020"; rel v0.4.0 2026-11-24 "$BODY_020"; rel v0.3.0 2026-11-25 "$BODY_020" true; } | listing
  local out
  out=$(run_json v0.3.0 "$GENERATED") || { fail "json failed"; return 0; }
  [[ $(jq -c '[.releases[].version]' <<<"$out") == '["0.3.0","0.2.0","0.1.0"]' ]] || fail "versions: $(jq -c '[.releases[].version]' <<<"$out")"
  [[ $(cat "$WORK/gh-log") == "api --paginate --slurp repos/{owner}/{repo}/releases" ]] || fail "gh calls: $(cat "$WORK/gh-log")"
}

test_versions_sort_by_semver_not_by_listing_order() {
  { rel v0.9.0 2026-10-01 "$BODY_010"; rel v0.10.0 2026-10-02 "$BODY_010"; rel v0.2.0 2026-09-01 "$BODY_010"; } | listing
  local out
  out=$(run_json v0.11.0 "$GENERATED") || { fail "json failed"; return 0; }
  [[ $(jq -c '[.releases[] | [.version, .previous]]' <<<"$out") == '[["0.11.0","0.10.0"],["0.10.0","0.9.0"],["0.9.0","0.2.0"],["0.2.0",null]]' ]] \
    || fail "order: $(jq -c '[.releases[] | [.version, .previous]]' <<<"$out")"
}

test_at_most_twenty_versions_and_previous_reaches_past_the_list() {
  many 1 25 1 10 | listing
  local out
  out=$(run_json v0.1.0 "$GENERATED") || { fail "json failed"; return 0; }
  [[ $(jq '.releases | length' <<<"$out") == 20 ]] || fail "count: $(jq '.releases | length' <<<"$out")"
  [[ $(jq -c '.releases[-1] | [.version, .previous]' <<<"$out") == '["0.0.7","0.0.6"]' ]] || fail "oldest: $(jq -c '.releases[-1]' <<<"$out")"
}

test_oversize_history_drops_the_oldest_first() {
  many 1 19 40 100 | listing
  run_json v0.1.0 "$GENERATED" >"$WORK/out.json" || { fail "json failed"; return 0; }
  local size n last
  size=$(wc -c <"$WORK/out.json")
  ((size <= 49152)) || fail "notes.json is $size bytes"
  n=$(jq '.releases | length' "$WORK/out.json")
  ((n > 1 && n < 20)) || fail "kept $n releases"
  [[ $(jq -r '.releases[0].version' "$WORK/out.json") == 0.1.0 ]] || fail "the newest was dropped"
  [[ $(jq '[.releases[0].groups[].items[]] | length' "$WORK/out.json") == 4 ]] || fail "the newest lost items"
  last=$(jq -r '.releases[-1].version' "$WORK/out.json")
  [[ $(jq -r '.releases[-1].previous' "$WORK/out.json") == "0.0.$((${last##*.} - 1))" ]] || fail "previous rewritten on $last"
}

test_an_oversize_newest_release_is_cut_with_a_count() {
  rm -f "$WORK/releases"
  run_json v0.1.0 "$(generated_items 1000 80)" >"$WORK/out.json" || { fail "json failed"; return 0; }
  local size last kept more
  size=$(wc -c <"$WORK/out.json")
  ((size <= 49152)) || fail "notes.json is $size bytes"
  last=$(jq -c '.releases[0].groups[-1].items[-1]' "$WORK/out.json")
  [[ $(jq -r .text <<<"$last") =~ ^\.\.\.and\ ([0-9]+)\ more$ ]] || { fail "last item: $last"; return 0; }
  more=${BASH_REMATCH[1]}
  [[ $(jq 'has("pr")' <<<"$last") == false ]] || fail "the count item has a pr"
  kept=$(jq '[.releases[0].groups[].items[]] | length - 1' "$WORK/out.json")
  ((kept + more == 1000)) || fail "kept $kept plus $more is not 1000"
}

test_a_newest_release_that_cannot_fit_fails() {
  rm -f "$WORK/releases"
  local out status=0
  out=$(NOTES_CAP=100 run_json v0.1.0 "$GENERATED" 2>/dev/null) || status=$?
  [[ $status != 0 ]] || fail "an unfittable release shipped"
  [[ -z $out ]] || fail "printed: $out"
}

test_files_writes_the_body_and_the_json_from_one_generate_call() {
  local d=$WORK/repo out=$WORK/notes-out
  git init -q "$d"; git -C "$d" config core.hooksPath /dev/null
  git -C "$d" -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q --allow-empty -m one
  git -C "$d" -c tag.gpgsign=false tag v0.3.0
  two_published
  printf '%s' "$GENERATED" >"$WORK/generated"
  : >"$WORK/gh-log"
  (cd "$d" && PATH=$WORK/bin:$PATH FAKE_GH_LOG=$WORK/gh-log FAKE_GH_RELEASES=$WORK/releases FAKE_GH_BODY=$WORK/generated \
    NOTES_DATE=2026-12-01 bash "$SCRIPT" files v0.3.0 "$out") || { fail "files failed"; return 0; }
  [[ $(command ls "$out" </dev/null | tr '\n' ' ') == "notes.json notes.md " ]] || fail "files wrote: $(command ls "$out" </dev/null)"
  grep -qxF "## What's Changed" "$out/notes.md" || fail "notes.md is not the draft body"
  [[ $(jq -r '.releases[0].version' "$out/notes.json") == 0.3.0 ]] || fail "notes.json: $(cat "$out/notes.json")"
  [[ $(grep -c generate-notes "$WORK/gh-log") == 1 ]] || fail "generate-notes called $(grep -c generate-notes "$WORK/gh-log") times"
}

test_a_window_with_only_new_contributors_has_no_groups_and_does_not_fail() {
  { rel v0.2.0 2026-11-20 $'## What\'s Changed\n### New Contributors\n* @someone made their first contribution in https://github.com/PhongGuy/dbento/pull/9\n\n## Downloads\n'; } | listing
  local out
  out=$(run_json v0.3.0 "$GENERATED") || { fail "json failed on a New Contributors-only window"; return 0; }
  [[ $(jq -c '.releases[1] | [.version, .groups]' <<<"$out") == '["0.2.0",[]]' ]] || fail "v0.2.0: $(jq -c '.releases[1]' <<<"$out")"
}

test_bullets_before_any_section_are_one_changes_group_without_a_kind() {
  rm -f "$WORK/releases"
  local out
  out=$(run_json v0.3.0 "## What's Changed"$'\n'"* loose one by @a in https://github.com/PhongGuy/dbento/pull/1"$'\n'"* loose two"$'\n'"### Fixes"$'\n'"* fixed by @a in https://github.com/PhongGuy/dbento/pull/3"$'\n') \
    || { fail "json failed"; return 0; }
  [[ $(jq -c '.releases[0].groups[0]' <<<"$out") == '{"title":"Changes","items":[{"text":"loose one","pr":1},{"text":"loose two"}]}' ]] \
    || fail "unsectioned group: $(jq -c '.releases[0].groups[0]' <<<"$out")"
  [[ $(jq -c '.releases[0].groups[1] | [.kind, .title]' <<<"$out") == '["fixes","Fixes"]' ]] || fail "sectioned group: $(jq -c '.releases[0].groups[1]' <<<"$out")"
}

for t in $(declare -F | awk '{print $3}' | grep '^test_'); do "$t"; done
if ((FAILS > 0)); then echo "$FAILS failure(s)" >&2; exit 1; fi
echo "release notes json tests passed"
