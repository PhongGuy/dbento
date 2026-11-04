#!/usr/bin/env bash
# Regenerates the README screenshots in assets/screenshots: `make screenshots`. The pilot test
# tools/dbento-pilot/tests/screenshots.rs walks the demo shop and saves each shot, then every
# PNG is recompressed losslessly with ImageMagick when it is installed.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

# Strip metadata and recompress at the highest zlib level; the pixels stay as shot.
optimise_pngs() {
  local f
  if ! command -v magick >/dev/null 2>&1; then
    echo "screenshots: magick not found, PNGs left as shot" >&2
    return 0
  fi
  for f in "$1"/*.png; do
    [[ -e $f ]] || continue
    magick "$f" -strip -define png:compression-level=9 -define png:compression-filter=5 "$f.tmp.png" || return 1
    mv -- "$f.tmp.png" "$f"
  done
}

main() {
  local out=$ROOT/assets/screenshots
  mkdir -p "$out"
  rm -f -- "$out"/*.png
  cd "$ROOT"
  DBENTO_SHOTS_DIR=$out env -u WAYLAND_DISPLAY -u DISPLAY "${CARGO:-cargo}" test \
    --manifest-path tools/dbento-pilot/Cargo.toml --test screenshots
  optimise_pngs "$out"
  du -ch "$out"/*.png
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then main "$@"; fi
