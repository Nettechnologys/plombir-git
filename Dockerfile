# === IronForge Dockerfile ===
# Multi-stage build: frontend (SvelteKit) + Rust builder + minimal runtime.
#
# Build:
#   docker build -t ironforge:latest .
#
# Run:
#   docker run -d -p 8080:8080 -p 2222:2222 \
#     -e IRONFORGE_JWT_SECRET=your-secret \
#     -v ironforge-data:/data \
#     ironforge:latest

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

# 2a. Copy workspace manifests for dependency caching
COPY Cargo.toml Cargo.lock ./
COPY crates/rg-cli/Cargo.toml    crates/rg-cli/
COPY crates/rg-core/Cargo.toml   crates/rg-core/
COPY crates/rg-git/Cargo.toml    crates/rg-git/
COPY crates/rg-ssh/Cargo.toml    crates/rg-ssh/
COPY crates/rg-http/Cargo.toml   crates/rg-http/
COPY crates/rg-db/Cargo.toml     crates/rg-db/
COPY crates/rg-ci/Cargo.toml     crates/rg-ci/
COPY crates/rg-runner/Cargo.toml crates/rg-runner/
COPY crates/rg-mcp/Cargo.toml    crates/rg-mcp/

# 2b. Create dummy source files so cargo can resolve all workspace members
#     Bin crates need main.rs; lib crates need lib.rs
RUN mkdir -p crates/rg-cli/src && echo 'fn main() {}' > crates/rg-cli/src/main.rs \
    && mkdir -p crates/rg-mcp/src && echo 'fn main() {}' > crates/rg-mcp/src/main.rs \
    && mkdir -p crates/rg-runner/src && echo 'fn main() {}' > crates/rg-runner/src/main.rs
RUN for crate in rg-core rg-git rg-ssh rg-http rg-db rg-ci; do \
      mkdir -p crates/$crate/src && echo '' > crates/$crate/src/lib.rs; \
    done

# 2c. Cache all crate dependencies (dummy code is valid Rust, will compile)
RUN cargo build --release

# 2d. Copy actual source, touch to force rebuild, and compile
COPY crates/ crates/
RUN touch crates/rg-cli/src/main.rs \
    crates/rg-mcp/src/main.rs \
    crates/rg-runner/src/main.rs \
    crates/rg-core/src/lib.rs \
    crates/rg-git/src/lib.rs \
    crates/rg-ssh/src/lib.rs \
    crates/rg-http/src/lib.rs \
    crates/rg-db/src/lib.rs \
    crates/rg-ci/src/lib.rs \
    && cargo build --release --bin ironforge --bin ironforge-runner --bin ironforge-mcp

# Strip symbols to reduce binary size
RUN strip target/release/ironforge \
    target/release/ironforge-runner \
    target/release/ironforge-mcp

# ── Stage 3: Runtime ────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    git \
    curl \
    libsqlite3-0 \
    openssh-client \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd --create-home --shell /bin/bash ironforge

# Copy binaries
COPY --from=builder /build/target/release/ironforge /usr/local/bin/ironforge
COPY --from=builder /build/target/release/ironforge-runner /usr/local/bin/ironforge-runner
COPY --from=builder /build/target/release/ironforge-mcp /usr/local/bin/ironforge-mcp

# Copy frontend static assets (served at web/build relative to WORKDIR)
COPY --from=frontend-builder /build/web/build /app/web/build

# Create data directories
RUN mkdir -p /data/repos /data/config /data/logs \
    && chown -R ironforge:ironforge /data /app

WORKDIR /app
USER ironforge

# Expose ports
EXPOSE 8080 2222

# Health check (uses ironforge's built-in /health endpoint)
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1

# Default command: serve with config via env vars.
# Set IRONFORGE_JWT_SECRET env var before running.
CMD ["ironforge", "serve", \
     "--repo-root", "/data/repos", \
     "--http-addr", "0.0.0.0:8080", \
     "--ssh-addr", "0.0.0.0:2222", \
     "--db-url", "sqlite:///data/ironforge.db?mode=rwc", \
     "--log-file", "/data/logs/ironforge.log"]
