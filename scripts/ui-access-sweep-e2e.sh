#!/usr/bin/env bash

# One private stand, two real accounts, one scenario matrix. The browser runner
# refuses non-loopback URLs as a second boundary under the stand's mktemp+trap
# ownership, because these scenarios create repositories and mutate data.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FIXTURE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/forgekeep-ui-ldap.XXXXXX")"
FIXTURE_PID=""

cleanup_fixture() {
  if [[ -n "${FIXTURE_PID}" ]]; then
    kill -s TERM "${FIXTURE_PID}" 2>/dev/null || true
    wait "${FIXTURE_PID}" 2>/dev/null || true
  fi
  rm -rf "${FIXTURE_DIR}"
}
trap cleanup_fixture EXIT INT TERM

node "${ROOT_DIR}/scripts/ui-access-sweep-e2e.mjs" --ldap-fixture \
  >"${FIXTURE_DIR}/port" 2>"${FIXTURE_DIR}/fixture.log" &
FIXTURE_PID=$!

for _ in $(seq 1 80); do
  [[ -s "${FIXTURE_DIR}/port" ]] && break
  if ! kill -0 "${FIXTURE_PID}" 2>/dev/null; then
    echo "LDAP fixture exited before publishing its port" >&2
    cat "${FIXTURE_DIR}/fixture.log" >&2 || true
    exit 1
  fi
  sleep 0.05
done
if [[ ! -s "${FIXTURE_DIR}/port" ]]; then
  echo "LDAP fixture did not publish its port" >&2
  exit 1
fi
LDAP_PORT="$(head -n 1 "${FIXTURE_DIR}/port")"

printf '[auth]\nallow_insecure_ldap_endpoints = ["ldap://127.0.0.1:%s"]\n' \
  "${LDAP_PORT}" >"${FIXTURE_DIR}/forgekeep.toml"

STAND_REBUILD_FRONTEND=1 \
STAND_CONFIG_PATH="${FIXTURE_DIR}/forgekeep.toml" \
UI_ACCESS_SWEEP_LDAP_PORT="${LDAP_PORT}" \
"${ROOT_DIR}/scripts/ephemeral-stand.sh" \
  --frontend \
  --no-founder \
  -- \
  node "${ROOT_DIR}/scripts/ui-access-sweep-e2e.mjs"
