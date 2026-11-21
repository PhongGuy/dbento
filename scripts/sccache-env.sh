#!/usr/bin/env bash
# Prints the export lines that route the C and C++ build through sccache; use it as
# `eval "$(scripts/sccache-env.sh)"` before cargo or make. Prints nothing without sccache.
# Toggling it for an existing target dir reruns the DuckDB C++ build once.
set -euo pipefail

command -v sccache >/dev/null 2>&1 || exit 0

# The basedir and server port are keyed on the physical target path, so a relative or
# symlinked CARGO_TARGET_DIR yields the same settings as its real location.
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
target=${CARGO_TARGET_DIR:-$repo_root/target}
mkdir -p "$target"
target=$(cd "$target" && pwd -P)

printf 'export %s=%q\n' CC 'sccache cc'
printf 'export %s=%q\n' CXX 'sccache c++'
if [[ -z ${SCCACHE_DIR:-} ]]; then
  printf 'export %s=%q\n' SCCACHE_DIR "$HOME/.cache/dbento-sccache"
fi
printf 'export %s=%q\n' SCCACHE_BASEDIRS "$target"
# One server per target dir, since each server strips only its own basedir; a port
# collision between two dirs only costs cache misses.
if [[ -z ${SCCACHE_SERVER_PORT:-} ]]; then
  sum=$(printf '%s' "$target" | cksum)
  printf 'export %s=%q\n' SCCACHE_SERVER_PORT "$((4300 + ${sum%% *} % 1000))"
fi
