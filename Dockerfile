FROM python:3.12-slim

# Install debugging tools, the cross-runtime process lock, and DejaVu fonts
# (needed by PIL for GIF text rendering).
RUN apt-get update && apt-get install -y --no-install-recommends gdb procps util-linux fonts-dejavu-core && \
    rm -rf /var/lib/apt/lists/*

# Create non-root user first
RUN useradd --create-home --shell /bin/bash --uid 1001 appuser && \
    mkdir -p /app && chown appuser:appuser /app

WORKDIR /app

# Install uv
COPY --from=ghcr.io/astral-sh/uv:0.11.28@sha256:0f36cb9361a3346885ca3677e3767016687b5a170c1a6b88465ec14aefec90aa /uv /usr/local/bin/uv

# Copy dependency files first for layer caching
COPY --chown=appuser:appuser pyproject.toml uv.lock ./

# Install dependencies as appuser
USER appuser
RUN uv sync --frozen --no-dev

# Make Python's hard-coded DejaVu lookup use the same bytes retained by the
# Rust image.  The distro package creates the conventional directory, while
# this copy and check keep its four rendering faces pinned to the locked
# Matplotlib wheel.
USER root
COPY assets/fonts/dejavu.sha256 /tmp/cama-dejavu.sha256
RUN set -eux; \
    for font in \
        DejaVuSans.ttf \
        DejaVuSans-Bold.ttf \
        DejaVuSansMono.ttf \
        DejaVuSansMono-Bold.ttf; do \
        cp "/app/.venv/lib/python3.12/site-packages/matplotlib/mpl-data/fonts/ttf/$font" \
            "/usr/share/fonts/truetype/dejavu/$font"; \
    done; \
    cd /usr/share/fonts/truetype/dejavu; \
    sha256sum -c /tmp/cama-dejavu.sha256
USER appuser

# Copy application code
COPY --chown=appuser:appuser . .

# The same fail-closed selector guard is baked into both runtime artifacts.
COPY --chown=root:root scripts/runtime-entrypoint /usr/local/bin/cama-runtime-entrypoint

# Bake deploy metadata late so a new SHA does not invalidate dependency layers.
ARG GIT_SHA=unknown
ENV GIT_SHA=${GIT_SHA}
ENV BOT_RUNTIME=python
LABEL org.opencontainers.image.revision=${GIT_SHA}

# Fail closed if a Rust replacement (or a stale Python deployment) already owns
# the production gateway/database lock. `--no-fork` leaves the Python process as
# PID 1 while the inherited descriptor retains the advisory lock.
ENTRYPOINT ["/usr/local/bin/cama-runtime-entrypoint", "python"]
CMD ["flock", "--nonblock", "--no-fork", "/app/data/.cama-runtime.lock", "uv", "run", "--no-sync", "python", "bot.py"]
