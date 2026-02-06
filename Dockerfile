FROM python:3.12-slim

# Install DejaVu fonts (needed by PIL for GIF text rendering)
RUN apt-get update && apt-get install -y --no-install-recommends gdb procps fonts-dejavu-core && \
    rm -rf /var/lib/apt/lists/*

# Create non-root user first
RUN useradd --create-home --shell /bin/bash --uid 1001 appuser && \
    mkdir -p /app && chown appuser:appuser /app

WORKDIR /app

# Install uv
COPY --from=ghcr.io/astral-sh/uv:latest /uv /usr/local/bin/uv

# Copy dependency files first for layer caching
COPY --chown=appuser:appuser pyproject.toml uv.lock ./

# Install dependencies as appuser
USER appuser
RUN uv sync --frozen --no-dev

# Copy application code
COPY --chown=appuser:appuser . .

# Run the bot
CMD ["uv", "run", "--no-sync", "python", "bot.py"]
