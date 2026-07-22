#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IRONFORGE_BIN="${IRONFORGE_BIN:-${ROOT_DIR}/target/release/ironforge}"

if [[ "${IRONFORGE_BIN}" != /* ]]; then
  IRONFORGE_BIN="${ROOT_DIR}/${IRONFORGE_BIN}"
fi

for command in curl git python3 ssh ssh-keygen; do
  if ! command -v "${command}" >/dev/null 2>&1; then
    echo "missing required command: ${command}" >&2
    exit 1
  fi
done

if [[ ! -x "${IRONFORGE_BIN}" ]]; then
  echo "IronForge binary not found: ${IRONFORGE_BIN}" >&2
  echo "build it first with: cargo build --release -p rg-cli" >&2
  exit 1
fi

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/ironforge-git-e2e.XXXXXX")"
SERVER_PID=""

cleanup() {
  local status=$?
  trap - EXIT INT TERM
  if [[ -n "${SERVER_PID}" ]] && kill -0 "${SERVER_PID}" 2>/dev/null; then
    kill "${SERVER_PID}" 2>/dev/null || true
    wait "${SERVER_PID}" 2>/dev/null || true
  fi
  if [[ ${status} -ne 0 ]]; then
    echo "git protocol E2E failed; server log follows:" >&2
    tail -200 "${WORK_DIR}/server.log" >&2 || true
  fi
  if [[ "${IRONFORGE_E2E_KEEP_TMP:-0}" == "1" ]]; then
    echo "kept E2E workspace: ${WORK_DIR}" >&2
  else
    rm -rf "${WORK_DIR}"
  fi
  exit "${status}"
}
trap cleanup EXIT INT TERM

free_port() {
  python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
}

assert_equal() {
  local expected=$1
  local actual=$2
  local message=$3
  if [[ "${expected}" != "${actual}" ]]; then
    echo "assertion failed: ${message}: expected=${expected} actual=${actual}" >&2
    return 1
  fi
}

assert_trace_v2() {
  local trace_file=$1
  local case_name=$2
  if ! grep -q "version 2" "${trace_file}"; then
    echo "${case_name} did not negotiate Protocol V2" >&2
    return 1
  fi
}

git_http() {
  local version=$1
  shift
  git \
    -c "protocol.version=${version}" \
    -c "http.extraHeader=Authorization: Bearer ${TOKEN}" \
    "$@"
}

git_ssh() {
  local version=$1
  shift
  git \
    -c "protocol.version=${version}" \
    -c "core.sshCommand=${SSH_COMMAND}" \
    "$@"
}

HTTP_PORT="$(free_port)"
SSH_PORT="$(free_port)"
while [[ "${SSH_PORT}" == "${HTTP_PORT}" ]]; do
  SSH_PORT="$(free_port)"
done

HTTP_BASE="http://127.0.0.1:${HTTP_PORT}"
USERNAME="protocol-user"
REPO_NAME="protocol-matrix"
HTTP_REPO="${HTTP_BASE}/git/${USERNAME}/${REPO_NAME}"
SSH_REPO="ssh://git@127.0.0.1:${SSH_PORT}/${USERNAME}/${REPO_NAME}"

mkdir -p "${WORK_DIR}/repos"
"${IRONFORGE_BIN}" serve \
  --repo-root "${WORK_DIR}/repos" \
  --http-addr "127.0.0.1:${HTTP_PORT}" \
  --ssh-addr "127.0.0.1:${SSH_PORT}" \
  --host-key "${WORK_DIR}/host-key" \
  --db-url "sqlite://${WORK_DIR}/ironforge.db?mode=rwc" \
  --jwt-secret "git-protocol-e2e-secret-2026" \
  >"${WORK_DIR}/server.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 120); do
  if curl -fsS "${HTTP_BASE}/health" >/dev/null 2>&1; then
    break
  fi
  if ! kill -0 "${SERVER_PID}" 2>/dev/null; then
    echo "IronForge server exited before becoming healthy" >&2
    exit 1
  fi
  sleep 0.25
done
curl -fsS "${HTTP_BASE}/health" >/dev/null

REGISTER_RESPONSE="$(curl -fsS \
  -X POST "${HTTP_BASE}/api/v1/users/register" \
  -H "Content-Type: application/json" \
  -d '{"username":"protocol-user","email":"protocol-user@example.com","password":"Qz7$wRtm"}')"
TOKEN="$(printf '%s' "${REGISTER_RESPONSE}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["token"])')"

curl -fsS \
  -X POST "${HTTP_BASE}/api/v1/repos" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer ${TOKEN}" \
  -d '{"name":"protocol-matrix","is_private":true,"default_branch":"main"}' \
  >/dev/null

ssh-keygen -q -t ed25519 -N "" -f "${WORK_DIR}/client-key"
SSH_PUBLIC_KEY="$(<"${WORK_DIR}/client-key.pub")"
export SSH_PUBLIC_KEY
python3 - <<'PY' >"${WORK_DIR}/ssh-key.json"
import json
import os
print(json.dumps({"title": "git-protocol-e2e", "key": os.environ["SSH_PUBLIC_KEY"]}))
PY
curl -fsS \
  -X POST "${HTTP_BASE}/api/v1/users/ssh-keys" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer ${TOKEN}" \
  --data-binary "@${WORK_DIR}/ssh-key.json" \
  >/dev/null

SSH_COMMAND="ssh -i ${WORK_DIR}/client-key -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR"

git init -q --initial-branch=main "${WORK_DIR}/seed"
git -C "${WORK_DIR}/seed" config user.name "Protocol Matrix"
git -C "${WORK_DIR}/seed" config user.email "protocol-matrix@example.com"
printf 'initial\n' >"${WORK_DIR}/seed/README.md"
python3 - <<'PY' >"${WORK_DIR}/seed/payload.txt"
print("ironforge-protocol-matrix-" * 4096)
PY
git -C "${WORK_DIR}/seed" add README.md payload.txt
GIT_AUTHOR_DATE="2026-01-01T00:00:00Z" \
  GIT_COMMITTER_DATE="2026-01-01T00:00:00Z" \
  git -C "${WORK_DIR}/seed" commit -q -m "initial protocol matrix commit"

echo "[1/12] HTTP V1 push"
git_http 1 -C "${WORK_DIR}/seed" push "${HTTP_REPO}" main

echo "[2/12] HTTP V1 clone and fetch"
HTTP_V1_TRACE="${WORK_DIR}/http-v1.trace"
GIT_TRACE_PACKET="${HTTP_V1_TRACE}" git_http 1 clone -q "${HTTP_REPO}" "${WORK_DIR}/http-v1"
if grep -q "version 2" "${HTTP_V1_TRACE}"; then
  echo "HTTP V1 case unexpectedly negotiated Protocol V2" >&2
  exit 1
fi
printf 'http-v1-fetch\n' >>"${WORK_DIR}/seed/README.md"
git -C "${WORK_DIR}/seed" add README.md
GIT_AUTHOR_DATE="2026-02-01T00:00:00Z" \
  GIT_COMMITTER_DATE="2026-02-01T00:00:00Z" \
  git -C "${WORK_DIR}/seed" commit -q -m "http v1 fetch commit"
git_http 1 -C "${WORK_DIR}/seed" push "${HTTP_REPO}" main
git_http 1 -C "${WORK_DIR}/http-v1" fetch -q origin
assert_equal \
  "$(git -C "${WORK_DIR}/seed" rev-parse HEAD)" \
  "$(git -C "${WORK_DIR}/http-v1" rev-parse origin/main)" \
  "HTTP V1 fetch result"

echo "[3/12] HTTP V2 clone and fetch"
HTTP_V2_TRACE="${WORK_DIR}/http-v2.trace"
GIT_TRACE_PACKET="${HTTP_V2_TRACE}" git_http 2 clone -q "${HTTP_REPO}" "${WORK_DIR}/http-v2"
assert_trace_v2 "${HTTP_V2_TRACE}" "HTTP clone"
printf 'http-v2-fetch\n' >>"${WORK_DIR}/seed/README.md"
git -C "${WORK_DIR}/seed" add README.md
GIT_AUTHOR_DATE="2026-03-01T00:00:00Z" \
  GIT_COMMITTER_DATE="2026-03-01T00:00:00Z" \
  git -C "${WORK_DIR}/seed" commit -q -m "http v2 fetch commit"
git_http 1 -C "${WORK_DIR}/seed" push "${HTTP_REPO}" main
: >"${HTTP_V2_TRACE}"
GIT_TRACE_PACKET="${HTTP_V2_TRACE}" git_http 2 -C "${WORK_DIR}/http-v2" fetch -q origin
assert_trace_v2 "${HTTP_V2_TRACE}" "HTTP fetch"
assert_equal \
  "$(git -C "${WORK_DIR}/seed" rev-parse HEAD)" \
  "$(git -C "${WORK_DIR}/http-v2" rev-parse origin/main)" \
  "HTTP V2 fetch result"

echo "[4/12] SSH V1 clone"
SSH_V1_TRACE="${WORK_DIR}/ssh-v1.trace"
GIT_TRACE_PACKET="${SSH_V1_TRACE}" git_ssh 1 clone -q "${SSH_REPO}" "${WORK_DIR}/ssh-v1"
if grep -q "version 2" "${SSH_V1_TRACE}"; then
  echo "SSH V1 case unexpectedly negotiated Protocol V2" >&2
  exit 1
fi

echo "[5/12] SSH V1 push and fetch"
git -C "${WORK_DIR}/ssh-v1" config user.name "Protocol Matrix"
git -C "${WORK_DIR}/ssh-v1" config user.email "protocol-matrix@example.com"
printf 'ssh-v1-push\n' >>"${WORK_DIR}/ssh-v1/README.md"
git -C "${WORK_DIR}/ssh-v1" add README.md
GIT_AUTHOR_DATE="2026-04-01T00:00:00Z" \
  GIT_COMMITTER_DATE="2026-04-01T00:00:00Z" \
  git -C "${WORK_DIR}/ssh-v1" commit -q -m "ssh v1 push commit"
git_ssh 1 -C "${WORK_DIR}/ssh-v1" push -q origin main
git_http 2 -C "${WORK_DIR}/http-v2" fetch -q origin
assert_equal \
  "$(git -C "${WORK_DIR}/ssh-v1" rev-parse HEAD)" \
  "$(git -C "${WORK_DIR}/http-v2" rev-parse origin/main)" \
  "SSH V1 push result"

echo "[6/12] SSH V2 clone"
SSH_V2_TRACE="${WORK_DIR}/ssh-v2.trace"
GIT_TRACE_PACKET="${SSH_V2_TRACE}" git_ssh 2 clone -q "${SSH_REPO}" "${WORK_DIR}/ssh-v2"
assert_trace_v2 "${SSH_V2_TRACE}" "SSH clone"

echo "[7/12] SSH V2 fetch"
printf 'ssh-v2-fetch\n' >>"${WORK_DIR}/ssh-v1/README.md"
git -C "${WORK_DIR}/ssh-v1" add README.md
GIT_AUTHOR_DATE="2026-05-01T00:00:00Z" \
  GIT_COMMITTER_DATE="2026-05-01T00:00:00Z" \
  git -C "${WORK_DIR}/ssh-v1" commit -q -m "ssh v2 fetch commit"
git_ssh 1 -C "${WORK_DIR}/ssh-v1" push -q origin main
: >"${SSH_V2_TRACE}"
GIT_TRACE_PACKET="${SSH_V2_TRACE}" git_ssh 2 -C "${WORK_DIR}/ssh-v2" fetch -q origin
assert_trace_v2 "${SSH_V2_TRACE}" "SSH fetch"
assert_equal \
  "$(git -C "${WORK_DIR}/ssh-v1" rev-parse HEAD)" \
  "$(git -C "${WORK_DIR}/ssh-v2" rev-parse origin/main)" \
  "SSH V2 fetch result"

echo "[8/12] HTTP V2 shallow clone, deepen, and unshallow"
HTTP_SHALLOW_TRACE="${WORK_DIR}/http-shallow.trace"
GIT_TRACE_PACKET="${HTTP_SHALLOW_TRACE}" git_http 2 clone -q --depth=1 \
  "${HTTP_REPO}" "${WORK_DIR}/http-shallow"
assert_trace_v2 "${HTTP_SHALLOW_TRACE}" "HTTP shallow clone"
assert_equal "1" "$(git -C "${WORK_DIR}/http-shallow" rev-list --count HEAD)" \
  "HTTP depth=1 commit count"
if [[ ! -s "${WORK_DIR}/http-shallow/.git/shallow" ]]; then
  echo "HTTP depth=1 clone did not record a shallow boundary" >&2
  exit 1
fi
git_http 2 -C "${WORK_DIR}/http-shallow" fetch -q --deepen=2 origin
assert_equal "3" "$(git -C "${WORK_DIR}/http-shallow" rev-list --count origin/main)" \
  "HTTP deepen=2 commit count"
git_http 2 -C "${WORK_DIR}/http-shallow" fetch -q --unshallow origin
if [[ -e "${WORK_DIR}/http-shallow/.git/shallow" ]]; then
  echo "HTTP unshallow left a shallow boundary file" >&2
  exit 1
fi
assert_equal \
  "$(git -C "${WORK_DIR}/ssh-v1" rev-list --count HEAD)" \
  "$(git -C "${WORK_DIR}/http-shallow" rev-list --count origin/main)" \
  "HTTP unshallow history"

echo "[9/12] SSH V2 shallow clone"
SSH_SHALLOW_TRACE="${WORK_DIR}/ssh-shallow.trace"
GIT_TRACE_PACKET="${SSH_SHALLOW_TRACE}" git_ssh 2 clone -q --depth=2 \
  "${SSH_REPO}" "${WORK_DIR}/ssh-shallow"
assert_trace_v2 "${SSH_SHALLOW_TRACE}" "SSH shallow clone"
assert_equal "2" "$(git -C "${WORK_DIR}/ssh-shallow" rev-list --count HEAD)" \
  "SSH depth=2 commit count"
if [[ ! -s "${WORK_DIR}/ssh-shallow/.git/shallow" ]]; then
  echo "SSH depth=2 clone did not record a shallow boundary" >&2
  exit 1
fi

echo "[10/12] HTTP V2 shallow-exclude clone"
SHALLOW_ROOT="$(git -C "${WORK_DIR}/ssh-v1" rev-list --max-parents=0 HEAD)"
git -C "${WORK_DIR}/ssh-v1" tag shallow-cut "${SHALLOW_ROOT}"
git_ssh 1 -C "${WORK_DIR}/ssh-v1" push -q origin refs/tags/shallow-cut
HTTP_EXCLUDE_TRACE="${WORK_DIR}/http-shallow-exclude.trace"
GIT_TRACE_PACKET="${HTTP_EXCLUDE_TRACE}" git_http 2 clone -q \
  --shallow-exclude=refs/tags/shallow-cut \
  "${HTTP_REPO}" "${WORK_DIR}/http-shallow-exclude"
assert_trace_v2 "${HTTP_EXCLUDE_TRACE}" "HTTP shallow-exclude clone"
assert_equal \
  "$(( $(git -C "${WORK_DIR}/ssh-v1" rev-list --count HEAD) - 1 ))" \
  "$(git -C "${WORK_DIR}/http-shallow-exclude" rev-list --count HEAD)" \
  "HTTP shallow-exclude history"
if [[ ! -s "${WORK_DIR}/http-shallow-exclude/.git/shallow" ]]; then
  echo "HTTP shallow-exclude clone did not record a shallow boundary" >&2
  exit 1
fi

echo "[11/12] HTTP V2 shallow-since clone"
HTTP_SINCE_TRACE="${WORK_DIR}/http-shallow-since.trace"
GIT_TRACE_PACKET="${HTTP_SINCE_TRACE}" git_http 2 clone -q \
  --shallow-since=2026-03-15T00:00:00Z \
  "${HTTP_REPO}" "${WORK_DIR}/http-shallow-since"
assert_trace_v2 "${HTTP_SINCE_TRACE}" "HTTP shallow-since clone"
assert_equal "2" \
  "$(git -C "${WORK_DIR}/http-shallow-since" rev-list --count HEAD)" \
  "HTTP shallow-since history"
if [[ ! -s "${WORK_DIR}/http-shallow-since/.git/shallow" ]]; then
  echo "HTTP shallow-since clone did not record a shallow boundary" >&2
  exit 1
fi

echo "[12/12] HTTP and SSH V2 partial clone filters"
HTTP_PARTIAL_TRACE="${WORK_DIR}/http-partial.trace"
GIT_TRACE_PACKET="${HTTP_PARTIAL_TRACE}" git_http 2 clone -q --no-checkout \
  --filter=blob:none "${HTTP_REPO}" "${WORK_DIR}/http-partial"
assert_trace_v2 "${HTTP_PARTIAL_TRACE}" "HTTP blob:none partial clone"
if ! grep -q "filter blob:none" "${HTTP_PARTIAL_TRACE}"; then
  echo "HTTP partial clone did not request blob:none" >&2
  exit 1
fi
assert_equal "true" \
  "$(git -C "${WORK_DIR}/http-partial" config --get remote.origin.promisor)" \
  "HTTP promisor configuration"
if ! GIT_NO_LAZY_FETCH=1 git -C "${WORK_DIR}/http-partial" \
  rev-list --objects --missing=print HEAD | grep -q '^?'; then
  echo "HTTP blob:none clone did not omit any objects" >&2
  exit 1
fi
HTTP_PAYLOAD_OID="$(git -C "${WORK_DIR}/http-partial" rev-parse HEAD:payload.txt)"
HTTP_LAZY_TRACE="${WORK_DIR}/http-partial-lazy.trace"
GIT_TRACE_PACKET="${HTTP_LAZY_TRACE}" git_http 2 \
  -C "${WORK_DIR}/http-partial" checkout -q main
if ! grep -q "want ${HTTP_PAYLOAD_OID}" "${HTTP_LAZY_TRACE}"; then
  echo "HTTP checkout did not lazily request the missing payload blob" >&2
  exit 1
fi
git -C "${WORK_DIR}/http-partial" cat-file -e "${HTTP_PAYLOAD_OID}"
test -f "${WORK_DIR}/http-partial/payload.txt"

SSH_PARTIAL_TRACE="${WORK_DIR}/ssh-partial.trace"
GIT_TRACE_PACKET="${SSH_PARTIAL_TRACE}" git_ssh 2 clone -q --no-checkout \
  --filter=tree:0 "${SSH_REPO}" "${WORK_DIR}/ssh-partial"
assert_trace_v2 "${SSH_PARTIAL_TRACE}" "SSH tree:0 partial clone"
git -C "${WORK_DIR}/ssh-partial" config core.sshCommand "${SSH_COMMAND}"
git -C "${WORK_DIR}/ssh-partial" config protocol.version 2
if ! grep -q "filter tree:0" "${SSH_PARTIAL_TRACE}"; then
  echo "SSH partial clone did not request tree:0" >&2
  exit 1
fi
if ! GIT_NO_LAZY_FETCH=1 git -C "${WORK_DIR}/ssh-partial" \
  rev-list --objects --missing=print HEAD | grep -q '^?'; then
  echo "SSH tree:0 clone did not omit any objects" >&2
  exit 1
fi
SSH_LAZY_TRACE="${WORK_DIR}/ssh-partial-lazy.trace"
GIT_TRACE_PACKET="${SSH_LAZY_TRACE}" git_ssh 2 \
  -C "${WORK_DIR}/ssh-partial" checkout -q main
if ! grep -q "want " "${SSH_LAZY_TRACE}"; then
  echo "SSH checkout did not lazily request missing objects" >&2
  exit 1
fi
git -C "${WORK_DIR}/ssh-partial" cat-file -e 'HEAD^{tree}'
test -f "${WORK_DIR}/ssh-partial/payload.txt"

echo "Git protocol E2E matrix passed:"
echo "  HTTP V1: clone/fetch/push"
echo "  HTTP V2: clone/fetch"
echo "  SSH V1: clone/fetch/push"
echo "  SSH V2: clone/fetch"
echo "  shallow/deepen: HTTP clone/deepen/unshallow/exclude/since and SSH clone"
echo "  partial clone filter: HTTP blob:none and SSH tree:0 with lazy fetch"
