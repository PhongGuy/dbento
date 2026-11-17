#!/usr/bin/env bash
# Release helpers for the release workflow: the tag guard, the build matrix, the secret-free
# build and package, the macOS dmg, stable-named copies, SHA256SUMS and the update manifest.
# Must stay bash 3.2 safe, since macOS runners run /bin/bash 3.2.
# Sourcing this file only defines functions; running it dispatches a subcommand.
source "$(dirname "${BASH_SOURCE[0]}")/package-linux.sh"
set -euo pipefail

# The workspace version, read the way package-linux.sh's main reads it.
workspace_version() {
  local v
  v=$(cargo pkgid -p dbento-cli) || return 1
  echo "${v##*[#@]}"
}

# Refuse a release from the wrong ref: a tag must be exactly vVERSION, and a
# branch may only start a dry run.
check_ref() {
  local type=$1 name=$2 version=$3 dry=$4
  case $type in
    tag)
      if [[ $name != "v$version" ]]; then
        echo "release: tag $name does not match the workspace version v$version" >&2
        return 1
      fi
      ;;
    branch)
      if [[ $dry != true ]]; then
        echo "release: a branch ($name) can only start a dry run; push tag v$version to release" >&2
        return 1
      fi
      ;;
    *)
      echo "release: unsupported ref type '$type'" >&2
      return 1
      ;;
  esac
}

# Print the build matrix for PLATFORMS as one JSON line, rows in a fixed order.
# Only a dry run may build a subset; a real release needs all four platforms.
select_targets() {
  local platforms=${1:-linux windows macos-arm64 macos-intel} dry=$2
  local linux=0 windows=0 mac_arm=0 mac_intel=0 p tokens=()
  # Split with read so a token is never glob-expanded; empty tokens are skipped.
  IFS=', ' read -r -a tokens <<<"$platforms"
  for p in ${tokens[@]+"${tokens[@]}"}; do
    case $p in
      '') ;;
      linux) linux=1 ;;
      windows) windows=1 ;;
      macos-arm64) mac_arm=1 ;;
      macos-intel) mac_intel=1 ;;
      *) echo "release: unknown platform '$p'" >&2; return 1 ;;
    esac
  done
  if [[ $dry != true && $linux$windows$mac_arm$mac_intel != 1111 ]]; then
    echo "release: a release must build linux, windows, macos-arm64 and macos-intel (got: $platforms)" >&2
    return 1
  fi

  local rows=()
  if ((linux)); then
    rows+=('{"os":"ubuntu-24.04","target":"x86_64-unknown-linux-gnu"}')
    rows+=('{"os":"ubuntu-24.04-arm","target":"aarch64-unknown-linux-gnu"}')
  fi
  if ((windows)); then rows+=('{"os":"windows-2025","target":"x86_64-pc-windows-msvc"}'); fi
  if ((mac_arm)); then rows+=('{"os":"macos-15","target":"aarch64-apple-darwin"}'); fi
  if ((mac_intel)); then rows+=('{"os":"macos-15-intel","target":"x86_64-apple-darwin"}'); fi
  if ((${#rows[@]} == 0)); then
    echo "release: no platforms selected" >&2
    return 1
  fi
  local IFS=,
  echo "{\"include\":[${rows[*]}]}"
}

# Ad-hoc sign PATH (no Apple identity) and check the signature holds.
adhoc_sign() {
  "${CODESIGN:-codesign}" --force --deep --sign - "$1" || return 1
  "${CODESIGN:-codesign}" --verify --deep --strict "$1"
}

# Ad-hoc sign OUT/Dbento.app, tar the signed bundle into UPDATE as the updater's payload, wrap
# it as OUT/Dbento_VERSION_ARCH.dmg, and drop the .app so only the release artifact is left.
macos_dmg() {
  local out=$1 version=$2 arch=$3 update=$4
  local app=$out/Dbento.app
  adhoc_sign "$app" || return 1
  mkdir -p "$update" || return 1
  "${TAR:-tar}" -C "$out" -czf "$update/Dbento_${version}_${arch}.app.tar.gz" Dbento.app || return 1
  "${HDIUTIL:-hdiutil}" create -volname Dbento -srcfolder "$app" -ov -format UDZO \
    "$out/Dbento_${version}_${arch}.dmg" || return 1
  rm -rf "$app"
}

# GNU realpath -m for hosts without it (macOS): resolve the last argument even
# when it does not exist. python3 is on every runner image.
portable_realpath() {
  python3 -c 'import os, sys; print(os.path.realpath(sys.argv[-1]))' "${@: -1}"
}

# Write OUT/dbento-VERSION-TARGET.zip holding dbento.exe and each FILE under one
# top-level directory, replacing any earlier zip. Uses 7z when present (the Windows runners
# ship it; Git Bash has no zip), else zip, and fails by name when neither is on PATH.
cli_zip() {
  local binary=$1 version=$2 target=$3 out=$4
  shift 4
  local name=dbento-$version-$target stage zip f
  if ! command -v 7z >/dev/null 2>&1 && ! command -v zip >/dev/null 2>&1; then
    echo "release: neither 7z nor zip is on PATH; install 7-Zip to write $name.zip" >&2
    return 1
  fi
  mkdir -p "$out" || return 1
  zip=$(cd "$out" && pwd)/$name.zip
  rm -f -- "$zip"
  stage=$(mktemp -d) || return 1
  install -d -m 0755 "$stage/$name"
  install -m 0755 "$binary" "$stage/$name/dbento.exe"
  for f in "$@"; do
    install -m 0644 "$f" "$stage/$name/$(basename "$f")"
  done
  if command -v 7z >/dev/null 2>&1; then
    (cd "$stage" && 7z a -tzip "$zip" "$name" >/dev/null) || { rm -rf -- "$stage"; return 1; }
  else
    (cd "$stage" && zip -q -r -X "$zip" "$name") || { rm -rf -- "$stage"; return 1; }
  fi
  rm -rf -- "$stage"
}

# The packaging host, macos or windows; Linux and anything else is sent to make package.
package_os() {
  case $(uname -s) in
    Darwin) echo macos ;;
    MINGW* | MSYS* | CYGWIN*) echo windows ;;
    *) echo "release: on $(uname -s), run make package" >&2; return 1 ;;
  esac
}

# The repository root and the absolute output directory for OS: OUT_DIR, default target/dist/OS.
package_dirs() {
  # BSD realpath has no -m; prepare_out_dir needs it.
  realpath -m / >/dev/null 2>&1 || realpath() { portable_realpath "$@"; }
  ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
  OUT=${OUT_DIR:-$ROOT/target/dist/$1}
  UPDATE=${UPDATE_DIR:-$ROOT/target/update/$1}
}

# Build the release binaries and the notices on macOS or Windows into OUT_DIR.
build_main() {
  local os
  os=$(package_os) || return 1
  package_dirs "$os"
  prepare_out_dir "$ROOT" "$OUT" || return 1
  OUT=$(realpath -m -- "$OUT")
  cd "$ROOT"
  # libduckdb does not build for msvc.
  local features=()
  if [[ $os == windows ]]; then features=(--no-default-features --features dbento-core/keychain); fi
  cargo build --release --locked -p dbento-cli -p dbento-app ${features[@]+"${features[@]}"}
  cargo fetch --locked
  cargo xtask notices --out "$OUT/THIRD-PARTY-NOTICES.html"
  echo "release: $os build in $OUT"
}

# Package what build_main left into the CLI archive and the app installer. macOS is ad-hoc
# signed; Windows ships unsigned, since there is no CA-issued certificate.
package_main() {
  local os
  os=$(package_os) || return 1
  package_dirs "$os"
  local out version target bin_dir
  out=$(realpath -m -- "$OUT")
  UPDATE=$(realpath -m -- "$UPDATE")
  if [[ ! -f $out/THIRD-PARTY-NOTICES.html ]]; then
    echo "release: no build in $out; run release.sh build first" >&2
    return 1
  fi
  unset DBENTO_MACOS_SIGNING_IDENTITY DBENTO_WINDOWS_SIGN_COMMAND
  cd "$ROOT"
  version=$(workspace_version)
  target=$(rustc -vV | sed -n 's/^host: //p')
  bin_dir=${CARGO_TARGET_DIR:-$ROOT/target}/release
  local files=("$ROOT/LICENSE-MIT" "$ROOT/LICENSE-APACHE" "$ROOT/README.md" "$out/THIRD-PARTY-NOTICES.html")
  if [[ $os == macos ]]; then
    package_app "$out" app
    TAR=gtar macos_dmg "$out" "$version" "${target%%-*}" "$UPDATE"
    adhoc_sign "$bin_dir/dbento"
    TAR=gtar cli_archive "$bin_dir/dbento" "$version" "$target" "$out" "${files[@]}"
  else
    package_app "$out" nsis
    cli_zip "$bin_dir/dbento.exe" "$version" "$target" "$out" "${files[@]}"
  fi
  rm -rf -- "$out/.cargo-packager"
  echo "release: $os artifacts in $out ($([[ $os == macos ]] && echo ad-hoc signed || echo unsigned))"
}

# The update directory name for this host: linux, macos or windows.
update_os() {
  case $(uname -s) in
    Linux) echo linux ;;
    Darwin) echo macos ;;
    MINGW* | MSYS* | CYGWIN*) echo windows ;;
    *) echo "release: no update files for $(uname -s)" >&2; return 1 ;;
  esac
}

# Copy the release CLI into UPDATE_DIR as dbento-VERSION-HOST[.exe], the raw binary the updater
# swaps in. The binary is checked first, so a missing build leaves no directory behind.
update_cli() {
  local os exe= bin version host
  os=$(update_os) || return 1
  package_dirs "$os"
  if [[ $os == windows ]]; then exe=.exe; fi
  bin=${CARGO_TARGET_DIR:-$ROOT/target}/release/dbento$exe
  if [[ ! -f $bin ]]; then
    echo "release: no release CLI at $bin; run release.sh build first" >&2
    return 1
  fi
  version=$(workspace_version) || return 1
  host=$(rustc -vV | sed -n 's/^host: //p')
  mkdir -p "$UPDATE" || return 1
  install -m 0755 "$bin" "$UPDATE/dbento-$version-$host$exe"
}

# The stable name of release file NAME at VERSION, for /releases/latest/download links: the
# version dropped. Prints nothing for a file that is not an artifact.
stable_name() {
  local name=$1 version=$2 out
  case $name in
    *.tar.gz | *.deb | *.AppImage | *.dmg | *.exe | *.zip) ;;
    *) return 0 ;;
  esac
  case $name in
    *"_${version}_"*) out=${name/"_${version}_"/_} ;;
    *"-${version}-"*) out=${name/"-${version}-"/-} ;;
    *) return 0 ;;
  esac
  echo "$out"
}

# Copy every versioned artifact under DIR to its stable name at DIR's top level, beside the
# versioned file. Fails when two files would share one stable name, or when none is copied.
stable_copies() {
  local dir=$1 version=$2 f stable seen=" " copied=0
  while IFS= read -r f; do
    stable=$(stable_name "$(basename "$f")" "$version")
    [[ -n $stable ]] || continue
    if [[ $seen == *" $stable "* ]]; then
      echo "release: two release files map to the stable name $stable" >&2
      return 1
    fi
    seen="$seen$stable "
    cp -- "$f" "$dir/$stable" || return 1
    copied=$((copied + 1))
  done < <(find "$dir" -type f | LC_ALL=C sort)
  if ((copied == 0)); then
    echo "release: no versioned release files under $dir" >&2
    return 1
  fi
}

# The update targets: id, where the file ships from, its name with @V@ for the version, and its format.
update_targets() {
  cat <<'EOF'
app-linux-x86_64 dist dbento-app_@V@_x86_64.AppImage appimage
app-linux-aarch64 dist dbento-app_@V@_aarch64.AppImage appimage
app-macos-aarch64 update Dbento_@V@_aarch64.app.tar.gz app
app-macos-x86_64 update Dbento_@V@_x86_64.app.tar.gz app
app-windows-x86_64 dist dbento-app_@V@_x64-setup.exe nsis
cli-linux-x86_64 update dbento-@V@-x86_64-unknown-linux-gnu binary
cli-linux-aarch64 update dbento-@V@-aarch64-unknown-linux-gnu binary
cli-macos-aarch64 update dbento-@V@-aarch64-apple-darwin binary
cli-macos-x86_64 update dbento-@V@-x86_64-apple-darwin binary
cli-windows-x86_64 update dbento-@V@-x86_64-pc-windows-msvc.exe binary
EOF
}

# One manifest entry as compact JSON, {ID: {url, format, size, sha256}}; refuses a malformed id or a missing file.
manifest_entry() {
  local id=$1 file=$2 format=$3 url=$4 re='^(app|cli)-(linux|macos|windows)-(x86_64|aarch64)$' size sha
  if [[ ! $id =~ $re ]]; then echo "release: malformed update target id '$id'" >&2; return 1; fi
  if [[ ! -f $file ]]; then echo "release: update target $id needs $file, which is missing" >&2; return 1; fi
  size=$(wc -c <"$file" | tr -d ' ') || return 1
  sha=$(sha256sum "$file" | cut -d' ' -f1) || return 1
  jq -cn --arg id "$id" --arg url "$url" --arg format "$format" --arg sha "$sha" --argjson size "$size" \
    '{($id): {url: $url, format: $format, size: $size, sha256: $sha}}'
}

# Write UPDATE/latest.json for stable VERSION, artifact URLs under BASE_URL; nothing is written on failure.
write_manifest() {
  local dist=$1 update=$2 version=$3 base=${4%/} re='^[0-9]+\.[0-9]+\.[0-9]+$' id where pattern format name dir entries=
  if [[ ! $version =~ $re ]]; then echo "release: the update manifest needs a stable version, got '$version'" >&2; return 1; fi
  while read -r id where pattern format; do
    name=${pattern//@V@/$version}
    if [[ $where == dist ]]; then dir=$dist; else dir=$update; fi
    entries="$entries$(manifest_entry "$id" "$dir/$name" "$format" "$base/$name")"$'\n' || return 1
  done < <(update_targets)
  printf '%s' "$entries" | jq -s --arg v "$version" --arg d "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{version: $v, pub_date: $d, targets: add}' >"$update/latest.json.tmp" || { rm -f -- "$update/latest.json.tmp"; return 1; }
  mv -- "$update/latest.json.tmp" "$update/latest.json"
}

usage() {
  echo "usage: release.sh guard TYPE NAME DRY [PLATFORMS] | build | package | update-cli | stable DIR VERSION | sums DIR | manifest DIST UPDATE VERSION BASE_URL" >&2
  exit 2
}

main() {
  case ${1:-} in
    guard)
      (($# >= 4)) || usage
      local version matrix
      version=$(workspace_version)
      check_ref "$2" "$3" "$version" "$4" || exit 1
      matrix=$(select_targets "${5:-}" "$4") || exit 1
      echo "version=$version"
      echo "matrix=$matrix"
      ;;
    # Called plainly so set -e covers every step; a function on the left of
    # || or in a condition runs with set -e off.
    build)
      (($# == 1)) || usage
      build_main
      ;;
    package)
      (($# == 1)) || usage
      package_main
      ;;
    update-cli)
      (($# == 1)) || usage
      update_cli
      ;;
    stable)
      (($# == 3)) || usage
      stable_copies "$2" "$3"
      ;;
    sums)
      (($# == 2)) || usage
      write_sums "$2"
      ;;
    manifest)
      (($# == 5)) || usage
      write_manifest "$2" "$3" "$4" "$5"
      ;;
    *) usage ;;
  esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then main "$@"; fi
