# Plombir Git

> A lightweight, self-hosted Git forge for teams where code is written by
> people and AI agents alike.

[![Rust](https://img.shields.io/badge/rust-1.89%2B-orange)](https://www.rust-lang.org/)
[![License: AGPL-3.0-or-later](https://img.shields.io/badge/license-AGPL--3.0--or--later-blue)](LICENSE)

**Built with [codeindexer.com](https://codeindexer.com):** two developers and
AI agents took a 114,400-line fork to a forge you can run, in three months, by
sweeping every class of defect until it was closed —
[how](docs/how-it-is-built.md).

Plombir Git hosts repositories, issues, pull requests, code review, CI, a wiki
and package registries — and treats an AI agent as a participant of its own:
its own account, its own narrowed token, its own audit trail, while a person
keeps the approval that opens the merge. One server process, in the same space
as [Gitea](https://gitea.com/) and [Forgejo](https://forgejo.org/).

<p align="center">
  <img src="docs/screenshots/agent-pr-demo.webp" width="880"
       alt="An AI agent's pull request in Plombir Git: the agent's bot account opened it and applied a reviewer's suggestion, and a second person approves and merges it">
  <br>
  <sub>An agent opens a pull request under its own bot account and applies a review suggestion over MCP; a second person approves and merges it.</sub>
</p>

## Try it in one command

```bash
docker run --rm -p 8080:8080 -p 2222:2222 ghcr.io/nettechnologys/plombir-git:latest plombir-git trial
```

No secrets, config files or build. The log prints a setup link — open it,
create the administrator account, and you have a forge at
`http://localhost:8080`. Trial mode says so on every page and is not for
production: with `--rm` the data goes with the container. To keep it, add
`-v plombir-git-trial:/data` and end the command with `--data-dir /data`.

`plombir-git trial` is new in the next release after v0.2.0; until it is
tagged, build the image from this repository first
(`docker build -t plombir-git .`) and run `plombir-git` in place of the
`ghcr.io/…:latest` name.

Then:

- **Connect your agent** — Claude Code, Codex or Cursor over MCP, with a bot
  account and a narrowed token, in two minutes: [docs/agents.md](docs/agents.md).
- **Bring your repositories over** from GitHub, GitLab, Gitea or Forgejo:
  issues and pull requests keep their numbers, with their reviews, approvals,
  labels, milestones and releases, and the import receipt lists what came over
  and what could not. It is the first button on an empty dashboard.
- **Deploy it for real** with Docker Compose, TLS and backups:
  [deploy/README.md](deploy/README.md).

## Why Plombir Git

1. **Agents work under their own name, and a person approves.** MCP is built
   into the server, any user can create a bot account, its token can be
   narrowed to repositories and tools, and a bot's approval never opens a
   protected merge. [Details](docs/why.md#1-agents-work-under-their-own-name-and-a-person-approves)
2. **Release assets that can be verified outside the forge**: in-toto / DSSE
   attestations signed with the instance's published key.
   [Details](docs/why.md#2-release-assets-that-can-be-verified-outside-the-forge)
3. **CI that runs what you wrote, or refuses by name**: a strict subset of
   Gitea / GitHub Actions where an unsupported key fails the run instead of
   being dropped. [Details and the honest comparison with Gitea](docs/why.md#actions-compatibility)

## A look around

<table>
  <tr>
    <td width="50%"><a href="docs/screenshots/pull-request.webp"><img src="docs/screenshots/pull-request.webp" alt="A pull request opened by a bot account, marked BOT and on behalf of its owner, with its Markdown description rendered"></a><br><sub><b>Pull request from an agent.</b> The bot account is marked, and so is the person it works for.</sub></td>
    <td width="50%"><a href="docs/screenshots/review-suggestion.webp"><img src="docs/screenshots/review-suggestion.webp" alt="An inline review suggestion on a diff line, applied by the agent, with its reply in the thread"></a><br><sub><b>Code review.</b> Inline comments and suggestions; the agent applies one and answers in the thread.</sub></td>
  </tr>
  <tr>
    <td><a href="docs/screenshots/pipeline.webp"><img src="docs/screenshots/pipeline.webp" alt="A CI pipeline with lint and test stages, every job passed"></a><br><sub><b>CI.</b> Stages, jobs and logs from <code>.plombir-git-ci.yml</code>, <code>.gitea/workflows</code> or <code>.github/workflows</code>.</sub></td>
    <td><a href="docs/screenshots/audit-log.webp"><img src="docs/screenshots/audit-log.webp" alt="The audit log listing agent.mcp_tool_call entries for the bot account next to the people's actions"></a><br><sub><b>Audit log.</b> Every MCP tool call an agent makes is recorded.</sub></td>
  </tr>
</table>

The screenshots come from a local demo instance with made-up users and a
made-up project. The whole agent loop is also an integration test and a
terminal recording, [`docs/demo/agent-review.cast`](docs/demo/agent-review.cast).

## Documentation

| | |
|---|---|
| [Connect your agent](docs/agents.md) | Bot account, narrowed token, Claude Code / Codex / Cursor configs |
| [MCP reference](docs/mcp.md) | The stdio binary, MCP over HTTP, agent accounts, working alone with your bot |
| [Production deployment](deploy/README.md) | Docker Compose, reverse proxy, backups, upgrades and rollback |
| [Install from source](docs/install.md) | Requirements, build, secrets and rotation, first repository |
| [Using Git](docs/using-git.md) | SSH and HTTPS, pull and push mirrors, Git LFS |
| [CI](docs/ci.md) and [Actions subset](docs/gitea-actions.md) | Pipelines, workflows, and the [runner](docs/runner.md) |
| [REST API](docs/api.md) and [CLI](docs/cli.md) | Endpoints and OpenAPI; every `plombir-git` subcommand |
| [Features and stack](docs/features.md) | What each area does, and where its code lives |
| [Why Plombir Git, in detail](docs/why.md) | The three reasons, the Gitea comparison, measured memory |
| [How it is built](docs/how-it-is-built.md) | The recursive sweep, who does what, and what every change passes |
| [Development](docs/development.md), [ARCHITECTURE.md](ARCHITECTURE.md), [CONTRIBUTING.md](CONTRIBUTING.md) | Repository layout, tests, crate boundaries |

> **Origin.** Plombir Git is a fork of [IronForge](https://github.com/lengyuqu/ironforge).
> See [NOTICE](NOTICE) for provenance and licensing details.

## License

Plombir Git is free software under the
[GNU Affero General Public License v3.0 or later](LICENSE), © [NetTechnologys s.r.o.](https://github.com/Nettechnologys).
You may use, study, modify and share it. If you run a modified version for
other people over a network, the AGPL asks you to offer them its source.

A commercial license without the AGPL obligations is available from the
copyright holder. Contributions are accepted under the
[Contributor License Agreement](CLA.md). See [NOTICE](NOTICE) for the fork's
upstream provenance and the MIT terms of the code that came from IronForge.
