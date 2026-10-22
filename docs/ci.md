# Pipeline configuration — `.plombir-git-ci.yml`

This is the reference for Plombir Git's **native** pipeline format: the file you
commit to the root of your own repository. The alternative format, Gitea
Actions (`.forgejo/workflows/*.yml`, `.gitea/workflows/*.yml` or `.github/workflows/*.yml`), has its own page —
[the supported subset](gitea-actions.md). Read that one rather than Gitea's or
GitHub's: Plombir Git implements a subset of Actions, and a workflow that is valid
by their documentation is often refused whole by this engine.

**Where the file goes:** `.plombir-git-ci.yml` in the repository root, at the
commit being built. The engine reads it from the commit, not from a checkout,
so a pipeline always runs the configuration that was committed with the code.

**Which format wins:** the Actions workflows are tried first, from the first of
`.forgejo/workflows`, `.gitea/workflows` and `.github/workflows` the commit has —
Forgejo's order. The native file is used when none of these directories is at
the commit, or when no workflow in the one read is triggered by this event. If
nothing exists the trigger is refused with a message naming every path. Workflows that exist but sit out the event are not
that refusal: a push to the branch of a repository whose only workflow is
`on: pull_request` simply runs nothing — no pipeline, the way Gitea and GitHub
treat it — while a manual run or a retry of such an event answers `400` naming
the event and the ref. The native format is also the supported escape hatch
for everything the Actions subset cannot express.

**Unknown keys are refused, not ignored.** Every job block, the `concurrency`
block and the `cache` block carry a closed schema: a misspelled key fails the
trigger with the parser's line and column instead of quietly changing nothing.
That is why this page exists — the names below are the whole vocabulary, and
guessing one costs you a red pipeline.

**Size ceiling.** The engine must hold the complete YAML document while parsing
it, so a native config larger than the following number of bytes is refused by
name before its blob is loaded:

<!-- inventory: native-ci-size-ceiling -->

```text
1048576
```

## A complete example

Every key the format accepts, in one file:

```yaml
stages:
  - build
  - test
  - deploy

concurrency:
  group: ci-${{ branch }}
  cancel_in_progress: true

build:
  stage: build
  image: rust:1.75
  script:
    - cargo build --locked --release
  variables:
    CARGO_TERM_COLOR: "always"
  timeout_seconds: 1800
  tags:
    - linux
  cache:
    key: cargo-${CI_REF}
    paths:
      - target
  artifacts:
    name: release-binaries
    paths:
      - target/release/plombir-git

test:
  stage: test
  image: rust:1.75
  script:
    - cargo test --all
  matrix:
    features:
      - default
      - all-features

lint:
  stage: test
  image: rust:1.75
  script:
    - cargo clippy --all-targets -- -D warnings
  if: github.event_name == 'push'
  allow_failure: true

deploy:
  stage: deploy
  image: alpine:3.20
  script:
    - ./scripts/deploy.sh
  only:
    - main
  when: manual
  environment: production
```

## Top level

| Key | Type | Meaning |
|-----|------|---------|
| `stages` | list of strings | Stage names, in execution order. Jobs of one stage run together; the next stage starts when the previous one finishes. Optional — see the note below. |
| `concurrency` | block | Serializes or cancels pipelines that share a group. See [Concurrency](#concurrency). |
| *anything else* | block | A **job**. The key is the job's name and the value is a [job block](#jobs). |

> **Every stage a job names must be listed in `stages`.** A job whose `stage`
> is not among them fails the trigger by name — you get a message naming the
> job, the stage it asked for and the stages this file declares, never a
> pipeline that runs without the job.
>
> **A job with no `stage:` needs no `stages:`.** It is put in a stage literally
> called `default`, and that stage is created for you: a file that declares only
> jobs runs all of them, in one `default` stage. When the file *does* list
> stages, the synthesized `default` runs after all of them — list `default`
> among your `stages` yourself to place it somewhere else.
>
> Two more rules on the same surface: `stages` may not list one name twice (the
> second declaration would take the jobs and leave the first stage permanently
> empty), and a file that declares no jobs at all is refused rather than run as
> an empty, and therefore green, pipeline.

## Jobs

A job's name is its key at the top level of the document; the block underneath
it takes these keys, of which only `script` is required:

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `script` | list of strings | — | The shell commands to run, in order. **Required.** |
| `stage` | string | `default` | The stage this job belongs to. Must appear in `stages`. |
| `image` | string | none | Container image to run the job in. |
| `only` | list of strings | run always | Run this job only on these refs. |
| `variables` | map string→string | none | Environment variables for the job's script. |
| `when` | string | `on_success` | `on_success` or `manual`. |
| `if` | string | run always | Condition evaluated before the job is scheduled. Also spelled `condition`. |
| `environment` | string | none | Name of an existing deployment environment; a protected one pauses the job for approval. |
| `allow_failure` | bool | `false` | A failure of this job does not fail the pipeline. |
| `timeout_seconds` | integer | instance default | Per-job execution timeout, `1`–`86400` (24 h). |
| `tags` | list of strings | any runner | Runner labels required to pick this job up. |
| `matrix` | map string→list of strings | none | Expand the job into one run per combination. See [Matrix](#matrix). |
| `cache` | block | none | Directories carried between runs. See [Cache](#cache). |
| `artifacts` | block | none | Files published for download after the job succeeds. See [Artifacts](#artifacts). |

### `script`

The lines are handed to `sh -c` as one program, prefixed with `set -e`, so the
**first** failing command fails the job. Without that prefix a shell reports the
exit code of the *last* command it ran, and a job would go green on a failed
test whenever a later line succeeded. If you write `set -e` yourself as the
first line it is not added twice.

### `image`

The image the job runs in. Two instance-level settings decide whether a job is
runnable at all, and both refuse loudly rather than falling back:

- `image` is set but Docker execution is disabled on the instance — the job
  fails immediately. Running a container-targeted script on the host would give
  it the server's own permissions.
- `image` is **not** set and host execution is disabled (`ci.allow_host_runner`
  is `false`, which is the default) — the job fails immediately. Give the job an
  image, route it to a dedicated runner with `tags` (which needs
  `ci.external_runners`, see [`tags`](#tags)), or have the operator enable host
  execution on a trusted instance.

So a default instance — Docker execution off, host execution off, no external
runners — runs no job at all. The operator turns one of the three on first; with
Docker on, every job wants an `image`.

The value must be an image reference, `[registry[:port]/]path[:tag][@digest]`
(an IPv6 registry is written in brackets, digests are `sha256:`, `sha384:` or
`sha512:`). Anything else — `--privileged`, `-v/:/host` — is refused when the
pipeline is created, with the job and the rule in the error, and the runners
refuse it again before `docker run`.

**Rootful and rootless Docker are both supported.** The container runs with
`--cap-drop ALL`, so it can write to the mounted workspace only as the
workspace's owner. On a rootful daemon that is the runner's own uid and gid
(`--user <uid>:<gid>`). On a rootless daemon — `docker info` lists
`name=rootless` under its security options — a container's uid is mapped onto
a subordinate id of the daemon's user, so the runner's uid would not own the
workspace inside the container; the job runs as the container's root
(`--user 0:0`) instead, which is the daemon's user on the host, and the
capabilities stay dropped. That needs the runner to run as the user that runs
the rootless daemon. The runner asks the daemon once per process; when
`docker info` does not answer, the job runs as the runner's uid and its log
says that writing to `/workspace` may fail on a rootless daemon.

### `only`

Plain string equality against the ref — no globs, no patterns. Each entry is
compared with both the short branch name and the full ref, so `main` and
`refs/heads/main` both match a push to `main`. A job whose `only` list matches
nothing is left out of the pipeline.

### `variables`

Exported into the job's environment. Names must look like `[A-Za-z_][A-Za-z0-9_]*`;
a name that does not is refused when the pipeline is created. A name the runner
sets itself (`CI`, `CI_*`) is ignored, and the job log says which ones.

A job that runs in a container gets its variables through the runner's own
`docker` client, which runs on the host. Names that would reconfigure that
client — `PATH`, `HOME`, `LANG`, `LC_*`, `TMPDIR`, `XDG_RUNTIME_DIR`, `LD_*`,
`DOCKER_*`, `*_PROXY`, `SSL_CERT_*`, the Go runtime's
`GOMAXPROCS`/`GOGC`/`GODEBUG`/… — are not passed to the container, and the job
log names each one. Set the value under another name and export it in the
script if the job needs it.

The image is found or pulled first, by a separate `docker` client that carries
none of the job's variables; only that one can start a registry credential
helper. The container then starts from the image's ID with `--pull never`. So
the variables a credential helper reads — every `AWS_*` (`AWS_PROFILE`
included), `GOOGLE_APPLICATION_CREDENTIALS`, `CLOUDSDK_*` — are ordinary job
variables and reach the container, and a missing image is always pulled with
the runner operator's credentials, never the job's. The pull's output opens the
job log.

The runner supplies these on its own: `CI_PIPELINE_ID`, `CI_COMMIT_SHA`,
`CI_SHA`, `CI_REF`, `CI_REF_NAME` (`CI_REF` by its short name: `main`, `v1.2`),
`CI_EVENT`, `CI_REPOSITORY`, `CI_REPOSITORY_OWNER`,
`CI_HEAD_REF` and `CI_BASE_REF` — the branch a pull request comes from and the
one it targets, empty for every other event — and, where a job token is issued,
`CI_JOB_TOKEN`. A run of an Actions workflow's `on: schedule` also gets
`CI_EVENT_SCHEDULE`, the cron entry that fired.

### `when`

`on_success` (the default) runs the job as part of the pipeline. `manual` holds
it until someone starts it from the UI or the API. Any other value is refused
with a message naming the job and the two accepted values.

### `if`

A small, deliberately limited expression evaluated **before the job is
scheduled**, against the event that triggered the pipeline. `condition:` is
accepted as a synonym.

- Contexts: `github.ref`, `github.ref_name`, `github.event_name`, `github.sha`,
  `github.repository`, `github.repository_owner`; plus `env.NAME` for the job's
  variables and `matrix.NAME` for the current matrix variant.
- Operators: `==`, `!=`, `&&`, `||`, `!`, parentheses; string literals in single
  quotes, a quote inside one doubled (`'it''s'`) as on GitHub, a backslash an
  ordinary character; `true` / `false`.
- Functions: `startsWith(a, b)`, `endsWith(a, b)`, `contains(a, b)`, `success()`.

Anything else — an unknown context name, an unsupported function — is refused
when the pipeline is triggered, naming the job and the reason.

### `environment`

The name of an environment **the repository already has**, 1–255 characters, no
control characters. If that environment is marked protected, the job waits for
an approval instead of running. The job reads that environment's
[secrets](#secrets-and-workload-identity), and its OIDC token names the
environment instead of the ref.

Environments are not created by naming them here. A name the repository has no
environment for is refused when the pipeline is triggered, naming the job, the
name it asked for and the environments the repository does have — because the
alternative is worse than a failed run: a mistyped `producton` would resolve to
no environment, find no protection there, and send the deploy its author gated
behind `production` straight to a runner.

### `timeout_seconds`

Between `1` and `86400`. A value outside the range is refused with a message
naming the job and the bounds. Omit the key to take the instance's default.

### `tags`

Labels a runner must carry to pick the job up. An empty or missing list means
any runner may run it.

Matching them against a runner needs runners to match against, so what `tags`
does depends on how the instance runs CI:

- **External runners** (`ci.external_runners = true`) — the job waits for a
  runner registered for this repository whose labels cover every tag. A runner
  token is a hard one-repository capability; matching labels never let it cross
  that boundary. Tags no runner in the repository matches leave the job queued.
- **In-process runner** (the default) — CI runs inside the server, and what that
  runner answers to is `ci.runner_labels`:

  ```toml
  [ci]
  runner_labels = ["self-hosted", "ubuntu-latest", "ubuntu-24.04", "ubuntu-22.04", "ubuntu-20.04"]
  ```

  A tag the list covers is honoured by running the job here — that is what
  declaring the label means. A tag it does not cover is **refused by name**,
  saying which label was asked for, which ones this instance carries, and the
  three ways to fix it. An empty list answers to nothing, so every job that
  declares a label is refused.

The refusal arrives at one of two moments, and which one depends on who wrote
the key:

- A hand-written `tags:` is refused **when the pipeline is triggered**. Routing
  is the only thing that key is for, so a value this instance cannot honour is
  the author's mistake to see immediately — and with no external runners there
  is no second runner to queue for, so leaving the job pending would hang for
  ever with nothing saying why.
- A workflow's `runs-on:` is refused **by the runner, per job**. It is mandatory
  GitHub syntax that most workflows fill with boilerplate, so failing the whole
  trigger over one job would take that workflow's other jobs down with it. The
  pipeline is built, the jobs that this instance can run do run, and the one
  that asked for another machine fails with the refusal in its log.

> The default list claims the `ubuntu-*` spellings precisely because they are
> boilerplate rather than a routing decision. It claims nothing else — not even
> `linux` or `x64`, which would describe most servers truthfully: a default that
> differed between an x86 and an ARM build is one no shipped config could state.
> An instance whose workflows say `runs-on: [self-hosted, linux, x64]` adds
> those labels to `ci.runner_labels`, which is the machine saying what it is.

> A pipeline created while `ci.external_runners` was on keeps its tags in the
> job rows. If the setting is turned off and such a pipeline is retried, the
> trigger-time check is long behind it — the runner's own refusal is what stops
> those jobs from running here.

## Secrets and workload identity

Secrets are configured per repository under **Settings → CI secrets**. Each one
is stored encrypted at rest, injected into a job's environment, and masked in
the stored job log.

A secret is either **repository-wide** or scoped to one **environment** of the
repository:

- A repository-wide secret reaches every job.
- An environment's secret reaches only a job that declares that environment
  with [`environment:`](#environment), and, when the environment is
  protected, only after an approval has released the job to a runner. A job
  that declares no environment reads no environment's secrets, whichever
  branch or pull request it runs from, and a `staging` job never sees
  `production`'s.
- The same name can be set once per scope. Where a job reads both scopes and
  both set a name, it gets the **environment's** value: a production deploy
  receives production's `DEPLOY_TOKEN`, never the repository-wide one.

An environment's secrets are only as strong as its protection. On an
unprotected environment, any branch that can trigger a pipeline can declare
the environment and read them; put deploy credentials behind a protected one.
Deleting an environment deletes its secrets, and the audit entry for the
deletion lists their names.

### OIDC tokens

A job also receives `CI_OIDC_TOKEN_URL` beside `CI_JOB_TOKEN`.
`GET <CI_OIDC_TOKEN_URL>?audience=<audience>` with
`Authorization: Bearer $CI_JOB_TOKEN` answers a short-lived, Ed25519-signed JWT
that a cloud provider's OIDC federation verifies against the instance's public
keys at `/api/v1/ci/oidc/jwks`, with no shared secret. The token is bound to the
running job: a finished job, a retried pipeline or a stale job token is refused.

| Claim | Value |
|-------|-------|
| `iss` | The instance's OIDC issuer (`…/api/v1/ci/oidc`) |
| `aud` | The requested audience |
| `sub` | `repo:<owner>/<name>:environment:<env>` for a job with an environment, `repo:<owner>/<name>:ref:<ref>` otherwise |
| `repository` / `repository_id` / `repository_owner` | `owner/name`, the numeric repository id, the owner |
| `ref` / `ref_type` / `sha` | The full ref, its kind (`branch`, `tag`, `pull_request`, `unknown`), the commit |
| `environment` | The job's environment; absent for a job without one |
| `pipeline_id` / `job_id` | The pipeline and the job |
| `actor` | The account that triggered the pipeline; absent when none did |

Write the trust policy on the cloud side like this:

- **Match the whole `sub`, not a prefix.** The environment name is copied into
  the subject as it is, so `…:environment:production` is also the start of
  `…:environment:production-eu`.
- **Also require `repository_id`.** `owner/name` is a name, and names are
  reused: after a rename or a transfer, or when a deleted repository's name is
  created again, a different repository produces the same subject. The numeric
  id is never reused.
- **Give a production role to a protected environment only**, for example
  `sub = repo:acme/app:environment:production` together with
  `repository_id = 42`, where `production` is **protected** with required
  approvals. An unprotected environment can be declared from any branch.
- A pull request's ref is `refs/pull/<n>/head`, so it can never produce
  `repo:acme/app:ref:refs/heads/main`.

> Until this release the subject was `repo:<repository id>:pipeline:<id>:job:<id>`,
> which only let a policy trust `repo:<id>:*`: every branch and every pull
> request. Policies written for that format no longer match and the exchange
> with the cloud provider fails until they are updated; see the changelog.

## Matrix

`matrix` turns one job declaration into one run per combination of the values —
the Cartesian product of every dimension:

```yaml
stages:
  - test

test:
  stage: test
  image: rust:1.75
  script:
    - cargo test --features "$MATRIX_FEATURES"
  matrix:
    os:
      - linux
      - macos
    features:
      - default
      - all-features
```

- Each variant is named after the job plus its coordinates, e.g.
  `test [features=default, os=linux]`.
- Each dimension is exported to the script upper-cased and prefixed:
  `MATRIX_OS`, `MATRIX_FEATURES`; a `-` in the key becomes `_`, so
  `node-version` is `MATRIX_NODE_VERSION`. The job's own `variables` are present
  too.
- The product is capped at **256 variants**, and a dimension with an empty list
  is refused — both name the job in the error.

## Cache

`cache` carries directories from one run of a job to the next:

```yaml
stages:
  - build

build:
  stage: build
  image: rust:1.75
  script:
    - cargo build
  cache:
    key: cargo-${CI_REF}
    paths:
      - target
      - .cargo/registry
```

| Key | Type | Meaning |
|-----|------|---------|
| `key` | string | Cache identity. Runs that resolve to the same key share an archive. |
| `paths` | list of strings | Directories to save, relative to the workspace. |

- `key` may reference the job's environment as `$NAME` or `${NAME}`, which is
  how you get one cache per branch (`cargo-${CI_REF}`) instead of one shared by
  every ref. After substitution it must be 1–512 bytes.
- `paths` takes 1–64 entries. Each must stay inside the workspace: an absolute
  path or one containing `..` is refused.
- A cache miss, a failed restore or a failed save **never** fails the job — the
  reason is appended to the job log and the run continues without the cache.

## Artifacts

`cache` is the engine's own bookkeeping; `artifacts` is what a **person**
downloads afterwards. The declared paths are packed into one `tar` archive and
published against the job, where the pipeline page lists them:

```yaml
stages:
  - build

build:
  stage: build
  image: rust:1.75
  script:
    - cargo build --release
  artifacts:
    name: plombir-git-linux
    paths:
      - target/release/plombir-git
      - target/release/plombir-git-runner
```

| Key | Type | Meaning |
|-----|------|---------|
| `name` | string | Name the archive is stored and downloaded under. Defaults to the job's own name. |
| `paths` | list of strings | Files and directories to pack, relative to the workspace. |

- `name` takes 1–100 characters and only ASCII letters, digits, `.`, `-` and
  `_`. Anything else is refused at trigger time rather than stripped, so the
  name you write is the name you download. No suffix is added: the download is
  the `tar` archive under exactly that name, so write `name: plombir-git.tar`
  yourself if you want the file to arrive with one.
- `paths` takes 1–64 entries. Each must stay inside the workspace: an absolute
  path or one containing `..` is refused.
- **Only a job that succeeded publishes.** A failed run's output is a partial
  build, and publishing it under the name a green run uses would hand whoever
  downloads it something broken with nothing saying so.
- If **none** of the declared paths exist when the job finishes, nothing is
  published and the reason is appended to the job log. An empty archive is not
  published: it would show an artifact on the pipeline whose emptiness is only
  discovered by whoever unpacks it.
- A failed publication does **not** fail the job — the script has already run
  and its exit code is the answer — but, unlike a cache failure, the reason
  always reaches the job log, because a missing artifact is usually the point of
  the job.
- How long an artifact is kept is the repository's own setting
  (`artifact_retention_days` under **Settings → Retention**), not the workflow's.
- One artifact per job. Two jobs may publish under the same `name`; each
  artifact belongs to the job that produced it.

## Concurrency

```yaml
concurrency:
  group: deploy-${{ branch }}
  cancel_in_progress: true

stages:
  - deploy

deploy:
  stage: deploy
  image: alpine:3.20
  script:
    - ./scripts/deploy.sh
```

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `group` | string | — | The group this pipeline joins. **Required** when the block is present. |
| `cancel_in_progress` | bool | `false` | Cancel the running pipelines of the group instead of refusing the new one. |

- `group` may use `${{ ref }}` (the full ref) and `${{ branch }}` (the short
  branch name). The Actions spellings `${{ github.ref }}`,
  `${{ github.ref_name }}`, `${{ github.workflow }}`, `${{ github.sha }}`,
  `${{ github.event_name }}`, `${{ github.repository }}` and
  `${{ github.repository_owner }}` are understood as well.
- A group that still contains an unresolved `${{ … }}` after expansion is
  **refused**. Left alone it would be one literal group shared by every ref of
  the repository, which under `cancel_in_progress` is a standing order to cancel
  whatever else is running.
- With `cancel_in_progress: false`, triggering while the group is busy is
  answered with a conflict naming the group and the number of active pipelines —
  wait for them, or set the flag.

## When the file is wrong

Every rule above is checked when the pipeline is triggered, and every refusal is
reported as a client error naming the job and the rule — `push` output, the API
response and the UI all show the same text. A configuration that parses but
declares something unrunnable never produces a half-built pipeline: the whole
graph is written in one transaction, or nothing is.

That includes a job whose `stage:` is missing from `stages:`, which is refused
by name rather than dropped from the graph — see the note under
[Top level](#top-level).
