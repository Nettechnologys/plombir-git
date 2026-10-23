# The `plombir-git` command line

[← README](../README.md)

## CLI

Beyond `serve`, the `plombir-git` binary offers:

| Command | Purpose |
|---------|---------|
| `serve` | Start the server (HTTP + SSH) |
| `trial` | Start a throwaway instance with nothing to prepare: secrets, keys, a SQLite database and the config are created in `--data-dir`, the first account comes from the printed setup link, and every page says it is a trial. Not for production |
| `migrate` | Run database migrations and exit (file-backed SQLite requires the server to be stopped; this is enforced) |
| `rebuild-fts` | Rebuild full-text search indexes (file-backed SQLite requires the server to be stopped; this is enforced) |
| `gen-secret` | Print a fresh 256-bit secret for `[auth].jwt_secret` or an encryption key |
| `backup-db` / `restore-db` | Create / restore a consistent SQLite backup by hand (for a schedule, use `[backup]` — the server snapshots itself) |
| `rotate-encryption-key` | Re-encrypt every at-rest secret onto a new encryption key (file-backed SQLite requires the server to be stopped, `--dry-run` included; this is enforced) |
| `rotate-instance-key` | Mint a new provenance signing identity (invalidates past attestations) |
| `create-admin --username <name> --email <address>` | Create the first instance administrator from the host (no setup token needed), or promote an existing account; the password is prompted for, or read with `--password-stdin` |
| `create-repo` | Create a bare repository (no DB record — quick testing) |
| `runner` | Run as a CI runner (polls and executes jobs) |
| `import github\|gitlab <url>` | Import a repository (and metadata) from GitHub/GitLab (file-backed SQLite requires the server to be stopped) |
| `index-repo <owner/name>` | Index a repository for code search |
| `package` | Manage the package registry (`package list` requires a stopped server on file-backed SQLite) |
| `list-tombstones` | Report the bytes interrupted deletions left in the storage root (reports only — moves, removes and creates nothing) |
| `reset-mfa <username>` | Take every second factor (authenticator, backup codes, passkeys) off an account whose owner is locked out, and sign its sessions out — for when no other administrator can use `POST /admin/users/{id}/mfa/reset` |

`create-repo` creates only a bare Git directory, with **no database row**. Use
it for filesystem-level testing, not to create a repository users can clone
from a running instance. For the latter, use the API or web UI as in the
[quick start](install.md#create-a-repository-and-clone-it).

```bash
./target/release/plombir-git create-repo scratch fixture --repo-root ./repos
```

Every subcommand that touches the database or the repository directory
(`migrate`, `rebuild-fts`, `backup-db`, `restore-db`, `rotate-instance-key`,
`rotate-encryption-key`, `create-admin`, `create-repo`, `import`, `index-repo`,
`list-tombstones`, `reset-mfa`, `package list`)
takes the same `--config` as `serve` and resolves `--db-url` / `--repo-root` as
**CLI arg > config file > built-in default**. On a config-file deployment, pass
`--config` rather than repeating the URL: with neither,
they fall back to `sqlite://./plombir-git.db?mode=rwc` in the working directory,
so `migrate` would migrate an empty database and `backup-db` would back it up.

For a file-backed SQLite deployment, two groups of commands are deliberately
offline-only, for two different reasons, and both are enforced rather than
documented and hoped for.

Every CLI path that can apply pending migrations (`migrate`, `import`, and
`package list`) is offline-only because another process caches the schema it is
about to change. Every whole-database maintenance pass (`rebuild-fts`,
`rotate-encryption-key`, `--dry-run` included) is offline-only because it holds
SQLite's single write lock from its first statement to its commit: a live
writer that meets a held write lock is refused with `database is locked` rather
than queued behind it, so running one of these against a live instance turns
ordinary user writes into errors for as long as the pass takes.

Either way: stop every Plombir Git server using the database, run the command,
then restart the server. The processes coordinate through a persistent sidecar
lease next to the database; a live server makes these commands fail before they
open a pool.

PostgreSQL and MySQL migrations stay online — no server has to be stopped — but
they are no longer unserialised. Every migrator takes a lock inside the database
itself (`pg_advisory_lock` / `GET_LOCK`) first, so two replicas booting at once,
a restart overlapping its predecessor, or a `migrate` run alongside a starting
server queue up instead of racing inside `CREATE TABLE`. A migrator that waits
more than five minutes for the holder gives up with a message naming Plombir Git
and what to do, rather than a PostgreSQL system-index name. The lock belongs to
a database session, so a crashed migrator releases it without leaving anything
to clean up.

That trap is the reason backups are not left to a manual command: enable
`[backup]` in the config file and the server takes a `VACUUM INTO` snapshot
every `interval_hours` from the pool it is already using, keeping the newest
`keep_last`. An unwritable `[backup].dir` fails the start rather than surfacing a
day later, and `plombir_git_db_backup_last_success_timestamp_seconds` lets
monitoring alert on "the last backup is older than N hours". It covers the
database only — repositories under `repo_root` need a volume snapshot of their
own. See `deploy/README.md` for the details.

Three tables grow with every event — webhook deliveries (each with its full
payload and response), notifications, and login attempts (each with the
client's IP address). `[retention]` keeps each to a window: by default 30 days
of deliveries, 90 days of read and 365 of unread notifications, 180 days of
login attempts. A notification still owed an e-mail is never deleted. The sweep
deletes in bounded batches so it never holds the database's write lock for a
whole backlog; `[retention].enabled = false` keeps every row.

Run `plombir-git <command> --help` for the full flag list.

`plombir-git package publish --token ...` follows the same boundary as the
standalone clients: remote `--server-url http://...` is refused, while loopback
HTTP remains usable for local development. `--allow-insecure-http` is the
explicit exception for a deliberately plaintext remote package server; it does
not relax the exact-origin redirect policy.
