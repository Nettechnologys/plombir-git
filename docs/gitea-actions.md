# Gitea Actions workflows — the supported subset

Plombir Git reads `.gitea/workflows/*.yml` in the Gitea/GitHub Actions format, and
implements a **subset** of it. This page is that subset's boundary.

Reaching for GitHub's own documentation instead will mislead you, which is why
this page exists rather than a link. There is no Actions runtime here: a job is
a list of steps the runner executes in a worktree of your repository, so the
parts of Actions that are *the runtime* — JavaScript and Docker actions — have
nothing to map onto and are refused. Composite actions are written into the job
as steps when the workflow is read; see [Actions](#actions). Steps, their
outputs, the outputs one job hands to the jobs that need it and service
containers are implemented by the runner itself; see [Steps](#steps),
[Outputs](#outputs) and [Services](#services).

**Refusals are loud and whole-file.** Every block below carries a closed schema.
An unknown key, an unrunnable trigger, an action this engine does not implement
— any one of them fails the trigger with a message naming the file, the job and
the key. You never get a pipeline that ran a shortened version of your workflow.
The one thing you must not read into that: a refusal means *no pipeline*, so a
workflow that has never run is worth checking against this page before assuming
the event did not fire.

**Which format wins.** `.gitea/workflows/` is tried first. The native
[`.plombir-git-ci.yml`](ci.md) is used when this directory is absent at the commit
being built, or when no workflow in it is triggered by the event — and when
there is no native file either, an automatic event (a push, a pull request, a
merge group) publishes no pipeline at all rather than a red one. The native
format is also the supported escape hatch for anything on this page marked
unsupported.

**Source budgets.** Plombir Git retains every workflow source from the immutable
commit while it resolves repository-local reusable workflows. The engine
therefore refuses a workflow set before loading the file that crosses any of
these limits; the error names that file:

<!-- inventory: actions-workflow-source-limits -->

```text
per-file-bytes=1048576
total-bytes=16777216
file-count=256
```

## A complete example

Every root key, and most of what a job can say:

```yaml
name: CI

on:
  push:
    branches:
      - main
    paths:
      - src/**
  pull_request:
  merge_group:
  workflow_dispatch:
    inputs:
      target:
        description: Where to deploy
        type: choice
        required: true
        options:
          - staging
          - production
      verbose:
        description: Print more
        type: boolean
        default: false
      retries:
        type: number
        default: 2

concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: true

env:
  CARGO_TERM_COLOR: always

defaults:
  run:
    working-directory: server

jobs:
  build:
    runs-on: ubuntu-latest
    container:
      image: rust:1.75
      env:
        RUSTFLAGS: -D warnings
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - uses: actions/cache@v4
        with:
          path: target
          key: cargo-${{ github.ref }}
      - name: Build
        run: cargo build --locked --release
        env:
          PROFILE: release
      - name: Package
        working-directory: dist
        run: tar czf app.tgz .

  test:
    needs: build
    runs-on: ubuntu-latest
    container:
      image: rust:1.75
    strategy:
      matrix:
        features:
          - default
          - all-features
    timeout-minutes: 30
    continue-on-error: false
    steps:
      - run: cargo test --features ${{ matrix.features }}

  deploy:
    needs: [build, test]
    if: github.ref == 'refs/heads/main'
    runs-on: ubuntu-latest
    environment: production
    container:
      image: alpine:3.20
    steps:
      - if: github.event_name == 'workflow_dispatch'
        run: ./deploy.sh ${{ inputs.target }}
```

## Root keys

| Key | Type | Meaning |
|-----|------|---------|
| `name` | string | Workflow label. Defaults to the file name. |
| `on` | string, list or mapping | The events that trigger it. **Required.** |
| `jobs` | mapping | The jobs, keyed by id. **Required.** |
| `concurrency` | block | Pipeline-level serialization. See [Concurrency](#concurrency). |
| `env` | map string→string | Variables every job inherits. |
| `defaults` | block | `run:` defaults every job inherits. See [Defaults](#defaults). |

Anything else at the root — `permissions`, `run-name` and the rest of the
Actions vocabulary — is an unknown key and fails the file.

## `on:` — triggers

Three spellings, all accepted — a bare event name, a list of them, or a mapping
that carries filters:

```yaml
name: Lint
on: push
jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - run: make lint
```

```yaml
name: Lint
on: [push, pull_request]
jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - run: make lint
```

```yaml
name: Lint
on:
  push:
    branches:
      - main
  pull_request:
jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - run: make lint
```

### Which events actually run something

| Trigger | Emitted by |
|---------|------------|
| `push` | A push to the repository. |
| `pull_request` | A pull request opened, updated or reopened. |
| `merge_group` | The merge queue's speculative merge. |
| `workflow_dispatch` | The Run button, or the API. |
| `schedule` | A tick of one of its `- cron:` entries. See [Scheduled runs](#scheduled-runs). |
| `workflow_call` | Nothing — it declares the file *callable* by another workflow. See [Reusable workflows](#reusable-workflows). |

Every other `on:` name is refused by the name you wrote, with that list in the
message. That includes a name Actions defines and Plombir Git has no producer
for — `pull_request_target` (it would run the workflow from the base branch,
which nothing here does):

<!-- example: refused -->
```yaml
name: Review
on:
  pull_request_target:
    branches:
      - main
jobs:
  audit:
    runs-on: ubuntu-latest
    steps:
      - run: cargo audit
```

Refusing beats accepting here. A workflow declaring an event nobody emits parses
perfectly and then never runs, and "my CI is silent" is a much worse thing to
debug than a named error at push time.

### Scheduled runs

`on: schedule` runs the workflow at each tick of one of its `- cron:` entries:

```yaml
name: Nightly
on:
  schedule:
    - cron: '0 3 * * *'
    - cron: '30 12 * * MON-FRI'
jobs:
  audit:
    runs-on: ubuntu-latest
    steps:
      - if: github.event.schedule == '0 3 * * *'
        run: cargo audit
      - run: echo "fired by ${{ github.event.schedule }}"
```

- **The dialect is GitHub's.** Five fields — minute, hour, day of month, month,
  day of week — each a list of `*`, values, ranges and `/steps`, with month and
  weekday names (`JAN`, `MON`) accepted in any case. A day of month and a day of
  week both written without `*` match on *either*: `0 0 1 * MON` is the 1st and
  every Monday. `N/step` without a range reads as `N-<max>/step`, as Gitea reads
  it. Refused by name: the `@daily` family (GitHub does not accept it either), a
  sixth field, values out of range, an entry key other than `cron`, an entry
  longer than 255 bytes, and a schedule that can never fire (`0 0 30 2 *`).
- **Times are UTC.**
- **The schedule is the default branch's.** Entries on any other branch schedule
  nothing, and neither do a pull mirror's: its workflows are its upstream's,
  which is also why a mirror sync runs no `push` pipeline. A push to the default branch that adds, changes or removes an entry
  takes effect at the next minute; the run builds the default branch's tip,
  with `github.ref` the branch and `github.event_name` `schedule`.
- **One tick, one run.** Each entry of each file keeps the last tick it ran in
  the database, and a tick is run by whoever moves that record past it in one
  conditional write — a restart cannot run a tick twice, nor can two replicas.
  Two files with the same cron are two runs, each with only its own jobs.
- **Missed ticks are not caught up.** A server that was down through several
  ticks runs one of them when it comes back, as GitHub does.
- **A new entry waits for its next tick.** A `0 3 * * *` pushed at 15:00 first
  runs at the next 03:00, not straight away.
- **There is a floor on frequency.** No entry runs more often than every
  `[ci] schedule_min_interval_minutes` (five by default, GitHub's floor):
  `* * * * *` runs every fifth minute.
- **`github.event.schedule`** is the entry that fired, spelled as the file spells
  it — in a condition, and as `${CI_EVENT_SCHEDULE}` in a `run:`.
- **A run that cannot be built is still a run.** When the tick's configuration is
  refused, a failed `schedule` pipeline says why; retrying it replays the same
  entry of the same file.

A bare `on: schedule` — or `schedule` inside a list — names the event without
any entry to say when, and is refused:

<!-- example: refused -->
```yaml
name: Nightly
on: [push, schedule]
jobs:
  audit:
    runs-on: ubuntu-latest
    steps:
      - run: cargo audit
```

### Event filters

Six keys, on any of the event mappings:

```yaml
on:
  push:
    branches:
      - main
      - release/*
    branches-ignore:
      - wip/*
    tags:
      - v*
    tags-ignore:
      - v*-rc*
    paths:
      - src/**
      - Cargo.toml
    paths-ignore:
      - docs/**
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - run: make check
```

The first four keys come in two halves, and each half is about one **kind** of
ref: `branches` / `branches-ignore` are asked only about `refs/heads/…`, `tags` /
`tags-ignore` only about `refs/tags/…`. Which half you write is therefore also a
statement about what the workflow is *for*:

- Neither half — every push runs it, branch or tag alike.
- Only the branch half — a tag push does not run it, whatever the patterns say.
  `branches: ['**']` is every branch, not every ref.
- Only the tag half — a branch push does not run it. `tags-ignore: [v*-rc*]` on
  its own is a workflow about tags that skips release candidates, not a workflow
  about everything.
- Both halves — one of them matching is enough: `branches: [main]` together with
  `tags: [v*]` runs on `main` and on `v1.0.0`.

`paths` / `paths-ignore` are not a third ref half. They narrow branch pushes and
pull-request-shaped events, so those workflows run only when the ref and changed
paths agree. A tag push is the deliberate exception: the Actions dialect treats
both path filters as satisfied without computing a diff, leaving `tags` /
`tags-ignore` (when declared) to decide whether the workflow runs.

For `pull_request`, “changed paths” means the pull request's net diff from the
merge base of its target branch through its current head — not merely the last
commit. For `merge_group`, the candidate is already a speculative merge commit,
so its changed paths are read against that commit's first parent (the base tip).

Anything else under an event — `types`, `branches_ignore` with an underscore, a
misspelling — is refused with the supported six listed. `types:` in particular
is worth calling out: it is the most-copied Actions key that does not exist
here, and a `pull_request` restricted to `types: [opened]` would otherwise look
like a filter that silently did nothing.

Patterns are globs, on refs and on paths alike: `*` matches within one segment,
`**` crosses separators, and `\` makes the next character a literal —
`a\*b.txt` is the file actually named with a star. Those three plus the leading
`!` below are the whole vocabulary; every other character stands for itself.

A pattern may open with `!` to exclude, and the list is read **in order**: a
plain pattern selects, a `!` pattern deselects what an earlier one selected, and
a later plain pattern selects it back.

```yaml
on:
  push:
    paths:
      - '**'             # everything…
      - '!docs/**'       # …except the docs…
      - docs/deploy.md   # …except this one after all
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - run: make check
```

Three spellings of `!` cannot carry that meaning, and each is refused by name
rather than quietly matched as a literal:

- A list of nothing but negations — `branches: ['!main']`. Nothing is ever
  selected for `!` to subtract from, so the workflow would run on no branch at
  all. Add one pattern without `!`, or say it with `branches-ignore`.
- `!` inside `branches-ignore`, `tags-ignore` or `paths-ignore`. Those keys are
  an exclusion already.
- A pattern ending in a lone `\`, which has nothing left to escape.

Three more characters carry a meaning in GitHub's filter dialect that this
matcher does not implement, and the reason is the same for all three: that
dialect is not a glob but half a regular expression, in which `+`, `?` and `[…]`
quantify or enumerate the character *before* them rather than standing for one
of their own.

| Character | What GitHub's cheat sheet means by it |
|-----------|---------------------------------------|
| `+` | One or more of the character before it. |
| `?` | **Zero or one of the character before it** — not "any one character". |
| `[…]` | One character from the set or range in the brackets. |

A pattern using any of them is refused by name, rather than matched byte for
byte and quietly selecting no tag anybody pushes:

<!-- example: refused -->
```yaml
on:
  push:
    tags:
      - 'v1.[0-9]'
jobs:
  release:
    runs-on: ubuntu-latest
    steps:
      - run: make release
```

`?` is the one worth reading twice, because until it was refused it was the
only one that did something. This matcher used to read it as a shell glob does
— "any one character" — so `v1.?` selected `v1.0` here and `v1` or `v1.` on
GitHub, and, in the direction that actually costs something, `release?/**`
selected `releaseX/**`: a run on a branch the author never named. One spelling
meaning two different things on two engines is worse than a spelling that is
refused, so it is now refused.

<!-- example: refused -->
```yaml
on:
  push:
    branches:
      - 'release?/**'
jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - run: make deploy
```

That refusal costs the patterns which used `+`, `?` or `[` as an ordinary
character, and the escape is the way back: `c\+\+/**` is everything under a
directory really named `c++`, and `docs/faq\?.md` is the file with a question
mark in its name.

A pattern is at most 256 bytes long, and a longer one is refused by name. The
cost of matching grows with the length of the pattern, and a push asks every
pattern once per changed path.

## `workflow_dispatch` and `workflow_call` inputs

```yaml
on:
  workflow_dispatch:
    inputs:
      environment:
        description: Which environment to target
        type: environment
        required: true
      level:
        type: choice
        options:
          - debug
          - info
        default: info
jobs:
  run:
    runs-on: ubuntu-latest
    steps:
      - run: ./run.sh --level ${{ inputs.level }}
```

| Key | Type | Meaning |
|-----|------|---------|
| `description` | string | Shown to whoever fills the form. |
| `required` | bool | A required input with no value refuses the run. |
| `type` | string | **Required.** See the table below. |
| `default` | string, number or bool | Must have the declared type, and be one of `options` for a `choice`. |
| `options` | list of strings | **Required** for `choice`, and refused for every other type. Must be non-empty and free of duplicates. |

| `type` | `workflow_dispatch` | `workflow_call` |
|--------|---------------------|-----------------|
| `string` | yes | yes |
| `boolean` | yes | yes |
| `number` | yes | yes |
| `choice` | yes | no |
| `environment` | yes | no |

`workflow_dispatch` takes at most 25 inputs. Input names must look like
`[A-Za-z_][A-Za-z0-9_-]*`, and two names that differ only in a `-` versus `_`
are refused as a pair: both become the same `INPUT_*` variable, and one would
quietly win.

## Jobs

| Key | Type | Meaning |
|-----|------|---------|
| `runs-on` | string or non-empty list of strings | Runner labels required to pick the job up. |
| `steps` | list | The job's work. See [Steps](#steps). |
| `needs` | string or list of strings | Jobs that must finish first. |
| `if` | string | Condition evaluated before the job is scheduled. |
| `env` | map string→string | Variables for this job, overriding the workflow's. |
| `container` | block | See [Containers](#containers). |
| `services` | map key→block | Containers the job's steps talk to, in Docker mode. See [Services](#services). |
| `defaults` | block | `run:` defaults for this job. See [Defaults](#defaults). |
| `strategy` | block | `matrix`, `fail-fast` and `max-parallel`. See [Matrix](#matrix). |
| `environment` | string or `{ name: … }` | A deployment environment **the repository already has**; a protected one holds the job for approval, and only jobs that declare it read its [secrets](ci.md#secrets-and-workload-identity). |
| `timeout-minutes` | integer | Per-job timeout. Converted to seconds, and the result must land in `1`–`86400` — so `1`–`1440` minutes. |
| `continue-on-error` | bool | A failure of this job does not fail the pipeline. |
| `outputs` | map string→string | What the job leaves for the jobs that need it, read from its steps after the last one. See [Outputs](#outputs). |
| `uses` / `with` / `secrets` | — | Reusable workflow invocation. See [Reusable workflows](#reusable-workflows). |

`needs:` must name jobs this workflow declares, and the graph must be acyclic.
A typo is refused rather than treated as "no dependency": `needs: [buidl]` under
a job spelled `build` would otherwise put the job in the first stage, running
next to the very job it was written to wait for.

Jobs are placed into stages by their `needs:` depth — a job with no dependencies
runs first, and every other job runs one stage after the deepest job it needs.

### `if:`

The same small expression language the native format uses, documented under
[`if` in the pipeline reference](ci.md#if): `==`, `!=`, `&&`, `||`, `!`,
parentheses, single-quoted strings, and `startsWith` / `endsWith` / `contains` /
`success()`. Contexts are `github.ref`, `github.ref_name`, `github.event_name`,
`github.sha`, `github.repository`, `github.repository_owner`,
`github.event.schedule`, and `env.NAME`.

A **job**'s `if:` is evaluated once, when the pipeline is built, and may also
read `matrix.NAME`; `success()` is the only status function it has, and it is
always true there. A **step**'s `if:` is read by the runner between steps — see
[Steps](#steps) — so it has all four status functions and no `matrix.`, and
reads `steps.*` and `needs.*` ([Outputs](#outputs)), which a job's `if:` cannot.

## Steps

| Key | Type | Meaning |
|-----|------|---------|
| `name` | string | Label for the step. Without one, the step is called `Run` and the first line of its `run:`, as on GitHub. |
| `id` | string | The step's id: a letter or `_`, then letters, digits, `-` and `_`. Unique in the job. Later steps read what it left as `steps.<id>.*` — see [Outputs](#outputs). |
| `run` | string | The script. |
| `shell` | string | `bash`, `sh`, `python`, `pwsh`, or a command line of your own with `{0}` where the script goes. Requires `run:`. |
| `uses` | string | A composite action, or one of three built-in actions. See [Actions](#actions). |
| `with` | map | Inputs for that action. |
| `env` | map string→string | Variables for this step. |
| `if` | string | Condition, read between steps. See below. |
| `working-directory` | string | Directory for this step's `run:`, relative to the workspace. Requires `run:`. |
| `continue-on-error` | boolean | A failure of this step does not fail the job. Requires `run:`. |
| `timeout-minutes` | whole number, 1–1440 | The step is stopped, with everything it started, when it runs longer. Requires `run:`. |

```yaml
name: CI
on: push
jobs:
  test:
    runs-on: ubuntu-latest
    defaults:
      run:
        shell: bash
    steps:
      - name: Install tools
        id: tools
        run: |
          mkdir -p "$RUNNER_TEMP/bin"
          echo "$RUNNER_TEMP/bin" >> "$GITHUB_PATH"
          echo "PROFILE=ci" >> "$GITHUB_ENV"
      - name: Flaky integration tests
        timeout-minutes: 20
        continue-on-error: true
        run: make integration
      - name: Unit tests
        shell: sh
        run: make test PROFILE="$PROFILE"
      - name: Report
        if: always()
        shell: python3 {0}
        run: print("done")
```

**Each step is its own process.** The runner executes a job's steps one by one —
on the host, or with `docker exec` in the one container the whole job runs in,
so what one step installs the next one finds — each with its shell, its working
directory, its `env:` and its deadline. Nothing else crosses from one step to
the next: a variable a step `export`s, a directory it `cd`s into and a shell
option it sets all end with it. What does cross goes through the files GitHub
defines:

- `$GITHUB_ENV` — `NAME=value` lines, or `NAME<<DELIMITER` … `DELIMITER` for a
  value over several lines, become variables of every later step. A step's own
  `env:` still wins over them. A line naming a variable the runner sets
  (`CI_*`, `PATH`, `HOME`, …) fails the step — extend `PATH` through
  `$GITHUB_PATH`.
- `$GITHUB_PATH` — each line is a directory put in front of `PATH` for every
  later step.
- `$GITHUB_OUTPUT` — `name=value` lines, or the same `name<<DELIMITER` form,
  become the step's outputs. See [Outputs](#outputs).
- `$GITHUB_STEP_SUMMARY` — Markdown appended to the step's own log.

`GITHUB_WORKSPACE`, `RUNNER_TEMP` and `GITHUB_ACTIONS=true` are set as on
GitHub. Each file is capped at 1 MiB.

**The shells start the way GitHub starts them.** `bash` is
`bash --noprofile --norc -eo pipefail {0}`, `sh` is `sh -e {0}`, `python` is
`python {0}`, `pwsh` is `pwsh -command ". '{0}'"`. A step without `shell:` takes
the job's `defaults.run.shell`, then the workflow's, and with none of them is
GitHub's unspecified shell: `bash -e {0}`, or `sh -e {0}` where there is no
bash. `cmd` and `powershell` are refused — the runners are Linux.

**Conditions are read between steps.** `success()` is "no earlier step failed",
`failure()` its opposite, `always()` runs regardless and `cancelled()` is false
(a canceled job stops where it stands). A condition that names none of them
means `success() && <condition>`. A step whose condition says no is shown as
skipped. `matrix.` is not available to a step condition, and a status function
other than `success()` on `actions/cache` or `actions/upload-artifact` is
refused: those run once, after a job that succeeded, not in the sequence.

**A failure stops the job.** After a step fails, the steps after it are
skipped, unless their condition asks for `failure()` or `always()`. A failing
step with `continue-on-error: true` is recorded as failed and counts as a
success for the job and for the steps after it — GitHub's *outcome* and
*conclusion*.

The job page lists the steps of a job with their status, and each step keeps
its own log; the job's log carries every step under a header naming it.

**External runners have to speak the step protocol.** A `plombir-git-runner`
built before the step model would run a job's steps as one script and drop
every step policy, so the server never hands it a job of steps: its poll is
answered `426` naming the job, and the job waits for an upgraded runner.
Native `.plombir-git-ci.yml` jobs run on either.

## Outputs

A step with an `id:` leaves values for the steps after it, and a job leaves
values for the jobs that need it — the same contexts GitHub has:

```yaml
name: Release
on: push
jobs:
  version:
    runs-on: ubuntu-latest
    outputs:
      number: ${{ steps.read.outputs.number }}
    steps:
      - id: read
        run: echo "number=$(cat VERSION)" >> "$GITHUB_OUTPUT"
      - if: steps.read.outputs.number != ''
        run: echo "building ${{ steps.read.outputs.number }}"
  publish:
    runs-on: ubuntu-latest
    needs: version
    env:
      VERSION: ${{ needs.version.outputs.number }}
    steps:
      - if: needs.version.result == 'success'
        run: ./publish.sh "$VERSION"
```

| Expression | Value |
|------------|-------|
| `steps.<id>.outputs.<name>` | What the step wrote to `$GITHUB_OUTPUT`. |
| `steps.<id>.outcome` | `success`, `failure` or `skipped` — before `continue-on-error`. |
| `steps.<id>.conclusion` | The same after `continue-on-error`: a failure it let through is `success`. |
| `needs.<job>.outputs.<name>` | What the needed job's `outputs:` resolved to. |
| `needs.<job>.result` | `success`, `failure`, `cancelled` or `skipped`. |

They are read in a step's `run:`, `env:`, `working-directory` and `if:`.
`needs.*` is also read in the job's `env:`, which is set when the job starts;
`steps.*` is not — a job variable exists before the first step runs. A job's
`outputs:` reads `steps.*`, `needs.*` and `env.*`, and is resolved after its
last step.

**A name that resolves to nothing is refused when the workflow is read, not
read as the empty string.** `steps.<id>` has to be a step of the same job that
runs *before* the one reading it (a job's `outputs:` may name any of its
steps); `needs.<job>` has to be a job this one lists under `needs:`. A typo
fails the file with the site and the name. An output a step simply did not
write is the empty string, as on GitHub.

**Limits.** A job's outputs, all steps together, are capped at 1 MiB, as on
GitHub. The step whose `$GITHUB_OUTPUT` passes it fails, naming the output — it
is never cut. A job's `outputs:` are masked like its log before another job can
read them, so a secret a step echoed into one arrives as `***`.

**`::set-output` is refused.** GitHub disabled the workflow command; a `run:`
that contains it fails the file, and a step that prints one at run time fails
naming it, rather than leaving an output that was never set.

A matrix job's `outputs` are those of the variant that finished last with
success, and its `result` is `failure` if any variant failed — GitHub's rule.
`needs.*` is not available in a job's `if:`, `runs-on`, `container.image` or
`environment`: those are decided when the pipeline is built, before any job ran.

### Keys parsed only so they can be refused

These are in the schema for one reason: to fail with their own name instead of
serde's "unknown field", and never to be silently dropped.

<!-- example: refused -->
```yaml
name: CI
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    container:
      image: rust:1.75
      options: --privileged
    steps:
      - name: Build
        continue-on-error: ${{ matrix.experimental }}
        run: cargo build
```

| Key | Why it is refused |
|-----|-------------------|
| step `continue-on-error` from an expression | It needs a value the workflow does not have when it is read. Write `true` or `false`. |
| step `shell`, `continue-on-error`, `timeout-minutes`, `working-directory` without `run` | They would apply to nothing. |
| `container.options` | Raw `docker run` flags would let a committed workflow undo the sandbox the runner builds around the job — the dropped capabilities, `no-new-privileges`, the pid/memory/cpu limits. There is no honest partial support. A job that needs more memory or CPUs gets them from the operator: `[ci]` `job_memory` / `job_cpus` / `job_pids` for the built-in runner, `--job-memory` / `--job-cpus` / `--job-pids` on an external runner the job selects with `runs-on:`. |

## Actions

A `uses:` step runs a **composite action**, or one of three actions this engine
implements itself.

### Built-in actions

| `uses:` | What happens |
|---------|--------------|
| `actions/checkout@…` | Nothing — the workspace already **is** a `git worktree` of the repository at the pipeline's commit, so the step is skipped. |
| `actions/cache@…` | Translated into the native [cache](ci.md#cache). |
| `actions/upload-artifact@…` | Translated into the native [artifacts](ci.md#artifacts) block. |

These three are never loaded from a repository, whatever the version after
the `@`.

### Composite actions

An `action.yml` at the root of `acme/greet`, tagged `v1`:

<!-- example: action acme/greet@v1 -->
```yaml
name: Greet
inputs:
  who:
    required: true
  greeting:
    default: Hello
outputs:
  said:
    value: ${{ steps.say.outputs.text }}
runs:
  using: composite
  steps:
    - id: say
      shell: bash
      run: echo "text=${{ inputs.greeting }}, ${{ inputs.who }}" >> "$GITHUB_OUTPUT"
```

and a workflow that uses it (`acme/tools/greet@<sha>` would name a directory
of a repository, `./.gitea/actions/greet` one of the workflow's own):

```yaml
on: push
jobs:
  build:
    steps:
      - id: hi
        uses: acme/greet@v1
        with:
          who: world
      - run: echo "${{ steps.hi.outputs.said }}"
```

The action is loaded when the workflow is read, on the server, and its steps
are written into the job in place of the `uses:` step; both runners then run
plain steps. Where it comes from:

| `uses:` | Read from |
|---------|-----------|
| `./path` | The workflow's own repository, at the commit being built. Always allowed. |
| `owner/repo[/path]@ref` | First a repository of this instance: a **public** one, or the workflow's own — a private repository of somebody else is refused. When the instance has no such repository, the upstream forge (`https://github.com` unless the operator set another). |

The operator decides which of those may be used, in `[ci]`:

| Key | Default | Meaning |
|-----|---------|---------|
| `actions_sources` | `["instance"]` | `"instance"`, `"upstream"`, both, or `[]` for neither. A `uses:` from a source that is not listed fails the workflow, naming the source. |
| `actions_upstream_url` | `"https://github.com"` | The forge `"upstream"` means: `https://`, or a `file://` directory of mirrors. |
| `actions_require_full_sha` | `false` | Load `owner/repo@ref` only by a full 40-character commit SHA, never a tag or a branch that can move. |
| `actions_cache_max_mb` | `1024` | Upstream actions are fetched (one commit deep) into a cache under the repository root and kept by commit; one fetch may not write more than this, and the actions used least recently are evicted past it. `0` removes the ceiling. |

A tag or a branch is resolved to a commit every time the workflow is read; the
job log's first step for the action says which commit, and from where.

**An action's steps are your job's steps.** They run with every variable the
job has — and the job's environment holds the repository's CI secrets — so an
action can read what a `run:` step of yours can. Allow `"upstream"` only for
forges whose actions you would paste into a workflow yourself, and pin them
(`actions_require_full_sha`).

What `action.yml` (or `action.yaml`) may say:

- `name`, `description`, `author`, `branding` — read by people, not by the engine;
- `inputs.<name>`: `description`, `required`, `default`, `deprecationMessage`. A
  `with:` key the action does not declare, and a required input the step does
  not give, fail the workflow;
- `outputs.<name>`: `description`, `value`. The caller reads them as
  `steps.<id>.outputs.<name>`;
- `runs.using: composite` and `runs.steps`. Each step takes `name`, `id`,
  `run` with its `shell` (required, as on GitHub), `uses` — another composite
  action, at most 10 deep —, `with`, `env`, `if`, `working-directory` and
  `continue-on-error`.

Inside the action, `${{ inputs.<name> }}` is the value the step gave, and
`github.action_repository` / `github.action_ref` name the action. The steps run
in the caller's job, so every other expression means what it means there.

One difference from GitHub to know before writing an `if:` on it: the calling
step's id belongs to the step that hands the action's `outputs` back, which runs
last. When a step inside the action fails, that step does not run, so
`steps.<id>.outcome` reads `skipped` where GitHub says `failure`. Test
`failure()` / `success()` — they see the inner step — rather than the outcome of
the calling step.

Refused, by name:

| What | Why |
|------|-----|
| `runs.using: node*` | A JavaScript action needs a Node runtime on the runner; there is none. |
| `runs.using: docker` | A Docker action is a container the workflow builds; refused like `container.options`. |
| `github.action_path` | The action's files do not reach the runner — only its steps do. Inline what it runs from its directory. |
| `secrets.*` inside the action | An action has no secrets context, as on GitHub; the workflow passes what it needs as an input. |
| `continue-on-error`, `timeout-minutes`, `shell`, `working-directory`, `run` on the calling step | They have no single step to apply to. |
| `timeout-minutes` on an action's step | GitHub does not allow it either. |
| Any other key in `action.yml` | It would be ignored. |

### Inputs of the built-in actions

An input the three built-in actions do not implement fails the workflow.
`actions/checkout` accepts exactly:

<!-- inventory: checkout-inputs -->
```text
fetch-depth
```

Accepted at any value, including `0`: the worktree always carries the
repository's full history, so "at least this much history" is always met.
Everything else the real action takes — `ref`, `repository`, `path`,
`submodules`, `lfs`, `sparse-checkout`, `clean`, `persist-credentials` —
changes what ends up in the workspace, and the workspace answers to none of
them. A job that asked to check out a different branch would otherwise have run
green against the pipeline's own commit.

`actions/cache` accepts exactly:

<!-- inventory: cache-inputs -->
```text
path
key
```

`restore-keys` (a miss becomes a fallback hit), `fail-on-cache-miss` (a miss
becomes a failure) and `lookup-only` (skip the restore) all change the outcome
of a miss, which this cache does not implement, so accepting them would report
the opposite of what the workflow asked for.

`actions/upload-artifact` accepts exactly:

<!-- inventory: upload-artifact-inputs -->
```text
name
path
```

`path` takes one path per line, exactly as the real action does; `name`
defaults to the job's own name. The rest change behaviour this artifact store
does not implement: `retention-days` would override a policy the repository
owns, `if-no-files-found`, `overwrite` and `include-hidden-files` each decide
what an empty or partial match means, and `compression-level` picks an archive
format that is not the `tar` this engine writes.

**One `actions/upload-artifact` step per job.** A second one is refused by name.
The translation keeps one artifact per job, so a second step would silently
replace the first and the workflow would show two uploads where only the last
one ever existed.

## Expressions

`${{ … }}` is resolved in two different places, and the two support different
contexts.

**In values that reach the shell** — `run:`, any `env:` value,
`working-directory`, and `actions/cache`'s `key` — these are available:

| Expression | Becomes |
|------------|---------|
| `github.ref` | `${CI_REF}` |
| `github.sha` | `${CI_SHA}` |
| `github.event_name` | `${CI_EVENT}` |
| `github.repository` | `${CI_REPOSITORY}` |
| `github.repository_owner` | `${CI_REPOSITORY_OWNER}` |
| `github.event.schedule` | `${CI_EVENT_SCHEDULE}` — the `- cron:` entry of a `schedule` run; empty for any other event. |
| `env.NAME` | The variable's value. |
| `secrets.NAME` | The secret: the environment's value when the job declares an environment that sets the name, the repository-wide one otherwise. |
| `matrix.NAME` | The variant's coordinate. |
| `inputs.NAME` | The dispatch or call input. |

**Between steps**, the runner resolves `steps.*` and `needs.*` — see
[Outputs](#outputs).

**In job fields resolved before a runner starts** — `runs-on`,
`container.image` and `environment` — only the first five `github.*` above, plus
`matrix.NAME` and `inputs.NAME`. `env.` and `secrets.` have no value at that
boundary; they are refused there by field and expression rather than left in the
configuration as literal `${{ … }}`.

`vars.NAME` is **not** supported anywhere: Plombir Git has no repository or
organization configuration-variable store, and silently reading `env.NAME`
instead would be a different Actions context with the same name.

Any unsupported expression fails the workflow, naming the site and the
expression — including a `${{` that is never closed.

## Defaults

```yaml
name: CI
on: push
defaults:
  run:
    working-directory: server
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: cargo build
```

`run.working-directory` and `run.shell` are honoured; a job's own
`defaults.run` overrides the workflow's, and a step's `working-directory` /
`shell` overrides both. See [Steps](#steps) for the shells.

## Containers

```yaml
name: CI
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    container:
      image: rust:1.75
      env:
        RUSTFLAGS: -D warnings
    steps:
      - run: cargo build
```

`image` is the image the job runs in, and `env` becomes the container's
environment — job and workflow `env:` override it, which is the precedence
Actions uses. `options` is refused.

Whether a job may run at all depends on the instance. Docker execution, host
execution and external runners are all off by default, so a default instance
runs no job until the operator enables one of them; with Docker on, every job
wants either a `container.image` or a runner that supplies one.

## Services

```yaml
name: CI
on: push
jobs:
  test:
    runs-on: ubuntu-latest
    container:
      image: postgres:16
    services:
      postgres:
        image: postgres:16
        env:
          POSTGRES_PASSWORD: ${{ secrets.PG_PASSWORD }}
        options: >-
          --health-cmd pg_isready
          --health-interval 5s
          --health-retries 20
    steps:
      - run: pg_isready -h postgres
```

Every key under `services:` is a container the runner starts before the job's
first step and removes after its last one. The job gets a Docker network of its
own; each service joins it under its key, and a job in `container:` joins it
too, so `postgres:5432` reaches the service above. A job that runs on the host
reaches a service through `ports:` instead, at `localhost:<port>`:

```yaml
name: CI
on: push
jobs:
  test:
    runs-on: self-hosted
    services:
      cache:
        image: registry.example.com/acme/redis:7
        credentials:
          username: ci-bot
          password: ${{ secrets.REGISTRY_TOKEN }}
        ports:
          - 6379:6379
        options: --health-cmd "redis-cli ping"
    steps:
      - run: redis-cli -h localhost -p 6379 ping
```

| Key | Type | Meaning |
|-----|------|---------|
| `image` | string | The service's image. Pulled with the operator's registry setup and the same image rules as `container.image`; `${{ matrix.* }}` and `${{ secrets.* }}` are read. |
| `env` | map string→string | The service's environment. `${{ secrets.* }}` is filled in by the runner from the job's own secrets and never reaches a command line. |
| `ports` | list | `<container port>` or `<runner port>:<container port>`, optionally `/tcp` or `/udp`. Published on the runner's **loopback only**; an address in front is refused. Without a runner port Docker picks one — `job.services.*.ports` is not available, so a host job that needs the number names it. |
| `credentials` | `{ username, password }` | A registry login for this image. The pull runs with a Docker configuration holding just that login, removed right after; the operator's own login is never changed. |
| `options` | string | Health-check flags only: `--health-cmd`, `--health-interval`, `--health-timeout`, `--health-retries`, `--health-start-period`, `--health-start-interval`. |

Before the first step the runner waits until every service whose image or
`options:` declares a health check reports **healthy**, for at most five minutes.
A service that turns unhealthy, stops, or is not healthy in time fails the job
before any step runs, with the end of the service's log. A service with no
health check is waited for only until it is running — the same rule GitHub
uses, so give a database its `--health-cmd`.

A service runs in the job's sandbox: the operator's pid, memory and CPU limits,
`no-new-privileges`, never privileged, no Docker socket, no workspace mount. It
keeps the few capabilities an official database image's entrypoint needs to
take ownership of its data directory and drop to its own user (`CHOWN`,
`DAC_OVERRIDE`, `FOWNER`, `SETGID`, `SETUID`, `KILL`, `NET_BIND_SERVICE`);
everything else is dropped.

The services and their network go when the job does — after its last step, on a
timeout, on a cancellation. A runner that dies with a job in hand leaves them
behind, and the next job with services on the same machine removes them first.

Services need Docker. The built-in runner refuses a job with `services:` when
`ci.docker_enabled` is off, naming the services; an external runner without a
reachable Docker daemon fails the job the same way, and a runner too old to
start services is never handed such a job.

| Key | Why it is refused |
|-----|-------------------|
| `volumes` | A bind mount is a way into the runner's filesystem, and a named volume outlives the job. |
| `options` other than the health-check flags | Any other `docker run` flag — `--privileged`, `--network`, `-v`, `--cap-add` — could undo the sandbox, the reason `container.options` is refused. |
| `services` on a job that calls a reusable workflow | The called workflow's jobs carry their own `services:`. |
| `${{ steps.* }}` or `${{ needs.* }}` in a service's `image`, `env` or `credentials` | A service starts before the job's first step; the runner fills these in between steps, never into a container that is already running. |

## Matrix

```yaml
name: CI
on: push
jobs:
  test:
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os:
          - ubuntu-latest
          - macos-latest
        features:
          - default
          - all-features
    steps:
      - run: cargo test --features ${{ matrix.features }}
```

Values must be strings, numbers or booleans. The rules of the expansion itself
(naming, the `MATRIX_*` variables, the 256-variant cap) are the native format's
and are documented under [Matrix](ci.md#matrix); `matrix.NAME` reads a
variant's coordinate in `runs-on`, `container.image`, `environment` and the
job's `if:`, also for a key such as `node-version` that is no variable name.

### `include` and `exclude`

```yaml
name: CI
on: push
jobs:
  test:
    runs-on: ubuntu-latest
    strategy:
      fail-fast: false
      max-parallel: 2
      matrix:
        os: [linux, mac]
        node: [18, 20]
        exclude:
          - os: mac
            node: 18
        include:
          - os: linux
            node: 20
            coverage: true
          - os: windows-wsl
            node: 20
    steps:
      - run: npm test
```

GitHub's rules, exactly: the dimensions multiply out; `exclude` drops every
combination one of its entries matches, and a partial match is enough; then
each `include` entry is added to every original combination none of whose
dimension values it would overwrite (values an earlier `include` added may be
overwritten), and an entry that fits no combination becomes one of its own.
The 256-variant cap counts what `include` adds. Entries are mappings of
strings, numbers and booleans.

### A matrix from a needed job's outputs

```yaml
name: CI
on: push
jobs:
  setup:
    runs-on: ubuntu-latest
    outputs:
      matrix: ${{ steps.list.outputs.matrix }}
    steps:
      - id: list
        run: echo 'matrix={"package":["api","web"]}' >> "$GITHUB_OUTPUT"
  test:
    runs-on: ubuntu-latest
    needs: setup
    strategy:
      matrix: ${{ fromJSON(needs.setup.outputs.matrix) }}
    steps:
      - run: make test PACKAGE=${{ matrix.package }}
```

The whole matrix, one dimension, `include` or `exclude` may be
`${{ fromJSON(needs.<job>.outputs.<name>) }}` — that expression and nothing
around it, naming a job this one lists under `needs:`. Such a job cannot be
expanded when the workflow is read: until its stage is reached the pipeline
shows it as one pending job under its own name, which is then replaced by its
variants. What the needed job left is checked like a matrix in the file; when
it is not one — empty, not JSON, the wrong shape, more than 256 variants — that
job fails with the reason in its log, and the pipeline fails with it.

### `fail-fast` and `max-parallel`

- `fail-fast` — `true` unless the workflow says `false`, as on GitHub: once a
  variant fails, the variants of the same job that have not finished are
  canceled, with a line in their log saying which one failed. A variant that
  fails under the job's `continue-on-error: true` does not trigger it.
- `max-parallel` — at most that many variants of the job are handed to
  runners at once; the next one waits until one of them finishes. The built-in
  runner runs the jobs of a stage one after another anyway.

Both must be literal values: an expression has no value when the graph is
written, and is refused by name.

## Concurrency

```yaml
name: Deploy
on: push
concurrency:
  group: deploy-${{ github.ref }}
  cancel-in-progress: true
jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - run: ./deploy.sh
```

`group` and `cancel-in-progress` are the two keys. `group` additionally
understands `${{ github.workflow }}`, so the canonical GitHub group
`${{ github.workflow }}-${{ github.ref }}` works. A group that still contains an
unresolved expression after substitution is refused: left alone it would be one
literal group shared by every ref of the repository, which under
`cancel-in-progress` is a standing order to cancel whatever else is running.

## Reusable workflows

A job may call another workflow **in the same repository**:

<!-- example: reusable-caller -->
```yaml
name: Release
on: push
jobs:
  build:
    uses: ./.gitea/workflows/build.yml
    with:
      profile: release
    secrets: inherit

  publish:
    needs: build
    runs-on: ubuntu-latest
    steps:
      - run: ./publish.sh
```

<!-- example: reusable-callee build.yml -->
```yaml
name: Build
on:
  workflow_call:
    inputs:
      profile:
        description: Cargo profile
        type: string
        default: debug
jobs:
  compile:
    runs-on: ubuntu-latest
    steps:
      - run: cargo build --profile ${{ inputs.profile }}
```

The rules:

- `uses:` must be `./.gitea/workflows/<file>` — a repository-local path with no
  subdirectory and no `..`. A remote `owner/repo/.gitea/workflows/x.yml@ref` is
  refused; there is no fetcher for it.
- The called file must declare `on: workflow_call`, and its inputs are checked
  against `with:` — an undeclared input, a missing required one and a wrong type
  are all refusals.
- `secrets:` accepts only `inherit`. Repository secrets are already scoped to
  every job, so named remapping would be a promise with no mechanism behind it.
- The called workflow may not declare `concurrency:` — declare it in the caller,
  where the pipeline it would govern actually exists.
- Nesting is capped at four levels, and a cycle is refused by name.
- Called jobs are flattened into the caller as `<caller job>/<called job>`, and a
  `needs:` on the calling job becomes a dependency of the called workflow's root
  jobs.

## When a workflow is refused

Every rule on this page is checked when the pipeline is triggered, and the
refusal is a client error naming `.gitea/workflows/<file>` and the offending
job, key or expression. `git push` output, the API response and the UI all show
the same text.

A refused workflow produces **no** pipeline — not a partial one. If a workflow
you expected has not run and there is no error in sight, check that the event
you are waiting for is one of the five above, and that the `on:` filters match
the ref and the paths you changed.
