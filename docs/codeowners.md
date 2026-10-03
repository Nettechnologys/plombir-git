# CODEOWNERS — automatic reviewer requests

This is the reference for the `CODEOWNERS` file you commit to **your own**
repository so that ForgeKeep requests reviewers on a new pull request by
itself. The format is the GitHub/Gitea one — a pattern followed by the owners
of everything it matches — but the matcher is ForgeKeep's own, so the rules
below are the ones that decide, not the ones you remember from elsewhere.

**Nothing in the product mentions this file**, which is why this page exists:
there is no settings screen for it, no editor that offers to create it, and no
message on a pull request saying that a rule matched or failed to. Copy a path
and a pattern from here rather than guessing.

**It requests, it does not require.** A CODEOWNERS rule adds people to the
pull request's reviewer list. It does not block the merge: branch protection
has no "require review from code owners" setting, so an owner who never
reviews holds nothing up.

**Whose approval counts.** When branch protection requires approvals, an
approval counts toward that number only if it comes from a person with write
access to the repository, checked at merge time, who is neither the author nor
— for a pull request a bot opened — the bot's owner. Approvals from bots, and
from collaborators who may only read, are shown on the pull request but do not
count. So an owner named here needs write access for their approval to
satisfy the rule.

**When it is read.** Once, at the moment the pull request is **created**,
from the tip of the pull request's **base branch** — out of the commit
itself, not a checkout. Three consequences worth knowing:

- Pushing more commits to an open pull request does not re-run the matching.
  A file that comes under a new rule after the pull request was opened brings
  no new reviewer with it.
- For a pull request from a fork, the file that counts is the one in the
  **base** repository. Your fork's copy is never read.
- Editing `CODEOWNERS` affects pull requests opened afterwards, and only
  those.

**Failure does not block pull-request creation.** The feature is advisory, but
an unexecutable declaration is not silent: the server logs its CODEOWNERS line,
the declaration and the reason against the pull request id. Search for
`CODEOWNERS declaration ignored`. Whole-file/diff failures use
`CODEOWNERS reviewer request failed` and `CODEOWNERS diff unavailable`.

## Where the file goes

The **first** of these paths that exists on the base branch is the policy; the
rest are not looked at. There is no merging of two files.

<!-- inventory: codeowners-paths -->
```text
.github/CODEOWNERS
CODEOWNERS
docs/CODEOWNERS
```

**There is a size limit.** The file is matched line by line, so the whole of it
has to be held in memory at once, and a file over the limit is refused before a
byte of it is read — no rule in it is applied, the pull request gets no
reviewers from it, and the only trace is a `CODEOWNERS reviewer request failed`
line in the server log. The limit, in bytes:

<!-- inventory: codeowners-size-ceiling -->
```text
1048576
```

## The format

One rule per line: a path pattern, then the owners it assigns, separated by
whitespace.

```text
# A comment. Everything from an unescaped `#` to the end of the line is
# discarded, including a `#` in the middle of a rule.
/docs/   @tech-writers @acme/docs-team   # trailing note, ignored
```

Four ways a line produces **no executable rule**:

- It is blank, or a comment. This is intentional and produces no diagnostic.
- It names no owner (`*` on its own). ForgeKeep logs the line and reason.
- None of its owners starts with `@`. An email address — which GitHub
  accepts — is not an owner here, and neither is a bare `alice`. A line whose
  owners are all unusable does not become an ownerless rule that wins; it
  disappears, the previous matching rule takes the path instead, and every
  unsupported owner is logged with the line number.
- Its pattern ends with a backslash that has nothing left to escape — see
  below, it is what trying to escape a space leaves behind. This is logged too.

### Backslashes

A backslash escapes the character after it: that character is then matched
literally and is neither a comment marker nor a wildcard. So `\#notes` is a
rule for the file named `#notes`, and `a\*b` is a rule for the file named
`a*b` and for no other. To write a literal backslash, double it — and after
an even run of them a `#` opens a comment again, so `docs\\#mine @alice` is
the pattern `docs\` with the rest of the line thrown away, and therefore no
rule.

A **space** is the one character this cannot reach. The pattern ends at the
first whitespace on the line, before anything reads the backslash, so
`docs\ dir/*.rs @alice` leaves a pattern holding a trailing `\` with nothing
to escape — and that line is dropped rather than turned into a rule nobody
wrote. A path with a space in it cannot be expressed here.

### Matching

The last rule that matches a path wins, so write the general rules first and
the specific ones after. Owners are collected across all the changed paths of
the pull request, in the order the paths were matched, and each owner is
requested once.

| Rule | What it means |
|------|---------------|
| A pattern with **no `/`** at all | Tried against each segment of the path on its own, so it matches at any depth. |
| A **leading `/`** | Anchors the pattern to the repository root; it is then matched against the whole path. |
| A `/` **anywhere inside** | Matched against the whole path, anchored or not. |
| A **trailing `/`** | Means "this directory and everything under it" — `**` is appended. |
| `*` | Any run of characters, but never across a `/`. |
| `**` | Any run of characters, `/` included. |
| `?` | Exactly one character, never a `/`. |
| `\` | The character after it is a literal — not a wildcard, not a comment marker. |

Worked out on real paths:

<!-- examples: pattern-matching -->
| Pattern | Path | Matches |
|---------|------|---------|
| `*` | `README.md` | yes |
| `docs` | `a/docs/b.md` | yes |
| `/docs` | `a/docs/b.md` | no |
| `/docs/` | `docs/guide/setup.md` | yes |
| `*.rs` | `src/api/pulls.rs` | yes |
| `src/*.rs` | `src/api/pulls.rs` | no |
| `src/**/*.rs` | `src/api/v1/pulls.rs` | yes |
| `/README.md` | `README.md` | yes |
| `/README.md` | `docs/README.md` | no |
| `src/test?.rs` | `src/test1.rs` | yes |
| `\#notes` | `#notes` | yes |
| `\#notes` | `notes` | no |
| `a\*b` | `a*b` | yes |
| `a\*b` | `axb` | no |

Two of those rows are the ones that catch people out: `src/*.rs` does **not**
reach `src/api/pulls.rs`, because `*` stops at a slash, and `/docs` does not
reach a `docs` directory that is not at the root.

## Who the owners can be

| Form | Meaning |
|------|---------|
| `@username` | A single account on this instance. |
| `@org/team` | Every member of a team, subject to the two conditions below. |

Write the username exactly as it is registered; whether a differently-cased
spelling also resolves depends on the database backend, so do not rely on it.
A nested name (`@org/team/subteam`) is not a form and is logged as unsupported.

A team owner is honoured only when **both** hold:

- The organization is the one that owns **this** repository — a team from
  anywhere else is rejected with a diagnostic, as is every team owner in a
  repository owned by a person rather than an organization. The comparison of
  the organization name ignores case.
- The team's permission on the repository is one of the levels below, and
  only those. A team on any other level is not an owner and is diagnosed.

<!-- inventory: team-owner-permissions -->
```text
write
admin
```

### Who is skipped after a rule matched

Even a matched, resolvable owner is not always added. These are dropped
without a trace:

- The pull request's own author.
- A deactivated or deleted account.
- Anyone who cannot read the repository — a team member whose own access was
  removed, for instance.
- Anyone already on the pull request's reviewer list.

Each reviewer that *is* added is recorded on the pull request timeline as a
`reviewer_requested` event with `"source": "codeowners"`, which is the one
place you can confirm after the fact that a rule fired.

## A complete example

<!-- example: codeowners-file -->
```text
# The fallback: everything, unless a later rule claims it.
*                                 @maintainers

# The docs tree at the repository root, and everything under it.
/docs/                            @tech-writers @acme/docs-team

# Every Rust file, at any depth.
*.rs                              @rustaceans

# One exact file, more specific than the rule above because it comes later.
/crates/rg-http/src/api/pulls.rs  @alice
```

Given that file, a pull request touching these paths requests these people:

<!-- examples: codeowners-resolution -->
| Changed path | Owners requested |
|--------------|------------------|
| `README.md` | `@maintainers` |
| `docs/guide/setup.md` | `@tech-writers`, `@acme/docs-team` |
| `crates/rg-core/src/review/codeowners.rs` | `@rustaceans` |
| `crates/rg-http/src/api/pulls.rs` | `@alice` |

`docs/guide/setup.md` goes to the writers rather than to `@maintainers`
because `/docs/` is the *last* rule that matches it, and
`crates/rg-http/src/api/pulls.rs` goes to `@alice` alone for the same reason —
being matched by `*` and `*.rs` as well changes nothing.
