# shellcheck shell=bash
#
# Shared loader for an ephemeral Plombir Git stand: its own binary, an empty
# database, a throwaway `repo_root`, and — when asked for — the built frontend
# behind `vite preview`, so a browser can drive the real product.
#
# Why this is a library and not a copy: `scripts/git-protocol-e2e.sh` grew the
# only working spelling of "boot a private Plombir Git and wait until it is
# actually up" — ephemeral ports published through `--listen-address-file`, a
# health poll that notices a server which exited instead of polling until the
# timeout, and a trap that takes the process down with the script. A browser
# test needs every one of those plus a frontend, and a second copy of that
# sequence is a second place for the boot to rot silently.
#
# What "ephemeral" has to mean here, stated because the alternative is not a
# smaller test but a destructive one: the e2e scenarios this loads create
# repositories, issues and users, and assert counts over them ("the list holds
# exactly one issue"). Run twice against a surviving database and the second run
# is red through no fault of the product. Run against the live instance at
# `git.bearby.io` and the test deletes real repositories that no daily backup
# covers. So every stand gets a fresh `mktemp -d`, and the trap removes it.
#
# The server is started with its working directory INSIDE that temporary
# directory. Every path handed to it below is absolute, but the defaults it
# falls back to are not (`./data/…`, `./repos`), and a stand that scatters those
# into the checkout is not ephemeral no matter what its `--db-url` says.
#
# What the stand does NOT give you is an instance admin. Registration on an
# empty instance succeeds even when the instance is closed — that bootstrap
# window is what lets the stand create its founder account without editing any
# config — but the account it creates is an ordinary user: `is_admin` is written
# `false` by `rg_core::user::service::register`, and the only production writer
# that sets it true is the admin user-update handler (`update_user` in
# `crates/rg-http/src/api/admin.rs`), which already requires an instance admin.
# So the admin surface cannot be reached from a fresh database at all, by a test
# or by an operator, and pretending otherwise here would hide that behind a
# shell workaround. Tracked on card_6366c347b9bc.
#
# Interface for a sourcing script:
#   stand_require_commands git curl …   — fail fast on a missing dependency
#   stand_open                          — temporary workspace + teardown trap
#   stand_start_backend                 — sets STAND_BACKEND_URL / STAND_SSH_ADDR
#   stand_register_founder <user> <mail>— sets STAND_TOKEN / STAND_USERNAME
#   stand_start_frontend                — sets STAND_FRONTEND_URL
#   stand_cleanup                       — idempotent teardown (the trap calls it)
# Set STAND_CONFIG_PATH before `stand_start_backend` to add one explicit
# `--config` file while retaining the stand-owned database/repo/listen flags.
# A script that defines `stand_on_failure` gets it called, with the exit status,
# before teardown removes the logs it wants to print.

STAND_PIDS=()
STAND_CLEANED=0

stand_require_commands() {
  local missing=()
  local command
  for command in "$@"; do
    if ! command -v "${command}" >/dev/null 2>&1; then
      missing+=("${command}")
    fi
  done
  if ((${#missing[@]} > 0)); then
    echo "stand: missing required command(s): ${missing[*]}" >&2
    return 1
  fi
}

stand_open() {
  : "${STAND_ROOT_DIR:="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"}"
  # Hex, and handed over through the environment rather than the command line.
  # Two reasons, both learnt the hard way: a `token_urlsafe` secret may begin
  # with `-`, and clap then reads the VALUE of `--jwt-secret` as a flag cluster
  # (the stand died with `unexpected argument '-Q' found` on roughly one boot in
  # ten); and a secret spelled on the command line is readable in `ps` by every
  # account on the host for as long as the stand runs.
  : "${STAND_JWT_SECRET:="$(python3 -c 'import secrets; print(secrets.token_hex(32))')"}"
  STAND_WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/plombir-git-stand.XXXXXX")"
  mkdir -p "${STAND_WORK_DIR}/repos"
  trap 'stand_trap' EXIT INT TERM
}

stand_trap() {
  local status=$?
  trap - EXIT INT TERM
  if [[ ${status} -ne 0 ]] && declare -F stand_on_failure >/dev/null; then
    stand_on_failure "${status}"
  fi
  stand_cleanup
  exit "${status}"
}

# Start a background child, remembering enough about it to kill whatever it
# spawns. `vite preview` is one node process today, but a wrapper around it
# (npm, a shell) leaves the real server behind when only the wrapper is
# signalled — the orphan then holds its port until somebody notices. `setsid`
# puts the child in its own process group so teardown can signal the group.
stand_spawn() {
  local log=$1
  local cwd=$2
  shift 2

  if command -v setsid >/dev/null 2>&1; then
    (cd "${cwd}" && exec setsid "$@" >"${log}" 2>&1) &
  else
    (cd "${cwd}" && exec "$@" >"${log}" 2>&1) &
  fi
  STAND_LAST_PID=$!
  STAND_PIDS+=("${STAND_LAST_PID}")
}

# Signal the child's whole process group when it has one of its own, and the
# child alone when it does not — a stand without `setsid` shares its group with
# this script, and `kill -- -$$` would take the test down with the server.
stand_stop_process() {
  local pid=$1
  kill -0 "${pid}" 2>/dev/null || return 0

  local own_pgid child_pgid target
  own_pgid="$(ps -o pgid= -p $$ 2>/dev/null | tr -d ' ')"
  child_pgid="$(ps -o pgid= -p "${pid}" 2>/dev/null | tr -d ' ')"
  target="${pid}"
  if [[ -n "${child_pgid}" && -n "${own_pgid}" && "${child_pgid}" != "${own_pgid}" ]]; then
    target="-${child_pgid}"
  fi

  kill -s TERM -- "${target}" 2>/dev/null || true
  local _
  for _ in $(seq 1 50); do
    kill -0 "${pid}" 2>/dev/null || break
    sleep 0.1
  done
  if kill -0 "${pid}" 2>/dev/null; then
    kill -s KILL -- "${target}" 2>/dev/null || true
  fi
  wait "${pid}" 2>/dev/null || true
}

stand_cleanup() {
  [[ ${STAND_CLEANED} -eq 1 ]] && return 0
  STAND_CLEANED=1

  local pid
  for pid in "${STAND_PIDS[@]:-}"; do
    [[ -n "${pid}" ]] && stand_stop_process "${pid}"
  done
  STAND_PIDS=()

  if [[ -n "${STAND_WORK_DIR:-}" ]]; then
    if [[ "${PLOMBIR_GIT_STAND_KEEP_TMP:-0}" == "1" ]]; then
      echo "stand: kept workspace ${STAND_WORK_DIR}" >&2
    else
      rm -rf "${STAND_WORK_DIR}"
    fi
  fi
}

# Poll `condition` until it holds, giving up the moment the process it belongs
# to has exited. Polling to the full timeout after the server is already dead
# turns a crash-on-startup into a timeout message that names the wrong problem.
stand_wait_for() {
  local pid=$1
  local what=$2
  shift 2

  local _
  for _ in $(seq 1 "${STAND_WAIT_TICKS:-240}"); do
    if "$@"; then
      return 0
    fi
    if ! kill -0 "${pid}" 2>/dev/null; then
      echo "stand: ${what} — the process exited first" >&2
      return 1
    fi
    sleep 0.25
  done
  echo "stand: timed out waiting for ${what}" >&2
  return 1
}

stand_resolve_binary() {
  STAND_BIN="${PLOMBIR_GIT_BIN:-${STAND_ROOT_DIR}/target/release/plombir-git}"
  if [[ "${STAND_BIN}" != /* ]]; then
    STAND_BIN="${STAND_ROOT_DIR}/${STAND_BIN}"
  fi
  if [[ ! -x "${STAND_BIN}" ]]; then
    echo "stand: Plombir Git binary not found: ${STAND_BIN}" >&2
    echo "stand: build it first with: cargo build --release -p rg-cli -j 6" >&2
    return 1
  fi
}

stand_addresses_published() {
  [[ -s "${STAND_LISTEN_FILE}" ]] || return 1
  local transport address
  while IFS='=' read -r transport address; do
    case "${transport}" in
      http) STAND_HTTP_ADDR="${address}" ;;
      ssh) STAND_SSH_ADDR="${address}" ;;
    esac
  done <"${STAND_LISTEN_FILE}"
  [[ -n "${STAND_HTTP_ADDR:-}" && -n "${STAND_SSH_ADDR:-}" ]]
}

stand_backend_healthy() {
  curl -fsS "${STAND_BACKEND_URL}/health" >/dev/null 2>&1
}

stand_start_backend() {
  stand_resolve_binary || return 1

  STAND_LISTEN_FILE="${STAND_WORK_DIR}/listen-addresses"
  STAND_SERVER_LOG="${STAND_WORK_DIR}/server.log"
  STAND_HTTP_ADDR=""
  STAND_SSH_ADDR=""

  local config_args=()
  if [[ -n "${STAND_CONFIG_PATH:-}" ]]; then
    if [[ "${STAND_CONFIG_PATH}" != /* || ! -f "${STAND_CONFIG_PATH}" ]]; then
      echo "stand: STAND_CONFIG_PATH must name an existing absolute file" >&2
      return 1
    fi
    config_args=(--config "${STAND_CONFIG_PATH}")
  fi

  export PLOMBIR_GIT_JWT_SECRET="${STAND_JWT_SECRET}"
  stand_spawn "${STAND_SERVER_LOG}" "${STAND_WORK_DIR}" \
    "${STAND_BIN}" serve \
    "${config_args[@]}" \
    --repo-root "${STAND_WORK_DIR}/repos" \
    --http-addr "127.0.0.1:0" \
    --ssh-addr "127.0.0.1:0" \
    --listen-address-file "${STAND_LISTEN_FILE}" \
    --host-key "${STAND_WORK_DIR}/host-key" \
    --db-url "sqlite://${STAND_WORK_DIR}/plombir-git.db?mode=rwc"
  STAND_SERVER_PID="${STAND_LAST_PID}"

  stand_wait_for "${STAND_SERVER_PID}" "the server to publish its listen addresses" \
    stand_addresses_published || return 1
  STAND_BACKEND_URL="http://${STAND_HTTP_ADDR}"
  stand_wait_for "${STAND_SERVER_PID}" "the server to answer /health" \
    stand_backend_healthy || return 1
}

# Create the account the stand acts as. On an empty instance this succeeds even
# with registration closed, which is what makes the stand usable against a build
# whose config the test is not allowed to touch.
stand_register_founder() {
  STAND_USERNAME=${1:-stand-founder}
  local email=${2:-${STAND_USERNAME}@example.com}
  local password=${STAND_PASSWORD:-Qz7\$wRtm}
  STAND_PASSWORD="${password}"

  STAND_USERNAME="${STAND_USERNAME}" STAND_EMAIL="${email}" STAND_PW="${password}" \
    python3 -c 'import json, os; print(json.dumps({"username": os.environ["STAND_USERNAME"], "email": os.environ["STAND_EMAIL"], "password": os.environ["STAND_PW"]}))' \
    >"${STAND_WORK_DIR}/register.json"

  local response
  response="$(curl -fsS -X POST "${STAND_BACKEND_URL}/api/v1/users/register" \
    -H "Content-Type: application/json" \
    --data-binary "@${STAND_WORK_DIR}/register.json")" || {
    echo "stand: registering ${STAND_USERNAME} failed" >&2
    return 1
  }
  STAND_TOKEN="$(printf '%s' "${response}" | python3 -c 'import json, sys; print(json.load(sys.stdin)["token"])')"
  [[ -n "${STAND_TOKEN}" ]] || {
    echo "stand: registration returned no token" >&2
    return 1
  }
}

# vite colours its banner whenever `CI` is set, TTY or not (picocolors reads
# the variable), and the colour codes land INSIDE the URL: on GitHub Actions
# the line is `http://127.0.0.1:\e[1m40523\e[22m/`. A pattern over the raw log
# never matched there, and the browser sweep timed out "waiting for vite
# preview to publish its URL" next to a log that showed the URL. The preview
# runs with NO_COLOR, and the codes are stripped here as well, so neither half
# alone decides whether the stand comes up.
stand_frontend_published() {
  [[ -s "${STAND_FRONTEND_LOG}" ]] || return 1
  local url
  url="$(sed 's/\x1b\[[0-9;]*m//g' "${STAND_FRONTEND_LOG}" \
    | grep -oE 'http://(127\.0\.0\.1|localhost):[0-9]+' | head -1)"
  [[ -n "${url}" ]] || return 1
  STAND_FRONTEND_URL="${url/localhost/127.0.0.1}"
  return 0
}

stand_frontend_reachable() {
  curl -fsS "${STAND_FRONTEND_URL}/" >/dev/null 2>&1
}

# `vite preview` serves the built SPA, and the SPA calls its backend on the
# ORIGIN IT WAS LOADED FROM: `web/src/lib/api/_base.svelte.ts` defaults the API
# base to the relative `/api/v1`. So the preview server has to proxy that path
# to this stand's backend — a preview without the proxy boots, serves every
# page, and answers every API call with its own 404, which is a stand that
# tests nothing while looking healthy. `web/vite.config.ts` reads the target out
# of PLOMBIR_GIT_BACKEND_ORIGIN for exactly this reason: the stand's port is only
# known at run time.
stand_start_frontend() {
  local web_dir="${STAND_ROOT_DIR}/web"
  local vite_bin="${web_dir}/node_modules/vite/bin/vite.js"

  if [[ ! -f "${vite_bin}" ]]; then
    echo "stand: ${vite_bin} is missing — run 'npm ci' in web/ first" >&2
    return 1
  fi
  if [[ "${STAND_REBUILD_FRONTEND:-0}" == "1" || ! -f "${web_dir}/build/index.html" ]]; then
    echo "stand: building the frontend (web/build)" >&2
    (cd "${web_dir}" && npm run build) >"${STAND_WORK_DIR}/frontend-build.log" 2>&1 || {
      echo "stand: frontend build failed; log follows:" >&2
      tail -40 "${STAND_WORK_DIR}/frontend-build.log" >&2
      return 1
    }
  fi

  STAND_FRONTEND_LOG="${STAND_WORK_DIR}/frontend.log"
  export PLOMBIR_GIT_BACKEND_ORIGIN="${STAND_BACKEND_URL}"
  stand_spawn "${STAND_FRONTEND_LOG}" "${web_dir}" \
    env NO_COLOR=1 node "${vite_bin}" preview --host 127.0.0.1 --port "${STAND_FRONTEND_PORT:-0}"
  STAND_FRONTEND_PID="${STAND_LAST_PID}"

  stand_wait_for "${STAND_FRONTEND_PID}" "vite preview to publish its URL" \
    stand_frontend_published || return 1
  stand_wait_for "${STAND_FRONTEND_PID}" "vite preview to serve the app" \
    stand_frontend_reachable || return 1
}
