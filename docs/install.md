# Install from source

[← README](../README.md)

To only try Plombir Git, the [one-command trial](../README.md#try-it-in-one-command) needs none of this. For a production deployment with Docker Compose, see [deploy/README.md](../deploy/README.md).

Tagged builds are available under [GitHub Releases](https://github.com/Nettechnologys/plombir-git/releases).
Each release includes Linux x86_64 and aarch64 archives, SHA-256 checksums,
SPDX SBOMs, a multi-architecture image at
`ghcr.io/nettechnologys/plombir-git:v0.3.0` (substitute the desired tag), and
from v0.2.0 on a `plombir-git-deploy-<tag>.tar.gz` with a
compose file pinned to that image's digest
([how to run it](../deploy/README.md#-published-release--no-source-tree)).
This page and `deploy/README.md` describe `main`: for a release, read them at
its tag (`https://github.com/Nettechnologys/plombir-git/blob/<tag>/README.md`).
What [CHANGELOG.md](../CHANGELOG.md) lists under *Unreleased* is in no release
yet. Read the [v0.3.0 release notes](releases/v0.3.0.md) before upgrading
a v0.2.0 instance, and the [v0.2.0 notes](releases/v0.2.0.md) first for a
v0.1.1 one.
Run an unpacked binary from its archive directory so its bundled `web/build`
is found by the default configuration.
`plombir-git --version`, `plombir-git-runner --version` and
`plombir-git-mcp --version` identify the build. Pin a deployment to the image
digest shown by `docker image inspect`, and follow the [migration and rollback
procedure](../deploy/README.md#rolling-back-across-a-migration) before replacing a
running server.

### Requirements

- Rust 1.89+ — the oldest compiler the crates build with, checked in CI.
  The repository's `rust-toolchain.toml` pins the toolchain the gates run
  (1.99.1), and rustup installs it on the first `cargo` call in this tree.
- `git` 2.38 or newer on `PATH`. A few pack / diff operations are still
  delegated to the CLI, and outbound clones (imports, pull mirrors) pin the
  remote's checked address through `http.curloptResolve`, which git before
  2.38 silently ignores — the server refuses to start on an older git rather
  than clone through an unpinned resolver. Debian 12, Ubuntu 24.04 and current
  macOS ship a new enough git; Ubuntu 22.04 (2.34) needs the `git-core` PPA.
- Linux or macOS
- Node.js and npm (to build the web UI), plus `curl` and `jq` for the API examples

### Build

```bash
git clone https://github.com/Nettechnologys/plombir-git.git
cd plombir-git
(cd web && npm ci && npm run build)
cargo build --release -j 6
```

The web bundle is `web/build/`; the server serves it from `[server].web_root`
(default: `web/build` relative to its working directory). Run the commands
below from the repository root, or set `web_root` to the bundle's absolute
path in your config file. The main binary is `target/release/plombir-git`.
The workspace also produces
`plombir-git-runner` (standalone CI runner) and `plombir-git-mcp` (MCP server).

### Generate an SSH host key

The server needs an SSH host key on first run:

```bash
ssh-keygen -t ed25519 -f ./plombir_git_host_key -N ""
```

### Run the server

Create the config **once** and keep it with the database and host key. Running
`gen-secret` on every start would invalidate existing sessions. The at-rest
encryption key is a different key that the server creates beside the host key
on first start; back it up with the database.

```bash
umask 077
printf '[server]\nweb_root = "./web/build"\n\n[auth]\njwt_secret = "%s"\n' \
  "$(./target/release/plombir-git gen-secret)" > plombir-git.toml
```

In a second terminal, from the repository root:

```bash
./target/release/plombir-git serve \
  --repo-root ./repos \
  --http-addr 127.0.0.1:8080 \
  --ssh-addr  127.0.0.1:2222 \
  --host-key  ./plombir_git_host_key \
  --db-url    "sqlite://./plombir-git.db?mode=rwc" \
  --config    ./plombir-git.toml
```

`gen-secret` prints a fresh 256-bit secret (the `openssl rand -base64 32`
equivalent). The server refuses the shipped `change-me-in-production`
placeholder. Keep `plombir-git.toml` out of version control. Database
migrations run automatically on startup. Set the log level with `RUST_LOG`
(e.g. `RUST_LOG=debug`). These loopback listeners are for a local trial; see
[the deployment guide](../deploy/README.md) before exposing an instance.

`[server].web_root` is config-only; use it when the built UI is outside the
server's working directory.

Common `serve` flags:

| Flag | Description | Default |
|------|-------------|---------|
| `--repo-root` | Root directory for bare repositories | `./repos` |
| `--http-addr` | HTTP listen address | `0.0.0.0:8080` |
| `--ssh-addr` | SSH listen address | `0.0.0.0:2222` |
| `--host-key` | SSH host key path | — |
| `--db-url` | `sqlite://` / `postgres://` / `mysql://` URL | `sqlite://./plombir-git.db?mode=rwc` |
| `--jwt-secret` | JWT signing key (use a long random value) | — |
| `--encryption-key` | Key for data at rest — see [Secrets and rotation](#secrets-and-rotation) | `[auth].key_file` |
| `--config` | TOML config file; a flag you pass wins over its config key | — |
| `--tls-cert` / `--tls-key` | PEM cert/key to enable HTTPS | — |
| `--docker` | Run CI jobs with an `image` in Docker | `false` |
| `--external-runners` | Use external runners instead of the embedded one | `false` |
| `--rate-limit-max` / `--rate-limit-window` | Rate limit (0 = disabled) | `0` / `60` |
| `--smtp-host` … `--smtp-from` | SMTP settings for email notifications | — |
| `--log-file` / `--log-max-files` | Enable daily-rotated file logs, keeping this many | — / `5` |
| `--log-format` | `text`, or `json` for a log shipper | `text` |

Prefer a config file? Create it with
`install -m 600 plombir-git.example.toml plombir-git.toml`, edit it, and pass
`--config plombir-git.toml`. Plombir Git refuses group- or world-readable config
files because they can carry signing keys, database credentials and service
tokens. Every flag in the table above has a config-file equivalent (named in
`plombir-git serve --help`), and values resolve as **CLI arg > config file >
built-in default** — so a config-only deployment needs no flags at all.

### Passkeys need one canonical public URL

Set `[server].external_url` before anyone registers a passkey:

```toml
[server]
external_url = "https://git.example.com"
```

WebAuthn credentials are bound to a relying-party hostname. Without this
setting Plombir Git has to use each request's `Host`, so the same person opening
the instance through another proxy name will not see the credential in their
browser. Credentials registered after this setting is present retain that RP
id and are deliberately excluded from challenges at a different host. Existing
credentials from before RP tracking remain compatible where possible; the
startup warning names them so they can be re-enrolled at the canonical URL.

### Secrets and rotation

Plombir Git holds two configured secrets and one stored key. They do different
jobs, and telling them apart is what makes rotation safe.

| Secret | Sources (first wins) | Protects |
|--------|----------------------|----------|
| **JWT secret** | `PLOMBIR_GIT_JWT_SECRET` › `--jwt-secret` › `[auth].jwt_secret` | Signatures: session tokens, PAT-derived tokens, CI job tokens |
| **Encryption key** | `PLOMBIR_GIT_ENCRYPTION_KEY` › `--encryption-key` › `[auth].encryption_key` › `[auth].key_file` | Data at rest: TOTP secrets, CI secrets, mirror and LDAP passwords, SSO client secrets, OAuth tokens, the instance signing key below |
| **Instance signing key** | Stored in the database, established on first start | This instance's public identity: release-asset provenance attestations and the CI OIDC JWKS |

**The encryption key establishes itself.** With no explicit key source, first
start generates `[auth].key_file` beside `[server].host_key` with mode `0600`
(the container configuration therefore uses `/data/encryption_key`). The file
belongs in the same backup as the database and is independent of the JWT
signing secret. Set an explicit key only when a KMS or vault is the intended
source:

```toml
[auth]
jwt_secret     = "..."   # rotate this freely
# encryption_key = "..." # optional external key source
# key_file = "/data/encryption_key"
```

**Rotating the JWT secret** (a leak, or routine hygiene): change `jwt_secret`
and restart. Existing sessions are invalidated — everyone logs in again — while
the durable at-rest key file remains unchanged.

**The instance signing key is not a config value.** It signs release-asset
attestations and backs the public keys at `/api/v1/ci/oidc/jwks`, so it has to
outlive the secrets you rotate: a key that changed with `jwt_secret` would make
every attestation this server ever issued fail verification — against the very
server that signed it — and change the `kid` under external verifiers that
already fetched it. The server establishes the key on first start (adopting the
one the old derivation produced, so nothing signed earlier is invalidated by
upgrading), seals it with the encryption key, and keeps it in the database.

Replace it only if the key itself is compromised, and knowing that every
attestation signed with it stops verifying for good:

```bash
./target/release/plombir-git rotate-instance-key --config plombir-git.toml --yes
```

**Rotating the encryption key** is a different operation — the stored
ciphertext has to be re-encrypted — so it has its own command. It refuses to
start while a Plombir Git server holds a file-backed SQLite database, for two
reasons that both point the same way: a handler writing an encrypted column
mid-pass would leave a value under the old key, and the pass holds the single
write lock for its whole duration. Stop the server first, and look before you
leap:

```bash
./target/release/plombir-git rotate-encryption-key --config plombir-git.toml \
    --old "<the current key>" --new "$(./target/release/plombir-git gen-secret)" --dry-run
```

The dry run reports, per column, how many stored values the old key opens and
how many it does not, and writes nothing. Re-run it with `--yes` instead of
`--dry-run` to apply: every value is re-sealed in one transaction, and then
the configured source has to carry the new secret before the server is started
again: update `[auth].encryption_key` / `PLOMBIR_GIT_ENCRYPTION_KEY`, or replace
`[auth].key_file` with mode `0600`.

`--old` defaults to the key this deployment already resolves, so you only need
it when the current key is not what the config says — on an instance that
rotated `jwt_secret` without ever setting `encryption_key`, the key that opens
the data is the *previous* signing secret. If the old key opens nothing at all,
the command refuses and changes nothing rather than sealing the database away.
Values it cannot open — legacy plaintext, or rows damaged earlier — are counted
and reported, never rewritten and never deleted. Keep the old secret until the
server has started under the new one.

On startup the server samples the encrypted columns and checks the configured
key opens them. If it opens none of them, it **refuses to start** and prints
the recovery, instead of starting and then failing MFA logins, CI jobs, mirror
syncs and LDAP binds one at a time with unrelated-looking 500s. A brand-new
database has nothing to check, so a fresh install is never blocked.

If the old secret is genuinely lost, no tool can recover the encrypted values:
clear them and have MFA re-enrolled and the stored credentials re-entered. That
includes the instance signing key — `rotate-instance-key` mints a new identity,
and attestations signed under the old one stay unverifiable.

### Create a repository and clone it

Back in the first terminal, register a local test user, create a repository
through the API (which records it in the database), and clone it. The password
is generated once in this shell; save it if you intend to log in again.

The first account on an empty instance becomes its administrator, so it has to
present the one-time setup token the server wrote beside its keys on first
start (`./setup_token` here, also printed in the server log); the server
deletes the file once that account exists. The `create-admin` subcommand
creates the same account from the shell instead.

```bash
BASE_URL="${BASE_URL:-http://127.0.0.1:8080}"
PASSWORD="$(./target/release/plombir-git gen-secret)Aa1!"
TOKEN="$(curl -fsS -X POST "$BASE_URL/api/v1/users/register" \
  -H 'Content-Type: application/json' \
  -H "X-Setup-Token: $(cat ./setup_token)" \
  -d "$(jq -n --arg username testuser --arg email test@example.com \
    --arg password "$PASSWORD" \
    '{username:$username,email:$email,password:$password}')" | jq -er '.token')"
curl -fsS -X POST "$BASE_URL/api/v1/repos" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name":"testrepo","auto_init":true}' > /dev/null
git clone "$BASE_URL/git/testuser/testrepo" "${QUICKSTART_CLONE_DIR:-/tmp/plombir-git-testrepo}"
```
