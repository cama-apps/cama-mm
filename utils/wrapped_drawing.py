"""
Spotify Wrapped style image generation for Cama yearly summaries.
"""

import io
from typing import TYPE_CHECKING

from PIL import Image, ImageDraw, ImageFont
from pilmoji import Pilmoji

if TYPE_CHECKING:
    from services.wrapped_service import Award, PersonalRecord, PlayerWrapped, ServerWrapped

# Color palette (Spotify-inspired but with Cama colors)
BG_GRADIENT_START = (30, 30, 35)  # Dark charcoal
BG_GRADIENT_END = (45, 45, 55)  # Slightly lighter
ACCENT_GOLD = (241, 196, 15)  # Jopacoin gold
ACCENT_GREEN = (87, 242, 135)  # Discord green
ACCENT_RED = (237, 66, 69)  # Discord red
ACCENT_BLUE = (88, 101, 242)  # Discord blurple
TEXT_WHITE = (255, 255, 255)
TEXT_GREY = (185, 187, 190)
TEXT_DARK = (100, 100, 100)

# Award category colors
CATEGORY_COLORS = {
    "performance": (88, 101, 242),  # Blue
    "rating": (155, 89, 182),  # Purple
    "economy": (241, 196, 15),  # Gold
    "hero": (46, 204, 113),  # Green
    "fun": (231, 76, 60),  # Red
}


def _get_font(size: int = 16, bold: bool = False) -> ImageFont.FreeTypeFont:
    """Get a font, falling back to default if unavailable."""
    try:
        font_name = "DejaVuSans-Bold.ttf" if bold else "DejaVuSans.ttf"
        return ImageFont.truetype(f"/usr/share/fonts/truetype/dejavu/{font_name}", size)
    except OSError:
        return ImageFont.load_default()


def _draw_gradient_background(
    draw: ImageDraw.Draw, width: int, height: int, start_color: tuple, end_color: tuple
) -> None:
    """Draw a vertical gradient background."""
    for y in range(height):
        ratio = y / height
        r = int(start_color[0] + (end_color[0] - start_color[0]) * ratio)
        g = int(start_color[1] + (end_color[1] - start_color[1]) * ratio)
        b = int(start_color[2] + (end_color[2] - start_color[2]) * ratio)
        draw.line([(0, y), (width, y)], fill=(r, g, b))


def _draw_rounded_rect(
    draw: ImageDraw.Draw,
    xy: tuple,
    radius: int,
    fill: tuple | None = None,
    outline: tuple | None = None,
    width: int = 1,
) -> None:
    """Draw a rounded rectangle."""
    x1, y1, x2, y2 = xy
    draw.rounded_rectangle(xy, radius=radius, fill=fill, outline=outline, width=width)


def draw_wrapped_summary(wrapped: "ServerWrapped", hero_names: dict[int, str] | None = None) -> io.BytesIO:
    """
    Generate the main wrapped summary card.

    Args:
        wrapped: ServerWrapped object with stats
        hero_names: Optional dict mapping hero_id to hero name

    Returns:
        BytesIO containing PNG image
    """
    width, height = 800, 600
    img = Image.new("RGB", (width, height), BG_GRADIENT_START)
    draw = ImageDraw.Draw(img)

    # Draw gradient background
    _draw_gradient_background(draw, width, height, BG_GRADIENT_START, BG_GRADIENT_END)

    # Fonts
    title_font = _get_font(42, bold=True)
    subtitle_font = _get_font(24, bold=True)
    large_font = _get_font(36, bold=True)
    medium_font = _get_font(20)
    small_font = _get_font(16)

    # Header
    header_text = "CAMA WRAPPED"
    bbox = draw.textbbox((0, 0), header_text, font=title_font)
    text_w = bbox[2] - bbox[0]
    draw.text(((width - text_w) // 2, 30), header_text, fill=ACCENT_GOLD, font=title_font)

    # Month/Year
    month_text = wrapped.year_label.upper()
    bbox = draw.textbbox((0, 0), month_text, font=subtitle_font)
    text_w = bbox[2] - bbox[0]
    draw.text(((width - text_w) // 2, 85), month_text, fill=TEXT_WHITE, font=subtitle_font)

    # Divider line
    draw.line([(50, 130), (width - 50, 130)], fill=ACCENT_GOLD, width=2)

    # Main stats section
    stats_y = 160
    stats = [
        (f"{wrapped.total_matches}", "MATCHES"),
        (f"{wrapped.unique_heroes}", "UNIQUE HEROES"),
        (f"{wrapped.total_wagered:,}", "JC WAGERED"),
    ]

    stat_width = (width - 100) // len(stats)
    for i, (value, label) in enumerate(stats):
        x = 50 + i * stat_width + stat_width // 2

        # Value
        bbox = draw.textbbox((0, 0), value, font=large_font)
        text_w = bbox[2] - bbox[0]
        draw.text((x - text_w // 2, stats_y), value, fill=ACCENT_GOLD, font=large_font)

        # Label
        bbox = draw.textbbox((0, 0), label, font=small_font)
        text_w = bbox[2] - bbox[0]
        draw.text((x - text_w // 2, stats_y + 45), label, fill=TEXT_GREY, font=small_font)

    # Top performer section
    top_y = 270
    if wrapped.top_players:
        top = wrapped.top_players[0]
        draw.text((50, top_y), "TOP PERFORMER", fill=TEXT_GREY, font=small_font)

        player_name = f"@{top['discord_username']}"
        draw.text((50, top_y + 25), player_name, fill=TEXT_WHITE, font=subtitle_font)

        # Find rating change for top player
        rating_text = f"{top['wins']}W {top['games_played'] - top['wins']}L ({top['win_rate']*100:.0f}% WR)"
        draw.text((50, top_y + 55), rating_text, fill=ACCENT_GREEN, font=medium_font)

    # Most played hero section
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

    # Best hero section (right side)
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

    # Footer
    footer_text = f"{wrapped.unique_players} players participated"
    bbox = draw.textbbox((0, 0), footer_text, font=small_font)
    text_w = bbox[2] - bbox[0]
    draw.text(((width - text_w) // 2, height - 40), footer_text, fill=TEXT_GREY, font=small_font)

    # Save to buffer
    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer


def draw_wrapped_award(award: "Award", hero_names: dict[int, str] | None = None) -> io.BytesIO:
    """
    Generate an individual award card.

    Args:
        award: Award object
        hero_names: Optional dict mapping hero_id to hero name

    Returns:
        BytesIO containing PNG image
    """
    width, height = 400, 300
    img = Image.new("RGB", (width, height), BG_GRADIENT_START)
    draw = ImageDraw.Draw(img)

    # Get category color
    accent_color = CATEGORY_COLORS.get(award.category, ACCENT_GOLD)

    # Draw gradient background
    _draw_gradient_background(draw, width, height, BG_GRADIENT_START, BG_GRADIENT_END)

    # Fonts
    emoji_font = _get_font(48)
    title_font = _get_font(28, bold=True)
    name_font = _get_font(22, bold=True)
    stat_font = _get_font(18)
    flavor_font = _get_font(14)

    # Emoji at top using pilmoji
    if award.emoji:
        with Pilmoji(img) as pilmoji:
            # Get text size for centering
            pilmoji.text(((width - 48) // 2, 25), award.emoji, font=emoji_font)

    # Award title
    bbox = draw.textbbox((0, 0), award.title.upper(), font=title_font)
    text_w = bbox[2] - bbox[0]
    draw.text(((width - text_w) // 2, 95), award.title.upper(), fill=accent_color, font=title_font)

    # Player name
    player_text = f"@{award.discord_username}"
    bbox = draw.textbbox((0, 0), player_text, font=name_font)
    text_w = bbox[2] - bbox[0]
    draw.text(((width - text_w) // 2, 140), player_text, fill=TEXT_WHITE, font=name_font)

    # Stat value
    stat_text = award.stat_value
    bbox = draw.textbbox((0, 0), stat_text, font=stat_font)
    text_w = bbox[2] - bbox[0]
    draw.text(((width - text_w) // 2, 180), stat_text, fill=ACCENT_GOLD, font=stat_font)

    # Flavor text
    if award.flavor_text:
        bbox = draw.textbbox((0, 0), f'"{award.flavor_text}"', font=flavor_font)
        text_w = bbox[2] - bbox[0]
        draw.text(
            ((width - text_w) // 2, 220),
            f'"{award.flavor_text}"',
            fill=TEXT_GREY,
            font=flavor_font,
        )

    # Save to buffer
    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer


def draw_wrapped_personal(
    player_wrapped: "PlayerWrapped", hero_names: dict[int, str] | None = None
) -> io.BytesIO:
    """
    Generate a personal wrapped card for a player.

    Args:
        player_wrapped: PlayerWrapped object
        hero_names: Optional dict mapping hero_id to hero name

    Returns:
        BytesIO containing PNG image
    """
    width, height = 800, 450
    img = Image.new("RGB", (width, height), BG_GRADIENT_START)
    draw = ImageDraw.Draw(img)

    # Draw gradient background
    _draw_gradient_background(draw, width, height, BG_GRADIENT_START, BG_GRADIENT_END)

    # Fonts
    title_font = _get_font(32, bold=True)
    large_font = _get_font(28, bold=True)
    medium_font = _get_font(18)
    small_font = _get_font(14)

    # Header
    draw.text((30, 20), "YOUR WRAPPED", fill=TEXT_GREY, font=medium_font)
    draw.text((30, 45), f"@{player_wrapped.discord_username}", fill=TEXT_WHITE, font=title_font)

    # Divider
    draw.line([(30, 95), (width - 30, 95)], fill=ACCENT_GOLD, width=2)

    # Main stats row
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

        # Value
        color = ACCENT_GREEN if (label == "RATING" and player_wrapped.rating_change >= 0) else (
            ACCENT_RED if label == "RATING" else ACCENT_GOLD
        )
        bbox = draw.textbbox((0, 0), value, font=large_font)
        text_w = bbox[2] - bbox[0]
        draw.text((x - text_w // 2, stats_y), value, fill=color, font=large_font)

        # Label
        bbox = draw.textbbox((0, 0), label, font=small_font)
        text_w = bbox[2] - bbox[0]
        draw.text((x - text_w // 2, stats_y + 35), label, fill=TEXT_GREY, font=small_font)

    # Top heroes section
    hero_y = 200
    draw.text((30, hero_y), "TOP HEROES", fill=TEXT_GREY, font=small_font)

    if player_wrapped.top_heroes:
        for i, hero in enumerate(player_wrapped.top_heroes[:3]):
            y = hero_y + 25 + i * 28
            hero_name = hero_names.get(hero["hero_id"], f"Hero #{hero['hero_id']}") if hero_names else f"Hero #{hero['hero_id']}"

            # Rank number
            draw.text((30, y), f"{i + 1}.", fill=ACCENT_GOLD, font=medium_font)

            # Hero name
            draw.text((55, y), hero_name, fill=TEXT_WHITE, font=medium_font)

            # Stats
            stats_text = f"{hero['picks']}g {hero['win_rate']*100:.0f}%"
            draw.text((250, y), stats_text, fill=TEXT_GREY, font=medium_font)

    # Betting stats section (right side)
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

    # Footer
    footer_text = f"W: {player_wrapped.wins} | L: {player_wrapped.losses}"
    bbox = draw.textbbox((0, 0), footer_text, font=small_font)
    text_w = bbox[2] - bbox[0]
    draw.text(((width - text_w) // 2, height - 35), footer_text, fill=TEXT_GREY, font=small_font)

    # Save to buffer
    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer


def draw_awards_grid(awards: list["Award"], max_awards: int = 6, viewer_discord_id: int | None = None) -> io.BytesIO:
    """
    Generate a grid of award cards.

    Args:
        awards: List of Award objects
        max_awards: Maximum awards to show
        viewer_discord_id: If provided, highlight awards won by this user

    Returns:
        BytesIO containing PNG image
    """
    awards = awards[:max_awards]
    if not awards:
        # Return empty placeholder
        img = Image.new("RGB", (800, 200), BG_GRADIENT_START)
        draw = ImageDraw.Draw(img)
        font = _get_font(20)
        draw.text((300, 90), "No awards yet!", fill=TEXT_GREY, font=font)
        buffer = io.BytesIO()
        img.save(buffer, format="PNG")
        buffer.seek(0)
        return buffer

    # Calculate grid dimensions
    cols = min(3, len(awards))
    rows = (len(awards) + cols - 1) // cols

    card_width, card_height = 250, 220
    padding = 20
    total_width = cols * card_width + (cols + 1) * padding
    total_height = rows * card_height + (rows + 1) * padding + 60  # Extra for header

    img = Image.new("RGB", (total_width, total_height), BG_GRADIENT_START)
    draw = ImageDraw.Draw(img)

    # Draw gradient background
    _draw_gradient_background(draw, total_width, total_height, BG_GRADIENT_START, BG_GRADIENT_END)

    # Header
    header_font = _get_font(24, bold=True)
    draw.text((padding, 15), "AWARDS", fill=ACCENT_GOLD, font=header_font)

    # Fonts for cards
    emoji_font = _get_font(24)
    title_font = _get_font(14, bold=True)
    name_font = _get_font(12, bold=True)
    stat_font = _get_font(11)
    flavor_font = _get_font(10)

    # Max text width inside a card (card_width minus left/right padding)
    text_max_w = card_width - 20

    # Draw each award card
    for i, award in enumerate(awards):
        row = i // cols
        col = i % cols
        x = padding + col * (card_width + padding)
        y = 60 + padding + row * (card_height + padding)

        # Card background — highlight if viewer won this award
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

        # Star badge for viewer's award
        if is_viewer:
            star_font = _get_font(12, bold=True)
            draw.text((x + card_width - 40, y + 8), "YOU", fill=ACCENT_GOLD, font=star_font)

        # Emoji using pilmoji
        if award.emoji:
            with Pilmoji(img) as pilmoji:
                pilmoji.text((x + 10, y + 10), award.emoji, font=emoji_font)

        # Title (next to emoji) — truncate only if truly too wide
        title_text = award.title.upper()
        title_w = draw.textlength(title_text, font=title_font)
        title_max = card_width - 55  # space after emoji
        if title_w > title_max:
            while draw.textlength(title_text + "..", font=title_font) > title_max and len(title_text) > 1:
                title_text = title_text[:-1]
            title_text = title_text.rstrip() + ".."
        draw.text((x + 45, y + 14), title_text, fill=accent_color, font=title_font)

        # Player name — truncate only if too wide
        player_text = f"@{award.discord_username}"
        player_w = draw.textlength(player_text, font=name_font)
        if player_w > text_max_w:
            while draw.textlength(player_text + "..", font=name_font) > text_max_w and len(player_text) > 1:
                player_text = player_text[:-1]
            player_text = player_text.rstrip() + ".."
        draw.text((x + 10, y + 55), player_text, fill=TEXT_WHITE, font=name_font)

        # Stat — truncate only if too wide
        stat_text = award.stat_value
        stat_w = draw.textlength(stat_text, font=stat_font)
        if stat_w > text_max_w:
            while draw.textlength(stat_text + "..", font=stat_font) > text_max_w and len(stat_text) > 1:
                stat_text = stat_text[:-1]
            stat_text = stat_text.rstrip() + ".."
        draw.text((x + 10, y + 80), stat_text, fill=ACCENT_GOLD, font=stat_font)

        # Flavor text — word-wrap up to 3 lines
        if award.flavor_text:
            flavor = f'"{award.flavor_text}"'
            lines = _word_wrap(flavor, flavor_font, text_max_w, draw)
            for li, line in enumerate(lines[:3]):
                draw.text((x + 10, y + 110 + li * 16), line, fill=TEXT_GREY, font=flavor_font)

    # Save to buffer
    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer


# Slide accent colors for personal records and story slides
SLIDE_COLORS = {
    "combat": (237, 66, 69),
    "farming": (241, 196, 15),
    "impact": (88, 101, 242),
    "vision": (87, 242, 135),
    "endurance": (155, 89, 182),
    # Story slides
    "story_games": (88, 101, 242),  # Blurple
    "story_summary": (241, 196, 15),  # Gold
    "story_hero": (46, 204, 113),  # Green
    "story_role": (52, 152, 219),  # Blue
    "story_teammates": (87, 242, 135),  # Discord green
    "story_rivals": (237, 66, 69),  # Discord red
    "story_packages": (155, 89, 182),  # Purple
    "story_rating": (241, 196, 15),  # Gold
    "story_gamba": (231, 76, 60),  # Red
    "server_summary": (241, 196, 15),  # Gold
    "awards": (88, 101, 242),  # Blurple
}

# Dimmed color for worst/N/A records
WORST_LABEL_COLOR = (255, 120, 120)
NA_COLOR = (80, 80, 90)


def draw_records_slide(
    slide_title: str,
    accent_color: tuple[int, int, int],
    records: list["PersonalRecord"],
    username: str,
    year_label: str,
    slide_number: int,
    total_slides: int,
    hero_names: dict[int, str],
) -> io.BytesIO:
    """
    Generate a single records slide image.

    Args:
        slide_title: e.g. "Combat"
        accent_color: RGB tuple for the slide theme
        records: PersonalRecord objects for this slide
        username: Player's display name
        year_label: e.g. "Cama Wrapped 2026"
        slide_number: 1-based slide index
        total_slides: Total number of slides
        hero_names: Dict mapping hero_id to hero name

    Returns:
        BytesIO containing 800x600 RGBA PNG
    """
    from utils.drawing import _fetch_hero_image

    width, height = 800, 600
    img = Image.new("RGBA", (width, height), (*BG_GRADIENT_START, 255))
    draw = ImageDraw.Draw(img)

    # Draw gradient background
    for y in range(height):
        ratio = y / height
        r = int(BG_GRADIENT_START[0] + (BG_GRADIENT_END[0] - BG_GRADIENT_START[0]) * ratio)
        g = int(BG_GRADIENT_START[1] + (BG_GRADIENT_END[1] - BG_GRADIENT_START[1]) * ratio)
        b = int(BG_GRADIENT_START[2] + (BG_GRADIENT_END[2] - BG_GRADIENT_START[2]) * ratio)
        draw.line([(0, y), (width, y)], fill=(r, g, b, 255))

    # Fonts
    header_font = _get_font(24, bold=True)
    subheader_font = _get_font(16)
    slide_title_font = _get_font(20, bold=True)
    label_font = _get_font(16)
    value_font = _get_font(20, bold=True)
    info_font = _get_font(12)

    # Header
    draw.text((30, 20), f"{username}'s Records", fill=TEXT_WHITE, font=header_font)
    draw.text((30, 50), f"— {year_label}", fill=TEXT_GREY, font=subheader_font)

    # Slide title
    draw.text((30, 80), slide_title.upper(), fill=accent_color, font=slide_title_font)

    # Thin divider
    draw.line([(30, 108), (width - 30, 108)], fill=(*accent_color, 128), width=1)

    # Records area (6 * 73 = 438px, fits in 600 - 120 - 35 = 445px)
    y_start = 120
    row_height = 73
    max_records = 6

    for i, record in enumerate(records[:max_records]):
        y = y_start + i * row_height
        is_na = record.value is None or record.display_value == "N/A"

        # Hero portrait (48x27, natural Dota aspect ratio)
        hero_x = 30
        if record.hero_id and not is_na:
            try:
                hero_img = _fetch_hero_image(record.hero_id, (48, 27))
                if hero_img:
                    if hero_img.mode != "RGBA":
                        hero_img = hero_img.convert("RGBA")
                    # Center vertically in the row
                    hero_y_offset = (row_height - 27) // 2
                    img.paste(hero_img, (hero_x, y + hero_y_offset), hero_img)
            except Exception:
                pass

        # Stat label
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

        # Value
        display = record.display_value if not is_na else "N/A"
        draw.text((label_x, y + 26), display, fill=value_color, font=value_font)

        # Match info
        if record.valve_match_id and not is_na:
            match_info = f"Match #{record.valve_match_id}"
            if record.match_date:
                match_info += f" · {record.match_date}"
            draw.text((label_x, y + 52), match_info, fill=TEXT_DARK, font=info_font)
        elif record.match_date and not is_na:
            draw.text((label_x, y + 52), record.match_date, fill=TEXT_DARK, font=info_font)

        # Hero name on right side
        if record.hero_id and not is_na:
            hero_name = hero_names.get(record.hero_id, "")
            if hero_name:
                bbox = draw.textbbox((0, 0), hero_name, font=info_font)
                name_w = bbox[2] - bbox[0]
                draw.text((width - 30 - name_w, y + 8), hero_name, fill=TEXT_GREY, font=info_font)

    # Save to buffer (no slide counter for clean story flow)
    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer


# ============ NEW WRAPPED STORY SLIDE DRAWING FUNCTIONS ============

ACCENT_PURPLE = (155, 89, 182)


def _word_wrap(text: str, font, max_width: int, draw: ImageDraw.Draw) -> list[str]:
    """Break text into lines that fit within max_width pixels."""
    words = text.split()
    lines: list[str] = []
    current = ""
    for word in words:
        test = f"{current} {word}".strip()
        if draw.textlength(test, font=font) <= max_width:
            current = test
        else:
            if current:
                lines.append(current)
            current = word
    if current:
        lines.append(current)
    # Truncate any single line that still exceeds max_width
    result = []
    for line in lines:
        if draw.textlength(line, font=font) > max_width:
            while draw.textlength(line + "..", font=font) > max_width and len(line) > 1:
                line = line[:-1]
            line = line.rstrip() + ".."
        result.append(line)
    return result


def _center_text(draw: ImageDraw.Draw, text: str, font, y: int, width: int, fill: tuple) -> None:
    """Draw centered text at given y position."""
    bbox = draw.textbbox((0, 0), text, font=font)
    text_w = bbox[2] - bbox[0]
    draw.text(((width - text_w) // 2, y), text, fill=fill, font=font)


def _draw_wrapped_header(draw: ImageDraw.Draw, username: str, year_label: str, width: int) -> None:
    """Draw the standard wrapped story header."""
    header_font = _get_font(14)
    draw.text((30, 15), f"@{username}", fill=TEXT_GREY, font=header_font)
    bbox = draw.textbbox((0, 0), year_label.upper(), font=header_font)
    text_w = bbox[2] - bbox[0]
    draw.text((width - 30 - text_w, 15), year_label.upper(), fill=TEXT_GREY, font=header_font)


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
    """
    Draw a big-number story reveal slide.

    Used for: Your Year, Rating Story, Gamba Story slides.
    """
    width, height = 800, 600
    img = Image.new("RGBA", (width, height), (*BG_GRADIENT_START, 255))
    draw = ImageDraw.Draw(img)

    # Gradient background
    for y in range(height):
        ratio = y / height
        r = int(BG_GRADIENT_START[0] + (BG_GRADIENT_END[0] - BG_GRADIENT_START[0]) * ratio)
        g = int(BG_GRADIENT_START[1] + (BG_GRADIENT_END[1] - BG_GRADIENT_START[1]) * ratio)
        b = int(BG_GRADIENT_START[2] + (BG_GRADIENT_END[2] - BG_GRADIENT_START[2]) * ratio)
        draw.line([(0, y), (width, y)], fill=(r, g, b, 255))

    # Fonts
    headline_font = _get_font(18, bold=True)
    big_font = _get_font(72, bold=True)
    label_font = _get_font(22)
    flavor_font = _get_font(16)
    comparison_font = _get_font(16)

    # Header
    _draw_wrapped_header(draw, username, year_label, width)

    # Headline
    _center_text(draw, headline.upper(), headline_font, 80, width, TEXT_GREY)

    # Big stat number
    _center_text(draw, stat_value, big_font, 140, width, accent_color)

    # Stat label
    _center_text(draw, stat_label, label_font, 230, width, TEXT_WHITE)

    # Comparisons (percentile lines)
    if comparisons:
        y_pos = 290
        for comp in comparisons:
            _center_text(draw, comp, comparison_font, y_pos, width, TEXT_GREY)
            y_pos += 30

    # Flavor text
    if flavor_text:
        _center_text(draw, f'"{flavor_text}"', flavor_font, 480, width, TEXT_GREY)

    # Accent line at bottom
    draw.line([(100, 540), (width - 100, 540)], fill=(*accent_color, 128), width=2)

    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer


def draw_summary_stats_slide(
    username: str,
    year_label: str,
    stats_list: list[tuple[str, str, str, tuple[int, int, int]]],
) -> io.BytesIO:
    """
    Draw a 2x3 summary stats grid.

    Args:
        stats_list: List of (value, label, quip, color) tuples, up to 6
    """
    width, height = 800, 600
    img = Image.new("RGBA", (width, height), (*BG_GRADIENT_START, 255))
    draw = ImageDraw.Draw(img)

    for y in range(height):
        ratio = y / height
        r = int(BG_GRADIENT_START[0] + (BG_GRADIENT_END[0] - BG_GRADIENT_START[0]) * ratio)
        g = int(BG_GRADIENT_START[1] + (BG_GRADIENT_END[1] - BG_GRADIENT_START[1]) * ratio)
        b = int(BG_GRADIENT_START[2] + (BG_GRADIENT_END[2] - BG_GRADIENT_START[2]) * ratio)
        draw.line([(0, y), (width, y)], fill=(r, g, b, 255))

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

        # Card background
        card_x1 = 40 + col * cell_w + 10
        card_y1 = cy
        card_x2 = card_x1 + cell_w - 20
        card_y2 = cy + cell_h - 20
        _draw_rounded_rect(draw, (card_x1, card_y1, card_x2, card_y2), radius=8, fill=(40, 40, 50))

        # Value
        bbox = draw.textbbox((0, 0), value, font=value_font)
        tw = bbox[2] - bbox[0]
        draw.text((cx - tw // 2, cy + 15), value, fill=color, font=value_font)

        # Label
        bbox = draw.textbbox((0, 0), label, font=label_font)
        tw = bbox[2] - bbox[0]
        draw.text((cx - tw // 2, cy + 60), label, fill=TEXT_WHITE, font=label_font)

        # Quip
        if quip:
            bbox = draw.textbbox((0, 0), quip, font=quip_font)
            tw = bbox[2] - bbox[0]
            draw.text((cx - tw // 2, cy + 82), quip, fill=TEXT_GREY, font=quip_font)

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
    """
    Draw a pairwise teammates or rivals slide.

    Args:
        entries: List of dicts with {discord_id, username, games, wins, win_rate, flavor}
        slide_type: "teammates" (green accent) or "rivals" (red accent)
        avatar_images: Dict mapping discord_id -> avatar bytes (48x48)
        section_labels: List of (entry_index, label) to draw section headers
    """
    width, height = 800, 600
    img = Image.new("RGBA", (width, height), (*BG_GRADIENT_START, 255))
    draw = ImageDraw.Draw(img)

    for y in range(height):
        ratio = y / height
        r = int(BG_GRADIENT_START[0] + (BG_GRADIENT_END[0] - BG_GRADIENT_START[0]) * ratio)
        g = int(BG_GRADIENT_START[1] + (BG_GRADIENT_END[1] - BG_GRADIENT_START[1]) * ratio)
        b = int(BG_GRADIENT_START[2] + (BG_GRADIENT_END[2] - BG_GRADIENT_START[2]) * ratio)
        draw.line([(0, y), (width, y)], fill=(r, g, b, 255))

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

    # Build set of entry indices that start a section
    section_map = dict(section_labels or [])

    y_pos = 115
    row_height = 75

    for i, entry in enumerate(entries[:6]):
        y = y_pos + i * row_height

        # Draw section header if this entry starts a new section
        has_section = i in section_map
        if has_section:
            draw.text((40, y + 2), section_map[i].upper(), fill=accent, font=section_font)
            y += 16  # shift content down under header

        avatar_x = 40

        # Draw avatar or fallback circle
        discord_id = entry.get("discord_id")
        drew_avatar = False
        if avatar_images and discord_id and discord_id in avatar_images:
            try:
                avatar_data = avatar_images[discord_id]
                avatar_img = Image.open(io.BytesIO(avatar_data)).convert("RGBA").resize((48, 48), Image.Resampling.LANCZOS)
                # Create circular mask
                mask = Image.new("L", (48, 48), 0)
                mask_draw = ImageDraw.Draw(mask)
                mask_draw.ellipse((0, 0, 47, 47), fill=255)
                img.paste(avatar_img, (avatar_x, y), mask)
                drew_avatar = True
            except Exception:
                pass

        if not drew_avatar:
            # Fallback: colored circle with initial
            initial = entry.get("username", "?")[0].upper()
            draw.ellipse((avatar_x, y, avatar_x + 48, y + 48), fill=(*accent, 100))
            init_font = _get_font(20, bold=True)
            bbox = draw.textbbox((0, 0), initial, font=init_font)
            iw = bbox[2] - bbox[0]
            ih = bbox[3] - bbox[1]
            draw.text((avatar_x + 24 - iw // 2, y + 24 - ih // 2), initial, fill=TEXT_WHITE, font=init_font)

        text_x = avatar_x + 60

        # Username
        uname = f"@{entry.get('username', '?')}"
        draw.text((text_x, y), uname, fill=TEXT_WHITE, font=name_font)

        # Stats
        games = entry.get("games", 0)
        wins = entry.get("wins", 0)
        losses = games - wins
        wr = entry.get("win_rate", 0)
        stat_text = f"{wins}W {losses}L ({wr*100:.0f}% WR) · {games} games"
        draw.text((text_x, y + 22), stat_text, fill=TEXT_GREY, font=stat_font)

        # Flavor
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
    """
    Draw hero spotlight slide with featured hero and top 3 bar chart.

    Args:
        top_hero: {name, picks, wins, win_rate}
        top_3_heroes: [{name, picks, wins, win_rate}, ...]
        unique_count: Total unique heroes played
    """
    width, height = 800, 600
    img = Image.new("RGBA", (width, height), (*BG_GRADIENT_START, 255))
    draw = ImageDraw.Draw(img)

    for y in range(height):
        ratio = y / height
        r = int(BG_GRADIENT_START[0] + (BG_GRADIENT_END[0] - BG_GRADIENT_START[0]) * ratio)
        g = int(BG_GRADIENT_START[1] + (BG_GRADIENT_END[1] - BG_GRADIENT_START[1]) * ratio)
        b = int(BG_GRADIENT_START[2] + (BG_GRADIENT_END[2] - BG_GRADIENT_START[2]) * ratio)
        draw.line([(0, y), (width, y)], fill=(r, g, b, 255))

    accent = (46, 204, 113)  # Green
    _draw_wrapped_header(draw, username, year_label, width)

    title_font = _get_font(20, bold=True)
    hero_font = _get_font(36, bold=True)
    stat_font = _get_font(18)
    bar_label_font = _get_font(14, bold=True)
    bar_value_font = _get_font(12)
    small_font = _get_font(14)

    _center_text(draw, "HERO SPOTLIGHT", title_font, 50, width, accent)
    draw.line([(50, 80), (width - 50, 80)], fill=(*accent, 128), width=1)

    # Featured hero
    _center_text(draw, top_hero.get("name", "Unknown"), hero_font, 100, width, TEXT_WHITE)

    wr = top_hero.get("win_rate", 0)
    picks = top_hero.get("picks", 0)
    wins = top_hero.get("wins", 0)
    stat_text = f"{picks} games · {wins} wins · {wr*100:.0f}% win rate"
    _center_text(draw, stat_text, stat_font, 150, width, accent)

    # Unique heroes count
    _center_text(draw, f"{unique_count} unique heroes played", small_font, 185, width, TEXT_GREY)

    # Top 3 bar chart
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

        # Hero name
        draw.text((50, y_bar), hero.get("name", "?"), fill=TEXT_WHITE, font=bar_label_font)

        # Bar
        _draw_rounded_rect(draw, (50, y_bar + 20, 50 + bar_w, y_bar + 20 + bar_height_px), radius=6, fill=(*color, 180))

        # Stats inside bar
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
    """
    Draw lane breakdown slide showing lane distribution.

    Args:
        lane_freq: Dict of lane_role -> count (detected lane from OpenDota)
        total_games: Total games with lane data
    """
    width, height = 800, 600
    img = Image.new("RGBA", (width, height), (*BG_GRADIENT_START, 255))
    draw = ImageDraw.Draw(img)

    for y in range(height):
        ratio = y / height
        r = int(BG_GRADIENT_START[0] + (BG_GRADIENT_END[0] - BG_GRADIENT_START[0]) * ratio)
        g = int(BG_GRADIENT_START[1] + (BG_GRADIENT_END[1] - BG_GRADIENT_START[1]) * ratio)
        b = int(BG_GRADIENT_START[2] + (BG_GRADIENT_END[2] - BG_GRADIENT_START[2]) * ratio)
        draw.line([(0, y), (width, y)], fill=(r, g, b, 255))

    accent = (52, 152, 219)  # Blue
    _draw_wrapped_header(draw, username, year_label, width)

    title_font = _get_font(20, bold=True)
    _center_text(draw, "LANE BREAKDOWN", title_font, 50, width, accent)
    draw.line([(50, 80), (width - 50, 80)], fill=(*accent, 128), width=1)

    # Lane role names (OpenDota lane_role: 1=safe, 2=mid, 3=off)
    lane_names = {1: "Safe Lane", 2: "Mid Lane", 3: "Off Lane"}
    lane_colors = {
        1: (87, 242, 135),   # Green
        2: (241, 196, 15),   # Gold
        3: (237, 66, 69),    # Red
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

            # Lane name
            draw.text((50, y_bar), name, fill=TEXT_WHITE, font=bar_font)

            # Bar
            bar_w = max(int((count / max_count) * bar_max_width), 30) if max_count > 0 else 30
            _draw_rounded_rect(draw, (50, y_bar + 20, 50 + bar_w, y_bar + 20 + bar_height_px), radius=6, fill=(*color, 180))

            # Count and percentage
            pct = (count / total_games * 100) if total_games > 0 else 0
            draw.text((60, y_bar + 24), f"{count} games ({pct:.0f}%)", fill=TEXT_WHITE, font=value_font)
    else:
        _center_text(draw, "No lane data available", label_font, 250, width, TEXT_GREY)
        _center_text(draw, "(Requires match enrichment from OpenDota)", value_font, 280, width, TEXT_DARK)

    # Total games footer
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
    """
    Draw anonymized package deal stats slide.

    Shows how many deals the player bought and how many were bought on them,
    without revealing any names. ``flavor_text`` is an optional caption drawn
    near the bottom; callers pass the localized string rather than having this
    util reach up to the services layer to generate one.
    """
    width, height = 800, 600
    img = Image.new("RGBA", (width, height), (*BG_GRADIENT_START, 255))
    draw = ImageDraw.Draw(img)

    for y in range(height):
        ratio = y / height
        r = int(BG_GRADIENT_START[0] + (BG_GRADIENT_END[0] - BG_GRADIENT_START[0]) * ratio)
        g = int(BG_GRADIENT_START[1] + (BG_GRADIENT_END[1] - BG_GRADIENT_START[1]) * ratio)
        b = int(BG_GRADIENT_START[2] + (BG_GRADIENT_END[2] - BG_GRADIENT_START[2]) * ratio)
        draw.line([(0, y), (width, y)], fill=(r, g, b, 255))

    accent = ACCENT_PURPLE
    _draw_wrapped_header(draw, username, year_label, width)

    title_font = _get_font(20, bold=True)
    _center_text(draw, "PACKAGE DEALS", title_font, 50, width, accent)
    draw.line([(50, 80), (width - 50, 80)], fill=(*accent, 128), width=1)

    # --- "Bought on you" card ---
    card_y = 110
    _draw_rounded_rect(draw, (40, card_y, width - 40, card_y + 100), radius=10, fill=(40, 40, 50), outline=(*accent, 100))

    big_font = _get_font(36, bold=True)
    label_font = _get_font(14, bold=True)
    detail_font = _get_font(14)

    # Big number
    draw.text((70, card_y + 10), str(times_bought_on_you), fill=accent, font=big_font)

    # Label
    buyer_label = "person bought a deal on you" if unique_buyers == 1 else "people bought deals on you"
    draw.text((70, card_y + 55), f"{unique_buyers} {buyer_label}", fill=TEXT_WHITE, font=label_font)
    draw.text((70, card_y + 75), f"{jc_spent_on_you} JC spent on you", fill=TEXT_GREY, font=detail_font)

    # --- "You bought" card ---
    card_y2 = 230
    _draw_rounded_rect(draw, (40, card_y2, width - 40, card_y2 + 100), radius=10, fill=(40, 40, 50), outline=(*accent, 100))

    draw.text((70, card_y2 + 10), str(times_bought), fill=ACCENT_GOLD, font=big_font)
    draw.text((70, card_y2 + 55), "deals you purchased", fill=TEXT_WHITE, font=label_font)
    draw.text((70, card_y2 + 75), f"{jc_spent} JC spent", fill=TEXT_GREY, font=detail_font)

    # --- Total games ---
    games_y = 360
    _draw_rounded_rect(draw, (40, games_y, width - 40, games_y + 70), radius=10, fill=(40, 40, 50), outline=(*ACCENT_GREEN, 80))
    draw.text((70, games_y + 10), str(total_games), fill=ACCENT_GREEN, font=_get_font(28, bold=True))
    draw.text((70, games_y + 45), "games committed across all deals", fill=TEXT_GREY, font=detail_font)

    # Flavor
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
    """
    Wrap a 700x400 chart in an 800x600 wrapped-styled canvas.

    Args:
        chart_bytes: PNG bytes of the chart (700x400)
        title: Title text above the chart
        flavor_text: Flavor text below the chart
        accent_color: Accent color for title
    """
    width, height = 800, 600
    img = Image.new("RGBA", (width, height), (*BG_GRADIENT_START, 255))
    draw = ImageDraw.Draw(img)

    for y in range(height):
        ratio = y / height
        r = int(BG_GRADIENT_START[0] + (BG_GRADIENT_END[0] - BG_GRADIENT_START[0]) * ratio)
        g = int(BG_GRADIENT_START[1] + (BG_GRADIENT_END[1] - BG_GRADIENT_START[1]) * ratio)
        b = int(BG_GRADIENT_START[2] + (BG_GRADIENT_END[2] - BG_GRADIENT_START[2]) * ratio)
        draw.line([(0, y), (width, y)], fill=(r, g, b, 255))

    title_font = _get_font(16, bold=True)
    flavor_font = _get_font(14)

    # Title
    _center_text(draw, title.upper(), title_font, 15, width, accent_color)

    # Chart
    try:
        chart_img = Image.open(io.BytesIO(chart_bytes)).convert("RGBA")
        # Scale to fit if needed
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

    # Flavor text at bottom
    if flavor_text:
        _center_text(draw, flavor_text, flavor_font, height - 40, width, TEXT_GREY)

    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer
