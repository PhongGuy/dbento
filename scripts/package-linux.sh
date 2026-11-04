#!/usr/bin/env bash
# Builds the Linux release artifacts: a CLI tarball, a .deb and an AppImage, each
# carrying THIRD-PARTY-NOTICES.html; the release scripts reuse its helpers for other
# platforms. Sourcing this file only defines functions; running it calls main.
set -euo pipefail

# The cache that holds verified tool downloads, keyed by their sha256.
tools_cache_dir() {
  echo "${XDG_CACHE_HOME:-$HOME/.cache}/dbento/appimage-tools"
}

# Seed cargo-packager's AppImage tools dir with the pinned tools for ARCH.
# Each tool is downloaded once into a hash-keyed cache, then reinstalled on
# every run, because cargo-packager patches linuxdeploy in place.
seed_tools() {
  local manifest=$1 arch=$2
  local cache dest_dir
  cache=$(tools_cache_dir)
  dest_dir="${XDG_CACHE_HOME:-$HOME/.cache}/.cargo-packager/AppImage"
  mkdir -p "$cache" "$dest_dir"
  local line_arch name sum url
  # The `|| -n` clause keeps a final line that lacks a trailing newline.
  while read -r line_arch name sum url || [[ -n ${line_arch:-} ]]; do
    [[ -z ${line_arch:-} || $line_arch == \#* || $line_arch != "$arch" ]] && continue
    local cached=$cache/$sum
    if [[ ! -f $cached ]]; then
      curl -sSLf -A 'dbento-package-linux' -o "$cached.part" "$url" || {
        rm -f "$cached.part"
        echo "download failed for $name: $url" >&2
        return 1
      }
      local got
      got=$(sha256sum "$cached.part" | cut -d' ' -f1)
      if [[ $got != "$sum" ]]; then
        rm -f "$cached.part"
        echo "sha256 mismatch for $name: got $got, want $sum" >&2
        return 1
      fi
      mv "$cached.part" "$cached"
    fi
    install -m 0764 "$cached" "$dest_dir/$name"
  done <"$manifest"
}

# Write OUT/dbento-VERSION-TARGET.tar.gz holding the binary and FILEs.
# Staging with install and fixing tar's order, owner and mtime makes the
# archive byte-reproducible for a given SOURCE_DATE_EPOCH. On any failure the
# stage dir and the partial tarball are removed and the call fails.
cli_archive() {
  local binary=$1 version=$2 target=$3 out=$4
  shift 4
  local name=dbento-$version-$target
  (
    stage=$(mktemp -d) || exit 1
    trap 'rm -rf -- "$stage"' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    install -d -m 0755 "$stage/$name" || exit 1
    install -m 0755 "$binary" "$stage/$name/dbento" || exit 1
    for f in "$@"; do
      install -m 0644 "$f" "$stage/$name/$(basename "$f")" || exit 1
    done
    mkdir -p "$out" || exit 1
    if ! "${TAR:-tar}" -C "$stage" --sort=name --owner=0 --group=0 --numeric-owner \
      --mtime="@${SOURCE_DATE_EPOCH:-0}" -cf - "$name" | gzip -n -9 >"$out/$name.tar.gz"; then
      rm -f -- "$out/$name.tar.gz"
      exit 1
    fi
  )
}

# Validate OUT and empty it of earlier artifacts only. OUT must be a dedicated directory
# strictly inside ROOT that is neither ROOT nor HOME; nothing else is ever deleted.
prepare_out_dir() {
  local root=$1 out=$2
  if [[ -z $out ]]; then echo "package-linux: empty output directory (OUT_DIR)" >&2; return 1; fi
  local r o h
  r=$(realpath -m -- "$root"); o=$(realpath -m -- "$out"); h=$(realpath -m -- "${HOME:-/}")
  if [[ $o == / || $o == "$r" || $o == "$h" || $o != "$r"/* ]]; then
    echo "package-linux: refusing output directory $o (OUT_DIR must lie strictly inside $r)" >&2
    return 1
  fi
  mkdir -p -- "$o"
  rm -rf -- "$o/.cargo-packager" "$o/Dbento.app"
  rm -f -- "$o/THIRD-PARTY-NOTICES.html" "$o/SHA256SUMS" "$o"/dbento-*.tar.gz "$o"/dbento-*.zip \
    "$o"/dbento-app_*.deb "$o"/dbento-app_*.AppImage "$o"/dbento-app_*.exe "$o"/Dbento_*.dmg
}

# Generate the packager config, which may hold signing values, into a private temporary
# directory and run cargo-packager on it for FORMATS (default deb,appimage); the directory
# is removed on every exit path. The notices are expected loose in OUT.
package_app() {
  local out=$1 formats=${2:-deb,appimage} cargo=${PACKAGE_LINUX_CARGO:-${CARGO:-cargo}} saved_umask
  saved_umask=$(umask)
  (
    umask 077
    dir=$(mktemp -d) || exit 1
    trap 'rm -rf -- "$dir"' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    chmod 700 "$dir"
    "$cargo" xtask packager-config --out-dir "$out" --output "$dir/packager-config.json" \
      --notices "$out/THIRD-PARTY-NOTICES.html" || exit 1
    umask "$saved_umask"
    # linuxdeploy's bundled strip cannot read newer ELF sections, so stripping is left to cargo.
    NO_STRIP=1 "$cargo" packager --config "$dir/packager-config.json" --formats "$formats" || exit 1
  )
}

# The Debian architecture name for a `uname -m` MACHINE; unknown machines fail.
deb_arch() {
  case $1 in
    x86_64) echo amd64 ;;
    aarch64) echo arm64 ;;
    *) echo "package-linux: no Debian architecture for $1" >&2; return 1 ;;
  esac
}

# Map a soname to the Debian package that provides it: one of the declared
# Depends, "essential" for the C runtime (aarch64 loader is .so.1), or "unknown".
soname_package() {
  case $1 in
    libxcb.so.1) echo libxcb1 ;;
    libxkbcommon.so.0) echo libxkbcommon0 ;;
    libxkbcommon-x11.so.0) echo libxkbcommon-x11-0 ;;
    libvulkan.so.1) echo libvulkan1 ;;
    libwayland-client.so.0) echo libwayland-client0 ;;
    libwayland-egl.so.1) echo libwayland-egl1 ;;
    libEGL.so.1) echo libegl1 ;;
    libc.so.6 | libm.so.6 | libgcc_s.so.1 | ld-linux-*.so.[12]) echo essential ;;
    *) echo unknown ;;
  esac
}

# Print "SONAME (PACKAGE)" for each soname in the space-separated SONAMES
# whose package is unknown or not an exact entry of the comma-separated DEPENDS.
missing_depends() {
  local sonames=$1 depends=$2
  local -A declared=()
  local entry entries
  IFS=',' read -ra entries <<<"$depends"
  for entry in "${entries[@]}"; do
    entry=${entry//[[:space:]]/}
    [[ -n $entry ]] && declared[$entry]=1
  done
  local so pkg
  for so in $sonames; do
    pkg=$(soname_package "$so")
    [[ $pkg == essential ]] && continue
    if [[ $pkg == unknown || -z ${declared[$pkg]:-} ]]; then
      echo "$so ($pkg)"
    fi
  done
}

# The sorted sonames an ELF loads: its NEEDED entries plus the ones it names
# as strings for dlopen, as wgpu and winit do.
binary_sonames() {
  local elf=$1
  # The group runs in a pipeline subshell, so a readelf failure exits it and pipefail carries
  # the failure out; only the dlopen string search may come up empty.
  {
    readelf -d "$elf" | sed -n 's/.*Shared library: \[\(.*\)\].*/\1/p' || exit 1
    strings "$elf" | grep -oE 'lib[A-Za-z0-9_+-]+\.so\.[0-9]+' || true
  } | LC_ALL=C sort -u
}

# Succeed when the desktop FILE names APP_ID as both its window class and its icon, so the
# shell can match the running window to the entry; otherwise name the missing line.
desktop_entry_ok() {
  local file=$1 app_id=$2 line
  for line in "StartupWMClass=$app_id" "Icon=$app_id"; do
    if ! grep -qxF -- "$line" "$file" 2>/dev/null; then
      echo "package-linux: verify: $file lacks $line" >&2
      return 1
    fi
  done
}

# Print each file the .deb must ship that is absent from its unpacked DATA_DIR.
deb_layout_missing() {
  local data=$1 f
  for f in usr/bin/dbento usr/bin/dbento-app usr/share/applications/dbento-app.desktop \
    usr/share/icons/hicolor/256x256/apps/dbento-app.png usr/lib/dbento-app/Inter-OFL.txt \
    usr/lib/dbento-app/JetBrainsMono-OFL.txt usr/lib/dbento-app/Lucide-ISC.txt \
    usr/lib/dbento-app/THIRD-PARTY-NOTICES.html; do
    [[ -f $data/$f ]] || echo "$f"
  done
}

# Unpack DEB into DIR/data and DIR/control. A .deb is an ar archive of control and
# data tarballs, so no dpkg-deb is needed.
unpack_deb() {
  local deb dir=$2
  deb=$(realpath -- "$1") || return 1
  mkdir -p "$dir/data" "$dir/control" || return 1
  (cd "$dir" && ar x "$deb") || return 1
  local data control
  data=$(compgen -G "$dir/data.tar.*" | head -n1) || return 1
  control=$(compgen -G "$dir/control.tar.*" | head -n1) || return 1
  tar -C "$dir/data" -xf "$data" || return 1
  tar -C "$dir/control" -xf "$control" || return 1
}

# The Depends: value of the control file unpacked under DIR.
deb_depends() {
  sed -n 's/^Depends: //p' "$1/control/control"
}

# Write DIR/SHA256SUMS over every release artifact under DIR, named relative to DIR in
# byte order; with none found, fail and leave no SHA256SUMS behind.
write_sums() {
  local dir=$1
  (
    cd "$dir" || exit 1
    find . -type f \( -name '*.tar.gz' -o -name '*.deb' -o -name '*.AppImage' \
      -o -name '*.dmg' -o -name '*.exe' -o -name '*.zip' \) \
      -printf '%P\n' | LC_ALL=C sort | xargs -r -d '\n' sha256sum >SHA256SUMS || exit 1
    if [[ ! -s SHA256SUMS ]]; then
      rm -f SHA256SUMS
      echo "package-linux: no release artifacts under $dir to checksum" >&2
      exit 1
    fi
  )
}

# Run COMMAND... --version without a display and require it to print "NAME VERSION". The
# program named in a failure is the command's last word, so an `env ...` wrapper is skipped.
expect_version() {
  local name=$1 version=$2 got
  shift 2
  local program=${*: -1}
  got=$(env -u DISPLAY -u WAYLAND_DISPLAY "$@" --version) || {
    echo "package-linux: verify: $program --version failed" >&2
    return 1
  }
  if [[ $got != "$name $version" ]]; then
    echo "package-linux: verify: $program --version printed '$got', want '$name $version'" >&2
    return 1
  fi
}

# Check that the .deb, AppImage and tarball in OUT hold working binaries of VERSION, ship
# the notices, and that the .deb declares every library the app loads. Any failure is fatal.
verify_packages() {
  local out=$1 version=$2 target=$3
  (
    work=$(mktemp -d)
    trap 'rm -rf -- "$work"' EXIT
    die() { echo "package-linux: verify: $*" >&2; exit 1; }

    local arch data missing
    arch=$(deb_arch "$(uname -m)") || die "unsupported machine $(uname -m)"
    unpack_deb "$out/dbento-app_${version}_$arch.deb" "$work/deb" || die "cannot unpack the .deb"
    data=$work/deb/data
    missing=$(deb_layout_missing "$data")
    [[ -z $missing ]] || die ".deb lacks: $(paste -sd' ' <<<"$missing")"
    desktop_entry_ok "$data/usr/share/applications/dbento-app.desktop" dbento-app ||
      die ".deb desktop entry is incomplete"
    expect_version dbento "$version" "$data/usr/bin/dbento" || exit 1
    expect_version dbento-app "$version" "$data/usr/bin/dbento-app" || exit 1
    local sonames
    sonames=$(binary_sonames "$data/usr/bin/dbento-app") || die "cannot read the libraries dbento-app loads"
    missing=$(missing_depends "$sonames" "$(deb_depends "$work/deb")")
    [[ -z $missing ]] || die "libraries missing from Depends: $missing"

    # Both AppImage modes unpack in user space, so no FUSE is needed.
    local appimage=$out/dbento-app_${version}_$(uname -m).AppImage root=$work/squashfs-root
    expect_version dbento-app "$version" env APPIMAGE_EXTRACT_AND_RUN=1 "$appimage" || exit 1
    (cd "$work" && "$appimage" --appimage-extract >/dev/null) || die "cannot extract the AppImage"
    desktop_entry_ok "$root/usr/share/applications/dbento-app.desktop" dbento-app ||
      die "AppImage desktop entry is incomplete"
    expect_version dbento "$version" "$root/usr/bin/dbento" || exit 1
    [[ -f $root/usr/lib/dbento-app/THIRD-PARTY-NOTICES.html ]] ||
      die "AppImage lacks usr/lib/dbento-app/THIRD-PARTY-NOTICES.html"

    local cli=$work/tar/dbento-$version-$target
    mkdir -p "$work/tar"
    tar -C "$work/tar" -xzf "$out/dbento-$version-$target.tar.gz" || die "cannot unpack the tarball"
    expect_version dbento "$version" "$cli/dbento" || exit 1
    [[ -f $cli/THIRD-PARTY-NOTICES.html ]] || die "tarball lacks THIRD-PARTY-NOTICES.html"
  )
}

# Print the build's SOURCE_DATE_EPOCH: the environment's when set, else ROOT's last commit
# date. Fail when neither gives a whole number, since an empty value crashes bindgen.
source_date_epoch() {
  local epoch=${SOURCE_DATE_EPOCH:-}
  if [[ -z $epoch ]]; then
    epoch=$(git -C "$1" log -1 --format=%ct 2>/dev/null) || epoch=
  fi
  if [[ ! $epoch =~ ^[0-9]+$ ]]; then
    echo "package-linux: no SOURCE_DATE_EPOCH (got '$epoch'); set it or package from a git checkout" >&2
    return 1
  fi
  echo "$epoch"
}

main() {
  local root cargo=${CARGO:-cargo}
  root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
  local want=0.11.8 got
  got=$("$cargo" packager --version 2>/dev/null) || got=none
  if [[ $got != "cargo-packager $want" ]]; then
    echo "package-linux: need cargo-packager $want (found: $got);" \
      "run: cargo install --locked cargo-packager --version $want" >&2
    return 1
  fi

  prepare_out_dir "$root" "${OUT_DIR:-$root/target/dist/linux}" || return 1
  local out
  out=$(realpath -m -- "${OUT_DIR:-$root/target/dist/linux}")
  cd "$root"

  # A bad tool pin or an unaccepted licence fails here, before the long release build;
  # the fetch is needed because cargo-about runs offline.
  seed_tools "$root/scripts/appimage-tools.txt" "$(uname -m)"
  local version target bin_dir
  version=$("$cargo" pkgid -p dbento-cli)
  version=${version##*[#@]}
  target=$(rustc -vV | sed -n 's/^host: //p')
  SOURCE_DATE_EPOCH=$(source_date_epoch "$root") || return 1
  export SOURCE_DATE_EPOCH
  bin_dir=${CARGO_TARGET_DIR:-$root/target}/release
  "$cargo" fetch --locked
  "$cargo" xtask notices --out "$out/THIRD-PARTY-NOTICES.html"

  "$cargo" build --release --locked -p dbento-cli -p dbento-app
  package_app "$out"
  cli_archive "$bin_dir/dbento" "$version" "$target" "$out" \
    "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$root/README.md" "$out/THIRD-PARTY-NOTICES.html"
  rm -rf -- "$out/.cargo-packager"
  verify_packages "$out" "$version" "$target"
  write_sums "$out"
  echo "package-linux: artifacts in $out"
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then main "$@"; fi
