#!/usr/bin/env bash
# Visual-pass QA: runs one set of pilot calls (stdin) in both themes on a fresh copy of the
# DBFlux-comparison fixture workspace, with `$WS` and `$THEME` replaced. Shot N is saved
# as qa/visual-pass/<prefix>-<view N>-<theme>.png, and each view with a DBFlux reference
# gets <prefix>-<view>-montage.png (dark, light, DBFlux). Exits non-zero if any call failed.
set -euo pipefail

if [ $# -lt 2 ]; then
    echo "usage: $0 <prefix> <view>... < calls" >&2
    exit 2
fi
prefix=$1
shift
views=("$@")
root=$(cd "$(dirname "$0")/.." && pwd)
compare="$HOME/.cache/dbento-orchestration/qa/dbflux-compare"
qa="$HOME/.cache/dbento-orchestration/qa/visual-pass"
run="$qa/run"

calls=$(mktemp)
trap 'rm -f "$calls"' EXIT
cat > "$calls"

# gpui-pilot.toml names the binary under the package's own target directory.
unset CARGO_TARGET_DIR
cd "$root"
cargo build --manifest-path tools/dbento-pilot/Cargo.toml

# A failed theme still lets the other run and the montages build, so one run shows all.
status=0
for t in dark light; do
    out="$run/$prefix-$t"
    rm -rf "$out"
    mkdir -p "$out"
    cp -R "$compare/ws/." "$out/ws"
    sed -e "s|\$WS|$out/ws|g" -e "s|\$THEME|$t|g" "$calls" |
        python3 "${DRIVE_PY:-$HOME/git/gpui-pilot/scripts/drive.py}" --out "$out" || status=1
    for i in "${!views[@]}"; do
        shot="$out/shot-$((i + 1)).png"
        if [ -e "$shot" ]; then
            command cp -f "$shot" "$qa/$prefix-${views[$i]}-$t.png"
        else
            echo "missing $shot for view ${views[$i]}" >&2
            status=1
        fi
    done
done

for view in "${views[@]}"; do
    ref="$compare/dbflux/png/$view-dark.png"
    dark="$qa/$prefix-$view-dark.png"
    light="$qa/$prefix-$view-light.png"
    [ -e "$ref" ] && [ -e "$dark" ] && [ -e "$light" ] || continue
    magick montage "$dark" "$light" "$ref" \
        -tile 3x -geometry '1600x900>+6+6' -background '#808080' \
        "$qa/$prefix-$view-montage.png"
done
exit "$status"
