"""Wheel of Fortune image generation using Pillow."""

import io
import math
import random
from PIL import Image, ImageDraw, ImageFont

from config import WHEEL_TARGET_EV

# Cached fonts for performance (loaded once, not per frame)
_CACHED_FONTS: dict[str, ImageFont.FreeTypeFont | ImageFont.ImageFont] = {}


def _get_cached_font(size: int, font_key: str, bold: bool = False) -> ImageFont.FreeTypeFont | ImageFont.ImageFont:
    """Get a cached font, loading it only on first access."""
    cache_key = f"{font_key}_{size}_{'bold' if bold else 'regular'}"
    if cache_key not in _CACHED_FONTS:
        try:
            font_name = "DejaVuSans-Bold.ttf" if bold else "DejaVuSans.ttf"
            font_path = f"/usr/share/fonts/truetype/dejavu/{font_name}"
            _CACHED_FONTS[cache_key] = ImageFont.truetype(font_path, size)
        except OSError:
            _CACHED_FONTS[cache_key] = ImageFont.load_default()
    return _CACHED_FONTS[cache_key]


# Cached static overlay (pointer, center circle, center text) - drawn once per size
_CACHED_STATIC_OVERLAY: dict[int, Image.Image] = {}

# Cached wheel face sprite (all wedges + text + glow ring) - drawn once per size
_CACHED_WHEEL_FACE: dict[int, Image.Image] = {}

# Matrix rain column data: pre-computed once per size
_CACHED_RAIN_COLUMNS: dict[int, list[dict]] = {}

# Jopacoin-themed glyphs for the matrix rain
_RAIN_GLYPHS = list("JOPACOINDEGEN$01234567890+-=%<>")

# Troll messages that occasionally spell out vertically in rain columns
_RAIN_MESSAGES = [
    "JOPA WAS HERE",
    "322",
    "JUMPMAN JUMPMAN JUMPMAN",
    "GG EZ",
    "DEGEN",
    "ALL IN",
    "GAMBA",
    "RIGGED",
    "EZ CLAP",
    "COPIUM",
    "JOPACOINS",
    "RNG GOD",
    "SEND IT",
    "NO BALLS",
]


def _get_rain_phase(frame_idx: int) -> dict:
    """Map frame index to rain visibility and fade-out.

    Frames 0-37: normal rain.  38-44: fading out.  45+: invisible.
    """
    if frame_idx < 38:
        return {"visible": True, "fade": 0.0}
    elif frame_idx < 45:
        progress = (frame_idx - 38) / 6  # 0.0 -> ~1.0 over 7 frames
        return {"visible": True, "fade": progress}
    else:
        return {"visible": False, "fade": 1.0}


def _get_rain_columns(size: int) -> list[dict]:
    """Get pre-computed matrix rain column data for a given image size."""
    if size not in _CACHED_RAIN_COLUMNS:
        _CACHED_RAIN_COLUMNS[size] = _create_rain_columns(size)
    return _CACHED_RAIN_COLUMNS[size]


def _make_column_glyphs(count: int) -> list[str]:
    """Generate glyphs for a rain column, occasionally spelling out a troll message."""
    if random.random() < 0.35:
        msg = random.choice(_RAIN_MESSAGES)
        chars = list(msg)
        # Tile the message characters to fill the column, with random padding between repeats
        glyphs = []
        while len(glyphs) < count:
            glyphs.extend(chars)
            # Add 2-5 random glyphs as spacing between repeats
            glyphs.extend(random.choice(_RAIN_GLYPHS) for _ in range(random.randint(2, 5)))
        return glyphs[:count]
    return [random.choice(_RAIN_GLYPHS) for _ in range(count)]


def _create_rain_columns(size: int) -> list[dict]:
    """Create matrix rain columns positioned in the dark corner areas."""
    center = size // 2
    radius = size // 2 - 30
    columns = []
    char_h = max(8, size // 50)
    glyph_count = size // char_h + 5

    for _ in range(18):
        x = random.randint(4, size - 8)
        # Only keep columns that have at least some visible area outside the wheel
        # Check if top or bottom of column is outside the circle
        dist_from_center_x = abs(x - center)
        if dist_from_center_x < radius - 10:
            # Column is under the wheel horizontally - skip unless near edges
            continue
        columns.append({
            "x": x,
            "speed": random.uniform(1.5, 4.0),  # chars per frame
            "offset": random.uniform(0, size),  # starting offset
            "length": random.randint(4, 10),  # trail length in chars
            "glyphs": _make_column_glyphs(glyph_count),
        })

    # Add a few columns in the very corners (always visible)
    for x in [3, 10, 18, size - 20, size - 12, size - 5]:
        columns.append({
            "x": x,
            "speed": random.uniform(1.5, 3.5),
            "offset": random.uniform(0, size),
            "length": random.randint(3, 8),
            "glyphs": _make_column_glyphs(glyph_count),
        })

    return columns


def _draw_matrix_rain(draw: ImageDraw.Draw, size: int, frame_idx: int, phase: dict) -> None:
    """Draw matrix rain in the dark background areas, fading with phase."""
    if not phase["visible"]:
        return

    center = size // 2
    radius = size // 2 - 30
    rain_font = _get_cached_font(max(7, size // 55), "rain")
    char_h = max(8, size // 50)
    columns = _get_rain_columns(size)
    fade = phase["fade"]

    for col in columns:
        x = col["x"]
        head_y = (col["offset"] + frame_idx * col["speed"] * char_h) % (size + col["length"] * char_h)

        for j in range(col["length"]):
            cy = int(head_y - j * char_h)
            if cy < 0 or cy >= size:
                continue

            # Skip if inside the wheel circle
            dx = x - center
            dy = cy - center
            dist = math.sqrt(dx * dx + dy * dy)
            if dist < radius + 8:
                continue

            if j == 0:
                alpha = 90
                color = (50, 200, 50, alpha)
            else:
                alpha = max(15, 70 - j * 10)
                color = (0, 100 + max(0, 40 - j * 8), 0, alpha)

            # Apply fade
            alpha = int(alpha * (1.0 - fade))
            alpha = max(0, min(255, alpha))
            if alpha == 0:
                continue
            color = (color[0], color[1], color[2], alpha)

            glyph_idx = (int(head_y / char_h) + j) % len(col["glyphs"])
            glyph = col["glyphs"][glyph_idx]
            draw.text((x, cy), glyph, fill=color, font=rain_font)


def _get_static_overlay(size: int) -> Image.Image:
    """Get cached static overlay with pointer and center elements."""
    if size not in _CACHED_STATIC_OVERLAY:
        _CACHED_STATIC_OVERLAY[size] = _create_static_overlay(size)
    return _CACHED_STATIC_OVERLAY[size]


def _create_static_overlay(size: int) -> Image.Image:
    """Create the static overlay (pointer, center circle, text) once."""
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)

    center = size // 2
    radius = size // 2 - 30
    inner_radius = radius // 3

    title_font = _get_cached_font(max(10, size // 40), "title")

    # Draw center circle
    draw.ellipse(
        [
            center - inner_radius,
            center - inner_radius,
            center + inner_radius,
            center + inner_radius,
        ],
        fill="#2c3e50",
        outline="#f1c40f",
        width=4,
    )

    # Center text
    for i, text in enumerate(["WHEEL OF", "FORTUNE"]):
        bbox = draw.textbbox((0, 0), text, font=title_font)
        text_w = bbox[2] - bbox[0]
        draw.text(
            (center - text_w / 2, center - 14 + i * 18),
            text,
            fill="#f1c40f",
            font=title_font,
        )

    # Draw pointer - proportional to radius/size
    pointer_y = center - radius - 5
    pw = int(size * 0.036)  # pointer half-width (~18 at 500)
    ph_tip = int(size * 0.07)  # pointer tip depth (~35 at 500)
    ph_notch = int(size * 0.036)  # pointer notch depth (~18 at 500)
    pw_inner = int(size * 0.012)  # inner notch width (~6 at 500)
    ph_top = int(size * 0.016)  # top offset (~8 at 500)

    pointer_points = [
        (center, pointer_y + ph_tip),
        (center - pw, pointer_y - ph_top),
        (center - pw_inner, pointer_y + 2),
        (center, pointer_y + ph_notch),
        (center + pw_inner, pointer_y + 2),
        (center + pw, pointer_y - ph_top),
    ]
    draw.polygon(pointer_points, fill="#e74c3c", outline="#ffffff", width=2)

    return img


# Base wheel wedge configuration: (label, base_value, color)
# 24 wedges at 15 degrees each (dropped 35 and 45 from original 26 for legibility)
# BANKRUPT value will be adjusted based on WHEEL_TARGET_EV
# Wedges ordered by color: black/gray, green, blue, purple, orange (BOLT here), red, gold
_BASE_WHEEL_WEDGES = [
    ("BANKRUPT", -100, "#1a1a1a"),
    ("BANKRUPT", -100, "#1a1a1a"),
    ("LOSE", 0, "#4a4a4a"),
    ("5", 5, "#2d5a27"),
    ("5", 5, "#2d5a27"),
    ("10", 10, "#3d7a37"),
    ("10", 10, "#3d7a37"),
    ("15", 15, "#4d9a47"),
    ("15", 15, "#4d9a47"),
    ("20", 20, "#5dba57"),
    ("20", 20, "#5dba57"),
    ("25", 25, "#3498db"),
    ("BLUE", "BLUE_SHELL", "#3498db"),
    ("30", 30, "#2980b9"),
    ("40", 40, "#9b59b6"),
    ("50", 50, "#7d3c98"),
    ("50", 50, "#7d3c98"),
    ("60", 60, "#e67e22"),
    ("BOLT", "LIGHTNING_BOLT", "#f39c12"),
    ("70", 70, "#d35400"),
    ("RED", "RED_SHELL", "#e74c3c"),
    ("80", 80, "#c0392b"),
    ("100", 100, "#f1c40f"),
    ("100", 100, "#f1c40f"),
]


_SPECIAL_WEDGE_EST_EVS: dict[str, float] = {}


def _load_special_wedge_evs() -> None:
    """Load estimated EVs for special wedges from config (deferred to avoid circular import)."""
    if _SPECIAL_WEDGE_EST_EVS:
        return
    from config import (
        WHEEL_BLUE_SHELL_EST_EV,
        WHEEL_LIGHTNING_BOLT_EST_EV,
        WHEEL_RED_SHELL_EST_EV,
    )
    _SPECIAL_WEDGE_EST_EVS.update({
        "RED_SHELL": WHEEL_RED_SHELL_EST_EV,
        "BLUE_SHELL": WHEEL_BLUE_SHELL_EST_EV,
        "LIGHTNING_BOLT": WHEEL_LIGHTNING_BOLT_EST_EV,
    })


def _calculate_adjusted_wedges(target_ev: float) -> list[tuple[str, int | str, str]]:
    """
    Calculate wheel wedges with BANKRUPT value adjusted to hit target EV.

    The BANKRUPT penalty is adjusted so that:
    sum(all_values) / num_wedges = target_ev

    BANKRUPT is capped at -1 minimum (can never be positive or zero).
    Special wedges (RED_SHELL, BLUE_SHELL, LIGHTNING_BOLT) use configurable
    estimated EVs so the BANKRUPT value compensates for their impact.
    """
    _load_special_wedge_evs()
    num_wedges = len(_BASE_WHEEL_WEDGES)

    # Calculate sum of non-bankrupt, non-special values (integers only)
    non_bankrupt_sum = sum(
        v for _, v, _ in _BASE_WHEEL_WEDGES
        if isinstance(v, int) and v >= 0
    )

    # Sum estimated EVs for special wedges
    special_ev_sum = sum(
        _SPECIAL_WEDGE_EST_EVS.get(v, 0.0)
        for _, v, _ in _BASE_WHEEL_WEDGES
        if isinstance(v, str)
    )

    # Count bankrupt wedges (negative integers)
    num_bankrupt = sum(
        1 for _, v, _ in _BASE_WHEEL_WEDGES
        if isinstance(v, int) and v < 0
    )

    # target_sum = non_bankrupt_sum + special_ev_sum + (num_bankrupt * bankrupt_value)
    # bankrupt_value = (target_sum - non_bankrupt_sum - special_ev_sum) / num_bankrupt
    target_sum = target_ev * num_wedges
    if num_bankrupt > 0:
        bankrupt_value = int((target_sum - non_bankrupt_sum - special_ev_sum) / num_bankrupt)
        # BANKRUPT must always be negative (minimum -1)
        bankrupt_value = min(bankrupt_value, -1)
    else:
        bankrupt_value = -100  # Fallback

    # Build adjusted wedges
    adjusted = []
    for label, value, color in _BASE_WHEEL_WEDGES:
        if isinstance(value, str):
            adjusted.append((label, value, color))
        elif value < 0:  # BANKRUPT
            # Update label to show actual value
            adjusted.append((str(bankrupt_value), bankrupt_value, color))
        else:
            adjusted.append((label, value, color))

    return adjusted


# Calculate wedges based on configured target EV
WHEEL_WEDGES = _calculate_adjusted_wedges(WHEEL_TARGET_EV)


def hex_to_rgb(hex_color: str) -> tuple[int, int, int]:
    """Convert hex color to RGB tuple."""
    hex_color = hex_color.lstrip("#")
    return tuple(int(hex_color[i : i + 2], 16) for i in (0, 2, 4))


def _get_wheel_face(size: int) -> Image.Image:
    """Get cached wheel face sprite (pieslices + glow ring, no text)."""
    if size not in _CACHED_WHEEL_FACE:
        _CACHED_WHEEL_FACE[size] = _create_wheel_face(size)
    return _CACHED_WHEEL_FACE[size]


def _create_wheel_face(size: int) -> Image.Image:
    """
    Create the wheel face sprite once: glow ring and colored wedge pieslices.

    Text is NOT included here - it's drawn horizontally per-frame after rotation
    so labels stay readable at every angle.
    """
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)

    center = size // 2
    radius = size // 2 - 30

    num_wedges = len(WHEEL_WEDGES)
    angle_per_wedge = 360 / num_wedges

    # Draw outer glow ring (part of face so it rotates with wheel)
    for glow in range(5, 0, -1):
        glow_radius = radius + glow * 3
        draw.ellipse(
            [
                center - glow_radius,
                center - glow_radius,
                center + glow_radius,
                center + glow_radius,
            ],
            outline=(255, 215, 0, 30 - glow * 5),
            width=2,
        )

    # Draw wedges
    for i, (label, value, color) in enumerate(WHEEL_WEDGES):
        start_angle = i * angle_per_wedge - 90
        end_angle = start_angle + angle_per_wedge

        draw.pieslice(
            [center - radius, center - radius, center + radius, center + radius],
            start_angle,
            end_angle,
            fill=color,
            outline="#ffffff",
            width=2,
        )

    return img


_CACHED_SHELL_ICONS: dict[tuple[str, int], Image.Image] = {}


def _get_shell_icon(shell_type: str, icon_size: int) -> Image.Image:
    """Get a cached Mario Kart-style shell icon."""
    key = (shell_type, icon_size)
    if key not in _CACHED_SHELL_ICONS:
        _CACHED_SHELL_ICONS[key] = _draw_shell_icon(shell_type, icon_size)
    return _CACHED_SHELL_ICONS[key]


def _draw_shell_icon(shell_type: str, s: int) -> Image.Image:
    """Draw an 8-bit pixel-art style Mario Kart shell icon.

    Red shell: blocky side-view koopa shell with bold stripes
    Blue shell: blocky top-down spiny shell with rectangular wings
    """
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)

    cx, cy = s // 2, s // 2
    r = s // 2 - 2
    outline_w = max(2, s // 10)

    if shell_type == "BLUE_SHELL":
        # Blue spiny shell - top-down view, blocky 8-bit style
        body_color = (52, 152, 219)       # #3498db
        dark_color = (41, 128, 185)        # #2980b9
        spike_color = (255, 255, 255)
        outline_color = (20, 60, 100)

        # Blocky body (rounded rectangle instead of ellipse)
        body_r = max(2, r // 4)
        draw.rounded_rectangle(
            [cx - r, cy - r, cx + r, cy + r],
            radius=body_r, fill=body_color, outline=outline_color, width=outline_w,
        )

        # 4 blocky triangular spikes with black outlines
        spike_len = int(r * 0.55)
        spike_w = int(r * 0.35)
        for angle_deg in [0, 90, 180, 270]:
            a = math.radians(angle_deg)
            perp = a + math.pi / 2
            # Base points on the body edge
            base_x = cx + int(r * 0.65 * math.cos(a))
            base_y = cy + int(r * 0.65 * math.sin(a))
            tip_x = cx + int((r + spike_len) * math.cos(a))
            tip_y = cy + int((r + spike_len) * math.sin(a))
            p1 = (base_x + int(spike_w * math.cos(perp)), base_y + int(spike_w * math.sin(perp)))
            p2 = (base_x - int(spike_w * math.cos(perp)), base_y - int(spike_w * math.sin(perp)))
            # Spike outline then fill
            draw.polygon([(tip_x, tip_y), p1, p2], fill=spike_color, outline=outline_color, width=max(1, outline_w // 2))

        # Rectangular wings with black outlines
        wing_w = int(r * 0.8)
        wing_h = int(r * 0.5)
        # Left wing
        draw.rectangle(
            [cx - r - wing_w, cy - wing_h // 2, cx - r + 2, cy + wing_h // 4],
            fill=(255, 255, 255, 220), outline=outline_color, width=max(1, outline_w // 2),
        )
        # Right wing
        draw.rectangle(
            [cx + r - 2, cy - wing_h // 2, cx + r + wing_w, cy + wing_h // 4],
            fill=(255, 255, 255, 220), outline=outline_color, width=max(1, outline_w // 2),
        )

        # Inner darker rectangle for shell ridge
        inner_r = int(r * 0.5)
        draw.rounded_rectangle(
            [cx - inner_r, cy - inner_r, cx + inner_r, cy + inner_r],
            radius=max(1, body_r // 2), fill=dark_color,
        )

        # Central square dot instead of circle
        dot_s = max(2, r // 3)
        draw.rectangle(
            [cx - dot_s, cy - dot_s, cx + dot_s, cy + dot_s],
            fill=spike_color,
        )

    elif shell_type == "LIGHTNING_BOLT":
        # Lightning bolt - 8-bit pixel art zigzag bolt
        body_color = (243, 156, 18)       # #f39c12 orange-yellow
        highlight_color = (241, 196, 15)  # #f1c40f bright yellow
        outline_color = (125, 102, 8)     # #7d6608 dark outline

        # Draw bolt as a polygon zigzag shape
        # Scale points relative to icon size
        bolt_points = [
            (cx + int(r * 0.15), cy - r),
            (cx + int(r * 0.55), cy - r),
            (cx + int(r * 0.10), cy - int(r * 0.05)),
            (cx + int(r * 0.50), cy - int(r * 0.05)),
            (cx - int(r * 0.15), cy + r),
            (cx - int(r * 0.15), cy + int(r * 0.10)),
            (cx + int(r * 0.35), cy + int(r * 0.10)),
            (cx - int(r * 0.10), cy + r),
            (cx + int(r * 0.05), cy + int(r * 0.15)),
            (cx - int(r * 0.35), cy + int(r * 0.15)),
        ]
        # Simplified bolt polygon (6 points for clean zigzag)
        bolt_points = [
            (cx + int(r * 0.1), cy - r),           # top-left
            (cx + int(r * 0.6), cy - r),            # top-right
            (cx + int(r * 0.05), cy - int(r * 0.1)), # mid-left notch
            (cx + int(r * 0.55), cy - int(r * 0.1)), # mid-right
            (cx - int(r * 0.2), cy + r),            # bottom-left tip
            (cx + int(r * 0.1), cy + int(r * 0.05)), # bottom notch
        ]
        # Draw outline first (thicker), then fill
        draw.polygon(bolt_points, fill=body_color, outline=outline_color, width=outline_w)

        # Inner highlight stripe (smaller bolt shape offset slightly)
        hl_points = [
            (cx + int(r * 0.2), cy - int(r * 0.7)),
            (cx + int(r * 0.45), cy - int(r * 0.7)),
            (cx + int(r * 0.12), cy - int(r * 0.05)),
            (cx + int(r * 0.4), cy - int(r * 0.05)),
            (cx - int(r * 0.05), cy + int(r * 0.6)),
            (cx + int(r * 0.15), cy + int(r * 0.1)),
        ]
        draw.polygon(hl_points, fill=highlight_color)

    else:
        # Red shell - side view, blocky 8-bit NES koopa shell
        body_color = (231, 76, 60)         # #e74c3c
        dark_color = (192, 57, 43)         # #c0392b
        belly_color = (255, 235, 200)      # cream
        outline_color = (80, 20, 15)

        # Shell dome body (rounded rectangle for chunky pixel feel)
        body_r = max(2, r // 3)
        draw.rounded_rectangle(
            [cx - r, cy - r, cx + r, cy + int(r * 0.3)],
            radius=body_r, fill=body_color, outline=outline_color, width=outline_w,
        )

        # Flat cream belly at the bottom
        belly_top = cy + int(r * 0.3)
        draw.rectangle(
            [cx - r + 1, belly_top, cx + r - 1, cy + r],
            fill=belly_color, outline=outline_color, width=max(1, outline_w // 2),
        )

        # 3 bold vertical stripe rectangles on the dome
        stripe_w = max(2, r // 4)
        stripe_top = cy - r + outline_w + 1
        stripe_bottom = belly_top - 1
        if stripe_bottom > stripe_top:
            for offset in [-1, 0, 1]:
                stripe_x = cx + int(offset * r * 0.4) - stripe_w // 2
                draw.rectangle(
                    [stripe_x, stripe_top,
                     stripe_x + stripe_w, stripe_bottom],
                    fill=dark_color,
                )

        # Square specular highlight (not ellipse)
        hl_s = max(2, r // 3)
        draw.rectangle(
            [cx - int(r * 0.45) - hl_s // 2, cy - int(r * 0.45) - hl_s // 2,
             cx - int(r * 0.45) + hl_s // 2, cy - int(r * 0.45) + hl_s // 2],
            fill=(255, 150, 140, 200),
        )

        # Small rectangular "feet" nubs at bottom corners
        nub_w = max(2, r // 4)
        nub_h = max(2, r // 5)
        # Left nub
        draw.rectangle(
            [cx - r + 2, cy + r, cx - r + 2 + nub_w, cy + r + nub_h],
            fill=body_color, outline=outline_color, width=max(1, outline_w // 3),
        )
        # Right nub
        draw.rectangle(
            [cx + r - 2 - nub_w, cy + r, cx + r - 2, cy + r + nub_h],
            fill=body_color, outline=outline_color, width=max(1, outline_w // 3),
        )

    return img


def _draw_wedge_labels(img: Image.Image, draw: ImageDraw.Draw, size: int, rotation: float) -> None:
    """Draw horizontal text labels on each wedge after rotation.

    For shell wedges, draws a Mario Kart shell icon next to the text.
    Long labels auto-shrink to fit within their wedge arc width.
    """
    center = size // 2
    radius = size // 2 - 30
    num_wedges = len(WHEEL_WEDGES)
    angle_per_wedge = 360 / num_wedges

    # Base font size and max arc width available at the text radius
    base_font_size = max(12, size // 30)
    text_radius_frac = 0.68
    arc_width = 2 * math.pi * (radius * text_radius_frac) * (angle_per_wedge / 360)
    # Leave some padding inside the arc
    max_label_width = arc_width * 0.85

    for i, (label, value, _color) in enumerate(WHEEL_WEDGES):
        # Determine display text
        if isinstance(value, str) or value <= 0:
            text = label
        else:
            text = str(value)

        is_shell = isinstance(value, str)

        # Dynamic font sizing: shrink if text + icon would exceed wedge arc
        font_size = base_font_size
        font = _get_cached_font(font_size, "small", bold=True)
        bbox = draw.textbbox((0, 0), text, font=font)
        text_w = bbox[2] - bbox[0]
        text_h = bbox[3] - bbox[1]
        icon_size = int(text_h * 1.8) if is_shell else 0
        icon_gap = icon_size + 3 if is_shell else 0
        total_w = text_w + icon_gap

        # Shrink font until it fits (minimum 8px)
        while total_w > max_label_width and font_size > 8:
            font_size -= 1
            font = _get_cached_font(font_size, "small", bold=True)
            bbox = draw.textbbox((0, 0), text, font=font)
            text_w = bbox[2] - bbox[0]
            text_h = bbox[3] - bbox[1]
            icon_size = int(text_h * 1.8) if is_shell else 0
            icon_gap = icon_size + 3 if is_shell else 0
            total_w = text_w + icon_gap

        # Position along the rotated wedge radial
        mid_angle_deg = i * angle_per_wedge + rotation - 90 + angle_per_wedge / 2
        mid_angle_rad = math.radians(mid_angle_deg)
        text_radius = radius * text_radius_frac
        text_cx = center + text_radius * math.cos(mid_angle_rad)
        text_cy = center + text_radius * math.sin(mid_angle_rad)

        tx = text_cx - total_w / 2
        ty = text_cy - text_h / 2

        if is_shell:
            shell_icon = _get_shell_icon(value, icon_size)
            icon_x = int(tx)
            icon_y = int(text_cy - icon_size / 2)
            img.paste(shell_icon, (icon_x, icon_y), shell_icon)

        # Text shadow + main text
        text_x = tx + icon_gap
        text_y = ty
        draw.text((text_x + 1, text_y + 1), text, fill="#000000", font=font)
        draw.text((text_x, text_y), text, fill="#ffffff", font=font)


def wheel_image_to_bytes(img: Image.Image) -> io.BytesIO:
    """Convert PIL Image to bytes buffer for Discord."""
    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer


def get_wedge_at_index(idx: int) -> tuple[str, int, str]:
    """Get wedge info (label, value, color) at given index."""
    return WHEEL_WEDGES[idx % len(WHEEL_WEDGES)]


# Block characters for glitch transition
_GLITCH_BLOCKS = list("\u2588\u2593\u2592\u2591")


def _draw_terminal_shell(draw: ImageDraw.Draw, size: int, frame_idx: int, display_name: str) -> None:
    """Draw the terminal shell prompt with glitch transition from status bar.

    Frames 38-43: Glitch transition - old status bar text corrupts into terminal prompt.
    Frames 44+: Steady-state terminal prompt with blinking cursor.
    """
    bar_font = _get_cached_font(max(7, size // 55), "statusbar")
    y = size - 13

    session_hex = format(hash(display_name) & 0xFFFF, "04x")
    old_text = f"JOPA-T/v3.7 \u2502 {display_name} \u2502 #{session_hex}"
    new_text = f"{display_name}@jopa-t:~$ "

    if frame_idx < 44:
        # Glitch transition (frames 38-43)
        glitch_progress = (frame_idx - 38) / 5  # 0.0 -> 1.0 over 6 frames
        rng = random.Random(frame_idx * 37 + 7)

        # Determine max length to render
        max_len = max(len(old_text), len(new_text))
        result_chars = []
        result_colors = []

        for ci in range(max_len):
            old_ch = old_text[ci] if ci < len(old_text) else " "
            new_ch = new_text[ci] if ci < len(new_text) else " "

            # Corruption spreads from edges inward
            edge_dist = min(ci, max_len - 1 - ci) if max_len > 1 else 0
            center_dist = edge_dist / (max_len / 2) if max_len > 0 else 0
            # Earlier frames corrupt edges first, later frames corrupt center
            corrupt_threshold = glitch_progress * 1.5 - center_dist * 0.5

            if rng.random() < corrupt_threshold:
                if glitch_progress > 0.6 and rng.random() < (glitch_progress - 0.4):
                    # New text emerging
                    result_chars.append(new_ch)
                    result_colors.append((55, 190, 55, 180))
                else:
                    # Garbled block character
                    result_chars.append(rng.choice(_GLITCH_BLOCKS))
                    # Color shifts: dim green -> bright flash at midpoint -> terminal green
                    if glitch_progress < 0.4:
                        result_colors.append((55, 90 + int(rng.random() * 60), 55, 160))
                    elif glitch_progress < 0.6:
                        brightness = 150 + int(rng.random() * 105)
                        result_colors.append((brightness, brightness, brightness, 200))
                    else:
                        result_colors.append((55, 140 + int(rng.random() * 50), 55, 170))
            else:
                # Original text still showing
                result_chars.append(old_ch)
                result_colors.append((55, 90, 55, 140))

        # Draw character by character (each may have different color)
        x_pos = 4
        for ch, color in zip(result_chars, result_colors):
            draw.text((x_pos, y), ch, fill=color, font=bar_font)
            bbox = draw.textbbox((0, 0), ch, font=bar_font)
            x_pos += bbox[2] - bbox[0]
    else:
        # Steady state (frames 44+) - solid cursor
        prompt = new_text + "\u2588"
        draw.text((4, y), prompt, fill=(55, 190, 55, 180), font=bar_font)


def create_wheel_frame_for_gif(
    size: int, rotation: float, selected_idx: int | None = None,
    display_name: str | None = None, frame_idx: int = 0,
) -> Image.Image:
    """
    Create a single wheel frame optimized for GIF animation.

    Uses cached wheel face sprite rotated to the desired angle, then composites
    the static overlay (pointer, center circle) on top.
    """
    img = Image.new("RGBA", (size, size), (30, 30, 35, 255))

    center = size // 2
    radius = size // 2 - 30

    # Get cached wheel face (pieslices only) and rotate it
    face = _get_wheel_face(size)
    rotated_face = face.rotate(-rotation, center=(center, center), resample=Image.BICUBIC)

    # Composite rotated face onto background
    img = Image.alpha_composite(img, rotated_face)

    # Draw winner highlight on top of rotated face (if selected)
    draw = ImageDraw.Draw(img)
    if selected_idx is not None:
        num_wedges = len(WHEEL_WEDGES)
        angle_per_wedge = 360 / num_wedges
        win_angle = selected_idx * angle_per_wedge + rotation - 90

        # Brighten the winning wedge by drawing a semi-transparent overlay
        _, _, win_color = WHEEL_WEDGES[selected_idx]
        rgb = hex_to_rgb(win_color)
        bright = tuple(min(255, c + 120) for c in rgb)
        draw.pieslice(
            [center - radius, center - radius, center + radius, center + radius],
            win_angle,
            win_angle + angle_per_wedge,
            fill=(*bright, 180),
            outline="#ffff00",
            width=12,
        )

        # Glow arcs
        for glow_offset in range(3, 0, -1):
            glow_width = 12 + glow_offset * 4
            glow_alpha = 200 - glow_offset * 50
            draw.arc(
                [center - radius - 2, center - radius - 2,
                 center + radius + 2, center + radius + 2],
                win_angle, win_angle + angle_per_wedge,
                fill=(255, 255, 0, glow_alpha),
                width=glow_width,
            )

    # Draw matrix rain in dark corners (behind wheel labels, above background)
    if display_name:
        phase = _get_rain_phase(frame_idx)
        _draw_matrix_rain(draw, size, frame_idx, phase)

    # Draw horizontal text labels on each wedge (after rotation so they stay readable)
    _draw_wedge_labels(img, draw, size, rotation)

    # Composite cached static overlay (center circle, text, pointer)
    static_overlay = _get_static_overlay(size)
    img = Image.alpha_composite(img, static_overlay)

    # Status bar / terminal shell at bottom
    if display_name:
        draw = ImageDraw.Draw(img)
        if frame_idx >= 38:
            _draw_terminal_shell(draw, size, frame_idx, display_name)
        else:
            bar_font = _get_cached_font(max(7, size // 55), "statusbar")
            session_hex = format(hash(display_name) & 0xFFFF, "04x")
            bar_text = f"JOPA-T/v3.7 \u2502 {display_name} \u2502 #{session_hex}"
            draw.text((4, size - 13), bar_text, fill=(55, 90, 55, 140), font=bar_font)

    return img


def create_wheel_gif(target_idx: int, size: int = 500, display_name: str | None = None) -> io.BytesIO:
    """
    Create an animated GIF of the wheel spinning and landing on target_idx.

    Uses physics-inspired animation: smooth deceleration with a randomized
    "near-miss" moment where the wheel almost stops before the target,
    then creeps forward to the final position - like a real wheel fighting
    against friction.

    Args:
        target_idx: Index of wedge to land on
        size: Image size in pixels
        display_name: User's Discord display name for JOPA-T terminal prompt

    Returns:
        BytesIO buffer containing the GIF data
    """
    import random

    frames = []
    durations = []

    num_wedges = len(WHEEL_WEDGES)
    angle_per_wedge = 360 / num_wedges

    # Calculate final rotation to land on target wedge
    final_rotation = -(target_idx * angle_per_wedge + angle_per_wedge / 2)
    total_spin = 360 * 6 + final_rotation  # 6 full rotations for drama

    # Animation with randomized "near-miss" physics:
    # The wheel spins, slows down, almost stops some distance before target,
    # then creeps forward to land on the final position

    num_frames = 100

    # Phase boundaries (scaled from 150 to 100 frames)
    fast_end = 40       # End of fast spin (frames 0-39)
    medium_end = 60     # End of medium spin (frames 40-59)
    slow_end = 83       # End of slow crawl (frames 60-82)
    creep_end = 98      # End of creep to final (frames 83-97)
    # Frame 98 is second-to-last, frame 99 is final

    # Ending styles with varied physics (20 items for easy % math)
    ending_styles = [
        "clean",        # 5% - Stops exactly on target
        "full_stop",    # 20% - Stops short, dramatic pause, then creeps
        "full_stop",
        "full_stop",
        "full_stop",
        "smooth",       # 20% - Gradual deceleration into target
        "smooth",
        "smooth",
        "smooth",
        "overshoot",    # 15% - Goes past, settles back
        "overshoot",
        "overshoot",
        "stutter",      # 15% - Mini-pauses as it crawls
        "stutter",
        "stutter",
        "tease",        # 10% - Almost stops on adjacent wedge, then moves
        "tease",
        "double_pump",  # 10% - Slows, tiny acceleration, then final slow
        "double_pump",
        "reverse",      # 5% - Spins forward, then REVERSES, then forward to target
    ]
    ending_style = random.choice(ending_styles)

    # Configure physics based on ending style
    if ending_style == "clean":
        near_miss_wedges = 0
        full_stop_duration = 0
    elif ending_style == "full_stop":
        near_miss_wedges = random.uniform(0.4, 2.5)
        full_stop_duration = random.randint(600, 1800)
    elif ending_style == "smooth":
        near_miss_wedges = random.uniform(0.1, 1.5)
        full_stop_duration = 0
    elif ending_style == "overshoot":
        near_miss_wedges = random.uniform(-1.2, -0.3)  # Negative = past target
        full_stop_duration = random.randint(400, 900)
    elif ending_style == "stutter":
        near_miss_wedges = random.uniform(0.6, 2.5)
        full_stop_duration = random.randint(0, 300)
    elif ending_style == "tease":
        # Stop on adjacent wedge briefly, then move to target
        near_miss_wedges = random.choice([-1, 1]) * random.uniform(0.9, 1.5)
        full_stop_duration = random.randint(800, 2000)
    elif ending_style == "double_pump":
        near_miss_wedges = random.uniform(0.3, 1.8)
        full_stop_duration = random.randint(200, 500)
    else:  # reverse - the unhinged one
        near_miss_wedges = random.uniform(1.0, 3.0)  # Go past, then reverse back
        full_stop_duration = random.randint(300, 600)

    near_miss_rotation = total_spin - (angle_per_wedge * near_miss_wedges)

    # Timing parameters
    creep_base_duration = random.randint(100, 180)
    creep_speed_factor = random.uniform(0.9, 1.2)

    # CHAOS MODE: precompute wild direction changes
    if ending_style == "reverse":
        # Generate chaotic keyframes: forward, REVERSE, forward, reverse, settle
        chaos_keyframes = []
        pos = 0
        # Initial forward burst
        pos += 360 * random.uniform(2.5, 4.0)
        chaos_keyframes.append(pos)
        # HARD REVERSE
        pos -= 360 * random.uniform(1.5, 2.5)
        chaos_keyframes.append(pos)
        # Forward again!
        pos += 360 * random.uniform(1.0, 2.0)
        chaos_keyframes.append(pos)
        # Another reverse!
        pos -= 360 * random.uniform(0.5, 1.2)
        chaos_keyframes.append(pos)
        # Final push to target
        chaos_keyframes.append(total_spin)

    # Pre-render first frame to establish shared palette
    first_frame_rgba = create_wheel_frame_for_gif(size, 0, selected_idx=None, display_name=display_name, frame_idx=0)
    first_frame_rgb = first_frame_rgba.convert("RGB")
    palette_image = first_frame_rgb.convert("P", palette=Image.ADAPTIVE, colors=256)

    for i in range(num_frames):
        # Calculate rotation based on frame
        if ending_style == "reverse":
            # CHAOS: interpolate through wild keyframes
            chaos_progress = i / (num_frames - 1)
            num_segments = len(chaos_keyframes)
            segment_idx = min(int(chaos_progress * num_segments), num_segments - 1)
            segment_progress = (chaos_progress * num_segments) - segment_idx

            if segment_idx == 0:
                rotation = chaos_keyframes[0] * segment_progress
            elif segment_idx < num_segments:
                prev = chaos_keyframes[segment_idx - 1]
                curr = chaos_keyframes[segment_idx]
                # Snappy easing for that whiplash feel
                eased = 1 - pow(1 - segment_progress, 2)
                rotation = prev + (curr - prev) * eased
            else:
                rotation = total_spin
        elif i <= slow_end:
            # Main spin with quintic ease-out
            phase_progress = i / slow_end
            eased = 1 - pow(1 - phase_progress, 5)
            rotation = near_miss_rotation * eased
        elif ending_style == "double_pump" and i <= slow_end + 5:
            # Tiny acceleration burst
            base_rotation = near_miss_rotation
            pump_progress = (i - slow_end) / 5
            pump_amount = angle_per_wedge * 0.15 * math.sin(pump_progress * math.pi)
            rotation = base_rotation + pump_amount
        else:
            # Creep phase
            if ending_style == "double_pump":
                creep_start = slow_end + 5
            else:
                creep_start = slow_end
            creep_progress = (i - creep_start) / (creep_end - creep_start)
            creep_progress = min(1.0, max(0.0, creep_progress))
            creep_eased = 1 - pow(1 - creep_progress, 2)
            remaining = total_spin - near_miss_rotation
            rotation = near_miss_rotation + (remaining * creep_eased)

        is_final = i == num_frames - 1

        frame = create_wheel_frame_for_gif(
            size, rotation, selected_idx=target_idx if is_final else None,
            display_name=display_name, frame_idx=i,
        )

        # Quantize against shared palette for consistent colors across frames
        frame_rgb = frame.convert("RGB")
        frame_p = frame_rgb.quantize(palette=palette_image, dither=Image.Dither.FLOYDSTEINBERG)
        frames.append(frame_p)

        # Timing: variable animation duration
        if i < 20:
            durations.append(30)       # Very fast: 30ms
        elif i < fast_end:
            durations.append(40)       # Fast: 40ms
        elif i < medium_end:
            durations.append(65)       # Medium: 65ms
        elif i < slow_end:
            durations.append(100)      # Slow: 100ms
        elif i == slow_end:
            # Near-miss moment with dramatic pause
            base = int(creep_base_duration * creep_speed_factor)
            durations.append(base + full_stop_duration)
        elif i < creep_end:
            creep_idx = i - slow_end
            creep_frames = creep_end - slow_end
            slowdown = 1 + (0.7 * creep_idx / creep_frames)
            duration = int(creep_base_duration * creep_speed_factor * slowdown)
            # Style-specific timing adds variability
            if ending_style == "stutter" and creep_idx % 4 == 0:
                duration += random.randint(200, 500)
            elif ending_style == "tease" and creep_idx < 4:
                duration += random.randint(150, 350)
            elif ending_style == "full_stop" and creep_idx > creep_frames - 3:
                duration += random.randint(100, 250)  # Extra suspense at end
            durations.append(duration)
        else:
            durations.append(60000)    # Hold final for 60s

    buffer = io.BytesIO()
    frames[0].save(
        buffer,
        format="GIF",
        save_all=True,
        append_images=frames[1:],
        duration=durations,
        loop=1,  # Play once, hold on final frame
    )
    buffer.seek(0)
    return buffer


def create_explosion_gif(size: int = 500, display_name: str | None = None) -> io.BytesIO:
    """
    Create an animated GIF of the wheel exploding.

    The wheel spins briefly, then EXPLODES with particles, fire, and smoke.
    A "67 JC" appears in the aftermath with an apology.

    Args:
        size: Image size in pixels

    Returns:
        BytesIO buffer containing the GIF data
    """
    frames = []
    durations = []

    center = size // 2
    radius = size // 2 - 30
    scale = size / 400.0  # Scale factor for pixel values calibrated at 400px

    # Phase 1: Normal spin for ~1 second (builds tension)
    spin_frames = 20
    for i in range(spin_frames):
        rotation = i * 25  # Fast spin
        frame = create_wheel_frame_for_gif(size, rotation, selected_idx=None, display_name=display_name, frame_idx=i)
        frame_p = frame.convert("RGB").convert("P", palette=Image.ADAPTIVE, colors=256)
        frames.append(frame_p)
        durations.append(50)

    # Phase 2: Wheel starts shaking/glitching (something's wrong...)
    shake_frames = 15
    base_rotation = spin_frames * 25
    for i in range(shake_frames):
        # Increasingly violent shaking
        shake_intensity = int((i + 1) * 3 * scale)
        shake_x = random.randint(-shake_intensity, shake_intensity)
        shake_y = random.randint(-shake_intensity, shake_intensity)

        frame = create_wheel_frame_for_gif(size, base_rotation + random.randint(-5, 5), display_name=display_name, frame_idx=spin_frames + i)

        # Apply shake by creating offset composite
        shaken = Image.new("RGBA", (size, size), (30, 30, 35, 255))
        shaken.paste(frame, (shake_x, shake_y))

        # Add warning red tint that intensifies
        red_overlay = Image.new("RGBA", (size, size), (255, 0, 0, int(20 + i * 8)))
        shaken = Image.alpha_composite(shaken.convert("RGBA"), red_overlay)

        frame_p = shaken.convert("RGB").convert("P", palette=Image.ADAPTIVE, colors=256)
        frames.append(frame_p)
        durations.append(60 + i * 10)  # Slowing down before explosion

    # Phase 3: THE EXPLOSION
    explosion_frames = 25

    # Pre-generate explosion particles
    num_particles = 80
    particles = []
    for _ in range(num_particles):
        angle = random.uniform(0, 2 * math.pi)
        speed = random.uniform(3, 15) * scale
        particle = {
            "x": float(center),
            "y": float(center),
            "vx": math.cos(angle) * speed,
            "vy": math.sin(angle) * speed,
            "size": random.randint(int(4 * scale), int(20 * scale)),
            "color": random.choice([
                (255, 100, 0),    # Orange fire
                (255, 200, 0),    # Yellow fire
                (255, 50, 0),     # Red fire
                (200, 200, 200),  # Smoke/debris
                (100, 100, 100),  # Dark smoke
                (255, 255, 100),  # Bright spark
            ]),
            "decay": random.uniform(0.85, 0.95),
        }
        particles.append(particle)

    # Generate wheel fragments
    num_fragments = 12
    fragments = []
    for i in range(num_fragments):
        angle = (i / num_fragments) * 2 * math.pi + random.uniform(-0.2, 0.2)
        speed = random.uniform(5, 12) * scale
        fragments.append({
            "x": float(center),
            "y": float(center),
            "vx": math.cos(angle) * speed,
            "vy": math.sin(angle) * speed,
            "rotation": random.uniform(0, 360),
            "rot_speed": random.uniform(-20, 20),
            "size": random.randint(int(20 * scale), int(50 * scale)),
            "color": random.choice(["#e74c3c", "#f1c40f", "#3498db", "#2ecc71", "#9b59b6"]),
        })

    for frame_idx in range(explosion_frames):
        img = Image.new("RGBA", (size, size), (30, 30, 35, 255))
        draw = ImageDraw.Draw(img)

        # Initial flash (first few frames)
        if frame_idx < 3:
            flash_alpha = 255 - frame_idx * 80
            flash = Image.new("RGBA", (size, size), (255, 255, 200, flash_alpha))
            img = Image.alpha_composite(img, flash)
            draw = ImageDraw.Draw(img)

        # Draw expanding shockwave rings
        if frame_idx < 15:
            for ring in range(3):
                ring_radius = int((frame_idx + 1) * 15 * scale + ring * 30 * scale)
                ring_alpha = max(0, 200 - frame_idx * 15 - ring * 40)
                if ring_alpha > 0 and ring_radius < size:
                    draw.ellipse(
                        [center - ring_radius, center - ring_radius,
                         center + ring_radius, center + ring_radius],
                        outline=(255, 200, 100, ring_alpha),
                        width=max(1, 4 - ring),
                    )

        # Update and draw fragments
        for frag in fragments:
            frag["x"] += frag["vx"]
            frag["y"] += frag["vy"]
            frag["vy"] += 0.3 * scale  # Gravity
            frag["rotation"] += frag["rot_speed"]
            frag["vx"] *= 0.97  # Air resistance

            # Draw fragment as a simple wedge shape
            fx, fy = int(frag["x"]), int(frag["y"])
            fsize = frag["size"]
            if 0 <= fx < size and 0 <= fy < size:
                # Draw a triangular fragment
                rot_rad = math.radians(frag["rotation"])
                points = []
                for j in range(3):
                    point_angle = rot_rad + j * (2 * math.pi / 3)
                    px = fx + fsize * math.cos(point_angle)
                    py = fy + fsize * math.sin(point_angle)
                    points.append((px, py))
                try:
                    rgb = hex_to_rgb(frag["color"])
                    alpha = max(0, 255 - frame_idx * 10)
                    draw.polygon(points, fill=(*rgb, alpha))
                except Exception:
                    pass

        # Update and draw particles
        for p in particles:
            p["x"] += p["vx"]
            p["y"] += p["vy"]
            p["vy"] += 0.2 * scale  # Gravity
            p["vx"] *= p["decay"]
            p["vy"] *= p["decay"]
            p["size"] = max(1, p["size"] * 0.95)

            px, py = int(p["x"]), int(p["y"])
            psize = int(p["size"])
            if 0 <= px < size and 0 <= py < size and psize > 0:
                alpha = max(0, 255 - frame_idx * 8)
                color = (*p["color"], alpha)
                draw.ellipse(
                    [px - psize, py - psize, px + psize, py + psize],
                    fill=color,
                )

        # Draw smoke clouds (appear after initial explosion)
        if frame_idx > 5:
            for smoke_idx in range(5):
                smoke_x = center + random.randint(int(-80 * scale), int(80 * scale))
                smoke_y = center + random.randint(int(-80 * scale), int(40 * scale)) - int(frame_idx * 2 * scale)
                smoke_size = int((30 + frame_idx * 2 + smoke_idx * 10) * scale)
                smoke_alpha = max(0, 100 - frame_idx * 3)
                if smoke_alpha > 0:
                    draw.ellipse(
                        [smoke_x - smoke_size, smoke_y - smoke_size,
                         smoke_x + smoke_size, smoke_y + smoke_size],
                        fill=(80, 80, 80, smoke_alpha),
                    )

        frame_p = img.convert("RGB").convert("P", palette=Image.ADAPTIVE, colors=256)
        frames.append(frame_p)
        durations.append(60 if frame_idx < 5 else 80)

    # Phase 4: Aftermath with "67 JC" and smoke clearing
    aftermath_frames = 20
    big_font = _get_cached_font(int(48 * scale), "explosion_big", bold=True)
    small_font = _get_cached_font(int(20 * scale), "explosion_small", bold=True)

    for frame_idx in range(aftermath_frames):
        img = Image.new("RGBA", (size, size), (30, 30, 35, 255))
        draw = ImageDraw.Draw(img)

        # Fading smoke
        smoke_alpha = max(0, 60 - frame_idx * 3)
        if smoke_alpha > 0:
            for _ in range(8):
                sx = center + random.randint(int(-100 * scale), int(100 * scale))
                sy = center + random.randint(int(-60 * scale), int(60 * scale)) - int(frame_idx * 3 * scale)
                ssize = random.randint(int(40 * scale), int(80 * scale))
                draw.ellipse(
                    [sx - ssize, sy - ssize, sx + ssize, sy + ssize],
                    fill=(60, 60, 60, smoke_alpha),
                )

        # Scattered debris on ground
        for _ in range(15):
            dx = center + random.randint(int(-150 * scale), int(150 * scale))
            dy = center + random.randint(int(50 * scale), int(120 * scale))
            dsize = random.randint(int(3 * scale), int(8 * scale))
            draw.ellipse(
                [dx - dsize, dy - dsize, dx + dsize, dy + dsize],
                fill=(100, 100, 100, 150),
            )

        # Draw the compensation message
        text_alpha = min(255, frame_idx * 25)

        # "67 JC" in gold
        jc_text = "+67 JC"
        bbox = draw.textbbox((0, 0), jc_text, font=big_font)
        text_w = bbox[2] - bbox[0]
        jc_x = center - text_w // 2
        jc_y = center - int(50 * scale)

        # Glow effect
        for glow in range(3, 0, -1):
            glow_alpha = min(text_alpha, 50)
            draw.text(
                (jc_x - glow, jc_y - glow), jc_text,
                fill=(255, 215, 0, glow_alpha), font=big_font
            )
            draw.text(
                (jc_x + glow, jc_y + glow), jc_text,
                fill=(255, 215, 0, glow_alpha), font=big_font
            )

        # Main text
        draw.text((jc_x + 2, jc_y + 2), jc_text, fill=(0, 0, 0, text_alpha), font=big_font)
        draw.text((jc_x, jc_y), jc_text, fill=(255, 215, 0, text_alpha), font=big_font)

        # Apology text
        sorry_text = "Sorry for the inconvenience!"
        bbox2 = draw.textbbox((0, 0), sorry_text, font=small_font)
        sorry_w = bbox2[2] - bbox2[0]
        sorry_x = center - sorry_w // 2
        sorry_y = center + int(20 * scale)

        draw.text((sorry_x + 1, sorry_y + 1), sorry_text, fill=(0, 0, 0, text_alpha), font=small_font)
        draw.text((sorry_x, sorry_y), sorry_text, fill=(255, 255, 255, text_alpha), font=small_font)

        frame_p = img.convert("RGB").convert("P", palette=Image.ADAPTIVE, colors=256)
        frames.append(frame_p)
        durations.append(100 if frame_idx < aftermath_frames - 1 else 60000)  # Hold final

    buffer = io.BytesIO()
    frames[0].save(
        buffer,
        format="GIF",
        save_all=True,
        append_images=frames[1:],
        duration=durations,
        loop=1,
    )
    buffer.seek(0)
    return buffer
