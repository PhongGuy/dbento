#!/usr/bin/env bash
# Checks on the Makefile recipes that change with the environment, read from make -n.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
FAILS=0
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }

# The recipe make would run for windows-check under the given environment assignments.
windows_check() { env -u CI "$@" make -s -n -C "$ROOT" windows-check CARGO=cargo; }

# Only a CI value that means true makes a missing cross compiler fatal.
test_windows_check_requires_the_compiler_only_when_ci_is_true() {
  local v got
  for v in true 1 yes; do
    got=$(windows_check CI=$v)
    [[ $got == "cargo xtask check-windows --require" ]] || fail "CI=$v: $got"
  done
  for v in false 0 no ""; do
    got=$(windows_check CI=$v)
    [[ $got == "cargo xtask check-windows " || $got == "cargo xtask check-windows" ]] || fail "CI=$v: $got"
  done
  got=$(windows_check)
  [[ $got == "cargo xtask check-windows " || $got == "cargo xtask check-windows" ]] || fail "CI unset: $got"
}

# The msvc cross-build recipes carry the pg_query flag only under the msvc target's own
# CFLAGS name, never plain CFLAGS (which would reach host build scripts too).
test_xwin_recipes_scope_the_pg_query_flag_to_msvc() {
  local target got
  for target in xwin-check xwin-build; do
    got=$(make -s -n -C "$ROOT" "$target" CARGO=cargo 2>&1) || true
    [[ $got == *"'CFLAGS_x86_64-pc-windows-msvc=-Wno-error=incompatible-pointer-types'"* ]] || fail "$target: $got"
    [[ $got != *" CFLAGS="* && $got != "CFLAGS="* ]] || fail "$target sets plain CFLAGS: $got"
    [[ $got == *"cargo xwin "*"--locked --target x86_64-pc-windows-msvc -p dbento-cli -p dbento-app -p xtask"* ]] || fail "$target: $got"
  done
}

# The msvc cross-build compiles DuckDB: no feature switch drops it, and the static-build define
# that clang-cl needs reaches only msvc C++ builds, never plain CXXFLAGS.
test_xwin_recipes_build_duckdb_statically_for_msvc() {
  local target got
  for target in xwin-check xwin-build; do
    got=$(make -s -n -C "$ROOT" "$target" CARGO=cargo 2>&1) || true
    [[ $got == *"'CXXFLAGS_x86_64-pc-windows-msvc=-DDUCKDB_STATIC_BUILD'"* ]] || fail "$target: $got"
    [[ $got != *" CXXFLAGS="* && $got != "CXXFLAGS="* ]] || fail "$target sets plain CXXFLAGS: $got"
    [[ $got != *"--no-default-features"* ]] || fail "$target drops default features (DuckDB): $got"
  done
}

# Only the release-style cross-build uses the runtime-shader profile; the shipping release
# profile never sees the override.
test_xwin_build_uses_its_own_profile() {
  local got
  got=$(make -s -n -C "$ROOT" xwin-build CARGO=cargo 2>&1) || true
  [[ $got == *"cargo xwin build --profile xwin-release "* ]] || fail "xwin-build: $got"
  got=$(make -s -n -C "$ROOT" xwin-check CARGO=cargo 2>&1) || true
  [[ $got == *"cargo xwin check "* && $got != *"--profile"* ]] || fail "xwin-check: $got"
  awk '/^\[profile\.release(\.|\])/,/^$/' "$ROOT/Cargo.toml" | grep -q gpui-pre-windows \
    && fail "the shipping release profile overrides gpui-pre-windows"
  grep -q '^\[profile\.xwin-release\.package\.gpui-pre-windows\]' "$ROOT/Cargo.toml" \
    || fail "no xwin-release override for gpui-pre-windows"
}

for t in $(declare -F | awk '{print $3}' | grep '^test_'); do "$t"; done
if ((FAILS > 0)); then echo "$FAILS failure(s)" >&2; exit 1; fi
echo "makefile tests passed"
