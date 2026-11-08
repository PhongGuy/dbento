#!/usr/bin/env bash
# Checks that the README and the releasing guide name what the release scripts produce.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
README=$ROOT/README.md
DOC=$ROOT/RELEASING.md
FAILS=0
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }
need() { grep -qF -- "$2" "$1" || fail "$(basename "$1") lacks: $2"; }

test_readme_names_every_release_file() {
  local f
  for f in 'dbento-app_<v>_amd64.deb' 'dbento-app_<v>_arm64.deb' 'dbento-app_<v>_x86_64.AppImage' \
    'dbento-app_<v>_aarch64.AppImage' 'Dbento_<v>_aarch64.dmg' 'Dbento_<v>_x86_64.dmg' 'dbento-app_<v>_x64-setup.exe' \
    'dbento-<v>-x86_64-unknown-linux-gnu.tar.gz' 'dbento-<v>-aarch64-unknown-linux-gnu.tar.gz' \
    'dbento-<v>-aarch64-apple-darwin.tar.gz' 'dbento-<v>-x86_64-apple-darwin.tar.gz' 'dbento-<v>-x86_64-pc-windows-msvc.zip'; do
    need "$README" "$f"
  done
}

# The download links name the stable copies release.sh stable writes; versioned names never resolve.
test_readme_links_every_stable_download() {
  local f
  for f in dbento-app_amd64.deb dbento-app_arm64.deb dbento-app_x86_64.AppImage dbento-app_aarch64.AppImage \
    Dbento_aarch64.dmg Dbento_x86_64.dmg dbento-app_x64-setup.exe dbento-x86_64-unknown-linux-gnu.tar.gz \
    dbento-aarch64-unknown-linux-gnu.tar.gz dbento-aarch64-apple-darwin.tar.gz dbento-x86_64-apple-darwin.tar.gz \
    dbento-x86_64-pc-windows-msvc.zip; do
    need "$README" "(https://github.com/PhongGuy/dbento/releases/latest/download/$f)"
  done
  if grep -nE 'releases/latest/download/[^)]*(<v>|[0-9]+\.[0-9]+\.[0-9]+|-unsigned)' "$README"; then
    fail "a latest/download link names a versioned or -unsigned file"
  fi
}

test_readme_screenshots_exist() {
  local img
  for img in $(grep -oE 'assets/screenshots/[a-z-]+\.png' "$README" | sort -u); do
    [[ -f $ROOT/$img ]] || fail "README shows $img, which does not exist"
  done
  grep -qF 'assets/screenshots/hero.png' "$README" || fail "README has no hero screenshot"
}

test_readme_covers_first_launch_on_each_platform() {
  need "$README" 'sha256sum -c --ignore-missing SHA256SUMS'
  need "$README" '/Applications'
  need "$README" 'xattr -dr com.apple.quarantine /Applications/Dbento.app'
  need "$README" 'THIRD-PARTY-NOTICES.html'
  need "$README" 'The macOS and Windows packages have not yet been tested on those systems.'
  grep -qiE 'AppImage.*CLI archive|CLI archive.*AppImage' "$README" || fail "README does not send AppImage users to the CLI archive"
  need "$README" 'cargo install --locked --path crates/dbento-cli'
  if grep -qF 'comes with the release' "$README"; then fail "README still says the .app bundle comes with the release"; fi
}

test_releasing_doc_covers_the_release_steps() {
  [[ -f $DOC ]] || { fail "no RELEASING.md"; return 0; }
  local s
  for s in 'gh workflow run release.yml --ref master -f dry_run=true -f platforms=linux,windows,macos-arm64' \
    'gh run watch <id> --exit-status' 'git tag v<version>' 'git push origin v<version>' 'sha256sum -c SHA256SUMS' \
    'codesign -v' 'xattr -dr com.apple.quarantine /Applications/Dbento.app' 'macos-intel' \
    'THIRD-PARTY-NOTICES.html' 'CLI archive'; do
    need "$DOC" "$s"
  done
}

# A tag run waits at the attest job for the owner, and every file verifies against the attestation.
test_releasing_doc_covers_the_attestation() {
  [[ -f $DOC ]] || return 0
  need "$DOC" 'gh attestation verify <file> --repo PhongGuy/dbento'
  need "$DOC" 'The `attest` job is the only job bound to the `release` environment'
}

test_releasing_doc_says_outward_steps_are_the_maintainers() {
  [[ -f $DOC ]] || return 0
  need "$DOC" "Pushing a tag, publishing the draft, uploading anything and dispatching any workflow run are the maintainer's own actions, each taken only with their explicit approval."
}

test_releasing_doc_checks_the_intro_placeholder_is_gone() {
  [[ -f $DOC ]] || return 0
  need "$DOC" 'packaging/release-notes.md'
  need "$DOC" "gh release view v<version> --json body -q .body | grep -c '<!-- intro:'"
  grep -qF -- '<!-- intro:' "$ROOT/packaging/release-notes.md" 2>/dev/null || fail "the template has no intro marker"
}

# A dispatch on a tag ref with dry_run off drafts exactly as a tag push does (release.yml's draft if:).
test_releasing_doc_warns_that_a_tag_dispatch_drafts() {
  [[ -f $DOC ]] || return 0
  need "$DOC" 'A run dispatched on a tag ref with `dry_run=false` drafts a release too, exactly as pushing the tag does.'
}

# Windows ships unsigned under its plain names, so no doc names a certificate or the old marker.
test_docs_name_no_certificate_or_unsigned_marker() {
  local f
  for f in "$README" "$DOC" "$ROOT/packaging/release-notes.md"; do
    if grep -nE 'WINDOWS_CERTIFICATE|-unsigned|signtool' "$f"; then fail "$(basename "$f") still names Windows certificate signing"; fi
  done
}

test_docs_carry_no_updater_or_apinata_text() {
  local f
  for f in "$README" "$DOC"; do
    [[ -f $f ]] || continue
    if grep -niE 'apinata|latest\.json|APINATA_|\.app\.tar\.gz' "$f"; then fail "$(basename "$f") carries updater or Apinata text"; fi
  done
}

for t in $(declare -F | awk '{print $3}' | grep '^test_'); do "$t"; done
if ((FAILS > 0)); then echo "$FAILS failure(s)" >&2; exit 1; fi
echo "release docs tests passed"
