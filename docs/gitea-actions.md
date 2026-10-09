# Gitea Actions workflows — the supported subset

Plombir Git reads `.gitea/workflows/*.yml` in the Gitea/GitHub Actions format, and
implements a **subset** of it. This page is that subset's boundary.

Reaching for GitHub's own documentation instead will mislead you, which is why
this page exists rather than a link. There is no Actions runtime here: a job is
a shell script the runner executes in a worktree of your repository, so the
parts of Actions that are *the runtime* — the marketplace, `actions/*` beyond
two special cases, step outputs, composite actions, service containers — have
nothing to map onto and are refused.

**Refusals are loud and whole-file.** Every block below carries a closed schema.
An unknown key, an unrunnable trigger, an action this engine does not implement
— any one of them fails the trigger with a message naming the file, the job and
the key. You never get a pipeline that ran a shortened version of your workflow.
The one thing you must not read into that: a refusal means *no pipeline*, so a
workflow that has never run is worth checking against this page before assuming
the event did not fire.

**Which format wins.** `.gitea/workflows/` is tried first. The native
[`.plombir-git-ci.yml`](ci.md) is used when this directory is absent at the commit
being built, or when no workflow in it is triggered by the event. The native
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

Anything else at the root — `permissions`, `run-name`, `jobs.<id>.outputs`, and
the rest of the Actions vocabulary — is an unknown key and fails the file.

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
| `workflow_call` | Nothing — it declares the file *callable* by another workflow. See [Reusable workflows](#reusable-workflows). |

Every other `on:` name is refused by the name you wrote, with that list in the
message. That includes names Actions defines and Plombir Git has no producer for
— `schedule` (there is no scheduler in the tree) and `pull_request_target` (it
would run the workflow from the base branch, which nothing here does):

<!-- example: refused -->
```yaml
name: Nightly
on:
  schedule:
    - cron: '0 3 * * *'
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
| `defaults` | block | `run:` defaults for this job. See [Defaults](#defaults). |
| `strategy` | block | Only `matrix`. See [Matrix](#matrix). |
| `environment` | string or `{ name: … }` | A deployment environment **the repository already has**; a protected one holds the job for approval, and an environment-scoped CI secret reaches only jobs that declare it. |
| `timeout-minutes` | integer | Per-job timeout. Converted to seconds, and the result must land in `1`–`86400` — so `1`–`1440` minutes. |
| `continue-on-error` | bool | A failure of this job does not fail the pipeline. |
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
`github.sha`, `github.repository`, `github.repository_owner`, and `env.NAME`.

A **job**'s `if:` may also read `matrix.NAME`. A **step**'s may not — a step
condition is evaluated while the pipeline is being built, before matrix variants
exist, so a `matrix.` reference there is refused instead of quietly resolving to
nothing.

## Steps

| Key | Type | Meaning |
|-----|------|---------|
| `name` | string | Label for the step. |
| `run` | string | Shell commands. |
| `uses` | string | One of exactly two actions. See [Actions](#actions). |
| `with` | map | Inputs for that action. |
| `env` | map string→string | Variables for this step. |
| `if` | string | Condition. Evaluated while the pipeline is built; a false step is left out. |
| `working-directory` | string | Directory for this step's `run:`, relative to the workspace. Requires `run:`. |

Every `run:` in a job is concatenated into one script, prefixed with `set -e`, so
the **first** failing command fails the job.

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
    defaults:
      run:
        shell: pwsh
    container:
      image: rust:1.75
      options: --privileged
    steps:
      - name: Build
        id: build
        shell: bash
        continue-on-error: true
        timeout-minutes: 5
        run: cargo build
```

| Key | Why it is refused |
|-----|-------------------|
| step `shell`, `defaults.run.shell` | Every script runs under `sh -c` with `set -e`. Accepting the key would promise a shell the runner never invokes. |
| step `id` | Step outputs and `steps.<id>.*` do not exist here, so an id addresses nothing. |
| step `continue-on-error` | Steps are one script; the failure boundary is the job. Use `continue-on-error` on the **job**. |
| step `timeout-minutes` | Same reason — the timeout boundary is the job. |
| step `working-directory` without `run` | It would apply to nothing. |
| `container.options` | Raw `docker run` flags would let a committed workflow undo the sandbox the runner builds around the job — the dropped capabilities, `no-new-privileges`, the pid/memory/cpu limits. There is no honest partial support. |

## Actions

Three, and only three, `uses:` values are implemented:

| `uses:` | What happens |
|---------|--------------|
| `actions/checkout@…` | Nothing — the workspace already **is** a `git worktree` of the repository at the pipeline's commit, so the step is skipped. |
| `actions/cache@…` | Translated into the native [cache](ci.md#cache). |
| `actions/upload-artifact@…` | Translated into the native [artifacts](ci.md#artifacts) block. |

Any other action fails the workflow — there is no runtime to execute it, and
skipping it would be worse than refusing it.

An input these three do not implement fails the workflow too.
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
| `env.NAME` | The variable's value. |
| `secrets.NAME` | The secret's value: repository-wide, plus the job's environment scope when the job declares one. |
| `matrix.NAME` | The variant's coordinate. |
| `inputs.NAME` | The dispatch or call input. |

**In job fields resolved before a runner starts** — `runs-on`,
`container.image` and `environment` — only the five `github.*` above, plus
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

Only `run.working-directory` is honoured; a job's own `defaults.run` overrides
the workflow's, and a step's `working-directory` overrides both.
`defaults.run.shell` is refused — see the table above.

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

`strategy` accepts `matrix` and nothing else — no `fail-fast`, no
`max-parallel`. Values must be strings, numbers or booleans. The rules of the
expansion itself (naming, the `MATRIX_*` variables, the 256-variant cap) are the
native format's and are documented under [Matrix](ci.md#matrix).

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
