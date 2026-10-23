#!/usr/bin/env bash
# docs/agents.md, step by step, against a running instance.
#
#   scripts/agents-guide-check.sh <base-url> <owner-jwt> <owner> <repo>
#
# <owner-jwt> is a fresh session token of <owner>, who owns <owner>/<repo>
# (fresh: creating a bot's token needs sudo mode). scripts/trial-smoke.sh calls
# this right after it has created both on a trial instance.
#
# The guide is the input, not a copy of it: the MCP URL and the Authorization
# header are read out of its Claude Code, Codex and Cursor blocks, the three are
# required to agree, and the calls below are sent with exactly that header. A
# guide whose config would not connect fails here (card_127a12b9ac5a).
set -euo pipefail

base="${1:?usage: agents-guide-check.sh <base-url> <owner-jwt> <owner> <repo>}"
jwt="${2:?owner JWT}"
owner="${3:?owner}"
repo="${4:?repo}"
guide="$(cd "$(dirname "$0")/.." && pwd)/docs/agents.md"
documented_base='http://localhost:8080'
token_var='PLOMBIR_AGENT_TOKEN'

fail() {
  echo "agents guide: $*" >&2
  exit 1
}

[ -f "${guide}" ] || fail "no ${guide}"

# ── What the guide tells each client ─────────────────────────────────
claude_line="$(grep -E '^claude mcp add --transport http ' "${guide}" | head -n 1)"
[ -n "${claude_line}" ] || fail "no 'claude mcp add --transport http' line"
claude_url="$(grep -oE 'https?://[^ "]+/api/v1/mcp' <<<"${claude_line}" | head -n 1)"
grep -qF -- "--header \"Authorization: Bearer \$${token_var}\"" <<<"${claude_line}" \
  || fail "the Claude Code command does not send Authorization: Bearer \$${token_var}"

codex_url="$(sed -n '/^\[mcp_servers\.plombir-git\]/,/^```/p' "${guide}" | sed -nE 's/^url = "([^"]+)"$/\1/p')"
codex_var="$(sed -n '/^\[mcp_servers\.plombir-git\]/,/^```/p' "${guide}" | sed -nE 's/^bearer_token_env_var = "([^"]+)"$/\1/p')"
[ "${codex_var}" = "${token_var}" ] || fail "Codex reads the token from '${codex_var}', not ${token_var}"

# The Cursor block is the JSON block whose header uses `${env:…}`.
cursor_json="$(awk '
  /^```json$/ { inside = 1; block = ""; next }
  /^```$/ && inside { if (block ~ /\$\{env:/) { print block; exit } inside = 0; next }
  inside { block = block $0 "\n" }
' "${guide}")"
[ -n "${cursor_json}" ] || fail "no Cursor mcp.json block"
cursor_url="$(jq -er '.mcpServers["plombir-git"].url' <<<"${cursor_json}")"
cursor_auth="$(jq -er '.mcpServers["plombir-git"].headers.Authorization' <<<"${cursor_json}")"
[ "${cursor_auth}" = "Bearer \${env:${token_var}}" ] \
  || fail "the Cursor header is '${cursor_auth}', not Bearer \${env:${token_var}}"

for url in "${claude_url}" "${codex_url}"; do
  [ "${url}" = "${cursor_url}" ] || fail "the clients disagree about the URL: '${url}' vs '${cursor_url}'"
done
case "${cursor_url}" in
  "${documented_base}"/*) ;;
  *) fail "the guide's URL '${cursor_url}' is not under ${documented_base}" ;;
esac
mcp_url="${base}${cursor_url#"${documented_base}"}"

# ── Steps 1–3: bot, access, narrowed token ───────────────────────────
api() {
  local method="$1" path="$2" body="${3:-}"
  curl -fsS -X "${method}" "${base}/api/v1${path}" \
    -H "Authorization: Bearer ${jwt}" -H 'Content-Type: application/json' \
    ${body:+-d "${body}"}
}
bot="${owner}-agent"
api POST /users/bots "$(jq -n --arg u "${bot}" '{username:$u}')" >/dev/null \
  || fail "step 1: could not create the bot"
api POST "/repos/${owner}/${repo}/collaborators" \
  "$(jq -n --arg u "${bot}" '{username:$u,permission:"write"}')" >/dev/null \
  || fail "step 2: could not add the bot as a collaborator"
agent_token="$(api POST "/users/bots/${bot}/tokens" \
  "$(jq -n --arg r "${owner}/${repo}" '{name:"agent",repositories:[$r],deny_protected_merge:true}')" \
  | jq -er '.token')" || fail "step 3: could not mint the bot's token"

# ── Step 4: the client's calls, with the guide's header ──────────────
# `${env:VAR}` / `$VAR` resolved the way the clients resolve them.
authorization="${cursor_auth/"\${env:${token_var}}"/${agent_token}}"
mcp() {
  curl -fsS -X POST "${mcp_url}" \
    -H "Authorization: ${authorization}" \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d "$1"
}
init="$(mcp '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"agents-guide-check","version":"1"}}}')" \
  || fail "initialize was refused"
jq -e '.result.protocolVersion and .result.serverInfo' >/dev/null <<<"${init}" || fail "initialize: ${init}"
status="$(curl -sS -o /dev/null -w '%{http_code}' -X POST "${mcp_url}" \
  -H "Authorization: ${authorization}" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"notifications/initialized"}')"
[ "${status}" = 202 ] || fail "notifications/initialized answered ${status}, not 202"

tools="$(mcp '{"jsonrpc":"2.0","id":2,"method":"tools/list"}')" || fail "tools/list was refused"
for tool in read_file create_issue create_pr; do
  jq -e --arg t "${tool}" '[.result.tools[].name] | index($t)' >/dev/null <<<"${tools}" \
    || fail "tools/list does not offer ${tool}"
done

call="$(mcp "$(jq -n --arg o "${owner}" --arg r "${repo}" \
  '{jsonrpc:"2.0",id:3,method:"tools/call",params:{name:"create_issue",arguments:{owner:$o,repo:$r,title:"Opened by the agent"}}}')")" \
  || fail "tools/call create_issue was refused"
jq -e '.result.isError != true' >/dev/null <<<"${call}" || fail "create_issue failed: ${call}"

# ── Step 5: what the person sees ─────────────────────────────────────
issues="$(api GET "/repos/${owner}/${repo}/issues?state=open")"
jq -e --arg b "${bot}" '[.. | objects | select(.title? == "Opened by the agent") | .author] | index($b)' \
  >/dev/null <<<"${issues}" || fail "the issue is not the bot's: ${issues}"
audit="$(api GET '/admin/audit/logs?action=agent.mcp_tool_call&per_page=100')"
jq -e --arg n "${owner}/${repo}" --arg b "${bot}" \
  '[.logs[] | select(.username == $b and .resource_type == "repo" and .resource_name == $n)] | length > 0' \
  >/dev/null <<<"${audit}" || fail "the audit log does not name ${owner}/${repo} for the bot's call: ${audit}"

echo "agents guide: bot, narrowed token, initialize, tools/list and create_issue through ${cursor_url#"${documented_base}"} with the guide's header"
