#!/usr/bin/env bash
# Checks scripts/workspace-deps.sh against canned `cargo tree` output from a stub cargo.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
SCRIPT=$ROOT/scripts/workspace-deps.sh
FAILS=0
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# The stub records its arguments and prints $TREE as cargo tree would.
cat >"$TMP/cargo" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >"$STUB_ARGS"
printf '%s' "$TREE"
EOF
chmod +x "$TMP/cargo"

run() { TREE=$1 STUB_ARGS=$TMP/args CARGO=$TMP/cargo bash "$SCRIPT" 2>&1; }

CLEAN=$'dbento-workspace v0.2.0 (/src/crates/dbento-workspace)\nserde v1.0.229\ntoml v1.2.1+spec-1.1.0\ntoml_datetime v1.1.3+spec-1.1.0\ntempfile v3.27.0\n'

test_a_clean_tree_passes_and_asks_for_normal_edges_on_every_target() {
  local out args
  out=$(run "$CLEAN") || fail "clean tree refused: $out"
  args=$(cat "$TMP/args")
  [[ $args == *"tree"*"-p dbento-workspace"*"-e normal"*"--target all"* ]] || fail "args: $args"
}

test_each_forbidden_crate_fails_and_is_named() {
  local c out
  for c in tokio rusqlite tokio-postgres mysql_async tiberius duckdb mongodb redis surrealdb aws-sdk-dynamodb toml_edit; do
    if out=$(run "${CLEAN}$c v9.9.9 (*)"$'\n'); then fail "$c passed"; fi
    [[ $out == *"$c"* ]] || fail "$c not named: $out"
  done
}

test_a_name_that_only_starts_like_a_forbidden_one_passes() {
  local out
  out=$(run "${CLEAN}tokio-util v0.7.20"$'\n'"redis-protocol v1.0.0"$'\n'"duckdb-sys v1.0.0"$'\n') \
    || fail "near-miss refused: $out"
}

test_an_empty_tree_fails() {
  if run "" >/dev/null; then fail "empty tree passed"; fi
}

test_make_check_runs_the_tree_check() {
  local got
  got=$(make -s -n -C "$ROOT" check CARGO=cargo 2>&1) || true
  [[ $got == *"scripts/workspace-deps.sh"* ]] || fail "make check: $got"
}

for t in $(declare -F | awk '{print $3}' | grep '^test_'); do "$t"; done
if ((FAILS > 0)); then echo "$FAILS failure(s)" >&2; exit 1; fi
echo "workspace-deps tests passed"
