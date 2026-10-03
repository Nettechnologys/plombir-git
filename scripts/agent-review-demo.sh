#!/usr/bin/env bash
#
# Walk through "an agent opens a pull request, a person approves it" against a
# throwaway instance, printing each step — the script behind the recording in
# docs/demo/agent-review.cast.
#
#   STAND_USER=alice scripts/ephemeral-stand.sh -- scripts/agent-review-demo.sh
#
# The stand exports the backend's address and the founder's token; the founder
# is the person who owns the repository and the agent. Everything the agent
# does goes through the MCP endpoint (`POST /api/v1/mcp`) under a bot account;
# everything the people do goes through the REST API. The end-to-end assertions
# for the same flow live in
# crates/rg-http/tests/integration/agent_accounts_tests.rs
# (`an_agents_pull_request_merges_only_after_a_code_owner_approves`).
#
# DEMO_PAUSE (seconds, default 0) waits between steps so a recording can be read.

set -euo pipefail

: "${STAND_BACKEND_URL:?run under scripts/ephemeral-stand.sh}"
: "${STAND_TOKEN:?run under scripts/ephemeral-stand.sh}"
: "${STAND_USERNAME:?run under scripts/ephemeral-stand.sh}"

API="${STAND_BACKEND_URL}/api/v1"
OWNER="${STAND_USERNAME}"
PAUSE="${DEMO_PAUSE:-0}"
OWNER_TOKEN="${STAND_TOKEN}"

step() {
  sleep "${PAUSE}"
  printf '\n\033[1;36m▶ %s\033[0m\n' "$*"
}

say() {
  printf '  %s\n' "$*"
}

# Read one field out of the JSON on stdin: `field path.to.value`.
field() {
  python3 -c '
import json, sys
value = json.load(sys.stdin)
for key in sys.argv[1].split("."):
    value = value[int(key)] if isinstance(value, list) else value[key]
print(value if not isinstance(value, (dict, list)) else json.dumps(value))
' "$1"
}

# `api METHOD PATH TOKEN [JSON]` — the response body; fails on a 4xx/5xx
# unless ALLOW_FAILURE=1, in which case the body is printed as is.
api() {
  local method=$1 path=$2 token=$3 body=${4:-}
  local args=(-sS -X "${method}" "${API}${path}")
  if [[ -n "${token}" ]]; then
    args+=(-H "Authorization: Bearer ${token}")
  fi
  if [[ -n "${body}" ]]; then
    args+=(-H 'Content-Type: application/json' --data-binary "${body}")
  fi
  if [[ "${ALLOW_FAILURE:-0}" == 1 ]]; then
    curl "${args[@]}"
  else
    curl -f "${args[@]}"
  fi
}

# `mcp TOKEN TOOL ARGUMENTS` — the text the tool answered. A failed call
# answers text starting with `Error:`.
mcp() {
  local token=$1 tool=$2 arguments=$3 response
  response="$(curl -sS -X POST "${API}/mcp" \
    -H "Authorization: Bearer ${token}" -H 'Content-Type: application/json' \
    --data-binary "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"${tool}\",\"arguments\":${arguments}}}")"
  printf '%s' "${response}" | field result.content.0.text
}

# The server's own refusal message inside a failed tool's
# `Error: ForgeKeep API error: status=N, body={...}` text.
refusal() {
  python3 -c '
import json, sys
text = sys.stdin.read()
status = text.split("status=", 1)[1].split(",", 1)[0]
body = json.loads(text.split("body=", 1)[1])
print(status, body["error"]["message"])
'
}

step "Setting the stage: ${OWNER} owns ${OWNER}/app, bob reviews, ${OWNER}-agent is an AI agent"
BOB_TOKEN="$(api POST /users/register "" \
  '{"username":"bob","email":"bob@example.com","password":"Qz7$wRtm-reviewer"}' | field token)"
api POST /repos "${OWNER_TOKEN}" '{"name":"app","auto_init":true}' >/dev/null
api POST "/repos/${OWNER}/app/contents/.github/CODEOWNERS" "${OWNER_TOKEN}" \
  '{"content":"* @bob\n","message":"bob owns everything"}' >/dev/null
api POST "/repos/${OWNER}/app/collaborators" "${OWNER_TOKEN}" \
  '{"username":"bob","permission":"write"}' >/dev/null
api POST /users/bots "${OWNER_TOKEN}" "{\"username\":\"${OWNER}-agent\"}" >/dev/null
api POST "/repos/${OWNER}/app/collaborators" "${OWNER_TOKEN}" \
  "{\"username\":\"${OWNER}-agent\",\"permission\":\"write\"}" >/dev/null
AGENT_TOKEN="$(api POST "/users/bots/${OWNER}-agent/tokens" "${OWNER_TOKEN}" \
  "{\"name\":\"demo\",\"repositories\":[\"${OWNER}/app\"]}" | field token)"
api POST "/repos/${OWNER}/app/branches/protection" "${OWNER_TOKEN}" \
  '{"branch_name":"main","require_pr":true,"require_approval":true,"required_approvals":1}' >/dev/null
say "main is protected: 1 approval required; the agent's token is confined to ${OWNER}/app"

step "Agent → write_file: starts branch agent/answer with its first commit"
REPLY="$(mcp "${AGENT_TOKEN}" write_file \
  "{\"owner\":\"${OWNER}\",\"repo\":\"app\",\"path\":\"src/answer.rs\",\"branch\":\"agent/answer\",\"message\":\"Add the answer\",\"content\":\"pub fn answer() -> i32 { 41 }\\n\"}")"
say "commit $(printf '%s' "${REPLY}" | field commit_sha | cut -c1-12)"

step "Agent → create_pr"
PR="$(mcp "${AGENT_TOKEN}" create_pr \
  "{\"owner\":\"${OWNER}\",\"repo\":\"app\",\"title\":\"Add the answer\",\"head\":\"agent/answer\",\"base\":\"main\"}")"
NUMBER="$(printf '%s' "${PR}" | field number)"
say "#${NUMBER} by $(printf '%s' "${PR}" | field author), acting for $(printf '%s' "${PR}" | field author_bot_owner)"
say "CODEOWNERS requested: $(api GET "/repos/${OWNER}/app/pulls/${NUMBER}/reviewers" "${OWNER_TOKEN}" | field 0.username)"

step "bob (code owner) leaves an inline comment"
COMMENT_ID="$(api POST "/repos/${OWNER}/app/pulls/${NUMBER}/comments" "${BOB_TOKEN}" \
  '{"path":"src/answer.rs","line":1,"side":"RIGHT","body":"The answer is 42."}' | field id)"

step "Agent → list_review_comments"
say "$(mcp "${AGENT_TOKEN}" list_review_comments \
  "{\"owner\":\"${OWNER}\",\"repo\":\"app\",\"number\":${NUMBER}}" | field 0.body)"

step "Agent → read_file + write_file: fixes the code on its branch"
SHA="$(mcp "${AGENT_TOKEN}" read_file \
  "{\"owner\":\"${OWNER}\",\"repo\":\"app\",\"path\":\"src/answer.rs\",\"ref\":\"agent/answer\"}" | field sha)"
HEAD="$(mcp "${AGENT_TOKEN}" write_file \
  "{\"owner\":\"${OWNER}\",\"repo\":\"app\",\"path\":\"src/answer.rs\",\"branch\":\"agent/answer\",\"message\":\"Make it 42\",\"content\":\"pub fn answer() -> i32 { 42 }\\n\",\"sha\":\"${SHA}\"}" | field commit_sha)"
say "new head $(printf '%s' "${HEAD}" | cut -c1-12)"

step "Agent → create_review_comment: answers in bob's thread"
mcp "${AGENT_TOKEN}" create_review_comment \
  "{\"owner\":\"${OWNER}\",\"repo\":\"app\",\"number\":${NUMBER},\"path\":\"src/answer.rs\",\"body\":\"Fixed in ${HEAD:0:12}.\",\"reply_to_id\":${COMMENT_ID}}" >/dev/null
say "replied to comment #${COMMENT_ID}"

# The pull request's head is moved by a detached post-push hook.
for _ in $(seq 1 100); do
  [[ "$(api GET "/repos/${OWNER}/app/pulls/${NUMBER}" "${OWNER_TOKEN}" | field head_sha)" == "${HEAD}" ]] && break
  sleep 0.1
done

step "CI reports on the new head; agent → get_commit_status"
api POST "/repos/${OWNER}/app/statuses/${HEAD}" "${OWNER_TOKEN}" \
  '{"state":"success","context":"ci/build"}' >/dev/null
say "ci: $(mcp "${AGENT_TOKEN}" get_commit_status \
  "{\"owner\":\"${OWNER}\",\"repo\":\"app\",\"sha\":\"${HEAD}\"}" | field state)"

step "${OWNER} approves the agent's PR — and tries to merge"
api POST "/repos/${OWNER}/app/pulls/${NUMBER}/reviews" "${OWNER_TOKEN}" '{"action":"approve"}' >/dev/null
say "$(ALLOW_FAILURE=1 api POST "/repos/${OWNER}/app/pulls/${NUMBER}/merge" "${OWNER_TOKEN}" \
  '{"strategy":"merge"}' | field error.message)"

step "bob approves; the agent → merge_pr"
api POST "/repos/${OWNER}/app/pulls/${NUMBER}/reviews" "${BOB_TOKEN}" '{"action":"approve"}' >/dev/null
REPLY="$(mcp "${AGENT_TOKEN}" merge_pr \
  "{\"owner\":\"${OWNER}\",\"repo\":\"app\",\"number\":${NUMBER},\"strategy\":\"merge\"}")"
if [[ "${REPLY}" == Error:* ]]; then
  say "refused: $(printf '%s' "${REPLY}" | refusal)"
else
  say "${REPLY}"
fi

step "${OWNER} merges"
api POST "/repos/${OWNER}/app/pulls/${NUMBER}/merge" "${OWNER_TOKEN}" '{"strategy":"merge"}' >/dev/null
say "#${NUMBER} is $(api GET "/repos/${OWNER}/app/pulls/${NUMBER}" "${OWNER_TOKEN}" | field state);" \
  "main: $(api GET "/repos/${OWNER}/app/blob/src/answer.rs?ref=main" "${OWNER_TOKEN}" | field content)"
