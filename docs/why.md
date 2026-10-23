# Why Plombir Git, in detail

[← README](../README.md)

The three reasons from the README with what backs each one, the honest trade against Gitea's Actions, and the memory numbers we measured.

## Why Plombir Git

Three things, each with something you can check.

### 1. Agents work under their own name, and a person approves

- **MCP is part of the server**, at `POST /api/v1/mcp`, with the caller's
  credential and every access check of the REST API behind each tool call.
  There is nothing extra to install. (Gitea's MCP server is a separate
  program, [`gitea-mcp`](https://gitea.com/gitea/gitea-mcp).)
- **Any user can create a bot account they own.** It has no password, it acts
  only through tokens you mint, and it stops working when your account does.
- **Tokens can be narrowed** to named repositories and named MCP tools, and
  kept off protected branches — on by default for a bot's token.
- **A person opens the merge.** A bot's approval never counts toward a
  protected branch's required approvals, nor does its owner's approval on the
  bot's own pull request. A bot cannot approve a protected deployment
  environment, and CI for a pull request from a fork runs only after a person
  approves its exact head commit. Working alone with your own bot, where no
  second person exists to approve:
  [one person and their agent](mcp.md#one-person-and-their-agent).
- **Every MCP tool call is in the audit log** (`agent.mcp_tool_call`), and so is
  every request a token's narrowing refused (`agent.scope_denied`).

Proof: the whole loop — an agent branches, commits, opens a pull request,
answers review, a person approves — is an integration test and a recording,
[`docs/demo/agent-review.cast`](demo/agent-review.cast)
(`asciinema play docs/demo/agent-review.cast`). Details are under
[MCP over HTTP and agent accounts](mcp.md#mcp-over-http-and-agent-accounts).

Gitea 28 has bot accounts and an audit log too
([release notes](https://blog.gitea.com/release-of-28.0.0/)), so "has bots" is
no longer the difference. The difference is who may create one, what its token
can be narrowed to, and whose approval counts.

### 2. Release assets that can be verified outside the forge

With `attestation_enabled = true` under `[releases]`, anyone with write access
can have a release asset signed: `POST …/releases/assets/{id}/attestation` produces an
[in-toto Statement v1](https://github.com/in-toto/attestation) in a
[DSSE](https://github.com/secure-systems-lab/dsse) envelope, canonicalised with
RFC 8785 and signed with the instance's Ed25519 key. The public key is the one
already published at `/api/v1/ci/oidc/jwks`, and `…/attestation/verify` checks
an envelope on the server. The statement's `predicateType` is
`https://plombir.com/git/provenance/v1`. It names the format, not the signer:
`predicate.builder.id` is the issuing instance's URL. Envelopes issued before
the rename carry `https://forgekeep.dev/provenance/v1`, the same format under
the former name, and still verify.

What it states, precisely: *this instance received exactly this SHA-256 for
this release, from this uploader*. It is not SLSA build provenance — it says
nothing about how the file was built. The signing key is stored, not derived
from `jwt_secret`, so rotating that secret does not invalidate signatures
already issued (see [Secrets and rotation](install.md#secrets-and-rotation)).

### 3. CI that runs what you wrote, or refuses by name

Plombir Git runs `.gitea/workflows/*.yml` — or `.forgejo/workflows/*.yml` and
`.github/workflows/*.yml` as they came from Forgejo and GitHub, in Forgejo's order — as a **strict subset** of Actions. A key
it accepts is executed; a key, trigger or action it does not implement fails the
whole workflow with a message naming the file, the job and the key. It is never
quietly dropped, so you never get a green run of a shortened workflow. A
protected `environment:` holds the job until a person approves it.

The table below is the honest trade. Gitea runs much more of the Actions
ecosystem; Plombir Git's promise is narrower and stricter.

## Actions compatibility

Compared with **Gitea 28.0.0** and **Gitea Runner 2.0.0**, as of 2026-10-03.
Gitea documents its Actions as GitHub-compatible except for the differences on
[its comparison page](https://docs.gitea.com/usage/actions/comparison/); a ✅ in
the Gitea column means the key is not listed there, or that the release notes
cited in the row added it. Plombir Git's side is specified in full in
[docs/gitea-actions.md](gitea-actions.md).

| Workflow syntax | Plombir Git | Gitea 28 |
|-----------------|-----------|----------|
| `.github/workflows/` when there is no `.gitea/workflows/` | ✅ the same order, the two never merged | ✅ `[actions] WORKFLOW_DIRS` |
| `.forgejo/workflows/`, ahead of `.gitea/workflows/` | ✅ Forgejo's order, never merged | ❌ read by Forgejo, not Gitea |
| `on:` `push`, `pull_request`, `workflow_dispatch`, `workflow_call` | ✅ | ✅ |
| `on: schedule` | ✅ cron in UTC, from the default branch; one run per tick, missed ticks not caught up | ✅ |
| `on:` `pull_request_target`; `on.<event>.types` | ❌ refused by name | ✅ |
| Branch, tag and path filters, `!` negation | ✅ GitHub globs; `+`, `?` and `[…]` refused rather than read differently | ✅ |
| `concurrency` (workflow level) | ✅ `group`, `cancel-in-progress` | ✅ since 1.26 ([notes](https://blog.gitea.com/release-of-1.26.0/)) |
| `jobs.<id>.timeout-minutes` | ✅ | ✅ with Runner 2.0 ([notes](https://blog.gitea.com/release-of-runner-2.0.0/)) |
| `jobs.<id>.continue-on-error` | ✅ also an expression rendered per matrix variant (`${{ matrix.experimental }}`); such a variant failing does not fail the pipeline or trip `fail-fast` | ✅ with Runner 2.0 and Gitea ≥ 1.27 |
| `jobs.<id>.environment` | ✅ an existing environment; a protected one waits for a person's approval | ⚠️ ignored |
| `needs`, `if`, `env`, `defaults.run.working-directory` | ✅ | ✅ |
| `inputs.*` / `github.event.inputs.*` in `if:`, `run:`, `env:`, `with:` | ✅ typed as on GitHub (a boolean is `true` / `false`); an undeclared name refused | ✅ |
| `github.head_ref` / `github.base_ref` | ✅ the pull request's branches, `''` for other events — `${{ github.head_ref \|\| github.ref }}` groups work | ✅ |
| `github.ref_name` in `run:`, `env:`, `with:` and job fields | ✅ `CI_REF_NAME`: `main`, `v1.2` | ✅ |
| `actions/upload-artifact` `name:` / `path:` and `actions/cache` `path:` with expressions | ✅ rendered per matrix variant (`dist-${{ matrix.os }}`, `out-${{ matrix.os }}`); `artifact` without a name | ✅ |
| `jobs.<id>.name`, step `name:` with expressions | ✅ rendered per matrix variant (`Build on ${{ matrix.os }}`), shown beside the job id | ✅ |
| `run-name` | ✅ `github.*`, `inputs.*`, `github.event.inputs.*`, rendered when the run starts and shown in the pipeline list; `matrix.*`, `github.actor` refused by name; one event's workflows are one pipeline, named by each in file order | ✅ |
| `always()` / `failure()` in a job's `if:` | ✅ answer for every job it needs, transitively; the pipeline stays failed; `cancelled()` refused — a canceled pipeline runs nothing more | ✅ |
| `strategy.matrix` | ✅ static and dynamic (`fromJSON(needs.<job>.outputs.<name>)`), `include` / `exclude` | ✅ static and dynamic |
| `strategy.fail-fast`, `strategy.max-parallel` | ✅ | ✅ |
| Reusable workflows | ✅ same repository only | ✅ |
| `container.image`, `container.env` | ✅ | ✅ |
| `container.options` | ❌ refused — it could undo the job sandbox | ✅ |
| `services:` | ✅ Docker mode; a network per job, health-checked before the first step, `ports:` on loopback only; `volumes` and non-health `options` refused | ✅ |
| `uses:` actions | ⚠️ composite and JavaScript (`node12`–`node24`) actions from `./path`, this instance or an upstream the operator allows (`[ci] actions_sources`), plus the built-in `actions/checkout`, `actions/cache`, `actions/upload-artifact`; Docker actions refused by name; GitHub's artifact, cache and OIDC services not emulated | ✅ any action |
| Step `id` | ✅ | ✅ |
| Step outputs, `jobs.<id>.outputs`, `needs.<id>.outputs` | ✅ `$GITHUB_OUTPUT`, 1 MiB per job, masked; `::set-output` refused; a job's `if:` reads them, and a job that needs a skipped one is skipped | ✅ |
| Composite actions | ✅ `inputs`, `outputs`, nested up to 10 deep; the resolved commit is in the job log; `github.action_path` / `$GITHUB_ACTION_PATH` — the action's files reach the runner (runner protocol 6) | ✅ |
| Step `continue-on-error`, step `timeout-minutes`, step `shell` | ✅ each step is its own process, in both runners; `$GITHUB_ENV` / `$GITHUB_PATH` between steps | ✅ |
| `vars.*` | ✅ repository and organization variables, the repository's winning | ✅ |
| `permissions` | ⚠️ narrows the job token (`contents`, `packages`, `id-token: write` for OIDC); any other `write` refused — the token cannot write | ✅ |

If your workflows lean on Docker marketplace actions, or on JavaScript actions
that talk to GitHub's artifact, cache or OIDC services, Gitea will run them and
Plombir Git will not. Plombir Git also has its own pipeline format,
[`.plombir-git-ci.yml`](ci.md), for what the subset cannot express.

## Memory: measured

We publish only numbers we measured:

| Build | Load | `VmRSS` at idle | `VmHWM` (peak) |
|-------|------|-----------------|----------------|
| `1e3679f`, before the fix below | Production instance: 40 days up, 4 users, 35 MB database, 14 MB of repositories | 1.21 GB | 2.56 GB |
| Build of 2026-10-06, with `1068860` | The same production instance: 9.8 days up | 258 MB | 395 MB |

1.14 GB of the first idle figure was 31 glibc per-thread malloc arenas of about
58 MB each, held fully resident by transparent huge pages. Since `1068860` the
server caps glibc at two arenas before it starts its worker threads (an explicit
`MALLOC_ARENA_MAX` still wins). On the same host, still with transparent huge
pages set to `always`, that is 4.7 times less at idle and 6.5 times less at
peak; 178 MiB of the idle figure is in huge pages (`AnonHugePages`). Both rows
are read from `/proc/<pid>/status` of the running server.
