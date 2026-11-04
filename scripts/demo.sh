#!/usr/bin/env bash
# Opens the committed demo workspace in the app with throwaway app state: `make demo`.
# Each run copies examples/demo fresh into ${XDG_CACHE_HOME:-~/.cache}/dbento-demo/workspace and
# builds shop.sqlite there, so edits made in the app never reach the repo. DEMO_DRY_RUN=1
# prints the launch line instead; DBENTO_XTASK names a built xtask binary to skip cargo run.
# Must stay bash 3.2 safe, since macOS ships /bin/bash 3.2.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cargo=${CARGO:-cargo}

die() { echo "demo: $*" >&2; exit 1; }

# XDG requires an absolute cache home; per the XDG spec, a relative one is ignored.
cache=${XDG_CACHE_HOME:-}
case $cache in
  /*) ;;
  *) cache=${HOME:?HOME is not set}/.cache ;;
esac
base=$cache/dbento-demo
# The copy below deletes $base/workspace, so refuse anything that is not a real absolute path.
case $base in
  /?*/dbento-demo) ;;
  *) die "refusing demo directory $base" ;;
esac

rm -rf "$base/workspace"
mkdir -p "$base/state"
cp -R "$ROOT/examples/demo" "$base/workspace"

# Build and launch from the repo, so a relative CARGO_TARGET_DIR means the same dir to both.
cd "$ROOT"
if [ -n "${DBENTO_XTASK:-}" ]; then
  "$DBENTO_XTASK" demo-db "$base/workspace/seed.sql" "$base/workspace/shop.sqlite"
else
  "$cargo" xtask demo-db "$base/workspace/seed.sql" "$base/workspace/shop.sqlite"
fi

# A release build: a debug GPUI cannot hold frame rate on a high-refresh display.
app=${CARGO_TARGET_DIR:-$ROOT/target}/release/dbento-app
if [ -n "${DEMO_DRY_RUN:-}" ]; then
  echo "DBENTO_STATE_DIR=$base/state $app $base/workspace"
  exit 0
fi

"$cargo" build --release -p dbento-app
DBENTO_STATE_DIR=$base/state exec "$app" "$base/workspace"
