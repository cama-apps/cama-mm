"""
Disk-based image cache for trivia questions.

Downloads Steam CDN images to .cache/trivia/ so they can be sent as
Discord file attachments instead of remote URLs (much faster embeds).
"""

from __future__ import annotations

import hashlib
import logging
from pathlib import Path
from urllib.parse import urlparse

import discord
import requests

logger = logging.getLogger("cama_bot.trivia_image_cache")

CACHE_DIR = Path(".cache/trivia")


def _url_to_path(url: str) -> Path:
    """Map a CDN URL to a deterministic local cache path."""
    parsed = urlparse(url)
    # Use the URL path to build a meaningful filename
    # e.g. /apps/dota2/images/dota_react/heroes/antimage.png -> heroes/antimage.png
    path_parts = parsed.path.strip("/").split("/")
    # Find the segment after "dota_react" to get the category + filename
    try:
        idx = path_parts.index("dota_react")
        rel = "/".join(path_parts[idx + 1:])
    except ValueError:
        # Fallback: use hash of URL
        url_hash = hashlib.md5(url.encode()).hexdigest()
        ext = Path(parsed.path).suffix or ".png"
        rel = f"other/{url_hash}{ext}"
    return CACHE_DIR / rel


def ensure_cached(url: str) -> Path | None:
    """Download the image at *url* if not already cached. Returns local path or None on failure."""
    path = _url_to_path(url)
    if path.exists():
        return path
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        resp = requests.get(url, timeout=10)
        resp.raise_for_status()
        path.write_bytes(resp.content)
        return path
    except Exception:
        logger.debug("Failed to cache %s", url, exc_info=True)
        return None


def get_trivia_image(url: str) -> discord.File | None:
    """Return a discord.File from the cache for *url*, or None if not cached."""
    path = _url_to_path(url)
    if not path.exists():
        # Try a quick download as fallback
        path = ensure_cached(url)
    if path and path.exists():
        return discord.File(str(path), filename=path.name)
    return None


def warm_cache() -> int:
    """Pre-download all known trivia images. Returns count of newly cached files."""
    from services.trivia_data import load_abilities, load_heroes, load_items

    urls: set[str] = set()
    for h in load_heroes():
        if h.image_url:
            urls.add(h.image_url)
    for a in load_abilities():
        if a.icon_url:
            urls.add(a.icon_url)
    for i in load_items():
        if i.icon_url:
            urls.add(i.icon_url)

    newly_cached = 0
    for url in urls:
        path = _url_to_path(url)
        if not path.exists() and ensure_cached(url):
            newly_cached += 1
    logger.info("Trivia image cache warm: %d new, %d total", newly_cached, len(urls))
    return newly_cached
