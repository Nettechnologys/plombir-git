# Development

[← README](../README.md)

## Repository layout

```
plombir-git/
├── Cargo.toml              # workspace root
├── ARCHITECTURE.md         # architecture overview
├── CONTRIBUTING.md         # development guide
├── plombir-git.example.toml  # sample configuration
├── crates/
│   ├── rg-cli/     # main binary  → plombir-git
│   ├── rg-core/    # business logic (users, repos, issues, PRs, wiki, LFS, webhooks, ...)
│   ├── rg-git/     # Git protocol layer (pkt-line, upload/receive-pack, sideband)
│   ├── rg-ssh/     # SSH server (russh)
│   ├── rg-http/    # HTTP server + REST API + WebSocket (axum)
│   ├── rg-db/      # database layer (SeaORM entities + ops)
│   ├── rg-db-migration/ # schema migrations, below rg-db
│   ├── rg-ci/      # CI/CD engine (YAML parsing + pipeline executor)
│   ├── rg-runner/  # CI runner agent  → plombir-git-runner
│   └── rg-mcp/     # MCP server  → plombir-git-mcp
├── web/            # SvelteKit frontend (standalone SPA)
├── docs/           # design and protocol notes
├── deploy/         # deployment assets
└── scripts/        # regression / smoke-test scripts
```

> **Crate vs. binary naming.** Library crates keep a neutral `rg-*` prefix;
> the user-facing binaries are `plombir-git`, `plombir-git-runner`, and
> `plombir-git-mcp`.

See [ARCHITECTURE.md](../ARCHITECTURE.md) for subsystem design and
[CONTRIBUTING.md](../CONTRIBUTING.md) for development, testing, and crate
boundaries.

## Development

```bash
cargo build                 # debug build
cargo build --release       # release build
cargo clippy                # lint
cargo llvm-cov --html       # coverage report
```

Integration and runtime regressions are driven by scripts under `scripts/`
rather than ad-hoc shell snippets — for example:

```bash
# Full regression: backend tests, frontend checks/build, runtime smoke
node scripts/full-interface-regression.mjs

# Backend OpenAPI smoke against a running server. Replays every documented
# operation, so point it at a throwaway instance — it registers a user, sends
# sampled bodies and calls the delete verbs.
BACKEND_URL=http://127.0.0.1:8080 node scripts/openapi-interface-smoke.mjs

# The read-only half of the same script: one anonymous GET per documented path,
# asserting only that a route claims it. This is what CI runs against the built
# image, and it is safe against any instance.
OPENAPI_SMOKE_ROUTING_ONLY=1 BACKEND_URL=http://127.0.0.1:8080 \
  node scripts/openapi-interface-smoke.mjs
```

See [CONTRIBUTING.md](../CONTRIBUTING.md) for the full workflow and the available
environment toggles.
