"""Server + personal summary cards (summary, personal, summary_stats, records)."""

from __future__ import annotations

import io
from typing import TYPE_CHECKING

from PIL import Image, ImageDraw

from utils.wrapped_drawing._common import (
    ACCENT_GOLD,
    ACCENT_GREEN,
    ACCENT_RED,
    BG_GRADIENT_END,
    BG_GRADIENT_START,
    NA_COLOR,
    TEXT_DARK,
    TEXT_GREY,
    TEXT_WHITE,
    WORST_LABEL_COLOR,
    _center_text,
    _draw_gradient_background,
    _draw_rgba_gradient_background,
    _draw_rounded_rect,
    _draw_wrapped_header,
    _get_font,
)

if TYPE_CHECKING:
    from services.wrapped_service import PersonalRecord, PlayerWrapped, ServerWrapped


def draw_wrapped_summary(wrapped: ServerWrapped, hero_names: dict[int, str] | None = None) -> io.BytesIO:
    """Generate the main wrapped summary card for a server."""
    width, height = 800, 600
    img = Image.new("RGB", (width, height), BG_GRADIENT_START)
    draw = ImageDraw.Draw(img)

    _draw_gradient_background(draw, width, height, BG_GRADIENT_START, BG_GRADIENT_END)

    title_font = _get_font(42, bold=True)
    subtitle_font = _get_font(24, bold=True)
    large_font = _get_font(36, bold=True)
    medium_font = _get_font(20)
    small_font = _get_font(16)

    header_text = "CAMA WRAPPED"
    bbox = draw.textbbox((0, 0), header_text, font=title_font)
    text_w = bbox[2] - bbox[0]
    draw.text(((width - text_w) // 2, 30), header_text, fill=ACCENT_GOLD, font=title_font)

    month_text = wrapped.year_label.upper()
    bbox = draw.textbbox((0, 0), month_text, font=subtitle_font)
    text_w = bbox[2] - bbox[0]
    draw.text(((width - text_w) // 2, 85), month_text, fill=TEXT_WHITE, font=subtitle_font)

    draw.line([(50, 130), (width - 50, 130)], fill=ACCENT_GOLD, width=2)

    stats_y = 160
    stats = [
        (f"{wrapped.total_matches}", "MATCHES"),
        (f"{wrapped.unique_heroes}", "UNIQUE HEROES"),
        (f"{wrapped.total_wagered:,}", "JC WAGERED"),
    ]

    stat_width = (width - 100) // len(stats)
    for i, (value, label) in enumerate(stats):
        x = 50 + i * stat_width + stat_width // 2

        bbox = draw.textbbox((0, 0), value, font=large_font)
        text_w = bbox[2] - bbox[0]
        draw.text((x - text_w // 2, stats_y), value, fill=ACCENT_GOLD, font=large_font)

        bbox = draw.textbbox((0, 0), label, font=small_font)
        text_w = bbox[2] - bbox[0]
        draw.text((x - text_w // 2, stats_y + 45), label, fill=TEXT_GREY, font=small_font)

    top_y = 270
    if wrapped.top_players:
        top = wrapped.top_players[0]
        draw.text((50, top_y), "TOP PERFORMER", fill=TEXT_GREY, font=small_font)

        player_name = f"@{top['discord_username']}"
        draw.text((50, top_y + 25), player_name, fill=TEXT_WHITE, font=subtitle_font)

        rating_text = f"{top['wins']}W {top['games_played'] - top['wins']}L ({top['win_rate']*100:.0f}% WR)"
        draw.text((50, top_y + 55), rating_text, fill=ACCENT_GREEN, font=medium_font)

    hero_y = 380
    if wrapped.most_played_heroes:
        top_hero = wrapped.most_played_heroes[0]
        hero_name = hero_names.get(top_hero["hero_id"], f"Hero #{top_hero['hero_id']}") if hero_names else f"Hero #{top_hero['hero_id']}"
        draw.text((50, hero_y), "MOST PLAYED", fill=TEXT_GREY, font=small_font)
        draw.text((50, hero_y + 25), hero_name, fill=TEXT_WHITE, font=subtitle_font)
        draw.text(
            (50, hero_y + 55),
            f"{top_hero['picks']} picks ({top_hero['win_rate']*100:.0f}% WR)",
            fill=TEXT_GREY,
            font=medium_font,
        )

    if wrapped.best_hero:
        hero_name = hero_names.get(wrapped.best_hero["hero_id"], f"Hero #{wrapped.best_hero['hero_id']}") if hero_names else f"Hero #{wrapped.best_hero['hero_id']}"
        draw.text((width // 2 + 50, hero_y), "BEST WIN RATE", fill=TEXT_GREY, font=small_font)
        draw.text((width // 2 + 50, hero_y + 25), hero_name, fill=TEXT_WHITE, font=subtitle_font)
        draw.text(
            (width // 2 + 50, hero_y + 55),
            f"{wrapped.best_hero['win_rate']*100:.0f}% ({wrapped.best_hero['picks']} games)",
            fill=ACCENT_GREEN,
            font=medium_font,
        )

    footer_text = f"{wrapped.unique_players} players participated"
    bbox = draw.textbbox((0, 0), footer_text, font=small_font)
    text_w = bbox[2] - bbox[0]
    draw.text(((width - text_w) // 2, height - 40), footer_text, fill=TEXT_GREY, font=small_font)

    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer


def draw_wrapped_personal(
    player_wrapped: PlayerWrapped, hero_names: dict[int, str] | None = None
) -> io.BytesIO:
    """Generate a personal wrapped card for a player."""
    width, height = 800, 450
    img = Image.new("RGB", (width, height), BG_GRADIENT_START)
    draw = ImageDraw.Draw(img)

    _draw_gradient_background(draw, width, height, BG_GRADIENT_START, BG_GRADIENT_END)

    title_font = _get_font(32, bold=True)
    large_font = _get_font(28, bold=True)
    medium_font = _get_font(18)
    small_font = _get_font(14)

    draw.text((30, 20), "YOUR WRAPPED", fill=TEXT_GREY, font=medium_font)
    draw.text((30, 45), f"@{player_wrapped.discord_username}", fill=TEXT_WHITE, font=title_font)

    draw.line([(30, 95), (width - 30, 95)], fill=ACCENT_GOLD, width=2)

    stats_y = 115
    stats = [
        (f"{player_wrapped.games_played}", "GAMES"),
        (f"{player_wrapped.win_rate*100:.0f}%", "WIN RATE"),
        (
            f"+{player_wrapped.rating_change}" if player_wrapped.rating_change >= 0 else f"{player_wrapped.rating_change}",
            "RATING",
        ),
    ]

    stat_width = (width - 60) // len(stats)
    for i, (value, label) in enumerate(stats):
        x = 30 + i * stat_width + stat_width // 2

        color = ACCENT_GREEN if (label == "RATING" and player_wrapped.rating_change >= 0) else (
            ACCENT_RED if label == "RATING" else ACCENT_GOLD
        )
        bbox = draw.textbbox((0, 0), value, font=large_font)
        text_w = bbox[2] - bbox[0]
        draw.text((x - text_w // 2, stats_y), value, fill=color, font=large_font)

        bbox = draw.textbbox((0, 0), label, font=small_font)
        text_w = bbox[2] - bbox[0]
        draw.text((x - text_w // 2, stats_y + 35), label, fill=TEXT_GREY, font=small_font)

    hero_y = 200
    draw.text((30, hero_y), "TOP HEROES", fill=TEXT_GREY, font=small_font)

    if player_wrapped.top_heroes:
        for i, hero in enumerate(player_wrapped.top_heroes[:3]):
            y = hero_y + 25 + i * 28
            hero_name = hero_names.get(hero["hero_id"], f"Hero #{hero['hero_id']}") if hero_names else f"Hero #{hero['hero_id']}"

            draw.text((30, y), f"{i + 1}.", fill=ACCENT_GOLD, font=medium_font)
            draw.text((55, y), hero_name, fill=TEXT_WHITE, font=medium_font)

            stats_text = f"{hero['picks']}g {hero['win_rate']*100:.0f}%"
            draw.text((250, y), stats_text, fill=TEXT_GREY, font=medium_font)

    betting_y = 200
    draw.text((width // 2 + 30, betting_y), "BETTING", fill=TEXT_GREY, font=small_font)

    bet_stats = [
        (f"{player_wrapped.total_bets}", "BETS"),
        (
            f"+{player_wrapped.betting_pnl}" if player_wrapped.betting_pnl >= 0 else f"{player_wrapped.betting_pnl}",
            "P&L",
        ),
    ]

    if player_wrapped.degen_score is not None:
        bet_stats.append((f"{player_wrapped.degen_score}", "DEGEN"))

    for i, (value, label) in enumerate(bet_stats):
        y = betting_y + 25 + i * 35
        color = ACCENT_GREEN if (label == "P&L" and player_wrapped.betting_pnl >= 0) else (
            ACCENT_RED if label == "P&L" else TEXT_WHITE
        )
        draw.text((width // 2 + 30, y), f"{label}: ", fill=TEXT_GREY, font=medium_font)

        bbox = draw.textbbox((0, 0), f"{label}: ", font=medium_font)
        label_w = bbox[2] - bbox[0]
        draw.text((width // 2 + 30 + label_w, y), value, fill=color, font=medium_font)

    footer_text = f"W: {player_wrapped.wins} | L: {player_wrapped.losses}"
    bbox = draw.textbbox((0, 0), footer_text, font=small_font)
    text_w = bbox[2] - bbox[0]
    draw.text(((width - text_w) // 2, height - 35), footer_text, fill=TEXT_GREY, font=small_font)

    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer


def draw_summary_stats_slide(
    username: str,
    year_label: str,
    stats_list: list[tuple[str, str, str, tuple[int, int, int]]],
) -> io.BytesIO:
    """Draw a 2x3 summary stats grid."""
    width, height = 800, 600
    img = Image.new("RGBA", (width, height), (*BG_GRADIENT_START, 255))
    draw = ImageDraw.Draw(img)

    _draw_rgba_gradient_background(draw, width, height)

    _draw_wrapped_header(draw, username, year_label, width)

    title_font = _get_font(20, bold=True)
    _center_text(draw, "YOUR STATS", title_font, 50, width, ACCENT_GOLD)

    draw.line([(50, 80), (width - 50, 80)], fill=(*ACCENT_GOLD, 128), width=1)

    value_font = _get_font(32, bold=True)
    label_font = _get_font(14, bold=True)
    quip_font = _get_font(12)

    cols = 2
    cell_w = (width - 80) // cols
    cell_h = 150
    start_y = 100

    for i, (value, label, quip, color) in enumerate(stats_list[:6]):
        col = i % cols
        row = i // cols
        cx = 40 + col * cell_w + cell_w // 2
        cy = start_y + row * cell_h

        card_x1 = 40 + col * cell_w + 10
        card_y1 = cy
        card_x2 = card_x1 + cell_w - 20
        card_y2 = cy + cell_h - 20
        _draw_rounded_rect(draw, (card_x1, card_y1, card_x2, card_y2), radius=8, fill=(40, 40, 50))

        bbox = draw.textbbox((0, 0), value, font=value_font)
        tw = bbox[2] - bbox[0]
        draw.text((cx - tw // 2, cy + 15), value, fill=color, font=value_font)

        bbox = draw.textbbox((0, 0), label, font=label_font)
        tw = bbox[2] - bbox[0]
        draw.text((cx - tw // 2, cy + 60), label, fill=TEXT_WHITE, font=label_font)

        if quip:
            bbox = draw.textbbox((0, 0), quip, font=quip_font)
            tw = bbox[2] - bbox[0]
            draw.text((cx - tw // 2, cy + 82), quip, fill=TEXT_GREY, font=quip_font)

    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer


def draw_records_slide(
    slide_title: str,
    accent_color: tuple[int, int, int],
    records: list[PersonalRecord],
    username: str,
    year_label: str,
    slide_number: int,
    total_slides: int,
    hero_names: dict[int, str],
) -> io.BytesIO:
    """Generate a single records slide image with hero thumbnails + stats."""
    from utils.drawing import _fetch_hero_image

    width, height = 800, 600
    img = Image.new("RGBA", (width, height), (*BG_GRADIENT_START, 255))
    draw = ImageDraw.Draw(img)

    _draw_rgba_gradient_background(draw, width, height)

    header_font = _get_font(24, bold=True)
    subheader_font = _get_font(16)
    slide_title_font = _get_font(20, bold=True)
    label_font = _get_font(16)
    value_font = _get_font(20, bold=True)
    info_font = _get_font(12)

    draw.text((30, 20), f"{username}'s Records", fill=TEXT_WHITE, font=header_font)
    draw.text((30, 50), f"— {year_label}", fill=TEXT_GREY, font=subheader_font)

    draw.text((30, 80), slide_title.upper(), fill=accent_color, font=slide_title_font)

    draw.line([(30, 108), (width - 30, 108)], fill=(*accent_color, 128), width=1)

    y_start = 120
    row_height = 73
    max_records = 6

    for i, record in enumerate(records[:max_records]):
        y = y_start + i * row_height
        is_na = record.value is None or record.display_value == "N/A"

        hero_x = 30
        if record.hero_id and not is_na:
            try:
                hero_img = _fetch_hero_image(record.hero_id, (48, 27))
                if hero_img:
                    if hero_img.mode != "RGBA":
                        hero_img = hero_img.convert("RGBA")
                    hero_y_offset = (row_height - 27) // 2
                    img.paste(hero_img, (hero_x, y + hero_y_offset), hero_img)
            except Exception:
                pass

        label_x = 90
        if is_na:
            label_color = NA_COLOR
            value_color = NA_COLOR
        elif record.is_worst:
            label_color = WORST_LABEL_COLOR
            value_color = WORST_LABEL_COLOR
        else:
            label_color = TEXT_WHITE
            value_color = accent_color

        draw.text((label_x, y + 4), record.stat_label, fill=label_color, font=label_font)

        display = record.display_value if not is_na else "N/A"
        draw.text((label_x, y + 26), display, fill=value_color, font=value_font)

        if record.valve_match_id and not is_na:
            match_info = f"Match #{record.valve_match_id}"
            if record.match_date:
                match_info += f" · {record.match_date}"
            draw.text((label_x, y + 52), match_info, fill=TEXT_DARK, font=info_font)
        elif record.match_date and not is_na:
            draw.text((label_x, y + 52), record.match_date, fill=TEXT_DARK, font=info_font)

        if record.hero_id and not is_na:
            hero_name = hero_names.get(record.hero_id, "")
            if hero_name:
                bbox = draw.textbbox((0, 0), hero_name, font=info_font)
                name_w = bbox[2] - bbox[0]
                draw.text((width - 30 - name_w, y + 8), hero_name, fill=TEXT_GREY, font=info_font)

    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer
