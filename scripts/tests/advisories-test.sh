#!/usr/bin/env bash
# Checks that the RustSec advisory gate is wired in: a pinned cargo-deny in CI, the check first
# in `make check`, both lockfiles covered, and every ignored advisory justified.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
CI=$ROOT/.github/workflows/ci.yml
DENY=$ROOT/deny.toml
FAILS=0
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }

test_ci_gate_installs_a_pinned_cargo_deny() {
  local gate
  gate=$(awk '/^  gate:$/ { on = 1; next } on && /^  [a-z]/ { on = 0 } on' "$CI")
  grep -qE '^        run: cargo install --locked cargo-deny --version [0-9]+\.[0-9]+\.[0-9]+$' <<<"$gate" \
    || fail "the gate job does not install cargo-deny at an exact version"
}

# Advisories run first so a vulnerable lock fails before the long build and test steps.
test_check_runs_advisories_first() {
  local prereqs
  prereqs=$(grep -E '^check:' "$ROOT/Makefile" | sed 's/^check:[[:space:]]*//')
  [[ ${prereqs%% *} == advisories ]] || fail "check's first prerequisite is not advisories: $prereqs"
  grep -qE '^\.PHONY:.* advisories( |$)' "$ROOT/Makefile" || fail "advisories is not .PHONY"
}

# The pilot is its own workspace with its own Cargo.lock, so it needs its own run.
test_advisories_covers_both_lockfiles() {
  local got
  got=$(make -s -n -C "$ROOT" advisories CARGO=cargo 2>&1) || { fail "make -n advisories: $got"; return 0; }
  grep -qxE 'cargo deny check advisories' <<<"$got" || fail "no root advisories run: $got"
  grep -qxE 'cargo deny --manifest-path tools/dbento-pilot/Cargo.toml check advisories' <<<"$got" \
    || fail "no pilot advisories run: $got"
  if grep -q -- '--disable-fetch' <<<"$got"; then fail "advisories skips the database fetch"; fi
}

# Each ignore is an inline table with an id and a non-empty reason; a bare id would hide why.
test_every_ignore_has_a_reason() {
  [[ -f $DENY ]] || { fail "no deny.toml"; return 0; }
  grep -qxF '[advisories]' "$DENY" || fail "deny.toml has no [advisories] section"
  grep -qE '^yanked = "deny"$' "$DENY" || fail "yanked crates do not fail"
  local entries bad
  entries=$(awk '/^ignore = \[/ { on = 1; next } on && /^\]/ { on = 0 } on && !/^[[:space:]]*(#|$)/' "$DENY")
  bad=$(grep -vE '^[[:space:]]*\{ id = "RUSTSEC-[0-9]{4}-[0-9]{4}", reason = "[^"]+" \},?$' <<<"$entries" || true)
  [[ -z $bad ]] || fail "ignore entries without an id and reason: $bad"
}

for t in $(declare -F | awk '{print $3}' | grep '^test_'); do "$t"; done
if ((FAILS > 0)); then echo "$FAILS failure(s)" >&2; exit 1; fi
echo "advisories tests passed"
