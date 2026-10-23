# How Plombir Git is built

[← README](../README.md)

Plombir Git is built by two developers working with AI coding agents, through
a loop we call the **recursive sweep**. Fix a defect. Then ask where else the
same mistake lives, and fix every place it turns up. Then close the whole class
with a check, so it cannot come back. When the queue of tickets runs empty, the
next round starts from the product as it now stands, and it finds what the
previous round could not yet see.

## Who we are

Plombir Git is made by [NetTechnologys s.r.o.](https://github.com/Nettechnologys),
a team that has been building software for 25 years. Two of us work on this
forge, [Yahook](https://github.com/Yahook) and
[vadiksext](https://github.com/vadiksext), and our own team already does its
work in it. We also make [codeindexer.com](https://codeindexer.com), the
development platform Plombir Git is built with. It covers code search with a
call graph, a task manager and roadmap for AI agents, a shared base of solved
problems, security audits, and an autonomous runner that works through the
roadmap on its own.

## Who does what

**The developers set the course.** That means what Plombir Git is for and
whom it serves, which areas to harden first, and which classes of defect the
work is organised around. The roadmap phases are named after those classes on
purpose: *Silent Failure Sweep*, *Authorization as a Layer*, *Dead Wiring*,
*Lossless Migration*.

**The agents search and carry out.** They read the code, find defects, find
the same defect in the places nobody pointed them at, and write the fix
together with the test that proves it.

**Every finding comes back as a ticket, and the developers decide what
happens to it.** It can be fixed as it stands, folded into a redesign that
removes the class, deferred, or turned down. Each finding also shapes where
the next round digs.

Several times the queue ran empty. Each time a new round of sweeps over the
product as it then stood found the next layer: 78 phases on 4 October, 118 on
23 October.

## Starting from someone else's code

Plombir Git began on 23 July 2026 as a fork of
[IronForge](https://github.com/lengyuqu/ironforge): 114,400 lines of Rust,
Svelte and TypeScript in 552 files, none of it written by us. With the code
indexed in [codeindexer.com](https://codeindexer.com), the work did not begin
with weeks of reading. The first fixes to the inherited code landed the next
day: 21 fix commits by 24 July, 190 by the end of the first week.

A large part of what the sweeps found was in that inherited code. Run
`git blame` on every line a fix commit changed. Of the 772 fix commits up to
`4323c06a`, 539 changed lines that came with the fork, and 126 of those
changed nothing else. Of all the lines the fixes changed, 40% (13,683 of
33,838) date from the initial import.

<details>
<summary>How those numbers are counted</summary>

For each commit whose subject starts with `fix(` or `fix:`, take the lines its
diff removes or rewrites (code and configuration files), blame them in the
parent commit,
and check whether the line was introduced by the root commit `f160b234` — the
import of IronForge. Added lines have nothing to blame and are not counted. A
fix that changes an inherited line did not necessarily fix a defect born there,
so read the 70% as "fixes that had to change the code we inherited", not as a
verdict on IronForge.

```bash
root=f160b234075561479bdc7083cb9047ac691494d6
git log --no-merges --format='%H %s' 4323c06a | grep -E '^[0-9a-f]{40} fix(\(|:)' | cut -c1-40 |
while read -r c; do
  git diff -U0 --no-renames "$c^" "$c" -- '*.rs' '*.svelte' '*.ts' '*.js' '*.mjs' '*.toml' '*.sql' |
  awk '/^--- /{f=($2=="/dev/null")?"":substr($2,3)}
       /^@@/ && f!=""{split(substr($2,2),r,","); n=(r[2]=="")?1:r[2]; if (n>0) print f, r[1], n}' |
  while read -r f a n; do git blame --porcelain -L "$a,+$n" "$c^" -- "$f"; done |
  awk -v root="$root" -v c="$c" '/^[0-9a-f]{40} [0-9]+ [0-9]+/{ if ($1==root) up++; else ours++ }
       END{print c, up+0, ours+0}'
done | awk '{n++; up+=$2; ours+=$3; if ($2>0) inh++; if ($2>0 && $3==0) only++}
            END{print n " fix commits; " inh " changed inherited lines, " only " only those; " up " of " up+ours " changed lines inherited"}'
# 772 fix commits; 539 changed inherited lines, 126 only those; 13683 of 33838 changed lines inherited
```

</details>

## What the loop finds

Some of what the agents found would most likely have stayed hidden from a
person reading the code by hand. None of the three below ever showed up as an
error: each one answered as if it had worked.

- **A discarded write, then the whole class.** Turning on
  `clippy::let_underscore_must_use` found 120 discarded `Result`s across 36
  files. Most were harmless; some were not. An OCI blob push answered
  `201 Created` when the row that locates the blob was never written, so the
  blob the client believed it had pushed answered the next request with a 404.
  An SSO login succeeded while its refreshed tokens were dropped (`99cd0fb`).
  The class was then closed for good: the lint is `deny` across the workspace
  (`5925b1b`), and a new discard fails the build.
- **A database file that was not there.** Planning the rename of the
  production server turned up a problem. When `serve` was given a database
  path that did not exist, it created an empty database next to the live
  repositories. On an empty database the first account to register becomes an
  administrator, even with registration closed. So a typo in that path would
  have handed a public instance to its first visitor. `serve` now refuses to
  start when the repository root already holds repositories (`74e4f24`).
- **An approval nobody had any more.** Protected deployment environments
  counted a vote from an approver whose right had since been revoked
  (`95d20d5`). The sweep then asked every other stored approval the same
  question: is it still backed by a current right? Fork pull request CI failed
  it. A collaborator demoted to read access still released the base
  repository's CI secrets to fork code, through an approval recorded earlier
  (`9a4e736`).

## Reviews from outside

Two people outside the loop reviewed the code and helped fix it. Thank you,
both.

- **Rinat** ([hicaru](https://github.com/hicaru)) wrote a full-repository
  review and security audit on 1 September 2026 (`CODE_REVIEW.md` on the
  `audite` branch): 14 findings. Each was checked against the code before
  anything was changed. 12 held up and are fixed. Two did not: one looked for
  a fork-PR secrets gate under names the code does not use (it is
  `ci_approved_sha`), and one stopped reading the merge queue before the
  branch that re-queues a failed pull request. By `git blame`, the dangerous
  defaults in that report came with the IronForge import.
- **Alexey Stupenkov** ([alexio777](https://github.com/alexio777)) sent 18
  pull requests on 9 October: sandboxed downloads of user content, release
  asset names, the minimum git version, credentials kept out of request logs,
  deployment hardening and more. Each went through the same loop as our own
  findings. Five landed under his name, and the rest were folded into our own
  commits, with the decisions that shaped them.

After each review the sweep went on, and kept finding defects neither review
had reached.

## The loop, step by step

1. **Orient through the index.** Find the code by meaning or by name, and see
   who calls it (`find_callers`, `find_references`) before changing anything.
   On an unfamiliar codebase this is what turns "read for a week" into "fix
   today".
2. **Fix it, and prove the fix.** A test that stays green with the fix removed
   proves nothing. So the fix is taken out again, and the test has to turn
   red.
3. **Sweep sideways.** Ask where else the same construct lives: another
   parser, another cache, another call site. Each real hit becomes a ticket
   under the phase for its class of defect, not under the ticket being worked
   on. By 4 October, 633 of the 1,167 closed bug tickets had been found this
   way rather than reported.
4. **Close the class.** Where a class can be closed mechanically, it is: a
   lint set to `deny`, or a contract check that pins documentation,
   configuration, routes or access rules to the code.
5. **Remember the cause.** A non-obvious cause, together with the fix and the
   dead end tried first, goes into a shared base of solved problems — 1,349
   entries for this project. The next session, or another agent, finds it in
   seconds instead of rediscovering it.

## What every change passes

The pre-push hook (`.githooks/pre-push`) lets a push through only once
`scripts/verify-push-gates.sh` has passed on that exact commit:

- `cargo fmt --check`, workspace Clippy and `cargo doc`, where a warning is a
  failure;
- 89 `scripts/*-contract-check.mjs` checks that pin documentation,
  configuration, routes and access rules to the code, and 39 regression stands
  that break each check on purpose to prove it still catches what it guards;
- Docker Compose, observability-config and frontend checks.

The workspace test suite runs before a piece of work is closed: 4,530 tests on
23 October 2026, about 18 minutes under `cargo nextest` on the build machine.
The database smokes on PostgreSQL and MySQL and the dependency audit run in the
Regression workflow after the push. A push to `main` asks GitHub how the last
Regression run on `main` went, and is refused while it is red, naming the run
and its failed jobs.

## Built with codeindexer

[codeindexer.com](https://codeindexer.com) is what makes the loop fast enough
to run again and again. It gives three things:

- **An index of the code**: search by meaning, by name and by exact text,
  plus a call graph. An agent gets its bearings in an unfamiliar module in a
  few queries, and sees the blast radius of a change before making it.
- **A memory that outlives a session**: tickets, roadmap phases and solved
  problems, shared by every agent on the project. The sweep files its findings
  there, under the right class of defect. The next round starts from what the
  last one learned, not from scratch.
- **The bookkeeping that keeps a loop honest**: a ticket closes only when its
  acceptance checks pass. Progress by phase shows where a class of defect is
  still open.

As of 23 October 2026, at `4323c06a`:

| | Count |
|---|---|
| Commits since the fork on 23 July 2026 | 1,601 |
| … with a subject starting `fix(` or `fix:` | 772 |
| … of those, changing code that came with the fork | 539 |
| Roadmap phases, each one class of defect | 118 |
| Notes in the solutions base | 1,349 |
| Tests in the workspace | 4,530 |

The git rows can be recounted from a clone:

```bash
git rev-list --count 4323c06a                                  # 1601
git log --format=%s 4323c06a | grep -cE '^fix(\(|:)'           # 772
```

The phase and solution counts come from the team's codeindexer workspace: phases from `roadmap(action="list")`, notes from
`solutions(action="find")` for the project.

**Try the same loop on your own code:** [codeindexer.com](https://codeindexer.com).
