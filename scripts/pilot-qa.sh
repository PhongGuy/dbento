#!/usr/bin/env bash
# Drives the app under gpui-pilot: reads one call per line (`tool {json}`) from stdin,
# with `$WS` replaced by a fresh QA workspace, and leaves shot-N.png and tree-N.json in
# ~/.cache/dbento-orchestration/qa-m3a/<name>/. Exits non-zero if any call failed.
set -euo pipefail

if [ $# -ne 1 ]; then
    echo "usage: $0 <name> < calls" >&2
    exit 2
fi
root=$(cd "$(dirname "$0")/.." && pwd)
out="$HOME/.cache/dbento-orchestration/qa-m3a/$1"

rm -rf "$out"
mkdir -p "$out"
"$root/scripts/qa-workspace.sh" "$out/ws"

# gpui-pilot.toml names the binary under the package's own target directory.
unset CARGO_TARGET_DIR
cd "$root"
cargo build --manifest-path tools/dbento-pilot/Cargo.toml

sed "s|\$WS|$out/ws|g" | python3 "${DRIVE_PY:-$HOME/git/gpui-pilot/scripts/drive.py}" --out "$out"
