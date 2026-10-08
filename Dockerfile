# syntax=docker/dockerfile:1
# ^ Must stay the first line: once any other comment or instruction has been
# read, a parser directive is just a comment. It selects the external BuildKit
# Dockerfile frontend, which stage 2a needs for `COPY --parents` — the flag is
# stable there, but the daemon's builtin parser rejects it outright
# (`unknown flag: --parents`).
#
# === Plombir Git Dockerfile ===
# Multi-stage build: frontend (SvelteKit) + Rust builder + minimal runtime.
#
# Build:
#   docker build -t plombir-git:latest .
#
# Run:
#   docker run -d -p 8080:8080 -p 2222:2222 \
#     -e PLOMBIR_GIT_JWT_SECRET=your-secret \
#     -v plombir-git-data:/data \
#     plombir-git:latest

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
# The compiler is whatever `rust-toolchain.toml` names — the same one CI and the
# local push gates use. Keep this tag on the same version and the toolchain is
# already in the image; let it fall behind and the install below downloads the
# right one instead of building with the wrong one.
FROM rust:1.96.1-slim-bookworm AS builder
WORKDIR /build

RUN apt-get update && apt-get install -y --no-install-recommends \
    libsqlite3-dev \
    libssl-dev \
    pkg-config \
    jq \
    # curl + ca-certificates are needed at build time: utoipa-swagger-ui's
    # build script downloads the Swagger UI assets via the curl CLI.
    curl \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Install the toolchain `rust-toolchain.toml` names before anything compiles,
# in a layer of its own so a manifest change does not repeat it. Explicit rather
# than left to rustup's implicit install on the first cargo call, which is a
# setting rather than a guarantee.
COPY rust-toolchain.toml ./
RUN rustup toolchain install --no-self-update \
    && rustc --version

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
#
#     PLOMBIR_GIT_SOURCE_COMMIT is the commit these sources are, recorded into the
#     binary so every page can link the exact code it runs (AGPL §13; see
#     `rg_http::build_info`). It is declared here, after 2c, on purpose: an ARG
#     is part of the cache key of every later RUN, so declaring it above the
#     dependency build would rebuild every dependency on every commit. Pass
#     `--build-arg PLOMBIR_GIT_SOURCE_COMMIT=$(git rev-parse HEAD)`; left empty,
#     the server links the repository instead and warns about it at startup.
ARG PLOMBIR_GIT_SOURCE_COMMIT=
RUN find crates -name '*.rs' -delete
COPY crates/ crates/
RUN find crates -name '*.rs' -exec touch {} + \
    && cargo build --release --workspace --bins

# Derive the runtime payload from Cargo's target graph. Keeping this as a
# separate directory prevents `*.d`, `deps/` and other release-build outputs
# from leaking into the image while making every new workspace binary opt-out
# rather than opt-in.
RUN set -eu; \
    mkdir -p /out; \
    cargo metadata --format-version 1 --no-deps \
      | jq -r '[.packages[].targets[] | select(.kind | index("bin")) | .name] | unique[]' \
      | while IFS= read -r binary; do \
          artifact="target/release/${binary}"; \
          if [ ! -x "${artifact}" ]; then \
            echo "Cargo binary target has no executable release artifact: ${binary}" >&2; \
            exit 1; \
          fi; \
          strip "${artifact}"; \
          cp "${artifact}" "/out/${binary}"; \
        done; \
    test -n "$(find /out -maxdepth 1 -type f -print -quit)"

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
# `plombir-git` user has nothing to do with a host user of the same name. Build
# with `--build-arg PLOMBIR_GIT_UID=$(id -u)` to match the host owner of the
# bind-mount and skip the `chown` step entirely.
ARG PLOMBIR_GIT_UID=1000
ARG PLOMBIR_GIT_GID=1000
RUN groupadd --gid ${PLOMBIR_GIT_GID} plombir-git \
    && useradd --uid ${PLOMBIR_GIT_UID} --gid ${PLOMBIR_GIT_GID} \
       --create-home --shell /bin/bash plombir-git

# Copy the complete Cargo-derived runtime payload.
COPY --from=builder /out/ /usr/local/bin/

# Copy frontend static assets (served at web/build relative to WORKDIR)
COPY --from=frontend-builder /build/web/build /app/web/build

# Create data directories.
#
# `chmod 700 /data` is the named-volume half of the same decision the host
# quick-start makes with `install -d -m 700 data`: a fresh named volume is
# seeded from this path, modes included, so `mkdir`'s 0755 would follow the
# private repositories and the database into the volume. Only `/data` needs it —
# what is below an unreadable directory cannot be reached whatever its own mode
# — and `/app` stays as it is, since the static assets there are served to
# anyone anyway.
RUN mkdir -p /data/repos /data/config /data/logs \
    && chown -R plombir-git:plombir-git /data /app \
    && chmod 700 /data

WORKDIR /app
USER plombir-git

# Expose ports
EXPOSE 8080 2222

# Health check: /readyz — the database answers and the repository storage is
# readable. Not /health, whose full report includes optional dependencies
# (SMTP) that must not mark the container unhealthy.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:8080/readyz || exit 1

# Default command: serve with config via env vars.
# Set PLOMBIR_GIT_JWT_SECRET env var before running.
# No --log-file: the log goes to stdout, which is what `docker compose logs`
# and every Docker log driver read. A file here left `docker compose logs`
# empty while deploy/README.md told operators to look there.
CMD ["plombir-git", "serve", \
     "--repo-root", "/data/repos", \
     "--http-addr", "0.0.0.0:8080", \
     "--ssh-addr", "0.0.0.0:2222", \
     "--db-url", "sqlite:///data/plombir-git.db?mode=rwc"]
