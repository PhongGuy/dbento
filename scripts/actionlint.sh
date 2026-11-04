#!/usr/bin/env bash
# Runs a sha256-pinned actionlint release over .github/workflows, downloading it once into
# the user cache. shellcheck and pyflakes stay off so local and CI results agree.
set -euo pipefail

VERSION=1.7.12
case "$(uname -sm)" in
  "Linux x86_64")
    asset=linux_amd64
    sha=8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8
    ;;
  "Linux aarch64")
    asset=linux_arm64
    sha=325e971b6ba9bfa504672e29be93c24981eeb1c07576d730e9f7c8805afff0c6
    ;;
  *)
    echo "actionlint.sh: no pinned actionlint for $(uname -sm)" >&2
    exit 1
    ;;
esac

dir=${XDG_CACHE_HOME:-$HOME/.cache}/dbento/actionlint-$VERSION
bin=$dir/actionlint

# Verify the archive before anything is extracted, so a tampered download never lands
# in the cache under the binary's name.
if [[ ! -x $bin ]]; then
  mkdir -p "$dir"
  archive=$dir/actionlint_${VERSION}_$asset.tar.gz
  base=${ACTIONLINT_BASE_URL:-https://github.com/rhysd/actionlint/releases/download}
  curl -sSLf -A dbento-actionlint -o "$archive" "$base/v$VERSION/actionlint_${VERSION}_$asset.tar.gz"
  if [[ $(sha256sum "$archive" | cut -d' ' -f1) != "$sha" ]]; then
    rm -f "$archive"
    echo "actionlint.sh: sha256 mismatch for actionlint_${VERSION}_$asset.tar.gz" >&2
    exit 1
  fi
  tar -xzf "$archive" -C "$dir" actionlint
  rm -f "$archive"
fi

# actionlint finds .github/workflows from the git repository it runs in.
cd "$(dirname "${BASH_SOURCE[0]}")/.."
exec "$bin" -shellcheck= -pyflakes= "$@"
