#!/usr/bin/env bash
# Holds back a change to the grid, its scrolling or rendering until the wheel timing tests have
# passed on exactly this tree. `make pilot-timing` writes the stamp; `make timing-guard` checks it.
#   timing-guard.sh          check $BASE...HEAD (BASE defaults to origin/master)
#   timing-guard.sh tree     print the HEAD tree, refusing a dirty working tree
#   timing-guard.sh stamp [TREE]  record a measured pass for HEAD's tree (TREE: the one measured)
set -euo pipefail

# Files whose any change can move the frame timings.
TIMED_PATHS=(
  crates/dbento-app/src/grid.rs
  crates/dbento-app/src/results.rs
  crates/dbento-app/src/results_pane.rs
  crates/dbento-app/src/smooth_scroll.rs
  tools/dbento-pilot/tests/wheel_scroll.rs
)
# Files where only the gpui pins (and, in a manifest, the timing profile) count.
PIN_FILES=(
  Cargo.toml
  Cargo.lock
  crates/dbento-app/Cargo.toml
  tools/dbento-pilot/Cargo.toml
  tools/dbento-pilot/Cargo.lock
)

cd "$(git rev-parse --show-toplevel)"
STAMPS=${CARGO_TARGET_DIR:-target}/pilot-timing

# The gpui part of `file` at `rev`: whole gpui package entries of a lock, or a manifest's
# gpui lines and [profile.timing] section. Empty when the file does not exist there.
pins() {
  local rev=$1 file=$2
  git show "$rev:$file" 2>/dev/null | if [[ $file == *.lock ]]; then
    awk 'BEGIN { RS = "" } /(^|\n)name = "gpui/'
  else
    awk '/^\[/ { timing = ($0 == "[profile.timing]") } timing || /gpui/'
  fi || true
}

clean_tree() {
  if [[ -n $(git status --porcelain) ]]; then
    echo "timing-guard: the working tree is dirty; commit first so the stamp names what was measured" >&2
    return 1
  fi
  git rev-parse 'HEAD^{tree}'
}

check() {
  local base changed=() file tree
  base=$(git merge-base "${BASE:-origin/master}" HEAD)
  while IFS= read -r file; do
    [[ -n $file ]] && changed+=("$file")
  done < <(git diff --name-only "$base" HEAD -- "${TIMED_PATHS[@]}")
  for file in "${PIN_FILES[@]}"; do
    [[ $(pins "$base" "$file") == "$(pins HEAD "$file")" ]] || changed+=("$file (gpui pins)")
  done
  if ((${#changed[@]} == 0)); then
    echo "timing-guard: no grid, scroll or render change"
    return 0
  fi
  tree=$(git rev-parse 'HEAD^{tree}')
  if [[ -f $STAMPS/$tree.ok ]]; then
    echo "timing-guard: wheel timing passed on this tree"
    return 0
  fi
  {
    echo "timing-guard: these changes need a wheel timing pass on tree $tree:"
    printf '  %s\n' "${changed[@]}"
    echo "Run \`make pilot-timing\` on a quiet machine, then run this again."
  } >&2
  return 1
}

stamp() {
  local tree
  tree=$(clean_tree)
  if [[ -n ${1:-} && $1 != "$tree" ]]; then
    echo "timing-guard: HEAD's tree is $tree, but $1 was measured; not stamping" >&2
    return 1
  fi
  mkdir -p "$STAMPS"
  date -u +%FT%TZ >"$STAMPS/$tree.ok"
  echo "timing-guard: stamped $STAMPS/$tree.ok"
}

case ${1:-check} in
  check) check ;;
  tree) clean_tree ;;
  stamp) stamp "${2:-}" ;;
  *) echo "usage: timing-guard.sh [check | tree | stamp [TREE]]" >&2; exit 2 ;;
esac
