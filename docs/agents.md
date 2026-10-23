# Connect your agent

Two minutes from a running instance to an agent that opens issues and pull
requests under its own name: a bot account, a narrowed token, and one config
block for Claude Code, Codex or Cursor.

The MCP server is part of Plombir Git, at `POST /api/v1/mcp`. There is nothing
to install next to it. Every tool call goes through the same access checks as
the REST API, under the bot's credential, and lands in the audit log.

Every step below is run by `scripts/agents-guide-check.sh` against a fresh
trial instance, with the URL and the `Authorization` header read out of the
config blocks on this page.

## 0. An instance

Any Plombir Git instance you are an administrator or a repository owner on. To
try it out, start a throwaway one:

```bash
docker run --rm -p 8080:8080 -p 2222:2222 ghcr.io/nettechnologys/plombir-git:latest plombir-git trial
```

(`plombir-git trial` is new in the release after v0.2.0; until it is tagged,
build the image with `docker build -t plombir-git .` and use `plombir-git` as
the image name.) Open the setup link it prints, create your account, and create a repository
(**Dashboard → New Repository**, tick *Initialize repository*). The examples below
use `http://localhost:8080`, your account `you` and the repository `you/app`.
Put your own instance's address wherever `http://localhost:8080` appears.

## 1. A bot account for the agent

**Settings → Agents → Create agent.** Give it a username, for example
`you-agent`. It has no password and cannot sign in; it acts only through the
tokens you mint for it, and it stops working when your account does.

## 2. Access to the repository

**Repository → Settings → Collaborators → Add collaborator**: `you-agent` with **write**
access. That is the bot's access, the same as a person's. Nothing else gives it
any.

## 3. A token, narrowed

Back in **Settings → Agents**, open the bot's **Tokens** and create a **New
token**:

- **Repositories:** `you/app`. The token reaches nothing else, even where the
  bot account could.
- **MCP tools:** leave empty for now, or list the tools you allow (for example
  `read_file, create_issue, create_pr, list_review_comments`). A token with a
  tool list works only through `/api/v1/mcp`.
- **Keep off protected branches:** leave it on. The bot then cannot merge,
  push or commit to a protected branch; a person does that.

Copy the token. It is shown once. Put it in your shell's environment, where
all three clients below read it from:

```bash
export PLOMBIR_AGENT_TOKEN='paste-the-token-here'
```

<details>
<summary>The same three steps over the API</summary>

Creating a credential needs a recent login (*sudo mode*): a session token from
the last ten minutes, or the one `POST /api/v1/users/me/sudo` returns after
confirming your password.

```bash
BASE_URL=http://localhost:8080
curl -fsS -X POST "$BASE_URL/api/v1/users/bots" \
  -H "Authorization: Bearer $YOUR_TOKEN" -H 'Content-Type: application/json' \
  -d '{"username": "you-agent"}'
curl -fsS -X POST "$BASE_URL/api/v1/repos/you/app/collaborators" \
  -H "Authorization: Bearer $YOUR_TOKEN" -H 'Content-Type: application/json' \
  -d '{"username": "you-agent", "permission": "write"}'
curl -fsS -X POST "$BASE_URL/api/v1/users/bots/you-agent/tokens" \
  -H "Authorization: Bearer $YOUR_TOKEN" -H 'Content-Type: application/json' \
  -d '{"name": "agent", "repositories": ["you/app"], "deny_protected_merge": true}' \
  | jq -r '.token'
```

</details>

## 4. The client

The server speaks MCP's Streamable HTTP transport: one JSON-RPC message per
`POST`, the token as `Authorization: Bearer <token>`.

### Claude Code

```bash
claude mcp add --transport http --scope user plombir-git http://localhost:8080/api/v1/mcp --header "Authorization: Bearer $PLOMBIR_AGENT_TOKEN"
```

The shell puts the token's value into `~/.claude.json`. To keep it out of
files, put the server in the project's `.mcp.json` instead, where Claude Code
reads `${PLOMBIR_AGENT_TOKEN}` from the environment when it starts:

```json
{
  "mcpServers": {
    "plombir-git": {
      "type": "http",
      "url": "http://localhost:8080/api/v1/mcp",
      "headers": {
        "Authorization": "Bearer ${PLOMBIR_AGENT_TOKEN}"
      }
    }
  }
}
```

`claude mcp list` shows whether it connected.

### Codex

In `~/.codex/config.toml` (or a trusted project's `.codex/config.toml`):

```toml
[mcp_servers.plombir-git]
url = "http://localhost:8080/api/v1/mcp"
bearer_token_env_var = "PLOMBIR_AGENT_TOKEN"
```

Codex sends the variable's value as `Authorization: Bearer …`. Start `codex`
from the shell where `PLOMBIR_AGENT_TOKEN` is set.

### Cursor

In `~/.cursor/mcp.json`, or `.cursor/mcp.json` in a project:

```json
{
  "mcpServers": {
    "plombir-git": {
      "url": "http://localhost:8080/api/v1/mcp",
      "headers": {
        "Authorization": "Bearer ${env:PLOMBIR_AGENT_TOKEN}"
      }
    }
  }
}
```

Cursor reads `${env:…}` from the environment it was started in.

### Other clients

Anything that reads an `mcpServers` block with a remote server takes the same
URL and header. For a client that only starts local processes, run the stdio
binary `plombir-git-mcp` instead — see the [MCP reference](mcp.md#mcp-server-plombir-git-mcp).

## 5. A first task

Ask the agent, for example:

> Read issue #1 in `you/app` on Plombir Git, make the change it asks for on a
> new branch, and open a pull request. Then wait for my review and apply what
> I suggest.

Or start smaller: *"List the files in `you/app` and open an issue describing
what the README is missing."*

What you will see:

- The issue and the pull request are the bot's, marked as a bot, with you named
  as the person it works for.
- Every tool call is in the administrator's **Audit Log** as `agent.mcp_tool_call`, with
  the repository in the *Resource* column and the issue or pull request number
  and file path in its details. A request the token's narrowing refused is
  there too, as `agent.scope_denied`.
- The bot's approval does not count toward a protected branch's required
  approvals, and its token cannot merge there. You review and press *Merge*.
  Working alone with your own bot: [one person and their
  agent](mcp.md#one-person-and-their-agent).

## When it does not connect

| Symptom | Cause |
|---------|-------|
| `401` | No token, a wrong one, or a revoked one. Check that the client's process sees `PLOMBIR_AGENT_TOKEN`. |
| `403`, *this token may not use the tool '…'* | The token has a tool list without that tool. Mint a new token with it. |
| `403` on a repository | The token's repository list leaves it out, or the bot is not a collaborator there. |
| The agent's merge is refused | The branch is protected and the token is kept off protected branches. That is the point: a person merges. |
| `http://` refused by the client | Some clients want HTTPS for a remote host. Put the instance behind TLS ([deploy guide](../deploy/README.md)); `localhost` is fine without. |
