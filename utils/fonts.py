"""Single shared, cached font loader for image-drawing modules."""

from __future__ import annotations

from PIL import ImageFont

_DEJAVU_DIR = "/usr/share/fonts/truetype/dejavu"

_FONT_CACHE: dict[
    tuple[int, bool, bool], ImageFont.FreeTypeFont | ImageFont.ImageFont
] = {}


def get_font(
    size: int = 16, *, bold: bool = False, mono: bool = False
) -> ImageFont.FreeTypeFont | ImageFont.ImageFont:
    """Return a cached font at ``size``.

    Fallback chain: DejaVu (mono variant first when ``mono``), then
    ``arial.ttf`` via PIL's font search, then PIL's built-in default.
    """
    key = (size, bold, mono)
    if key not in _FONT_CACHE:
        _FONT_CACHE[key] = _load_font(size, bold, mono)
    return _FONT_CACHE[key]


def _load_font(
    size: int, bold: bool, mono: bool
) -> ImageFont.FreeTypeFont | ImageFont.ImageFont:
    suffix = "-Bold" if bold else ""
    candidates: list[str] = []
    if mono:
        candidates.append(f"{_DEJAVU_DIR}/DejaVuSansMono{suffix}.ttf")
    candidates.append(f"{_DEJAVU_DIR}/DejaVuSans{suffix}.ttf")
    candidates.append("arial.ttf")
    for path in candidates:
        try:
            return ImageFont.truetype(path, size)
        except OSError:
            continue
    return ImageFont.load_default()
