#!/usr/bin/env bash
# Tests for the functions in scripts/release.sh.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../release.sh"
RELEASE_SH=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/release.sh

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
FAILS=0
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }

LINUX_ROWS='{"os":"ubuntu-24.04","target":"x86_64-unknown-linux-gnu"},{"os":"ubuntu-24.04-arm","target":"aarch64-unknown-linux-gnu"}'
WINDOWS_ROW='{"os":"windows-2025","target":"x86_64-pc-windows-msvc"}'
ARM_ROW='{"os":"macos-15","target":"aarch64-apple-darwin"}'
INTEL_ROW='{"os":"macos-15-intel","target":"x86_64-apple-darwin"}'

test_check_ref_accepts_only_the_matching_tag() {
  check_ref tag v1.2.3 1.2.3 false || fail "matching tag refused"
  if check_ref tag v1.2.4 1.2.3 false 2>/dev/null; then fail "mismatched tag accepted"; fi
  if check_ref tag 1.2.3 1.2.3 false 2>/dev/null; then fail "tag without v accepted"; fi
  check_ref tag v1.2.3 1.2.3 true || fail "matching tag refused on a dry run"
  if check_ref tag v0.0.0-test 1.2.3 true 2>/dev/null; then fail "mismatched tag accepted on a dry run"; fi
  local msg; msg=$(check_ref tag v9.9.9 1.2.3 false 2>&1) || true
  [[ $msg == "release: tag v9.9.9 does not match the workspace version v1.2.3" ]] || fail "mismatch message: $msg"
}

test_release_sh_is_executable() {
  # release.yml runs the script directly, so git must record the exec bit; outside a git
  # checkout (a source tarball) only the file mode is checked.
  [[ -x $RELEASE_SH ]] || fail "scripts/release.sh is not executable"
  local mode; mode=$(git -C "$(dirname "$RELEASE_SH")" ls-files -s release.sh 2>/dev/null | cut -d' ' -f1) || true
  [[ -z $mode || $mode == 100755 ]] || fail "git records release.sh as $mode, want 100755"
}

test_check_ref_allows_a_branch_only_on_a_dry_run() {
  check_ref branch main 1.2.3 true || fail "dry run from a branch refused"
  if check_ref branch main 1.2.3 false 2>/dev/null; then fail "release from a branch accepted"; fi
  if check_ref branch main 1.2.3 "" 2>/dev/null; then fail "empty dry_run treated as a dry run"; fi
  local msg; msg=$(check_ref branch main 1.2.3 false 2>&1) || true
  [[ $msg == *"can only start a dry run"* ]] || fail "branch message: $msg"
  if check_ref pull 1 1.2.3 true 2>/dev/null; then fail "unknown ref type accepted"; fi
}

test_select_targets_builds_the_matrix_in_a_fixed_order() {
  local all="{\"include\":[$LINUX_ROWS,$WINDOWS_ROW,$ARM_ROW,$INTEL_ROW]}"
  [[ $(select_targets "" false) == "$all" ]] || fail "empty, release: $(select_targets "" false)"
  [[ $(select_targets "" true) == "$all" ]] || fail "empty, dry: $(select_targets "" true)"
  [[ $(select_targets "macos-intel, linux" true) == "{\"include\":[$LINUX_ROWS,$INTEL_ROW]}" ]] \
    || fail "subset: $(select_targets "macos-intel, linux" true)"
  [[ $(select_targets windows true) == "{\"include\":[$WINDOWS_ROW]}" ]] || fail "windows: $(select_targets windows true)"
  [[ $(select_targets macos-arm64 true) == "{\"include\":[$ARM_ROW]}" ]] || fail "macos-arm64: $(select_targets macos-arm64 true)"
  [[ $(select_targets linux,windows,macos-arm64 true) == "{\"include\":[$LINUX_ROWS,$WINDOWS_ROW,$ARM_ROW]}" ]] \
    || fail "dispatch default: $(select_targets linux,windows,macos-arm64 true)"
  [[ $(select_targets macos-intel,macos-arm64,macos-arm64 true) == "{\"include\":[$ARM_ROW,$INTEL_ROW]}" ]] \
    || fail "repeated token: $(select_targets macos-intel,macos-arm64,macos-arm64 true)"
}

test_select_targets_refuses_unknown_and_partial_releases() {
  if select_targets "linux,beos" true 2>/dev/null; then fail "unknown platform accepted"; fi
  if select_targets "macos" true 2>/dev/null; then fail "the dropped macos token accepted"; fi
  if select_targets " , " true 2>/dev/null; then fail "a list with no tokens accepted"; fi
  if select_targets "linux" false 2>/dev/null; then fail "a release without every platform accepted"; fi
  if select_targets "linux,windows,macos-arm64" false 2>/dev/null; then fail "a release without macos-intel accepted"; fi
  select_targets "windows,macos-intel,macos-arm64,linux" false >/dev/null || fail "a full release refused"
}

# A token is matched as text: a * must not expand to the names of files in the current directory.
test_select_targets_never_glob_expands_a_token() {
  mkdir -p "$WORK/glob" && touch "$WORK/glob/linux"
  local msg
  if msg=$(cd "$WORK/glob" && select_targets '*' true 2>&1); then fail "a * token selected: $msg"; fi
  [[ $msg == "release: unknown platform '*'" ]] || fail "glob token message: $msg"
  [[ $(cd "$WORK/glob" && select_targets 'linux,,' true) == "{\"include\":[$LINUX_ROWS]}" ]] \
    || fail "empty tokens: $(cd "$WORK/glob" && select_targets 'linux,,' true)"
}

test_guard_prints_the_version_and_matrix() {
  local out
  out=$(bash "$RELEASE_SH" guard branch main true windows) || { fail "guard failed: $out"; return 0; }
  [[ $(sed -n 1p <<<"$out") =~ ^version=[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "guard version line: $out"
  [[ $(sed -n 2p <<<"$out") == "matrix={\"include\":[$WINDOWS_ROW]}" ]] || fail "guard matrix line: $out"
  if bash "$RELEASE_SH" guard tag v0.0.0-test false "" >/dev/null 2>&1; then fail "a mismatched tag passed the guard"; fi
  if bash "$RELEASE_SH" guard branch main false linux >/dev/null 2>&1; then fail "a branch release passed the guard"; fi
}

test_a_failed_pkgid_stops_the_guard() {
  # A cargo that cannot read the workspace must not yield an empty version.
  local fake=$WORK/pkgid-fail-bin out
  mkdir -p "$fake"
  printf '#!/usr/bin/env bash\nexit 101\n' >"$fake/cargo"
  chmod +x "$fake/cargo"
  if out=$(PATH=$fake:$PATH workspace_version 2>/dev/null); then fail "workspace_version succeeded after a failed pkgid: '$out'"; fi
  if out=$(PATH=$fake:$PATH bash "$RELEASE_SH" guard branch main true linux 2>/dev/null); then
    fail "guard succeeded after a failed pkgid: $out"
  fi
}

test_unknown_subcommands_print_usage() {
  local status=0
  bash "$RELEASE_SH" manifest dist 1 x v1 out >/dev/null 2>&1 || status=$?
  [[ $status == 2 ]] || fail "manifest exited $status, want 2"
  status=0
  bash "$RELEASE_SH" >/dev/null 2>&1 || status=$?
  [[ $status == 2 ]] || fail "no subcommand exited $status, want 2"
  if grep -nE 'latest\.json|update_m|APINATA|apinata|cli_raw' "$RELEASE_SH"; then fail "updater or Apinata leftovers in release.sh"; fi
}

test_sums_writes_checksums_for_a_directory() {
  mkdir -p "$WORK/s"
  printf a >"$WORK/s/dbento-1-x.tar.gz"
  bash "$RELEASE_SH" sums "$WORK/s" || fail "sums failed"
  (cd "$WORK/s" && sha256sum -c --quiet SHA256SUMS) || fail "SHA256SUMS does not verify"
}

# The layout a merged per-platform artifact download leaves: files one level down.
test_sums_hashes_files_in_platform_subdirectories() {
  local d=$WORK/nested
  mkdir -p "$d/linux" "$d/macos" "$d/windows"
  printf a >"$d/linux/dbento-1-x86_64-unknown-linux-gnu.tar.gz"; printf b >"$d/linux/dbento-app_1_amd64.deb"
  printf c >"$d/macos/Dbento_1_aarch64.dmg"; printf d >"$d/linux/notes.txt"
  printf e >"$d/windows/dbento-app_1_x64-setup.exe"; printf f >"$d/windows/dbento-1-x86_64-pc-windows-msvc.zip"
  bash "$RELEASE_SH" sums "$d" || fail "sums failed on a nested tree"
  local want="linux/dbento-1-x86_64-unknown-linux-gnu.tar.gz linux/dbento-app_1_amd64.deb macos/Dbento_1_aarch64.dmg windows/dbento-1-x86_64-pc-windows-msvc.zip windows/dbento-app_1_x64-setup.exe "
  [[ $(cut -d' ' -f3 "$d/SHA256SUMS" | tr '\n' ' ') == "$want" ]] \
    || fail "nested SHA256SUMS names: $(cut -d' ' -f3 "$d/SHA256SUMS" | tr '\n' ' ')"
  (cd "$d" && sha256sum -c --quiet SHA256SUMS) || fail "nested SHA256SUMS does not verify"
}

test_sums_fails_when_nothing_is_hashed() {
  mkdir -p "$WORK/none"
  touch "$WORK/none/notes.txt"
  if bash "$RELEASE_SH" sums "$WORK/none" 2>/dev/null; then fail "sums passed with no artifacts"; fi
}

test_stable_name_drops_the_version() {
  local pair got
  for pair in 'dbento-app_1.2.3_amd64.deb dbento-app_amd64.deb' 'dbento-app_1.2.3_arm64.deb dbento-app_arm64.deb' \
    'dbento-app_1.2.3_x86_64.AppImage dbento-app_x86_64.AppImage' 'dbento-app_1.2.3_aarch64.AppImage dbento-app_aarch64.AppImage' \
    'Dbento_1.2.3_aarch64.dmg Dbento_aarch64.dmg' 'Dbento_1.2.3_x86_64.dmg Dbento_x86_64.dmg' \
    'dbento-app_1.2.3_x64-setup.exe dbento-app_x64-setup.exe' \
    'dbento-1.2.3-x86_64-unknown-linux-gnu.tar.gz dbento-x86_64-unknown-linux-gnu.tar.gz' \
    'dbento-1.2.3-aarch64-apple-darwin.tar.gz dbento-aarch64-apple-darwin.tar.gz' \
    'dbento-1.2.3-x86_64-pc-windows-msvc.zip dbento-x86_64-pc-windows-msvc.zip'; do
    got=$(stable_name "${pair% *}" 1.2.3)
    [[ $got == "${pair#* }" ]] || fail "stable_name ${pair% *}: got '$got', want ${pair#* }"
  done
  # A dot in the version is literal, and loose files or unversioned names get no copy.
  [[ -z $(stable_name dbento-app_1x2x3_amd64.deb 1.2.3) ]] || fail "the version matched as a pattern"
  [[ -z $(stable_name SHA256SUMS 1.2.3) ]] || fail "SHA256SUMS got a stable name"
  [[ -z $(stable_name dbento-app_amd64.deb 1.2.3) ]] || fail "a stable name got a second copy"
}

test_stable_copies_sit_beside_the_versioned_files_and_in_the_sums() {
  local d=$WORK/stable
  mkdir -p "$d"
  printf a >"$d/dbento-app_1.2.3_amd64.deb"; printf b >"$d/dbento-app_1.2.3_x64-setup.exe"
  printf c >"$d/dbento-1.2.3-x86_64-unknown-linux-gnu.tar.gz"; printf d >"$d/notes.txt"
  bash "$RELEASE_SH" stable "$d" 1.2.3 || fail "stable failed"
  [[ $(cat "$d/dbento-app_amd64.deb") == a && $(cat "$d/dbento-app_x64-setup.exe") == b ]] || fail "stable copies differ from their sources"
  [[ -f $d/dbento-x86_64-unknown-linux-gnu.tar.gz && -f $d/dbento-app_1.2.3_amd64.deb ]] || fail "a versioned file or the CLI copy is missing"
  [[ $(ls "$d" | wc -l) == 7 ]] || fail "stable left: $(ls "$d" | tr '\n' ' ')"
  bash "$RELEASE_SH" sums "$d" || fail "sums failed after stable"
  grep -q ' dbento-app_x64-setup.exe$' "$d/SHA256SUMS" || fail "SHA256SUMS lacks the stable copies"
  (cd "$d" && sha256sum -c --quiet SHA256SUMS) || fail "SHA256SUMS with stable copies does not verify"
}

test_stable_copies_refuse_a_clash_or_an_empty_directory() {
  local d=$WORK/clash
  mkdir -p "$d" "$WORK/empty"
  mkdir -p "$d/a" "$d/b"
  printf a >"$d/a/dbento-app_1.2.3_x64-setup.exe"; printf b >"$d/b/dbento-app_1.2.3_x64-setup.exe"
  if bash "$RELEASE_SH" stable "$d" 1.2.3 2>/dev/null; then fail "stable accepted two files with one stable name"; fi
  if bash "$RELEASE_SH" stable "$WORK/empty" 1.2.3 2>/dev/null; then fail "stable passed with nothing to copy"; fi
  local status=0
  bash "$RELEASE_SH" stable "$d" >/dev/null 2>&1 || status=$?
  [[ $status == 2 ]] || fail "stable without a version exited $status, want 2"
}

test_macos_dmg_ad_hoc_signs_then_wraps_the_app() {
  local out=$WORK/mac
  mkdir -p "$out/Dbento.app/Contents"
  printf '#!/usr/bin/env bash\necho "codesign $*" >>"%s/mac.log"\n' "$WORK" >"$WORK/codesign"
  printf '#!/usr/bin/env bash\necho "hdiutil $*" >>"%s/mac.log"\ntouch "${@: -1}"\n' "$WORK" >"$WORK/hdiutil"
  chmod +x "$WORK/codesign" "$WORK/hdiutil"
  CODESIGN=$WORK/codesign HDIUTIL=$WORK/hdiutil macos_dmg "$out" 1.2.3 aarch64 || fail "macos_dmg failed"
  local want="codesign --force --deep --sign - $out/Dbento.app
codesign --verify --deep --strict $out/Dbento.app
hdiutil create -volname Dbento -srcfolder $out/Dbento.app -ov -format UDZO $out/Dbento_1.2.3_aarch64.dmg"
  [[ $(cat "$WORK/mac.log") == "$want" ]] || fail "macOS commands: $(cat "$WORK/mac.log")"
  [[ $(ls "$out") == Dbento_1.2.3_aarch64.dmg ]] || fail "want only the dmg, got: $(ls "$out")"
}

test_macos_dmg_stops_when_the_signature_does_not_verify() {
  local out=$WORK/mac-bad
  mkdir -p "$out/Dbento.app/Contents"
  printf '#!/usr/bin/env bash\n[[ $1 == --verify ]] && exit 1\nexit 0\n' >"$WORK/codesign-bad"
  printf '#!/usr/bin/env bash\necho ran >"%s/hdiutil-bad.log"\n' "$WORK" >"$WORK/hdiutil-bad"
  chmod +x "$WORK/codesign-bad" "$WORK/hdiutil-bad"
  if CODESIGN=$WORK/codesign-bad HDIUTIL=$WORK/hdiutil-bad macos_dmg "$out" 1.2.3 aarch64; then fail "an unverified app was wrapped"; fi
  [[ ! -e $WORK/hdiutil-bad.log && -d $out/Dbento.app ]] || fail "hdiutil ran or the app was removed"
}

test_cli_zip_holds_the_exe_and_files() {
  printf 'MZ' >"$WORK/dbento.exe"; printf 'mit\n' >"$WORK/LICENSE-MIT"; printf 'readme\n' >"$WORK/README.md"
  printf '<html>\n' >"$WORK/THIRD-PARTY-NOTICES.html"
  cli_zip "$WORK/dbento.exe" 9.9.9 x86_64-pc-windows-msvc "$WORK/zout" "$WORK/LICENSE-MIT" "$WORK/README.md" "$WORK/THIRD-PARTY-NOTICES.html"
  local z=$WORK/zout/dbento-9.9.9-x86_64-pc-windows-msvc.zip
  [[ -f $z ]] || { fail "zip not written"; return 0; }
  local listing; listing=$(unzip -Z1 "$z" | LC_ALL=C sort | tr '\n' ' ')
  local p=dbento-9.9.9-x86_64-pc-windows-msvc
  [[ $listing == "$p/ $p/LICENSE-MIT $p/README.md $p/THIRD-PARTY-NOTICES.html $p/dbento.exe " ]] || fail "zip listing: $listing"
  cli_zip "$WORK/dbento.exe" 9.9.9 x86_64-pc-windows-msvc "$WORK/zout" "$WORK/LICENSE-MIT"
  [[ $(unzip -Z1 "$z" | wc -l) == 3 ]] || fail "a second cli_zip kept old entries: $(unzip -Z1 "$z" | tr '\n' ' ')"
}

# Git Bash ships no zip, so without 7z cli_zip must fail by name and leave nothing behind.
test_cli_zip_fails_by_name_without_7z_or_zip() {
  local bin=$WORK/no-zip-bin t msg
  mkdir -p "$bin" "$WORK/zip-tmp"
  for t in install mkdir mktemp rm basename; do ln -sf "$(command -v "$t")" "$bin/$t"; done
  printf 'MZ' >"$WORK/dbento.exe"
  if msg=$(PATH=$bin TMPDIR=$WORK/zip-tmp cli_zip "$WORK/dbento.exe" 9.9.9 x86_64-pc-windows-msvc "$WORK/nozip" 2>&1); then
    fail "cli_zip passed without 7z or zip"
  fi
  [[ $msg == *7z* && $msg == *zip* && $msg != *"command not found"* ]] || fail "no-zip message: $msg"
  [[ -z $(ls -A "$WORK/zip-tmp") && ! -e $WORK/nozip/dbento-9.9.9-x86_64-pc-windows-msvc.zip ]] \
    || fail "cli_zip left files: $(ls -A "$WORK/zip-tmp" "$WORK/nozip" | tr '\n' ' ')"
}

test_cli_archive_honours_the_tar_override() {
  printf 'bin' >"$WORK/dbento"; printf 'mit\n' >"$WORK/LICENSE-MIT"
  printf '#!/usr/bin/env bash\necho used >>"%s/tar.log"\nexec tar "$@"\n' "$WORK" >"$WORK/faketar"
  chmod +x "$WORK/faketar"
  TAR=$WORK/faketar cli_archive "$WORK/dbento" 1.2.3 aarch64-apple-darwin "$WORK/tout" "$WORK/LICENSE-MIT"
  [[ -s $WORK/tar.log ]] || fail "cli_archive ignored TAR, which macOS sets to gtar"
  grep -qx 'dbento-1.2.3-aarch64-apple-darwin/dbento' <<<"$(tar -tzf "$WORK/tout/dbento-1.2.3-aarch64-apple-darwin.tar.gz")" \
    || fail "archive listing: $(tar -tzf "$WORK/tout/dbento-1.2.3-aarch64-apple-darwin.tar.gz" | tr '\n' ' ')"
}

test_prepare_out_dir_clears_macos_and_windows_leftovers() {
  local root=$WORK/root out=$WORK/root/dist
  mkdir -p "$out/Dbento.app/Contents"
  touch "$out/Dbento_1_aarch64.dmg" "$out/dbento-app_1_x64-setup.exe" "$out/dbento-1-x86_64-pc-windows-msvc.zip" \
    "$out/dbento-1-aarch64-apple-darwin.tar.gz" "$out/keep.txt"
  prepare_out_dir "$root" "$out" || fail "prepare_out_dir failed"
  [[ $(ls -A "$out") == keep.txt ]] || fail "leftovers after prepare_out_dir: $(ls -A "$out" | tr '\n' ' ')"
  if prepare_out_dir "$root" "$root" 2>/dev/null; then fail "the root itself accepted as the output directory"; fi
}

test_portable_realpath_matches_gnu_realpath() {
  local p
  for p in / "$WORK" "$WORK/a/../b" "$WORK/missing/x" "relative/../y"; do
    [[ $(portable_realpath -m -- "$p") == $(realpath -m -- "$p") ]] || fail "portable_realpath $p: $(portable_realpath -m -- "$p")"
  done
}

test_package_and_build_refuse_linux() {
  local msg step
  for step in package_main build_main; do
    if msg=$("$step" 2>&1); then fail "$step ran on Linux"; fi
    [[ $msg == *"make package"* ]] || fail "$step Linux refusal message: $msg"
  done
}

# A fake macOS host: uname says Darwin, and cargo, codesign, hdiutil and gtar log to LOG.
# cargo's build exits 1 when FAIL_BUILD is set; pkgid answers 1.2.3.
fake_macos_host() {
  local fake=$1 log=$2 t
  mkdir -p "$fake"
  printf '#!/usr/bin/env bash\necho Darwin\n' >"$fake/uname"
  printf '#!/usr/bin/env bash\nif [[ $1 == pkgid ]]; then echo "x#dbento-cli@1.2.3"; exit 0; fi\necho "cargo $*" >>"%s"\n[[ $1 == build && -n ${FAIL_BUILD:-} ]] && exit 1\nexit 0\n' "$log" >"$fake/cargo"
  for t in codesign hdiutil gtar; do
    printf '#!/usr/bin/env bash\necho "%s $*" >>"%s"\n' "$t" "$log" >"$fake/$t"
  done
  chmod +x "$fake"/*
}

test_build_stops_at_the_first_failed_step() {
  # The release build fails: nothing after cargo build may run.
  local fake=$WORK/fail-bin root out
  root=$(cd "$(dirname "$RELEASE_SH")/.." && pwd)
  out=$root/target/release-test-$$
  fake_macos_host "$fake" "$WORK/fail.log"
  if FAIL_BUILD=1 PATH=$fake:$PATH OUT_DIR=$out bash "$RELEASE_SH" build >/dev/null 2>&1; then fail "build exited 0 after a failed build"; fi
  rm -rf -- "$out"
  [[ $(cat "$WORK/fail.log") == "cargo build --release --locked -p dbento-cli -p dbento-app" ]] \
    || fail "steps ran after the failed build: $(tr '\n' '|' <"$WORK/fail.log")"
}

# libduckdb does not build for msvc, so the Windows release builds without DuckDB; macOS keeps it.
test_build_leaves_duckdb_out_only_on_windows() {
  local fake=$WORK/win-bin root out
  root=$(cd "$(dirname "$RELEASE_SH")/.." && pwd)
  out=$root/target/release-test-$$
  fake_macos_host "$fake" "$WORK/win-build.log"
  printf '#!/usr/bin/env bash\necho MINGW64_NT-10.0\n' >"$fake/uname"
  PATH=$fake:$PATH OUT_DIR=$out bash "$RELEASE_SH" build >/dev/null 2>&1 || fail "Windows build failed"
  rm -rf -- "$out"
  [[ $(head -n 1 "$WORK/win-build.log") == "cargo build --release --locked -p dbento-cli -p dbento-app --no-default-features --features dbento-core/keychain" ]] \
    || fail "Windows build line: $(head -n 1 "$WORK/win-build.log")"

  fake_macos_host "$fake" "$WORK/win-mac.log"
  PATH=$fake:$PATH OUT_DIR=$out bash "$RELEASE_SH" build >/dev/null 2>&1 || fail "macOS build failed"
  rm -rf -- "$out"
  [[ $(head -n 1 "$WORK/win-mac.log") == "cargo build --release --locked -p dbento-cli -p dbento-app" ]] \
    || fail "macOS build line: $(head -n 1 "$WORK/win-mac.log")"
}

test_package_needs_the_build_output_and_never_builds() {
  local fake=$WORK/pkg-bin root out msg
  root=$(cd "$(dirname "$RELEASE_SH")/.." && pwd)
  out=$root/target/release-test-$$
  fake_macos_host "$fake" "$WORK/pkg.log"
  rm -rf -- "$out"
  if msg=$(PATH=$fake:$PATH OUT_DIR=$out bash "$RELEASE_SH" package 2>&1); then fail "package ran without a build"; fi
  [[ $msg == *"release.sh build"* ]] || fail "missing-build message: $msg"
  [[ ! -e $WORK/pkg.log ]] || fail "package ran a step without a build: $(tr '\n' '|' <"$WORK/pkg.log")"

  # With the notices and binary in place, package packages, signs and archives, but never builds.
  mkdir -p "$out/Dbento.app" "$WORK/pkg-target/release"
  touch "$out/THIRD-PARTY-NOTICES.html" "$WORK/pkg-target/release/dbento"
  PATH=$fake:$PATH OUT_DIR=$out CARGO_TARGET_DIR=$WORK/pkg-target bash "$RELEASE_SH" package >/dev/null 2>&1 \
    || fail "package failed after a build: $(tr '\n' '|' <"$WORK/pkg.log")"
  rm -rf -- "$out"
  grep -q '^cargo xtask packager-config ' "$WORK/pkg.log" || fail "package did not package: $(tr '\n' '|' <"$WORK/pkg.log")"
  if grep -qE '^cargo (build|fetch)|^cargo xtask notices' "$WORK/pkg.log"; then fail "package built: $(tr '\n' '|' <"$WORK/pkg.log")"; fi
}

# Windows ships unsigned under its plain names; no file is renamed for a missing certificate.
test_windows_package_keeps_the_plain_names() {
  local fake=$WORK/winpkg-bin root out
  root=$(cd "$(dirname "$RELEASE_SH")/.." && pwd)
  out=$root/target/release-test-win-$$
  fake_macos_host "$fake" "$WORK/winpkg.log"
  printf '#!/usr/bin/env bash\necho MINGW64_NT-10.0\n' >"$fake/uname"
  printf '#!/usr/bin/env bash\nif [[ $1 == pkgid ]]; then echo "x#dbento-cli@1.2.3"; exit 0; fi\nif [[ $1 == packager ]]; then touch "%s/dbento-app_1.2.3_x64-setup.exe"; fi\nexit 0\n' "$out" >"$fake/cargo"
  mkdir -p "$out" "$WORK/winpkg-target/release"
  touch "$out/THIRD-PARTY-NOTICES.html" "$WORK/winpkg-target/release/dbento.exe"
  PATH=$fake:$PATH OUT_DIR=$out CARGO_TARGET_DIR=$WORK/winpkg-target bash "$RELEASE_SH" package >/dev/null 2>&1 \
    || fail "Windows package failed"
  local got; got=$(cd "$out" && ls | LC_ALL=C sort | tr '\n' ' ')
  rm -rf -- "$out"
  [[ $got == "THIRD-PARTY-NOTICES.html dbento-1.2.3-"*".zip dbento-app_1.2.3_x64-setup.exe " && $got != *unsigned* ]] \
    || fail "Windows package names: $got"
}

test_macos_code_paths_avoid_bash4_only_syntax() {
  local bash4='mapfile|readarray|declare -A|local -A|\|&|;;&|\$\{[A-Za-z_]+(,,|\^\^|@[A-Za-z])'
  if grep -nE "$bash4" "$RELEASE_SH" | grep -vE '^[0-9]+:[[:space:]]*#'; then fail "release.sh uses bash 4 syntax"; fi
  declare -F prepare_out_dir package_app cli_archive >/dev/null || fail "a package-linux.sh function macOS needs is missing"
  if declare -f prepare_out_dir package_app cli_archive | grep -nE "$bash4"; then
    fail "a package-linux.sh function used on macOS uses bash 4 syntax"
  fi
}

for t in $(declare -F | awk '{print $3}' | grep '^test_'); do "$t"; done
if ((FAILS > 0)); then echo "$FAILS failure(s)" >&2; exit 1; fi
echo "release tests passed"
