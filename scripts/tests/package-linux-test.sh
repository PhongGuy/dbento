#!/usr/bin/env bash
# Tests for the functions in scripts/package-linux.sh.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../package-linux.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
FAILS=0
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

test_tools_manifest_pins_three_fixed_tools_per_arch() {
  local arch name sum url n_x86=0 n_arm=0
  while read -r arch name sum url || [[ -n ${arch:-} ]]; do
    [[ -z ${arch:-} || $arch == \#* ]] && continue
    [[ $sum =~ ^[0-9a-f]{64}$ ]] || fail "$name: bad sha256 '$sum'"
    [[ $url == https://github.com/*/releases/download/* ]] || fail "$name: url not a release asset: $url"
    [[ $url != *continuous* ]] || fail "$name: continuous url $url"
    case $arch in x86_64) n_x86=$((n_x86 + 1)) ;; aarch64) n_arm=$((n_arm + 1)) ;; *) fail "unknown arch $arch" ;; esac
  done <"$ROOT/scripts/appimage-tools.txt"
  [[ $n_x86 -eq 3 && $n_arm -eq 3 ]] || fail "tools per arch: x86_64 $n_x86, aarch64 $n_arm, want 3 each"
}

test_tools_cache_dir_is_dbento_scoped() {
  [[ $(XDG_CACHE_HOME=/c tools_cache_dir) == /c/dbento/appimage-tools ]] || fail "cache dir $(XDG_CACHE_HOME=/c tools_cache_dir)"
}

test_seed_tools_installs_verified_files_and_restores_patched_ones() {
  export XDG_CACHE_HOME=$WORK/cache-ok
  printf 'tool bytes\n' >"$WORK/tool"
  local sum; sum=$(sha256sum "$WORK/tool" | cut -d' ' -f1)
  cat >"$WORK/manifest" <<EOF
# comment line
x86_64 linuxdeploy-x86_64.AppImage $sum file://$WORK/tool
aarch64 other-arch.AppImage $sum file://$WORK/missing
EOF
  seed_tools "$WORK/manifest" x86_64 || fail "seed_tools returned non-zero"
  local dest=$XDG_CACHE_HOME/.cargo-packager/AppImage/linuxdeploy-x86_64.AppImage
  [[ -f $dest ]] || fail "tool not installed"
  [[ $(stat -c %a "$dest") == 764 ]] || fail "mode is $(stat -c %a "$dest"), want 764"
  [[ ! -e $XDG_CACHE_HOME/.cargo-packager/AppImage/other-arch.AppImage ]] || fail "other arch installed"
  printf 'XXX' | dd of="$dest" bs=1 seek=8 conv=notrunc status=none
  rm "$WORK/tool"
  seed_tools "$WORK/manifest" x86_64 || fail "second seed_tools returned non-zero"
  cmp -s "$dest" <(printf 'tool bytes\n') || fail "patched tool not restored from the cache"
  unset XDG_CACHE_HOME
}

test_seed_tools_rejects_a_hash_mismatch() {
  export XDG_CACHE_HOME=$WORK/cache-bad
  printf 'tampered\n' >"$WORK/bad"
  echo "x86_64 linuxdeploy-x86_64.AppImage $(printf '0%.0s' {1..64}) file://$WORK/bad" >"$WORK/manifest-bad"
  local err
  if err=$(seed_tools "$WORK/manifest-bad" x86_64 2>&1); then fail "mismatch accepted"; fi
  [[ $err == *linuxdeploy-x86_64.AppImage* ]] || fail "mismatch does not name the tool: $err"
  [[ ! -e $XDG_CACHE_HOME/.cargo-packager/AppImage/linuxdeploy-x86_64.AppImage ]] || fail "bad tool installed"
  [[ -z $(find "$XDG_CACHE_HOME/dbento" -type f) ]] || fail "bad download left in the cache"
  unset XDG_CACHE_HOME
}

test_seed_tools_reads_a_final_line_without_a_newline() {
  export XDG_CACHE_HOME=$WORK/cache-nonl
  printf 'last tool\n' >"$WORK/tool-nonl"
  local sum; sum=$(sha256sum "$WORK/tool-nonl" | cut -d' ' -f1)
  printf 'x86_64 AppRun-x86_64 %s file://%s' "$sum" "$WORK/tool-nonl" >"$WORK/manifest-nonl"
  seed_tools "$WORK/manifest-nonl" x86_64 || fail "seed_tools returned non-zero"
  [[ -f $XDG_CACHE_HOME/.cargo-packager/AppImage/AppRun-x86_64 ]] || fail "final line without a newline skipped"
  unset XDG_CACHE_HOME
}

test_cli_archive_is_reproducible_and_complete() {
  printf '#!/bin/sh\necho dbento 9.9.9\n' >"$WORK/dbento-bin"
  printf 'mit\n' >"$WORK/LICENSE-MIT"
  printf 'apache\n' >"$WORK/LICENSE-APACHE"
  printf 'readme\n' >"$WORK/README.md"
  printf '<html></html>\n' >"$WORK/THIRD-PARTY-NOTICES.html"
  local files=("$WORK/LICENSE-MIT" "$WORK/LICENSE-APACHE" "$WORK/README.md" "$WORK/THIRD-PARTY-NOTICES.html")
  cli_archive "$WORK/dbento-bin" 9.9.9 x86_64-unknown-linux-gnu "$WORK/out1" "${files[@]}"
  touch "$WORK/README.md"
  sleep 1
  cli_archive "$WORK/dbento-bin" 9.9.9 x86_64-unknown-linux-gnu "$WORK/out2" "${files[@]}"
  local a=$WORK/out1/dbento-9.9.9-x86_64-unknown-linux-gnu.tar.gz
  cmp -s "$a" "$WORK/out2/dbento-9.9.9-x86_64-unknown-linux-gnu.tar.gz" || fail "archive not reproducible"
  local listing; listing=$(tar -tzf "$a" | LC_ALL=C sort | tr '\n' ' ')
  local d=dbento-9.9.9-x86_64-unknown-linux-gnu
  local want="$d/ $d/LICENSE-APACHE $d/LICENSE-MIT $d/README.md $d/THIRD-PARTY-NOTICES.html $d/dbento "
  [[ $listing == "$want" ]] || fail "archive listing: $listing"
  # Listed once into a variable: grep -q quits early, and under pipefail tar's SIGPIPE fails the check.
  local long; long=$(tar -tvzf "$a")
  grep -q '^-rwxr-xr-x 0/0 .*/dbento$' <<<"$long" || fail "binary not executable or not owned by 0/0"
  grep -q '^-rw-r--r-- 0/0 .*/THIRD-PARTY-NOTICES.html$' <<<"$long" || fail "notices mode or owner wrong"
}

test_cli_archive_uses_the_tar_override() {
  printf '#!/usr/bin/env bash\ntouch "%s/tar-used"\nexec tar "$@"\n' "$WORK" >"$WORK/fake-tar"
  chmod +x "$WORK/fake-tar"
  printf 'bin\n' >"$WORK/bin-t"
  TAR=$WORK/fake-tar cli_archive "$WORK/bin-t" 1.0.0 aarch64-apple-darwin "$WORK/tout"
  [[ -f $WORK/tar-used ]] || fail "TAR override ignored"
  [[ -f $WORK/tout/dbento-1.0.0-aarch64-apple-darwin.tar.gz ]] || fail "archive not written"
}

test_prepare_out_dir_refuses_unsafe_directories() {
  local root=$WORK/repo
  mkdir -p "$root/home" "$WORK/outside"
  local bad
  for bad in "" / "$root" "$root/" "$root/home" "$WORK/outside" "$root/../outside" "$root/x/../.."; do
    if HOME=$root/home prepare_out_dir "$root" "$bad" 2>/dev/null; then fail "accepted '$bad'"; fi
  done
  [[ -d $WORK/outside && -d $root/home ]] || fail "a refused directory was touched"
  local msg; msg=$(HOME=$root/home prepare_out_dir "$root" "$root/x/../.." 2>&1) || true
  [[ $msg == *"$WORK"* && $msg != *x/../..* && $msg == *OUT_DIR* ]] || fail "refusal does not name the resolved path and OUT_DIR: $msg"
}

test_prepare_out_dir_removes_only_known_artifacts() {
  local root=$WORK/repo2 out=$WORK/repo2/target/dist/linux
  mkdir -p "$out/.cargo-packager/x" "$out/Dbento.app/Contents"
  touch "$out/THIRD-PARTY-NOTICES.html" "$out/SHA256SUMS" "$out/dbento-1-x.tar.gz" "$out/dbento-app_1_amd64.deb" \
    "$out/dbento-app_1_x86_64.AppImage" "$out/Dbento_1_aarch64.dmg" "$out/dbento-app_1_x64-setup.exe" \
    "$out/dbento-1-x86_64-pc-windows-msvc.zip" "$out/keep.txt" "$out/apinata-1-x.tar.gz"
  HOME=$WORK prepare_out_dir "$root" "$out" || fail "valid directory refused"
  [[ $(ls -A "$out" | LC_ALL=C sort | tr '\n' ' ') == "apinata-1-x.tar.gz keep.txt " ]] || fail "left: $(ls -A "$out" | tr '\n' ' ')"
  HOME=$WORK prepare_out_dir "$root" "$root/target/dist/new" || fail "new directory refused"
  [[ -d $root/target/dist/new ]] || fail "new directory not created"
}

# A stand-in cargo: `xtask packager-config` logs its arguments and writes --output;
# `packager --config F` records F's mode, its directory's mode, the umask and the formats,
# and fails when FAKE_PACKAGER_FAIL is set.
write_fake_cargo() {
  cat >"$WORK/fake-cargo" <<'EOF'
#!/usr/bin/env bash
case $1 in
  xtask)
    echo "$*" >"$FAKE_LOG.xtask"
    while [[ $# -gt 0 ]]; do [[ $1 == --output ]] && { printf '{"secret":1}' >"$2"; }; shift; done ;;
  packager)
    cfg=$3
    echo "$cfg" >"$FAKE_LOG"
    stat -c %a "$cfg" >>"$FAKE_LOG"
    stat -c %a "$(dirname "$cfg")" >>"$FAKE_LOG"
    umask >>"$FAKE_LOG"
    echo "$5" >>"$FAKE_LOG"
    echo "${NO_STRIP:-unset}" >>"$FAKE_LOG"
    [[ -z ${FAKE_PACKAGER_FAIL:-} ]] ;;
esac
EOF
  chmod +x "$WORK/fake-cargo"
}

test_package_app_removes_the_config_on_success_and_failure() {
  write_fake_cargo
  export PACKAGE_LINUX_CARGO=$WORK/fake-cargo FAKE_LOG=$WORK/fake.log
  package_app "$WORK/o" || fail "package_app failed with a passing packager"
  local cfg; cfg=$(sed -n 1p "$FAKE_LOG")
  [[ $(sed -n 2p "$FAKE_LOG") == 600 ]] || fail "config mode $(sed -n 2p "$FAKE_LOG"), want 600"
  [[ $(sed -n 3p "$FAKE_LOG") == 700 ]] || fail "config dir mode $(sed -n 3p "$FAKE_LOG"), want 700"
  [[ $(sed -n 4p "$FAKE_LOG") == "$(umask)" ]] || fail "packager ran with umask $(sed -n 4p "$FAKE_LOG")"
  [[ $(sed -n 6p "$FAKE_LOG") == 1 ]] || fail "NO_STRIP is $(sed -n 6p "$FAKE_LOG"), want 1"
  [[ ! -e $cfg && ! -e $(dirname "$cfg") ]] || fail "config left after success"
  [[ $cfg != "$PWD"/* ]] || fail "config written under the working directory"
  [[ $(cat "$FAKE_LOG.xtask") == *"--out-dir $WORK/o "*"--notices $WORK/o/THIRD-PARTY-NOTICES.html"* ]] || fail "xtask args: $(cat "$FAKE_LOG.xtask")"

  if FAKE_PACKAGER_FAIL=1 package_app "$WORK/o"; then fail "packager failure not reported"; fi
  cfg=$(sed -n 1p "$FAKE_LOG")
  [[ ! -e $cfg && ! -e $(dirname "$cfg") ]] || fail "config left after failure"
  unset PACKAGE_LINUX_CARGO FAKE_LOG
}

test_package_app_passes_the_requested_formats() {
  write_fake_cargo
  export PACKAGE_LINUX_CARGO=$WORK/fake-cargo FAKE_LOG=$WORK/fake-formats.log
  package_app "$WORK/o" || fail "package_app failed"
  [[ $(sed -n 5p "$FAKE_LOG") == deb,appimage ]] || fail "default formats: $(sed -n 5p "$FAKE_LOG")"
  package_app "$WORK/o" dmg || fail "package_app dmg failed"
  [[ $(sed -n 5p "$FAKE_LOG") == dmg ]] || fail "dmg formats: $(sed -n 5p "$FAKE_LOG")"
  unset PACKAGE_LINUX_CARGO FAKE_LOG
}

test_deb_arch_maps_machines_to_debian_names() {
  [[ $(deb_arch x86_64) == amd64 ]] || fail "x86_64 gave $(deb_arch x86_64)"
  [[ $(deb_arch aarch64) == arm64 ]] || fail "aarch64 gave $(deb_arch aarch64)"
  if deb_arch riscv64 2>/dev/null; then fail "riscv64 accepted"; fi
}

DEPENDS="libxcb1, libxkbcommon0, libxkbcommon-x11-0, libvulkan1, libwayland-client0, libwayland-egl1, libegl1, libstdc++6"

test_a_failed_cli_archive_leaves_no_stage_or_partial_tarball() {
  printf '#!/usr/bin/env bash\necho partial\nexit 2\n' >"$WORK/broken-tar"
  chmod +x "$WORK/broken-tar"
  printf 'bin\n' >"$WORK/bin-b"
  mkdir -p "$WORK/archive-tmp"
  if TMPDIR=$WORK/archive-tmp TAR=$WORK/broken-tar cli_archive "$WORK/bin-b" 1.0.0 x86_64-unknown-linux-gnu "$WORK/bout"; then
    fail "cli_archive passed after tar failed"
  fi
  [[ -z $(ls -A "$WORK/archive-tmp") ]] || fail "stage dir left: $(ls -A "$WORK/archive-tmp")"
  [[ ! -e $WORK/bout/dbento-1.0.0-x86_64-unknown-linux-gnu.tar.gz ]] || fail "partial tarball left"
}

test_missing_depends_reports_only_undeclared_libraries() {
  local all="libc.so.6 libm.so.6 libgcc_s.so.1 libstdc++.so.6 ld-linux-x86-64.so.2 libxcb.so.1 libxkbcommon.so.0 libxkbcommon-x11.so.0 libvulkan.so.1 libwayland-client.so.0 libwayland-egl.so.1 libEGL.so.1"
  [[ -z $(missing_depends "$all" "$DEPENDS") ]] || fail "declared set reported: $(missing_depends "$all" "$DEPENDS")"
  [[ -z $(missing_depends "ld-linux-aarch64.so.1" "$DEPENDS") ]] || fail "aarch64 loader reported: $(missing_depends "ld-linux-aarch64.so.1" "$DEPENDS")"
  [[ $(missing_depends "libEGL.so.1" "libxcb1, libvulkan1") == "libEGL.so.1 (libegl1)" ]] || fail "undeclared libegl1 not reported"
  [[ $(missing_depends "libfoo.so.3" "$DEPENDS") == "libfoo.so.3 (unknown)" ]] || fail "unknown soname not reported"
  [[ $(missing_depends "libxcb.so.1" "libxcb1-dev") == "libxcb.so.1 (libxcb1)" ]] || fail "prefix match accepted"
}

test_binary_sonames_reads_a_real_elf() {
  local got; got=$(binary_sonames /bin/ls)
  grep -qx 'libc.so.6' <<<"$got" || fail "libc.so.6 not among /bin/ls sonames: $got"
  [[ $got == "$(LC_ALL=C sort -u <<<"$got")" ]] || fail "sonames not sorted and unique"
}

test_binary_sonames_fails_when_readelf_cannot_read_the_file() {
  printf 'not an elf\n' >"$WORK/not-elf"
  if binary_sonames "$WORK/not-elf" >/dev/null 2>&1; then fail "binary_sonames passed on a file readelf rejects"; fi
}

test_desktop_entry_ok_needs_the_class_and_icon() {
  printf '[Desktop Entry]\nIcon=dbento-app\nStartupWMClass=dbento-app\n' >"$WORK/good.desktop"
  printf '[Desktop Entry]\nIcon=dbento-app\n' >"$WORK/noclass.desktop"
  printf '[Desktop Entry]\nIcon=dbento\nStartupWMClass=dbento-app\n' >"$WORK/badicon.desktop"
  desktop_entry_ok "$WORK/good.desktop" dbento-app || fail "good entry rejected"
  local err
  if err=$(desktop_entry_ok "$WORK/noclass.desktop" dbento-app 2>&1); then fail "entry without class passed"; fi
  [[ $err == *"lacks StartupWMClass=dbento-app"* ]] || fail "error was: $err"
  if desktop_entry_ok "$WORK/badicon.desktop" dbento-app 2>/dev/null; then fail "old Icon=dbento passed"; fi
  if desktop_entry_ok "$WORK/missing.desktop" dbento-app 2>/dev/null; then fail "missing file passed"; fi
}

# A data tree holding every file the deb must ship.
full_deb_tree() {
  local d=$1 f
  for f in usr/bin/dbento usr/bin/dbento-app usr/share/applications/dbento-app.desktop \
    usr/share/icons/hicolor/256x256/apps/dbento-app.png usr/lib/dbento-app/Inter-OFL.txt \
    usr/lib/dbento-app/JetBrainsMono-OFL.txt usr/lib/dbento-app/Lucide-ISC.txt \
    usr/lib/dbento-app/THIRD-PARTY-NOTICES.html; do
    mkdir -p "$d/$(dirname "$f")"
    printf x >"$d/$f"
  done
}

test_deb_layout_missing_names_each_absent_file() {
  full_deb_tree "$WORK/layout"
  [[ -z $(deb_layout_missing "$WORK/layout") ]] || fail "full tree reported: $(deb_layout_missing "$WORK/layout")"
  rm "$WORK/layout/usr/lib/dbento-app/THIRD-PARTY-NOTICES.html" "$WORK/layout/usr/bin/dbento"
  [[ $(deb_layout_missing "$WORK/layout" | tr '\n' ' ') == "usr/bin/dbento usr/lib/dbento-app/THIRD-PARTY-NOTICES.html " ]] \
    || fail "missing list: $(deb_layout_missing "$WORK/layout" | tr '\n' ' ')"
}

test_unpack_deb_reads_data_and_control() {
  local d=$WORK/fakedeb
  full_deb_tree "$d/data"
  mkdir -p "$d/ctl"
  printf 'Package: dbento\nVersion: 0.1.0\nDepends: %s\n' "$DEPENDS" >"$d/ctl/control"
  tar -C "$d/data" -czf "$d/data.tar.gz" .
  tar -C "$d/ctl" -czf "$d/control.tar.gz" .
  printf '2.0\n' >"$d/debian-binary"
  (cd "$d" && ar rc pkg.deb debian-binary control.tar.gz data.tar.gz)
  unpack_deb "$d/pkg.deb" "$WORK/unpacked" || fail "unpack_deb failed"
  [[ -z $(deb_layout_missing "$WORK/unpacked/data") ]] || fail "unpacked tree lacks: $(deb_layout_missing "$WORK/unpacked/data")"
  [[ $(deb_depends "$WORK/unpacked") == "$DEPENDS" ]] || fail "Depends: $(deb_depends "$WORK/unpacked")"
  printf 'not an ar archive\n' >"$WORK/broken.deb"
  if unpack_deb "$WORK/broken.deb" "$WORK/broken" 2>/dev/null; then fail "broken deb unpacked"; fi
}

test_write_sums_covers_every_platform_artifact() {
  local d=$WORK/sums
  mkdir -p "$d/dist-x86_64-pc-windows-msvc"
  printf a >"$d/dbento-1-x.tar.gz"; printf b >"$d/dbento-app_1_amd64.deb"
  printf c >"$d/dbento-app_1_x86_64.AppImage"; printf d >"$d/Dbento_1_aarch64.dmg"
  printf e >"$d/dist-x86_64-pc-windows-msvc/dbento-app_1_x64-setup.exe"
  printf f >"$d/dist-x86_64-pc-windows-msvc/dbento-1-x86_64-pc-windows-msvc.zip"
  printf g >"$d/notes.txt"; printf h >"$d/THIRD-PARTY-NOTICES.html"
  write_sums "$d" || fail "write_sums failed"
  [[ $(cut -d' ' -f3 "$d/SHA256SUMS" | tr '\n' ' ') == "Dbento_1_aarch64.dmg dbento-1-x.tar.gz dbento-app_1_amd64.deb dbento-app_1_x86_64.AppImage dist-x86_64-pc-windows-msvc/dbento-1-x86_64-pc-windows-msvc.zip dist-x86_64-pc-windows-msvc/dbento-app_1_x64-setup.exe " ]] \
    || fail "SHA256SUMS names: $(cut -d' ' -f3 "$d/SHA256SUMS" | tr '\n' ' ')"
  (cd "$d" && sha256sum -c --quiet SHA256SUMS) || fail "SHA256SUMS does not verify"
}

test_write_sums_fails_without_artifacts() {
  local d=$WORK/nosums
  mkdir -p "$d"; printf x >"$d/notes.txt"
  if write_sums "$d" 2>/dev/null; then fail "empty directory accepted"; fi
  [[ ! -e $d/SHA256SUMS ]] || fail "SHA256SUMS left behind"
}

test_expect_version_names_the_program_that_failed() {
  printf '#!/bin/sh\necho "dbento-app 0.1.0"\n' >"$WORK/good-app"
  printf '#!/bin/sh\nexit 1\n' >"$WORK/bad-app"
  printf '#!/bin/sh\necho "dbento-app 9.9.9"\n' >"$WORK/old-app"
  chmod +x "$WORK/good-app" "$WORK/bad-app" "$WORK/old-app"
  expect_version dbento-app 0.1.0 env APPIMAGE_EXTRACT_AND_RUN=1 "$WORK/good-app" || fail "good version rejected"
  local err
  if err=$(expect_version dbento-app 0.1.0 env APPIMAGE_EXTRACT_AND_RUN=1 "$WORK/bad-app" 2>&1); then fail "failing program passed"; fi
  [[ $err == "package-linux: verify: $WORK/bad-app --version failed" ]] || fail "error was: $err"
  if err=$(expect_version dbento-app 0.1.0 "$WORK/old-app" 2>&1); then fail "wrong version passed"; fi
  [[ $err == *"$WORK/old-app --version printed 'dbento-app 9.9.9', want 'dbento-app 0.1.0'"* ]] || fail "error was: $err"
}

test_source_date_epoch_prefers_a_valid_environment_value() {
  [[ $(SOURCE_DATE_EPOCH=1700000000 source_date_epoch "$WORK/nowhere") == 1700000000 ]] \
    || fail "env epoch not used"
  if (SOURCE_DATE_EPOCH=yesterday source_date_epoch "$WORK/nowhere") >/dev/null 2>"$WORK/sde.err"; then
    fail "a non-numeric SOURCE_DATE_EPOCH was accepted"
  fi
  grep -q SOURCE_DATE_EPOCH "$WORK/sde.err" || fail "invalid epoch error does not name the variable"
}

test_source_date_epoch_falls_back_to_the_last_commit() {
  local repo=$WORK/sde-repo
  git init -q "$repo"
  # CI runners have no git identity; a configured one is kept so local commit hooks accept it.
  (
    if ! git config user.email >/dev/null; then
      export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.invalid
      export GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.invalid
    fi
    GIT_COMMITTER_DATE=@1600000000 GIT_AUTHOR_DATE=@1600000000 \
      git -C "$repo" commit -q --allow-empty -m "test: fixture"
  ) || fail "cannot commit the fixture"
  [[ $(SOURCE_DATE_EPOCH='' source_date_epoch "$repo") == 1600000000 ]] || fail "commit date not used"
}

# An empty epoch reaches clang and crashes pg_query's bindgen, so a missing date must stop the build.
test_source_date_epoch_fails_loudly_without_a_commit_date() {
  mkdir -p "$WORK/not-a-repo"
  if (SOURCE_DATE_EPOCH='' GIT_CEILING_DIRECTORIES=$WORK source_date_epoch "$WORK/not-a-repo") >/dev/null 2>"$WORK/sde.err"; then
    fail "an empty epoch outside a git checkout was accepted"
  fi
  grep -q SOURCE_DATE_EPOCH "$WORK/sde.err" || fail "missing-date error does not name SOURCE_DATE_EPOCH"
}

for t in $(declare -F | awk '{print $3}' | grep '^test_'); do "$t"; done
if ((FAILS > 0)); then echo "$FAILS failure(s)" >&2; exit 1; fi
echo "package-linux tests passed"
