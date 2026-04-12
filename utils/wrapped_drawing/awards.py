"""Individual award cards and grid layout."""

from __future__ import annotations

import io
from typing import TYPE_CHECKING

from PIL import Image, ImageDraw
from pilmoji import Pilmoji

from utils.wrapped_drawing._common import (
    ACCENT_GOLD,
    BG_GRADIENT_END,
    BG_GRADIENT_START,
    CATEGORY_COLORS,
    TEXT_GREY,
    TEXT_WHITE,
    _draw_gradient_background,
    _draw_rounded_rect,
    _get_font,
    _word_wrap,
)

if TYPE_CHECKING:
    from services.wrapped_service import Award


def draw_wrapped_award(award: Award, hero_names: dict[int, str] | None = None) -> io.BytesIO:
    """Generate a single award card."""
    width, height = 400, 300
    img = Image.new("RGB", (width, height), BG_GRADIENT_START)
    draw = ImageDraw.Draw(img)

    accent_color = CATEGORY_COLORS.get(award.category, ACCENT_GOLD)

    _draw_gradient_background(draw, width, height, BG_GRADIENT_START, BG_GRADIENT_END)

    emoji_font = _get_font(48)
    title_font = _get_font(28, bold=True)
    name_font = _get_font(22, bold=True)
    stat_font = _get_font(18)
    flavor_font = _get_font(14)

    if award.emoji:
        with Pilmoji(img) as pilmoji:
            pilmoji.text(((width - 48) // 2, 25), award.emoji, font=emoji_font)

    bbox = draw.textbbox((0, 0), award.title.upper(), font=title_font)
    text_w = bbox[2] - bbox[0]
    draw.text(((width - text_w) // 2, 95), award.title.upper(), fill=accent_color, font=title_font)

    player_text = f"@{award.discord_username}"
    bbox = draw.textbbox((0, 0), player_text, font=name_font)
    text_w = bbox[2] - bbox[0]
    draw.text(((width - text_w) // 2, 140), player_text, fill=TEXT_WHITE, font=name_font)

    stat_text = award.stat_value
    bbox = draw.textbbox((0, 0), stat_text, font=stat_font)
    text_w = bbox[2] - bbox[0]
    draw.text(((width - text_w) // 2, 180), stat_text, fill=ACCENT_GOLD, font=stat_font)

    if award.flavor_text:
        bbox = draw.textbbox((0, 0), f'"{award.flavor_text}"', font=flavor_font)
        text_w = bbox[2] - bbox[0]
        draw.text(
            ((width - text_w) // 2, 220),
            f'"{award.flavor_text}"',
            fill=TEXT_GREY,
            font=flavor_font,
        )

    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer


def draw_awards_grid(
    awards: list[Award],
    max_awards: int = 6,
    viewer_discord_id: int | None = None,
) -> io.BytesIO:
    """Generate a grid of award cards. Highlights cards won by ``viewer_discord_id``."""
    awards = awards[:max_awards]
    if not awards:
        img = Image.new("RGB", (800, 200), BG_GRADIENT_START)
        draw = ImageDraw.Draw(img)
        font = _get_font(20)
        draw.text((300, 90), "No awards yet!", fill=TEXT_GREY, font=font)
        buffer = io.BytesIO()
        img.save(buffer, format="PNG")
        buffer.seek(0)
        return buffer

    cols = min(3, len(awards))
    rows = (len(awards) + cols - 1) // cols

    card_width, card_height = 250, 220
    padding = 20
    total_width = cols * card_width + (cols + 1) * padding
    total_height = rows * card_height + (rows + 1) * padding + 60

    img = Image.new("RGB", (total_width, total_height), BG_GRADIENT_START)
    draw = ImageDraw.Draw(img)

    _draw_gradient_background(draw, total_width, total_height, BG_GRADIENT_START, BG_GRADIENT_END)

    header_font = _get_font(24, bold=True)
    draw.text((padding, 15), "AWARDS", fill=ACCENT_GOLD, font=header_font)

    emoji_font = _get_font(24)
    title_font = _get_font(14, bold=True)
    name_font = _get_font(12, bold=True)
    stat_font = _get_font(11)
    flavor_font = _get_font(10)

    text_max_w = card_width - 20

    for i, award in enumerate(awards):
        row = i // cols
        col = i % cols
        x = padding + col * (card_width + padding)
        y = 60 + padding + row * (card_height + padding)

        is_viewer = viewer_discord_id is not None and award.discord_id == viewer_discord_id
        accent_color = CATEGORY_COLORS.get(award.category, ACCENT_GOLD)
        card_fill = (50, 45, 30) if is_viewer else (40, 40, 50)
        card_outline = ACCENT_GOLD if is_viewer else accent_color
        card_border_width = 3 if is_viewer else 2
        _draw_rounded_rect(
            draw,
            (x, y, x + card_width, y + card_height),
            radius=10,
            fill=card_fill,
            outline=card_outline,
            width=card_border_width,
        )

        if is_viewer:
            star_font = _get_font(12, bold=True)
            draw.text((x + card_width - 40, y + 8), "YOU", fill=ACCENT_GOLD, font=star_font)

        if award.emoji:
            with Pilmoji(img) as pilmoji:
                pilmoji.text((x + 10, y + 10), award.emoji, font=emoji_font)

        title_text = award.title.upper()
        title_w = draw.textlength(title_text, font=title_font)
        title_max = card_width - 55
        if title_w > title_max:
            while draw.textlength(title_text + "..", font=title_font) > title_max and len(title_text) > 1:
                title_text = title_text[:-1]
            title_text = title_text.rstrip() + ".."
        draw.text((x + 45, y + 14), title_text, fill=accent_color, font=title_font)

        player_text = f"@{award.discord_username}"
        player_w = draw.textlength(player_text, font=name_font)
        if player_w > text_max_w:
            while draw.textlength(player_text + "..", font=name_font) > text_max_w and len(player_text) > 1:
                player_text = player_text[:-1]
            player_text = player_text.rstrip() + ".."
        draw.text((x + 10, y + 55), player_text, fill=TEXT_WHITE, font=name_font)

        stat_text = award.stat_value
        stat_w = draw.textlength(stat_text, font=stat_font)
        if stat_w > text_max_w:
            while draw.textlength(stat_text + "..", font=stat_font) > text_max_w and len(stat_text) > 1:
                stat_text = stat_text[:-1]
            stat_text = stat_text.rstrip() + ".."
        draw.text((x + 10, y + 80), stat_text, fill=ACCENT_GOLD, font=stat_font)

        if award.flavor_text:
            flavor = f'"{award.flavor_text}"'
            lines = _word_wrap(flavor, flavor_font, text_max_w, draw)
            for li, line in enumerate(lines[:3]):
                draw.text((x + 10, y + 110 + li * 16), line, fill=TEXT_GREY, font=flavor_font)

    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer
