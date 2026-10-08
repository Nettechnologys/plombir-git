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
# unrelated push the day a proxy release changes its grammar.
set -euo pipefail

NGINX_IMAGE=nginx:1.27-alpine
CADDY_IMAGE=caddy:2.8.4

cd "$(dirname "$0")/.."

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
