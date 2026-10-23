# MCP reference

[← README](../README.md)

To connect Claude Code, Codex or Cursor in two minutes, follow [Connect your agent](agents.md). This page is the reference behind it.

## MCP server (`plombir-git-mcp`)

`plombir-git-mcp` exposes repositories, issues, pull requests, pipelines and code
search to an AI agent over the
[Model Context Protocol](https://modelcontextprotocol.io). It speaks JSON-RPC on
**stdio** and is meant to be launched by the agent as a subprocess; it has no
HTTP/SSE transport of its own, and `--sse` exits with an error instead of
starting a partial server. Over HTTP the **server itself** serves the same tools
— see [MCP over HTTP and agent accounts](#mcp-over-http-and-agent-accounts).

It takes no flags — the whole configuration is three environment variables:

| Variable | Default | Purpose |
|----------|---------|---------|
| `PLOMBIR_GIT_URL` | `http://localhost:8080` | Base URL of the Plombir Git API |
| `PLOMBIR_GIT_PAT` | _(none)_ | Personal access token, sent as `Authorization: Bearer` |
| `PLOMBIR_GIT_ALLOW_INSECURE_HTTP` | `false` | Explicitly permit the PAT on a non-loopback plaintext HTTP server |

Without `PLOMBIR_GIT_PAT` the server still starts: it logs a warning, and every API
call goes out unauthenticated, so anything non-public fails at the first tool
call. Issue the token from the web UI under user settings.

With a PAT, remote `http://` is rejected before the first request. Loopback HTTP
remains available for local development. Set
`PLOMBIR_GIT_ALLOW_INSECURE_HTTP=true` only for a deliberately plaintext remote
deployment; the redirect policy still prevents the PAT moving to another
scheme, host, or port.

An agent that reads the usual `mcpServers` block:

```json
{
  "mcpServers": {
    "plombir-git": {
      "command": "plombir-git-mcp",
      "env": {
        "PLOMBIR_GIT_URL": "https://forge.example.com",
        "PLOMBIR_GIT_PAT": "…"
      }
    }
  }
}
```

Logs go to stderr so the stdio channel stays clean; `RUST_LOG` selects what is
logged.

### MCP over HTTP and agent accounts

The server answers MCP itself at `POST /api/v1/mcp` — one JSON-RPC message per
request, `Authorization: Bearer <token>` — so an agent needs no local binary:

```json
{
  "mcpServers": {
    "plombir-git": {
      "type": "http",
      "url": "https://forge.example.com/api/v1/mcp",
      "headers": { "Authorization": "Bearer …" }
    }
  }
}
```

Each tool call is dispatched in-process through the server's own router, with
the caller's credential and every access check, which is what lets the server
treat an agent as a participant of its own rather than as its owner:

- **Bot accounts.** *Settings → Agents* (`/api/v1/users/bots`) creates an
  account for an agent, owned by you. It has no password; you mint its tokens,
  and it stops working when your account does. Give it access the usual way —
  as a collaborator on a repository. Its issues, comments and pull requests are
  shown as the bot's, with you named as the person it acts for.
- **Token narrowing.** Any token — a bot's or your own — can be confined to
  named repositories, to named MCP tools (it then works only through
  `/api/v1/mcp`), and kept off protected branches (no merge, push or
  server-side commit there; on by default for a bot's token). A token confined
  to repositories reaches a fork pull request's diff, merge, CI approval and
  suggestions only when the fork is on its list too, and it never adds a
  repository anywhere — no fork, no transfer. A bot's token carries the `repo`
  scope only, so it cannot manage credentials of its own. A token confined to
  tools reaches exactly the API routes those tools call — whatever arguments
  it passes — and reads MCP resources only through the tool that reads the
  same thing (`file://` needs `read_file`, `issue://` needs `get_issue`,
  `repo://` needs `list_repos`).
- **A person approves.** An agent can do the whole loop through MCP — start a
  branch with `write_file`, `create_pr`, read inline review with
  `list_review_comments`, push a fix, answer in the thread
  (`create_review_comment` with `reply_to_id`), and read CI with
  `list_pipelines` / `get_commit_status` — but a bot's approval never counts
  toward a protected branch's required approvals, nor does its owner's on the
  bot's own pull request, nor a read-only collaborator's. The approval that
  opens the merge comes from another person with write access (CODEOWNERS can
  request them), and the bot's token cannot merge into the protected branch
  itself. [`docs/demo/agent-review.cast`](demo/agent-review.cast) is a
  recording of the whole loop (`asciinema play docs/demo/agent-review.cast`);
  `STAND_USER=alice scripts/ephemeral-stand.sh -- scripts/agent-review-demo.sh`
  runs it against a throwaway instance.
- **Limits and audit.** Every token of one bot shares a request budget,
  `[rate_limit].agent_max` per `agent_window_secs` (600 a minute by default).
  Every tool call is written to the audit log as `agent.mcp_tool_call`; a
  request outside a token's narrowing answers `403` and is written as
  `agent.scope_denied`; and every audit row made through a token carries
  `credential.token_id` and, through MCP, `credential.mcp_tool`.

#### One person and their agent

Required approvals are written for a team: the approval that opens the merge
comes from a *second* person with write access. When you are the only person
on the repository and the pull requests come from your own bot, nobody can give
that approval — yours on your bot's pull request is the author approving
themselves — so `"require_approval": true` leaves every one of the bot's pull
requests unmergeable. That is deliberate: the refused merge says which
approvals did not count and why.

What keeps `main` safe in that setup is who can write to it, not a count of
approvals. Three calls, with your own token (`you`, `you-bot` and `you/app`
stand for your account, your bot and the repository):

```bash
# 1. The bot works on the repository as a collaborator with write access.
curl -X POST https://forge.example.com/api/v1/repos/you/app/collaborators \
  -H "Authorization: Bearer $YOUR_TOKEN" -H 'Content-Type: application/json' \
  -d '{"username": "you-bot", "permission": "write"}'

# 2. main takes changes only through a pull request. You alone may still push
#    to it directly; no approval count.
curl -X POST https://forge.example.com/api/v1/repos/you/app/branches/protection \
  -H "Authorization: Bearer $YOUR_TOKEN" -H 'Content-Type: application/json' \
  -d '{"branch_name": "main", "require_pr": true, "allowed_push_users": ["you"]}'

# 3. The bot's token stays off protected branches: no merge, push or
#    server-side commit there. That is already the default for a bot's token;
#    saying it keeps it from being switched off by accident.
curl -X POST https://forge.example.com/api/v1/users/bots/you-bot/tokens \
  -H "Authorization: Bearer $YOUR_TOKEN" -H 'Content-Type: application/json' \
  -d '{"name": "agent", "repositories": ["you/app"], "deny_protected_merge": true}'
```

The bot then opens pull requests into `main` and cannot land them by any
route: `require_pr` refuses its `git push` to `main` over HTTPS and SSH alike,
and its token refuses the merge and a commit from the web editor. A bot has no
password, so there is no browser session to use instead. You review and press
*Merge*. Add `"require_status_check": true` to the rule when CI has to pass
first — it binds your merges and the bot's pull requests the same way. In the
web UI the same settings are the repository's *Settings → Collaborators* and
*Settings → Branches*, and your account's *Settings → Agents*.
