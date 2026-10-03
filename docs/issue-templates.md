# Issue and pull-request templates

This is the reference for the template files you commit to **your own**
repository so that Plombir Git pre-fills the new-issue and new-pull-request
forms. The format is the Gitea/GitHub Markdown one: a `.md` file whose optional
YAML front matter names the template and the labels it applies.

**Where the files go** is the awkward part, and it is why this page exists:
Plombir Git reads eight different template directories, eight different chooser
configuration paths and six different pull-request template paths, and nothing
in the UI tells you which. The complete inventories are below — copy a path
from them rather than guessing.

**Unknown keys are refused, not ignored.** The front matter block, the chooser
configuration and each contact link carry a closed schema. A misspelled key
does not quietly do nothing: the template is dropped from the chooser and the
reason is recorded against the file. The failure is quiet on your side — the
API still answers `200` with the templates that did parse — so a typo can sit
in your repository for weeks looking like a template that "just doesn't show
up". The names below are the whole vocabulary.

**Which commit is read.** Templates are read from the tip of the repository's
default branch, out of the commit itself rather than a checkout. A repository
whose default branch does not exist yet has no templates, no chooser
configuration and no pull-request template — not an error.

## Issue templates

### A complete example

Every front-matter key the format accepts, in one file
(`.gitea/ISSUE_TEMPLATE/bug.md`):

```markdown
---
name: Bug report
title: '[Bug] '
about: Something in Plombir Git behaves differently than documented
labels: bug, triage
assignees: [alice, bob]
ref: main
---

## What happened

## What you expected instead

## Steps to reproduce

1.
2.
3.
```

Everything after the closing `---` is the issue body the reporter starts from.
Everything before it is metadata and is never shown in the issue.

### Front-matter keys

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `name` | string | the file name | The template's entry in the chooser. |
| `title` | string | empty | Text pre-filled into the issue title field. |
| `about` | string | see below | One-line description under the name in the chooser. |
| `description` | string | empty | Accepted as a synonym of `about`, for templates written for GitHub. `about` wins when both are present. |
| `labels` | string or list of strings | none | Labels applied to the new issue. A plain string is split on commas. |
| `assignees` | string or list of strings | none | Users assigned to the new issue. Same two spellings as `labels`. |
| `ref` | string | repository default | Branch the issue is filed against. |

`about` falls back through `description` and then to the first 80 characters of
the template body (with an ellipsis when it was cut). So a template with no
front matter at all is still usable: it is listed under its file name with an
excerpt of its own body as the description, and a template imported from a
GitHub repository keeps working unchanged:

```markdown
---
name: Question
description: Ask about usage, configuration or a surprising behaviour
labels:
  - question
---

## What are you trying to do?
```

An entry of `labels` or `assignees` that is not a string — a number, a nested
map — is an error, not a value silently converted. So is any key not in the
table above.

### The front-matter delimiter

The block is opened by a line of **three or more dashes and nothing else**, on
the very first line of the file, and closed by another such line. Two
consequences worth knowing:

- A file that opens the block and never closes it is not an error. It is read
  as a template with **no** metadata whose body happens to start with dashes —
  which is exactly how it will look in the chooser: named after the file, with
  none of the labels you wrote.
- A blank line or a byte-order mark before the opening dashes has the same
  effect. The dashes must be first.

### Where issue templates are read from

Every `.md` file directly inside any of these directories is a template. All
eight directories are read, not just the first one that exists, so a repository
carrying both a `.github` and a `.gitea` directory offers both sets.

<!-- inventory: issue-template-directories -->
```text
ISSUE_TEMPLATE
issue_template
.gitea/ISSUE_TEMPLATE
.gitea/issue_template
.github/ISSUE_TEMPLATE
.github/issue_template
.gitlab/ISSUE_TEMPLATE
.gitlab/issue_template
```

Three rules apply to the files inside:

- The name must end in `.md` (any capitalization). A `.yml` form template —
  GitHub's issue *forms* — is not read; that format is not supported.
- Only files directly in the directory count. A template in a subdirectory is
  ignored.
- A file larger than 1048576 bytes (1 MiB), or one that is not valid UTF-8, is
  reported as a broken template rather than served.

Within a directory the templates are listed in file-name order.

## The chooser configuration

`config.yml` sits beside the templates and controls the chooser page itself:
whether a reporter may skip the templates entirely, and which external links to
offer instead.

```yaml
blank_issues_enabled: false
contact_links:
  - name: Security disclosure
    url: https://example.com/security
    about: Report a vulnerability privately instead of opening an issue
  - name: Community chat
    url: https://chat.example.com/plombir-git
    about: Questions and usage help
```

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `blank_issues_enabled` | bool | `true` | Whether the chooser offers "open a blank issue" alongside the templates. |
| `contact_links` | list of blocks | empty | External destinations shown under the templates. |

Each entry of `contact_links` takes exactly three keys, **all required**:

| Key | Type | Meaning |
|-----|------|---------|
| `name` | string | The link's label. Must not be blank. |
| `url` | string | Absolute `http://` or `https://` URL, with a host. A relative path or a `mailto:` link is refused. |
| `about` | string | One-line description under the label. Must not be blank. |

### Where the configuration is read from

The **first** of these paths that exists is the configuration; the rest are not
looked at. Note that this list is shorter than the template-directory list
above: a `config.yml` in a bare `ISSUE_TEMPLATE/` directory, or under
`.gitlab/`, is not read.

<!-- inventory: issue-config-paths -->
```text
.gitea/ISSUE_TEMPLATE/config.yaml
.gitea/ISSUE_TEMPLATE/config.yml
.gitea/issue_template/config.yaml
.gitea/issue_template/config.yml
.github/ISSUE_TEMPLATE/config.yaml
.github/ISSUE_TEMPLATE/config.yml
.github/issue_template/config.yaml
.github/issue_template/config.yml
```

A configuration that fails to parse or fails validation is an error for the
whole repository, not a file that is skipped — the chooser reports it instead
of silently falling back to the defaults. `GET
/api/v1/repos/{owner}/{name}/issue_config/validate` exists to tell you which
line is wrong before you rely on the file.

## Pull-request templates

A pull-request template is plain Markdown with **no** front matter: the whole
file, including any leading dashes, becomes the initial description of a new
pull request. There is one template per repository — the first of these paths
that exists wins.

<!-- inventory: pull-request-templates -->
```text
PULL_REQUEST_TEMPLATE.md
pull_request_template.md
.gitea/PULL_REQUEST_TEMPLATE.md
.gitea/pull_request_template.md
.github/PULL_REQUEST_TEMPLATE.md
.github/pull_request_template.md
```

The same 1048576-byte (1 MiB) ceiling and UTF-8 requirement apply.

## When a template does not appear

Because a broken template is dropped rather than raised, checking is worth a
minute. The templates the server actually parsed are what these endpoints
return:

| Endpoint | Answers |
|----------|---------|
| `GET /api/v1/repos/{owner}/{name}/issue_templates` | The templates that parsed, each with its resolved `name`, `title`, `about`, `labels`, `assignees`, `ref`, `content` and the `file_name` it came from. |
| `GET /api/v1/repos/{owner}/{name}/issue_config` | The effective chooser configuration. |
| `GET /api/v1/repos/{owner}/{name}/issue_config/validate` | Whether the configuration is usable, and why not when it is not. |
| `GET /api/v1/repos/{owner}/{name}/pull_request_template` | The pull-request template, or nothing when no path matched. |

A template missing from the first response was dropped, and the reason is in
the server log against its path — search for `ignored invalid issue template`.
The usual causes are a misspelled front-matter key, a `labels:` value that is
not a string or a list of strings, and an unterminated front-matter block.

The `file_name` in the response is the path the template was found at, while a
template with no `name:` is named after its file's last segment — so
`.github/ISSUE_TEMPLATE/bug.md` with no front matter is listed as `bug.md`.
