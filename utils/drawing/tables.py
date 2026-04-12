"""Match history table rendering (``draw_matches_table``)."""

from __future__ import annotations

from io import BytesIO

from PIL import Image, ImageDraw

from utils.drawing._common import (
    DISCORD_ACCENT,
    DISCORD_BG,
    DISCORD_DARKER,
    DISCORD_GREEN,
    DISCORD_GREY,
    DISCORD_RED,
    DISCORD_WHITE,
    _get_font,
    _get_text_size,
)


def draw_matches_table(
    matches: list[dict],
    hero_names: dict[int, str] | None = None,
) -> BytesIO:
    """
    Generate a PNG image of recent matches table.

    Args:
        matches: List of match dicts with keys: hero_id, kills, deaths, assists,
                 duration, won, match_id, game_mode (optional)
        hero_names: Optional dict mapping hero_id to hero name

    Returns:
        BytesIO containing the PNG image
    """
    if not matches:
        # Return empty image
        img = Image.new("RGBA", (400, 100), DISCORD_BG)
        draw = ImageDraw.Draw(img)
        font = _get_font(20)
        draw.text((20, 40), "No matches found", fill=DISCORD_GREY, font=font)
        fp = BytesIO()
        img.save(fp, format="PNG")
        fp.seek(0)
        return fp

    # Fonts
    header_font = _get_font(20)
    cell_font = _get_font(16)

    # Column definitions: (header, width, align)
    columns = [
        ("Hero", 120, "left"),
        ("K", 35, "center"),
        ("D", 35, "center"),
        ("A", 35, "center"),
        ("Result", 55, "center"),
        ("Duration", 70, "center"),
    ]

    # Calculate dimensions
    row_height = 36
    header_height = 32
    padding = 10
    total_width = sum(c[1] for c in columns) + padding * 2
    total_height = header_height + len(matches) * row_height + padding * 2

    # Create image
    img = Image.new("RGBA", (total_width, total_height), DISCORD_BG)
    draw = ImageDraw.Draw(img)

    # Draw header row
    x = padding
    y = padding
    for header, width, _ in columns:
        draw.text((x + 5, y + 8), header, fill=DISCORD_WHITE, font=header_font)
        x += width

    # Header underline
    draw.line(
        [(padding, y + header_height - 2), (total_width - padding, y + header_height - 2)],
        fill=DISCORD_ACCENT,
        width=2,
    )

    # Draw match rows
    y = padding + header_height
    for i, match in enumerate(matches):
        # Alternate row background
        if i % 2 == 1:
            draw.rectangle(
                [(padding, y), (total_width - padding, y + row_height)],
                fill=DISCORD_DARKER,
            )

        x = padding

        # Hero name
        hero_id = match.get("hero_id", 0)
        hero_name = "Unknown"
        if hero_names and hero_id in hero_names:
            hero_name = hero_names[hero_id]
        elif match.get("hero_name"):
            hero_name = match["hero_name"]

        # Truncate long hero names
        if len(hero_name) > 14:
            hero_name = hero_name[:12] + ".."

        draw.text((x + 5, y + 10), hero_name, fill=DISCORD_WHITE, font=cell_font)
        x += columns[0][1]

        # KDA
        kills = str(match.get("kills", 0))
        deaths = str(match.get("deaths", 0))
        assists = str(match.get("assists", 0))

        for val, (_, width, _) in zip([kills, deaths, assists], columns[1:4]):
            text_w = _get_text_size(cell_font, val)[0]
            draw.text((x + (width - text_w) // 2, y + 10), val, fill=DISCORD_WHITE, font=cell_font)
            x += width

        # Result
        won = match.get("won", match.get("radiant_win"))
        if isinstance(won, bool):
            result_text = "Win" if won else "Loss"
            result_color = DISCORD_GREEN if won else DISCORD_RED
        else:
            result_text = "?"
            result_color = DISCORD_GREY

        text_w = _get_text_size(cell_font, result_text)[0]
        draw.text(
            (x + (columns[4][1] - text_w) // 2, y + 10),
            result_text,
            fill=result_color,
            font=cell_font,
        )
        x += columns[4][1]

        # Duration
        duration = match.get("duration", 0)
        if duration:
            mins = duration // 60
            secs = duration % 60
            duration_text = f"{mins}:{secs:02d}"
        else:
            duration_text = "-"

        text_w = _get_text_size(cell_font, duration_text)[0]
        draw.text(
            (x + (columns[5][1] - text_w) // 2, y + 10),
            duration_text,
            fill=DISCORD_GREY,
            font=cell_font,
        )

        y += row_height

    # Save to BytesIO
    fp = BytesIO()
    img.save(fp, format="PNG")
    fp.seek(0)
    return fp

