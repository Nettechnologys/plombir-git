#!/usr/bin/env bash
# The README's first command, walked the way a newcomer walks it.
#
#   scripts/trial-smoke.sh <image>
#
# Starts <image> with the documented one-liner — `docker run --rm -p 8080:8080
# -p 2222:2222 <image> plombir-git trial` — detached, named, and without `--rm`
# so a container that died still has a log to show; the trap removes it. Then
# it follows the setup link it prints: the first account
# is created with the token from that link, a repository is created, and it is
# cloned over HTTP. Nothing is prepared beforehand: no secret, no config, no
# volume (card_794de0f08b9a). Then docs/agents.md is walked on the same
# instance by scripts/agents-guide-check.sh.
#
# Run by regression.yml against the image it builds and by release.yml against
# the image it has just published. TRIAL_HTTP_PORT / TRIAL_SSH_PORT move the
# published ports when 8080 / 2222 are taken on the host.
set -euo pipefail

image="${1:?usage: scripts/trial-smoke.sh <image>}"
http_port="${TRIAL_HTTP_PORT:-8080}"
ssh_port="${TRIAL_SSH_PORT:-2222}"
name="plombir-git-trial-smoke-$$"
work="$(mktemp -d)"
base="http://127.0.0.1:${http_port}"

cleanup() {
  docker rm -f "${name}" >/dev/null 2>&1 || true
  rm -rf "${work}"
}
trap cleanup EXIT

fail() {
  echo "trial smoke: $*" >&2
  docker logs "${name}" >&2 2>&1 || true
  exit 1
}

# On the documented ports this is the README's command word for word; a moved
# HTTP port has to be named, or the printed link would point at 8080.
public_url=()
if [ "${http_port}" != 8080 ]; then
  public_url=(--public-url "http://localhost:${http_port}")
fi
docker run -d --name "${name}" \
  -p "${http_port}:8080" -p "${ssh_port}:2222" \
  "${image}" plombir-git trial "${public_url[@]}" >/dev/null

# The setup link is the one line the README tells a person to look for.
link=""
for _ in $(seq 1 120); do
  # Read whole, then search: `docker logs … | grep -q` under pipefail fails
  # whenever grep stops reading first and docker logs takes the SIGPIPE.
  logs="$(docker logs "${name}" 2>&1 || true)"
  link="$(grep -oE "http://localhost:${http_port}/register\?setup_token=[A-Za-z0-9_-]+" <<<"${logs}" | head -n 1 || true)"
  if [ -n "${link}" ]; then
    break
  fi
  sleep 1
done
[ -n "${link}" ] || fail "no setup link in the log after 120 s"
token="${link##*setup_token=}"
grep -q 'TRIAL MODE' <<<"${logs}" || fail "the log never says TRIAL MODE"

for _ in $(seq 1 60); do
  if curl -fs "${base}/readyz" >/dev/null; then
    break
  fi
  sleep 1
done
curl -fsS "${base}/readyz" >/dev/null || fail "the server never became ready"

curl -fsS "${base}/api/v1/instance" | jq -e '.trial_mode == true and .setup_required == true' >/dev/null \
  || fail "/api/v1/instance does not announce a trial waiting for its first account"
page="$(curl -fsS "${base}/register")" || fail "the setup link does not answer"
grep -qi '<!doctype html' <<<"${page}" || fail "the setup link does not open the web UI"

password="Trial-$(od -An -N12 -tx1 /dev/urandom | tr -d ' \n')"
jwt="$(curl -fsS -X POST "${base}/api/v1/users/register" \
  -H 'Content-Type: application/json' \
  -d "$(jq -n --arg password "${password}" --arg setup_token "${token}" \
    '{username:"founder",email:"founder@example.com",password:$password,setup_token:$setup_token}')" \
  | jq -er '.token')" || fail "the setup token from the link did not create the administrator"

curl -fsS -X POST "${base}/api/v1/repos" \
  -H "Authorization: Bearer ${jwt}" \
  -H 'Content-Type: application/json' \
  -d '{"name":"hello","auto_init":true}' >/dev/null || fail "could not create a repository"

GIT_TERMINAL_PROMPT=0 git clone --quiet "${base}/git/founder/hello" "${work}/hello" \
  || fail "could not clone the repository over HTTP"
[ -d "${work}/hello/.git" ] || fail "the clone has no .git"

# docs/agents.md on the same instance: the next thing the README sends a
# newcomer to do.
"$(dirname "$0")/agents-guide-check.sh" "${base}" "${jwt}" founder hello \
  || fail "docs/agents.md does not work against this instance"

echo "trial smoke: ${image} — one command, setup link, first account, repository, clone, agent"
