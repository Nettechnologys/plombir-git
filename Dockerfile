# syntax=docker/dockerfile:1
# ^ Must stay the first line: once any other comment or instruction has been
# read, a parser directive is just a comment. It selects the external BuildKit
# Dockerfile frontend, which stage 2a needs for `COPY --parents` — the flag is
# stable there, but the daemon's builtin parser rejects it outright
# (`unknown flag: --parents`).
#
# === ForgeKeep Dockerfile ===
# Multi-stage build: frontend (SvelteKit) + Rust builder + minimal runtime.
#
# Build:
#   docker build -t forgekeep:latest .
#
# Run:
#   docker run -d -p 8080:8080 -p 2222:2222 \
#     -e FORGEKEEP_JWT_SECRET=your-secret \
#     -v forgekeep-data:/data \
#     forgekeep:latest

# ── Stage 1: Frontend (SvelteKit SPA) ────────────────────────
FROM node:22-alpine AS frontend-builder
WORKDIR /build/web

# Cache npm deps
COPY web/package.json web/package-lock.json* ./
RUN npm ci

# Build the SPA (static adapter, output to ./build/)
COPY web/svelte.config.js web/tsconfig.json web/vite.config.ts ./
COPY web/src/ ./src/
COPY web/static/ ./static/
RUN npm run build
# Output: /build/web/build/ (static adapter with fallback: index.html)

# ── Stage 2: Rust builder ───────────────────────────────────
FROM rust:1.95.0-slim-bookworm AS builder
WORKDIR /build

RUN apt-get update && apt-get install -y --no-install-recommends \
    libsqlite3-dev \
    libssl-dev \
    pkg-config \
    # curl + ca-certificates are needed at build time: utoipa-swagger-ui's
    # build script downloads the Swagger UI assets via the curl CLI.
    curl \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# 2a. Copy workspace manifests for dependency caching.
#
#     `--parents` is what makes the wildcard usable: a plain
#     `COPY crates/*/Cargo.toml crates/` flattens every match onto the single
#     destination path, so all nine manifests land in `crates/Cargo.toml` and
#     the last one wins. That is why this used to be nine hand-written lines
#     that a tenth workspace member would have silently missed.
#
#     Only the manifests are copied on purpose — this layer is the cache key of
#     stage 2c, so it must not see a single `.rs` file, or every source edit
#     would rebuild the whole dependency tree.
COPY Cargo.toml Cargo.lock ./
COPY --parents crates/*/Cargo.toml ./

# 2b. Stub out every workspace member so cargo can resolve the graph before the
#     real sources exist.
#
#     Both target files are written for each crate, rather than deciding per
#     crate which one it needs: that decision is exactly the per-crate list this
#     stage is trying not to have. The cost is a handful of extra empty targets
#     compiled once (an auto-discovered `rg-core` bin, an `rg_cli` lib, and so
#     on); the stubs are deleted again in 2d before the real sources arrive.
RUN for manifest in crates/*/Cargo.toml; do \
      src="$(dirname "${manifest}")/src"; \
      mkdir -p "${src}"; \
      : > "${src}/lib.rs"; \
      echo 'fn main() {}' > "${src}/main.rs"; \
    done

# 2c. Cache all crate dependencies (the stubs are valid Rust and compile)
RUN cargo build --release

# 2d. Drop the stubs, copy the real sources, and compile for real.
#
#     The stubs are removed first because `COPY` overwrites but never deletes:
#     a stub `main.rs` left in a lib-only crate would stay in the tree as an
#     auto-discovered binary target built from `fn main() {}`.
#
#     `touch` is not cosmetic — cargo's fingerprint is mtime-based and `COPY`
#     preserves the build context's timestamps, so without it cargo can consider
#     the artifacts built from the stubs newer than the sources that replaced
#     them and skip the rebuild entirely.
RUN find crates -name '*.rs' -delete
COPY crates/ crates/
RUN find crates -name '*.rs' -exec touch {} + \
    && cargo build --release --bin forgekeep --bin forgekeep-runner --bin forgekeep-mcp

# Strip symbols to reduce binary size
RUN strip target/release/forgekeep \
    target/release/forgekeep-runner \
    target/release/forgekeep-mcp

# ── Stage 3: Runtime ────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    git \
    curl \
    libsqlite3-0 \
    openssh-client \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user.
#
# The uid/gid are pinned (and overridable) on purpose: with a bind-mounted data
# directory the *number* is what matters, not the name — the container's
# `forgekeep` user has nothing to do with a host user of the same name. Build
# with `--build-arg FORGEKEEP_UID=$(id -u)` to match the host owner of the
# bind-mount and skip the `chown` step entirely.
ARG FORGEKEEP_UID=1000
ARG FORGEKEEP_GID=1000
RUN groupadd --gid ${FORGEKEEP_GID} forgekeep \
    && useradd --uid ${FORGEKEEP_UID} --gid ${FORGEKEEP_GID} \
       --create-home --shell /bin/bash forgekeep

# Copy binaries
COPY --from=builder /build/target/release/forgekeep /usr/local/bin/forgekeep
COPY --from=builder /build/target/release/forgekeep-runner /usr/local/bin/forgekeep-runner
COPY --from=builder /build/target/release/forgekeep-mcp /usr/local/bin/forgekeep-mcp

# Copy frontend static assets (served at web/build relative to WORKDIR)
COPY --from=frontend-builder /build/web/build /app/web/build

# Create data directories
RUN mkdir -p /data/repos /data/config /data/logs \
    && chown -R forgekeep:forgekeep /data /app

WORKDIR /app
USER forgekeep

# Expose ports
EXPOSE 8080 2222

# Health check (uses forgekeep's built-in /health endpoint)
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1

# Default command: serve with config via env vars.
# Set FORGEKEEP_JWT_SECRET env var before running.
CMD ["forgekeep", "serve", \
     "--repo-root", "/data/repos", \
     "--http-addr", "0.0.0.0:8080", \
     "--ssh-addr", "0.0.0.0:2222", \
     "--db-url", "sqlite:///data/forgekeep.db?mode=rwc", \
     "--log-file", "/data/logs/forgekeep.log"]
