#!/usr/bin/env bash
# Tests for scripts/timing-guard.sh, each against a throwaway git repo shaped like this one.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
GUARD=$ROOT/scripts/timing-guard.sh

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
FAILS=0
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }

# A fresh repo with a grid file, an unrelated file and a lock holding one gpui package, all
# committed on a branch named base. Prints its path.
repo() {
  local dir=$WORK/$1
  mkdir -p "$dir/crates/dbento-app/src"
  git init -q -b base "$dir"
  git -C "$dir" config core.hooksPath /dev/null
  git -C "$dir" config user.name test
  git -C "$dir" config user.email test@example.invalid
  echo target/ >"$dir/.gitignore"
  echo grid >"$dir/crates/dbento-app/src/grid.rs"
  echo other >"$dir/crates/dbento-app/src/other.rs"
  printf '[[package]]\nname = "gpui-kit"\nversion = "0.7.1"\n\n[[package]]\nname = "serde"\nversion = "1.0.0"\n' >"$dir/Cargo.lock"
  git -C "$dir" add -A
  git -C "$dir" commit -q -m base
  git -C "$dir" checkout -q -b work
  echo "$dir"
}

# Appends a line to `file` in `dir` and commits it.
change() {
  local dir=$1 file=$2
  echo changed >>"$dir/$file"
  git -C "$dir" commit -q -am "change $file"
}

# Runs the guard in repo `dir` against the base branch, with any further arguments.
guard() {
  local dir=$1
  shift
  (cd "$dir" && env -u CARGO_TARGET_DIR BASE=base bash "$GUARD" "$@" 2>&1)
}

test_no_relevant_change_passes() {
  local dir; dir=$(repo plain)
  change "$dir" crates/dbento-app/src/other.rs
  guard "$dir" >/dev/null || fail "an unrelated change was held back"
}

test_a_grid_change_without_a_stamp_fails() {
  local dir msg; dir=$(repo nostamp)
  change "$dir" crates/dbento-app/src/grid.rs
  if msg=$(guard "$dir"); then fail "a grid change passed without a stamp"; fi
  [[ $msg == *"make pilot-timing"* ]] || fail "failure does not name make pilot-timing: $msg"
  [[ $msg == *"crates/dbento-app/src/grid.rs"* ]] || fail "failure does not name the file: $msg"
}

test_a_grid_change_with_a_stamp_for_this_tree_passes() {
  local dir; dir=$(repo stamped)
  change "$dir" crates/dbento-app/src/grid.rs
  guard "$dir" stamp >/dev/null || fail "stamp failed on a clean tree"
  [[ -f $dir/target/pilot-timing/$(git -C "$dir" rev-parse 'HEAD^{tree}').ok ]] || fail "no stamp under target"
  guard "$dir" >/dev/null || fail "a stamped grid change was held back"
}

test_a_stamp_for_another_tree_fails() {
  local dir; dir=$(repo stale)
  change "$dir" crates/dbento-app/src/grid.rs
  guard "$dir" stamp >/dev/null
  change "$dir" crates/dbento-app/src/grid.rs
  if guard "$dir" >/dev/null; then fail "a stamp for an older tree was accepted"; fi
}

test_a_gpui_lock_bump_counts_and_another_lock_change_does_not() {
  local dir; dir=$(repo lock)
  sed -i 's/1.0.0/1.0.1/' "$dir/Cargo.lock"
  git -C "$dir" commit -q -am "bump serde"
  guard "$dir" >/dev/null || fail "a non-gpui lock change was held back"
  sed -i 's/0.7.1/0.7.2/' "$dir/Cargo.lock"
  git -C "$dir" commit -q -am "bump gpui-kit"
  if guard "$dir" >/dev/null; then fail "a gpui-kit bump passed without a stamp"; fi
}

test_stamp_refuses_a_dirty_tree() {
  local dir; dir=$(repo dirty)
  echo edit >>"$dir/crates/dbento-app/src/grid.rs"
  if guard "$dir" stamp >/dev/null; then fail "stamped a dirty tree"; fi
  [[ ! -d $dir/target/pilot-timing ]] || fail "a refused stamp left a file"
}

test_stamp_refuses_a_tree_other_than_the_one_measured() {
  local dir before; dir=$(repo moved)
  before=$(guard "$dir" tree)
  change "$dir" crates/dbento-app/src/other.rs
  if guard "$dir" stamp "$before" >/dev/null; then fail "stamped a tree that was not measured"; fi
}

test_the_stamp_honours_cargo_target_dir() {
  local dir; dir=$(repo targetdir)
  change "$dir" crates/dbento-app/src/grid.rs
  (cd "$dir" && CARGO_TARGET_DIR=$WORK/elsewhere bash "$GUARD" stamp >/dev/null) || fail "stamp failed"
  [[ -n $(find "$WORK/elsewhere/pilot-timing" -name '*.ok' 2>/dev/null) ]] || fail "no stamp under CARGO_TARGET_DIR"
  (cd "$dir" && CARGO_TARGET_DIR=$WORK/elsewhere BASE=base bash "$GUARD" >/dev/null) || fail "stamp under CARGO_TARGET_DIR not found"
}

test_make_targets_wire_the_guard() {
  local got
  got=$(make -s -n -C "$ROOT" pilot-timing CARGO=cargo)
  [[ $got == *"cargo test --manifest-path tools/dbento-pilot/Cargo.toml --test wheel_scroll -- --exact wheel_scroll_headless wheel_scroll_session"* ]] || fail "pilot-timing: $got"
  [[ $got == *"scripts/timing-guard.sh stamp"* ]] || fail "pilot-timing does not stamp: $got"
  got=$(make -s -n -C "$ROOT" pilot-check CARGO=cargo)
  [[ $got == *"-- --skip wheel_scroll_headless --skip wheel_scroll_session"* ]] || fail "pilot-check: $got"
  got=$(make -s -n -C "$ROOT" timing-guard)
  [[ $got == *"scripts/timing-guard.sh"* ]] || fail "timing-guard: $got"
  [[ -x $GUARD ]] || fail "scripts/timing-guard.sh is not executable"
}

for t in $(declare -F | awk '{print $3}' | grep '^test_'); do "$t"; done
if ((FAILS > 0)); then echo "$FAILS failure(s)" >&2; exit 1; fi
echo "timing-guard tests passed"
