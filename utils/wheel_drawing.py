"""Wheel of Fortune image generation using Pillow."""

import io
import math
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
    ("30", 30, "#2980b9"),
    ("35", 35, "#1f6dad"),
    ("40", 40, "#9b59b6"),
    ("45", 45, "#8e44ad"),
    ("50", 50, "#7d3c98"),
    ("50", 50, "#7d3c98"),
    ("60", 60, "#e67e22"),
    ("70", 70, "#d35400"),
    ("80", 80, "#c0392b"),
    ("100", 100, "#f1c40f"),
    ("100", 100, "#f1c40f"),
]


def _calculate_adjusted_wedges(target_ev: float) -> list[tuple[str, int, str]]:
    """
    Calculate wheel wedges with BANKRUPT value adjusted to hit target EV.

    The BANKRUPT penalty is adjusted so that:
    sum(all_values) / num_wedges = target_ev

    BANKRUPT is capped at -1 minimum (can never be positive or zero).
    """
    num_wedges = len(_BASE_WHEEL_WEDGES)

    # Calculate sum of non-bankrupt values
    non_bankrupt_sum = sum(v for _, v, _ in _BASE_WHEEL_WEDGES if v >= 0)

    # Count bankrupt wedges
    num_bankrupt = sum(1 for _, v, _ in _BASE_WHEEL_WEDGES if v < 0)

    # Target sum = target_ev * num_wedges
    # Target sum = non_bankrupt_sum + (num_bankrupt * bankrupt_value)
    # bankrupt_value = (target_sum - non_bankrupt_sum) / num_bankrupt
    target_sum = target_ev * num_wedges
    bankrupt_value = int((target_sum - non_bankrupt_sum) / num_bankrupt)

    # BANKRUPT must always be negative (minimum -1)
    bankrupt_value = min(bankrupt_value, -1)

    # Build adjusted wedges
    adjusted = []
    for label, value, color in _BASE_WHEEL_WEDGES:
        if value < 0:  # BANKRUPT
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

        text = label if value <= 0 else str(value)
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


def get_rotation_for_index(target_idx: int, num_wedges: int = 24) -> float:
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

        text = label if value <= 0 else str(value)
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
