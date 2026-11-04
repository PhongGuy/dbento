#!/usr/bin/env bash
# Release helpers for the release workflow: the tag guard, the build matrix, the secret-free
# build, Windows signing, the macOS dmg, the per-OS package entry, stable-named copies and SHA256SUMS.
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

# Windows signing is driven by these two secrets; a missing GitHub secret
# expands to an empty string, so blank (empty or whitespace) counts as unset.
WINDOWS_SIGNING_VARS="WINDOWS_CERTIFICATE WINDOWS_CERTIFICATE_PASSWORD"

# Print the name of each named variable that is unset, empty or whitespace only.
blank_vars() {
  local v val
  for v in "$@"; do
    val=${!v:-}
    if [[ -z ${val//[[:space:]]/} ]]; then echo "$v"; fi
  done
}

# Print signed when both signing secrets are present and unsigned when both are
# blank; with only one present, fail and name the blank one (never a value).
windows_signing_mode() {
  local blank total
  blank=$(blank_vars $WINDOWS_SIGNING_VARS)
  set -- $WINDOWS_SIGNING_VARS
  total=$#
  set -- $blank
  if (($# == 0)); then
    echo signed
  elif (($# == total)); then
    echo unsigned
  else
    echo "release: set both Windows signing secrets or neither; blank: $*" >&2
    return 1
  fi
}

# The shell cargo-packager starts for signCommand: Git Bash's own bash.exe as
# an 8.3 path, so a bare bash never resolves to WSL's System32\bash.exe.
sign_shell() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -m -s "$(command -v bash)"
  else
    echo bash
  fi
}

# Write KEYDIR/sign.sh, which signtool-signs one file with KEYDIR/dbento.pfx.
# The heredoc is quoted so the password is read only when sign.sh runs.
write_sign_script() {
  cat >"$1/sign.sh" <<'SIGN'
#!/usr/bin/env bash
# Signs one file with the release certificate; cargo-packager runs it per file.
set -euo pipefail
export MSYS2_ARG_CONV_EXCL='*'
pfx=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/dbento.pfx
if command -v cygpath >/dev/null; then pfx=$(cygpath -w "$pfx"); fi
tool=${SIGNTOOL:-$(ls "/c/Program Files (x86)/Windows Kits/10/bin/"10.*/x64/signtool.exe | sort -V | tail -n 1)}
exec "$tool" sign /fd SHA256 /f "$pfx" /p "$WINDOWS_CERTIFICATE_PASSWORD" /d Dbento "$1"
SIGN
  chmod 700 "$1/sign.sh"
}

# Set up Windows signing in the caller's shell. Unsigned clears the secrets;
# signed decodes the certificate into a private KEYDIR and exports the
# signCommand. Every step returns explicitly, since callers may run it where
# set -e is suspended.
prepare_windows_signing() {
  local mode=$1 keydir=$2 shell
  case $mode in
    unsigned)
      unset WINDOWS_CERTIFICATE WINDOWS_CERTIFICATE_PASSWORD DBENTO_WINDOWS_SIGN_COMMAND
      return 0
      ;;
    signed) ;;
    *) echo "release: unknown Windows signing mode '$mode'" >&2; return 1 ;;
  esac

  # cargo-packager splits signCommand on spaces, so neither part may hold one.
  shell=${SIGN_SHELL:-$(sign_shell)} || return 1
  if [[ $shell == *' '* || $keydir == *' '* ]]; then
    echo "release: the sign shell ($shell) and key directory ($keydir) must not contain a space" >&2
    return 1
  fi

  (umask 077 && mkdir -p "$keydir") || return 1
  chmod 700 "$keydir" || return 1
  if ! (umask 077 && printf '%s' "$WINDOWS_CERTIFICATE" | base64 -d >"$keydir/dbento.pfx") 2>/dev/null; then
    rm -f "$keydir/dbento.pfx"
    echo "release: WINDOWS_CERTIFICATE is not valid base64" >&2
    return 1
  fi
  write_sign_script "$keydir" || return 1
  export DBENTO_WINDOWS_SIGN_COMMAND="$shell $keydir/sign.sh %1"
  unset WINDOWS_CERTIFICATE
}

# Rename the Windows installer and CLI zip directly in DIR to carry -unsigned.
mark_unsigned() {
  local f ext
  for f in "$1"/*-setup.exe "$1"/*.zip; do
    [[ -e $f ]] || continue
    case $f in *-unsigned.*) continue ;; esac
    ext=${f##*.}
    mv "$f" "${f%.*}-unsigned.$ext" || return 1
  done
}

# Ad-hoc sign PATH (no Apple identity) and check the signature holds.
adhoc_sign() {
  "${CODESIGN:-codesign}" --force --deep --sign - "$1" || return 1
  "${CODESIGN:-codesign}" --verify --deep --strict "$1"
}

# Ad-hoc sign OUT/Dbento.app, wrap it as OUT/Dbento_VERSION_ARCH.dmg, and
# drop the .app so only the release artifact is left.
macos_dmg() {
  local out=$1 version=$2 arch=$3
  local app=$out/Dbento.app
  adhoc_sign "$app" || return 1
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
}

# Build the release binaries and the notices on macOS or Windows into OUT_DIR. Build scripts
# run here, so this step refuses to start while a Windows signing secret is in the environment.
build_main() {
  local os v
  os=$(package_os) || return 1
  for v in $WINDOWS_SIGNING_VARS; do
    if [[ -z $(blank_vars "$v") ]]; then
      echo "release: $v is set; the build step must not see the signing secrets" >&2
      return 1
    fi
  done
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

# Package what build_main left into the CLI archive and the app installer. macOS is always
# ad-hoc signed; Windows is signed only with both secrets, which only this step sees.
package_main() {
  local os
  os=$(package_os) || return 1
  package_dirs "$os"
  local out mode=unsigned version target bin_dir
  out=$(realpath -m -- "$OUT")
  if [[ ! -f $out/THIRD-PARTY-NOTICES.html ]]; then
    echo "release: no build in $out; run release.sh build first" >&2
    return 1
  fi
  if [[ $os == windows ]]; then mode=$(windows_signing_mode) || return 1; fi
  KEYDIR=$(mktemp -d)
  trap 'rm -rf -- "$KEYDIR"' EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  prepare_windows_signing "$mode" "$KEYDIR/keys" || return 1
  unset DBENTO_MACOS_SIGNING_IDENTITY
  cd "$ROOT"
  version=$(workspace_version)
  target=$(rustc -vV | sed -n 's/^host: //p')
  bin_dir=${CARGO_TARGET_DIR:-$ROOT/target}/release
  local files=("$ROOT/LICENSE-MIT" "$ROOT/LICENSE-APACHE" "$ROOT/README.md" "$out/THIRD-PARTY-NOTICES.html")
  if [[ $os == macos ]]; then
    package_app "$out" app
    TAR=gtar macos_dmg "$out" "$version" "${target%%-*}"
    adhoc_sign "$bin_dir/dbento"
    TAR=gtar cli_archive "$bin_dir/dbento" "$version" "$target" "$out" "${files[@]}"
  else
    package_app "$out" nsis
    if [[ $mode == signed ]]; then bash "$KEYDIR/keys/sign.sh" "$(cygpath -w "$bin_dir/dbento.exe")"; fi
    cli_zip "$bin_dir/dbento.exe" "$version" "$target" "$out" "${files[@]}"
  fi
  rm -rf -- "$out/.cargo-packager"
  if [[ $mode == unsigned && $os == windows ]]; then mark_unsigned "$out"; fi
  echo "release: $os artifacts in $out ($([[ $os == macos ]] && echo ad-hoc signed || echo "$mode"))"
}

# The stable name of release file NAME at VERSION, for /releases/latest/download links: the
# version and any -unsigned marker dropped. Prints nothing for a file that is not an artifact.
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
  echo "${out/-unsigned./.}"
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

usage() {
  echo "usage: release.sh guard TYPE NAME DRY [PLATFORMS] | build | package | stable DIR VERSION | sums DIR" >&2
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
    stable)
      (($# == 3)) || usage
      stable_copies "$2" "$3"
      ;;
    sums)
      (($# == 2)) || usage
      write_sums "$2"
      ;;
    *) usage ;;
  esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then main "$@"; fi
