"""Shared colors, role order, font helpers, and heatmap primitives for ``utils.drawing``."""

from __future__ import annotations

from PIL import ImageFont

# ─── Discord-like dark theme colors ────────────────────────────────────────

DISCORD_BG = "#36393F"
DISCORD_DARKER = "#2F3136"
DISCORD_ACCENT = "#5865F2"
DISCORD_GREEN = "#57F287"
DISCORD_RED = "#ED4245"
DISCORD_YELLOW = "#FEE75C"
DISCORD_WHITE = "#FFFFFF"
DISCORD_GREY = "#B9BBBE"

# Role colors for radar graph
ROLE_COLORS = {
    "Carry": "#F44336",
    "Nuker": "#9C27B0",
    "Initiator": "#3F51B5",
    "Disabler": "#00BCD4",
    "Durable": "#4CAF50",
    "Escape": "#FFEB3B",
    "Support": "#FF9800",
    "Pusher": "#795548",
    "Jungler": "#607D8B",
}

# Fixed role order for consistent radar graph positioning
# Arranged for visual clarity: core roles at top, support at bottom
ROLE_ORDER = [
    "Carry",      # Top
    "Nuker",      # Top-right
    "Initiator",  # Right
    "Disabler",   # Bottom-right
    "Durable",    # Bottom
    "Escape",     # Bottom-left
    "Support",    # Left
    "Pusher",     # Top-left
    "Jungler",    # Near top-left
]

# Colors per draft position for the scout report heatmap.
POSITION_COLORS = {
    1: "#FF9800",  # Orange - Carry
    2: "#9C27B0",  # Purple - Mid
    3: "#4CAF50",  # Green - Offlane
    4: "#00BCD4",  # Cyan - Soft Support
    5: "#2196F3",  # Blue - Hard Support
}


# ─── Font + size helpers ───────────────────────────────────────────────────

def _get_font(size: int = 16) -> ImageFont.FreeTypeFont:
    """Get a font, falling back to default if custom fonts unavailable."""
    try:
        return ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", size)
    except OSError:
        try:
            return ImageFont.truetype("arial.ttf", size)
        except OSError:
            return ImageFont.load_default()


def _get_text_size(font: ImageFont.FreeTypeFont, text: str) -> tuple[int, int]:
    """Get text dimensions for the given font."""
    bbox = font.getbbox(text)
    return (bbox[2] - bbox[0], bbox[3] - bbox[1])


# ─── Heatmap color utilities (used by the scout report) ──────────────────

def _lerp_color(c1: tuple, c2: tuple, t: float) -> tuple:
    """Linearly interpolate between two RGB tuples."""
    t = max(0.0, min(1.0, t))
    return tuple(int(c1[i] + (c2[i] - c1[i]) * t) for i in range(3))


def _heatmap_contest_rate(rate: float) -> tuple:
    """Heatmap for contest rate: grey (0%) -> amber (50%) -> red (100%)."""
    rate = max(0.0, min(1.0, rate))
    if rate < 0.5:
        return _lerp_color((150, 150, 150), (255, 180, 50), rate / 0.5)
    return _lerp_color((255, 180, 50), (255, 60, 60), (rate - 0.5) / 0.5)


def _heatmap_winrate(rate: float) -> tuple:
    """Heatmap for win rate: red (0%) -> yellow (50%) -> green (100%)."""
    rate = max(0.0, min(1.0, rate))
    if rate < 0.5:
        return _lerp_color((255, 60, 60), (255, 220, 50), rate / 0.5)
    return _lerp_color((255, 220, 50), (80, 220, 80), (rate - 0.5) / 0.5)
