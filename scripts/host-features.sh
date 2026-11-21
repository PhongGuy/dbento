#!/usr/bin/env bash
# Keeps the libduckdb-sys and pg_query build scripts shared between a workspace build and narrower
# ones: cargo hashes a build script with the features of its host dependencies, so when
# `-p dbento-core` resolves them differently it compiles DuckDB again. Fails, printing the diff,
# when a narrower selection gives those host crates other features than `--workspace` does.
set -euo pipefail
CARGO=${CARGO:-cargo}
LOCKED=${LOCKED:-}
HOST=${HOST:-$(rustc -vV | sed -n 's/^host: //p')}

CRATES=(libduckdb-sys pg_query)
SELECTIONS=(
  "-p dbento-core"
  "-p dbento-core --no-default-features"
  "-p dbento-core --no-default-features --features duckdb"
  "-p dbento-cli"
  "-p dbento-testkit --all-features"
)

# Prints "name version [features]" for every crate under the first <crate> node's
# [build-dependencies] section, sorted; nothing when <crate> is not in the tree. awk reads to the
# end, since leaving early would kill cargo with SIGPIPE and pipefail would fail the check.
build_deps() {
  local crate=$1
  shift
  # shellcheck disable=SC2086 # LOCKED is empty or one flag; the selection is several words.
  "$CARGO" tree $LOCKED "$@" -e normal,build --no-dedupe --charset ascii --color never --target "$HOST" \
    --format '{p} [{f}]' </dev/null |
    awk -v c="$crate" '
      { match($0, /[^|`\- ]/); d = (RSTART - 1) / 4; t = substr($0, RSTART) }
      !found { if (index(t, c " v") == 1) { found = 1; base = d }; next }
      done { next }
      t ~ /^\[/ { if (d == base) inbuild = (t == "[build-dependencies]"); next }
      d <= base { done = 1; next }
      inbuild { sub(/ \(proc-macro\)$/, "", t); print t }
    ' | sort -u
}

status=0
for crate in "${CRATES[@]}"; do
  want=$(build_deps "$crate" --workspace)
  if [[ -z $want ]]; then
    echo "host-features: cargo tree --workspace showed no build dependencies for $crate" >&2
    exit 1
  fi
  for selection in "${SELECTIONS[@]}"; do
    # shellcheck disable=SC2086 # The selection is several words.
    got=$(build_deps "$crate" $selection)
    # A selection that leaves the crate out never runs its build script.
    [[ -z $got ]] && continue
    if [[ $got != "$want" ]]; then
      echo "host-features: $crate build dependencies differ under '$selection' (< it, > workspace):" >&2
      diff <(echo "$got") <(echo "$want") >&2 || true
      # The pins live in the selected crate's manifest; dbento-cli inherits dbento-core's.
      manifest=crates/dbento-core/Cargo.toml
      [[ $selection == "-p dbento-testkit"* ]] && manifest=crates/dbento-testkit/Cargo.toml
      echo "host-features: add the missing features to [build-dependencies] in $manifest" >&2
      status=1
    fi
  done
  ((status == 0)) && echo "host-features: $crate build script is shared by every selection"
done
exit "$status"
