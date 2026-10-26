# Third-party images pinned where no Dependabot ecosystem looks, pinned here
# too so Dependabot can see them.
#
# Never built. Each line is the one place one image is pinned, by the digest of
# its multi-platform index: a version tag alone is rebuilt under the same name,
# so a rerun of one commit could test against a different server or proxy.
#
# Dependabot has no ecosystem for a workflow's `services.*.image` or for a
# variable in a shell script, and a digest pinned where no updater looks stays
# on the patch level it was pinned at until somebody remembers it. Dependabot's
# `docker` ecosystem does read a Dockerfile, so `.github/dependabot.yml` points
# one at this directory and every bump arrives as a reviewable pull request
# (card_2d33ee054ad2).
#
# Not under `.github/`. It lived in `.github/ci-images/` from 2026-10-16, and
# every weekly run found the bumps (nginx 1.31, caddy 2.11, forgejo 17) and then
# failed to open the pull request with `400 The request contains invalid or
# unauthorized changes`: only the github-actions ecosystem gets the `workflows`
# permission a change under `.github/` needs (dependabot-core#15237), so no
# docker update for this file ever arrived.
#
# Who reads which line:
#   postgres, mysql  `services:` of the postgres-smoke and mysql-smoke jobs in
#                    `.github/workflows/regression.yml`. A service image cannot
#                    be read from a file, so the workflow repeats the reference
#                    and `scripts/ci-image-pins-contract-check.mjs` fails while
#                    the two differ: a Dependabot pull request for one of these
#                    is red until the same reference is copied into the
#                    workflow, and the message names the line to change.
#   nginx, caddy     `scripts/check-reverse-proxy-examples.sh` reads them from
#                    here, so their pull requests need nothing else.
#   dockerfile-frontend
#                    the `# syntax=` parser directive on the first line of the
#                    repository's `Dockerfile`. Dependabot's `docker` entry for
#                    that file updates its `FROM` lines and not the directive,
#                    and a directive cannot be read from anywhere else, so it is
#                    repeated there and held to this line the same way.
#   gitea, forgejo   `scripts/gitea-import-live.sh` reads them from here: the
#                    servers the Regression job imports from live
#                    (card_7f2bdd581d17). A bump is the point — a release that
#                    changes the shape of an answer turns that job red in the
#                    bump's own pull request, not in a user's import.
#
# One `FROM <image>:<tag>@sha256:<digest> AS <name>` per line and nothing else:
# the check refuses any other shape rather than guess what a reader would take.
FROM postgres:18@sha256:edb0c08dc33befd0262ab3b5cf1c4f3f8cfd8b09e88c05b45adee29d6761ddb6 AS postgres
FROM mysql:8.4@sha256:76f0358884cb92d9a086d24e5671c38048d3fef40f2588703d73eb55392d6b69 AS mysql
FROM nginx:1.31-alpine@sha256:a3554c0d0b48177b1413c26e01f695334428020d9df01243fbae3e11cfd17417 AS nginx
FROM caddy:2.8.4@sha256:226d1f059b75399fe19182893c7184591c07b97afc8dfcf44eeb80c9a77a530f AS caddy
FROM docker/dockerfile:1@sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e AS dockerfile-frontend
FROM gitea/gitea:1.27.3@sha256:87a67ee09d3ae0d1df5fda5dcda3e2a1f9236a45b0a59025d6e00e46adc43bef AS gitea
FROM codeberg.org/forgejo/forgejo:15.0.10@sha256:7b2f61163ff1d19f2aaf62d37ae4e805e822c2f86d6628b2bd468470929beacb AS forgejo
