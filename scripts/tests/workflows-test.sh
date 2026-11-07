#!/usr/bin/env bash
# Structural checks on the workflows that actionlint does not make: pins, gate steps, timeouts.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
CI=$ROOT/.github/workflows/ci.yml
RELEASE=$ROOT/.github/workflows/release.yml
WEEKLY=$ROOT/.github/workflows/weekly.yml
FAILS=0
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }

# Print "job=minutes " for every job under jobs:, sorted by job name.
job_timeouts() {
  awk '/^jobs:/ { in_jobs = 1; next }
       in_jobs && /^  [a-z][a-z0-9_-]*:$/ { job = $1; sub(":", "", job) }
       in_jobs && /^    timeout-minutes:/ { print job "=" $2 }' "$1" | LC_ALL=C sort | tr '\n' ' '
}

# Every uses: must name a 40-hex commit, never a tag or a branch.
pinned_uses_only() {
  local file=$1 bad
  bad=$(grep -nE '^[[:space:]]*(- )?uses:' "$file" | grep -vE 'uses: [A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+@[0-9a-f]{40}( #.*)?$' || true)
  [[ -z $bad ]] || fail "$(basename "$file") has unpinned actions: $bad"
  grep -qE 'uses:' "$file" || fail "$(basename "$file") uses no actions at all"
}

# dtolnay/rust-toolchain regenerates its stable branch, orphaning older commits, so a pin must
# name a master commit (marked "# master") and pass the toolchain explicitly.
test_rust_toolchain_pinned_to_master() {
  local file uses
  for file in "$CI" "$RELEASE"; do
    uses=$(grep -E 'uses: dtolnay/rust-toolchain@' "$file" || true)
    [[ -n $uses ]] || { fail "$(basename "$file") does not use dtolnay/rust-toolchain"; continue; }
    if grep -vE '@[0-9a-f]{40} # master$' <<<"$uses" >/dev/null; then
      fail "$(basename "$file") pins dtolnay/rust-toolchain off master: $uses"
    fi
    [[ $(grep -cE 'uses: dtolnay/rust-toolchain@' "$file") == $(grep -A2 'uses: dtolnay/rust-toolchain@' "$file" | grep -cE '^[[:space:]]+toolchain: stable$') ]] \
      || fail "$(basename "$file") runs dtolnay/rust-toolchain without toolchain: stable"
  done
}

test_ci_gate_runs_every_check() {
  grep -qxE '[[:space:]]+run: make LOCKED=--locked check icons-check windows-check notices-check script-test workflows-lint' "$CI" \
    || fail "ci gate line: $(grep -n 'run: make' "$CI")"
}

test_ci_runs_on_push_and_pull_request() {
  grep -qxE '  push:' "$CI" && grep -qxF '    branches: [master]' "$CI" || fail "ci does not run on push to master"
  grep -qxE '  pull_request:' "$CI" || fail "ci does not run on pull requests"
  grep -qxE '  workflow_dispatch:' "$CI" || fail "ci lost workflow_dispatch"
  grep -qxF '  cancel-in-progress: true' "$CI" || fail "ci lost cancel-in-progress"
}

# The SQL Server licence is accepted for the Linux gate only, never workflow-wide.
test_ci_accepts_the_mssql_licence_in_the_gate_only() {
  [[ $(grep -c 'DBENTO_MSSQL_ACCEPT_EULA' "$CI") == 1 ]] || fail "DBENTO_MSSQL_ACCEPT_EULA set more than once"
  awk '/^  gate:$/ { on = 1; next } on && /^  [a-z]/ { on = 0 } on && /^      DBENTO_MSSQL_ACCEPT_EULA: "Y"$/ { ok = 1 } END { exit !ok }' "$CI" \
    || fail "the gate job does not set DBENTO_MSSQL_ACCEPT_EULA=Y"
}

test_ci_installs_the_cross_and_notices_tools() {
  grep -qE '^[[:space:]]+targets: x86_64-pc-windows-gnu$' "$CI" || fail "no x86_64-pc-windows-gnu target"
  grep -qE 'apt-get install -y --no-install-recommends \$LINUX_BUILD_DEPS gcc-mingw-w64-x86-64$' "$CI" || fail "no mingw install"
  grep -qE 'run: cargo install --locked cargo-about --version 0\.9\.2 --features cli$' "$CI" || fail "no cargo-about 0.9.2 install"
  grep -q 'libclang-dev' "$CI" || fail "libclang-dev dropped from LINUX_BUILD_DEPS"
}

test_ci_gate_timeout_and_pins() {
  [[ $(job_timeouts "$CI") == "gate=60 macos-clippy=60 " ]] || fail "ci timeouts: $(job_timeouts "$CI")"
  pinned_uses_only "$CI"
}

# Tests need containers, so macOS runs clippy alone.
test_ci_macos_runs_locked_clippy() {
  grep -qxF '    runs-on: macos-latest' "$CI" || fail "no macos-latest job"
  grep -qxF '        run: cargo clippy --workspace --all-targets --all-features --locked -- -Dwarnings' "$CI" \
    || fail "macOS clippy line"
}

# Beta clippy plus a check after cargo update, unlocked on purpose, read-only and pinned.
test_weekly_checks_beta_and_updated_deps() {
  [[ -f $WEEKLY ]] || { fail "no weekly.yml"; return 0; }
  [[ $(job_timeouts "$WEEKLY") == "beta-clippy=60 updated-deps=60 " ]] || fail "weekly timeouts: $(job_timeouts "$WEEKLY")"
  pinned_uses_only "$WEEKLY"
  grep -qxE '  schedule:' "$WEEKLY" && grep -qxE '  workflow_dispatch:' "$WEEKLY" || fail "weekly triggers"
  grep -qxF '  contents: read' "$WEEKLY" || fail "weekly is not contents: read"
  grep -qxF '          toolchain: beta' "$WEEKLY" || fail "no beta toolchain"
  grep -qxF '        run: cargo clippy --workspace --all-targets --all-features -- -Dwarnings' "$WEEKLY" || fail "beta clippy line"
  grep -qxF '        run: cargo update' "$WEEKLY" || fail "no cargo update"
  grep -qxF '        run: cargo check --workspace --all-targets --all-features' "$WEEKLY" || fail "updated check line"
  grep -q 'libclang-dev' "$WEEKLY" || fail "weekly LINUX_BUILD_DEPS lacks libclang-dev"
}

test_release_jobs_have_their_timeouts() {
  [[ -f $RELEASE ]] || { fail "no release.yml"; return 0; }
  [[ $(job_timeouts "$RELEASE") == "build=90 checksums=45 guard=5 " ]] || fail "release timeouts: $(job_timeouts "$RELEASE")"
  pinned_uses_only "$RELEASE"
}

test_release_triggers_and_dispatch_defaults() {
  [[ -f $RELEASE ]] || return 0
  grep -qE '^    tags: \["v\*"\]$' "$RELEASE" || fail "no v* tag trigger"
  grep -qE '^        default: true$' "$RELEASE" || fail "dry_run does not default to true"
  grep -qE '^        default: linux,windows,macos-arm64$' "$RELEASE" || fail "platforms default"
  grep -qF 'linux,windows,macos-arm64,macos-intel' "$RELEASE" || fail "platforms description lacks the four tokens"
}

test_release_guard_runs_the_script() {
  [[ -f $RELEASE ]] || return 0
  grep -qF 'run: scripts/release.sh guard "$GITHUB_REF_TYPE" "$GITHUB_REF_NAME" "$DRY_RUN" "$PLATFORMS" >>"$GITHUB_OUTPUT"' "$RELEASE" \
    || fail "guard step"
  grep -qF 'DRY_RUN: ${{ inputs.dry_run || false }}' "$RELEASE" || fail "DRY_RUN env"
  grep -qF 'matrix: ${{ fromJSON(needs.guard.outputs.matrix) }}' "$RELEASE" || fail "matrix from the guard"
}

test_release_builds_with_the_packaging_tools() {
  [[ -f $RELEASE ]] || return 0
  grep -qF 'run: cargo install --locked cargo-packager --version 0.11.8' "$RELEASE" || fail "no cargo-packager 0.11.8"
  grep -qF 'run: cargo install --locked cargo-about --version 0.9.2 --features cli' "$RELEASE" || fail "no cargo-about 0.9.2"
  grep -qE '^        run: make package$' "$RELEASE" || fail "no Linux make package"
  grep -qE '^        run: bash scripts/release.sh package$' "$RELEASE" || fail "no macOS and Windows package step"
  grep -qF "WINDOWS_CERTIFICATE: \${{ runner.os == 'Windows' && secrets.WINDOWS_CERTIFICATE || '' }}" "$RELEASE" || fail "certificate secret not passed"
  grep -qF "WINDOWS_CERTIFICATE_PASSWORD: \${{ runner.os == 'Windows' && secrets.WINDOWS_CERTIFICATE_PASSWORD || '' }}" "$RELEASE" || fail "password secret not passed"
  [[ $(grep -c 'secrets\.' "$RELEASE") == 2 ]] || fail "secrets referenced outside the package step: $(grep -n 'secrets\.' "$RELEASE")"
  grep -qF 'TMPDIR: ${{ runner.temp }}' "$RELEASE" || fail "the signing key dir is not under runner.temp"
  grep -qF 'run: rm -f target/dist/*/SHA256SUMS target/dist/*/THIRD-PARTY-NOTICES.html' "$RELEASE" || fail "loose files not dropped"
  grep -qE '^  LINUX_BUILD_DEPS: .*libclang-dev' "$RELEASE" || fail "LINUX_BUILD_DEPS lacks libclang-dev"
}

# Build scripts must never see the signing secrets: the macOS and Windows build is its own step,
# with no env, before the one package step that takes them.
test_release_builds_before_the_secrets_arrive() {
  [[ -f $RELEASE ]] || { fail "no release.yml"; return 0; }
  local build package
  build=$(grep -nE '^        run: bash scripts/release.sh build$' "$RELEASE" | cut -d: -f1) || true
  package=$(grep -nE '^        run: bash scripts/release.sh package$' "$RELEASE" | cut -d: -f1) || true
  [[ -n $build ]] || { fail "release.yml has no separate macOS and Windows build step"; return 0; }
  [[ -n $package && $build -lt $package ]] || fail "the build step (line $build) does not come before the package step (line $package)"
  # The build step runs from its name to its run: line; none of it may carry an env.
  local start
  start=$(awk -v run="$build" 'NR < run && /^      - name:/ { start = NR } NR == run { print start; exit }' "$RELEASE")
  [[ -n $start ]] || { fail "the build step has no name"; return 0; }
  if grep -qE '^        env:|secrets\.' <<<"$(sed -n "${start},${build}p" "$RELEASE")"; then fail "the build step has an env or a secret"; fi
}

# The stable copies must exist before the sums run, so SHA256SUMS covers them too.
test_release_adds_stable_copies_before_the_sums() {
  [[ -f $RELEASE ]] || { fail "no release.yml"; return 0; }
  local stable sums
  stable=$(grep -nF 'run: scripts/release.sh stable dist "$VERSION"' "$RELEASE" | cut -d: -f1)
  sums=$(grep -nF 'run: scripts/release.sh sums dist' "$RELEASE" | cut -d: -f1)
  [[ -n $stable ]] || { fail "release.yml adds no stable-named copies"; return 0; }
  [[ -n $sums && $stable -lt $sums ]] || fail "stable copies (line $stable) do not come before the sums (line $sums)"
}

test_release_drafts_only_on_a_tag() {
  [[ -f $RELEASE ]] || return 0
  [[ $(grep -cE '^[[:space:]]+run: .*gh release' "$RELEASE") == 1 ]] || fail "want exactly one run: line calling gh release: $(grep -n 'gh release' "$RELEASE")"
  [[ $(grep -cE '^[[:space:]]+gh release' "$RELEASE") == 0 ]] || fail "a gh release call inside a run block: $(grep -n 'gh release' "$RELEASE")"
  grep -qF 'gh release create "$GITHUB_REF_NAME" --repo "$GITHUB_REPOSITORY" --draft --verify-tag --title "Dbento $VERSION" --notes-file "$RUNNER_TEMP/notes.md" dist/*' "$RELEASE" \
    || fail "draft command"
  # gh puts a notes file before generated notes, so the two never mix.
  if grep -qF -- '--generate-notes' "$RELEASE"; then fail "release.yml still asks gh to generate notes"; fi
  grep -qF 'run: scripts/release-notes.sh "$GITHUB_REF_NAME" >"$RUNNER_TEMP/notes.md"' "$RELEASE" || fail "no step writes the notes"
  # The draft step's if: sits at most six lines above its gh release line (if, env and two values, run).
  awk '/if: github.ref_type == .tag. && !inputs.dry_run/ { armed = NR } /gh release create/ { if (armed && NR - armed <= 6) ok = 1 } END { exit !ok }' "$RELEASE" \
    || fail "the draft step is not guarded by a tag and not-dry-run if"
  [[ $(grep -c 'contents: write' "$RELEASE") == 1 ]] || fail "contents: write outside the checksums job"
}

# The notes come from GitHub's generate-notes API, which needs the job token (the checksums job holds contents: write).
test_release_notes_step_has_the_token() {
  [[ -f $RELEASE ]] || return 0
  local run start
  run=$(grep -nF 'run: scripts/release-notes.sh' "$RELEASE" | cut -d: -f1) || true
  [[ -n $run ]] || { fail "no step writes the notes"; return 0; }
  start=$(awk -v run="$run" 'NR < run && /^      - name:/ { start = NR } NR == run { print start; exit }' "$RELEASE")
  grep -qF 'GH_TOKEN: ${{ github.token }}' <<<"$(sed -n "${start},${run}p" "$RELEASE")" \
    || fail "the notes step has no GH_TOKEN"
  awk '/^  checksums:$/ { on = 1 } on && /scripts\/release-notes\.sh/ { ok = 1 } on && /^  [a-z]/ && !/checksums/ { on = 0 } END { exit !ok }' "$RELEASE" \
    || fail "the notes step is not in the checksums job"
}

# The attribution check is a shared template; it still pins every action to a commit.
test_attribution_check_is_pinned_and_reads_the_base() {
  local file=$ROOT/.github/workflows/attribution.yml
  [[ -f $file ]] || { fail "no attribution.yml"; return 0; }
  pinned_uses_only "$file"
  grep -qF 'git show "$BASE:.github/scripts/attribution-lib.sh"' "$file" || fail "attribution.yml does not load the lib from the base"
  [[ -f $ROOT/.github/scripts/attribution-lib.sh ]] || fail "no .github/scripts/attribution-lib.sh"
}

# What's Changed walks back to the previous tag, which a shallow checkout does not have.
test_release_notes_checkout_has_full_history() {
  [[ -f $RELEASE ]] || return 0
  awk '/^  checksums:$/ { on = 1 } on && /fetch-depth: 0/ { ok = 1 } on && /^  [a-z]/ && !/checksums/ { on = 0 } END { exit !ok }' "$RELEASE" \
    || fail "the checksums job checks out without fetch-depth: 0"
}

test_release_has_no_updater_pieces() {
  [[ -f $RELEASE ]] || return 0
  if grep -niE 'apinata|UPDATE_|latest\.json|signer|manifest|choco' "$RELEASE"; then fail "updater or Apinata leftovers in release.yml"; fi
}

for t in $(declare -F | awk '{print $3}' | grep '^test_'); do "$t"; done
if ((FAILS > 0)); then echo "$FAILS failure(s)" >&2; exit 1; fi
echo "workflow tests passed"
