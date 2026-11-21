#!/usr/bin/env bash
# Checks the sccache opt-in: silent without sccache, and with it a stable set of exports per
# target dir, so a target never flips between cached and uncached compilers.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
SCRIPT=$ROOT/scripts/sccache-env.sh
FAILS=0
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/fakebin" "$TMP/home"
printf '#!/bin/sh\nexit 0\n' >"$TMP/fakebin/sccache"
chmod +x "$TMP/fakebin/sccache"
BASE_PATH=/usr/bin:/bin

# Runs the script with a controlled PATH and HOME; extra NAME=value pairs go to env.
run_with() { env -i PATH="$1" HOME="$TMP/home" "${@:2}" bash "$SCRIPT"; }
run_present() { run_with "$TMP/fakebin:$BASE_PATH" "$@"; }

# Evaluates the printed exports in a clean subshell and echoes one variable.
value_of() {
  local out=$1 name=$2
  env -i bash -c "eval \"\$1\"; printf '%s' \"\${$2-<unset>}\"" _ "$out"
}

expected_port() { echo $((4300 + $(printf '%s' "$1" | cksum | cut -d' ' -f1) % 1000)); }

test_absent_prints_nothing() {
  if [[ -x /usr/bin/sccache || -x /bin/sccache ]]; then
    fail "sccache is in $BASE_PATH, cannot test the absent case"; return 0
  fi
  local out
  out=$(run_with "$BASE_PATH" CARGO_TARGET_DIR="$TMP/absent") || { fail "absent run exited non-zero"; return 0; }
  [[ -z $out ]] || fail "absent run printed: $out"
}

test_present_exports_wrappers_and_paths() {
  local out t
  out=$(run_present CARGO_TARGET_DIR="$TMP/t1") || { fail "present run exited non-zero"; return 0; }
  [[ -d $TMP/t1 ]] || fail "target dir was not created"
  t=$(cd "$TMP/t1" && pwd -P)
  [[ $(value_of "$out" CC) == "sccache cc" ]] || fail "CC: $(value_of "$out" CC)"
  [[ $(value_of "$out" CXX) == "sccache c++" ]] || fail "CXX: $(value_of "$out" CXX)"
  [[ $(value_of "$out" SCCACHE_DIR) == "$TMP/home/.cache/dbento-sccache" ]] \
    || fail "SCCACHE_DIR: $(value_of "$out" SCCACHE_DIR)"
  [[ $(value_of "$out" SCCACHE_BASEDIRS) == "$t" ]] || fail "BASEDIRS: $(value_of "$out" SCCACHE_BASEDIRS)"
  local port
  port=$(value_of "$out" SCCACHE_SERVER_PORT)
  [[ $port =~ ^[0-9]+$ ]] && ((port >= 4300 && port <= 5299)) || fail "port out of range: $port"
  [[ $port == "$(expected_port "$t")" ]] || fail "port $port is not derived from $t"
  [[ $(grep -c '^export ' <<<"$out") == 5 ]] || fail "expected five export lines: $out"
}

test_same_dir_gives_identical_output() {
  local a b
  a=$(run_present CARGO_TARGET_DIR="$TMP/t2")
  b=$(run_present CARGO_TARGET_DIR="$TMP/t2")
  [[ $a == "$b" ]] || fail "two runs differ: $a / $b"
}

test_relative_and_symlink_paths_agree() {
  mkdir -p "$TMP/real"
  ln -sfn "$TMP/real" "$TMP/link"
  local abs rel link
  abs=$(run_present CARGO_TARGET_DIR="$TMP/real")
  rel=$(cd "$TMP" && env -i PATH="$TMP/fakebin:$BASE_PATH" HOME="$TMP/home" CARGO_TARGET_DIR=real bash "$SCRIPT")
  link=$(run_present CARGO_TARGET_DIR="$TMP/link")
  [[ $rel == "$abs" ]] || fail "relative path differs: $rel / $abs"
  [[ $link == "$abs" ]] || fail "symlinked path differs: $link / $abs"
}

test_path_with_space_survives_eval() {
  local out t
  out=$(run_present CARGO_TARGET_DIR="$TMP/with space")
  t=$(cd "$TMP/with space" && pwd -P)
  [[ $(value_of "$out" SCCACHE_BASEDIRS) == "$t" ]] || fail "spaced BASEDIRS: $(value_of "$out" SCCACHE_BASEDIRS)"
}

test_preset_dir_and_port_are_left_alone() {
  local out
  out=$(run_present CARGO_TARGET_DIR="$TMP/t3" SCCACHE_DIR="$TMP/mine" SCCACHE_SERVER_PORT=4999)
  if grep -q 'SCCACHE_DIR=' <<<"$out"; then fail "overrode SCCACHE_DIR: $out"; fi
  if grep -q 'SCCACHE_SERVER_PORT=' <<<"$out"; then fail "overrode SCCACHE_SERVER_PORT: $out"; fi
  grep -q '^export SCCACHE_BASEDIRS=' <<<"$out" || fail "no BASEDIRS with presets: $out"
}

test_default_target_is_repo_target() {
  local out
  out=$(run_present)
  [[ $(value_of "$out" SCCACHE_BASEDIRS) == "$(cd "$ROOT/target" && pwd -P)" ]] \
    || fail "default BASEDIRS: $(value_of "$out" SCCACHE_BASEDIRS)"
}

for t in $(declare -F | awk '{print $3}' | grep '^test_'); do "$t"; done
if ((FAILS > 0)); then echo "$FAILS failure(s)" >&2; exit 1; fi
echo "sccache-env tests passed"
