# How Plombir Git is built

[← README](../README.md)

## How it is built

Plombir Git is developed with AI coding agents. They write the code; the
maintainer decides what to build and reviews it. The repository's pre-push hook
(`.githooks/pre-push`) lets a push through only once
`scripts/verify-push-gates.sh` has passed on that exact commit:

- `cargo fmt --check`, workspace Clippy and `cargo doc`, where a warning is a
  failure;
- around eighty `scripts/*-contract-check.mjs` checks that pin documentation,
  configuration, routes and access rules to the code;
- Docker Compose, observability-config and frontend checks.

The workspace test suite — 3,440 tests on 4 October 2026, about six minutes
under `cargo nextest` on a warm build — is not part of that push gate; it is
run before a piece of work is closed.

The database smokes and the dependency audit need a live database or the
network, so they run only in the Regression workflow after the push. A push to
`main` therefore also asks GitHub how the last finished Regression run on `main`
went, and is refused while it is red — naming the run and its failed jobs —
unless `PUSH_OVER_RED_MAIN=1` says the pusher saw it. Without `gh` or a network
it warns instead of refusing.

## Built with codeindexer

The agents that write Plombir Git work through
[codeindexer.dev](https://codeindexer.dev): an index of the code with search
and a call graph, plus a memory that outlives a session — tickets, roadmap
phases and notes on solved problems, shared by every agent on the project.

Every ticket goes through the same loop. Find the code through the index, check
who calls it (`find_callers`, `find_references`) before changing it, fix it.
Then ask where else the same construct lives — another parser, cache or call
site — and file a ticket for each real hit, under the roadmap phase for its
class of defect rather than the one being worked on. A non-obvious cause goes
into the solutions base, so the next session finds it in seconds instead of
rediscovering it. That is why the phases are named after defects, not
features: *Silent Failure Sweep*, *Authorization as a Layer*, *Dead Wiring*.

As of 4 October 2026, at `9a4e736`:

| | Count |
|---|---|
| Commits since the fork on 23 July 2026 | 1,294 |
| … with a subject starting `fix(` or `fix:` | 767 |
| Roadmap phases, each one class of defect | 78 |
| Bug tickets filed / closed | 1,174 / 1,167 |
| … of them found by a sideways sweep, not by the ticket being worked on | 633 |
| Notes in the solutions base | 1,131 |

The git rows can be recounted from a clone:

```bash
git rev-list --count 9a4e736                                   # 1294
git log --format=%s 9a4e736 | grep -cE '^fix(\(|:)'            # 767
```

The other rows come from the maintainer's codeindexer workspace, which is not
public: phases from `roadmap(action="list")`, tickets from `memory_cards` with
`card_type="bug"` (sideways finds carry the `sideways-sweep` or
`sideways-finding` tag), notes from `solutions(action="find")` for the project.

Three finds from the history:

- **A discarded write, then the whole class.** Turning on
  `clippy::let_underscore_must_use` found 120 discarded `Result`s across 36
  files. Most were harmless; some were not. An OCI blob push answered
  `201 Created` when the row that locates the blob was never written, so the
  blob the client believed it had pushed answered the next request with a 404.
  An SSO
  login succeeded while its refreshed tokens were dropped (`99cd0fb`). The
  class was then closed for good: the lint is `deny` across the workspace
  (`5925b1b`), and a new discard fails the build.
- **A database file that was not there.** Planning the rename of the
  production server, which renames its database file, turned up that `serve`
  given a missing database path created an empty one next to live
  repositories. On an empty database the first account to register becomes an
  administrator, even with registration closed, so a typo in that path would
  have handed a public instance to its first visitor. `serve` now refuses to
  start when the repository root already holds repositories (`74e4f24`).
- **An approval nobody had any more.** Protected deployment environments
  counted a vote from an approver whose right had since been revoked
  (`95d20d5`). The sweep put the same question — is a stored approval still
  backed by a current right? — to every other stored approval, and fork pull
  request CI failed it: a collaborator demoted to read access still released
  the base repository's CI secrets to fork code through an approval recorded
  earlier (`9a4e736`).
