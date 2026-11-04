#!/usr/bin/env bash
# Runs one set of pilot calls (stdin) in both themes through pilot-qa.sh, as <name>-dark and
# <name>-light with `$THEME` replaced, then sets each dark/light shot pair above the DBFlux,
# DataGrip and Zed references in qa-m3a/<name>-montage/montage-N.png.
set -euo pipefail

if [ $# -ne 1 ]; then
    echo "usage: $0 <name> < calls" >&2
    exit 2
fi
name=$1
root=$(cd "$(dirname "$0")/.." && pwd)
qa="$HOME/.cache/dbento-orchestration/qa-m3a"
refs="$HOME/.cache/dbento-orchestration/refs"

calls=$(mktemp)
trap 'rm -f "$calls"' EXIT
cat > "$calls"

# The reference screenshots are fetched into the cache; they are never committed.
mkdir -p "$refs"
# Download beside the target and rename on success, so an interrupted fetch is retried.
fetch() {
    [ -s "$refs/$1" ] && return
    curl -fsSL -A 'dbento-research (+https://github.com)' -o "$refs/$1.part" "$2"
    mv "$refs/$1.part" "$refs/$1"
}
fetch dbflux.png https://raw.githubusercontent.com/0xErwin1/dbflux/main/resources/dbflux.png
fetch zed.webp https://zed.dev/img/home/og.webp
fetch datagrip.png https://resources.jetbrains.com/help/img/idea/2026.2/db_data_editor_query_result_tab_dark.png

for t in dark light; do
    sed "s|\$THEME|$t|g" "$calls" | "$root/scripts/pilot-qa.sh" "$name-$t"
done

out="$qa/$name-montage"
rm -rf "$out"
mkdir -p "$out"
for dark in "$qa/$name-dark"/shot-*.png; do
    [ -e "$dark" ] || continue
    shot=$(basename "$dark" .png)
    magick montage "$dark" "$qa/$name-light/$shot.png" \
        "$refs/dbflux.png" "$refs/datagrip.png" "$refs/zed.webp" \
        -tile 2x -geometry '1280x800>+6+6' -background '#808080' \
        "$out/montage-${shot#shot-}.png"
done
