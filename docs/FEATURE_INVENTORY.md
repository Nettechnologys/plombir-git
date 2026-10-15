# Plombir Git — Feature Inventory

This is a maintained map of implemented features and known gaps, focused on
security boundaries. It is not a substitute for source or runtime checks.
Last partial source review: 2026-10-11 (agent features and README claims).

Legend: ✅ implemented · 🟡 partial · ❌ absent · 🔒 security relevant.
Paths below are relative to `crates/` unless stated otherwise.

## Git transport

| Feature | State | Implementation and limit |
|---|---|---|
| Smart Protocol V1 and V2 | ✅ | `rg-git/src/protocol/`: pkt-line, sideband, pack handling |
| HTTPS and SSH | ✅ 🔒 | `rg-http/src/lib.rs`, `rg-ssh/`; SSH public-key authentication |
| Git request body limits | ✅ 🔒 | `rg-http/src/routes.rs` |
| Git subprocess timeout and termination | ✅ 🔒 | `rg-git/src/cli_gateway.rs`; default 120 seconds |
| Stream wall-clock limit | ✅ 🔒 | `rg-http/src/git_http.rs`, `rg-ssh/src/lib.rs`; `[timeouts].git_stream_secs`, default 300, 0 disables |
| Idle stream limit | ✅ 🔒 | `rg-git/src/io_timeout.rs`, HTTP and SSH call sites; `[timeouts].git_idle_secs`, default 30, 0 disables |
| Outgoing pack memory bound | 🟡 🔒 | Upload bodies are spooled with limits; generated download packs can still be assembled in memory |

## Authentication and authorization

| Feature | State | Implementation and limit |
|---|---|---|
| Registration and login | ✅ 🔒 | `rg-core/src/auth/`, `rg-core/src/user/service.rs`; Argon2 and JWT |
| Scoped personal access tokens | ✅ 🔒 | `rg-http/src/pat_auth.rs`, `pat_scope.rs`; stored as SHA-256 hashes |
| SSH keys, MFA/TOTP, SSO/LDAP/OAuth | ✅ 🔒 | `rg-ssh/`, `rg-core/src/auth/` |
| CI OIDC and JWKS | ✅ 🔒 | `rg-core/src/auth/ci_oidc.rs`; short-lived Ed25519 JWTs |
| Sanitized internal errors | ✅ 🔒 | `rg-http/src/error.rs`; generic client messages |
| Invite, approval, or CAPTCHA for registration | ❌ 🔒 | Registration is open; auth routes have an always-on per-route rate-limit (10/60s) instead |

## Abuse limits

| Feature | State | Implementation and limit |
|---|---|---|
| Global per-IP rate limit | ✅ 🔒 | `rg-http/src/rate_limit.rs`; disabled by default when `max=0` |
| Trusted proxy address resolution | ✅ 🔒 | `rg-http/src/rate_limit.rs`; allowlisted proxy addresses |
| Bounded client-key map | ✅ 🔒 | `[rate_limit].max_keys`, default 100000; a new key is rejected before insertion when full |
| Auth route limits and reset-mail cooldown | ✅ 🔒 | `rg-http/src/routes.rs`; always-on by default (10/60s) for sensitive routes |
| Agent account limit | ✅ 🔒 | `rg-http/src/pat_auth.rs`; default 600 requests per 60 seconds |
| Dedicated Git push limit | ❌ 🔒 | Pushes use the global limit only |
| CAPTCHA or proof of work | ❌ | No external CAPTCHA service is required |

## Outbound network calls

| Feature | State | Implementation and limit |
|---|---|---|
| Outbound webhooks | ✅ 🔒 | `rg-core/src/webhook/`; HTTPS by default; HTTP requires explicit opt-in |
| Mirror transport policy | ✅ 🔒 | `rg-core/src/mirror/`; HTTPS by default, HTTP opt-in, native `git://` rejected |
| Webhook HMAC signing and inbound verification | ✅ 🔒 | `X-Hub-Signature-256`; inbound check requires a configured external secret |
| HTTP timeouts and redirect refusal | ✅ 🔒 | `rg-core/src/net.rs`; 30-second request and 10-second connect timeouts |
| Webhook DNS-rebinding defense | ✅ 🔒 | Checked addresses are passed to the connector; proxy use and redirects are disabled |
| Mirror SSRF defense | ✅ 🔒 | URL checks at configuration and execution, plus checked address selection |
| Push mirrors | ✅ 🔒 | `rg-core/src/mirror/push.rs`; pull-mirror transport rules, background push after every ref move, fast-forward only unless `force` |

## Runtime robustness

| Feature | State | Implementation and limit |
|---|---|---|
| Graceful HTTP/SSH shutdown | ✅ | `rg-cli/src/serve.rs`, `rg-http/src/lib.rs`; configurable grace period |
| Worker and detached-delivery drain | ✅ | Shutdown channel and `rg-core/src/task_tracker.rs` |
| Database unavailable vs internal error | ✅ 🔒 | `rg-http/src/error.rs`; connection errors use 503, statement errors 500 |
| Git timeout response | ✅ 🔒 | `GIT_TIMEOUT` returns 504 without leaking command details |
| Numeric configuration range checks | 🟡 | Some values still accept zero or extreme input; check each setting |
| Prometheus metrics | ✅ | `rg-http/src/metrics.rs`; served on the main app port |
| Private metrics listener | ❌ | Restrict `/metrics` at the proxy or configure a metrics token |

## Storage and artifacts

| Feature | State | Implementation and limit |
|---|---|---|
| Atomic local blob storage | ✅ | `rg-core/src/blob_storage.rs` |
| OCI content-addressed packages | ✅ 🔒 | `rg-core/src/package_registry/oci/` |
| Release asset SHA-256 | ✅ 🔒 | `rg-core/src/release/service.rs`; legacy assets without a digest are separate |
| Release attestations | ✅ 🔒 | Optional DSSE envelope, Ed25519 signing, and verification API |

## Agent and MCP support

| Feature | State | Implementation and limit |
|---|---|---|
| MCP stdio and HTTP tools | ✅ 🔒 | `rg-mcp/`, `rg-http/src/api/mcp.rs`; REST authorization applies |
| Repository read and write tools | ✅ 🔒 | `rg-mcp/src/tools/mod.rs`; writes use REST authorization |
| CI tools and search | ✅ | `rg-mcp/src/tools/mod.rs`; code search needs an index |
| AI tree endpoint | 🟡 | MCP wrapper exists; HTTP `repo_tree` currently returns 501 |
| Bot accounts and scoped tokens | ✅ 🔒 | `rg-core/src/user/bots.rs`, `rg-http/src/agent_scope.rs` |
| Agent activity audit | ✅ 🔒 | `rg-core/src/audit/audit.rs` |
| Human approval for bot-authored PRs | ✅ 🔒 | `rg-core/src/branch_protection/service.rs` |

## Deliberately out of scope

Decentralized identity, UCAN proof chains, peer-to-peer replication, and
wallet-backed bounties are not part of this centralized forge. The project
uses accounts, scoped tokens, and repository permissions instead.

When changing a feature, update its row in the same PR. Verify the behavior
against current code and tests before promoting a partial row to implemented.
