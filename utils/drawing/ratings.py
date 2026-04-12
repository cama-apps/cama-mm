"""Rating-related charts (history, distribution, calibration, comparison)."""

from __future__ import annotations

from io import BytesIO

import matplotlib.pyplot as plt
import numpy as np
from PIL import Image, ImageDraw
from scipy import stats

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
)


def draw_rating_history_chart(
    username: str,
    history: list[dict],
) -> BytesIO:
    """
    Generate a dual Y-axis rating history chart with win/loss markers.

    Args:
        username: Player's display name
        history: From get_player_rating_history_detailed, most-recent-first

    Returns:
        BytesIO containing the PNG image
    """
    from openskill_rating_system import CamaOpenSkillSystem

    # Image dimensions (matching gamba chart)
    width = 700
    height = 400
    padding = 60
    padding_right = 60
    header_height = 50
    footer_height = 40
    chart_width = width - padding - padding_right
    chart_height = height - header_height - footer_height - padding

    # Create image
    img = Image.new("RGBA", (width, height), DISCORD_BG)
    draw = ImageDraw.Draw(img)

    # Fonts
    title_font = _get_font(22)
    value_font = _get_font(13)
    legend_font = _get_font(14)

    # Draw header
    title = f"{username}'s Rating History"
    draw.text((padding, 12), title, fill=DISCORD_WHITE, font=title_font)

    # Handle empty data
    if not history or len(history) < 2:
        msg = "No rating history" if not history else "Need 2+ matches for chart"
        text_w = _get_text_size(title_font, msg)[0]
        draw.text(
            ((width - text_w) // 2, height // 2),
            msg,
            fill=DISCORD_GREY,
            font=title_font,
        )
        fp = BytesIO()
        img.save(fp, format="PNG")
        fp.seek(0)
        return fp

    # Reverse to chronological order
    data = list(reversed(history))

    # Extract Glicko values
    glicko_values = [h["rating"] for h in data]
    won_flags = [h.get("won") for h in data]

    # Extract OpenSkill display values (may be None)
    os_values = []
    has_os = False
    for h in data:
        mu = h.get("os_mu_after")
        if mu is not None:
            os_values.append(CamaOpenSkillSystem.mu_to_display(mu))
            has_os = True
        else:
            os_values.append(None)

    # Compute Y ranges with 10% padding
    glicko_min = min(glicko_values)
    glicko_max = max(glicko_values)
    glicko_range = max(glicko_max - glicko_min, 1)
    glicko_min -= glicko_range * 0.1
    glicko_max += glicko_range * 0.1
    glicko_range = glicko_max - glicko_min

    os_min = os_max = os_range = 0
    if has_os:
        os_valid = [v for v in os_values if v is not None]
        if os_valid:
            os_min = min(os_valid)
            os_max = max(os_valid)
            os_range = max(os_max - os_min, 1)
            os_min -= os_range * 0.1
            os_max += os_range * 0.1
            os_range = os_max - os_min

    # Chart origin
    chart_x = padding
    chart_y = header_height + 20

    # Helper: data index to pixel X
    n = len(data)

    def idx_to_x(i: int) -> int:
        return chart_x + int(i / max(n - 1, 1) * chart_width)

    # Helper: Glicko value to pixel Y
    def glicko_to_y(val: float) -> int:
        return chart_y + int((glicko_max - val) / glicko_range * chart_height)

    # Helper: OpenSkill value to pixel Y
    def os_to_y(val: float) -> int:
        if os_range == 0:
            return chart_y + chart_height // 2
        return chart_y + int((os_max - val) / os_range * chart_height)

    # Draw faint horizontal grid lines
    grid_color = "#444444"
    for frac in [0.0, 0.25, 0.5, 0.75, 1.0]:
        gy = chart_y + int(frac * chart_height)
        draw.line([(chart_x, gy), (chart_x + chart_width, gy)], fill=grid_color, width=1)

    # Left Y-axis labels (Glicko, blue, rounded to nearest 50)
    for frac in [0.0, 0.25, 0.5, 0.75, 1.0]:
        val = glicko_max - frac * glicko_range
        label = str(int(round(val / 50) * 50))
        gy = chart_y + int(frac * chart_height)
        text_w = _get_text_size(value_font, label)[0]
        draw.text((chart_x - text_w - 6, gy - 6), label, fill=DISCORD_ACCENT, font=value_font)

    # Right Y-axis labels (OpenSkill display, yellow, rounded to nearest 100)
    if has_os and os_range > 0:
        for frac in [0.0, 0.25, 0.5, 0.75, 1.0]:
            val = os_max - frac * os_range
            label = str(int(round(val / 100) * 100))
            gy = chart_y + int(frac * chart_height)
            draw.text(
                (chart_x + chart_width + 6, gy - 6),
                label,
                fill=DISCORD_YELLOW,
                font=value_font,
            )

    # Draw Glicko line (blue, width=2)
    for i in range(n - 1):
        x1, y1 = idx_to_x(i), glicko_to_y(glicko_values[i])
        x2, y2 = idx_to_x(i + 1), glicko_to_y(glicko_values[i + 1])
        draw.line([(x1, y1), (x2, y2)], fill=DISCORD_ACCENT, width=2)

    # Draw OpenSkill line (yellow, width=2) — skip None gaps
    if has_os:
        for i in range(n - 1):
            v1 = os_values[i]
            v2 = os_values[i + 1]
            if v1 is not None and v2 is not None:
                x1, y1 = idx_to_x(i), os_to_y(v1)
                x2, y2 = idx_to_x(i + 1), os_to_y(v2)
                draw.line([(x1, y1), (x2, y2)], fill=DISCORD_YELLOW, width=2)

    # Draw win/loss dot markers on Glicko line
    dot_r = 3 if n > 30 else 4
    for i in range(n):
        px = idx_to_x(i)
        py = glicko_to_y(glicko_values[i])
        won = won_flags[i]
        if won is None:
            color = DISCORD_GREY
        elif won:
            color = DISCORD_GREEN
        else:
            color = DISCORD_RED
        draw.ellipse(
            [(px - dot_r, py - dot_r), (px + dot_r, py + dot_r)],
            fill=color,
        )

    # Draw legend in footer
    legend_y = chart_y + chart_height + 22
    marker_size = 12

    # Glicko line swatch (blue)
    draw.line(
        [(padding, legend_y + marker_size // 2), (padding + 20, legend_y + marker_size // 2)],
        fill=DISCORD_ACCENT,
        width=2,
    )
    draw.text((padding + 25, legend_y - 1), "Glicko-2", fill=DISCORD_GREY, font=legend_font)

    # OpenSkill line swatch (yellow) — only if data exists
    lx = padding + 110
    if has_os:
        draw.line(
            [(lx, legend_y + marker_size // 2), (lx + 20, legend_y + marker_size // 2)],
            fill=DISCORD_YELLOW,
            width=2,
        )
        draw.text((lx + 25, legend_y - 1), "OpenSkill", fill=DISCORD_GREY, font=legend_font)
        lx += 110

    # Win dot
    draw.ellipse(
        [(lx, legend_y), (lx + marker_size, legend_y + marker_size)],
        fill=DISCORD_GREEN,
    )
    draw.text((lx + marker_size + 5, legend_y - 1), "Win", fill=DISCORD_GREY, font=legend_font)

    # Loss dot
    lx_loss = lx + 60
    draw.ellipse(
        [(lx_loss, legend_y), (lx_loss + marker_size, legend_y + marker_size)],
        fill=DISCORD_RED,
    )
    draw.text((lx_loss + marker_size + 5, legend_y - 1), "Loss", fill=DISCORD_GREY, font=legend_font)

    # Save to BytesIO
    fp = BytesIO()
    img.save(fp, format="PNG")
    fp.seek(0)
    return fp


def draw_rating_distribution(
    ratings: list[float], avg_rating: float | None = None, median_rating: float | None = None
) -> BytesIO:
    """
    Generate a histogram with fitted normal distribution curve overlay.

    Args:
        ratings: List of player ratings
        avg_rating: Optional average rating to display
        median_rating: Optional median rating to display

    Returns:
        BytesIO containing the PNG image
    """
    if not ratings:
        # Return empty image if no data
        fig, ax = plt.subplots(figsize=(6.5, 4), facecolor="#36393F")
        ax.set_facecolor("#2F3136")
        ax.text(0.5, 0.5, "No rating data", ha="center", va="center", color="white", fontsize=14)
        ax.set_xticks([])
        ax.set_yticks([])
        fp = BytesIO()
        fig.savefig(fp, format="PNG", dpi=100, bbox_inches="tight", facecolor="#36393F")
        plt.close(fig)
        fp.seek(0)
        return fp

    ratings_arr = np.array(ratings)

    # Calculate statistics
    mean = np.mean(ratings_arr)
    std = np.std(ratings_arr)
    skewness = stats.skew(ratings_arr)
    kurtosis = stats.kurtosis(ratings_arr)  # Excess kurtosis (0 = normal)

    # Shapiro-Wilk test for normality (only reliable for n < 5000)
    if len(ratings_arr) >= 3:
        if len(ratings_arr) <= 5000:
            shapiro_stat, shapiro_p = stats.shapiro(ratings_arr)
        else:
            # Use D'Agostino-Pearson for larger samples
            shapiro_stat, shapiro_p = stats.normaltest(ratings_arr)
    else:
        _shapiro_stat, shapiro_p = None, None

    # Create figure with Discord-like dark theme
    fig, ax = plt.subplots(figsize=(6.5, 4), facecolor="#36393F")
    ax.set_facecolor("#2F3136")

    # Plot histogram with more granular bins (100-point bins)
    bin_width = 100
    min_rating = max(0, int(min(ratings_arr) // bin_width) * bin_width)
    max_rating = int(np.ceil(max(ratings_arr) / bin_width) * bin_width) + bin_width
    bins = np.arange(min_rating, max_rating + bin_width, bin_width)

    # Plot histogram (density=True to normalize for PDF overlay)
    n, bins_edges, patches = ax.hist(
        ratings_arr,
        bins=bins,
        density=True,
        alpha=0.7,
        color="#5865F2",
        edgecolor="#36393F",
        linewidth=0.5,
        label=f"Data (n={len(ratings)})",
    )

    # Fit and plot normal distribution curve
    x_range = np.linspace(min_rating, max_rating, 200)
    normal_pdf = stats.norm.pdf(x_range, mean, std)
    ax.plot(x_range, normal_pdf, color="#57F287", linewidth=2.5, label="Normal fit", linestyle="-")

    # Also show a kernel density estimate for comparison
    if len(ratings_arr) >= 5:
        kde = stats.gaussian_kde(ratings_arr)
        kde_pdf = kde(x_range)
        ax.plot(x_range, kde_pdf, color="#FEE75C", linewidth=2, label="KDE", linestyle="--", alpha=0.8)

    # Add vertical lines for mean and median
    ax.axvline(mean, color="#ED4245", linestyle="-", linewidth=1.5, alpha=0.8, label=f"Mean: {mean:.0f}")
    if median_rating is not None:
        ax.axvline(median_rating, color="#F47B67", linestyle="--", linewidth=1.5, alpha=0.8, label=f"Median: {median_rating:.0f}")

    # Style the plot
    ax.set_xlabel("Rating", color="#B9BBBE", fontsize=11)
    ax.set_ylabel("Density", color="#B9BBBE", fontsize=11)
    ax.tick_params(colors="#B9BBBE", labelsize=9)
    for spine in ax.spines.values():
        spine.set_color("#4F545C")

    # Title with stats
    title = f"Rating Distribution (n={len(ratings)})"
    ax.set_title(title, color="white", fontsize=13, fontweight="bold", pad=10)

    # Legend
    ax.legend(loc="upper right", facecolor="#2F3136", edgecolor="#4F545C", labelcolor="white", fontsize=8)

    # Add stats annotation box
    normality_text = ""
    if shapiro_p is not None:
        if shapiro_p > 0.05:
            normality_text = f"Normal (p={shapiro_p:.3f})"
        else:
            normality_text = f"Non-normal (p={shapiro_p:.3f})"

    stats_text = f"μ={mean:.0f}, σ={std:.0f}\nSkew={skewness:.2f}, Kurt={kurtosis:.2f}"
    if normality_text:
        stats_text += f"\n{normality_text}"

    ax.text(
        0.02, 0.98, stats_text,
        transform=ax.transAxes,
        fontsize=9,
        verticalalignment="top",
        color="#B9BBBE",
        family="monospace",
        bbox={"boxstyle": "round,pad=0.3", "facecolor": "#2F3136", "edgecolor": "#4F545C", "alpha": 0.9},
    )

    plt.tight_layout()

    # Save to BytesIO
    fp = BytesIO()
    fig.savefig(fp, format="PNG", dpi=100, bbox_inches="tight", facecolor="#36393F")
    plt.close(fig)
    fp.seek(0)
    return fp


def draw_calibration_curve(
    glicko_data: list[tuple[float, float, int]],
    openskill_data: list[tuple[float, float, int]],
) -> BytesIO:
    """
    Draw calibration curves comparing Glicko-2 and OpenSkill predictions.

    A well-calibrated system has points on the diagonal (predicted = actual).

    Args:
        glicko_data: List of (avg_predicted, actual_rate, count) tuples for Glicko-2
        openskill_data: List of (avg_predicted, actual_rate, count) tuples for OpenSkill

    Returns:
        BytesIO containing the PNG image
    """
    fig, ax = plt.subplots(figsize=(6.5, 5), facecolor=DISCORD_BG)
    ax.set_facecolor(DISCORD_DARKER)

    # Perfect calibration line
    ax.plot([0, 1], [0, 1], color=DISCORD_GREY, linestyle="--", linewidth=1.5,
            label="Perfect calibration", alpha=0.7)

    # Glicko-2 curve
    if glicko_data:
        g_predicted = [d[0] for d in glicko_data]
        g_actual = [d[1] for d in glicko_data]
        g_counts = [d[2] for d in glicko_data]
        # Size points by sample count
        sizes = [min(200, 20 + c * 3) for c in g_counts]
        ax.scatter(g_predicted, g_actual, s=sizes, c=DISCORD_ACCENT, alpha=0.8,
                   label="Glicko-2", edgecolors="white", linewidths=0.5)
        if len(g_predicted) > 1:
            ax.plot(g_predicted, g_actual, color=DISCORD_ACCENT, alpha=0.5, linewidth=1)

    # OpenSkill curve
    if openskill_data:
        o_predicted = [d[0] for d in openskill_data]
        o_actual = [d[1] for d in openskill_data]
        o_counts = [d[2] for d in openskill_data]
        sizes = [min(200, 20 + c * 3) for c in o_counts]
        ax.scatter(o_predicted, o_actual, s=sizes, c=DISCORD_GREEN, alpha=0.8,
                   label="OpenSkill", edgecolors="white", linewidths=0.5, marker="s")
        if len(o_predicted) > 1:
            ax.plot(o_predicted, o_actual, color=DISCORD_GREEN, alpha=0.5, linewidth=1)

    # Styling
    ax.set_xlabel("Predicted Win Probability", color=DISCORD_GREY, fontsize=11)
    ax.set_ylabel("Actual Win Rate", color=DISCORD_GREY, fontsize=11)
    ax.set_xlim(0, 1)
    ax.set_ylim(0, 1)
    ax.tick_params(colors=DISCORD_GREY, labelsize=9)
    for spine in ax.spines.values():
        spine.set_color("#4F545C")

    ax.set_title("Calibration Curve: Predicted vs Actual", color="white",
                 fontsize=13, fontweight="bold", pad=10)
    ax.legend(loc="lower right", facecolor=DISCORD_DARKER, edgecolor="#4F545C",
              labelcolor="white", fontsize=9)

    # Add grid for easier reading
    ax.grid(True, alpha=0.2, color=DISCORD_GREY)

    plt.tight_layout()

    fp = BytesIO()
    fig.savefig(fp, format="PNG", dpi=100, bbox_inches="tight", facecolor=DISCORD_BG)
    plt.close(fig)
    fp.seek(0)
    return fp


def draw_rating_comparison_chart(comparison_data: dict) -> BytesIO:
    """
    Draw a comparison chart showing Glicko-2 vs OpenSkill metrics.

    Args:
        comparison_data: Dict from RatingComparisonService.get_comparison_summary()

    Returns:
        BytesIO containing the PNG image
    """
    if "error" in comparison_data:
        # Return error image
        fig, ax = plt.subplots(figsize=(6.5, 4), facecolor=DISCORD_BG)
        ax.set_facecolor(DISCORD_DARKER)
        ax.text(0.5, 0.5, comparison_data["error"], ha="center", va="center",
                color="white", fontsize=14)
        ax.set_xticks([])
        ax.set_yticks([])
        fp = BytesIO()
        fig.savefig(fp, format="PNG", dpi=100, bbox_inches="tight", facecolor=DISCORD_BG)
        plt.close(fig)
        fp.seek(0)
        return fp

    glicko = comparison_data["glicko"]
    openskill = comparison_data["openskill"]

    fig, axes = plt.subplots(1, 3, figsize=(10, 4), facecolor=DISCORD_BG)

    metrics = [
        ("Brier Score\n(Lower = Better)", "brier_score", True),
        ("Accuracy\n(Higher = Better)", "accuracy", False),
        ("Log Loss\n(Lower = Better)", "log_loss", True),
    ]

    for ax, (title, key, lower_is_better) in zip(axes, metrics):
        ax.set_facecolor(DISCORD_DARKER)

        g_val = glicko[key]
        o_val = openskill[key]

        bars = ax.bar(
            ["Glicko-2", "OpenSkill"],
            [g_val, o_val],
            color=[DISCORD_ACCENT, DISCORD_GREEN],
            edgecolor="white",
            linewidth=0.5,
        )

        # Highlight winner
        if lower_is_better:
            winner_idx = 0 if g_val < o_val else 1
        else:
            winner_idx = 0 if g_val > o_val else 1
        bars[winner_idx].set_edgecolor(DISCORD_YELLOW)
        bars[winner_idx].set_linewidth(2)

        ax.set_title(title, color="white", fontsize=10, fontweight="bold")
        ax.tick_params(colors=DISCORD_GREY, labelsize=9)
        for spine in ax.spines.values():
            spine.set_color("#4F545C")

        # Add value labels on bars
        for bar, val in zip(bars, [g_val, o_val]):
            height = bar.get_height()
            ax.annotate(
                f"{val:.3f}",
                xy=(bar.get_x() + bar.get_width() / 2, height),
                xytext=(0, 3),
                textcoords="offset points",
                ha="center", va="bottom",
                color="white", fontsize=9,
            )

    fig.suptitle(
        f"Rating System Comparison ({comparison_data['matches_analyzed']} matches)",
        color="white", fontsize=12, fontweight="bold", y=1.02,
    )

    plt.tight_layout()

    fp = BytesIO()
    fig.savefig(fp, format="PNG", dpi=100, bbox_inches="tight", facecolor=DISCORD_BG)
    plt.close(fig)
    fp.seek(0)
    return fp

