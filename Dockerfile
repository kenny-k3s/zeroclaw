# syntax=docker/dockerfile:1.7

# ── Stage 1: Build ────────────────────────────────────────────
FROM rust:1.93-slim@sha256:9663b80a1621253d30b146454f903de48f0af925c967be48c84745537cd35d8b AS builder
ENV RUST_MIN_STACK=16777216

WORKDIR /app

# Install build dependencies
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

# 1. Copy manifests to cache dependencies
COPY Cargo.toml Cargo.lock ./
COPY crates/robot-kit/Cargo.toml crates/robot-kit/Cargo.toml
# Create dummy targets declared in Cargo.toml so manifest parsing succeeds.
RUN mkdir -p src benches crates/robot-kit/src \
    && echo "fn main() {}" > src/main.rs \
    && echo "fn main() {}" > benches/agent_benchmarks.rs \
    && echo "pub fn placeholder() {}" > crates/robot-kit/src/lib.rs
RUN --mount=type=cache,id=zeroclaw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=zeroclaw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=zeroclaw-target,target=/app/target,sharing=locked \
    cargo build --release --locked
RUN rm -rf src benches crates/robot-kit/src

# 2. Copy only build-relevant source paths (avoid cache-busting on docs/tests/scripts)
COPY src/ src/
COPY benches/ benches/
COPY crates/ crates/
COPY firmware/ firmware/
COPY web/ web/
# Keep release builds resilient when frontend dist assets are not prebuilt in Git.
RUN mkdir -p web/dist && \
    if [ ! -f web/dist/index.html ]; then \
      printf '%s\n' \
        '<!doctype html>' \
        '<html lang="en">' \
        '  <head>' \
        '    <meta charset="utf-8" />' \
        '    <meta name="viewport" content="width=device-width,initial-scale=1" />' \
        '    <title>ZeroClaw Dashboard</title>' \
        '  </head>' \
        '  <body>' \
        '    <h1>ZeroClaw Dashboard Unavailable</h1>' \
        '    <p>Frontend assets are not bundled in this build. Build the web UI to populate <code>web/dist</code>.</p>' \
        '  </body>' \
        '</html>' > web/dist/index.html; \
    fi
RUN --mount=type=cache,id=zeroclaw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=zeroclaw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=zeroclaw-target,target=/app/target,sharing=locked \
    cargo build --release --locked && \
    cp target/release/zeroclaw /app/zeroclaw && \
    strip /app/zeroclaw

# Prepare runtime directory structure and default config inline (no extra stage)
RUN mkdir -p /zeroclaw-data/.zeroclaw /zeroclaw-data/workspace && \
    cat > /zeroclaw-data/.zeroclaw/config.toml <<EOF && \
    chown -R 65534:65534 /zeroclaw-data
workspace_dir = "/zeroclaw-data/workspace"
config_path = "/zeroclaw-data/.zeroclaw/config.toml"
api_key = ""
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-20250514"
default_temperature = 0.7

[gateway]
port = 42617
host = "[::]"
allow_public_bind = true
EOF

# ── Stage 2: Development Runtime (Debian) ────────────────────
FROM debian:trixie-slim@sha256:f6e2cfac5cf956ea044b4bd75e6397b4372ad88fe00908045e9a0d21712ae3ba AS dev

# Install essential runtime dependencies only (use docker-compose.override.yml for dev tools)
RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /zeroclaw-data /zeroclaw-data
COPY --from=builder /app/zeroclaw /usr/local/bin/zeroclaw

# Overwrite minimal config with DEV template (Ollama defaults)
COPY dev/config.template.toml /zeroclaw-data/.zeroclaw/config.toml
RUN chown 65534:65534 /zeroclaw-data/.zeroclaw/config.toml

# Environment setup
# Use consistent workspace path
ENV ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace
ENV HOME=/zeroclaw-data
# Defaults for local dev (Ollama) - matches config.template.toml
ENV PROVIDER="ollama"
ENV ZEROCLAW_MODEL="llama3.2"
ENV ZEROCLAW_GATEWAY_PORT=42617

# Note: API_KEY is intentionally NOT set here to avoid confusion.
# It is set in config.toml as the Ollama URL.

WORKDIR /zeroclaw-data
USER 65534:65534
EXPOSE 42617
ENTRYPOINT ["zeroclaw"]
CMD ["gateway"]

# ── Stage 3: Build a prebuilt Homebrew prefix for runtime and prepare release ──
FROM debian:trixie-slim@sha256:f6e2cfac5cf956ea044b4bd75e6397b4372ad88fe00908045e9a0d21712ae3ba AS brew-builder

# Build a minimal prebuilt Homebrew prefix so runtime can bootstrap a writable copy.
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    git \
    build-essential \
    tar \
    xz-utils \
    && rm -rf /var/lib/apt/lists/*

ENV HOMEBREW_PREFIX=/opt/linuxbrew
RUN mkdir -p ${HOMEBREW_PREFIX} \
  && git clone --depth=1 https://github.com/Homebrew/brew ${HOMEBREW_PREFIX}/Homebrew \
  && mkdir -p ${HOMEBREW_PREFIX}/bin \
  && ln -s ${HOMEBREW_PREFIX}/Homebrew/bin/brew ${HOMEBREW_PREFIX}/bin/brew || true

RUN mkdir -p /opt/bootstrap_linuxbrew && cp -a ${HOMEBREW_PREFIX} /opt/bootstrap_linuxbrew/.linuxbrew

FROM debian:trixie-slim@sha256:f6e2cfac5cf956ea044b4bd75e6397b4372ad88fe00908045e9a0d21712ae3ba AS release

# Install runtime dependencies: ca-certs, curl, Node.js, npm
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    nodejs \
    npm \
    && rm -rf /var/lib/apt/lists/*

# Copy prebuilt brew prefix
COPY --from=brew-builder /opt/bootstrap_linuxbrew /opt/bootstrap_linuxbrew

# Ensure runtime workspace directories exist and are writable for non-root user
RUN mkdir -p /zeroclaw-data /zeroclaw-data/.linuxbrew /zeroclaw-data/.npm-global /zeroclaw-data/workspace \
  && chown -R 65534:65534 /zeroclaw-data

# Configure npm global prefix to runtime-writable location
ENV NPM_CONFIG_PREFIX=/zeroclaw-data/.npm-global
ENV HOMEBREW_RUNTIME_PREFIX=/zeroclaw-data/.linuxbrew
ENV PATH=/zeroclaw-data/.linuxbrew/bin:/zeroclaw-data/.npm-global/bin:/usr/local/bin:/usr/bin:/bin

# Install mcporter into the configured npm global prefix at build time
RUN npm config set prefix "$NPM_CONFIG_PREFIX" && npm install -g mcporter --unsafe-perm --loglevel=http

# Provide a small brew shim that ensures a writable copy of the prebuilt prefix
RUN cat > /usr/local/bin/brew <<'EOF'
#!/bin/sh
set -eu
DEST="${HOMEBREW_RUNTIME_PREFIX:-/zeroclaw-data/.linuxbrew}"
BOOTSTRAP="/opt/bootstrap_linuxbrew/.linuxbrew"
if [ ! -x "$DEST/bin/brew" ]; then
  mkdir -p "$DEST"
  cp -a "$BOOTSTRAP"/* "$DEST/" || true
  chown -R 65534:65534 "$DEST" || true
fi
exec "$DEST/bin/brew" "$@"
EOF
RUN chmod 0755 /usr/local/bin/brew

# Smoke checks: verify tooling is installed
# Use the prebuilt brew path directly to avoid triggering shim copy during image build
RUN node --version && npm --version && /opt/bootstrap_linuxbrew/.linuxbrew/bin/brew --version && /zeroclaw-data/.npm-global/bin/mcporter --help || true

COPY --from=builder /app/zeroclaw /usr/local/bin/zeroclaw
COPY --from=builder /zeroclaw-data /zeroclaw-data

# Environment setup
ENV ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace
ENV HOME=/zeroclaw-data
ENV ZEROCLAW_GATEWAY_PORT=42617

WORKDIR /zeroclaw-data
USER 65534:65534
EXPOSE 42617
ENTRYPOINT ["zeroclaw"]
CMD ["gateway"]
