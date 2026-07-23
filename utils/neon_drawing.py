"""
Neon Degen Terminal - GIF animation engine using Pillow.

Generates CRT-style terminal GIF animations for Layer 3 dramatic events.
Follows wheel_drawing.py patterns: BytesIO output, frame list, PIL Image.

Specs:
- 400x300px, 256-color adaptive palette, target < 4MB
- Neon color palette with CRT effects (scanlines, phosphor glow, glitch)
"""

from __future__ import annotations

import io
import math
import random

from PIL import Image, ImageDraw, ImageFilter, ImageFont

from utils.fonts import get_font

# ---------------------------------------------------------------------------
# Neon color palette
# ---------------------------------------------------------------------------
NEON_GREEN = (0, 255, 65)
NEON_CYAN = (0, 255, 255)
NEON_PINK = (255, 0, 128)
NEON_RED = (255, 30, 30)
NEON_YELLOW = (255, 220, 0)
CRT_BLACK = (10, 10, 15)
DIM_GREEN = (0, 120, 30)
DIM_CYAN = (0, 100, 100)

POST_MATCH_GIF_THEMES: tuple[str, ...] = (
    "divine_rapier_position",
    "buyback_denied",
    "ancient_liquidated",
    "beyond_godlike",
    "odds_anomaly",
)

# Witch's Curse palette (toxic green + violet hellfire over a darker, purpler CRT)
WITCH_GREEN = (57, 255, 20)
WITCH_VIOLET = (170, 60, 255)
WITCH_DIM_GREEN = (20, 90, 30)
WITCH_BG = (8, 6, 12)

# Standard size
WIDTH = 400
HEIGHT = 300

# Keep palette analysis bounded even as animation frame counts grow. At the
# current 400x300 render size this samples roughly one sixteenth of the source
# pixels for the longest Neon animation.
_PALETTE_SAMPLE_PIXEL_BUDGET = 500_000


def _get_font(size: int, bold: bool = False) -> ImageFont.FreeTypeFont | ImageFont.ImageFont:
    """Get a cached monospace font."""
    return get_font(size, bold=bold, mono=True)


# ---------------------------------------------------------------------------
# CRT effect helpers
# ---------------------------------------------------------------------------

def _apply_scanlines(img: Image.Image, intensity: int = 40, spacing: int = 2) -> Image.Image:
    """Apply horizontal scanline effect."""
    overlay = Image.new("RGBA", img.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(overlay)
    for y in range(0, img.height, spacing):
        draw.line([(0, y), (img.width, y)], fill=(0, 0, 0, intensity), width=1)
    return Image.alpha_composite(img.convert("RGBA"), overlay)


def _apply_phosphor_glow(img: Image.Image, radius: int = 2) -> Image.Image:
    """Apply phosphor glow effect (blur + additive composite)."""
    glow = img.filter(ImageFilter.GaussianBlur(radius))
    # Blend: 70% original + 30% glow
    return Image.blend(img, glow, 0.3)


def _apply_glitch_lines(
    img: Image.Image, num_lines: int = 5, max_offset: int = 15
) -> Image.Image:
    """Apply horizontal glitch displacement to random scanline bands."""
    result = img.copy()
    for _ in range(num_lines):
        y = random.randint(0, img.height - 10)
        h = random.randint(2, 8)
        offset = random.randint(-max_offset, max_offset)
        band = img.crop((0, y, img.width, y + h))
        # Wrap around
        result.paste(band, (offset % img.width, y))
        if offset > 0:
            result.paste(band, (offset - img.width, y))
    return result


def _draw_text_centered(
    draw: ImageDraw.Draw,
    text: str,
    y: int,
    color: tuple,
    font: ImageFont.FreeTypeFont | ImageFont.ImageFont,
    width: int = WIDTH,
) -> None:
    """Draw text centered horizontally."""
    bbox = draw.textbbox((0, 0), text, font=font)
    text_w = bbox[2] - bbox[0]
    x = (width - text_w) // 2
    draw.text((x, y), text, fill=color, font=font)


def _draw_text_left(
    draw: ImageDraw.Draw,
    text: str,
    x: int,
    y: int,
    color: tuple,
    font: ImageFont.FreeTypeFont | ImageFont.ImageFont,
) -> None:
    """Draw text left-aligned."""
    draw.text((x, y), text, fill=color, font=font)


def _corrupt_text(text: str, intensity: float = 0.2) -> str:
    """Corrupt text with random glitch characters."""
    glitch_chars = "@#$%&*!?~^|/\\<>{}[]"
    return "".join(
        random.choice(glitch_chars) if ch != " " and random.random() < intensity else ch
        for ch in text
    )


def _make_frame(img: Image.Image, apply_crt: bool = True, glitch: bool = False) -> Image.Image:
    """Apply CRT effects, retaining RGB pixels for animation-wide quantization."""
    if apply_crt:
        img = _apply_scanlines(img)
        img = _apply_phosphor_glow(img)
    if glitch:
        img = _apply_glitch_lines(img, num_lines=random.randint(3, 10))
    return img.convert("RGB")


def _build_shared_palette(frames: list[Image.Image]) -> Image.Image:
    """Build one representative adaptive palette for an entire animation.

    Each frame contributes a proportionally downsampled RGB image to a bounded
    contact sheet. Running MEDIANCUT once over that sheet captures colors from
    every animation phase without repeating the expensive adaptive-palette
    search for every full-resolution frame.
    """
    frame_width, frame_height = frames[0].size
    source_pixels = frame_width * frame_height * len(frames)
    sample_scale = max(
        1.0,
        math.sqrt(source_pixels / _PALETTE_SAMPLE_PIXEL_BUDGET),
    )
    sample_width = max(1, int(frame_width / sample_scale))
    sample_height = max(1, int(frame_height / sample_scale))

    contact_sheet = Image.new(
        "RGB",
        (sample_width, sample_height * len(frames)),
    )
    for frame_index, frame in enumerate(frames):
        sample = frame.resize(
            (sample_width, sample_height),
            Image.Resampling.NEAREST,
        )
        if sample.mode != "RGB":
            rgb_sample = sample.convert("RGB")
            sample.close()
            sample = rgb_sample
        contact_sheet.paste(sample, (0, frame_index * sample_height))
        sample.close()

    quantized_sample = contact_sheet.quantize(
        colors=256,
        method=Image.Quantize.MEDIANCUT,
        dither=Image.Dither.NONE,
    )
    contact_sheet.close()

    # Only the palette is needed for the full-size frames. Copy it onto a tiny
    # image so the sampled contact sheet can be released before remapping them.
    palette = Image.new("P", (1, 1))
    palette.putpalette(quantized_sample.getpalette())
    quantized_sample.close()
    return palette


def _quantize_frames_with_shared_palette(frames: list[Image.Image]) -> None:
    """Remap RGB frames in place using one animation-wide adaptive palette."""
    palette = _build_shared_palette(frames)
    try:
        for frame_index, frame in enumerate(frames):
            frames[frame_index] = frame.quantize(
                palette=palette,
                dither=Image.Dither.FLOYDSTEINBERG,
            )
            frame.close()
    finally:
        palette.close()


def _save_gif(frames: list[Image.Image], durations: list[int]) -> io.BytesIO:
    """Save frames as GIF to BytesIO buffer."""
    _quantize_frames_with_shared_palette(frames)
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


def _fit_neon_text(text: str, font: ImageFont.FreeTypeFont | ImageFont.ImageFont) -> str:
    """Truncate terminal text so centered labels stay inside the CRT viewport."""
    max_width = WIDTH - 40
    if font.getbbox(text)[2] <= max_width:
        return text
    truncated = text
    while truncated and font.getbbox(f"{truncated}...")[2] > max_width:
        truncated = truncated[:-1]
    return f"{truncated}..."


# ---------------------------------------------------------------------------
# GIF Generators
# ---------------------------------------------------------------------------


def create_post_match_gif(
    name: str,
    value: int,
    *,
    theme: str,
) -> io.BytesIO:
    """Render a themed JOPA-T settlement animation for a completed match."""
    if theme not in POST_MATCH_GIF_THEMES:
        raise ValueError(f"Unsupported post-match GIF theme: {theme!r}")

    display_name = " ".join(name.split())[:22] or "UNKNOWN CLIENT"
    font_lg = _get_font(20, bold=True)
    font_md = _get_font(14, bold=True)
    font_sm = _get_font(12)
    value_formats = {
        "divine_rapier_position": f"+{value:,} RATING",
        "buyback_denied": f"{value:,} JC LOST",
        "ancient_liquidated": f"{value:,} JC PAID",
        "beyond_godlike": f"{value:,} MATCH STREAK",
        "odds_anomaly": f"{value:,}% IMPLIED ODDS",
    }
    value_text = _fit_neon_text(value_formats[theme], font_lg)

    theme_details = {
        "divine_rapier_position": ("RAPIER POSITION", NEON_YELLOW, NEON_GREEN),
        "buyback_denied": ("BUYBACK DENIED", NEON_RED, NEON_PINK),
        "ancient_liquidated": ("ANCIENT LIQUIDATED", NEON_CYAN, NEON_GREEN),
        "beyond_godlike": ("BEYOND GODLIKE", (180, 80, 255), NEON_YELLOW),
        "odds_anomaly": ("ODDS ANOMALY", NEON_CYAN, NEON_PINK),
    }
    banner, primary, secondary = theme_details[theme]
    frames: list[Image.Image] = []
    durations: list[int] = []

    for frame_index in range(18):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)
        progress = (frame_index + 1) / 18
        draw.rectangle([12, 12, WIDTH - 12, HEIGHT - 12], outline=primary, width=2)
        _draw_text_centered(draw, "JOPA-T/v3.7 MATCH LEDGER", 24, secondary, font_sm)
        _draw_text_centered(draw, banner, 50, primary, font_lg)
        _draw_text_centered(draw, _fit_neon_text(display_name, font_md), 88, secondary, font_md)
        _draw_text_centered(draw, value_text, 112, primary, font_lg)

        if theme == "divine_rapier_position":
            size = int(20 + progress * 48)
            center_x, center_y = WIDTH // 2, 210
            draw.polygon(
                [(center_x, center_y - size), (center_x + size, center_y),
                 (center_x, center_y + size), (center_x - size, center_y)],
                outline=primary,
                width=3,
            )
            for offset in range(3):
                y = 250 - offset * 16
                draw.line([(80 + frame_index * 8, y), (320 - frame_index * 8, y)], fill=secondary)
        elif theme == "buyback_denied":
            for row in range(6):
                y = 160 + row * 18
                width = max(12, int(50 + progress * 250) - row * 18)
                draw.rectangle([WIDTH // 2 - width // 2, y, WIDTH // 2 + width // 2, y + 8], fill=primary)
            _draw_text_centered(draw, "LIQUIDATION LOCKED", 268, secondary, font_sm)
        elif theme == "ancient_liquidated":
            for column in range(8):
                x = 54 + column * 38
                height = int((column + 2) * 8 * progress)
                draw.rectangle([x, 255 - height, x + 22, 255], outline=primary, width=2)
                if frame_index >= column * 2:
                    draw.rectangle([x + 4, 251 - height, x + 18, 251], fill=secondary)
            _draw_text_centered(draw, "SETTLEMENT COMPLETE", 268, primary, font_sm)
        elif theme == "beyond_godlike":
            for streak in range(7):
                x = (frame_index * 24 + streak * 59) % (WIDTH + 80) - 40
                y = 170 + (streak * 17) % 90
                draw.line([(x, y), (x + 64, y - 36)], fill=primary if streak % 2 else secondary, width=3)
            _draw_text_centered(draw, "STREAK ASCENDING", 268, secondary, font_sm)
        else:
            points = []
            for x in range(36, WIDTH - 36, 8):
                y = 210 + int(math.sin((x + frame_index * 18) / 20) * 28)
                points.append((x, y))
            draw.line(points, fill=primary, width=3)
            for x in range(36, WIDTH - 36, 16):
                y = 210 + int(math.cos((x + frame_index * 18) / 22) * 20)
                draw.ellipse([x - 2, y - 2, x + 2, y + 2], fill=secondary)
            _draw_text_centered(draw, "MARKET SIGNAL UNSTABLE", 268, secondary, font_sm)

        frames.append(_make_frame(img, glitch=frame_index in (0, 9)))
        durations.append(80 if frame_index < 17 else 60000)

    return _save_gif(frames, durations)

def create_terminal_crash_gif(name: str, filing_number: int) -> io.BytesIO:
    """
    Terminal crash GIF for 3rd+ bankruptcy.
    CRT glitch/breakdown/reboot sequence (~80 frames).
    """
    frames = []
    durations = []
    font_lg = _get_font(18, bold=True)
    font_sm = _get_font(12)
    font_md = _get_font(14, bold=True)

    # Phase 1: Normal terminal display (10 frames)
    for i in range(10):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)
        _draw_text_centered(draw, "JOPA-T/v3.7 TERMINAL", 20, NEON_GREEN, font_md)
        _draw_text_left(draw, f"> Processing filing #{filing_number}...", 20, 60, DIM_GREEN, font_sm)
        _draw_text_left(draw, f"> Debtor: {name}", 20, 80, DIM_GREEN, font_sm)
        _draw_text_left(draw, "> Status: PROCESSING", 20, 100, NEON_YELLOW, font_sm)
        # Blinking cursor
        if i % 2 == 0:
            _draw_text_left(draw, "> _", 20, 120, NEON_GREEN, font_sm)
        frames.append(_make_frame(img))
        durations.append(120)

    # Phase 2: Glitching intensifies (20 frames)
    error_messages = [
        "ERR: COMPASSION_MODULE overflow",
        "WARN: Dignity buffer underrun",
        "ERR: Faith_in_humanity.dll CORRUPT",
        "FATAL: Too many bankruptcies",
        f"ERR: Cannot process filing #{filing_number}",
        "WARN: System patience EXCEEDED",
        "ERR: STACK OVERFLOW in empathy.c",
        "SEGFAULT at 0xDEADBEEF",
    ]
    for i in range(20):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)

        intensity = i / 20
        # Increasingly corrupted header
        header = _corrupt_text("JOPA-T/v3.7 TERMINAL", intensity * 0.5)
        color = (
            int(NEON_GREEN[0] * (1 - intensity) + NEON_RED[0] * intensity),
            int(NEON_GREEN[1] * (1 - intensity) + NEON_RED[1] * intensity),
            int(NEON_GREEN[2] * (1 - intensity) + NEON_RED[2] * intensity),
        )
        _draw_text_centered(draw, header, 20, color, font_md)

        # Error messages accumulating
        y = 60
        for j in range(min(i // 2 + 1, len(error_messages))):
            msg = error_messages[j]
            if random.random() < intensity * 0.3:
                msg = _corrupt_text(msg, 0.4)
            err_color = NEON_RED if "FATAL" in msg or "SEGFAULT" in msg else NEON_YELLOW
            _draw_text_left(draw, msg, 20, y, err_color, font_sm)
            y += 16

        glitch_level = i > 10
        frames.append(_make_frame(img, glitch=glitch_level))
        durations.append(80 + i * 5)

    # Phase 3: Full breakdown (20 frames)
    for i in range(20):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)

        # Random noise blocks
        for _ in range(10 + i * 3):
            bx = random.randint(0, WIDTH - 40)
            by = random.randint(0, HEIGHT - 10)
            bw = random.randint(10, 60)
            bh = random.randint(2, 8)
            color = random.choice([NEON_RED, NEON_GREEN, NEON_PINK, NEON_CYAN])
            alpha = random.randint(40, 200)
            draw.rectangle([bx, by, bx + bw, by + bh], fill=(*color, alpha))

        # Flickering error text
        if random.random() > 0.3:
            crash_text = random.choice([
                "SYSTEM FAILURE",
                "KERNEL PANIC",
                "FATAL ERROR",
                f"BANKRUPTCY #{filing_number} CAUSED CRASH",
                "TERMINAL UNRESPONSIVE",
            ])
            y_pos = random.randint(80, 200)
            _draw_text_centered(draw, crash_text, y_pos, NEON_RED, font_lg)

        frames.append(_make_frame(img, glitch=True))
        durations.append(60)

    # Phase 4: Black screen + reboot (15 frames)
    # Black frames
    for i in range(5):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        frames.append(_make_frame(img, apply_crt=False))
        durations.append(300 if i == 0 else 200)

    # Reboot sequence
    reboot_lines = [
        ("JOPA-T/v3.7 REBOOTING...", NEON_GREEN),
        ("Memory check... OK", DIM_GREEN),
        ("Ledger integrity... OK", DIM_GREEN),
        (f"Bankruptcy #{filing_number}... FILED", NEON_RED),
        (f"Client {name}... NOTED", NEON_YELLOW),
        ("", NEON_GREEN),
        ("The system endures.", NEON_GREEN),
        ("It always does.", DIM_GREEN),
    ]
    for i in range(len(reboot_lines)):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)
        y = 40
        for j in range(i + 1):
            text, color = reboot_lines[j]
            if text:
                _draw_text_left(draw, text, 20, y, color, font_sm)
            y += 18
        is_last = i == len(reboot_lines) - 1
        frames.append(_make_frame(img))
        durations.append(60000 if is_last else 300)

    return _save_gif(frames, durations)


def create_void_welcome_gif(name: str) -> io.BytesIO:
    """
    Welcome to the Void GIF for first-ever bankruptcy.
    Neon initiation sequence.
    """
    frames = []
    durations = []
    font_lg = _get_font(20, bold=True)
    font_sm = _get_font(12)
    font_md = _get_font(14)

    # Phase 1: Darkness with a flickering cursor (10 frames)
    for i in range(10):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)
        if i % 3 != 0:
            _draw_text_left(draw, "> _", 20, HEIGHT // 2, DIM_GREEN, font_sm)
        frames.append(_make_frame(img))
        durations.append(150)

    # Phase 2: Text types out (20 frames)
    welcome_lines = [
        ("> INITIALIZING...", DIM_GREEN),
        ("> NEW CLIENT DETECTED", NEON_GREEN),
        (f"> IDENTITY: {name}", NEON_CYAN),
        ("> FIRST BANKRUPTCY RECORDED", NEON_RED),
        ("", None),
        ("> Welcome to the void.", NEON_GREEN),
        ("> The system sees you now.", DIM_GREEN),
        ("> There is no going back.", NEON_PINK),
    ]
    for i in range(len(welcome_lines)):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)
        y = 30
        for j in range(i + 1):
            text, color = welcome_lines[j]
            if text and color:
                _draw_text_left(draw, text, 20, y, color, font_sm)
            y += 20
        frames.append(_make_frame(img))
        durations.append(400)

    # Phase 3: Neon title reveal (15 frames)
    for i in range(15):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)

        # Draw all the typed text dimmed
        y = 30
        for text, color in welcome_lines:
            if text and color:
                dim_color = tuple(c // 3 for c in color[:3])
                _draw_text_left(draw, text, 20, y, dim_color, font_sm)
            y += 20

        # Big title
        title_y = HEIGHT - 80
        _draw_text_centered(draw, "DEBTOR #1", title_y, NEON_GREEN, font_lg)
        _draw_text_centered(draw, "CLASSIFICATION: FRESH", title_y + 30, DIM_GREEN, font_md)

        is_last = i == 14
        frames.append(_make_frame(img))
        durations.append(60000 if is_last else 200)

    return _save_gif(frames, durations)


def create_debt_collector_gif(name: str, debt: int) -> io.BytesIO:
    """
    Debt Collector GIF for 5x leverage into MAX_DEBT.
    Red scanline descent effect.
    """
    frames = []
    durations = []
    font_lg = _get_font(20, bold=True)
    font_sm = _get_font(12)
    font_md = _get_font(14, bold=True)

    total_frames = 40

    for i in range(total_frames):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)

        progress = i / total_frames

        # Red scanline descending
        scan_y = int(progress * HEIGHT)
        for sy in range(max(0, scan_y - 4), min(HEIGHT, scan_y + 4)):
            alpha = int(200 * (1 - abs(sy - scan_y) / 4))
            draw.line([(0, sy), (WIDTH, sy)], fill=(255, 0, 0, alpha), width=1)

        # Text appears as scanline passes
        if scan_y > 40:
            _draw_text_centered(draw, "DEBT COLLECTION", 30, NEON_RED, font_lg)
        if scan_y > 70:
            _draw_text_centered(draw, "=" * 30, 55, NEON_RED, font_sm)
        if scan_y > 100:
            _draw_text_left(draw, f"  Debtor: {name}", 20, 80, NEON_RED, font_sm)
        if scan_y > 130:
            _draw_text_left(draw, f"  Amount: {debt} JC", 20, 100, NEON_RED, font_sm)
        if scan_y > 160:
            _draw_text_left(draw, "  Status: MAXIMUM DEBT", 20, 120, NEON_RED, font_sm)
        if scan_y > 190:
            _draw_text_left(draw, "  Action: GARNISHMENT", 20, 140, NEON_RED, font_sm)
        if scan_y > 220:
            _draw_text_centered(draw, "=" * 30, 165, NEON_RED, font_sm)
        if scan_y > 250:
            _draw_text_centered(draw, "ALL WINNINGS SEIZED", 190, NEON_YELLOW, font_md)
        if scan_y > 270:
            _draw_text_centered(draw, "The system collects.", 220, DIM_GREEN, font_sm)

        is_last = i == total_frames - 1
        frames.append(_make_frame(img))
        durations.append(60000 if is_last else 80)

    return _save_gif(frames, durations)


def create_freefall_gif(name: str, start_balance: int, end_balance: int) -> io.BytesIO:
    """
    Freefall GIF for 100+ balance to 0 in one event.
    Balance numbers cascade down.
    """
    frames = []
    durations = []
    font_lg = _get_font(28, bold=True)
    font_sm = _get_font(12)
    font_md = _get_font(14, bold=True)

    total_frames = 45

    for i in range(total_frames):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)

        progress = i / total_frames

        # Header
        _draw_text_centered(draw, "BALANCE UPDATE", 15, NEON_RED, font_md)
        _draw_text_left(draw, f"  Client: {name}", 20, 40, DIM_GREEN, font_sm)

        # Cascading balance number
        current = int(start_balance * (1 - progress) + end_balance * progress)

        # Color transitions from green to red
        if current > 0:
            r = int(NEON_GREEN[0] * (1 - progress) + NEON_RED[0] * progress)
            g = int(NEON_GREEN[1] * (1 - progress) + NEON_RED[1] * progress)
            b = int(NEON_GREEN[2] * (1 - progress) + NEON_RED[2] * progress)
        else:
            r, g, b = NEON_RED

        # Big balance number
        balance_text = str(current)
        _draw_text_centered(draw, balance_text, HEIGHT // 2 - 20, (r, g, b), font_lg)
        _draw_text_centered(draw, "JC", HEIGHT // 2 + 20, (r // 2, g // 2, b // 2), font_md)

        # Trailing numbers falling
        for j in range(min(i, 8)):
            trail_y = HEIGHT // 2 - 20 + (j + 1) * 25
            trail_val = int(start_balance * (1 - (progress - j * 0.02)))
            if trail_y < HEIGHT - 20:
                alpha_factor = max(0.1, 1 - j * 0.12)
                trail_color = (int(r * alpha_factor), int(g * alpha_factor), int(b * alpha_factor))
                _draw_text_centered(draw, str(trail_val), trail_y, trail_color, font_sm)

        # Bottom status
        if progress > 0.8:
            status = "ZERO" if end_balance == 0 else f"DEBT: {abs(end_balance)}"
            _draw_text_centered(draw, f"FINAL: {status}", HEIGHT - 40, NEON_RED, font_md)

        is_last = i == total_frames - 1
        glitch = progress > 0.6
        frames.append(_make_frame(img, glitch=glitch))
        durations.append(60000 if is_last else 60 + int(progress * 80))

    return _save_gif(frames, durations)


def create_don_coin_flip_gif(name: str, balance_lost: int) -> io.BytesIO:
    """
    Double or Nothing coin flip LOSE GIF.
    Coin spinning, slows down, result: NOTHING. Balance cascades to 0.
    ~50 frames, 400x300px.
    """
    frames = []
    durations = []
    font_lg = _get_font(20, bold=True)
    font_sm = _get_font(12)
    font_md = _get_font(14, bold=True)
    font_bal = _get_font(24, bold=True)

    # Phase 1: Coin spinning (15 frames)
    coin_faces = ["DOUBLE", "NOTHING"]
    for i in range(15):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)

        _draw_text_centered(draw, "DOUBLE OR NOTHING", 15, NEON_YELLOW, font_md)
        _draw_text_left(draw, f"  Client: {name}", 20, 40, DIM_GREEN, font_sm)
        _draw_text_left(draw, f"  At Risk: {balance_lost} JC", 20, 58, NEON_YELLOW, font_sm)

        # Alternating coin text (fast)
        face = coin_faces[i % 2]
        face_color = NEON_GREEN if face == "DOUBLE" else NEON_RED
        _draw_text_centered(draw, face, HEIGHT // 2 - 15, face_color, font_lg)

        # Coin outline (simulated as a rectangle that squishes)
        scale = abs(math.sin(i * 0.8))
        coin_w = int(120 * max(0.1, scale))
        cx = WIDTH // 2
        cy = HEIGHT // 2
        draw.rectangle(
            [cx - coin_w // 2, cy - 25, cx + coin_w // 2, cy + 25],
            outline=NEON_YELLOW,
            width=2,
        )

        frames.append(_make_frame(img))
        durations.append(60)

    # Phase 2: Slowing down, background flickers red (15 frames)
    for i in range(15):
        # Denominator 14 so the last frame (i=14) hits full intensity, matching
        # the other 15-frame phases in this file.
        bg_red = int(30 * (i / 14))
        bg = (10 + bg_red, 10, 15)
        img = Image.new("RGBA", (WIDTH, HEIGHT), (*bg, 255))
        draw = ImageDraw.Draw(img)

        _draw_text_centered(draw, "DOUBLE OR NOTHING", 15, NEON_YELLOW, font_md)

        # Coin slows - show NOTHING more often as it decelerates
        if i < 5:
            face = coin_faces[i % 2]
        elif i < 10:
            face = "NOTHING" if i % 3 != 0 else "DOUBLE"
        else:
            face = "NOTHING"
        face_color = NEON_GREEN if face == "DOUBLE" else NEON_RED
        _draw_text_centered(draw, face, HEIGHT // 2 - 15, face_color, font_lg)

        # Status text
        status = "CALCULATING..." if i < 12 else "RESULT:"
        _draw_text_centered(draw, status, HEIGHT // 2 + 30, NEON_YELLOW, font_sm)

        frames.append(_make_frame(img, glitch=i > 8))
        durations.append(100 + i * 30)

    # Phase 3: Balance cascades to 0, red intensifies (20 frames)
    for i in range(20):
        progress = i / 19
        bg_red = int(40 + 30 * progress)
        bg = (bg_red, 10, 15)
        img = Image.new("RGBA", (WIDTH, HEIGHT), (*bg, 255))
        draw = ImageDraw.Draw(img)

        _draw_text_centered(draw, "DOUBLE OR NOTHING", 15, NEON_RED, font_md)
        _draw_text_centered(draw, "RESULT: NOTHING", 40, NEON_RED, font_sm)

        # Cascading balance number
        current = int(balance_lost * (1 - progress))
        bal_color = (
            int(NEON_YELLOW[0] * (1 - progress) + NEON_RED[0] * progress),
            int(NEON_YELLOW[1] * (1 - progress) + NEON_RED[1] * progress),
            int(NEON_YELLOW[2] * (1 - progress) + NEON_RED[2] * progress),
        )
        _draw_text_centered(draw, str(current), HEIGHT // 2 - 15, bal_color, font_bal)
        _draw_text_centered(draw, "JC", HEIGHT // 2 + 15, DIM_GREEN, font_sm)

        # Final frame: show 0 and message
        if i == 19:
            _draw_text_centered(draw, "0", HEIGHT // 2 - 15, NEON_RED, font_bal)
            _draw_text_centered(draw, "BALANCE: 0 JC", HEIGHT - 60, NEON_RED, font_md)
            _draw_text_centered(draw, "The coin has spoken.", HEIGHT - 35, DIM_GREEN, font_sm)

        is_last = i == 19
        frames.append(_make_frame(img, glitch=progress > 0.5))
        durations.append(60000 if is_last else 80 + int(progress * 60))

    return _save_gif(frames, durations)


def create_market_crash_gif(total_pool: int, outcome: str, winners: int, losers: int) -> io.BytesIO:
    """
    Market Crash GIF for large prediction market resolution.
    Rising graph → crash → settlement display.
    ~45 frames, 400x300px.
    """
    frames = []
    durations = []
    font_lg = _get_font(18, bold=True)
    font_sm = _get_font(11)
    font_md = _get_font(14, bold=True)

    graph_left = 40
    graph_right = WIDTH - 30
    graph_top = 80
    graph_bottom = 200
    graph_w = graph_right - graph_left
    graph_h = graph_bottom - graph_top

    # Phase 1: Green line graph rising (15 frames)
    for i in range(15):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)

        _draw_text_centered(draw, "PREDICTION MARKET", 10, NEON_GREEN, font_md)
        _draw_text_centered(draw, f"Pool: {total_pool} JC", 32, DIM_GREEN, font_sm)

        # Draw graph axes
        draw.line([(graph_left, graph_bottom), (graph_right, graph_bottom)], fill=DIM_GREEN, width=1)
        draw.line([(graph_left, graph_top), (graph_left, graph_bottom)], fill=DIM_GREEN, width=1)

        # Rising line
        points = []
        num_points = min(i + 2, 15)
        for j in range(num_points):
            x = graph_left + int(j * graph_w / 14)
            # Rising with some noise
            base_y = graph_bottom - int((j / 14) * graph_h * 0.8)
            noise = random.randint(-5, 5)
            y = max(graph_top, min(graph_bottom, base_y + noise))
            points.append((x, y))

        if len(points) >= 2:
            draw.line(points, fill=NEON_GREEN, width=2)

        # Pool stats below graph
        _draw_text_left(draw, f"  Participants: {winners + losers}", 20, 215, DIM_GREEN, font_sm)
        _draw_text_left(draw, "  Status: ACTIVE", 20, 233, NEON_GREEN, font_sm)

        frames.append(_make_frame(img))
        durations.append(100)

    # Phase 2: Graph crashes, red wash (15 frames)
    for i in range(15):
        progress = i / 14
        bg_red = int(40 * progress)
        img = Image.new("RGBA", (WIDTH, HEIGHT), (10 + bg_red, 10, 15, 255))
        draw = ImageDraw.Draw(img)

        header_color = (
            int(NEON_GREEN[0] * (1 - progress) + NEON_RED[0] * progress),
            int(NEON_GREEN[1] * (1 - progress) + NEON_RED[1] * progress),
            int(NEON_GREEN[2] * (1 - progress) + NEON_RED[2] * progress),
        )
        _draw_text_centered(draw, "PREDICTION MARKET", 10, header_color, font_md)

        # Draw graph axes
        draw.line([(graph_left, graph_bottom), (graph_right, graph_bottom)], fill=DIM_GREEN, width=1)
        draw.line([(graph_left, graph_top), (graph_left, graph_bottom)], fill=DIM_GREEN, width=1)

        # Crashing line - starts at peak, falls
        peak_x = graph_left + int(graph_w * 0.7)
        peak_y = graph_top + int(graph_h * 0.2)

        # Draw the historical rise
        rise_points = []
        for j in range(10):
            x = graph_left + int(j * (peak_x - graph_left) / 9)
            y = graph_bottom - int((j / 9) * (graph_bottom - peak_y))
            rise_points.append((x, y))

        # Crash portion
        crash_end_y = graph_bottom - int(graph_h * 0.1 * (1 - progress))
        crash_x = peak_x + int((graph_right - peak_x) * progress)
        rise_points.append((crash_x, crash_end_y))

        if len(rise_points) >= 2:
            draw.line(rise_points, fill=NEON_RED, width=2)

        # Flashing "MARKET CRASH" text
        if i % 2 == 0 or i > 10:
            _draw_text_centered(draw, "MARKET CRASH", HEIGHT // 2 + 20, NEON_RED, font_lg)

        _draw_text_left(draw, "  Status: SETTLING", 20, 233, NEON_YELLOW, font_sm)

        frames.append(_make_frame(img, glitch=i > 5))
        durations.append(80)

    # Phase 3: Settlement display (15 frames)
    for i in range(15):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)

        _draw_text_centered(draw, "MARKET SETTLED", 20, NEON_YELLOW, font_lg)
        _draw_text_centered(draw, "=" * 32, 45, DIM_GREEN, font_sm)

        _draw_text_centered(draw, f"Outcome: {outcome.upper()}", 70, NEON_YELLOW, font_md)
        _draw_text_centered(draw, f"Total Pool: {total_pool} JC", 95, NEON_RED, font_sm)

        # Winners in green
        _draw_text_left(draw, f"  Winners: {winners}", 60, 130, NEON_GREEN, font_md)
        # Losers in red
        _draw_text_left(draw, f"  Losers:  {losers}", 60, 155, NEON_RED, font_md)

        _draw_text_centered(draw, "=" * 32, 185, DIM_GREEN, font_sm)
        _draw_text_centered(draw, "WEALTH REDISTRIBUTED", 210, NEON_GREEN, font_md)
        _draw_text_centered(draw, "The system takes its cut.", 240, DIM_GREEN, font_sm)
        _draw_text_centered(draw, "JOPA-T/v3.7", HEIGHT - 25, DIM_GREEN, font_sm)

        is_last = i == 14
        frames.append(_make_frame(img))
        durations.append(60000 if is_last else 200)

    return _save_gif(frames, durations)


def create_degen_certificate_gif(name: str, score: int) -> io.BytesIO:
    """
    Degen Certificate GIF for crossing degen score 90.
    Achievement unlocked animation.
    """
    frames = []
    durations = []
    font_lg = _get_font(22, bold=True)
    font_sm = _get_font(11)
    font_md = _get_font(14, bold=True)
    font_score = _get_font(36, bold=True)

    # Phase 1: Score counting up (20 frames)
    for i in range(20):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)

        _draw_text_centered(draw, "DEGEN SCORE ANALYSIS", 20, DIM_GREEN, font_md)
        _draw_text_centered(draw, f"Subject: {name}", 45, DIM_CYAN, font_sm)

        # Counting up animation
        display_score = int(score * (i / 19))
        score_color = NEON_GREEN if display_score < 60 else NEON_YELLOW if display_score < 80 else NEON_RED
        _draw_text_centered(draw, str(display_score), HEIGHT // 2 - 30, score_color, font_score)
        _draw_text_centered(draw, "/ 100", HEIGHT // 2 + 15, DIM_GREEN, font_sm)

        # Progress bar
        bar_x = 60
        bar_y = HEIGHT // 2 + 40
        bar_w = WIDTH - 120
        bar_h = 12
        draw.rectangle([bar_x, bar_y, bar_x + bar_w, bar_y + bar_h], outline=DIM_GREEN)
        fill_w = int(bar_w * display_score / 100)
        draw.rectangle([bar_x, bar_y, bar_x + fill_w, bar_y + bar_h], fill=score_color)

        frames.append(_make_frame(img))
        durations.append(60)

    # Phase 2: Achievement flash (5 frames)
    for i in range(5):
        flash_alpha = 255 - i * 50
        img = Image.new("RGBA", (WIDTH, HEIGHT), (*NEON_RED[:3], min(255, flash_alpha)))
        frames.append(_make_frame(img, apply_crt=False))
        durations.append(60)

    # Phase 3: Certificate display (10 frames)
    for i in range(10):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)

        # Border
        draw.rectangle([10, 10, WIDTH - 10, HEIGHT - 10], outline=NEON_YELLOW, width=2)
        draw.rectangle([15, 15, WIDTH - 15, HEIGHT - 15], outline=DIM_GREEN, width=1)

        _draw_text_centered(draw, "ACHIEVEMENT UNLOCKED", 30, NEON_YELLOW, font_lg)
        _draw_text_centered(draw, "=" * 32, 58, DIM_GREEN, font_sm)

        _draw_text_centered(draw, "LEGENDARY DEGEN", 80, NEON_RED, font_lg)
        _draw_text_centered(draw, f"Score: {score}", 115, NEON_YELLOW, font_md)

        _draw_text_centered(draw, f"Certified to: {name}", 150, NEON_CYAN, font_sm)
        _draw_text_centered(draw, "=" * 32, 175, DIM_GREEN, font_sm)

        _draw_text_centered(draw, "The system acknowledges", 200, DIM_GREEN, font_sm)
        _draw_text_centered(draw, "your commitment to", 218, DIM_GREEN, font_sm)
        _draw_text_centered(draw, "financial ruin.", 236, NEON_GREEN, font_sm)

        _draw_text_centered(draw, "JOPA-T/v3.7", HEIGHT - 35, DIM_GREEN, font_sm)

        is_last = i == 9
        frames.append(_make_frame(img))
        durations.append(60000 if is_last else 200)

    return _save_gif(frames, durations)


# ---------------------------------------------------------------------------
# NEW GIF Generators - Easter Egg Events Expansion
# ---------------------------------------------------------------------------


def create_bomb_pot_gif(pool: int, contributors: int) -> io.BytesIO:
    """
    Bomb Pot GIF - Mandatory contribution animation.
    Countdown explosion with mandatory stakes display.
    ~50 frames, 400x300px.
    """
    frames = []
    durations = []
    font_lg = _get_font(22, bold=True)
    font_sm = _get_font(11)
    font_md = _get_font(14, bold=True)
    font_pool = _get_font(28, bold=True)

    # Phase 1: Countdown (15 frames)
    for i in range(15):
        countdown = 15 - i
        bg_intensity = min(80, i * 5)
        img = Image.new("RGBA", (WIDTH, HEIGHT), (10 + bg_intensity, 10, 15, 255))
        draw = ImageDraw.Draw(img)

        _draw_text_centered(draw, "BOMB POT", 20, NEON_RED, font_lg)
        _draw_text_centered(draw, "MANDATORY CONTRIBUTION", 50, NEON_YELLOW, font_sm)

        # Big countdown number
        count_color = NEON_GREEN if countdown > 10 else NEON_YELLOW if countdown > 5 else NEON_RED
        _draw_text_centered(draw, str(countdown), HEIGHT // 2 - 30, count_color, font_pool)

        # Pool accumulating
        partial_pool = int(pool * (i / 14))
        _draw_text_centered(draw, f"Pool: {partial_pool} JC", HEIGHT // 2 + 30, DIM_GREEN, font_md)
        _draw_text_centered(draw, f"Contributors: {contributors}", HEIGHT // 2 + 55, DIM_GREEN, font_sm)

        frames.append(_make_frame(img, glitch=i > 10))
        durations.append(100)

    # Phase 2: Explosion flash (10 frames)
    for i in range(10):
        flash = 255 - i * 25
        bg = (min(255, 100 + flash), 50, 30)
        img = Image.new("RGBA", (WIDTH, HEIGHT), (*bg, 255))
        draw = ImageDraw.Draw(img)

        # Shaking text effect
        offset_y = random.randint(-5, 5) if i < 5 else 0

        text = _corrupt_text("DETONATED", 0.3 if i < 5 else 0)
        _draw_text_centered(draw, text, HEIGHT // 2 - 30 + offset_y, NEON_RED, font_lg)

        if i > 3:
            _draw_text_centered(draw, f"POOL: {pool} JC", HEIGHT // 2 + 20, NEON_YELLOW, font_pool)

        frames.append(_make_frame(img, glitch=i < 5))
        durations.append(60)

    # Phase 3: Result display (15 frames)
    for i in range(15):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)

        _draw_text_centered(draw, "BOMB POT COMPLETE", 25, NEON_RED, font_lg)
        _draw_text_centered(draw, "=" * 32, 55, DIM_GREEN, font_sm)

        _draw_text_centered(draw, f"POOL: {pool} JC", HEIGHT // 2 - 20, NEON_YELLOW, font_pool)
        _draw_text_centered(draw, f"Contributors: {contributors}", HEIGHT // 2 + 25, DIM_GREEN, font_md)

        _draw_text_centered(draw, "=" * 32, HEIGHT // 2 + 55, DIM_GREEN, font_sm)
        _draw_text_centered(draw, "CONSENT: NOT REQUIRED", HEIGHT // 2 + 80, NEON_RED, font_sm)
        _draw_text_centered(draw, "ESCAPE: IMPOSSIBLE", HEIGHT // 2 + 100, NEON_RED, font_sm)

        _draw_text_centered(draw, "JOPA-T/v3.7", HEIGHT - 30, DIM_GREEN, font_sm)

        is_last = i == 14
        frames.append(_make_frame(img))
        durations.append(60000 if is_last else 200)

    return _save_gif(frames, durations)


def create_streak_record_gif(name: str, streak: int) -> io.BytesIO:
    """
    Streak Record GIF - Personal best win streak animation.
    Rising win counter with fireworks effect.
    ~45 frames, 400x300px.
    """
    frames = []
    durations = []
    font_lg = _get_font(20, bold=True)
    font_sm = _get_font(11)
    font_md = _get_font(14, bold=True)
    font_streak = _get_font(48, bold=True)

    # Phase 1: Counting up (20 frames)
    for i in range(20):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)

        _draw_text_centered(draw, "WIN STREAK", 20, NEON_GREEN, font_lg)
        _draw_text_centered(draw, f"Subject: {name}", 50, DIM_GREEN, font_sm)

        # Counting up
        display_streak = int(streak * (i / 19))
        _draw_text_centered(draw, str(display_streak), HEIGHT // 2 - 30, NEON_GREEN, font_streak)

        # Progress bar
        bar_x = 60
        bar_y = HEIGHT // 2 + 35
        bar_w = WIDTH - 120
        bar_h = 12
        draw.rectangle([bar_x, bar_y, bar_x + bar_w, bar_y + bar_h], outline=DIM_GREEN)
        fill_w = int(bar_w * i / 19)
        draw.rectangle([bar_x, bar_y, bar_x + fill_w, bar_y + bar_h], fill=NEON_GREEN)

        frames.append(_make_frame(img))
        durations.append(60)

    # Phase 2: Flash and reveal (10 frames)
    for i in range(10):
        flash = 150 - i * 15
        img = Image.new("RGBA", (WIDTH, HEIGHT), (10, flash // 3, 10, 255))
        draw = ImageDraw.Draw(img)

        _draw_text_centered(draw, "PERSONAL RECORD", 25, NEON_YELLOW, font_lg)
        _draw_text_centered(draw, str(streak), HEIGHT // 2 - 30, NEON_GREEN, font_streak)
        _draw_text_centered(draw, "CONSECUTIVE WINS", HEIGHT // 2 + 35, NEON_GREEN, font_md)

        # Sparkle effects
        if i < 7:
            for _ in range(5 + i):
                sx = random.randint(20, WIDTH - 20)
                sy = random.randint(20, HEIGHT - 20)
                sr = random.randint(2, 5)
                color = random.choice([NEON_GREEN, NEON_YELLOW, NEON_CYAN])
                draw.ellipse([sx - sr, sy - sr, sx + sr, sy + sr], fill=color)

        frames.append(_make_frame(img))
        durations.append(80)

    # Phase 3: Final display (15 frames)
    for i in range(15):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)

        # Border
        draw.rectangle([10, 10, WIDTH - 10, HEIGHT - 10], outline=NEON_GREEN, width=2)

        _draw_text_centered(draw, "ANOMALY DETECTED", 30, NEON_GREEN, font_lg)
        _draw_text_centered(draw, "=" * 30, 58, DIM_GREEN, font_sm)

        _draw_text_centered(draw, f"WIN x{streak}", HEIGHT // 2 - 25, NEON_GREEN, font_lg)
        _draw_text_centered(draw, "Status: UNPRECEDENTED", HEIGHT // 2 + 5, NEON_YELLOW, font_md)

        _draw_text_centered(draw, f"Subject: {name}", HEIGHT // 2 + 40, DIM_CYAN, font_sm)
        _draw_text_centered(draw, "=" * 30, HEIGHT // 2 + 65, DIM_GREEN, font_sm)

        _draw_text_centered(draw, "The algorithm adjusts.", HEIGHT - 55, DIM_GREEN, font_sm)
        _draw_text_centered(draw, "JOPA-T/v3.7", HEIGHT - 30, DIM_GREEN, font_sm)

        is_last = i == 14
        frames.append(_make_frame(img))
        durations.append(60000 if is_last else 200)

    return _save_gif(frames, durations)


def create_bigwin_gif(
    name: str, payout: int, *, source: str = "match", flavor: str = "bigwin"
) -> io.BytesIO:
    """
    Celebratory payout GIF — the win-side counterpart to the crash animations.

    Used for big wins across betting surfaces.
    source: "match" | "prediction" | "gamba"   (sets the context line)
    flavor: "bigwin" | "top_dog" | "underdog"   (sets the banner)
    """
    frames = []
    durations = []
    font_lg = _get_font(22, bold=True)
    font_md = _get_font(14, bold=True)
    font_sm = _get_font(12)

    source_line = {
        "match": "MATCH SETTLED",
        "prediction": "MARKET RESOLVED",
        "gamba": "TABLE PAYS OUT",
    }.get(source, "POSITION SETTLED")

    banner, accent = {
        "bigwin": ("PAYOUT CONFIRMED", NEON_GREEN),
        "top_dog": ("TOP OF THE BOOK", NEON_CYAN),
        "underdog": ("FADE THE PUBLIC", NEON_YELLOW),
    }.get(flavor, ("PAYOUT CONFIRMED", NEON_GREEN))

    # Phase 1: reconciling (8 frames)
    for i in range(8):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)
        _draw_text_centered(draw, "JOPA-T/v3.7 LEDGER", 24, NEON_GREEN, font_md)
        _draw_text_left(draw, f"> {source_line.lower()}...", 24, 70, DIM_GREEN, font_sm)
        _draw_text_left(draw, f"> client: {name}", 24, 92, DIM_GREEN, font_sm)
        if i % 2 == 0:
            _draw_text_left(draw, "> reconciling _", 24, 114, NEON_GREEN, font_sm)
        frames.append(_make_frame(img))
        durations.append(110)

    # Phase 2: payout counts up (18 frames)
    for i in range(18):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)
        prog = (i + 1) / 18
        shown = int(payout * prog)
        _draw_text_centered(draw, source_line, 32, DIM_GREEN, font_sm)
        _draw_text_centered(draw, f"+{shown:,}", HEIGHT // 2 - 22, accent, font_lg)
        _draw_text_centered(draw, "JOPACOIN", HEIGHT // 2 + 16, DIM_CYAN, font_sm)
        frames.append(_make_frame(img))
        durations.append(55 if prog < 0.9 else 95)

    # Phase 3: banner flash + jackpot spray (14 frames)
    for i in range(14):
        img = Image.new("RGBA", (WIDTH, HEIGHT), CRT_BLACK)
        draw = ImageDraw.Draw(img)
        flash = i % 2 == 0
        _draw_text_centered(draw, "=" * 30, 40, DIM_GREEN, font_sm)
        _draw_text_centered(draw, banner, 68, accent if flash else NEON_YELLOW, font_lg)
        _draw_text_centered(draw, "=" * 30, 104, DIM_GREEN, font_sm)
        _draw_text_centered(draw, f"+{payout:,} jc", 135, NEON_GREEN, font_md)
        _draw_text_centered(draw, f"client {name}", 165, DIM_GREEN, font_sm)
        if flash:
            for _ in range(30):
                sx = random.randint(20, WIDTH - 20)
                sy = random.randint(20, HEIGHT - 20)
                draw.point((sx, sy), fill=random.choice([NEON_GREEN, NEON_CYAN, NEON_YELLOW]))
        _draw_text_centered(draw, "JOPA-T/v3.7", HEIGHT - 24, DIM_GREEN, font_sm)
        is_last = i == 13
        frames.append(_make_frame(img, glitch=(i == 0)))
        durations.append(60000 if is_last else 110)

    return _save_gif(frames, durations)


def create_witch_curse_gif(target_name: str, *, stack_count: int = 1) -> io.BytesIO:
    """
    Witch's Curse hex GIF — green/violet hellfire erupting on a cursed client.

    The visual counterpart to create_bigwin_gif, in the witch palette. Pure PIL
    (no AI), fired only when a hexed target suffers a loss (see CurseService).
    stack_count (number of active casters) drives the banner + flame intensity.
    """
    frames = []
    durations = []
    font_lg = _get_font(22, bold=True)
    font_md = _get_font(14, bold=True)
    font_sm = _get_font(12)

    target_name = target_name[:22]
    stacks = max(1, stack_count)
    hexed_banner = f"HEXED x{stacks}" if stacks > 1 else "HEXED"
    flame_cols = min(stacks, 3)  # extra violet flame columns when ganged up
    particles = 24 + 12 * flame_cols  # denser ember spray with more casters
    embers = (WITCH_GREEN, WITCH_VIOLET, NEON_GREEN)

    # Phase 1: the hex manifests — glitchy corruption over the dark scrying glass (8 frames)
    for i in range(8):
        img = Image.new("RGBA", (WIDTH, HEIGHT), WITCH_BG)
        draw = ImageDraw.Draw(img)
        _draw_text_centered(draw, "JOPA-T/v3.7 GRIMOIRE", 24, WITCH_GREEN, font_md)
        _draw_text_left(draw, "> tracing the hex...", 24, 70, WITCH_DIM_GREEN, font_sm)
        _draw_text_left(draw, f"> client: {target_name}", 24, 92, WITCH_DIM_GREEN, font_sm)
        if i % 2 == 0:
            _draw_text_centered(
                draw, _corrupt_text("HEXED", 0.5), HEIGHT // 2, WITCH_VIOLET, font_lg
            )
        frames.append(_make_frame(img, glitch=(i % 2 == 0)))
        durations.append(110)

    # Phase 2: green witchfire rises with violet embers (18 frames)
    for i in range(18):
        img = Image.new("RGBA", (WIDTH, HEIGHT), WITCH_BG)
        draw = ImageDraw.Draw(img)
        prog = (i + 1) / 18
        _draw_text_centered(draw, "HEX PROPAGATING", 30, WITCH_DIM_GREEN, font_sm)
        _draw_text_centered(draw, target_name, HEIGHT // 2 - 8, WITCH_GREEN, font_lg)
        flame_h = max(1, int(prog * (HEIGHT // 2)))
        for c in range(flame_cols):
            base_x = WIDTH // 2 + (c - (flame_cols - 1) / 2) * 90
            for _ in range(particles):
                fx = int(base_x + random.randint(-30, 30))
                fy = HEIGHT - random.randint(0, flame_h)
                draw.point((fx, fy), fill=random.choice(embers))
        frames.append(_make_frame(img))
        durations.append(60 if prog < 0.9 else 95)

    # Phase 3: HEXED banner flash + ember spray (14 frames)
    for i in range(14):
        img = Image.new("RGBA", (WIDTH, HEIGHT), WITCH_BG)
        draw = ImageDraw.Draw(img)
        flash = i % 2 == 0
        _draw_text_centered(draw, "~" * 28, 44, WITCH_DIM_GREEN, font_sm)
        _draw_text_centered(draw, hexed_banner, 70, WITCH_VIOLET if flash else WITCH_GREEN, font_lg)
        _draw_text_centered(draw, "~" * 28, 106, WITCH_DIM_GREEN, font_sm)
        _draw_text_centered(draw, f"client {target_name}", 140, WITCH_GREEN, font_md)
        if flash:
            for _ in range(particles):
                sx = random.randint(20, WIDTH - 20)
                sy = random.randint(20, HEIGHT - 20)
                draw.point((sx, sy), fill=random.choice((WITCH_GREEN, WITCH_VIOLET)))
        _draw_text_centered(draw, "JOPA-T/v3.7", HEIGHT - 24, WITCH_DIM_GREEN, font_sm)
        is_last = i == 13
        frames.append(_make_frame(img, glitch=flash))
        durations.append(60000 if is_last else 110)

    return _save_gif(frames, durations)
