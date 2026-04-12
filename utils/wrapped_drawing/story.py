"""Story slides (story beats, pairwise, hero spotlight, lane breakdown, packages, chart wrap)."""

from __future__ import annotations

import io

from PIL import Image, ImageDraw

from utils.wrapped_drawing._common import (
    ACCENT_GOLD,
    ACCENT_GREEN,
    ACCENT_PURPLE,
    ACCENT_RED,
    BG_GRADIENT_START,
    TEXT_DARK,
    TEXT_GREY,
    TEXT_WHITE,
    _center_text,
    _draw_rgba_gradient_background,
    _draw_rounded_rect,
    _draw_wrapped_header,
    _get_font,
)


def draw_story_slide(
    headline: str,
    stat_value: str,
    stat_label: str,
    flavor_text: str,
    accent_color: tuple[int, int, int],
    username: str,
    year_label: str,
    comparisons: list[str] | None = None,
) -> io.BytesIO:
    """Draw a big-number story reveal slide.

    Used for: Your Year, Rating Story, Gamba Story slides.
    """
    width, height = 800, 600
    img = Image.new("RGBA", (width, height), (*BG_GRADIENT_START, 255))
    draw = ImageDraw.Draw(img)

    _draw_rgba_gradient_background(draw, width, height)

    headline_font = _get_font(18, bold=True)
    big_font = _get_font(72, bold=True)
    label_font = _get_font(22)
    flavor_font = _get_font(16)
    comparison_font = _get_font(16)

    _draw_wrapped_header(draw, username, year_label, width)

    _center_text(draw, headline.upper(), headline_font, 80, width, TEXT_GREY)
    _center_text(draw, stat_value, big_font, 140, width, accent_color)
    _center_text(draw, stat_label, label_font, 230, width, TEXT_WHITE)

    if comparisons:
        y_pos = 290
        for comp in comparisons:
            _center_text(draw, comp, comparison_font, y_pos, width, TEXT_GREY)
            y_pos += 30

    if flavor_text:
        _center_text(draw, f'"{flavor_text}"', flavor_font, 480, width, TEXT_GREY)

    draw.line([(100, 540), (width - 100, 540)], fill=(*accent_color, 128), width=2)

    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer


def draw_pairwise_slide(
    username: str,
    year_label: str,
    entries: list[dict],
    slide_type: str = "teammates",
    avatar_images: dict[int, bytes] | None = None,
    section_labels: list[tuple[int, str]] | None = None,
) -> io.BytesIO:
    """Draw a pairwise teammates or rivals slide."""
    width, height = 800, 600
    img = Image.new("RGBA", (width, height), (*BG_GRADIENT_START, 255))
    draw = ImageDraw.Draw(img)

    _draw_rgba_gradient_background(draw, width, height)

    accent = ACCENT_GREEN if slide_type == "teammates" else ACCENT_RED
    title = "YOUR TEAMMATES" if slide_type == "teammates" else "YOUR RIVALS"
    subtitle = "All-time pairwise data" if slide_type == "teammates" else "All-time matchup data"

    _draw_wrapped_header(draw, username, year_label, width)

    title_font = _get_font(20, bold=True)
    subtitle_font = _get_font(14)
    name_font = _get_font(18, bold=True)
    stat_font = _get_font(14)
    section_font = _get_font(12, bold=True)
    flavor_font = _get_font(12)

    _center_text(draw, title, title_font, 50, width, accent)
    _center_text(draw, subtitle, subtitle_font, 78, width, TEXT_GREY)

    draw.line([(50, 100), (width - 50, 100)], fill=(*accent, 128), width=1)

    section_map = dict(section_labels or [])

    y_pos = 115
    row_height = 75

    for i, entry in enumerate(entries[:6]):
        y = y_pos + i * row_height

        has_section = i in section_map
        if has_section:
            draw.text((40, y + 2), section_map[i].upper(), fill=accent, font=section_font)
            y += 16

        avatar_x = 40

        discord_id = entry.get("discord_id")
        drew_avatar = False
        if avatar_images and discord_id and discord_id in avatar_images:
            try:
                avatar_data = avatar_images[discord_id]
                avatar_img = Image.open(io.BytesIO(avatar_data)).convert("RGBA").resize((48, 48), Image.Resampling.LANCZOS)
                mask = Image.new("L", (48, 48), 0)
                mask_draw = ImageDraw.Draw(mask)
                mask_draw.ellipse((0, 0, 47, 47), fill=255)
                img.paste(avatar_img, (avatar_x, y), mask)
                drew_avatar = True
            except Exception:
                pass

        if not drew_avatar:
            initial = entry.get("username", "?")[0].upper()
            draw.ellipse((avatar_x, y, avatar_x + 48, y + 48), fill=(*accent, 100))
            init_font = _get_font(20, bold=True)
            bbox = draw.textbbox((0, 0), initial, font=init_font)
            iw = bbox[2] - bbox[0]
            ih = bbox[3] - bbox[1]
            draw.text((avatar_x + 24 - iw // 2, y + 24 - ih // 2), initial, fill=TEXT_WHITE, font=init_font)

        text_x = avatar_x + 60

        uname = f"@{entry.get('username', '?')}"
        draw.text((text_x, y), uname, fill=TEXT_WHITE, font=name_font)

        games = entry.get("games", 0)
        wins = entry.get("wins", 0)
        losses = games - wins
        wr = entry.get("win_rate", 0)
        stat_text = f"{wins}W {losses}L ({wr*100:.0f}% WR) · {games} games"
        draw.text((text_x, y + 22), stat_text, fill=TEXT_GREY, font=stat_font)

        flavor = entry.get("flavor")
        if flavor:
            draw.text((text_x, y + 40), f'"{flavor}"', fill=TEXT_DARK, font=flavor_font)

    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer


def draw_hero_spotlight_slide(
    username: str,
    year_label: str,
    top_hero: dict,
    top_3_heroes: list[dict],
    unique_count: int,
) -> io.BytesIO:
    """Draw hero spotlight slide with featured hero and top 3 bar chart."""
    width, height = 800, 600
    img = Image.new("RGBA", (width, height), (*BG_GRADIENT_START, 255))
    draw = ImageDraw.Draw(img)

    _draw_rgba_gradient_background(draw, width, height)

    accent = (46, 204, 113)
    _draw_wrapped_header(draw, username, year_label, width)

    title_font = _get_font(20, bold=True)
    hero_font = _get_font(36, bold=True)
    stat_font = _get_font(18)
    bar_label_font = _get_font(14, bold=True)
    bar_value_font = _get_font(12)
    small_font = _get_font(14)

    _center_text(draw, "HERO SPOTLIGHT", title_font, 50, width, accent)
    draw.line([(50, 80), (width - 50, 80)], fill=(*accent, 128), width=1)

    _center_text(draw, top_hero.get("name", "Unknown"), hero_font, 100, width, TEXT_WHITE)

    wr = top_hero.get("win_rate", 0)
    picks = top_hero.get("picks", 0)
    wins = top_hero.get("wins", 0)
    stat_text = f"{picks} games · {wins} wins · {wr*100:.0f}% win rate"
    _center_text(draw, stat_text, stat_font, 150, width, accent)

    _center_text(draw, f"{unique_count} unique heroes played", small_font, 185, width, TEXT_GREY)

    draw.text((50, 230), "TOP HEROES", fill=TEXT_GREY, font=bar_label_font)

    bar_y = 260
    max_picks = max((h.get("picks", 0) for h in top_3_heroes), default=1)
    bar_max_width = 500
    bar_height_px = 35
    bar_spacing = 85

    bar_colors = [accent, (88, 101, 242), (241, 196, 15)]

    for i, hero in enumerate(top_3_heroes[:3]):
        y_bar = bar_y + i * bar_spacing
        picks_h = hero.get("picks", 0)
        wins_h = hero.get("wins", 0)
        bar_w = max(int((picks_h / max_picks) * bar_max_width), 30) if max_picks > 0 else 30
        color = bar_colors[i] if i < len(bar_colors) else accent

        draw.text((50, y_bar), hero.get("name", "?"), fill=TEXT_WHITE, font=bar_label_font)

        _draw_rounded_rect(draw, (50, y_bar + 20, 50 + bar_w, y_bar + 20 + bar_height_px), radius=6, fill=(*color, 180))

        wr_h = hero.get("win_rate", 0)
        kda_h = hero.get("kda")
        if kda_h is not None:
            bar_text = f"{picks_h} games · {wins_h}W · {wr_h*100:.0f}% WR · {kda_h:.1f} KDA"
        else:
            bar_text = f"{picks_h} games · {wins_h}W · {wr_h*100:.0f}% WR"
        draw.text((60, y_bar + 27), bar_text, fill=TEXT_WHITE, font=bar_value_font)

    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer


def draw_lane_breakdown_slide(
    username: str,
    year_label: str,
    lane_freq: dict[int, int],
    total_games: int,
) -> io.BytesIO:
    """Draw lane breakdown slide showing lane distribution."""
    width, height = 800, 600
    img = Image.new("RGBA", (width, height), (*BG_GRADIENT_START, 255))
    draw = ImageDraw.Draw(img)

    _draw_rgba_gradient_background(draw, width, height)

    accent = (52, 152, 219)
    _draw_wrapped_header(draw, username, year_label, width)

    title_font = _get_font(20, bold=True)
    _center_text(draw, "LANE BREAKDOWN", title_font, 50, width, accent)
    draw.line([(50, 80), (width - 50, 80)], fill=(*accent, 128), width=1)

    lane_names = {1: "Safe Lane", 2: "Mid Lane", 3: "Off Lane"}
    lane_colors = {
        1: (87, 242, 135),
        2: (241, 196, 15),
        3: (237, 66, 69),
    }

    label_font = _get_font(16, bold=True)
    value_font = _get_font(14)
    bar_font = _get_font(14, bold=True)

    if lane_freq:
        draw.text((50, 100), "LANE DISTRIBUTION", fill=TEXT_GREY, font=label_font)

        max_count = max(lane_freq.values(), default=1)

        bar_max_width = 450
        bar_y = 130
        bar_height_px = 30
        lane_spacing = 65

        for i, (lane_role, count) in enumerate(sorted(lane_freq.items())):
            y_bar = bar_y + i * lane_spacing
            name = lane_names.get(lane_role, f"Lane {lane_role}")
            color = lane_colors.get(lane_role, accent)

            draw.text((50, y_bar), name, fill=TEXT_WHITE, font=bar_font)

            bar_w = max(int((count / max_count) * bar_max_width), 30) if max_count > 0 else 30
            _draw_rounded_rect(draw, (50, y_bar + 20, 50 + bar_w, y_bar + 20 + bar_height_px), radius=6, fill=(*color, 180))

            pct = (count / total_games * 100) if total_games > 0 else 0
            draw.text((60, y_bar + 24), f"{count} games ({pct:.0f}%)", fill=TEXT_WHITE, font=value_font)
    else:
        _center_text(draw, "No lane data available", label_font, 250, width, TEXT_GREY)
        _center_text(draw, "(Requires match enrichment from OpenDota)", value_font, 280, width, TEXT_DARK)

    _center_text(draw, f"{total_games} total games with lane data", value_font, height - 50, width, TEXT_GREY)

    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer


def draw_package_deal_slide(
    username: str,
    year_label: str,
    times_bought: int,
    times_bought_on_you: int,
    unique_buyers: int,
    jc_spent: int,
    jc_spent_on_you: int,
    total_games: int,
    flavor_text: str | None = None,
) -> io.BytesIO:
    """Draw anonymized package deal stats slide.

    ``flavor_text`` is an optional caption; the caller generates the localized
    string so this drawing function doesn't have to reach into the service layer.
    """
    width, height = 800, 600
    img = Image.new("RGBA", (width, height), (*BG_GRADIENT_START, 255))
    draw = ImageDraw.Draw(img)

    _draw_rgba_gradient_background(draw, width, height)

    accent = ACCENT_PURPLE
    _draw_wrapped_header(draw, username, year_label, width)

    title_font = _get_font(20, bold=True)
    _center_text(draw, "PACKAGE DEALS", title_font, 50, width, accent)
    draw.line([(50, 80), (width - 50, 80)], fill=(*accent, 128), width=1)

    card_y = 110
    _draw_rounded_rect(draw, (40, card_y, width - 40, card_y + 100), radius=10, fill=(40, 40, 50), outline=(*accent, 100))

    big_font = _get_font(36, bold=True)
    label_font = _get_font(14, bold=True)
    detail_font = _get_font(14)

    draw.text((70, card_y + 10), str(times_bought_on_you), fill=accent, font=big_font)

    buyer_label = "person bought a deal on you" if unique_buyers == 1 else "people bought deals on you"
    draw.text((70, card_y + 55), f"{unique_buyers} {buyer_label}", fill=TEXT_WHITE, font=label_font)
    draw.text((70, card_y + 75), f"{jc_spent_on_you} JC spent on you", fill=TEXT_GREY, font=detail_font)

    card_y2 = 230
    _draw_rounded_rect(draw, (40, card_y2, width - 40, card_y2 + 100), radius=10, fill=(40, 40, 50), outline=(*accent, 100))

    draw.text((70, card_y2 + 10), str(times_bought), fill=ACCENT_GOLD, font=big_font)
    draw.text((70, card_y2 + 55), "deals you purchased", fill=TEXT_WHITE, font=label_font)
    draw.text((70, card_y2 + 75), f"{jc_spent} JC spent", fill=TEXT_GREY, font=detail_font)

    games_y = 360
    _draw_rounded_rect(draw, (40, games_y, width - 40, games_y + 70), radius=10, fill=(40, 40, 50), outline=(*ACCENT_GREEN, 80))
    draw.text((70, games_y + 10), str(total_games), fill=ACCENT_GREEN, font=_get_font(28, bold=True))
    draw.text((70, games_y + 45), "games committed across all deals", fill=TEXT_GREY, font=detail_font)

    if flavor_text:
        flavor_font = _get_font(14)
        _center_text(draw, f'"{flavor_text}"', flavor_font, height - 60, width, TEXT_GREY)

    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer


def wrap_chart_in_slide(
    chart_bytes: bytes,
    title: str,
    flavor_text: str,
    accent_color: tuple[int, int, int] = ACCENT_GOLD,
) -> io.BytesIO:
    """Wrap a 700x400 chart in an 800x600 wrapped-styled canvas."""
    width, height = 800, 600
    img = Image.new("RGBA", (width, height), (*BG_GRADIENT_START, 255))
    draw = ImageDraw.Draw(img)

    _draw_rgba_gradient_background(draw, width, height)

    title_font = _get_font(16, bold=True)
    flavor_font = _get_font(14)

    _center_text(draw, title.upper(), title_font, 15, width, accent_color)

    try:
        chart_img = Image.open(io.BytesIO(chart_bytes)).convert("RGBA")
        chart_w, chart_h = chart_img.size
        max_w, max_h = 740, 480
        if chart_w > max_w or chart_h > max_h:
            ratio = min(max_w / chart_w, max_h / chart_h)
            new_w = int(chart_w * ratio)
            new_h = int(chart_h * ratio)
            chart_img = chart_img.resize((new_w, new_h), Image.Resampling.LANCZOS)
            chart_w, chart_h = new_w, new_h

        x_offset = (width - chart_w) // 2
        y_offset = 45
        img.paste(chart_img, (x_offset, y_offset), chart_img)
    except Exception:
        _center_text(draw, "Chart unavailable", title_font, 250, width, TEXT_GREY)

    if flavor_text:
        _center_text(draw, flavor_text, flavor_font, height - 40, width, TEXT_GREY)

    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer
