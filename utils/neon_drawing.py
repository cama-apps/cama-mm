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

# ---------------------------------------------------------------------------
# Neon color palette
# ---------------------------------------------------------------------------
NEON_GREEN = (0, 255, 65)
NEON_CYAN = (0, 255, 255)
NEON_PINK = (255, 0, 128)
NEON_RED = (255, 30, 30)
NEON_YELLOW = (255, 220, 0)
CRT_BLACK = (10, 10, 15)
CRT_DARK = (18, 18, 24)
DIM_GREEN = (0, 120, 30)
DIM_CYAN = (0, 100, 100)

# Standard size
WIDTH = 400
HEIGHT = 300

# Font cache
_FONT_CACHE: dict[str, ImageFont.FreeTypeFont | ImageFont.ImageFont] = {}


def _get_font(size: int, bold: bool = False) -> ImageFont.FreeTypeFont | ImageFont.ImageFont:
    """Get a cached monospace font."""
    key = f"{'bold' if bold else 'regular'}_{size}"
    if key not in _FONT_CACHE:
        try:
            name = "DejaVuSansMono-Bold.ttf" if bold else "DejaVuSansMono.ttf"
            path = f"/usr/share/fonts/truetype/dejavu/{name}"
            _FONT_CACHE[key] = ImageFont.truetype(path, size)
        except OSError:
            try:
                name = "DejaVuSans-Bold.ttf" if bold else "DejaVuSans.ttf"
                path = f"/usr/share/fonts/truetype/dejavu/{name}"
                _FONT_CACHE[key] = ImageFont.truetype(path, size)
            except OSError:
                _FONT_CACHE[key] = ImageFont.load_default()
    return _FONT_CACHE[key]


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
    """Apply CRT effects and convert to palette mode for GIF."""
    if apply_crt:
        img = _apply_scanlines(img)
        img = _apply_phosphor_glow(img)
    if glitch:
        img = _apply_glitch_lines(img, num_lines=random.randint(3, 10))
    return img.convert("RGB").convert("P", palette=Image.ADAPTIVE, colors=256)


def _save_gif(frames: list[Image.Image], durations: list[int]) -> io.BytesIO:
    """Save frames as GIF to BytesIO buffer."""
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


# ---------------------------------------------------------------------------
# GIF Generators
# ---------------------------------------------------------------------------

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
        _draw_text_left(draw, f"> Status: PROCESSING", 20, 100, NEON_YELLOW, font_sm)
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

        # Pulsing glow
        alpha = int(128 + 127 * math.sin(i * 0.5))
        glow_color = (NEON_GREEN[0], NEON_GREEN[1], NEON_GREEN[2], alpha)

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
