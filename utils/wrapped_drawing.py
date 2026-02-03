"""
Spotify Wrapped style image generation for Cama monthly summaries.
"""

import io
from PIL import Image, ImageDraw, ImageFont
from pilmoji import Pilmoji

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from services.wrapped_service import Award, PlayerWrapped, ServerWrapped

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
    header_text = f"CAMA WRAPPED"
    bbox = draw.textbbox((0, 0), header_text, font=title_font)
    text_w = bbox[2] - bbox[0]
    draw.text(((width - text_w) // 2, 30), header_text, fill=ACCENT_GOLD, font=title_font)

    # Month/Year
    month_text = wrapped.month_name.upper()
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
    name_font = _get_font(24, bold=True)
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


def draw_awards_grid(awards: list["Award"], max_awards: int = 6) -> io.BytesIO:
    """
    Generate a grid of award cards.

    Args:
        awards: List of Award objects
        max_awards: Maximum awards to show

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

    card_width, card_height = 250, 180
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

    # Draw each award card
    for i, award in enumerate(awards):
        row = i // cols
        col = i % cols
        x = padding + col * (card_width + padding)
        y = 60 + padding + row * (card_height + padding)

        # Card background
        accent_color = CATEGORY_COLORS.get(award.category, ACCENT_GOLD)
        _draw_rounded_rect(
            draw,
            (x, y, x + card_width, y + card_height),
            radius=10,
            fill=(40, 40, 50),
            outline=accent_color,
            width=2,
        )

        # Emoji using pilmoji
        if award.emoji:
            with Pilmoji(img) as pilmoji:
                pilmoji.text((x + 10, y + 8), award.emoji, font=emoji_font)

        # Title (next to emoji)
        title_text = award.title.upper()
        if len(title_text) > 18:
            title_text = title_text[:16] + ".."
        draw.text((x + 45, y + 12), title_text, fill=accent_color, font=title_font)

        # Player name
        player_text = f"@{award.discord_username}"
        if len(player_text) > 22:
            player_text = player_text[:20] + ".."
        draw.text((x + 10, y + 50), player_text, fill=TEXT_WHITE, font=name_font)

        # Stat
        draw.text((x + 10, y + 75), award.stat_value, fill=ACCENT_GOLD, font=stat_font)

        # Flavor text (truncated)
        if award.flavor_text:
            flavor = f'"{award.flavor_text}"'
            if len(flavor) > 30:
                flavor = flavor[:28] + '.."'
            draw.text((x + 10, y + 100), flavor, fill=TEXT_GREY, font=stat_font)

    # Save to buffer
    buffer = io.BytesIO()
    img.save(buffer, format="PNG")
    buffer.seek(0)
    return buffer
