"""Wheel of Fortune image generation using Pillow."""

import io
import math
from PIL import Image, ImageDraw, ImageFont

from config import WHEEL_TARGET_EV

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
    No motion trails (since GIF provides temporal motion), cleaner for compression.
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
            wedge_color = tuple(min(255, c + 50) for c in rgb)
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
    for i, text in enumerate(["WHEEL OF", "FORTUNE"]):
        bbox = draw.textbbox((0, 0), text, font=title_font)
        text_w = bbox[2] - bbox[0]
        draw.text(
            (center - text_w / 2, center - 15 + i * 18),
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

    num_frames = 95

    # Phase boundaries
    fast_end = 45       # End of fast spin
    medium_end = 60     # End of medium spin
    slow_end = 80       # End of slow crawl (near-miss point)
    creep_end = 94      # End of creep to final

    # Randomize the ending style:
    # 25% - "Clean landing": stops right on target, no creep needed
    # 35% - "Full stop then creep": stops short, pauses dramatically, then creeps
    # 40% - "Smooth creep": slows into a creep without full stop
    ending_roll = random.random()

    if ending_roll < 0.25:
        # Clean landing - stops right on target
        near_miss_wedges = 0
        do_full_stop = False
        full_stop_duration = 0
    elif ending_roll < 0.60:
        # Full stop then creep - dramatic pause before creeping
        near_miss_wedges = random.uniform(0.3, 2.0)
        do_full_stop = True
        full_stop_duration = random.randint(1000, 2500)
    else:
        # Smooth creep - no full stop, just gradual creep
        near_miss_wedges = random.uniform(0.1, 1.5)
        do_full_stop = False
        full_stop_duration = 0

    near_miss_rotation = total_spin - (angle_per_wedge * near_miss_wedges)

    # Randomize creep speed: how long each creep frame takes
    # Base duration 600-1000ms, multiplied by random factor 0.7-1.5
    creep_base_duration = random.randint(600, 1000)
    creep_speed_factor = random.uniform(0.7, 1.5)

    for i in range(num_frames):
        progress = i / (num_frames - 1)

        if i <= slow_end:
            # Main spin: fast -> medium -> slow, stopping at near-miss point
            phase_progress = i / slow_end
            # Quintic ease-out for smooth deceleration
            eased = 1 - pow(1 - phase_progress, 5)
            rotation = near_miss_rotation * eased
        else:
            # Creep phase: slowly cover the final 0.7 wedges
            creep_progress = (i - slow_end) / (creep_end - slow_end)
            # Quadratic ease-out for the final creep (like fighting friction)
            creep_eased = 1 - pow(1 - creep_progress, 2)
            remaining = total_spin - near_miss_rotation
            rotation = near_miss_rotation + (remaining * creep_eased)

        is_final = i == num_frames - 1

        frame = create_wheel_frame_for_gif(
            size, rotation, selected_idx=target_idx if is_final else None
        )

        # Convert to palette mode for GIF
        frame_p = frame.convert("RGB").convert("P", palette=Image.ADAPTIVE, colors=256)
        frames.append(frame_p)

        # Variable timing - slower during creep for suspense
        if i < fast_end:
            durations.append(50)      # Fast spin: 50ms
        elif i < medium_end:
            durations.append(100)     # Medium: 100ms
        elif i < slow_end:
            durations.append(250)     # Slow crawl: 250ms
        elif i == slow_end:
            # First creep frame - this is the "near-miss" moment
            # Add full stop duration if we're doing a full stop
            base = int(creep_base_duration * creep_speed_factor)
            durations.append(base + full_stop_duration)
        elif i < creep_end:
            # Creep phase: randomized base + gradual slowdown
            creep_idx = i - slow_end
            creep_frames = creep_end - slow_end
            # Duration increases as we approach the end
            slowdown = 1 + (0.5 * creep_idx / creep_frames)
            duration = int(creep_base_duration * creep_speed_factor * slowdown)
            durations.append(duration)
        else:
            # Hold final frame for 60s (appears frozen)
            durations.append(60000)

    buffer = io.BytesIO()
    frames[0].save(
        buffer,
        format="GIF",
        save_all=True,
        append_images=frames[1:],
        duration=durations,
        loop=1,  # Play once
    )
    buffer.seek(0)
    return buffer
