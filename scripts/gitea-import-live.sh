#!/usr/bin/env bash
# Start a throwaway Gitea in Docker, fill one repository with what an import
# has to carry, and print the environment the live import test reads
# (card_64ae115d5bd6).
#
#   scripts/gitea-import-live.sh up [gitea|forgejo]  # start + seed, prints `export …` lines
#   scripts/gitea-import-live.sh down                # remove the container
#
# The Regression workflow runs it for both servers and runs the test after each
# (card_7f2bdd581d17). Their images are pinned in .github/ci-images/Dockerfile,
# where Dependabot proposes the next release — and that pull request's own run
# is where a changed answer shows up. Forgejo's image answers to the same
# `gitea` command and `GITEA__` settings. `GITEA_LIVE_IMAGE` overrides the pin,
# to try a release by hand before it is pinned.
#
# The repository `importer/moved` holds: two labels and a milestone; issue #1
# with a comment by a second account; a pull request (#2, sharing the issue
# numbering as Gitea does) whose branch is merged with a merge commit and then
# deleted, with an approving review by that second account that comments on the
# line the pull request adds; a release `v1.0`;
# and a wiki page. Everything is written through Gitea's own API, so the shapes
# are the ones a real server answers with.
set -euo pipefail

NAME="${GITEA_LIVE_NAME:-plombir-gitea-live}"
PORT="${GITEA_LIVE_PORT:-33000}"
BASE="http://127.0.0.1:${PORT}"
CI_IMAGES="$(dirname "$0")/../.github/ci-images/Dockerfile"

# The same reader as scripts/check-reverse-proxy-examples.sh: one
# `FROM <image> AS <stage>` line per pin.
ci_image() {
  local image
  image="$(awk -v stage="$1" '$1 == "FROM" && $3 == "AS" && $4 == stage { print $2 }' "${CI_IMAGES}")"
  if [ -z "${image}" ] || [ "$(printf '%s\n' "${image}" | wc -l)" -ne 1 ]; then
    echo "${CI_IMAGES} must pin exactly one \`FROM <image> AS $1\`." >&2
    exit 1
  fi
  printf '%s\n' "${image}"
}

down() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }

if [ "${1:-up}" = "down" ]; then
  down
  exit 0
fi

case "${2:-gitea}" in
  gitea) PINNED="$(ci_image gitea)" ;;
  forgejo) PINNED="$(ci_image forgejo)" ;;
  *)
    echo "unknown server \`$2\`: gitea or forgejo" >&2
    exit 2
    ;;
esac
IMAGE="${GITEA_LIVE_IMAGE:-${PINNED}}"

down
docker run -d --rm --name "$NAME" -p "127.0.0.1:${PORT}:3000" \
  -e GITEA__security__INSTALL_LOCK=true \
  -e GITEA__database__DB_TYPE=sqlite3 \
  -e GITEA__server__ROOT_URL="${BASE}/" \
  -e GITEA__service__DISABLE_REGISTRATION=true \
  "$IMAGE" >/dev/null

for _ in $(seq 1 60); do
  curl -fsS "${BASE}/api/v1/version" >/dev/null 2>&1 && break
  sleep 2
done
curl -fsS "${BASE}/api/v1/version" >/dev/null

user() { # name password -> token
  docker exec -u git "$NAME" gitea admin user create --username "$1" --password "$2" \
    --email "$1@example.com" --must-change-password=false ${3:-} >/dev/null
  docker exec -u git "$NAME" gitea admin user generate-access-token --username "$1" \
    --token-name live --scopes all --raw
}
IMPORTER_TOKEN="$(user importer importer-pass-1 --admin)"
REVIEWER_TOKEN="$(user reviewer reviewer-pass-1)"

api() { # token method path [json]
  curl -fsS -X "$2" -H "Authorization: token $1" -H 'Content-Type: application/json' \
    "${BASE}/api/v1$3" ${4:+-d "$4"}
}

api "$IMPORTER_TOKEN" POST /user/repos '{"name":"moved","auto_init":true,"default_branch":"main"}' >/dev/null
api "$IMPORTER_TOKEN" PATCH /repos/importer/moved '{"has_wiki":true}' >/dev/null
api "$IMPORTER_TOKEN" PUT /repos/importer/moved/collaborators/reviewer '{"permission":"write"}' >/dev/null
api "$IMPORTER_TOKEN" POST /repos/importer/moved/labels '{"name":"bug","color":"#ee0701"}' >/dev/null
api "$IMPORTER_TOKEN" POST /repos/importer/moved/labels '{"name":"docs","color":"#0075ca"}' >/dev/null
MILESTONE="$(api "$IMPORTER_TOKEN" POST /repos/importer/moved/milestones '{"title":"v1"}' | python3 -c 'import json,sys;print(json.load(sys.stdin)["id"])')"
BUG="$(api "$IMPORTER_TOKEN" GET /repos/importer/moved/labels | python3 -c 'import json,sys;print([l["id"] for l in json.load(sys.stdin) if l["name"]=="bug"][0])')"

api "$IMPORTER_TOKEN" POST /repos/importer/moved/issues \
  "{\"title\":\"filed on Gitea\",\"body\":\"the first issue\",\"labels\":[${BUG}],\"milestone\":${MILESTONE}}" >/dev/null
api "$REVIEWER_TOKEN" POST /repos/importer/moved/issues/1/comments '{"body":"a reply by the reviewer"}' >/dev/null

CONTENT="$(printf 'what the pull request adds\n' | base64)"
api "$IMPORTER_TOKEN" POST /repos/importer/moved/contents/feature.txt \
  "{\"content\":\"${CONTENT}\",\"message\":\"the pull request's change\",\"branch\":\"main\",\"new_branch\":\"feature\"}" >/dev/null
api "$IMPORTER_TOKEN" POST /repos/importer/moved/pulls \
  '{"title":"merged on Gitea","body":"the change","head":"feature","base":"main"}' >/dev/null
api "$REVIEWER_TOKEN" POST /repos/importer/moved/pulls/2/reviews \
  '{"event":"APPROVED","body":"looks good","comments":[{"path":"feature.txt","new_position":1,"body":"on the added line"}]}' >/dev/null
api "$IMPORTER_TOKEN" POST /repos/importer/moved/pulls/2/merge '{"Do":"merge","delete_branch_after_merge":true}' >/dev/null
curl -fsS -X DELETE -H "Authorization: token $IMPORTER_TOKEN" "${BASE}/api/v1/repos/importer/moved/branches/feature" >/dev/null 2>&1 || true

api "$IMPORTER_TOKEN" POST /repos/importer/moved/releases \
  '{"tag_name":"v1.0","target_commitish":"main","name":"First release","body":"notes"}' >/dev/null
# The wiki is a second repository, `<repo>.wiki.git` — the one the import
# clones. Gitea 1.22 creates it on the first page, but its API then fails to
# write that page on a repository whose default branch is not `master`
# ("object does not exist [id: refs/heads/master]"): the call is made for the
# repository it leaves behind, and the page arrives by a push.
curl -sS -o /dev/null -X POST -H "Authorization: token $IMPORTER_TOKEN" -H 'Content-Type: application/json' \
  "${BASE}/api/v1/repos/importer/moved/wiki/new" -d '{"title":"Home","content_base64":"SGk="}' || true
WIKI_DIR="$(mktemp -d)"
git -C "$WIKI_DIR" init -q -b master
printf '# Home\n\nThe wiki on Gitea.\n' >"$WIKI_DIR/Home.md"
git -C "$WIKI_DIR" -c user.name=importer -c user.email=importer@example.com add Home.md
git -C "$WIKI_DIR" -c user.name=importer -c user.email=importer@example.com commit -qm wiki
git -C "$WIKI_DIR" push -q "http://importer:${IMPORTER_TOKEN}@127.0.0.1:${PORT}/importer/moved.wiki.git" master
rm -rf "$WIKI_DIR"

echo "export PLOMBIR_GIT_TEST_GITEA_URL=${BASE}"
echo "export PLOMBIR_GIT_TEST_GITEA_TOKEN=${IMPORTER_TOKEN}"
