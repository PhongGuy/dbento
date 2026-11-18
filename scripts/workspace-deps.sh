#!/usr/bin/env bash
# Keeps dbento-workspace and dbento-rules driver-free: fails when either normal dependency tree,
# on any target, holds a database driver, tokio or toml_edit. An empty tree means cargo failed.
set -euo pipefail
CARGO=${CARGO:-cargo}
LOCKED=${LOCKED:-}

CRATES=(dbento-workspace dbento-rules)
FORBIDDEN=(tokio rusqlite tokio-postgres mysql_async tiberius duckdb mongodb redis surrealdb
  aws-sdk-dynamodb toml_edit)

for crate in "${CRATES[@]}"; do
  # shellcheck disable=SC2086 # LOCKED is empty or one flag.
  tree=$("$CARGO" tree $LOCKED -p "$crate" -e normal --target all --prefix none --format '{p}')
  if [[ -z $tree ]]; then
    echo "workspace-deps: cargo tree printed nothing for $crate" >&2
    exit 1
  fi

  # A crate matches only on its exact name, the first word of a line, so tokio-util is allowed.
  found=()
  for name in "${FORBIDDEN[@]}"; do
    if awk -v n="$name" '$1 == n { hit = 1 } END { exit !hit }' <<<"$tree"; then
      found+=("$name")
    fi
  done
  if ((${#found[@]} > 0)); then
    echo "workspace-deps: $crate depends on forbidden crate(s): ${found[*]}" >&2
    exit 1
  fi
  echo "workspace-deps: $crate is driver-free"
done
