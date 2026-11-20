#!/usr/bin/env bash
# Checks scripts/host-features.sh against canned `cargo tree` output from a stub cargo.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
SCRIPT=$ROOT/scripts/host-features.sh
FAILS=0
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# The stub logs its arguments and prints $WS_TREE for --workspace, $NARROW_TREE otherwise. Like
# cargo under CARGO_TERM_COLOR=always (CI sets it), it colors the tree unless told `--color never`.
cat >"$TMP/cargo" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$STUB_ARGS"
if [[ " $* " == *" --workspace "* ]]; then out=$WS_TREE; else out=$NARROW_TREE; fi
if [[ " $* " != *" --color never "* ]]; then out=$(sed $'s/|/\e[2m|\e[0m/g' <<<"$out"); fi
printf '%s' "$out"
EOF
chmod +x "$TMP/cargo"

run() {
  : >"$TMP/args"
  WS_TREE=$1 NARROW_TREE=$2 STUB_ARGS=$TMP/args CARGO=$TMP/cargo HOST=x86_64-unknown-linux-gnu \
    bash "$SCRIPT" 2>&1
}

# A tree in cargo's ascii charset: each crate's build dependencies, then a normal dependency of
# libduckdb-sys after them, then a sibling that must not count as part of the subtree.
tree() {
  local serde_json=$1 regex=$2
  cat <<EOF
dbento-core v0.2.0 (/src/crates/dbento-core) [default,duckdb]
|-- duckdb v1.20000.1 [bundled]
|   |-- libduckdb-sys v1.20000.1 [bundled,capi-v1,cc]
|   |   [build-dependencies]
|   |   |-- cc v1.2.67 [parallel]
|   |   \`-- serde_json v1.0.154 [$serde_json]
|   |       \`-- memchr v2.8.3 [alloc,std]
|   \`-- outside v1.0.0 [x]
|-- pg_query v18.1.0 []
|   |-- prost v0.13.5 [derive]
|   [build-dependencies]
|   \`-- prost-build v0.13.5 [default]
|       \`-- regex v1.13.1 [$regex]
|           \`-- syn v2.0.119 [full] (proc-macro)
\`-- serde_json v1.0.154 [default,other]
EOF
}
SAME=$(tree default,std default)

test_matching_trees_pass_for_both_crates_with_build_edges_on_the_host() {
  local out line
  out=$(run "$SAME" "$SAME") || fail "matching trees refused: $out"
  [[ $out == *"libduckdb-sys build script is shared"* ]] || fail "duckdb not reported: $out"
  [[ $out == *"pg_query build script is shared"* ]] || fail "pg_query not reported: $out"
  line=$(grep -m1 -- '-p dbento-core' "$TMP/args") || fail "no -p dbento-core tree: $(cat "$TMP/args")"
  [[ $line == *"-e normal,build"*"--no-dedupe"*"--charset ascii"*"--color never"*"--target x86_64-unknown-linux-gnu"* ]] \
    || fail "args: $line"
}

test_a_host_feature_missing_from_the_narrow_selection_fails_naming_crate_and_diff() {
  local out
  if out=$(run "$(tree default,std,preserve_order default)" "$SAME"); then fail "diff passed"; fi
  [[ $out == *"libduckdb-sys build dependencies differ under '-p dbento-core'"* ]] || fail "not named: $out"
  [[ $out == *"> serde_json v1.0.154 [default,std,preserve_order]"* ]] || fail "no diff: $out"
}

test_a_difference_outside_the_build_dependencies_is_ignored() {
  local narrow
  narrow=$(tree default,std default | sed 's/prost v0.13.5 \[derive\]/prost v0.13.5 [std]/; s/outside v1.0.0 \[x\]/outside v1.0.0 [y]/; s/\[default,other\]/[default]/')
  run "$SAME" "$narrow" >/dev/null || fail "normal-dependency difference refused"
}

test_a_pg_query_build_dependency_difference_fails() {
  local out
  if out=$(run "$(tree default,std default,perf)" "$SAME"); then fail "pg_query diff passed"; fi
  [[ $out == *"pg_query build dependencies differ"* ]] || fail "not named: $out"
}

test_a_selection_without_the_crate_is_skipped() {
  run "$SAME" "dbento-core v0.2.0 (/src/crates/dbento-core) []"$'\n' >/dev/null || fail "absent crate refused"
}

test_an_empty_workspace_tree_fails() {
  local out
  if out=$(run "" "$SAME"); then fail "empty workspace tree passed"; fi
  [[ $out == *"showed no build dependencies for libduckdb-sys"* ]] || fail "not named: $out"
}

test_make_check_runs_the_host_feature_check() {
  local got
  got=$(make -s -n -C "$ROOT" check CARGO=cargo 2>&1) || true
  [[ $got == *"scripts/host-features.sh"* ]] || fail "make check: $got"
}

for t in $(declare -F | awk '{print $3}' | grep '^test_'); do "$t"; done
if ((FAILS > 0)); then echo "$FAILS failure(s)" >&2; exit 1; fi
echo "host-features tests passed"
