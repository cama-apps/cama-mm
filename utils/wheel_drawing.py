"""Wheel of Fortune image generation using Pillow."""

import io
import math
import random
from PIL import Image, ImageDraw, ImageFont
from pilmoji import Pilmoji

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

# Cache for pre-rendered emoji text images (avoids pilmoji calls per GIF frame)
_CACHED_EMOJI_TEXT: dict[tuple[str, int, str], Image.Image] = {}


def _has_emoji(text: str) -> bool:
    """Check if text contains emoji characters."""
    # Check for characters in emoji ranges (beyond basic ASCII/Latin)
    return any(ord(c) > 0x1F00 for c in text)


def _get_emoji_text_image(
    text: str, font: ImageFont.FreeTypeFont | ImageFont.ImageFont, size: int, fill: str = "#ffffff"
) -> Image.Image:
    """
    Pre-render emoji text to a transparent image, cached for performance.

    This avoids calling pilmoji for every GIF frame by caching the rendered result.
    """
    cache_key = (text, size, fill)
    if cache_key not in _CACHED_EMOJI_TEXT:
        # Create transparent image sized for the text
        temp_img = Image.new("RGBA", (size * 4, size * 2), (0, 0, 0, 0))
        with Pilmoji(temp_img) as pilmoji:
            pilmoji.text((0, 0), text, font=font, fill=fill)
        _CACHED_EMOJI_TEXT[cache_key] = temp_img
    return _CACHED_EMOJI_TEXT[cache_key]


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
    radius = size // 2 - 50
    inner_radius = radius // 3

    title_font = _get_cached_font(max(9, size // 45), "title")

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
            (center - text_w / 2, center - 12 + i * 16),
            text,
            fill="#f1c40f",
            font=title_font,
        )

    # Draw pointer
    pointer_y = center - radius - 5
    pointer_points = [
        (center, pointer_y + 35),
        (center - 18, pointer_y - 8),
        (center - 6, pointer_y + 2),
        (center, pointer_y + 18),
        (center + 6, pointer_y + 2),
        (center + 18, pointer_y - 8),
    ]
    draw.polygon(pointer_points, fill="#e74c3c", outline="#ffffff", width=2)

    return img


# Base wheel wedge configuration: (label, base_value, color)
# BANKRUPT value will be adjusted based on WHEEL_TARGET_EV
# Shell wedges are spread out for visual variety (BLUE with 25s, RED between 70-80)
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
    ("25", 25, "#3498db"),
    ("🔵 BLUE", "BLUE_SHELL", "#3498db"),  # Mario Kart: Steal from richest (with 25s - same blue)
    ("30", 30, "#2980b9"),
    ("35", 35, "#1f6dad"),
    ("40", 40, "#9b59b6"),
    ("45", 45, "#8e44ad"),
    ("50", 50, "#7d3c98"),
    ("50", 50, "#7d3c98"),
    ("60", 60, "#e67e22"),
    ("70", 70, "#d35400"),
    ("🔴 RED", "RED_SHELL", "#e74c3c"),   # Mario Kart: Steal from player above (between 70-80)
    ("80", 80, "#c0392b"),
    ("100", 100, "#f1c40f"),
    ("100", 100, "#f1c40f"),
]


def _calculate_adjusted_wedges(target_ev: float) -> list[tuple[str, int | str, str]]:
    """
    Calculate wheel wedges with BANKRUPT value adjusted to hit target EV.

    The BANKRUPT penalty is adjusted so that:
    sum(all_values) / num_wedges = target_ev

    BANKRUPT is capped at -1 minimum (can never be positive or zero).
    Special shell wedges (RED_SHELL, BLUE_SHELL) are excluded from EV calculation
    since their value depends on stealing from other players.
    """
    num_wedges = len(_BASE_WHEEL_WEDGES)

    # Calculate sum of non-bankrupt, non-special values (integers only)
    non_bankrupt_sum = sum(
        v for _, v, _ in _BASE_WHEEL_WEDGES
        if isinstance(v, int) and v >= 0
    )

    # Count bankrupt wedges (negative integers)
    num_bankrupt = sum(
        1 for _, v, _ in _BASE_WHEEL_WEDGES
        if isinstance(v, int) and v < 0
    )

    # Target sum = target_ev * num_wedges
    # Target sum = non_bankrupt_sum + (num_bankrupt * bankrupt_value) + shell_ev
    # For shell wedges, assume average EV of 0 (steal/self-hit balance)
    # bankrupt_value = (target_sum - non_bankrupt_sum) / num_bankrupt
    target_sum = target_ev * num_wedges
    if num_bankrupt > 0:
        bankrupt_value = int((target_sum - non_bankrupt_sum) / num_bankrupt)
        # BANKRUPT must always be negative (minimum -1)
        bankrupt_value = min(bankrupt_value, -1)
    else:
        bankrupt_value = -100  # Fallback

    # Build adjusted wedges
    adjusted = []
    for label, value, color in _BASE_WHEEL_WEDGES:
        if isinstance(value, str):
            # Special wedge (RED_SHELL, BLUE_SHELL) - keep as-is
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


def create_wheel_image(
    size: int = 400,
    rotation: float = 0,
    spinning: bool = False,
    selected_idx: int | None = None,
) -> Image.Image:
    """
    Create a wheel of fortune image.

    Args:
        size: Image size in pixels (square)
        rotation: Wheel rotation in degrees (0 = first wedge at top)
        spinning: Whether to show motion effects
        selected_idx: Index of selected wedge to highlight (for result display)

    Returns:
        PIL Image object
    """
    img = Image.new("RGBA", (size, size), (30, 30, 35, 255))
    draw = ImageDraw.Draw(img)

    center = size // 2
    radius = size // 2 - 50
    inner_radius = radius // 3

    num_wedges = len(WHEEL_WEDGES)
    angle_per_wedge = 360 / num_wedges

    # Load fonts
    try:
        font_path = "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"
        small_font = ImageFont.truetype(font_path, max(12, size // 35))
        title_font = ImageFont.truetype(font_path, max(10, size // 40))
    except OSError:
        small_font = ImageFont.load_default()
        title_font = small_font

    # Draw motion trails if spinning
    if spinning:
        for trail in range(3, 0, -1):
            trail_alpha = 60 - trail * 15
            trail_rotation = rotation - trail * 8

            for i, (label, value, color) in enumerate(WHEEL_WEDGES):
                start_angle = i * angle_per_wedge + trail_rotation - 90
                end_angle = start_angle + angle_per_wedge

                rgb = hex_to_rgb(color)
                faded_color = (*rgb, trail_alpha)

                trail_img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
                trail_draw = ImageDraw.Draw(trail_img)
                trail_draw.pieslice(
                    [
                        center - radius,
                        center - radius,
                        center + radius,
                        center + radius,
                    ],
                    start_angle,
                    end_angle,
                    fill=faded_color,
                )
                img = Image.alpha_composite(img, trail_img)

            draw = ImageDraw.Draw(img)

    # Draw outer glow ring
    for glow in range(5, 0, -1):
        glow_radius = radius + glow * 3
        glow_alpha = 30 - glow * 5
        draw.ellipse(
            [
                center - glow_radius,
                center - glow_radius,
                center + glow_radius,
                center + glow_radius,
            ],
            outline=(255, 215, 0, glow_alpha),
            width=2,
        )

    # Draw wedges
    for i, (label, value, color) in enumerate(WHEEL_WEDGES):
        start_angle = i * angle_per_wedge + rotation - 90
        end_angle = start_angle + angle_per_wedge

        # Highlight selected wedge
        wedge_color = color
        outline_color = "#ffffff"
        outline_width = 2

        if selected_idx is not None and i == selected_idx:
            # Brighten the selected wedge
            rgb = hex_to_rgb(color)
            wedge_color = tuple(min(255, c + 40) for c in rgb)
            outline_color = "#f1c40f"
            outline_width = 4

        draw.pieslice(
            [center - radius, center - radius, center + radius, center + radius],
            start_angle,
            end_angle,
            fill=wedge_color,
            outline=outline_color,
            width=outline_width,
        )

        # Calculate text position
        mid_angle = math.radians(start_angle + angle_per_wedge / 2)
        text_radius = radius * 0.72
        text_x = center + text_radius * math.cos(mid_angle)
        text_y = center + text_radius * math.sin(mid_angle)

        # For special wedges (string values like RED_SHELL), show the label
        # For BANKRUPT/LOSE (value <= 0), show the label
        # For positive values, show the numeric value
        if isinstance(value, str) or value <= 0:
            text = label
        else:
            text = str(value)

        # Use pilmoji for emoji labels (shell wedges), standard draw for others
        if _has_emoji(text):
            # Get cached emoji text image
            emoji_img = _get_emoji_text_image(text, small_font, max(12, size // 35))
            # Composite the emoji text onto the main image
            paste_x = int(text_x - emoji_img.width / 2)
            paste_y = int(text_y - emoji_img.height / 2)
            temp = Image.new("RGBA", img.size, (0, 0, 0, 0))
            temp.paste(emoji_img, (paste_x, paste_y), emoji_img)
            img = Image.alpha_composite(img, temp)
            draw = ImageDraw.Draw(img)
        else:
            bbox = draw.textbbox((0, 0), text, font=small_font)
            text_w = bbox[2] - bbox[0]
            text_h = bbox[3] - bbox[1]

            # Text shadow
            draw.text(
                (text_x - text_w / 2 + 1, text_y - text_h / 2 + 1),
                text,
                fill="#000000",
                font=small_font,
            )
            draw.text(
                (text_x - text_w / 2, text_y - text_h / 2),
                text,
                fill="#ffffff",
                font=small_font,
            )

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
    center_text = "WHEEL OF"
    bbox = draw.textbbox((0, 0), center_text, font=title_font)
    text_w = bbox[2] - bbox[0]
    draw.text(
        (center - text_w / 2, center - 15), center_text, fill="#f1c40f", font=title_font
    )

    center_text2 = "FORTUNE"
    bbox = draw.textbbox((0, 0), center_text2, font=title_font)
    text_w = bbox[2] - bbox[0]
    draw.text(
        (center - text_w / 2, center + 3), center_text2, fill="#f1c40f", font=title_font
    )

    # Draw pointer
    pointer_y = center - radius - 5

    # Pointer glow
    for glow in range(4, 0, -1):
        glow_color = (231, 76, 60, 50 - glow * 10)
        points = [
            (center, pointer_y + 35 + glow),
            (center - 20 - glow, pointer_y - 10 - glow),
            (center - 8, pointer_y),
            (center, pointer_y + 15),
            (center + 8, pointer_y),
            (center + 20 + glow, pointer_y - 10 - glow),
        ]
        draw.polygon(points, fill=glow_color)

    # Main pointer
    pointer_points = [
        (center, pointer_y + 35),
        (center - 18, pointer_y - 8),
        (center - 6, pointer_y + 2),
        (center, pointer_y + 18),
        (center + 6, pointer_y + 2),
        (center + 18, pointer_y - 8),
    ]
    draw.polygon(pointer_points, fill="#e74c3c", outline="#ffffff", width=2)

    # Pointer highlight
    highlight_points = [
        (center, pointer_y + 20),
        (center - 8, pointer_y),
        (center, pointer_y + 10),
    ]
    draw.polygon(highlight_points, fill="#ec7063")

    # Direction indicators if spinning
    if spinning:
        arrow_radius = radius + 25

        for arrow_angle in [45, 135, 225, 315]:
            angle_rad = math.radians(arrow_angle + rotation)
            ax = center + arrow_radius * math.cos(angle_rad)
            ay = center + arrow_radius * math.sin(angle_rad)

            tangent_angle = angle_rad + math.pi / 2
            arrow_len = 12

            tip_x = ax + arrow_len * math.cos(tangent_angle)
            tip_y = ay + arrow_len * math.sin(tangent_angle)

            wing_angle1 = tangent_angle + math.pi * 0.8
            wing_angle2 = tangent_angle - math.pi * 0.8
            wing_len = 6

            wing1_x = tip_x + wing_len * math.cos(wing_angle1)
            wing1_y = tip_y + wing_len * math.sin(wing_angle1)
            wing2_x = tip_x + wing_len * math.cos(wing_angle2)
            wing2_y = tip_y + wing_len * math.sin(wing_angle2)

            draw.polygon(
                [(tip_x, tip_y), (wing1_x, wing1_y), (wing2_x, wing2_y)], fill="#f1c40f"
            )

            # Motion lines
            for line in range(3):
                line_start = tangent_angle + math.pi
                line_len = 8 + line * 4
                lx1 = ax + (line * 3) * math.cos(line_start)
                ly1 = ay + (line * 3) * math.sin(line_start)
                lx2 = lx1 + line_len * math.cos(line_start)
                ly2 = ly1 + line_len * math.sin(line_start)

                alpha = 150 - line * 40
                draw.line(
                    [(lx1, ly1), (lx2, ly2)], fill=(255, 255, 255, alpha), width=2
                )

    return img


def wheel_image_to_bytes(img: Image.Image) -> io.BytesIO:
    """Convert PIL Image to bytes buffer for Discord."""
    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer


def get_rotation_for_index(target_idx: int, num_wedges: int = 26) -> float:
    """
    Calculate rotation needed to position a wedge at the top (under pointer).

    Args:
        target_idx: Index of wedge to position at top
        num_wedges: Total number of wedges

    Returns:
        Rotation in degrees
    """
    angle_per_wedge = 360 / num_wedges
    # Rotation needed to put target wedge at top (0 degrees)
    # Each wedge starts at idx * angle_per_wedge, we want center of wedge at top
    return -(target_idx * angle_per_wedge + angle_per_wedge / 2)


def get_wedge_at_index(idx: int) -> tuple[str, int, str]:
    """Get wedge info (label, value, color) at given index."""
    return WHEEL_WEDGES[idx % len(WHEEL_WEDGES)]


def create_wheel_frame_for_gif(
    size: int, rotation: float, selected_idx: int | None = None
) -> Image.Image:
    """
    Create a single wheel frame optimized for GIF animation.
    Uses cached static overlay for pointer/center to avoid redrawing each frame.
    """
    img = Image.new("RGBA", (size, size), (30, 30, 35, 255))
    draw = ImageDraw.Draw(img)

    center = size // 2
    radius = size // 2 - 50

    num_wedges = len(WHEEL_WEDGES)
    angle_per_wedge = 360 / num_wedges

    # Use cached regular font (not bold) at smaller size
    small_font = _get_cached_font(max(12, size // 32), "small", bold=True)

    # Draw outer glow ring
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
        start_angle = i * angle_per_wedge + rotation - 90
        end_angle = start_angle + angle_per_wedge

        # Highlight selected wedge
        wedge_color = color
        outline_color = "#ffffff"
        outline_width = 2

        if selected_idx is not None and i == selected_idx:
            rgb = hex_to_rgb(color)
            wedge_color = tuple(min(255, c + 120) for c in rgb)
            outline_color = "#ffff00"  # Bright yellow
            outline_width = 12  # Thicc outline

        draw.pieslice(
            [center - radius, center - radius, center + radius, center + radius],
            start_angle,
            end_angle,
            fill=wedge_color,
            outline=outline_color,
            width=outline_width,
        )

        # Calculate text position
        mid_angle = math.radians(start_angle + angle_per_wedge / 2)
        text_radius = radius * 0.72
        text_x = center + text_radius * math.cos(mid_angle)
        text_y = center + text_radius * math.sin(mid_angle)

        # For special wedges (string values like RED_SHELL), show the label
        # For BANKRUPT/LOSE (value <= 0), show the label
        # For positive values, show the numeric value
        if isinstance(value, str) or value <= 0:
            text = label
        else:
            text = str(value)

        # Use pilmoji for emoji labels (shell wedges), standard draw for others
        if _has_emoji(text):
            # Get cached emoji text image (rendered once, reused across all frames)
            emoji_img = _get_emoji_text_image(text, small_font, max(12, size // 32))
            # Composite the emoji text onto the main image
            paste_x = int(text_x - emoji_img.width / 2)
            paste_y = int(text_y - emoji_img.height / 2)
            temp = Image.new("RGBA", img.size, (0, 0, 0, 0))
            temp.paste(emoji_img, (paste_x, paste_y), emoji_img)
            img = Image.alpha_composite(img, temp)
            draw = ImageDraw.Draw(img)
        else:
            bbox = draw.textbbox((0, 0), text, font=small_font)
            text_w = bbox[2] - bbox[0]
            text_h = bbox[3] - bbox[1]

            # Text shadow
            draw.text(
                (text_x - text_w / 2 + 1, text_y - text_h / 2 + 1),
                text,
                fill="#000000",
                font=small_font,
            )
            draw.text(
                (text_x - text_w / 2, text_y - text_h / 2),
                text,
                fill="#ffffff",
                font=small_font,
            )

    # Draw winner glow effect AFTER all wedges (so it's on top)
    if selected_idx is not None:
        win_angle = selected_idx * angle_per_wedge + rotation - 90
        # Redraw winning wedge outline extra thick for emphasis
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

    # Composite cached static overlay (center circle, text, pointer)
    static_overlay = _get_static_overlay(size)
    img = Image.alpha_composite(img, static_overlay)

    return img


def create_wheel_gif(target_idx: int, size: int = 400) -> io.BytesIO:
    """
    Create an animated GIF of the wheel spinning and landing on target_idx.

    Uses physics-inspired animation: smooth deceleration with a randomized
    "near-miss" moment where the wheel almost stops before the target,
    then creeps forward to the final position - like a real wheel fighting
    against friction.

    Args:
        target_idx: Index of wedge to land on (0-23)
        size: Image size in pixels

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

    num_frames = 150

    # Phase boundaries (optimized for smooth 8-18 second animation)
    fast_end = 60       # End of fast spin (frames 0-59)
    medium_end = 90     # End of medium spin (frames 60-89)
    slow_end = 125      # End of slow crawl (frames 90-124)
    creep_end = 148     # End of creep to final (frames 125-147)
    # Frame 148 is second-to-last, frame 149 is final

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
            size, rotation, selected_idx=target_idx if is_final else None
        )

        frame_p = frame.convert("RGB").convert("P", palette=Image.ADAPTIVE, colors=256)
        frames.append(frame_p)

        # Timing: variable 8-18 seconds of animation
        if i < 30:
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


def create_explosion_gif(size: int = 400) -> io.BytesIO:
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
    radius = size // 2 - 50

    # Phase 1: Normal spin for ~1 second (builds tension)
    spin_frames = 20
    for i in range(spin_frames):
        rotation = i * 25  # Fast spin
        frame = create_wheel_frame_for_gif(size, rotation, selected_idx=None)
        frame_p = frame.convert("RGB").convert("P", palette=Image.ADAPTIVE, colors=256)
        frames.append(frame_p)
        durations.append(50)

    # Phase 2: Wheel starts shaking/glitching (something's wrong...)
    shake_frames = 15
    base_rotation = spin_frames * 25
    for i in range(shake_frames):
        # Increasingly violent shaking
        shake_intensity = (i + 1) * 3
        shake_x = random.randint(-shake_intensity, shake_intensity)
        shake_y = random.randint(-shake_intensity, shake_intensity)

        frame = create_wheel_frame_for_gif(size, base_rotation + random.randint(-5, 5))

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
        speed = random.uniform(3, 15)
        particle = {
            "x": center,
            "y": center,
            "vx": math.cos(angle) * speed,
            "vy": math.sin(angle) * speed,
            "size": random.randint(4, 20),
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
        speed = random.uniform(5, 12)
        fragments.append({
            "x": center,
            "y": center,
            "vx": math.cos(angle) * speed,
            "vy": math.sin(angle) * speed,
            "rotation": random.uniform(0, 360),
            "rot_speed": random.uniform(-20, 20),
            "size": random.randint(20, 50),
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
                ring_radius = (frame_idx + 1) * 15 + ring * 30
                ring_alpha = max(0, 200 - frame_idx * 15 - ring * 40)
                if ring_alpha > 0 and ring_radius < size:
                    draw.ellipse(
                        [center - ring_radius, center - ring_radius,
                         center + ring_radius, center + ring_radius],
                        outline=(255, 200, 100, ring_alpha),
                        width=4 - ring,
                    )

        # Update and draw fragments
        for frag in fragments:
            frag["x"] += frag["vx"]
            frag["y"] += frag["vy"]
            frag["vy"] += 0.3  # Gravity
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
            p["vy"] += 0.2  # Gravity
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
                smoke_x = center + random.randint(-80, 80)
                smoke_y = center + random.randint(-80, 40) - frame_idx * 2
                smoke_size = 30 + frame_idx * 2 + smoke_idx * 10
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
    try:
        font_path = "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"
        big_font = ImageFont.truetype(font_path, 48)
        small_font = ImageFont.truetype(font_path, 20)
    except OSError:
        big_font = ImageFont.load_default()
        small_font = big_font

    for frame_idx in range(aftermath_frames):
        img = Image.new("RGBA", (size, size), (30, 30, 35, 255))
        draw = ImageDraw.Draw(img)

        # Fading smoke
        smoke_alpha = max(0, 60 - frame_idx * 3)
        if smoke_alpha > 0:
            for _ in range(8):
                sx = center + random.randint(-100, 100)
                sy = center + random.randint(-60, 60) - frame_idx * 3
                ssize = random.randint(40, 80)
                draw.ellipse(
                    [sx - ssize, sy - ssize, sx + ssize, sy + ssize],
                    fill=(60, 60, 60, smoke_alpha),
                )

        # Scattered debris on ground
        for _ in range(15):
            dx = center + random.randint(-150, 150)
            dy = center + random.randint(50, 120)
            dsize = random.randint(3, 8)
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
        jc_y = center - 50

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
        sorry_y = center + 20

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
