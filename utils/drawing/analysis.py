"""Match-analysis charts: prediction-over-time, advantage graph, scout report."""

from __future__ import annotations

from io import BytesIO

import matplotlib.pyplot as plt
import numpy as np
from PIL import Image, ImageDraw

from utils.drawing._common import (
    DISCORD_ACCENT,
    DISCORD_BG,
    DISCORD_DARKER,
    DISCORD_GREEN,
    DISCORD_GREY,
    DISCORD_RED,
    DISCORD_WHITE,
    DISCORD_YELLOW,
    _get_font,
    _get_text_size,
    _heatmap_contest_rate,
    _heatmap_winrate,
)
from utils.drawing.heroes import _get_hero_images_batch


def draw_prediction_over_time(match_data: list[dict], window: int = 20) -> BytesIO:
    """
    Draw rolling accuracy of predictions over time for both systems.

    Args:
        match_data: List of match dicts with prediction data (chronological)
        window: Rolling window size for smoothing

    Returns:
        BytesIO containing the PNG image
    """
    if len(match_data) < window:
        # Return error image
        fig, ax = plt.subplots(figsize=(8, 4), facecolor=DISCORD_BG)
        ax.set_facecolor(DISCORD_DARKER)
        ax.text(0.5, 0.5, f"Need at least {window} matches for trend analysis",
                ha="center", va="center", color="white", fontsize=14)
        ax.set_xticks([])
        ax.set_yticks([])
        fp = BytesIO()
        fig.savefig(fp, format="PNG", dpi=100, bbox_inches="tight", facecolor=DISCORD_BG)
        plt.close(fig)
        fp.seek(0)
        return fp

    fig, ax = plt.subplots(figsize=(8, 4), facecolor=DISCORD_BG)
    ax.set_facecolor(DISCORD_DARKER)

    # Calculate rolling accuracy
    n = len(match_data)
    glicko_rolling = []
    openskill_rolling = []
    x_vals = []

    for i in range(window, n + 1):
        window_data = match_data[i - window:i]
        g_correct = sum(1 for m in window_data if m["glicko_correct"])
        o_correct = sum(1 for m in window_data if m["openskill_correct"])
        glicko_rolling.append(g_correct / window)
        openskill_rolling.append(o_correct / window)
        x_vals.append(i)

    ax.plot(x_vals, glicko_rolling, color=DISCORD_ACCENT, linewidth=2,
            label=f"Glicko-2 ({window}-match rolling)")
    ax.plot(x_vals, openskill_rolling, color=DISCORD_GREEN, linewidth=2,
            label=f"OpenSkill ({window}-match rolling)")

    # 50% reference line (coin flip)
    ax.axhline(0.5, color=DISCORD_GREY, linestyle="--", alpha=0.5, label="Coin flip (50%)")

    ax.set_xlabel("Match Number", color=DISCORD_GREY, fontsize=11)
    ax.set_ylabel("Prediction Accuracy", color=DISCORD_GREY, fontsize=11)
    ax.set_ylim(0.3, 0.9)
    ax.tick_params(colors=DISCORD_GREY, labelsize=9)
    for spine in ax.spines.values():
        spine.set_color("#4F545C")

    ax.set_title("Prediction Accuracy Over Time", color="white",
                 fontsize=13, fontweight="bold", pad=10)
    ax.legend(loc="lower right", facecolor=DISCORD_DARKER, edgecolor="#4F545C",
              labelcolor="white", fontsize=9)
    ax.grid(True, alpha=0.2, color=DISCORD_GREY)

    plt.tight_layout()

    fp = BytesIO()
    fig.savefig(fp, format="PNG", dpi=100, bbox_inches="tight", facecolor=DISCORD_BG)
    plt.close(fig)
    fp.seek(0)
    return fp


def draw_advantage_graph(
    enrichment_data: dict,
    match_id: int | None = None,
) -> BytesIO | None:
    """
    Draw a team advantage per minute graph (gold + XP) from OpenDota enrichment data.

    Args:
        enrichment_data: Parsed OpenDota match JSON (from enrichment_data column)
        match_id: Optional match ID for the title

    Returns:
        BytesIO containing PNG image, or None if no advantage data available
    """
    gold_adv = enrichment_data.get("radiant_gold_adv")
    xp_adv = enrichment_data.get("radiant_xp_adv")

    if not gold_adv and not xp_adv:
        return None

    fig, ax = plt.subplots(figsize=(8, 3.5), facecolor=DISCORD_BG)
    ax.set_facecolor(DISCORD_DARKER)

    has_legend = False

    if gold_adv:
        minutes = list(range(len(gold_adv)))
        gold_arr = np.array(gold_adv, dtype=float)
        ax.plot(minutes, gold_arr, color=DISCORD_YELLOW, linewidth=2, label="Gold", zorder=3)
        ax.fill_between(minutes, gold_arr, 0, where=gold_arr >= 0,
                        color=DISCORD_GREEN, alpha=0.15, interpolate=True)
        ax.fill_between(minutes, gold_arr, 0, where=gold_arr <= 0,
                        color=DISCORD_RED, alpha=0.15, interpolate=True)
        has_legend = True

    if xp_adv:
        minutes_xp = list(range(len(xp_adv)))
        ax.plot(minutes_xp, xp_adv, color=DISCORD_ACCENT, linewidth=1.5,
                linestyle="--", label="XP", zorder=2)
        has_legend = True

    # Zero reference line
    ax.axhline(0, color=DISCORD_GREY, linewidth=0.8, alpha=0.5)

    # Radiant/Dire labels
    ax.text(0.01, 0.97, "Radiant", transform=ax.transAxes, color=DISCORD_GREEN,
            fontsize=9, va="top", ha="left", alpha=0.7)
    ax.text(0.01, 0.03, "Dire", transform=ax.transAxes, color=DISCORD_RED,
            fontsize=9, va="bottom", ha="left", alpha=0.7)

    # Format y-axis with "k" suffix
    ax.yaxis.set_major_formatter(plt.FuncFormatter(
        lambda v, _: f"{v / 1000:.0f}k" if abs(v) >= 1000 else f"{v:.0f}"
    ))

    ax.set_xlabel("Minutes", color=DISCORD_GREY, fontsize=10)
    ax.set_ylabel("Advantage", color=DISCORD_GREY, fontsize=10)
    ax.tick_params(colors=DISCORD_GREY, labelsize=9)
    for spine in ax.spines.values():
        spine.set_color("#4F545C")
    ax.grid(True, alpha=0.15, color=DISCORD_GREY)

    title_text = "Team Advantages Per Minute"
    if match_id is not None:
        title_text += f" — Match #{match_id}"
    ax.set_title(title_text, color="white", fontsize=12, fontweight="bold", pad=8)

    if has_legend:
        ax.legend(loc="upper right", facecolor=DISCORD_DARKER, edgecolor="#4F545C",
                  labelcolor="white", fontsize=9)

    plt.tight_layout()

    fp = BytesIO()
    fig.savefig(fp, format="PNG", dpi=100, bbox_inches="tight", facecolor=DISCORD_BG)
    plt.close(fig)
    fp.seek(0)
    return fp



def draw_scout_report(
    scout_data: dict,
    player_names: list[str],
    title: str = "SCOUT REPORT",
) -> BytesIO:
    """
    Generate a visual scouting report with hero portraits.

    Shows aggregated hero stats for a team/group of players in a compact
    table format with hero portrait images.

    Layout (~360px width, mobile-friendly):
    +----------------------------------------+
    |          SCOUT REPORT                  |
    | Player1, Player2, ...                  |
    +----------------------------------------+
    | Hero  Tot  CR%  W   L   B   WR%       |
    +----------------------------------------+
    | [IMG]  25  85%  18  7   2   72%       |
    | [IMG]  20  68%  12  8   0   60%       |
    | ...                                    |
    +----------------------------------------+

    Columns: Hero | Tot (W+L+Bans) | CR% (contest rate) | W | L | B | WR% (win rate)
    CR% and WR% use heatmap coloring.

    Args:
        scout_data: Dict from get_scout_data() with player_count, total_matches, and heroes list
        player_names: List of player display names (for header)
        title: Title text for the report

    Returns:
        BytesIO containing the PNG image
    """
    heroes = scout_data.get("heroes", [])
    total_matches = scout_data.get("total_matches", 0)

    # Handle empty data
    if not heroes:
        img = Image.new("RGBA", (360, 100), DISCORD_BG)
        draw = ImageDraw.Draw(img)
        font = _get_font(16)
        draw.text((20, 40), "No hero data available", fill=DISCORD_GREY, font=font)
        fp = BytesIO()
        img.save(fp, format="PNG")
        fp.seek(0)
        return fp

    # Dimensions
    WIDTH = 360
    PADDING = 12
    HEADER_HEIGHT = 50
    ROW_HEIGHT = 32
    HERO_IMG_WIDTH = 48
    HERO_IMG_HEIGHT = 27

    # Calculate height based on number of heroes
    num_heroes = len(heroes)
    height = PADDING + HEADER_HEIGHT + (num_heroes * ROW_HEIGHT) + PADDING

    # Create image
    img = Image.new("RGBA", (WIDTH, height), DISCORD_BG)
    draw = ImageDraw.Draw(img)

    # Fonts
    title_font = _get_font(14)
    player_font = _get_font(11)
    stat_font = _get_font(13)

    # --- Draw header ---
    # Title
    title_w = _get_text_size(title_font, title)[0]
    draw.text(((WIDTH - title_w) // 2, PADDING), title, fill=DISCORD_WHITE, font=title_font)

    # Player names (truncated)
    if player_names:
        names_text = ", ".join(player_names[:5])
        if len(player_names) > 5:
            names_text += f" +{len(player_names) - 5}"
        # Truncate if too long
        max_name_len = 38
        if len(names_text) > max_name_len:
            names_text = names_text[:max_name_len - 2] + ".."
        names_w = _get_text_size(player_font, names_text)[0]
        draw.text(
            ((WIDTH - names_w) // 2, PADDING + 22),
            names_text,
            fill=DISCORD_GREY,
            font=player_font,
        )

    # Fixed column positions for alignment
    # Layout: Hero | Tot | CR% | W | L | B | WR%
    COL_HERO_X = PADDING + 4
    COL_TOTAL_X = COL_HERO_X + HERO_IMG_WIDTH + 8   # 72
    COL_CR_X = COL_TOTAL_X + 34                      # 106
    COL_W_X = COL_CR_X + 40                          # 146
    COL_L_X = COL_W_X + 28                           # 174
    COL_B_X = COL_L_X + 28                           # 202
    COL_WR_X = COL_B_X + 28                          # 230

    # Column headers
    header_font = _get_font(11)
    header_y = PADDING + HEADER_HEIGHT - 18
    draw.text((COL_TOTAL_X, header_y), "Tot", fill=DISCORD_GREY, font=header_font)
    draw.text((COL_CR_X, header_y), "CR", fill=DISCORD_GREY, font=header_font)
    draw.text((COL_W_X, header_y), "W", fill=DISCORD_GREY, font=header_font)
    draw.text((COL_L_X, header_y), "L", fill=DISCORD_GREY, font=header_font)
    draw.text((COL_B_X, header_y), "B", fill=DISCORD_GREY, font=header_font)
    draw.text((COL_WR_X, header_y), "WR", fill=DISCORD_GREY, font=header_font)

    # Header separator line
    sep_y = PADDING + HEADER_HEIGHT - 5
    draw.line([(PADDING, sep_y), (WIDTH - PADDING, sep_y)], fill=DISCORD_ACCENT, width=1)

    # --- Fetch hero images ---
    hero_ids = [h["hero_id"] for h in heroes]
    hero_images = _get_hero_images_batch(hero_ids, (HERO_IMG_WIDTH, HERO_IMG_HEIGHT))

    # --- Draw hero rows ---
    y = PADDING + HEADER_HEIGHT

    for i, hero in enumerate(heroes):
        hero_id = hero["hero_id"]
        wins = hero["wins"]
        losses = hero["losses"]
        bans = hero.get("bans", 0)
        games = wins + losses
        total = games + bans

        # Contest rate and win rate
        contest_rate = total / total_matches if total_matches > 0 else 0.0
        win_rate = wins / games if games > 0 else 0.0

        # Alternate row background
        if i % 2 == 1:
            draw.rectangle(
                [(PADDING, y), (WIDTH - PADDING, y + ROW_HEIGHT)],
                fill=DISCORD_DARKER,
            )

        stat_y = y + (ROW_HEIGHT - 15) // 2

        # Hero portrait (fixed position)
        hero_img = hero_images.get(hero_id)
        if hero_img:
            img_y = y + (ROW_HEIGHT - HERO_IMG_HEIGHT) // 2
            img.paste(hero_img, (COL_HERO_X, img_y), hero_img)
        else:
            img_y = y + (ROW_HEIGHT - HERO_IMG_HEIGHT) // 2
            draw.rectangle(
                [(COL_HERO_X, img_y), (COL_HERO_X + HERO_IMG_WIDTH, img_y + HERO_IMG_HEIGHT)],
                fill=DISCORD_DARKER,
                outline=DISCORD_GREY,
            )

        # Total count (right-aligned)
        total_text = str(total)
        tw = _get_text_size(stat_font, total_text)[0]
        draw.text(
            (max(COL_TOTAL_X, COL_TOTAL_X + 25 - tw), stat_y),
            total_text,
            fill=DISCORD_WHITE,
            font=stat_font,
        )

        # Contest rate % (heatmap colored, right-aligned)
        cr_text = f"{contest_rate * 100:.0f}%"
        cr_w = _get_text_size(stat_font, cr_text)[0]
        cr_color = _heatmap_contest_rate(contest_rate)
        draw.text(
            (max(COL_CR_X, COL_CR_X + 32 - cr_w), stat_y),
            cr_text,
            fill=cr_color,
            font=stat_font,
        )

        # Wins (green, right-aligned)
        w_text = str(wins)
        w_tw = _get_text_size(stat_font, w_text)[0]
        draw.text(
            (max(COL_W_X, COL_W_X + 20 - w_tw), stat_y),
            w_text, fill=DISCORD_GREEN, font=stat_font,
        )

        # Losses (red, right-aligned)
        l_text = str(losses)
        l_tw = _get_text_size(stat_font, l_text)[0]
        draw.text(
            (max(COL_L_X, COL_L_X + 20 - l_tw), stat_y),
            l_text, fill=DISCORD_RED, font=stat_font,
        )

        # Bans (red if > 0, grey if 0, right-aligned)
        b_text = str(bans)
        b_tw = _get_text_size(stat_font, b_text)[0]
        draw.text(
            (max(COL_B_X, COL_B_X + 20 - b_tw), stat_y),
            b_text, fill=DISCORD_RED if bans > 0 else DISCORD_GREY, font=stat_font,
        )

        # Win rate % (heatmap colored, right-aligned)
        if games > 0:
            wr_text = f"{win_rate * 100:.0f}%"
            wr_color = _heatmap_winrate(win_rate)
        else:
            wr_text = "-"
            wr_color = DISCORD_GREY
        wr_w = _get_text_size(stat_font, wr_text)[0]
        draw.text(
            (max(COL_WR_X, COL_WR_X + 32 - wr_w), stat_y),
            wr_text,
            fill=wr_color,
            font=stat_font,
        )

        y += ROW_HEIGHT

    # Save to BytesIO
    fp = BytesIO()
    img.save(fp, format="PNG")
    fp.seek(0)
    return fp
