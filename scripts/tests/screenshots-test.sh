#!/usr/bin/env bash
# Tests for scripts/screenshots.sh: the PNG pass and the make target that runs it.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
source "$ROOT/scripts/screenshots.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
FAILS=0
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }

# A stand-in magick that logs its input and writes "small" to its output path.
fake_magick() {
  mkdir -p "$WORK/bin"
  cat >"$WORK/bin/magick" <<EOF
#!/usr/bin/env bash
echo "\$1" >>"$WORK/magick.log"
printf small >"\${@: -1}"
EOF
  chmod +x "$WORK/bin/magick"
}

test_optimise_rewrites_each_png_in_place() {
  local d=$WORK/shots
  mkdir -p "$d"
  printf big >"$d/a.png"; printf big >"$d/b.png"; printf keep >"$d/notes.txt"
  fake_magick
  PATH=$WORK/bin:$PATH optimise_pngs "$d" || fail "optimise failed"
  [[ $(cat "$d/a.png") == small && $(cat "$d/b.png") == small ]] || fail "PNGs not replaced"
  [[ $(cat "$d/notes.txt") == keep ]] || fail "a non-PNG was touched"
  [[ $(wc -l <"$WORK/magick.log") == 2 ]] || fail "magick ran $(wc -l <"$WORK/magick.log") times, want 2"
  [[ -z $(find "$d" -name '*.tmp.png') ]] || fail "a temporary PNG was left behind"
}

test_optimise_without_magick_leaves_the_pngs() {
  local d=$WORK/plain msg
  mkdir -p "$d"
  printf big >"$d/a.png"
  msg=$(PATH=/nonexistent optimise_pngs "$d" 2>&1) || fail "optimise failed without magick"
  [[ $(cat "$d/a.png") == big ]] || fail "PNG changed without magick"
  [[ $msg == *"magick not found"* ]] || fail "no message without magick: $msg"
}

test_make_target_runs_the_script() {
  grep -qE '^screenshots:$' "$ROOT/Makefile" || fail "no screenshots target"
  grep -qF 'scripts/screenshots.sh' "$ROOT/Makefile" || fail "the target does not run scripts/screenshots.sh"
  [[ -x $ROOT/scripts/screenshots.sh ]] || fail "scripts/screenshots.sh is not executable"
}

for t in $(declare -F | awk '{print $3}' | grep '^test_'); do "$t"; done
if ((FAILS > 0)); then echo "$FAILS failure(s)" >&2; exit 1; fi
echo "screenshots tests passed"
