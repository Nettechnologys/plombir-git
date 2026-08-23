#!/usr/bin/env bash
#
# Bring up a throwaway ForgeKeep — own binary, empty database, temporary
# `repo_root`, and optionally the built frontend behind `vite preview` — print
# the URLs, and take all of it down again on the way out.
#
#   scripts/ephemeral-stand.sh --frontend                       # hold it open
#   scripts/ephemeral-stand.sh --frontend --no-founder -- <e2e> # register in UI
#   scripts/ephemeral-stand.sh --frontend -- npm run smoke:admin-browser
#   scripts/ephemeral-stand.sh -- curl -fsS "$STAND_BACKEND_URL/health"
#
# With a command after `--` the stand runs it with the addresses exported and
# exits with the command's status; with no command it prints the addresses and
# waits, so a browser or a debugger can attach by hand.
#
# The boot sequence itself lives in `scripts/lib/stand.sh`, shared
# with `scripts/git-protocol-e2e.sh` — see the header there for why an e2e run
# must never point at a real instance.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STAND_ROOT_DIR="${ROOT_DIR}"
# shellcheck source=lib/stand.sh
source "${ROOT_DIR}/scripts/lib/stand.sh"

WITH_FRONTEND=0
BUILD_BINARY=0
REGISTER_FOUNDER=1
COMMAND=()

while (($# > 0)); do
  case "$1" in
    --frontend) WITH_FRONTEND=1 ;;
    --build) BUILD_BINARY=1 ;;
    --no-founder) REGISTER_FOUNDER=0 ;;
    --) shift; COMMAND=("$@"); break ;;
    -h|--help)
      sed -n '2,17p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "stand: unknown argument: $1" >&2
      exit 2
      ;;
  esac
  shift
done

stand_require_commands curl python3 ps mktemp

if [[ ${BUILD_BINARY} -eq 1 ]]; then
  echo "stand: building target/release/forgekeep" >&2
  (cd "${ROOT_DIR}" && cargo build --release -p rg-cli -j 6)
fi

# Printed before teardown runs, because teardown is where the logs go.
stand_on_failure() {
  echo "stand: failed with status $1" >&2
  if [[ -n "${STAND_SERVER_LOG:-}" && -f "${STAND_SERVER_LOG}" ]]; then
    echo "stand: server log:" >&2
    tail -60 "${STAND_SERVER_LOG}" >&2 || true
  fi
  if [[ -n "${STAND_FRONTEND_LOG:-}" && -f "${STAND_FRONTEND_LOG}" ]]; then
    echo "stand: frontend log:" >&2
    tail -40 "${STAND_FRONTEND_LOG}" >&2 || true
  fi
}

stand_open
stand_start_backend
STAND_USERNAME=""
STAND_TOKEN=""
STAND_PASSWORD=""
if [[ ${REGISTER_FOUNDER} -eq 1 ]]; then
  stand_register_founder "${STAND_USER:-stand-founder}"
fi
if [[ ${WITH_FRONTEND} -eq 1 ]]; then
  stand_start_frontend
fi

# One file a consumer can source, so a caller in another language does not have
# to scrape this output. It lives in the workspace and dies with it.
STAND_ENV_FILE="${STAND_WORK_DIR}/stand.env"
{
  printf 'BACKEND_URL=%s\n' "${STAND_BACKEND_URL}"
  printf 'API_BASE=%s/api/v1\n' "${STAND_BACKEND_URL}"
  printf 'STAND_BACKEND_URL=%s\n' "${STAND_BACKEND_URL}"
  printf 'STAND_SSH_ADDR=%s\n' "${STAND_SSH_ADDR}"
  printf 'STAND_USERNAME=%s\n' "${STAND_USERNAME}"
  printf 'STAND_TOKEN=%s\n' "${STAND_TOKEN}"
  printf 'STAND_WORK_DIR=%s\n' "${STAND_WORK_DIR}"
  if [[ -n "${STAND_FRONTEND_URL:-}" ]]; then
    printf 'FRONTEND_URL=%s\n' "${STAND_FRONTEND_URL}"
    printf 'STAND_FRONTEND_URL=%s\n' "${STAND_FRONTEND_URL}"
  fi
} >"${STAND_ENV_FILE}"

echo "stand: backend=${STAND_BACKEND_URL}"
if [[ -n "${STAND_FRONTEND_URL:-}" ]]; then
  echo "stand: frontend=${STAND_FRONTEND_URL}"
else
  echo "stand: frontend=(not started; pass --frontend)"
fi
echo "stand: ssh=${STAND_SSH_ADDR}"
if [[ -n "${STAND_USERNAME}" ]]; then
  echo "stand: user=${STAND_USERNAME}"
else
  echo "stand: user=(not created; the consumer owns registration)"
fi
echo "stand: env=${STAND_ENV_FILE}"

if ((${#COMMAND[@]} > 0)); then
  export BACKEND_URL="${STAND_BACKEND_URL}"
  export API_BASE="${STAND_BACKEND_URL}/api/v1"
  export STAND_BACKEND_URL STAND_SSH_ADDR STAND_USERNAME STAND_TOKEN STAND_WORK_DIR STAND_ENV_FILE
  if [[ -n "${STAND_FRONTEND_URL:-}" ]]; then
    export FRONTEND_URL="${STAND_FRONTEND_URL}"
    export STAND_FRONTEND_URL
  fi
  status=0
  "${COMMAND[@]}" || status=$?
  exit "${status}"
fi

echo "stand: ready — press Ctrl-C to tear it down"
while true; do
  sleep 1
done
