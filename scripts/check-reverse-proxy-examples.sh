#!/usr/bin/env bash
# Validates the reverse-proxy examples deploy/README.md tells operators to copy:
# `nginx -t` on deploy/nginx.conf.example, `caddy validate` on
# deploy/Caddyfile.example, each in the official image.
#
# Why this exists: the deployment guide said "put a TLS-terminating reverse
# proxy in front" and shipped no configuration for one, so every operator wrote
# their own, and the settings a forge needs beyond a stock proxy — no body
# ceiling for pushes, unbuffered streaming, long timeouts, the WebSocket
# upgrade, X-Forwarded-For — were each left to be discovered by a failure
# (card_b7bfdbf0b00d). An example nobody loads drifts into one that does not
# parse, so both are loaded here.
#
# One script, run by the `deploy-config` job of regression.yml and by its local
# mirror in scripts/run-local-gates.mjs, so the two cannot disagree on what
# "valid" means.
#
# The images are pinned, not `latest`: a floating tag would turn this red on an
# unrelated push the day a proxy release changes its grammar. A version tag is
# not enough either — `1.27-alpine` is rebuilt under the same name — so each
# carries the digest of its multi-platform index. Both pins live in
# ci/images/Dockerfile, the file Dependabot watches for them, and are
# read from there: a copy here would be the one no updater ever proposes to bump
# (card_2d33ee054ad2). scripts/ci-image-pins-contract-check.mjs holds that file
# to one `FROM <image>@sha256:<digest> AS <name>` per line, which is the only
# shape the read below understands.
set -euo pipefail

cd "$(dirname "$0")/.."

CI_IMAGES=ci/images/Dockerfile

ci_image() {
  local image
  image="$(awk -v stage="$1" '$1 == "FROM" && $3 == "AS" && $4 == stage { print $2 }' "${CI_IMAGES}")"
  if [ -z "${image}" ] || [ "$(printf '%s\n' "${image}" | wc -l)" -ne 1 ]; then
    echo "${CI_IMAGES} must pin exactly one \`FROM <image> AS $1\`." >&2
    exit 1
  fi
  printf '%s\n' "${image}"
}

NGINX_IMAGE="$(ci_image nginx)"
CADDY_IMAGE="$(ci_image caddy)"

for example in deploy/nginx.conf.example deploy/Caddyfile.example; do
  if [ ! -f "${example}" ]; then
    echo "${example} is gone — deploy/README.md still points operators at it." >&2
    exit 1
  fi
done

scratch="$(mktemp -d)"
trap 'rm -rf "${scratch}"' EXIT

# `nginx -t` loads the certificate it is pointed at, so the example's certbot
# paths get a throwaway self-signed pair. The image has no openssl of its own.
mkdir -p "${scratch}/live"
openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj "/CN=git.example.com" \
  -keyout "${scratch}/live/privkey.pem" -out "${scratch}/live/fullchain.pem" 2>/dev/null
chmod 0644 "${scratch}/live/privkey.pem"

echo "Validating deploy/nginx.conf.example with ${NGINX_IMAGE}"
docker run --rm \
  -v "${PWD}/deploy/nginx.conf.example:/etc/nginx/conf.d/plombir-git.conf:ro" \
  -v "${scratch}/live:/etc/letsencrypt/live/git.example.com:ro" \
  "${NGINX_IMAGE}" nginx -t

echo "Validating deploy/Caddyfile.example with ${CADDY_IMAGE}"
docker run --rm \
  -v "${PWD}/deploy/Caddyfile.example:/etc/caddy/Caddyfile:ro" \
  "${CADDY_IMAGE}" caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
