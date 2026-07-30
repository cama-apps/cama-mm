"""Animated blame selector."""

from __future__ import annotations

import io
from collections.abc import Sequence

from PIL import Image, ImageDraw

from utils.fonts import get_font

BLAME_LUKE_REASONS: tuple[str, ...] = (
    "Luke hosted a lobby while invisible",
    "Luke marked himself ready and vanished",
    "Egg of dog",
    "Luke deleted his messages",
    "Luke pinned his own message and called it evidence.",
    "Luke changed the rules halfway through enforcing them",
    "Luke deleted the evidence and posted a GIF",
    "Luke moved everyone to another channel except himself",
    "Luke made a poll and ignored the result",
    "Luke gave the role to the wrong person",
    "Luke blamed the permissions he configured",
    "Luke posted a message in a read-only channel",
    "Luke abused admin perms to gamba",
    "ash",
    "Luke forgot to update roles",
    "Luke: exists",
    "Luke is still not immortal"
)

_CANVAS_SIZE = (720, 420)
_BACKGROUND = (13, 18, 29)
_PAPER = (238, 232, 211)
_INK = (26, 31, 39)
_MUTED_INK = (92, 94, 91)
_ACCENT = (245, 187, 54)
_DANGER = (196, 48, 54)


def _wrapped_lines(
    draw: ImageDraw.ImageDraw,
    text: str,
    *,
    font,
    max_width: int,
) -> list[str]:
    """Wrap text using measured pixel width rather than a fixed character count."""
    words = text.split()
    lines: list[str] = []
    current = ""
    for word in words:
        candidate = f"{current} {word}".strip()
        if current and draw.textbbox((0, 0), candidate, font=font)[2] > max_width:
            lines.append(current)
            current = word
        else:
            current = candidate
    if current:
        lines.append(current)
    return lines


def _draw_pin(draw: ImageDraw.ImageDraw, x: int, y: int) -> None:
    """Draw a chunky red pushpin over the selected blame card."""
    draw.ellipse((x - 18, y - 12, x + 18, y + 24), fill=(95, 21, 25))
    draw.ellipse((x - 18, y - 18, x + 18, y + 18), fill=(224, 61, 68))
    draw.ellipse((x - 9, y - 12, x + 5, y + 2), fill=(255, 127, 127))
    draw.polygon(((x - 5, y + 17), (x + 5, y + 17), (x, y + 51)), fill=(174, 178, 181))


def _draw_stamp(image: Image.Image) -> None:
    stamp_width = 500
    stamp = Image.new("RGBA", (stamp_width, 68), (0, 0, 0, 0))
    draw = ImageDraw.Draw(stamp)
    font = get_font(27, bold=True)
    draw.rounded_rectangle(
        (3, 3, stamp_width - 4, 64),
        radius=8,
        outline=(*_DANGER, 225),
        width=5,
    )
    label = "OFFICIALLY PINNED ON LUKE"
    box = draw.textbbox((0, 0), label, font=font)
    draw.text(
        ((stamp_width - (box[2] - box[0])) / 2, 16),
        label,
        font=font,
        fill=(*_DANGER, 235),
    )
    stamp = stamp.rotate(2, resample=Image.Resampling.BICUBIC, expand=True)
    image.alpha_composite(stamp, (108, 311))


def _draw_frame(
    reason: str,
    *,
    candidate_index: int,
    candidate_count: int,
    final: bool,
    pin_progress: float = 0.0,
) -> Image.Image:
    image = Image.new("RGBA", _CANVAS_SIZE, (*_BACKGROUND, 255))
    draw = ImageDraw.Draw(image)

    title_font = get_font(23, bold=True, mono=True)
    label_font = get_font(15, bold=True, mono=True)
    reason_font = get_font(38, bold=True)
    footer_font = get_font(15, mono=True)

    draw.rectangle((0, 0, 720, 7), fill=_ACCENT)
    draw.text(
        (36, 27),
        "MINISTRY OF ACCOUNTABILITY",
        font=title_font,
        fill=(244, 245, 247),
    )
    draw.text(
        (36, 61),
        "OFFICIAL BLAME ALLOCATION DEPARTMENT",
        font=footer_font,
        fill=(131, 145, 166),
    )

    card_box = (36, 101, 684, 326)
    draw.rounded_rectangle(
        (42, 108, 690, 333),
        radius=14,
        fill=(4, 7, 12, 90),
    )
    draw.rounded_rectangle(
        card_box,
        radius=14,
        fill=_PAPER,
        outline=_DANGER if final else _ACCENT,
        width=5 if final else 3,
    )

    state_label = "FINAL FINDING" if final else "SCANNING PLAUSIBLE CAUSE"
    draw.text((63, 125), state_label, font=label_font, fill=_DANGER if final else _MUTED_INK)
    counter = f"{candidate_index + 1:02d}/{candidate_count:02d}"
    counter_box = draw.textbbox((0, 0), counter, font=label_font)
    draw.text(
        (655 - (counter_box[2] - counter_box[0]), 125),
        counter,
        font=label_font,
        fill=_MUTED_INK,
    )
    draw.line((63, 153, 657, 153), fill=(180, 174, 155), width=2)

    lines = _wrapped_lines(draw, reason, font=reason_font, max_width=560)
    line_height = 50
    block_height = len(lines) * line_height
    y = 175 + max(0, (120 - block_height) // 2)
    for line in lines:
        box = draw.textbbox((0, 0), line, font=reason_font)
        x = 360 - (box[2] - box[0]) / 2
        draw.text((x, y), line, font=reason_font, fill=_INK)
        y += line_height

    if pin_progress > 0:
        pin_y = int(96 + (1 - pin_progress) * -100)
        _draw_pin(draw, 360, pin_y)

    if final:
        _draw_stamp(image)
        draw = ImageDraw.Draw(image)
        footer = "CASE CLOSED  •  EVIDENCE OPTIONAL  •  APPEALS DENIED"
        footer_color = (231, 115, 119)
        footer_y = 397
    else:
        footer = "CROSS-REFERENCING CHAT LOGS, VIBES, AND RECENT LUKE ACTIVITY..."
        footer_color = (131, 145, 166)
        footer_y = 381
    footer_box = draw.textbbox((0, 0), footer, font=footer_font)
    draw.text(
        (360 - (footer_box[2] - footer_box[0]) / 2, footer_y),
        footer,
        font=footer_font,
        fill=footer_color,
    )
    return image.convert("P", palette=Image.Palette.ADAPTIVE, colors=128)


def create_blame_luke_gif(
    selected_reason: str,
    *,
    reasons: Sequence[str] = BLAME_LUKE_REASONS,
) -> io.BytesIO:
    """Create a one-shot GIF that scans the reasons and pins ``selected_reason``."""
    reason_list = tuple(reasons)
    if not reason_list:
        raise ValueError("At least one blame reason is required")
    if selected_reason not in reason_list:
        raise ValueError("Selected blame reason must be present in reasons")

    selected_index = reason_list.index(selected_reason)
    # Start immediately after the winner so one complete pass naturally lands on it.
    ordered = reason_list[selected_index + 1 :] + reason_list[: selected_index + 1]
    frames = [
        _draw_frame(
            reason,
            candidate_index=reason_list.index(reason),
            candidate_count=len(reason_list),
            final=False,
        )
        for reason in ordered
    ]
    durations = [max(75, 170 - (index * 7)) for index in range(len(frames))]

    for progress, duration in ((0.25, 90), (0.6, 110), (1.0, 160)):
        frames.append(
            _draw_frame(
                selected_reason,
                candidate_index=selected_index,
                candidate_count=len(reason_list),
                final=False,
                pin_progress=progress,
            )
        )
        durations.append(duration)
    frames.append(
        _draw_frame(
            selected_reason,
            candidate_index=selected_index,
            candidate_count=len(reason_list),
            final=True,
            pin_progress=1.0,
        )
    )
    durations.append(8_000)

    buffer = io.BytesIO()
    frames[0].save(
        buffer,
        format="GIF",
        save_all=True,
        append_images=frames[1:],
        duration=durations,
        optimize=False,
        disposal=2,
    )
    buffer.seek(0)
    return buffer
