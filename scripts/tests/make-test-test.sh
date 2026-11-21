#!/usr/bin/env bash
# Checks the `make test` recipe against a stub cargo: shared servers around the workspace pass,
# the testkit pass without their URLs, removal on every exit, and the first failure's status.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
FAILS=0
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# The stub logs each call with the Postgres URL it saw and its parent pid, prints two export lines
# for `up`, and exits with the status the test asks of each step.
cat >"$TMP/cargo" <<'EOF'
#!/usr/bin/env bash
printf '%s | pg=%s | ppid=%s\n' "$*" "${DBENTO_PG_URL-unset}" "$PPID" >>"$STUB_LOG"
case "$*" in
  "xtask test-servers up "*)
    [[ -n ${STUB_UP_STATUS:-} ]] && exit "$STUB_UP_STATUS"
    echo "export DBENTO_PG_URL=postgres://stub/postgres"
    echo "export DBENTO_MYSQL_URL=mysql://stub/" ;;
  "xtask test-servers down "*) exit "${STUB_DOWN_STATUS:-0}" ;;
  "test --workspace")
    [[ -n ${STUB_TERM:-} ]] && kill -TERM "$PPID"
    exit "${STUB_WS_STATUS:-0}" ;;
  "test -p dbento-testkit --all-features") exit "${STUB_KIT_STATUS:-0}" ;;
  *) echo "unexpected cargo $*" >&2; exit 99 ;;
esac
EOF
chmod +x "$TMP/cargo"

# Runs `make test` on the stub with the given NAME=VALUE settings; prints make's output.
run() {
  : >"$TMP/log"
  env -u DBENTO_PG_URL -u DBENTO_MYSQL_URL STUB_LOG="$TMP/log" "$@" \
    make -s -C "$ROOT" test CARGO="$TMP/cargo" LOCKED= 2>&1
}

# The log without owner pids and parent pids, so it compares as text.
calls() { sed -E 's/--owner [0-9]+/--owner N/; s/ \| ppid=[0-9]+$//' "$TMP/log"; }

test_the_workspace_runs_on_shared_servers_and_the_testkit_without_them() {
  local out want line3
  out=$(run) || fail "passing run failed: $out"
  want=$(cat <<'EOF'
xtask test-servers up --owner N | pg=unset
test --workspace | pg=postgres://stub/postgres
xtask test-servers down --owner N | pg=postgres://stub/postgres
test -p dbento-testkit --all-features | pg=unset
xtask test-servers down --owner N | pg=postgres://stub/postgres
EOF
)
  [[ $(calls) == "$want" ]] || fail "calls: $(calls)"
  # The owner is the recipe's shell, the parent of the cargo that `down` runs.
  line3=$(sed -n 3p "$TMP/log")
  [[ $line3 =~ --owner\ ([0-9]+).*ppid=([0-9]+)$ ]] || fail "no owner or ppid: $line3"
  [[ ${BASH_REMATCH[1]} == "${BASH_REMATCH[2]}" ]] || fail "owner is not the recipe shell: $line3"
  [[ $(grep -oE -- '--owner [0-9]+' "$TMP/log" | sort -u | wc -l) -eq 1 ]] || fail "owners differ: $(cat "$TMP/log")"
}

test_the_exit_status_is_the_first_failing_pass() {
  local out
  if out=$(run STUB_WS_STATUS=101 STUB_KIT_STATUS=7); then fail "failing workspace passed"; fi
  [[ $out == *"Error 101"* ]] || fail "workspace status lost: $out"
  grep -q '^test -p dbento-testkit' "$TMP/log" || fail "testkit pass skipped after a workspace failure"
  if out=$(run STUB_KIT_STATUS=7); then fail "failing testkit passed"; fi
  [[ $out == *"Error 7"* ]] || fail "testkit status lost: $out"
}

test_a_failed_start_runs_no_tests() {
  local out
  if out=$(run STUB_UP_STATUS=3); then fail "failed start passed"; fi
  [[ $out == *"Error 3"* ]] || fail "start status lost: $out"
  if grep -q '^test ' "$TMP/log"; then fail "tests ran without servers: $(cat "$TMP/log")"; fi
}

test_a_failed_removal_fails_a_passing_run() {
  local out
  if out=$(run STUB_DOWN_STATUS=1); then fail "failed removal passed"; fi
  [[ $out == *"Error 1"* ]] || fail "removal status lost: $out"
}

test_term_still_removes_the_servers() {
  local out
  if out=$(run STUB_TERM=1); then fail "terminated run passed"; fi
  [[ $out == *"Error 143"* ]] || fail "TERM status lost: $out"
  [[ $(calls | tail -n 1) == "xtask test-servers down --owner N | pg=postgres://stub/postgres" ]] \
    || fail "no removal after TERM: $(calls)"
  if grep -q '^test -p dbento-testkit' "$TMP/log"; then fail "testkit pass ran after TERM"; fi
}

for t in $(declare -F | awk '{print $3}' | grep '^test_'); do "$t"; done
if ((FAILS > 0)); then echo "$FAILS failure(s)" >&2; exit 1; fi
echo "make test recipe tests passed"
