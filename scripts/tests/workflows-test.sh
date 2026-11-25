#!/usr/bin/env bash
# Structural checks on the workflows that actionlint does not make: pins, gate steps, timeouts.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
CI=$ROOT/.github/workflows/ci.yml
RELEASE=$ROOT/.github/workflows/release.yml
WEEKLY=$ROOT/.github/workflows/weekly.yml
ZIZMOR=$ROOT/.github/workflows/zizmor.yml
ADVISORIES=$ROOT/.github/workflows/advisories.yml
FAILS=0
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }

# Print "job=minutes " for every job under jobs:, sorted by job name.
job_timeouts() {
  awk '/^jobs:/ { in_jobs = 1; next }
       in_jobs && /^  [a-z][a-z0-9_-]*:$/ { job = $1; sub(":", "", job) }
       in_jobs && /^    timeout-minutes:/ { print job "=" $2 }' "$1" | LC_ALL=C sort | tr '\n' ' '
}

# Print the lines of job JOB in FILE, from its key to the next job.
job_body() {
  awk -v job="  $2:" '/^jobs:/ { in_jobs = 1; next }
       in_jobs && /^  [a-z][a-z0-9_-]*:$/ { on = ($0 == job) }
       in_jobs && on' "$1"
}

# Print the lines of the step named NAME in job JOB of FILE, up to the next step.
step_body() {
  job_body "$1" "$2" | awk -v name="      - name: $3" '$0 == name { on = 1; print; next } on && /^      - / { exit } on'
}

# Print job JOB of FILE with each folded `run: >-` block joined onto its run: line.
job_runs() {
  job_body "$1" "$2" | awk '
    fold && /^          / { sub(/^ +/, ""); line = line " " $0; next }
    fold { print line; fold = 0 }
    /^        run: >-$/ { fold = 1; line = "        run:"; next }
    { print }
    END { if (fold) print line }'
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

# The quick, network-bound checks run before the hour-long suite, so a download outage
# fails the gate in minutes instead of after every test has passed.
test_ci_gate_runs_every_check() {
  grep -qxE '[[:space:]]+run: make LOCKED=--locked workflows-lint script-test check icons-check windows-check notices-check' "$CI" \
    || fail "ci gate line: $(grep -n 'run: make' "$CI")"
}

test_actionlint_download_retries_a_blip() {
  grep -qE '^  curl .*--retry [0-9]+ .*--retry-delay [0-9]+' "$ROOT/scripts/actionlint.sh" \
    || fail "scripts/actionlint.sh downloads without --retry and --retry-delay"
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
  [[ $(job_timeouts "$CI") == "each-feature=60 gate=120 macos-clippy=60 pilot=120 windows=90 " ]] || fail "ci timeouts: $(job_timeouts "$CI")"
  pinned_uses_only "$CI"
}

# rust-cache strips the files out of target/duckdb-sources but keeps its folders, so the first
# run step after every restore must remove that tree before anything builds.
test_every_cache_restore_drops_the_duckdb_sources() {
  local file restores drops
  for file in "$CI" "$WEEKLY"; do
    restores=$(grep -c 'uses: Swatinem/rust-cache@' "$file")
    drops=$(awk '/uses: Swatinem\/rust-cache@/ { want = 1; next }
      want && /^ +run: / { if ($0 ~ /^        run: rm -rf target\/duckdb-sources( |$)/) n++; want = 0 }
      END { print n + 0 }' "$file")
    [[ $restores -gt 0 && $restores == "$drops" ]] \
      || fail "$(basename "$file"): $drops of $restores cache restores drop target/duckdb-sources"
  done
}

# Tests need containers, so macOS runs clippy alone.
test_ci_macos_runs_locked_clippy() {
  grep -qxF '    runs-on: macos-latest' "$CI" || fail "no macos-latest job"
  grep -qxF '        run: cargo clippy --workspace --all-targets --all-features --locked -- -Dwarnings' "$CI" \
    || fail "macOS clippy line"
}

# Each engine feature builds on its own, so a cfg gap under reduced features fails CI; the
# powerset is skipped because every combination rebuilds DuckDB and pg_query.
test_ci_checks_each_feature_alone() {
  local job
  job=$(job_body "$CI" each-feature)
  [[ -n $job ]] || { fail "ci has no each-feature job"; return 0; }
  grep -qE '^      - uses: taiki-e/install-action@[0-9a-f]{40} # v[0-9.]+$' <<<"$job" || fail "each-feature does not install cargo-hack by pinned action"
  grep -qxF '          tool: cargo-hack' <<<"$job" || fail "each-feature does not install cargo-hack"
  grep -qxF '        run: make LOCKED=--locked each-feature' <<<"$job" || fail "each-feature run line"
  grep -qE '^each-feature:' "$ROOT/Makefile" || fail "no Makefile each-feature target"
  grep -qxF 'FEATURE_CRATES := -p dbento-core -p dbento-cli -p dbento-mcp' "$ROOT/Makefile" || fail "Makefile FEATURE_CRATES"
  grep -qE -- 'RUSTFLAGS=-Dwarnings \$\(CARGO\) hack check --each-feature --no-dev-deps \$\(FEATURE_CRATES\)$' "$ROOT/Makefile" \
    || fail "Makefile each-feature does not run cargo hack on the three crates"
  if grep -qE -- '--feature-powerset' "$ROOT/Makefile"; then fail "Makefile checks the feature powerset"; fi
}

# The Windows run is opt-in from a dispatch, builds what the release ships with the release's
# Windows features, and runs only updater tests, since the runner has no podman.
test_ci_windows_run_is_opt_in() {
  local dispatch job features
  dispatch=$(awk '/^  workflow_dispatch:$/ { on = 1; next } on && /^([a-z]|  [a-z])/ { on = 0 } on' "$CI")
  grep -qxF '      windows:' <<<"$dispatch" || fail "ci workflow_dispatch has no windows input"
  grep -qxF '        type: boolean' <<<"$dispatch" || fail "the windows input is not a boolean"
  grep -qxF '        default: false' <<<"$dispatch" || fail "the windows input does not default to false"
  job=$(job_runs "$CI" windows)
  [[ -n $job ]] || { fail "ci has no windows job"; return 0; }
  grep -qxF '    if: inputs.windows' <<<"$job" || fail "the windows job is not gated on inputs.windows"
  grep -qxF '    runs-on: windows-latest' <<<"$job" || fail "the windows job does not run on windows-latest"
  grep -qxF '          persist-credentials: false' <<<"$job" || fail "the windows checkout persists credentials"
  grep -qxF '        shell: bash' <<<"$job" || fail "the windows job does not run its steps in bash"
  features=$(sed -n 's/^  if \[\[ \$os == windows \]\]; then features=(\(.*\)); fi$/\1/p' "$ROOT/scripts/release.sh")
  [[ -n $features ]] || { fail "release.sh names no Windows features"; return 0; }
  grep -qxF "        run: cargo build --locked -p dbento-cli -p dbento-app $features" <<<"$job" \
    || fail "the windows job does not build the CLI and app with the release's features ($features)"
  local m
  for m in update:: update_check:: update_config:: update_download:: update_install:: update_install_kind:: \
    update_manifest:: update_signature::; do
    grep -qE "^        run: cargo test --locked -p dbento-core --no-default-features --features keychain,update --lib --test integration -- (.* )?$m( |$)" <<<"$job" \
      || fail "the windows job does not run the core $m tests"
  done
  grep -qxF "        run: cargo test --locked -p dbento-cli $features --test integration -- cli_self_update::" <<<"$job" \
    || fail "the windows job does not run the CLI self-update tests"
  grep -qxF "        run: cargo test --locked -p dbento-app $features --test integration -- update_arm:: update_banner:: update_quit:: updates::" <<<"$job" \
    || fail "the windows job does not run the app update tests"
  if grep -qE 'DOCKER_HOST|ACCEPT_EULA' <<<"$job"; then fail "the windows job reaches for containers"; fi
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

# A new RustSec advisory lands without a change on our side, so both lockfiles are scanned
# daily, read-only, with the same cargo-deny the gate installs.
test_advisories_scans_daily_with_the_gate_pin() {
  [[ -f $ADVISORIES ]] || { fail "no advisories.yml"; return 0; }
  [[ $(job_timeouts "$ADVISORIES") == "advisories=15 " ]] || fail "advisories timeouts: $(job_timeouts "$ADVISORIES")"
  pinned_uses_only "$ADVISORIES"
  grep -qxE '    - cron: "[0-9]+ [0-9]+ \* \* \*"' "$ADVISORIES" || fail "advisories is not a daily cron"
  if grep -qE '    - cron: "0 ' "$ADVISORIES"; then fail "advisories cron runs on the hour"; fi
  grep -qxE '  workflow_dispatch:' "$ADVISORIES" || fail "advisories lost workflow_dispatch"
  grep -qxF '  contents: read' "$ADVISORIES" || fail "advisories is not contents: read"
  if grep -qE ': write' "$ADVISORIES"; then fail "advisories asks for a write permission"; fi
  grep -qxF '          persist-credentials: false' "$ADVISORIES" || fail "advisories keeps checkout credentials"
  grep -qxF '        run: make advisories' "$ADVISORIES" || fail "advisories does not run make advisories"
  local want got
  want=$(grep -oE 'cargo install --locked cargo-deny --version [0-9.]+$' "$CI")
  got=$(grep -oE 'cargo install --locked cargo-deny --version [0-9.]+$' "$ADVISORIES" || true)
  [[ -n $want && $got == "$want" ]] || fail "advisories cargo-deny pin '$got' differs from ci's '$want'"
}

test_release_jobs_have_their_timeouts() {
  [[ -f $RELEASE ]] || { fail "no release.yml"; return 0; }
  [[ $(job_timeouts "$RELEASE") == "attest=15 build=90 checksums=45 draft=30 guard=5 notes=10 " ]] || fail "release timeouts: $(job_timeouts "$RELEASE")"
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
  grep -qF 'run: rm -f target/dist/*/SHA256SUMS target/dist/*/THIRD-PARTY-NOTICES.html' "$RELEASE" || fail "loose files not dropped"
  grep -qE '^  LINUX_BUILD_DEPS: .*libclang-dev' "$RELEASE" || fail "LINUX_BUILD_DEPS lacks libclang-dev"
}

# The build and package steps hold no secrets and no env carrying one.
test_release_build_job_has_no_secrets() {
  [[ -f $RELEASE ]] || return 0
  local build
  build=$(job_body "$RELEASE" build)
  if grep -nE 'secrets\.|environment:' <<<"$build"; then fail "the build job holds a secret"; fi
  grep -qE '^        run: bash scripts/release.sh build$' <<<"$build" || fail "no separate macOS and Windows build step"
}

# A cache written by another run could poison a signed artifact, so release jobs restore none.
test_release_restores_no_cache() {
  [[ -f $RELEASE ]] || return 0
  if grep -nE 'rust-cache|actions/cache|^[[:space:]]+cache:|enable-cache|cache-' "$RELEASE"; then fail "release.yml restores a cache"; fi
}

# zizmor audits every workflow, at a pinned version, with a read-only token and no credentials kept.
test_zizmor_audits_the_workflows() {
  [[ -f $ZIZMOR ]] || { fail "no zizmor.yml"; return 0; }
  pinned_uses_only "$ZIZMOR"
  grep -qxF '  contents: read' "$ZIZMOR" || fail "zizmor.yml is not contents: read"
  grep -qxF '          persist-credentials: false' "$ZIZMOR" || fail "zizmor.yml keeps checkout credentials"
  grep -qE '^        run: uvx zizmor@[0-9]+\.[0-9]+\.[0-9]+ ' "$ZIZMOR" || fail "zizmor version not pinned"
  grep -qE 'run: uvx zizmor@.* \.github/workflows$' "$ZIZMOR" || fail "zizmor does not audit .github/workflows"
  grep -qxF '          enable-cache: false' "$ZIZMOR" || fail "setup-uv caches"
  [[ $(job_timeouts "$ZIZMOR") == "zizmor=10 " ]] || fail "zizmor timeouts: $(job_timeouts "$ZIZMOR")"
}

# The two update-signing secrets appear once each, on the attest job's sign step env, piped from
# builtins; there is still no code-signing certificate.
test_release_holds_secrets_only_on_the_sign_step() {
  [[ -f $RELEASE ]] || return 0
  if grep -nE 'WINDOWS_CERTIFICATE|signtool' "$RELEASE"; then fail "release.yml still handles a code-signing secret"; fi
  local got step
  got=$(grep -oE 'secrets\.[A-Z_]+' "$RELEASE" | LC_ALL=C sort | tr '\n' ' ') || true
  [[ $got == "secrets.CARGO_PACKAGER_SIGN_PRIVATE_KEY secrets.CARGO_PACKAGER_SIGN_PRIVATE_KEY_PASSWORD " ]] \
    || fail "secrets in release.yml: $got"
  step=$(step_body "$RELEASE" attest 'Sign the update manifest')
  [[ -n $step ]] || { fail "the attest job has no sign step"; return 0; }
  grep -qxF '          CARGO_PACKAGER_SIGN_PRIVATE_KEY: ${{ secrets.CARGO_PACKAGER_SIGN_PRIVATE_KEY }}' <<<"$step" \
    || fail "the key is not on the sign step's env"
  grep -qxF '          CARGO_PACKAGER_SIGN_PRIVATE_KEY_PASSWORD: ${{ secrets.CARGO_PACKAGER_SIGN_PRIVATE_KEY_PASSWORD }}' <<<"$step" \
    || fail "the password is not on the sign step's env"
  grep -qF "printf '%s\n' \"\$CARGO_PACKAGER_SIGN_PRIVATE_KEY_PASSWORD\" |" <<<"$step" || fail "the password is not piped from printf"
  grep -qF -- "-s <(printf '%s' \"\$CARGO_PACKAGER_SIGN_PRIVATE_KEY\" | base64 -d)" <<<"$step" \
    || fail "the key is not read through a process substitution"
  grep -qF 'base64 -w0 update/latest.json.minisig > update/latest.json.sig' <<<"$step" || fail "no one-line .sig"
  grep -qF 'rm update/latest.json.minisig' <<<"$step" || fail "the raw .minisig is left for upload"
}

# The attest job compiles nothing and attests exactly the files the draft uploads, SHA256SUMS
# included; it alone holds the release environment, so nothing is drafted without approval.
test_release_attests_every_drafted_file() {
  [[ -f $RELEASE ]] || return 0
  local attest draft checksums
  attest=$(job_body "$RELEASE" attest)
  draft=$(job_body "$RELEASE" draft)
  checksums=$(job_body "$RELEASE" checksums)
  [[ -n $attest ]] || { fail "release.yml has no attest job"; return 0; }
  grep -qxF '      - uses: actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6 # v4.2.2' <<<"$attest" || fail "attest action pin"
  grep -qxF '          subject-path: |' <<<"$attest" || fail "attest subject-path is not a list"
  { grep -qxF '            dist/*' <<<"$attest" && grep -qxF '            update/*' <<<"$attest"; } \
    || fail "attest does not cover dist/* and update/*"
  [[ $(grep -cE '^    environment:' "$RELEASE") == 1 ]] || fail "environment: outside one job: $(grep -n 'environment:' "$RELEASE")"
  grep -qxF '    environment: release' <<<"$attest" || fail "the attest job is not bound to environment: release"
  grep -qxF "    if: github.ref_type == 'tag'" <<<"$attest" || fail "the attest job does not run only on a tag"
  local perms
  perms=$(awk '/^    permissions:$/ { on = 1; next } on && /^      [a-z-]+: [a-z]+( #.*)?$/ { sub(/ #.*/, ""); print $0; next } { on = 0 }' <<<"$attest" | LC_ALL=C sort | tr -s ' ' | tr '\n' '|')
  [[ $perms == " artifact-metadata: write| attestations: write| contents: read| id-token: write|" ]] || fail "attest permissions: $perms"
  [[ $(grep -c 'id-token: write' "$RELEASE") == 1 ]] || fail "id-token: write outside the attest job"
  # It compiles nothing: its only commands are the minisign install, the sign step and the notes check.
  [[ $(grep -cE '^        run:' <<<"$attest") == 3 ]] || fail "attest runs more than the minisign install, the sign step and the notes check"
  if grep -nE 'cargo|make |scripts/|actions/checkout' <<<"$attest"; then fail "the attest job builds or runs repository code"; fi
  grep -qxF '          sudo apt-get install -y -qq --no-install-recommends minisign=0.11-1' <<<"$attest" \
    || fail "minisign is not the pinned distro package"
  [[ $(grep -cxF "        if: hashFiles('update/latest.json') != ''" <<<"$attest") == 3 ]] \
    || fail "the minisign and notes check steps do not wait for a manifest"
  local install sign check att
  install=$(grep -nxF '      - name: Install minisign' <<<"$attest" | cut -d: -f1) || true
  sign=$(grep -nxF '      - name: Sign the update manifest' <<<"$attest" | cut -d: -f1) || true
  check=$(grep -nxF '      - name: Check the release notes' <<<"$attest" | cut -d: -f1) || true
  att=$(grep -n 'uses: actions/attest@' <<<"$attest" | cut -d: -f1) || true
  [[ -n $install && -n $sign && -n $check && -n $att && $install -lt $sign && $sign -lt $check && $check -lt $att ]] \
    || fail "attest order: install '$install', sign '$sign', check '$check', attest '$att'"
  local notes_check
  notes_check=$(step_body "$RELEASE" attest 'Check the release notes')
  if grep -nE 'secrets\.|scripts/|gh |cargo' <<<"$notes_check"; then fail "the notes check holds a secret or runs repository code"; fi
  { grep -qF 'sha256sum' <<<"$notes_check" && grep -qF 'stat -c %s' <<<"$notes_check" && grep -qF 'jq' <<<"$notes_check"; } \
    || fail "the notes check does not compare size and sha256 with jq: $notes_check"
  if grep -qxF '          path: dist' <<<"$attest$draft"; then fail "attest or draft still unpacks into dist only"; fi
  grep -qF 'name: signed-${{ needs.guard.outputs.version }}' <<<"$attest" || fail "attest does not upload the signed set"
  # One artifact carries the files from the sums to the attestation to the draft.
  grep -qF 'name: release-${{ needs.guard.outputs.version }}' <<<"$checksums" || fail "checksums does not upload the release files"
  grep -qF 'name: release-${{ needs.guard.outputs.version }}' <<<"$attest" || fail "attest does not download the release files"
  grep -qF 'name: signed-${{ needs.guard.outputs.version }}' <<<"$draft" || fail "draft does not download the signed set"
  grep -qxF '    needs: [guard, checksums]' <<<"$attest" || fail "attest does not follow checksums"
  grep -qxF '    needs: [guard, checksums, attest, notes]' <<<"$draft" || fail "draft does not wait for attest and notes"
  grep -qxF "    if: github.ref_type == 'tag' && !inputs.dry_run" <<<"$draft" || fail "draft does not run only on a tag release"
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
  grep -qE 'gh release create "\$GITHUB_REF_NAME" --repo "\$GITHUB_REPOSITORY" --draft --verify-tag --title "Dbento \$VERSION" --notes-file notes/notes\.md dist/\* update/\*$' "$RELEASE" \
    || fail "draft command"
  # gh puts a notes file before generated notes, so the two never mix.
  if grep -qF -- '--generate-notes' "$RELEASE"; then fail "release.yml still asks gh to generate notes"; fi
  grep -qF 'run: scripts/release-notes.sh files "$GITHUB_REF_NAME" notes' "$RELEASE" || fail "no step writes the notes"
  grep -q 'gh release create' <<<"$(job_body "$RELEASE" draft)" || fail "the draft command is not in the draft job"
  [[ $(grep -c 'contents: write' "$RELEASE") == 2 ]] && grep -q 'contents: write' <<<"$(job_body "$RELEASE" notes)" \
    && grep -q 'contents: write' <<<"$(job_body "$RELEASE" draft)" \
    || fail "contents: write outside the notes and draft jobs"
}

# The notes come from GitHub's generate-notes API, which needs the job token (the draft job holds contents: write).
test_release_notes_step_has_the_token() {
  [[ -f $RELEASE ]] || return 0
  local run start
  run=$(grep -nF 'run: scripts/release-notes.sh' "$RELEASE" | cut -d: -f1) || true
  [[ -n $run ]] || { fail "no step writes the notes"; return 0; }
  start=$(awk -v run="$run" 'NR < run && /^      - name:/ { start = NR } NR == run { print start; exit }' "$RELEASE")
  grep -qF 'GH_TOKEN: ${{ github.token }}' <<<"$(sed -n "${start},${run}p" "$RELEASE")" \
    || fail "the notes step has no GH_TOKEN"
  grep -q 'scripts/release-notes\.sh' <<<"$(job_body "$RELEASE" notes)" || fail "the notes step is not in the notes job"
}

# The attribution check is a shared template; it still pins every action to a commit.
test_attribution_check_is_pinned_and_reads_the_base() {
  local file=$ROOT/.github/workflows/attribution.yml
  [[ -f $file ]] || { fail "no attribution.yml"; return 0; }
  pinned_uses_only "$file"
  grep -qF 'lib=.github/scripts/attribution-lib.sh' "$file" && grep -qF 'git show "$BASE:$lib"' "$file" \
    || fail "attribution.yml does not load the lib from the base"
  [[ -f $ROOT/.github/scripts/attribution-lib.sh ]] || fail "no .github/scripts/attribution-lib.sh"
}

# What's Changed walks back to the previous tag, which a shallow checkout does not have.
test_release_notes_checkout_has_full_history() {
  [[ -f $RELEASE ]] || return 0
  grep -q 'fetch-depth: 0' <<<"$(job_body "$RELEASE" notes)" || fail "the notes job checks out without fetch-depth: 0"
}
# The notes job never compiles, holds no secret, and gates its steps rather than itself, so a dry run still reaches checksums.
test_release_notes_job_is_secret_free_and_never_skips_checksums() {
  [[ -f $RELEASE ]] || return 0
  local notes checksums
  notes=$(job_body "$RELEASE" notes)
  checksums=$(job_body "$RELEASE" checksums)
  [[ -n $notes ]] || { fail "release.yml has no notes job"; return 0; }
  grep -qxF '    needs: guard' <<<"$notes" || fail "notes does not need guard"
  if grep -qE '^    if:' <<<"$notes"; then fail "the notes job has a job-level if, which would skip checksums"; fi
  if grep -nE 'secrets\.|cargo|make |environment:' <<<"$notes"; then fail "the notes job builds or holds secrets"; fi
  [[ $(grep -cxF "        if: github.ref_type == 'tag' && !inputs.dry_run" <<<"$notes") == 2 ]] || fail "notes steps are not gated"
  grep -qF 'name: notes-${{ needs.guard.outputs.version }}' <<<"$notes" || fail "notes uploads no artifact"
  grep -qxF '    needs: [guard, build, notes]' <<<"$checksums" || fail "checksums does not wait for notes"
  grep -qF 'run: cp notes/notes.json dist/notes.json' <<<"$checksums" || fail "checksums does not add notes.json"
  local add sums
  add=$(grep -nF 'run: cp notes/notes.json dist/notes.json' "$RELEASE" | cut -d: -f1)
  sums=$(grep -nF 'run: scripts/release.sh sums dist' "$RELEASE" | cut -d: -f1)
  [[ -n $add && $add -lt $sums ]] || fail "notes.json is added after the sums"
}


# The updater lives only where the design puts it: no Apinata leftovers, no cargo packager signer,
# no repository variables, and latest.json written in checksums and signed in attest only.
test_release_updater_pieces_sit_in_checksums_and_attest() {
  [[ -f $RELEASE ]] || return 0
  if grep -niE 'apinata|choco|signer|vars\.' "$RELEASE"; then fail "Apinata, signer or variable leftovers in release.yml"; fi
  local j
  for j in guard build draft notes; do
    if grep -n 'latest\.json' <<<"$(job_body "$RELEASE" "$j")"; then fail "$j touches latest.json"; fi
  done
  local step
  step=$(step_body "$RELEASE" checksums 'Write the update manifest')
  grep -qxF "        if: github.ref_type == 'tag' && !inputs.dry_run" <<<"$step" || fail "the manifest is written outside tag releases"
  grep -qxF '        run: scripts/release.sh manifest dist update "$VERSION" "$GITHUB_SERVER_URL/$GITHUB_REPOSITORY/releases/download/$GITHUB_REF_NAME"' <<<"$step" \
    || fail "manifest command: $step"
  local sums manifest
  sums=$(grep -nF 'run: scripts/release.sh sums dist' "$RELEASE" | cut -d: -f1) || true
  manifest=$(grep -nF 'run: scripts/release.sh manifest dist update' "$RELEASE" | cut -d: -f1) || true
  [[ -n $sums && -n $manifest && $sums -lt $manifest ]] || fail "the manifest (line $manifest) does not follow the sums (line $sums)"
}

# Only the release build job marks its binaries, and every runner hands on its update payloads.
# The raw CLI is staged after packaging, because the macOS package step ad-hoc signs it in place.
test_release_build_marks_and_stages_update_files() {
  [[ -f $RELEASE ]] || return 0
  local build checksums
  build=$(job_body "$RELEASE" build)
  checksums=$(job_body "$RELEASE" checksums)
  grep -qxF '      DBENTO_RELEASE_BUILD: "1"' <<<"$build" || fail "the build job does not set DBENTO_RELEASE_BUILD"
  [[ $(grep -c 'DBENTO_RELEASE_BUILD' "$RELEASE") == 1 ]] || fail "DBENTO_RELEASE_BUILD outside the build job env"
  grep -qxF '        run: bash scripts/release.sh update-cli' <<<"$build" || fail "the build job stages no raw CLI"
  local linux package stage
  linux=$(grep -nxF '        run: make package' <<<"$build" | cut -d: -f1) || true
  package=$(grep -nxF '        run: bash scripts/release.sh package' <<<"$build" | cut -d: -f1) || true
  stage=$(grep -nxF '        run: bash scripts/release.sh update-cli' <<<"$build" | cut -d: -f1) || true
  [[ -n $linux && -n $package && -n $stage && $linux -lt $stage && $package -lt $stage ]] \
    || fail "update-cli (line $stage) does not follow both package steps (lines $linux, $package)"
  grep -qF 'name: update-${{ matrix.target }}' <<<"$build" || fail "the build job uploads no update files"
  grep -qF "path: target/update/\${{ runner.os == 'Linux' && 'linux' || runner.os == 'macOS' && 'macos' || 'windows' }}/" <<<"$build" \
    || fail "update upload path"
  { grep -qxF '          pattern: update-*' <<<"$checksums" && grep -qxF '          path: update' <<<"$checksums"; } \
    || fail "checksums does not gather the update files into update/"
  { grep -qxF '            dist/' <<<"$checksums" && grep -qxF '            update/' <<<"$checksums"; } \
    || fail "the release artifact does not carry dist/ and update/"
}

for t in $(declare -F | awk '{print $3}' | grep '^test_'); do "$t"; done
if ((FAILS > 0)); then echo "$FAILS failure(s)" >&2; exit 1; fi
echo "workflow tests passed"
