"""Shared constants, palette, and helper primitives for wrapped slides."""

from __future__ import annotations

from PIL import ImageDraw, ImageFont

# ─── Palette ────────────────────────────────────────────────────────────────

BG_GRADIENT_START = (30, 30, 35)
BG_GRADIENT_END = (45, 45, 55)
ACCENT_GOLD = (241, 196, 15)
ACCENT_GREEN = (87, 242, 135)
ACCENT_RED = (237, 66, 69)
ACCENT_BLUE = (88, 101, 242)
ACCENT_PURPLE = (155, 89, 182)
TEXT_WHITE = (255, 255, 255)
TEXT_GREY = (185, 187, 190)
TEXT_DARK = (100, 100, 100)

CATEGORY_COLORS = {
    "performance": (88, 101, 242),
    "rating": (155, 89, 182),
    "economy": (241, 196, 15),
    "hero": (46, 204, 113),
    "fun": (231, 76, 60),
}

SLIDE_COLORS = {
    "combat": (237, 66, 69),
    "farming": (241, 196, 15),
    "impact": (88, 101, 242),
    "vision": (87, 242, 135),
    "endurance": (155, 89, 182),
    "story_games": (88, 101, 242),
    "story_summary": (241, 196, 15),
    "story_hero": (46, 204, 113),
    "story_role": (52, 152, 219),
    "story_teammates": (87, 242, 135),
    "story_rivals": (237, 66, 69),
    "story_packages": (155, 89, 182),
    "story_rating": (241, 196, 15),
    "story_gamba": (231, 76, 60),
    "server_summary": (241, 196, 15),
    "awards": (88, 101, 242),
}

WORST_LABEL_COLOR = (255, 120, 120)
NA_COLOR = (80, 80, 90)


# ─── Primitives ─────────────────────────────────────────────────────────────

def _get_font(size: int = 16, bold: bool = False) -> ImageFont.FreeTypeFont:
    """Return a DejaVu font at ``size``, falling back to PIL's default."""
    try:
        font_name = "DejaVuSans-Bold.ttf" if bold else "DejaVuSans.ttf"
        return ImageFont.truetype(f"/usr/share/fonts/truetype/dejavu/{font_name}", size)
    except OSError:
        return ImageFont.load_default()


def _draw_gradient_background(
    draw: ImageDraw.Draw, width: int, height: int, start_color: tuple, end_color: tuple
) -> None:
    """Paint a vertical gradient between ``start_color`` and ``end_color``."""
    for y in range(height):
        ratio = y / height
        r = int(start_color[0] + (end_color[0] - start_color[0]) * ratio)
        g = int(start_color[1] + (end_color[1] - start_color[1]) * ratio)
        b = int(start_color[2] + (end_color[2] - start_color[2]) * ratio)
        draw.line([(0, y), (width, y)], fill=(r, g, b))


def _draw_rgba_gradient_background(
    draw: ImageDraw.Draw, width: int, height: int
) -> None:
    """Gradient variant that outputs RGBA tuples (for slides on RGBA canvases)."""
    for y in range(height):
        ratio = y / height
        r = int(BG_GRADIENT_START[0] + (BG_GRADIENT_END[0] - BG_GRADIENT_START[0]) * ratio)
        g = int(BG_GRADIENT_START[1] + (BG_GRADIENT_END[1] - BG_GRADIENT_START[1]) * ratio)
        b = int(BG_GRADIENT_START[2] + (BG_GRADIENT_END[2] - BG_GRADIENT_START[2]) * ratio)
        draw.line([(0, y), (width, y)], fill=(r, g, b, 255))


def _draw_rounded_rect(
    draw: ImageDraw.Draw,
    xy: tuple,
    radius: int,
    fill: tuple | None = None,
    outline: tuple | None = None,
    width: int = 1,
) -> None:
    """Thin wrapper around ``draw.rounded_rectangle`` used across slides."""
    draw.rounded_rectangle(xy, radius=radius, fill=fill, outline=outline, width=width)


def _word_wrap(text: str, font, max_width: int, draw: ImageDraw.Draw) -> list[str]:
    """Break ``text`` into lines that fit within ``max_width`` pixels.

    Any line that still exceeds ``max_width`` after wrapping is truncated with
    a trailing ``..`` so long single tokens degrade gracefully.
    """
    words = text.split()
    lines: list[str] = []
    current = ""
    for word in words:
        test = f"{current} {word}".strip()
        if draw.textlength(test, font=font) <= max_width:
            current = test
        else:
            if current:
                lines.append(current)
            current = word
    if current:
        lines.append(current)
    result = []
    for line in lines:
        if draw.textlength(line, font=font) > max_width:
            while draw.textlength(line + "..", font=font) > max_width and len(line) > 1:
                line = line[:-1]
            line = line.rstrip() + ".."
        result.append(line)
    return result


def _center_text(
    draw: ImageDraw.Draw, text: str, font, y: int, width: int, fill: tuple
) -> None:
    """Draw ``text`` horizontally centered at the given y position."""
    bbox = draw.textbbox((0, 0), text, font=font)
    text_w = bbox[2] - bbox[0]
    draw.text(((width - text_w) // 2, y), text, fill=fill, font=font)


def _draw_wrapped_header(
    draw: ImageDraw.Draw, username: str, year_label: str, width: int
) -> None:
    """Draw the standard wrapped story header with user on left, year on right."""
    header_font = _get_font(14)
    draw.text((30, 15), f"@{username}", fill=TEXT_GREY, font=header_font)
    bbox = draw.textbbox((0, 0), year_label.upper(), font=header_font)
    text_w = bbox[2] - bbox[0]
    draw.text((width - 30 - text_w, 15), year_label.upper(), fill=TEXT_GREY, font=header_font)
