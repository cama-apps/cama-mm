#!/usr/bin/env python3
"""Cross-language visual-equivalence gate for representative media.

The normal Rust/Python unit suites stay hermetic and do not invoke one
another.  Run this explicit, dependency-aware gate from the repository root:

    uv run --locked python scripts/visual_equivalence.py

    It renders production prediction-market, balance-journey, rating-history,
    rating distribution, rating-analysis comparison, calibration, trend,
    OpenDota advantage, profile role/lane/hero/recent-match media, betting
    wheel/explosion, Blame Luke, scout report, Hero Grid, post-match,
    terminal-crash, pinnacle phase-3, and one production pet-card attachment from
the shared JSON fixture, then decodes both
sides to RGBA.
Geometry, animation metadata, and ordered frame correspondence are checked
separately from pixel similarity.  The native Rust renderers intentionally use
an embedded bitmap font, while Python uses its configured Pillow font, so exact
pixel identity is not an appropriate acceptance criterion.  The thresholds
below are fixed regression guards, not tuning knobs: a future implementation
must either remain within them or update this gate with an explicit review of
the changed visual contract.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import math
import random
import subprocess
import sys
import tempfile
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import Any

from PIL import Image, ImageSequence

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_FIXTURE = ROOT / "rust" / "crates" / "cama-app" / "tests" / "fixtures" / "visual_equivalence.json"
if str(ROOT) not in sys.path:
    # Executing a file under scripts/ places that directory, rather than the
    # repository root, on sys.path.  Keep the explicit runner self-contained
    # when invoked as `python scripts/visual_equivalence.py`.
    sys.path.insert(0, str(ROOT))

# These limits are intentionally conservative relative to the first native
# bitmap-font port.  They apply to every decoded RGBA channel, including alpha.
# MAE/RMS are normalized to 0..1 (channel difference divided by 255).
CHART_MAX_MAE = 0.080
CHART_MAX_RMS = 0.180
BALANCE_MAX_MAE = 0.040
BALANCE_MAX_RMS = 0.100
ANIMATION_MAX_MEAN_FRAME_MAE = 0.130
ANIMATION_MAX_MEAN_FRAME_RMS = 0.260
ANIMATION_MAX_FRAME_MAE = 0.260
TERMINAL_CRASH_MAX_MEAN_FRAME_MAE = 0.060
TERMINAL_CRASH_MAX_MEAN_FRAME_RMS = 0.180
TERMINAL_CRASH_MAX_FRAME_MAE = 0.100
TERMINAL_CRASH_MIN_FOREGROUND_GRID_IOU = 0.08
TERMINAL_CRASH_MIN_FOREGROUND_COUNT_RATIO = 0.45
PINNACLE_MAX_MEAN_FRAME_MAE = 0.075
PINNACLE_MAX_MEAN_FRAME_RMS = 0.120
PINNACLE_MAX_FRAME_MAE = 0.085
FOREGROUND_CHANNEL_THRESHOLD = 80
CHART_MIN_FOREGROUND_GRID_IOU = 0.80
# The rating-analysis comparison uses the live Python Matplotlib renderer on
# one side and the native Rust bitmap renderer on the other.  Their fonts and
# raster primitives differ, but the three metric panels, bars, winner
# outlines, and labels must remain in the same coarse layout.
RATING_ANALYSIS_MAX_MAE = 0.180
RATING_ANALYSIS_MAX_RMS = 0.340
RATING_ANALYSIS_MIN_FOREGROUND_GRID_IOU = 0.70
RATING_ANALYSIS_MIN_FOREGROUND_COUNT_RATIO = 0.35
RATING_ANALYSIS_CALIBRATION_MAX_MAE = 0.180
RATING_ANALYSIS_CALIBRATION_MAX_RMS = 0.340
RATING_ANALYSIS_CALIBRATION_MIN_SERIES_GRID_IOU = 0.55
RATING_ANALYSIS_CALIBRATION_MIN_SERIES_COUNT_RATIO = 0.30
RATING_ANALYSIS_TREND_MAX_MAE = 0.180
RATING_ANALYSIS_TREND_MAX_RMS = 0.340
RATING_ANALYSIS_TREND_MIN_SERIES_GRID_IOU = 0.70
RATING_ANALYSIS_TREND_MIN_SERIES_COUNT_RATIO = 0.30
# The calibration rating distribution crosses Matplotlib/Scipy and the native
# bitmap renderer at the representative live 640x390 attachment boundary. Its histogram,
# normal fit, KDE, mean, and explicit median each get a separate palette gate
# in addition to coarse semantic placement and bounded whole-frame drift.
RATING_DISTRIBUTION_MAX_MAE = 0.080
RATING_DISTRIBUTION_MAX_RMS = 0.180
RATING_DISTRIBUTION_MIN_SEMANTIC_GRID_IOU = 0.90
RATING_DISTRIBUTION_MIN_COLOR_GRID_IOU = 0.82
RATING_DISTRIBUTION_MIN_COLOR_COUNT_RATIO = 0.20
RATING_DISTRIBUTION_COLOR_DISTANCE = 60
RATING_DISTRIBUTION_COLORS = {
    "histogram": (88, 101, 242),
    "normal": (87, 242, 135),
    "kde": (254, 231, 92),
    "mean": (237, 66, 69),
    "median": (244, 123, 103),
}
RATING_DISTRIBUTION_COLOR_VARIANTS = {
    "histogram": ((88, 101, 242), (76, 85, 186)),
    "normal": ((87, 242, 135),),
    "kde": ((254, 231, 92), (213, 195, 84)),
    "mean": ((237, 66, 69), (199, 63, 66)),
    "median": ((244, 123, 103), (205, 108, 93)),
}
ANIMATION_MIN_FOREGROUND_GRID_IOU = 0.65
DIG_NEON_MAX_MEAN_FRAME_MAE = 0.135
DIG_NEON_MAX_MEAN_FRAME_RMS = 0.255
DIG_NEON_MAX_FRAME_MAE = 0.280
# The native family uses an indexed palette and bitmap font while Python's
# source uses Pillow's alpha-composited radial glow and a host font. Keep the
# pixel gate bounded but record that coarse masks are not byte-identical; the
# Rust phase-state tests independently keep the authored motion/text timing
# strict.
DIG_NEON_TERMINAL_MIN_FOREGROUND_GRID_IOU = 0.50
DIG_NEON_PRESTIGE_MIN_FOREGROUND_GRID_IOU = 0.25
DIG_NEON_MIN_FOREGROUND_COUNT_RATIO = 0.50
PINNACLE_MIN_FOREGROUND_GRID_IOU = 0.85
PINNACLE_MIN_FOREGROUND_COUNT_RATIO = 0.80
BALANCE_MIN_FOREGROUND_GRID_IOU = 0.80
BALANCE_MIN_FOREGROUND_COUNT_RATIO = 0.80
MIN_FOREGROUND_COUNT_RATIO = 0.50
MIN_FOREGROUND_PIXELS = 200
# Wheel and explosion intentionally use different raster primitives and
# palettes from Pillow.  These limits constrain broad shape/timing drift while
# allowing the two implementations' fonts and particle trajectories to differ.
WHEEL_MAX_MEAN_FRAME_MAE = 0.190
WHEEL_MAX_MEAN_FRAME_RMS = 0.330
WHEEL_MAX_FRAME_MAE = 0.400
WHEEL_MIN_FOREGROUND_GRID_IOU = 0.55
WHEEL_MIN_FOREGROUND_COUNT_RATIO = 0.35
EXPLOSION_MAX_MEAN_FRAME_MAE = 0.230
EXPLOSION_MAX_MEAN_FRAME_RMS = 0.390
EXPLOSION_MAX_FRAME_MAE = 0.650
EXPLOSION_MIN_AGGREGATE_GRID_IOU = 0.75
EXPLOSION_MIN_AGGREGATE_COUNT_RATIO = 0.40
EXPLOSION_MAX_AGGREGATE_COUNT_RATIO = 2.25
EXPLOSION_MIN_AGGREGATE_CELL_RATIO = 0.75
EXPLOSION_MAX_AGGREGATE_CELL_RATIO = 1.25
EXPLOSION_MAX_AGGREGATE_CENTROID_DRIFT = 24.0
BLAME_LUKE_MAX_MEAN_FRAME_MAE = 0.160
BLAME_LUKE_MAX_MEAN_FRAME_RMS = 0.300
BLAME_LUKE_MAX_FRAME_MAE = 0.300
BLAME_LUKE_MIN_FOREGROUND_GRID_IOU = 0.65
BLAME_LUKE_MIN_FOREGROUND_COUNT_RATIO = 0.45
BLAME_LUKE_MIN_AGGREGATE_GRID_IOU = 0.80
BLAME_LUKE_MIN_AGGREGATE_COUNT_RATIO = 0.70
BLAME_LUKE_MAX_AGGREGATE_COUNT_RATIO = 1.40
BLAME_LUKE_MAX_AGGREGATE_CENTROID_DRIFT = 18.0
# Scout reports share the live 360px mobile table geometry, but Pillow's
# TrueType text and the native renderer's bitmap glyphs are intentionally not
# pixel-identical.  This gate constrains both the report's row structure and
# its semantic foreground without requiring the network/cache portrait path.
SCOUT_MAX_MAE = 0.080
SCOUT_MAX_RMS = 0.180
SCOUT_MIN_FOREGROUND_GRID_IOU = 0.75
SCOUT_MIN_FOREGROUND_COUNT_RATIO = 0.50
# Hero Grid uses the live renderer's fixed table geometry and semantic circle
# colors, while the Rust side uses a native bitmap font and raster primitives.
# Keep the gate broad enough for those intentional backend differences while
# rejecting missing rows, columns, or circle clusters.
HERO_GRID_MAX_MAE = 0.120
HERO_GRID_MAX_RMS = 0.250
HERO_GRID_MIN_FOREGROUND_GRID_IOU = 0.75
HERO_GRID_MIN_FOREGROUND_COUNT_RATIO = 0.50
# The pet path shares checked-in RGBA components, so it gets a tighter pixel
# gate than the intentionally native-only chart/GIF ports.  The foreground
# checks still guard against a blank or badly registered component composite.
PET_MAX_MAE = 0.050
PET_MAX_RMS = 0.120
PET_MIN_FOREGROUND_GRID_IOU = 0.85
PET_MIN_FOREGROUND_COUNT_RATIO = 0.75
PET_MAX_FOREGROUND_COUNT_RATIO = 1.35
# Profile charts share the live Python Pillow helpers and native Rust raster
# geometry. Keep the semantic palette/row geometry strict while allowing the
# intentionally different fonts and antialiasing backends.
PROFILE_ROLE_MAX_MAE = 0.120
PROFILE_ROLE_MAX_RMS = 0.260
PROFILE_ROLE_MIN_FOREGROUND_GRID_IOU = 0.70
PROFILE_ROLE_MIN_FOREGROUND_COUNT_RATIO = 0.35
PROFILE_LANE_MAX_MAE = 0.090
PROFILE_LANE_MAX_RMS = 0.200
PROFILE_LANE_MIN_FOREGROUND_GRID_IOU = 0.70
PROFILE_LANE_MIN_FOREGROUND_COUNT_RATIO = 0.55
PROFILE_HERO_MAX_MAE = 0.090
PROFILE_HERO_MAX_RMS = 0.200
PROFILE_HERO_MIN_FOREGROUND_GRID_IOU = 0.70
PROFILE_HERO_MIN_FOREGROUND_COUNT_RATIO = 0.55
PROFILE_RECENT_MAX_MAE = 0.090
PROFILE_RECENT_MAX_RMS = 0.200
PROFILE_RECENT_MIN_FOREGROUND_GRID_IOU = 0.80
PROFILE_RECENT_MIN_FOREGROUND_COUNT_RATIO = 0.55
# The live `/profile` Gambling chart has a deliberately native bitmap-font
# backend on the Rust side. Keep the whole-frame gate broad enough for that
# font difference, while the independent semantic masks below keep every
# authored chart layer present and in the same coarse region.
PROFILE_GAMBA_MAX_MAE = 0.110
PROFILE_GAMBA_MAX_RMS = 0.240
PROFILE_GAMBA_MIN_FOREGROUND_GRID_IOU = 0.72
PROFILE_GAMBA_MIN_FOREGROUND_COUNT_RATIO = 0.50
PROFILE_GAMBA_MIN_LAYER_COUNT_RATIO = 0.35
PROFILE_GAMBA_MIN_LAYER_GRID_IOU = 0.45
PROFILE_GAMBA_COLOR_DISTANCE = 72
PROFILE_GAMBA_MIN_MARKER_ROLE_RATIO = 0.18
PROFILE_GAMBA_MIN_MARKER_SHAPE_IOU = 0.60
PROFILE_GAMBA_MARKER_COLOR_DISTANCE = 55
PROFILE_GAMBA_MARKER_DARK_DISTANCE = 18
PROFILE_GAMBA_MARKER_WHITE_DISTANCE = 40
# `/wrapped` renders the same authored 700x400 Gamba chart into a separate
# 800x600 story canvas. The native bitmap font and PNG compositing differ from
# Pillow, but the wrapper geometry, chart layers, and footer remain guarded.
WRAPPED_GAMBA_MAX_MAE = 0.105
WRAPPED_GAMBA_MAX_RMS = 0.235
WRAPPED_GAMBA_MIN_FOREGROUND_GRID_IOU = 0.78
WRAPPED_GAMBA_MIN_FOREGROUND_COUNT_RATIO = 0.50
# Native copy is intentionally bitmap-backed. Whole-frame error is too
# background-heavy to catch a middle dot falling through to `?` or authored
# lowercase being uppercased, so the Rust boundary also verifies these glyph
# cells directly. This remains scoped to the Wrapped/Gamba visual gate.
_NATIVE_CASE_GLYPHS: dict[str, tuple[int, ...]] = {
    "a": (0, 0, 14, 1, 15, 17, 15),
    "b": (16, 16, 30, 17, 17, 17, 30),
    "c": (0, 0, 14, 17, 16, 17, 14),
    "d": (1, 1, 15, 17, 17, 17, 15),
    "e": (0, 0, 14, 17, 31, 16, 14),
    "f": (6, 9, 8, 28, 8, 8, 8),
    "g": (0, 0, 15, 17, 15, 1, 14),
    "h": (16, 16, 30, 17, 17, 17, 17),
    "i": (4, 0, 12, 4, 4, 4, 14),
    "j": (2, 0, 6, 2, 2, 18, 12),
    "k": (16, 16, 18, 20, 24, 20, 18),
    "l": (12, 4, 4, 4, 4, 4, 14),
    "m": (0, 0, 26, 21, 21, 21, 21),
    "n": (0, 0, 30, 17, 17, 17, 17),
    "o": (0, 0, 14, 17, 17, 17, 14),
    "p": (0, 0, 30, 17, 30, 16, 16),
    "q": (0, 0, 15, 17, 15, 1, 1),
    "r": (0, 0, 22, 25, 16, 16, 16),
    "s": (0, 0, 15, 16, 14, 1, 30),
    "t": (8, 8, 28, 8, 8, 9, 6),
    "u": (0, 0, 17, 17, 17, 19, 13),
    "v": (0, 0, 17, 17, 17, 10, 4),
    "w": (0, 0, 17, 17, 21, 21, 10),
    "x": (0, 0, 17, 10, 4, 10, 17),
    "y": (0, 0, 17, 17, 15, 1, 14),
    "z": (0, 0, 31, 2, 4, 8, 31),
}
_NATIVE_MIDDLE_DOT_GLYPH = (0, 0, 0, 4, 0, 0, 0)
_NATIVE_WRAPPED_GREY = (170, 174, 190, 255)
_NATIVE_GAMBA_GREY = (185, 187, 190, 255)
_NATIVE_GAMBA_WHITE = (255, 255, 255, 255)


class FixedDateTime(dt.datetime):
    """datetime replacement used to make the Python chart's ``now`` fixed."""

    fixed_timestamp: int = 0

    @classmethod
    def now(cls, tz: dt.tzinfo | None = None) -> FixedDateTime:
        return cls.fromtimestamp(cls.fixed_timestamp, tz=tz)


@contextmanager
def fixed_python_clock(timestamp: int) -> Iterator[None]:
    # predictions.py intentionally takes the worker clock from datetime.now.
    # Patch only the module-local datetime class; no source or global clock is
    # changed, and the patch is restored before the runner exits.
    from unittest.mock import patch

    FixedDateTime.fixed_timestamp = timestamp
    from utils.drawing import predictions

    with (
        patch.object(predictions._dt, "datetime", FixedDateTime),
        patch.object(predictions._dt, "UTC", dt.UTC, create=True),
    ):
        yield


def load_fixture(path: Path) -> dict[str, Any]:
    fixture = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(fixture, dict) or set(fixture) != {
        "chart",
        "animation",
        "terminal_crash",
        "dig_neon",
        "pinnacle",
        "balance",
        "gamba",
        "wrapped_gamba",
        "rating_history",
        "rating_distribution",
        "rating_analysis",
        "advantage",
        "pet",
        "wheel",
        "explosion",
        "blame_luke",
        "scout",
        "hero_grid",
        "profile",
    }:
        raise ValueError(
            "fixture must contain exactly chart, animation, terminal_crash, dig_neon, pinnacle, balance, gamba, wrapped_gamba, rating_history, rating_distribution, rating_analysis, advantage, pet, wheel, explosion, blame_luke, scout, hero_grid, and profile objects"
        )
    return fixture


def render_python(
    fixture: dict[str, Any],
    output_dir: Path,
    fixture_path: Path | None = None,
) -> None:
    from utils import dig_drawing, pet_compositor
    from utils.drawing import analysis as drawing_analysis
    from utils.drawing import (
        draw_advantage_graph,
        draw_balance_chart,
        draw_calibration_curve,
        draw_gamba_chart,
        draw_hero_grid,
        draw_prediction_over_time,
        draw_rating_comparison_chart,
        draw_rating_distribution,
        draw_rating_history_chart,
    )
    from utils.drawing import heroes as drawing_heroes
    from utils.drawing import roles as drawing_roles
    from utils.drawing import tables as drawing_tables
    from utils.drawing.predictions import draw_market_fair_history
    from utils.neon_drawing import create_post_match_gif, create_terminal_crash_gif
    from utils.pet_assets import get_pet_card
    from utils.wheel_drawing import create_explosion_gif, create_wheel_gif

    chart = fixture["chart"]
    snapshots = [tuple(snapshot) for snapshot in chart["snapshots"]]
    with fixed_python_clock(int(chart["now"])):
        chart_bytes = draw_market_fair_history(
            market_id=int(chart["market_id"]),
            snapshots=snapshots,
            created_at=int(chart["created_at"]),
            title=chart.get("title"),
        ).getvalue()
    (output_dir / "python_chart.png").write_bytes(chart_bytes)

    balance = fixture["balance"]
    balance_series = [
        (int(event_number), int(cumulative), {"source": str(source)})
        for event_number, cumulative, source in balance["series"]
    ]
    balance_bytes = draw_balance_chart(
        str(balance["username"]),
        balance_series,
        {str(source): int(total) for source, total in balance["source_totals"].items()},
    ).getvalue()
    (output_dir / "python_balance.png").write_bytes(balance_bytes)

    # This is the live `/profile` Gambling attachment boundary. `/wrapped`
    # intentionally owns a separate chart path and is not included here.
    gamba = fixture["gamba"]
    gamba_bytes = draw_gamba_chart(
        username=str(gamba["username"]),
        degen_score=int(gamba["degen_score"]),
        degen_title=str(gamba["degen_title"]),
        degen_emoji=str(gamba.get("degen_emoji", "")),
        pnl_series=[
            (
                int(point["event_number"]),
                int(point["cumulative"]),
                {
                    "source": str(point["source"]),
                    "outcome": str(point["outcome"]),
                    "leverage": int(point["leverage"]),
                    "profit": int(point["profit"]),
                },
            )
            for point in gamba["series"]
        ],
        stats={
            "total_bets": int(gamba["stats"]["total_bets"]),
            "win_rate": float(gamba["stats"]["win_rate"]),
            "net_pnl": int(gamba["stats"]["net_pnl"]),
            "roi": float(gamba["stats"]["roi"]),
        },
    ).getvalue()
    (output_dir / "python_profile_gamba.png").write_bytes(gamba_bytes)

    # This is the live `/wrapped` Gamba story boundary. It deliberately
    # reuses the typed chart payload but then applies Python's separate
    # wrap_chart_in_slide canvas and attachment lifecycle.
    wrapped_gamba = fixture["wrapped_gamba"]
    wrapped_payload = wrapped_gamba["gamba"]
    wrapped_chart = draw_gamba_chart(
        username=str(wrapped_payload["username"]),
        degen_score=int(wrapped_payload["degen_score"]),
        degen_title=str(wrapped_payload["degen_title"]),
        degen_emoji=str(wrapped_payload.get("degen_emoji", "")),
        pnl_series=[
            (
                int(point["event_number"]),
                int(point["cumulative"]),
                {
                    "source": str(point["source"]),
                    "outcome": str(point["outcome"]),
                    "leverage": int(point["leverage"]),
                    "profit": int(point["profit"]),
                },
            )
            for point in wrapped_payload["series"]
        ],
        stats={
            "total_bets": int(wrapped_payload["stats"]["total_bets"]),
            "win_rate": float(wrapped_payload["stats"]["win_rate"]),
            "net_pnl": int(wrapped_payload["stats"]["net_pnl"]),
            "roi": float(wrapped_payload["stats"]["roi"]),
        },
    ).getvalue()
    from utils.wrapped_drawing import wrap_chart_in_slide

    wrapped_bytes = wrap_chart_in_slide(
        wrapped_chart,
        str(wrapped_gamba["title"]),
        str(wrapped_gamba["footer"]),
    ).getvalue()
    (output_dir / "python_wrapped_gamba.png").write_bytes(wrapped_bytes)

    rating_history = fixture["rating_history"]
    rating_history_bytes = draw_rating_history_chart(
        str(rating_history["username"]),
        list(rating_history["entries"]),
    ).getvalue()
    (output_dir / "python_rating_history.png").write_bytes(rating_history_bytes)

    # This is the live Python `/calibration` attachment boundary. The service
    # computes the median once and passes it explicitly to the production
    # helper, just as the Rust provider does below.
    rating_distribution = fixture["rating_distribution"]
    rating_distribution_bytes = draw_rating_distribution(
        [float(rating) for rating in rating_distribution["ratings"]],
        median_rating=float(rating_distribution["median_rating"]),
    ).getvalue()
    (output_dir / "python_rating_distribution.png").write_bytes(
        rating_distribution_bytes
    )
    # This is the live `/ratinganalysis compare` attachment boundary: the
    # command receives the comparison service payload and passes it directly
    # to the production drawing helper.  The Rust example invokes the same
    # native renderer that RatingAnalysisDrawingPort wires into the runtime
    # provider.
    rating_analysis = fixture["rating_analysis"]
    rating_analysis_bytes = draw_rating_comparison_chart(
        dict(rating_analysis["comparison"])
    ).getvalue()
    (output_dir / "python_rating_analysis_comparison.png").write_bytes(
        rating_analysis_bytes
    )

    calibration = rating_analysis["calibration"]
    calibration_bytes = draw_calibration_curve(
        [tuple(point) for point in calibration["glicko"]],
        [tuple(point) for point in calibration["openskill"]],
    ).getvalue()
    (output_dir / "python_rating_analysis_calibration.png").write_bytes(
        calibration_bytes
    )

    trend = rating_analysis["trend"]
    trend_bytes = draw_prediction_over_time(
        [dict(match) for match in trend["match_data"]],
        window=int(trend["window"]),
    ).getvalue()
    (output_dir / "python_rating_analysis_trend.png").write_bytes(trend_bytes)

    advantage = fixture["advantage"]
    advantage_bytes = draw_advantage_graph(
        {
            "radiant_gold_adv": list(advantage["radiant_gold_adv"]),
            "radiant_xp_adv": list(advantage["radiant_xp_adv"]),
        },
        int(advantage["match_id"]),
    )
    if advantage_bytes is None:
        raise AssertionError("advantage fixture unexpectedly rendered no image")
    (output_dir / "python_advantage.png").write_bytes(advantage_bytes.getvalue())

    # This is the live Python attachment boundary: get_pet_card selects the
    # full-card override, then pet_compositor's authored-layer hybrid, then
    # render_pet_card's native fallback.  The fixture deliberately uses the
    # checked-in component pack and a seed whose shared variant choices match
    # the Rust HybridPetRenderer selection.
    pet = fixture["pet"]
    expected_components = (
        (fixture_path or DEFAULT_FIXTURE).resolve().parent / str(pet["components_path"])
    ).resolve()
    if pet_compositor.COMPONENTS_DIR.resolve() != expected_components:
        raise AssertionError(
            "pet fixture components path does not match Python pet_compositor: "
            f"{pet_compositor.COMPONENTS_DIR} != {expected_components}"
        )
    pet_file = get_pet_card(
        str(pet["species_id"]),
        str(pet["stage"]),
        str(pet["mood"]),
        int(pet["seed"]),
        accessory=pet.get("accessory"),
    )
    if pet_file is None:
        raise AssertionError("pet fixture unexpectedly rendered no attachment")
    if pet_file.filename != str(pet["attachment_filename"]):
        raise AssertionError(
            f"unexpected pet attachment name: {pet_file.filename!r} "
            f"(expected {pet['attachment_filename']!r})"
        )
    if str(pet["embed_image"]) != f"attachment://{pet_file.filename}":
        raise AssertionError(f"unexpected pet attachment URL: {pet['embed_image']!r}")
    (output_dir / "python_pet.png").write_bytes(pet_file.fp.read())

    # The production wheel renderer chooses an intentionally varied ending
    # style.  Pin the existing Python RNG for this explicit cross-language
    # gate; the live command still uses its normal process RNG.
    wheel = fixture["wheel"]
    random_state = random.getstate()
    try:
        random.seed(int(wheel["seed"]))
        wheel_bytes = create_wheel_gif(
            target_idx=int(wheel["target_index"]),
            size=int(wheel["size"]),
            display_name=wheel.get("display_name"),
            is_bankrupt=bool(wheel["is_bankrupt"]),
            is_golden=bool(wheel["is_golden"]),
        ).getvalue()
    finally:
        random.setstate(random_state)
    (output_dir / "python_wheel.gif").write_bytes(wheel_bytes)

    # Explosion particles and smoke also use the module RNG.  Keep the
    # production implementation unchanged while making this recording
    # reproducible for the Rust comparison.
    explosion = fixture["explosion"]
    random_state = random.getstate()
    try:
        random.seed(int(explosion["seed"]))
        explosion_bytes = create_explosion_gif(size=int(explosion["size"])).getvalue()
    finally:
        random.setstate(random_state)
    (output_dir / "python_explosion.gif").write_bytes(explosion_bytes)

    blame_luke = fixture["blame_luke"]
    from utils.blame_luke_drawing import BLAME_LUKE_REASONS, create_blame_luke_gif

    selected_index = int(blame_luke["selected_index"])
    if not 0 <= selected_index < len(BLAME_LUKE_REASONS):
        raise ValueError(f"blame_luke.selected_index is out of range: {selected_index}")
    blame_luke_bytes = create_blame_luke_gif(BLAME_LUKE_REASONS[selected_index]).getvalue()
    (output_dir / "python_blame_luke.gif").write_bytes(blame_luke_bytes)

    # The live Python command calls draw_scout_report after loading portraits
    # from its cache/CDN helper.  Keep this cross-language fixture hermetic by
    # exercising the production cache-miss fallback branch on both sides; the
    # normal provider still uses its unchanged cache/network behavior.
    scout = fixture["scout"]
    if scout.get("portrait_mode") != "cache_miss_fallback":
        raise ValueError("scout fixture must explicitly select cache_miss_fallback")
    scout_data = {
        "player_count": int(scout["player_count"]),
        "total_matches": int(scout["total_matches"]),
        "heroes": [dict(hero) for hero in scout["heroes"]],
    }
    from unittest.mock import patch

    with patch.object(drawing_analysis, "_get_hero_images_batch", return_value={}):
        scout_bytes = drawing_analysis.draw_scout_report(
            scout_data=scout_data,
            player_names=[str(name) for name in scout["player_names"]],
            title=str(scout["title"]),
        ).getvalue()
    (output_dir / "python_scout.png").write_bytes(scout_bytes)

    # This is the live Python `/herogrid` renderer boundary: the command
    # resolves its source and repository data before calling draw_hero_grid.
    # Keep the cross-language recording at that production renderer boundary
    # with deterministic, typed fixture rows and insertion-ordered players.
    hero_grid = fixture["hero_grid"]
    hero_grid_bytes = draw_hero_grid(
        [dict(stat) for stat in hero_grid["stats"]],
        {
            int(player["discord_id"]): str(player["name"])
            for player in hero_grid["players"]
        },
        min_games=int(hero_grid["min_games"]),
        title=str(hero_grid["title"]),
    ).getvalue()
    (output_dir / "python_hero_grid.png").write_bytes(hero_grid_bytes)

    # These are the live profile/media attachment boundaries. The profile
    # command supplies role/lane distributions and hero aggregates directly to
    # the drawing helpers; `/matches recent` supplies the ordered match rows to
    # the same table renderer. Keep the fixture at that typed payload edge so
    # neither side can pass by copying the other runtime's pixels.
    profile = fixture["profile"]
    role_bytes = drawing_roles.draw_role_graph(
        {str(name): float(value) for name, value in profile["roles"].items()},
        title=f"Roles: {profile['username']}",
    ).getvalue()
    (output_dir / "python_profile_role_graph.png").write_bytes(role_bytes)
    lane_bytes = drawing_roles.draw_lane_distribution(
        {str(row["name"]): float(row["value"]) for row in profile["lanes"]}
    ).getvalue()
    (output_dir / "python_profile_lane_distribution.png").write_bytes(lane_bytes)
    hero_bytes = drawing_heroes.draw_hero_performance_chart(
        [dict(row) for row in profile["hero_performance"]],
        str(profile["username"]),
    ).getvalue()
    (output_dir / "python_profile_hero_performance.png").write_bytes(hero_bytes)
    recent_bytes = drawing_tables.draw_matches_table(
        [dict(row) for row in profile["recent_matches"]],
    ).getvalue()
    (output_dir / "python_profile_recent_matches.png").write_bytes(recent_bytes)

    # The production Python animation uses random glitch displacement.  A
    # fixture seed makes that existing behavior reproducible for comparison;
    # it does not alter the Rust renderer or the live provider path.
    animation = fixture["animation"]
    random_state = random.getstate()
    try:
        random.seed(0xCA7A47)
        animation_bytes = create_post_match_gif(
            str(animation["name"]),
            int(animation["value"]),
            theme=str(animation["theme"]),
        ).getvalue()
    finally:
        random.setstate(random_state)
    (output_dir / "python_animation.gif").write_bytes(animation_bytes)

    # The production terminal-crash renderer consumes random glitch/noise
    # rolls.  Keep the existing runtime behavior while pinning the fixture's
    # seed so Python output is reproducible for cross-language comparison.
    terminal_crash = fixture["terminal_crash"]
    random_state = random.getstate()
    try:
        random.seed(0xC0FFEE)
        terminal_crash_bytes = create_terminal_crash_gif(
            str(terminal_crash["name"]),
            int(terminal_crash["filing_number"]),
        ).getvalue()
    finally:
        random.setstate(random_state)
    (output_dir / "python_terminal_crash.gif").write_bytes(terminal_crash_bytes)

    # The live Dig prestige and depth-350 terminal hooks both call the same
    # authored Python renderer with a boolean mode. Keep both variants at the
    # typed hook boundary so the Rust provider cannot pass with only one mode.
    dig_neon = fixture["dig_neon"]
    for mode in ("terminal", "prestige"):
        if not isinstance(dig_neon[mode]["prestige"], bool):
            raise ValueError(f"dig_neon.{mode}.prestige must be boolean")
        dig_bytes = dig_drawing.animate_pinnacle(
            prestige=bool(dig_neon[mode]["prestige"])
        ).getvalue()
        (output_dir / f"python_dig_{mode}.gif").write_bytes(dig_bytes)

    pinnacle = fixture["pinnacle"]
    source_root = (fixture_path or DEFAULT_FIXTURE).resolve().parent
    source_path = (source_root / str(pinnacle["source_path"])).resolve()
    if not source_path.is_file():
        raise FileNotFoundError(f"pinnacle source image does not exist: {source_path}")
    pinnacle_bytes = dig_drawing.animate_pinnacle_phase3(
        source_path.read_bytes(),
        str(pinnacle["boss_id"]),
        secret=bool(pinnacle["secret"]),
    ).getvalue()
    (output_dir / "python_pinnacle_phase3.gif").write_bytes(pinnacle_bytes)


def run_rust(fixture_path: Path, output_dir: Path, target_dir: Path | None) -> None:
    import os

    environment = os.environ.copy()
    if target_dir is not None:
        environment["CARGO_TARGET_DIR"] = str(target_dir)

    python_font_dir = (
        ROOT
        / ".venv"
        / "lib"
        / "python3.12"
        / "site-packages"
        / "matplotlib"
        / "mpl-data"
        / "fonts"
        / "ttf"
    )
    if python_font_dir.is_dir():
        environment["CAMA_FONT_DIR"] = str(python_font_dir)

    command = [
        "cargo",
        "run",
        "--locked",
        "--manifest-path",
        str(ROOT / "rust" / "Cargo.toml"),
        "-p",
        "cama-app",
        "--example",
        "visual_equivalence",
        "--",
        str(fixture_path),
        str(output_dir),
    ]
    subprocess.run(command, cwd=ROOT, env=environment, check=True)

    # Betting media lives in cama-runtime because the provider owns the
    # Discord attachment boundary.  This second target calls its exact
    # production renderers; the app example above continues to own the other
    # renderer families in this fixture.
    betting_command = [
        "cargo",
        "run",
        "--locked",
        "--manifest-path",
        str(ROOT / "rust" / "Cargo.toml"),
        "-p",
        "cama-runtime",
        "--example",
        "visual_equivalence",
        "--",
        str(fixture_path),
        str(output_dir),
    ]
    subprocess.run(betting_command, cwd=ROOT, env=environment, check=True)


def rgba_pixels(path: Path) -> tuple[tuple[int, int], bytes]:
    with Image.open(path) as image:
        rgba = image.convert("RGBA")
        return rgba.size, rgba.tobytes()


def gif_frames(path: Path) -> tuple[tuple[int, int], int | None, list[int], list[bytes]]:
    with Image.open(path) as image:
        size = image.size
        loop = image.info.get("loop")
        durations: list[int] = []
        frames: list[bytes] = []
        for frame in ImageSequence.Iterator(image):
            durations.append(int(frame.info.get("duration", image.info.get("duration", 0))))
            frames.append(frame.convert("RGBA").tobytes())
        return size, loop, durations, frames


def pixel_metrics(left: bytes, right: bytes) -> tuple[float, float, float]:
    if len(left) != len(right):
        raise ValueError(f"pixel buffers differ in length: {len(left)} != {len(right)}")
    if not left:
        return 0.0, 0.0, 1.0
    absolute = 0
    squared = 0
    equal = 0
    for first, second in zip(left, right):
        difference = abs(first - second)
        absolute += difference
        squared += difference * difference
        equal += first == second
    scale = 255.0 * len(left)
    mae = absolute / scale
    rms = (squared / len(left)) ** 0.5 / 255.0
    exact = equal / len(left)
    return mae, rms, exact


def foreground_structure(
    rgba: bytes,
    size: tuple[int, int],
    grid: tuple[int, int],
    margin: int,
) -> tuple[int, set[tuple[int, int]]]:
    """Return bright interior pixels and occupied coarse cells.

    Both representative renderers use a dark background.  Looking only at
    whole-frame error lets that shared background dominate the score, so this
    signature separately records where meaningful neon/text/chart pixels
    occur.  The fixed threshold is above both fixtures' background colors.
    """

    width, height = size
    columns, rows = grid
    expected = width * height * 4
    if len(rgba) != expected:
        raise ValueError(f"RGBA buffer has {len(rgba)} bytes, expected {expected}")
    if columns <= 0 or rows <= 0 or margin < 0 or margin * 2 >= min(size):
        raise ValueError("invalid foreground grid or margin")

    interior_count = 0
    occupied: set[tuple[int, int]] = set()
    for pixel_index in range(width * height):
        offset = pixel_index * 4
        if max(rgba[offset : offset + 3]) <= FOREGROUND_CHANNEL_THRESHOLD:
            continue
        x = pixel_index % width
        y = pixel_index // width
        occupied.add((x * columns // width, y * rows // height))
        if margin <= x < width - margin and margin <= y < height - margin:
            interior_count += 1
    return interior_count, occupied


def compare_foreground_structure(
    reference: bytes,
    candidate: bytes,
    size: tuple[int, int],
    *,
    grid: tuple[int, int],
    margin: int,
    minimum_grid_iou: float,
    label: str,
    minimum_count_ratio: float = MIN_FOREGROUND_COUNT_RATIO,
) -> tuple[float, float]:
    """Reject blank/border-only output hidden by a shared dark background."""

    reference_count, reference_cells = foreground_structure(reference, size, grid, margin)
    candidate_count, candidate_cells = foreground_structure(candidate, size, grid, margin)
    if reference_count == 0 or not reference_cells:
        raise AssertionError(f"{label} reference contains no foreground structure")
    minimum_count = max(MIN_FOREGROUND_PIXELS, reference_count * minimum_count_ratio)
    if candidate_count < minimum_count:
        raise AssertionError(
            f"{label} foreground is missing: {candidate_count} pixels, "
            f"expected at least {minimum_count:.0f}"
        )
    union = reference_cells | candidate_cells
    grid_iou = len(reference_cells & candidate_cells) / len(union) if union else 1.0
    if grid_iou < minimum_grid_iou:
        raise AssertionError(
            f"{label} foreground layout drifted: grid IoU {grid_iou:.3f} < {minimum_grid_iou:.3f}"
        )
    return candidate_count / reference_count, grid_iou


def semantic_series_structure(
    rgba: bytes,
    size: tuple[int, int],
    grid: tuple[int, int],
    distance: int = 120,
) -> tuple[int, set[tuple[int, int]]]:
    """Return coarse placement of the accent/green chart series.

    The Python Matplotlib charts alpha-blend and antialias their lines and
    markers; the native renderer uses opaque palette pixels.  A bounded
    distance from either series color captures both implementations while
    ignoring shared dark backgrounds, axes, and labels.
    """

    width, height = size
    columns, rows = grid
    colors = ((88, 101, 242), (87, 242, 135))
    interior_count = 0
    occupied: set[tuple[int, int]] = set()
    for pixel_index in range(width * height):
        offset = pixel_index * 4
        pixel = rgba[offset : offset + 3]
        if min(
            sum(abs(channel - expected) for channel, expected in zip(pixel, color))
            for color in colors
        ) > distance:
            continue
        x = pixel_index % width
        y = pixel_index // width
        occupied.add((x * columns // width, y * rows // height))
        interior_count += 1
    return interior_count, occupied


def compare_semantic_series_structure(
    reference: bytes,
    candidate: bytes,
    size: tuple[int, int],
    *,
    grid: tuple[int, int],
    minimum_grid_iou: float,
    minimum_count_ratio: float,
    label: str,
) -> tuple[float, float]:
    """Reject a chart whose colored series are missing or misplaced."""

    reference_count, reference_cells = semantic_series_structure(reference, size, grid)
    candidate_count, candidate_cells = semantic_series_structure(candidate, size, grid)
    if reference_count == 0 or not reference_cells:
        raise AssertionError(f"{label} reference contains no semantic series")
    minimum_count = max(50, reference_count * minimum_count_ratio)
    if candidate_count < minimum_count:
        raise AssertionError(
            f"{label} semantic series is missing: {candidate_count} pixels, "
            f"expected at least {minimum_count:.0f}"
        )
    union = reference_cells | candidate_cells
    grid_iou = len(reference_cells & candidate_cells) / len(union) if union else 1.0
    if grid_iou < minimum_grid_iou:
        raise AssertionError(
            f"{label} semantic series layout drifted: grid IoU {grid_iou:.3f} "
            f"< {minimum_grid_iou:.3f}"
        )
    return candidate_count / reference_count, grid_iou


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()[:16]


def check_png(
    python_path: Path, rust_path: Path, *, label: str = "chart"
) -> list[str]:
    python_size, python_pixels = rgba_pixels(python_path)
    rust_size, rust_pixels = rgba_pixels(rust_path)
    if python_size != rust_size:
        raise AssertionError(
            f"{label} dimensions differ: Python {python_size}, Rust {rust_size}"
        )
    mae, rms, exact = pixel_metrics(python_pixels, rust_pixels)
    foreground_ratio, foreground_iou = compare_foreground_structure(
        python_pixels,
        rust_pixels,
        python_size,
        grid=(max(1, python_size[0] // 20), max(1, python_size[1] // 20)),
        margin=0,
        minimum_grid_iou=CHART_MIN_FOREGROUND_GRID_IOU,
        label=label,
    )
    print(
        f"{label}: size={python_size[0]}x{python_size[1]} "
        f"MAE={mae:.5f} RMS={rms:.5f} exact_channels={exact:.3%} "
        f"foreground_ratio={foreground_ratio:.3f} grid_IoU={foreground_iou:.3f} "
        f"python_sha={sha256(python_pixels)} rust_sha={sha256(rust_pixels)}"
    )
    if mae > CHART_MAX_MAE or rms > CHART_MAX_RMS:
        raise AssertionError(
            f"{label} pixel drift exceeds threshold: MAE {mae:.5f} <= {CHART_MAX_MAE:.5f}, "
            f"RMS {rms:.5f} <= {CHART_MAX_RMS:.5f}"
        )
    return []


def _rating_distribution_color_structure(
    rgba: bytes,
    size: tuple[int, int],
    colors: tuple[tuple[int, int, int], ...],
    *,
    grid: tuple[int, int] = (16, 10),
) -> tuple[int, set[tuple[int, int]]]:
    """Locate one authored rating-distribution layer by semantic color."""

    width, height = size
    columns, rows = grid
    expected = width * height * 4
    if len(rgba) != expected:
        raise ValueError(f"RGBA buffer has {len(rgba)} bytes, expected {expected}")
    count = 0
    occupied: set[tuple[int, int]] = set()
    for pixel_index in range(width * height):
        offset = pixel_index * 4
        pixel = rgba[offset : offset + 3]
        if min(
            sum(
                abs(channel - expected_channel)
                for channel, expected_channel in zip(pixel, color)
            )
            for color in colors
        ) > RATING_DISTRIBUTION_COLOR_DISTANCE:
            continue
        x = pixel_index % width
        y = pixel_index // width
        count += 1
        occupied.add((x * columns // width, y * rows // height))
    return count, occupied


def check_rating_distribution(python_path: Path, rust_path: Path) -> list[str]:
    """Compare the live server `/calibration` rating-distribution attachment."""

    python_size, python_pixels = rgba_pixels(python_path)
    rust_size, rust_pixels = rgba_pixels(rust_path)
    expected_size = (640, 390)
    if python_size != expected_size or rust_size != python_size:
        raise AssertionError(
            "rating distribution dimensions differ or are invalid: "
            f"Python {python_size}, Rust {rust_size}; expected {expected_size}"
        )

    mae, rms, exact = pixel_metrics(python_pixels, rust_pixels)
    reference_cells: set[tuple[int, int]] = set()
    candidate_cells: set[tuple[int, int]] = set()
    reference_counts: dict[str, int] = {}
    candidate_counts: dict[str, int] = {}
    color_ratios: dict[str, float] = {}
    color_grid_ious: dict[str, float] = {}
    for name, colors in RATING_DISTRIBUTION_COLOR_VARIANTS.items():
        reference_count, reference_color_cells = _rating_distribution_color_structure(
            python_pixels, python_size, colors
        )
        candidate_count, candidate_color_cells = _rating_distribution_color_structure(
            rust_pixels, rust_size, colors
        )
        if reference_count < 12 or not reference_color_cells:
            raise AssertionError(
                f"rating distribution Python reference lost {name}: "
                f"{reference_count} semantic pixels"
            )
        minimum_count = max(
            12,
            reference_count * RATING_DISTRIBUTION_MIN_COLOR_COUNT_RATIO,
        )
        if candidate_count < minimum_count:
            raise AssertionError(
                f"rating distribution {name} is missing: {candidate_count} pixels, "
                f"expected at least {minimum_count:.0f}"
            )
        reference_counts[name] = reference_count
        candidate_counts[name] = candidate_count
        color_ratios[name] = candidate_count / reference_count
        color_union = reference_color_cells | candidate_color_cells
        color_grid_iou = (
            len(reference_color_cells & candidate_color_cells) / len(color_union)
            if color_union
            else 1.0
        )
        if color_grid_iou < RATING_DISTRIBUTION_MIN_COLOR_GRID_IOU:
            raise AssertionError(
                f"rating distribution {name} layout drifted: grid IoU "
                f"{color_grid_iou:.3f} < "
                f"{RATING_DISTRIBUTION_MIN_COLOR_GRID_IOU:.3f}"
            )
        color_grid_ious[name] = color_grid_iou
        reference_cells.update(reference_color_cells)
        candidate_cells.update(candidate_color_cells)

    semantic_union = reference_cells | candidate_cells
    semantic_iou = (
        len(reference_cells & candidate_cells) / len(semantic_union)
        if semantic_union
        else 1.0
    )
    if semantic_iou < RATING_DISTRIBUTION_MIN_SEMANTIC_GRID_IOU:
        raise AssertionError(
            "rating distribution semantic layout drifted: "
            f"grid IoU {semantic_iou:.3f} < "
            f"{RATING_DISTRIBUTION_MIN_SEMANTIC_GRID_IOU:.3f}"
        )

    print(
        f"rating_distribution: size={python_size[0]}x{python_size[1]} "
        f"MAE={mae:.5f} RMS={rms:.5f} exact_channels={exact:.3%} "
        f"semantic_grid_IoU={semantic_iou:.3f} color_ratios={color_ratios} "
        f"color_grid_IoUs={color_grid_ious} "
        f"python_colors={reference_counts} rust_colors={candidate_counts} "
        f"python_sha={sha256(python_pixels)} rust_sha={sha256(rust_pixels)}"
    )
    if mae > RATING_DISTRIBUTION_MAX_MAE or rms > RATING_DISTRIBUTION_MAX_RMS:
        raise AssertionError(
            "rating distribution pixel drift exceeds threshold: "
            f"MAE {mae:.5f} <= {RATING_DISTRIBUTION_MAX_MAE:.5f}, "
            f"RMS {rms:.5f} <= {RATING_DISTRIBUTION_MAX_RMS:.5f}"
        )
    return []


def check_rating_analysis_comparison(
    python_path: Path, rust_path: Path
) -> list[str]:
    """Compare the live `/ratinganalysis compare` chart attachment.

    Matplotlib's tight bounding box gives this production chart a stable
    989x413 RGBA canvas for the checked-in fixture, while Rust intentionally
    uses a native bitmap font and raster primitives.  The exact canvas and
    semantic palette are part of the attachment contract; the bounded pixel
    and coarse foreground gates cover the intentional backend differences.
    """

    python_size, python_pixels = rgba_pixels(python_path)
    rust_size, rust_pixels = rgba_pixels(rust_path)
    expected_size = (989, 413)
    if python_size != expected_size or rust_size != python_size:
        raise AssertionError(
            "rating-analysis comparison dimensions differ or are invalid: "
            f"Python {python_size}, Rust {rust_size}; expected {expected_size}"
        )
    mae, rms, exact = pixel_metrics(python_pixels, rust_pixels)
    foreground_ratio, foreground_iou = compare_foreground_structure(
        python_pixels,
        rust_pixels,
        python_size,
        grid=(20, 10),
        margin=0,
        minimum_grid_iou=RATING_ANALYSIS_MIN_FOREGROUND_GRID_IOU,
        minimum_count_ratio=RATING_ANALYSIS_MIN_FOREGROUND_COUNT_RATIO,
        label="rating-analysis comparison",
    )

    def color_count(pixels: bytes, color: tuple[int, int, int, int]) -> int:
        return sum(
            pixels[offset : offset + 4] == bytes(color)
            for offset in range(0, len(pixels), 4)
        )

    semantic_counts = {
        "accent": color_count(rust_pixels, (88, 101, 242, 255)),
        "green": color_count(rust_pixels, (87, 242, 135, 255)),
        "winner": color_count(rust_pixels, (254, 231, 92, 255)),
    }
    if min(semantic_counts.values()) == 0:
        raise AssertionError(
            "rating-analysis comparison lost a semantic metric color: "
            f"{semantic_counts}"
        )
    print(
        f"rating_analysis_comparison: size={python_size[0]}x{python_size[1]} "
        f"MAE={mae:.5f} RMS={rms:.5f} exact_channels={exact:.3%} "
        f"foreground_ratio={foreground_ratio:.3f} grid_IoU={foreground_iou:.3f} "
        f"semantic_colors={semantic_counts} "
        f"python_sha={sha256(python_pixels)} rust_sha={sha256(rust_pixels)}"
    )
    if mae > RATING_ANALYSIS_MAX_MAE or rms > RATING_ANALYSIS_MAX_RMS:
        raise AssertionError(
            "rating-analysis comparison pixel drift exceeds threshold: "
            f"MAE {mae:.5f} <= {RATING_ANALYSIS_MAX_MAE:.5f}, "
            f"RMS {rms:.5f} <= {RATING_ANALYSIS_MAX_RMS:.5f}"
        )
    return []


def _check_rating_analysis_plot(
    python_path: Path,
    rust_path: Path,
    *,
    label: str,
    expected_size: tuple[int, int],
    minimum_series_grid_iou: float,
    minimum_series_count_ratio: float,
    max_mae: float,
    max_rms: float,
) -> list[str]:
    """Check one live calibration/trend attachment at its PNG boundary."""

    python_size, python_pixels = rgba_pixels(python_path)
    rust_size, rust_pixels = rgba_pixels(rust_path)
    if python_size != expected_size or rust_size != python_size:
        raise AssertionError(
            f"{label} dimensions differ or are invalid: Python {python_size}, "
            f"Rust {rust_size}; expected {expected_size}"
        )
    mae, rms, exact = pixel_metrics(python_pixels, rust_pixels)
    series_ratio, series_iou = compare_semantic_series_structure(
        python_pixels,
        rust_pixels,
        python_size,
        grid=(16, 12) if label.endswith("calibration") else (16, 8),
        minimum_grid_iou=minimum_series_grid_iou,
        minimum_count_ratio=minimum_series_count_ratio,
        label=label,
    )

    def color_count(pixels: bytes, color: tuple[int, int, int, int]) -> int:
        # Matplotlib alpha-blends and antialiases the same semantic series,
        # while the native renderer emits opaque palette pixels.  Use a
        # bounded RGB distance so this remains a palette gate on both sides.
        return sum(
            sum(
                abs(channel - expected)
                for channel, expected in zip(
                    pixels[offset : offset + 3], color[:3]
                )
            )
            <= 120
            for offset in range(0, len(pixels), 4)
        )

    semantic_counts = {
        "accent": color_count(rust_pixels, (88, 101, 242, 255)),
        "green": color_count(rust_pixels, (87, 242, 135, 255)),
    }
    if min(semantic_counts.values()) == 0:
        raise AssertionError(f"{label} lost a semantic series color: {semantic_counts}")
    print(
        f"{label}: size={python_size[0]}x{python_size[1]} "
        f"MAE={mae:.5f} RMS={rms:.5f} exact_channels={exact:.3%} "
        f"series_ratio={series_ratio:.3f} series_grid_IoU={series_iou:.3f} "
        f"semantic_colors={semantic_counts} "
        f"python_sha={sha256(python_pixels)} rust_sha={sha256(rust_pixels)}"
    )
    if mae > max_mae or rms > max_rms:
        raise AssertionError(
            f"{label} pixel drift exceeds threshold: MAE {mae:.5f} <= {max_mae:.5f}, "
            f"RMS {rms:.5f} <= {max_rms:.5f}"
        )
    return []


def check_rating_analysis_calibration(
    python_path: Path, rust_path: Path
) -> list[str]:
    """Compare the live `/ratinganalysis calibration` curve attachment."""

    return _check_rating_analysis_plot(
        python_path,
        rust_path,
        label="rating-analysis calibration",
        expected_size=(640, 490),
        minimum_series_grid_iou=RATING_ANALYSIS_CALIBRATION_MIN_SERIES_GRID_IOU,
        minimum_series_count_ratio=RATING_ANALYSIS_CALIBRATION_MIN_SERIES_COUNT_RATIO,
        max_mae=RATING_ANALYSIS_CALIBRATION_MAX_MAE,
        max_rms=RATING_ANALYSIS_CALIBRATION_MAX_RMS,
    )


def check_rating_analysis_trend(
    python_path: Path, rust_path: Path
) -> list[str]:
    """Compare the live `/ratinganalysis trend` rolling-accuracy attachment."""

    return _check_rating_analysis_plot(
        python_path,
        rust_path,
        label="rating-analysis trend",
        expected_size=(789, 390),
        minimum_series_grid_iou=RATING_ANALYSIS_TREND_MIN_SERIES_GRID_IOU,
        minimum_series_count_ratio=RATING_ANALYSIS_TREND_MIN_SERIES_COUNT_RATIO,
        max_mae=RATING_ANALYSIS_TREND_MAX_MAE,
        max_rms=RATING_ANALYSIS_TREND_MAX_RMS,
    )


def check_gif(python_path: Path, rust_path: Path) -> list[str]:
    python_size, python_loop, python_durations, python_frames = gif_frames(python_path)
    rust_size, rust_loop, rust_durations, rust_frames = gif_frames(rust_path)
    if python_size != rust_size:
        raise AssertionError(f"animation dimensions differ: Python {python_size}, Rust {rust_size}")
    if python_loop != rust_loop:
        raise AssertionError(
            f"animation loop count differs: Python {python_loop}, Rust {rust_loop}"
        )
    if len(python_frames) != len(rust_frames):
        raise AssertionError(
            f"animation frame count differs: Python {len(python_frames)}, Rust {len(rust_frames)}"
        )
    if python_durations != rust_durations:
        raise AssertionError(
            f"animation durations/order differ: Python {python_durations}, Rust {rust_durations}"
        )

    metrics = [pixel_metrics(left, right) for left, right in zip(python_frames, rust_frames)]
    structures = [
        compare_foreground_structure(
            left,
            right,
            python_size,
            grid=(10, 10),
            margin=24,
            minimum_grid_iou=ANIMATION_MIN_FOREGROUND_GRID_IOU,
            label=f"animation frame {index}",
        )
        for index, (left, right) in enumerate(zip(python_frames, rust_frames))
    ]
    mean_mae = sum(metric[0] for metric in metrics) / len(metrics)
    mean_rms = sum(metric[1] for metric in metrics) / len(metrics)
    max_mae = max(metric[0] for metric in metrics)
    minimum_foreground_ratio = min(structure[0] for structure in structures)
    minimum_foreground_iou = min(structure[1] for structure in structures)
    print(
        f"animation: size={python_size[0]}x{python_size[1]} frames={len(python_frames)} "
        f"loop={python_loop} durations={python_durations} "
        f"mean_MAE={mean_mae:.5f} mean_RMS={mean_rms:.5f} max_frame_MAE={max_mae:.5f} "
        f"min_foreground_ratio={minimum_foreground_ratio:.3f} "
        f"min_grid_IoU={minimum_foreground_iou:.3f} "
        f"python_sha={sha256(python_frames[0])} rust_sha={sha256(rust_frames[0])}"
    )
    if (
        mean_mae > ANIMATION_MAX_MEAN_FRAME_MAE
        or mean_rms > ANIMATION_MAX_MEAN_FRAME_RMS
        or max_mae > ANIMATION_MAX_FRAME_MAE
    ):
        raise AssertionError(
            "animation pixel drift exceeds threshold: "
            f"mean MAE {mean_mae:.5f} <= {ANIMATION_MAX_MEAN_FRAME_MAE:.5f}, "
            f"mean RMS {mean_rms:.5f} <= {ANIMATION_MAX_MEAN_FRAME_RMS:.5f}, "
            f"max frame MAE {max_mae:.5f} <= {ANIMATION_MAX_FRAME_MAE:.5f}"
        )
    return []


def check_dig_neon(python_path: Path, rust_path: Path, *, mode: str) -> list[str]:
    """Compare one live Dig prestige/Pinnacle attachment variant.

    Pillow coalesces a small number of identical early logical frames when it
    decodes the Python GIF. The authored Python durations and the Rust output
    are still checked exactly; visual frames are paired by each frame's
    cumulative authored start time (with the terminal hold excluded from the
    active-motion scale). This keeps a coalesced 270ms Python frame aligned
    with the first of the three 90ms Rust phases it represents rather than
    pairing by decoded frame index.
    """

    python_size, python_loop, python_durations, python_frames = gif_frames(python_path)
    rust_size, rust_loop, rust_durations, rust_frames = gif_frames(rust_path)
    expected_authored = [90] * 17 + [130] * 12 + [60_000]
    expected_decoded = {
        "terminal": [270] + [90] * 14 + [130] * 12 + [60_000],
        "prestige": [90, 180] + [90] * 14 + [130] * 12 + [60_000],
    }
    if mode not in expected_decoded:
        raise AssertionError(f"unknown Dig Neon mode: {mode}")
    if python_size != (320, 180) or rust_size != python_size:
        raise AssertionError(
            f"Dig Neon {mode} dimensions differ or are invalid: "
            f"Python {python_size}, Rust {rust_size}"
        )
    if python_loop != 1 or rust_loop != python_loop:
        raise AssertionError(
            f"Dig Neon {mode} loop count differs: Python {python_loop}, Rust {rust_loop}"
        )
    if python_durations != expected_decoded[mode]:
        raise AssertionError(
            f"Python Dig Neon {mode} decoded timing drifted: {python_durations}"
        )
    if rust_durations != expected_authored:
        raise AssertionError(
            f"Rust Dig Neon {mode} authored timing drifted: {rust_durations}"
        )
    if len(rust_frames) != len(expected_authored) or not python_frames:
        raise AssertionError(
            f"Dig Neon {mode} frame count differs: "
            f"Python decoded {len(python_frames)}, Rust authored {len(rust_frames)}"
        )

    pairs = []
    python_active = python_durations[:-1]
    rust_active = rust_durations[:-1]
    python_active_total = sum(python_active)
    rust_active_total = sum(rust_active)
    if python_active_total != rust_active_total:
        raise AssertionError(
            f"Dig Neon {mode} active authored timelines differ: "
            f"Python {python_active_total}ms, Rust {rust_active_total}ms"
        )
    rust_starts: list[int] = []
    elapsed = 0
    for duration in rust_active:
        rust_starts.append(elapsed)
        elapsed += duration
    for python_index, (python_frame, _duration) in enumerate(
        zip(python_frames[:-1], python_active)
    ):
        # Map the Python frame's cumulative start into the equal authored
        # active span, then select the Rust interval containing that point.
        # The Python decoder can only coalesce adjacent duplicate frames; the
        # first Rust phase in that coalesced interval is its authored visual
        # representative (not an arbitrary normalized frame index).
        python_start = sum(python_active[:python_index])
        target = python_start * rust_active_total / python_active_total
        rust_index = len(rust_active) - 1
        for candidate, start in enumerate(rust_starts):
            if target < start + rust_active[candidate]:
                rust_index = candidate
                break
        pairs.append((python_frame, rust_frames[rust_index]))
    # Both encoders retain the final authored frame and its 60s hold.
    pairs.append((python_frames[-1], rust_frames[-1]))
    metrics = [pixel_metrics(left, right) for left, right in pairs]
    structures = [
        compare_foreground_structure(
            left,
            right,
            python_size,
            grid=(10, 10),
            margin=8,
            minimum_grid_iou=(
                DIG_NEON_PRESTIGE_MIN_FOREGROUND_GRID_IOU
                if mode == "prestige"
                else DIG_NEON_TERMINAL_MIN_FOREGROUND_GRID_IOU
            ),
            minimum_count_ratio=DIG_NEON_MIN_FOREGROUND_COUNT_RATIO,
            label=f"Dig Neon {mode} frame {index}",
        )
        for index, (left, right) in enumerate(pairs)
    ]
    mean_mae = sum(metric[0] for metric in metrics) / len(metrics)
    mean_rms = sum(metric[1] for metric in metrics) / len(metrics)
    max_mae = max(metric[0] for metric in metrics)
    minimum_foreground_ratio = min(structure[0] for structure in structures)
    minimum_foreground_iou = min(structure[1] for structure in structures)
    print(
        f"dig_neon_{mode}: size={python_size[0]}x{python_size[1]} "
        f"python_frames={len(python_frames)} rust_frames={len(rust_frames)} "
        f"loop={python_loop} authored_durations={expected_authored} "
        f"mean_MAE={mean_mae:.5f} mean_RMS={mean_rms:.5f} "
        f"max_frame_MAE={max_mae:.5f} min_foreground_ratio={minimum_foreground_ratio:.3f} "
        f"min_grid_IoU={minimum_foreground_iou:.3f} "
        f"python_sha={sha256(python_frames[0])} rust_sha={sha256(rust_frames[0])}"
    )
    if (
        mean_mae > DIG_NEON_MAX_MEAN_FRAME_MAE
        or mean_rms > DIG_NEON_MAX_MEAN_FRAME_RMS
        or max_mae > DIG_NEON_MAX_FRAME_MAE
    ):
        raise AssertionError(
            f"Dig Neon {mode} pixel drift exceeds threshold: "
            f"mean MAE {mean_mae:.5f} <= {DIG_NEON_MAX_MEAN_FRAME_MAE:.5f}, "
            f"mean RMS {mean_rms:.5f} <= {DIG_NEON_MAX_MEAN_FRAME_RMS:.5f}, "
            f"max frame MAE {max_mae:.5f} <= {DIG_NEON_MAX_FRAME_MAE:.5f}"
        )
    return []


def _paired_frame_metrics(
    python_frames: list[bytes],
    rust_frames: list[bytes],
    size: tuple[int, int],
    *,
    label: str,
    minimum_grid_iou: float,
    minimum_count_ratio: float,
) -> tuple[list[tuple[float, float, float]], list[tuple[float, float]]]:
    """Compare ordered animation phases while tolerating Pillow coalescing.

    Pillow may merge adjacent pixel-identical wheel frames when writing a GIF;
    the Rust encoder intentionally keeps all authored phases.  Pair frames by
    normalized position for the wheel while retaining per-frame visual gates.
    Explosion has a one-to-one frame contract and uses this helper as well.
    """

    if not python_frames or not rust_frames:
        raise AssertionError(f"{label} contains no frames")
    pairs = []
    python_span = max(1, len(python_frames) - 1)
    for python_index, python_frame in enumerate(python_frames):
        rust_index = round(python_index * (len(rust_frames) - 1) / python_span)
        pairs.append((python_frame, rust_frames[rust_index]))
    metrics = [pixel_metrics(left, right) for left, right in pairs]
    structures = [
        compare_foreground_structure(
            left,
            right,
            size,
            grid=(10, 10),
            margin=24,
            minimum_grid_iou=minimum_grid_iou,
            minimum_count_ratio=minimum_count_ratio,
            label=f"{label} frame {index}",
        )
        for index, (left, right) in enumerate(pairs)
    ]
    return metrics, structures


def _aggregate_foreground_structure(
    frames: list[bytes], size: tuple[int, int]
) -> tuple[int, set[tuple[int, int]], tuple[float, float]]:
    count = 0
    occupied: set[tuple[int, int]] = set()
    weighted_x = 0
    weighted_y = 0
    weighted_count = 0
    for frame in frames:
        frame_count, frame_cells = foreground_structure(frame, size, (10, 10), 24)
        count += frame_count
        occupied.update(frame_cells)
        for pixel_index in range(size[0] * size[1]):
            offset = pixel_index * 4
            if max(frame[offset : offset + 3]) <= FOREGROUND_CHANNEL_THRESHOLD:
                continue
            weighted_x += pixel_index % size[0]
            weighted_y += pixel_index // size[0]
            weighted_count += 1
    centroid = (
        weighted_x / max(1, weighted_count),
        weighted_y / max(1, weighted_count),
    )
    return count, occupied, centroid


def check_wheel(python_path: Path, rust_path: Path) -> list[str]:
    """Compare the production Python/Rust regular-wheel GIFs.

    The Python GIF can coalesce one or more duplicate frames, while the Rust
    renderer keeps its 70-frame contract.  The shared acceleration bands and
    terminal hold are exact; visual phases are paired by normalized position.
    """

    python_size, python_loop, python_durations, python_frames = gif_frames(python_path)
    rust_size, rust_loop, rust_durations, rust_frames = gif_frames(rust_path)
    if python_size != (500, 500) or rust_size != python_size:
        raise AssertionError(
            f"wheel dimensions differ or are invalid: Python {python_size}, Rust {rust_size}"
        )
    if python_loop != 1 or rust_loop != python_loop:
        raise AssertionError(f"wheel loop count differs: Python {python_loop}, Rust {rust_loop}")
    if not 68 <= len(python_frames) <= 70 or len(rust_frames) != 70:
        raise AssertionError(
            f"wheel frame count differs: Python {len(python_frames)}, Rust {len(rust_frames)}"
        )
    shared_prefix = [30] * 14 + [40] * 14 + [70] * 14 + [110] * 16
    if python_durations[:58] != shared_prefix or rust_durations[:58] != shared_prefix:
        raise AssertionError("wheel acceleration/deceleration timing drifted from Python")
    if python_durations[-2:] != [60_000, 60_000] or rust_durations[-2:] != [60_000, 60_000]:
        raise AssertionError("wheel terminal hold timing drifted")

    metrics, structures = _paired_frame_metrics(
        python_frames,
        rust_frames,
        python_size,
        label="wheel",
        minimum_grid_iou=WHEEL_MIN_FOREGROUND_GRID_IOU,
        minimum_count_ratio=WHEEL_MIN_FOREGROUND_COUNT_RATIO,
    )
    mean_mae = sum(metric[0] for metric in metrics) / len(metrics)
    mean_rms = sum(metric[1] for metric in metrics) / len(metrics)
    max_mae = max(metric[0] for metric in metrics)
    minimum_foreground_ratio = min(structure[0] for structure in structures)
    minimum_foreground_iou = min(structure[1] for structure in structures)
    print(
        f"wheel: size={python_size[0]}x{python_size[1]} "
        f"python_frames={len(python_frames)} rust_frames={len(rust_frames)} "
        f"mean_MAE={mean_mae:.5f} mean_RMS={mean_rms:.5f} max_frame_MAE={max_mae:.5f} "
        f"min_foreground_ratio={minimum_foreground_ratio:.3f} "
        f"min_grid_IoU={minimum_foreground_iou:.3f} "
        f"python_sha={sha256(python_frames[0])} rust_sha={sha256(rust_frames[0])}"
    )
    if (
        mean_mae > WHEEL_MAX_MEAN_FRAME_MAE
        or mean_rms > WHEEL_MAX_MEAN_FRAME_RMS
        or max_mae > WHEEL_MAX_FRAME_MAE
    ):
        raise AssertionError(
            "wheel pixel drift exceeds threshold: "
            f"mean MAE {mean_mae:.5f} <= {WHEEL_MAX_MEAN_FRAME_MAE:.5f}, "
            f"mean RMS {mean_rms:.5f} <= {WHEEL_MAX_MEAN_FRAME_RMS:.5f}, "
            f"max frame MAE {max_mae:.5f} <= {WHEEL_MAX_FRAME_MAE:.5f}"
        )
    return []


def check_explosion(python_path: Path, rust_path: Path) -> list[str]:
    """Compare the production Python/Rust explosion GIFs."""

    python_size, python_loop, python_durations, python_frames = gif_frames(python_path)
    rust_size, rust_loop, rust_durations, rust_frames = gif_frames(rust_path)
    expected_durations = (
        [50] * 14
        + [60, 70, 80, 90, 100, 110, 120, 130, 140, 150]
        + [60] * 4
        + [80] * 14
        + [100] * 13
        + [60_000]
    )
    if python_size != (500, 500) or rust_size != python_size:
        raise AssertionError(
            f"explosion dimensions differ or are invalid: Python {python_size}, Rust {rust_size}"
        )
    if python_loop != 1 or rust_loop != python_loop:
        raise AssertionError(
            f"explosion loop count differs: Python {python_loop}, Rust {rust_loop}"
        )
    if len(python_frames) != 56 or len(rust_frames) != len(python_frames):
        raise AssertionError(
            f"explosion frame count differs: Python {len(python_frames)}, Rust {len(rust_frames)}"
        )
    if python_durations != expected_durations or rust_durations != python_durations:
        raise AssertionError(
            f"explosion durations/order differ: Python {python_durations}, Rust {rust_durations}"
        )

    metrics = [pixel_metrics(left, right) for left, right in zip(python_frames, rust_frames)]
    reference_count, reference_cells, reference_centroid = _aggregate_foreground_structure(
        python_frames, python_size
    )
    candidate_count, candidate_cells, candidate_centroid = _aggregate_foreground_structure(
        rust_frames, rust_size
    )
    if reference_count == 0 or not reference_cells:
        raise AssertionError("explosion reference contains no foreground structure")
    aggregate_ratio = candidate_count / reference_count
    union = reference_cells | candidate_cells
    aggregate_iou = len(reference_cells & candidate_cells) / len(union) if union else 1.0
    aggregate_cell_ratio = len(candidate_cells) / len(reference_cells)
    centroid_drift = (
        (candidate_centroid[0] - reference_centroid[0]) ** 2
        + (candidate_centroid[1] - reference_centroid[1]) ** 2
    ) ** 0.5
    if not (
        EXPLOSION_MIN_AGGREGATE_COUNT_RATIO
        <= aggregate_ratio
        <= EXPLOSION_MAX_AGGREGATE_COUNT_RATIO
    ):
        raise AssertionError(
            "explosion aggregate foreground count drifted: "
            f"ratio {aggregate_ratio:.3f} must be between "
            f"{EXPLOSION_MIN_AGGREGATE_COUNT_RATIO:.3f} and "
            f"{EXPLOSION_MAX_AGGREGATE_COUNT_RATIO:.3f}"
        )
    if aggregate_iou < EXPLOSION_MIN_AGGREGATE_GRID_IOU:
        raise AssertionError(
            "explosion aggregate foreground layout drifted: "
            f"grid IoU {aggregate_iou:.3f} < {EXPLOSION_MIN_AGGREGATE_GRID_IOU:.3f}"
        )
    if not (
        EXPLOSION_MIN_AGGREGATE_CELL_RATIO
        <= aggregate_cell_ratio
        <= EXPLOSION_MAX_AGGREGATE_CELL_RATIO
    ):
        raise AssertionError(
            "explosion aggregate occupied-cell drifted: "
            f"ratio {aggregate_cell_ratio:.3f} must be between "
            f"{EXPLOSION_MIN_AGGREGATE_CELL_RATIO:.3f} and "
            f"{EXPLOSION_MAX_AGGREGATE_CELL_RATIO:.3f}"
        )
    if centroid_drift > EXPLOSION_MAX_AGGREGATE_CENTROID_DRIFT:
        raise AssertionError(
            "explosion aggregate foreground centroid drifted: "
            f"{centroid_drift:.2f}px > {EXPLOSION_MAX_AGGREGATE_CENTROID_DRIFT:.2f}px"
        )
    mean_mae = sum(metric[0] for metric in metrics) / len(metrics)
    mean_rms = sum(metric[1] for metric in metrics) / len(metrics)
    max_mae = max(metric[0] for metric in metrics)
    print(
        f"explosion: size={python_size[0]}x{python_size[1]} frames={len(python_frames)} "
        f"mean_MAE={mean_mae:.5f} mean_RMS={mean_rms:.5f} max_frame_MAE={max_mae:.5f} "
        f"aggregate_foreground_ratio={aggregate_ratio:.3f} "
        f"aggregate_grid_IoU={aggregate_iou:.3f} "
        f"aggregate_cell_ratio={aggregate_cell_ratio:.3f} "
        f"aggregate_centroid_drift={centroid_drift:.2f}px "
        f"python_sha={sha256(python_frames[0])} rust_sha={sha256(rust_frames[0])}"
    )
    if (
        mean_mae > EXPLOSION_MAX_MEAN_FRAME_MAE
        or mean_rms > EXPLOSION_MAX_MEAN_FRAME_RMS
        or max_mae > EXPLOSION_MAX_FRAME_MAE
    ):
        raise AssertionError(
            "explosion pixel drift exceeds threshold: "
            f"mean MAE {mean_mae:.5f} <= {EXPLOSION_MAX_MEAN_FRAME_MAE:.5f}, "
            f"mean RMS {mean_rms:.5f} <= {EXPLOSION_MAX_MEAN_FRAME_RMS:.5f}, "
            f"max frame MAE {max_mae:.5f} <= {EXPLOSION_MAX_FRAME_MAE:.5f}"
        )
    return []


def check_blame_luke(python_path: Path, rust_path: Path) -> list[str]:
    """Compare the production Blame Luke scan/pin/hold animation.

    The Python renderer uses Pillow's configured TrueType fonts and the Rust
    renderer uses its bundled bitmap glyphs, so exact pixels are not expected.
    The gate keeps the live GIF contract exact and constrains both individual
    frame layout and aggregate temporal coverage so a static or contentless
    replacement cannot pass on shared background pixels alone.
    """

    python_size, python_loop, python_durations, python_frames = gif_frames(python_path)
    rust_size, rust_loop, rust_durations, rust_frames = gif_frames(rust_path)
    expected_durations = (
        [170, 160, 150, 140, 140, 130, 120, 120, 110, 100, 100, 90, 80, 70, 70, 70, 70]
        + [90, 110, 160, 8_000]
    )
    if python_size != (720, 420) or rust_size != python_size:
        raise AssertionError(
            f"Blame Luke dimensions differ or are invalid: Python {python_size}, Rust {rust_size}"
        )
    if python_loop is not None or rust_loop != python_loop:
        raise AssertionError(
            f"Blame Luke loop count differs: Python {python_loop}, Rust {rust_loop}"
        )
    if len(python_frames) != 21 or len(rust_frames) != len(python_frames):
        raise AssertionError(
            f"Blame Luke frame count differs: Python {len(python_frames)}, Rust {len(rust_frames)}"
        )
    if python_durations != expected_durations or rust_durations != python_durations:
        raise AssertionError(
            f"Blame Luke durations/order differ: Python {python_durations}, Rust {rust_durations}"
        )
    if python_path.name != "python_blame_luke.gif" or rust_path.name != "rust_blame_luke.gif":
        raise AssertionError("Blame Luke output filenames drifted from the live attachment contract")

    metrics = [pixel_metrics(left, right) for left, right in zip(python_frames, rust_frames)]
    structures = [
        compare_foreground_structure(
            left,
            right,
            python_size,
            grid=(12, 10),
            margin=24,
            minimum_grid_iou=BLAME_LUKE_MIN_FOREGROUND_GRID_IOU,
            minimum_count_ratio=BLAME_LUKE_MIN_FOREGROUND_COUNT_RATIO,
            label=f"Blame Luke frame {index}",
        )
        for index, (left, right) in enumerate(zip(python_frames, rust_frames))
    ]
    reference_count, reference_cells, reference_centroid = _aggregate_foreground_structure(
        python_frames, python_size
    )
    candidate_count, candidate_cells, candidate_centroid = _aggregate_foreground_structure(
        rust_frames, rust_size
    )
    if reference_count == 0 or not reference_cells:
        raise AssertionError("Blame Luke reference contains no aggregate foreground structure")
    aggregate_ratio = candidate_count / reference_count
    aggregate_union = reference_cells | candidate_cells
    aggregate_iou = (
        len(reference_cells & candidate_cells) / len(aggregate_union) if aggregate_union else 1.0
    )
    centroid_drift = (
        (candidate_centroid[0] - reference_centroid[0]) ** 2
        + (candidate_centroid[1] - reference_centroid[1]) ** 2
    ) ** 0.5
    if not (
        BLAME_LUKE_MIN_AGGREGATE_COUNT_RATIO
        <= aggregate_ratio
        <= BLAME_LUKE_MAX_AGGREGATE_COUNT_RATIO
    ):
        raise AssertionError(
            "Blame Luke aggregate foreground count drifted: "
            f"ratio {aggregate_ratio:.3f} must be between "
            f"{BLAME_LUKE_MIN_AGGREGATE_COUNT_RATIO:.3f} and "
            f"{BLAME_LUKE_MAX_AGGREGATE_COUNT_RATIO:.3f}"
        )
    if aggregate_iou < BLAME_LUKE_MIN_AGGREGATE_GRID_IOU:
        raise AssertionError(
            "Blame Luke aggregate foreground layout drifted: "
            f"grid IoU {aggregate_iou:.3f} < {BLAME_LUKE_MIN_AGGREGATE_GRID_IOU:.3f}"
        )
    if centroid_drift > BLAME_LUKE_MAX_AGGREGATE_CENTROID_DRIFT:
        raise AssertionError(
            "Blame Luke aggregate foreground centroid drifted: "
            f"{centroid_drift:.2f}px > {BLAME_LUKE_MAX_AGGREGATE_CENTROID_DRIFT:.2f}px"
        )
    mean_mae = sum(metric[0] for metric in metrics) / len(metrics)
    mean_rms = sum(metric[1] for metric in metrics) / len(metrics)
    max_mae = max(metric[0] for metric in metrics)
    minimum_foreground_ratio = min(structure[0] for structure in structures)
    minimum_foreground_iou = min(structure[1] for structure in structures)
    print(
        f"blame_luke: size={python_size[0]}x{python_size[1]} frames={len(python_frames)} "
        f"loop={python_loop} durations={python_durations} "
        f"mean_MAE={mean_mae:.5f} mean_RMS={mean_rms:.5f} max_frame_MAE={max_mae:.5f} "
        f"min_foreground_ratio={minimum_foreground_ratio:.3f} "
        f"min_grid_IoU={minimum_foreground_iou:.3f} "
        f"aggregate_foreground_ratio={aggregate_ratio:.3f} "
        f"aggregate_grid_IoU={aggregate_iou:.3f} "
        f"aggregate_centroid_drift={centroid_drift:.2f}px "
        f"python_sha={sha256(python_frames[0])} rust_sha={sha256(rust_frames[0])}"
    )
    if (
        mean_mae > BLAME_LUKE_MAX_MEAN_FRAME_MAE
        or mean_rms > BLAME_LUKE_MAX_MEAN_FRAME_RMS
        or max_mae > BLAME_LUKE_MAX_FRAME_MAE
    ):
        raise AssertionError(
            "Blame Luke pixel drift exceeds threshold: "
            f"mean MAE {mean_mae:.5f} <= {BLAME_LUKE_MAX_MEAN_FRAME_MAE:.5f}, "
            f"mean RMS {mean_rms:.5f} <= {BLAME_LUKE_MAX_MEAN_FRAME_RMS:.5f}, "
            f"max frame MAE {max_mae:.5f} <= {BLAME_LUKE_MAX_FRAME_MAE:.5f}"
        )
    return []


def check_scout(python_path: Path, rust_path: Path, expected_rows: int) -> list[str]:
    """Compare the production Scout report's cache-miss fallback geometry."""

    python_size, python_pixels = rgba_pixels(python_path)
    rust_size, rust_pixels = rgba_pixels(rust_path)
    expected_size = (360, 12 + 50 + expected_rows * 32 + 12)
    if python_size != expected_size or rust_size != python_size:
        raise AssertionError(
            f"scout dimensions differ or are invalid: Python {python_size}, Rust {rust_size}; "
            f"expected {expected_size}"
        )
    mae, rms, exact = pixel_metrics(python_pixels, rust_pixels)
    foreground_ratio, foreground_iou = compare_foreground_structure(
        python_pixels,
        rust_pixels,
        python_size,
        grid=(18, max(1, python_size[1] // 10)),
        margin=0,
        minimum_grid_iou=SCOUT_MIN_FOREGROUND_GRID_IOU,
        minimum_count_ratio=SCOUT_MIN_FOREGROUND_COUNT_RATIO,
        label="scout",
    )
    print(
        f"scout: size={python_size[0]}x{python_size[1]} rows={expected_rows} "
        f"MAE={mae:.5f} RMS={rms:.5f} exact_channels={exact:.3%} "
        f"foreground_ratio={foreground_ratio:.3f} grid_IoU={foreground_iou:.3f} "
        f"python_sha={sha256(python_pixels)} rust_sha={sha256(rust_pixels)}"
    )
    if mae > SCOUT_MAX_MAE or rms > SCOUT_MAX_RMS:
        raise AssertionError(
            "scout pixel drift exceeds threshold: "
            f"MAE {mae:.5f} <= {SCOUT_MAX_MAE:.5f}, "
            f"RMS {rms:.5f} <= {SCOUT_MAX_RMS:.5f}"
        )
    return []


def check_hero_grid(python_path: Path, rust_path: Path, expected_players: int, expected_heroes: int) -> list[str]:
    """Compare the live Python/Rust Hero Grid raster geometry and semantics."""

    python_size, python_pixels = rgba_pixels(python_path)
    rust_size, rust_pixels = rgba_pixels(rust_path)
    expected_size = (
        15 + 120 + expected_heroes * 44 + 15,
        15 + 30 + 90 + expected_players * 44 + 80 + 15,
    )
    if python_size != expected_size or rust_size != python_size:
        raise AssertionError(
            f"hero grid dimensions differ or are invalid: Python {python_size}, Rust {rust_size}; "
            f"expected {expected_size}"
        )
    mae, rms, exact = pixel_metrics(python_pixels, rust_pixels)
    foreground_ratio, foreground_iou = compare_foreground_structure(
        python_pixels,
        rust_pixels,
        python_size,
        grid=(10, 10),
        margin=8,
        minimum_grid_iou=HERO_GRID_MIN_FOREGROUND_GRID_IOU,
        minimum_count_ratio=HERO_GRID_MIN_FOREGROUND_COUNT_RATIO,
        label="hero grid",
    )
    print(
        f"hero_grid: size={python_size[0]}x{python_size[1]} players={expected_players} "
        f"heroes={expected_heroes} MAE={mae:.5f} RMS={rms:.5f} exact_channels={exact:.3%} "
        f"foreground_ratio={foreground_ratio:.3f} grid_IoU={foreground_iou:.3f} "
        f"python_sha={sha256(python_pixels)} rust_sha={sha256(rust_pixels)}"
    )
    if mae > HERO_GRID_MAX_MAE or rms > HERO_GRID_MAX_RMS:
        raise AssertionError(
            "hero grid pixel drift exceeds threshold: "
            f"MAE {mae:.5f} <= {HERO_GRID_MAX_MAE:.5f}, "
            f"RMS {rms:.5f} <= {HERO_GRID_MAX_RMS:.5f}"
        )
    return []


def _check_profile_image(
    python_path: Path,
    rust_path: Path,
    *,
    label: str,
    expected_size: tuple[int, int],
    grid: tuple[int, int],
    margin: int,
    minimum_grid_iou: float,
    minimum_count_ratio: float,
    max_mae: float,
    max_rms: float,
    semantic_colors: dict[str, tuple[int, int, int, int]],
) -> list[str]:
    """Compare one profile attachment at its production renderer boundary."""

    python_size, python_pixels = rgba_pixels(python_path)
    rust_size, rust_pixels = rgba_pixels(rust_path)
    if python_size != expected_size or rust_size != python_size:
        raise AssertionError(
            f"{label} dimensions differ or are invalid: Python {python_size}, "
            f"Rust {rust_size}; expected {expected_size}"
        )
    mae, rms, exact = pixel_metrics(python_pixels, rust_pixels)
    foreground_ratio, foreground_iou = compare_foreground_structure(
        python_pixels,
        rust_pixels,
        python_size,
        grid=grid,
        margin=margin,
        minimum_grid_iou=minimum_grid_iou,
        minimum_count_ratio=minimum_count_ratio,
        label=label,
    )

    def color_count(pixels: bytes, color: tuple[int, int, int, int]) -> int:
        # Pillow text and result labels are antialiased, so semantic colors
        # are checked by a tight RGB distance rather than requiring every
        # reference edge pixel to be opaque. Native bars/results remain exact
        # palette pixels and therefore still satisfy this stricter bound.
        return sum(
            sum(
                abs(channel - expected)
                for channel, expected in zip(pixels[offset : offset + 3], color[:3])
            )
            <= 30
            for offset in range(0, len(pixels), 4)
        )

    counts = {
        name: color_count(rust_pixels, color)
        for name, color in semantic_colors.items()
    }
    missing = [name for name, count in counts.items() if count == 0]
    if missing:
        raise AssertionError(f"{label} lost semantic colors: {counts}")
    print(
        f"{label}: size={python_size[0]}x{python_size[1]} "
        f"MAE={mae:.5f} RMS={rms:.5f} exact_channels={exact:.3%} "
        f"foreground_ratio={foreground_ratio:.3f} grid_IoU={foreground_iou:.3f} "
        f"semantic_colors={counts} python_sha={sha256(python_pixels)} "
        f"rust_sha={sha256(rust_pixels)}"
    )
    if mae > max_mae or rms > max_rms:
        raise AssertionError(
            f"{label} pixel drift exceeds threshold: MAE {mae:.5f} <= {max_mae:.5f}, "
            f"RMS {rms:.5f} <= {max_rms:.5f}"
        )
    return []


def check_profile_role_graph(python_path: Path, rust_path: Path) -> list[str]:
    """Compare the profile role radar and its labels/scale annotations."""

    return _check_profile_image(
        python_path,
        rust_path,
        label="profile role graph",
        expected_size=(400, 400),
        grid=(10, 10),
        margin=0,
        minimum_grid_iou=PROFILE_ROLE_MIN_FOREGROUND_GRID_IOU,
        minimum_count_ratio=PROFILE_ROLE_MIN_FOREGROUND_COUNT_RATIO,
        max_mae=PROFILE_ROLE_MAX_MAE,
        max_rms=PROFILE_ROLE_MAX_RMS,
        semantic_colors={
            "accent": (88, 101, 242, 255),
        },
    )


def check_profile_lane_distribution(
    python_path: Path, rust_path: Path, lane_count: int
) -> list[str]:
    """Compare the profile lane bars, order, labels, and semantic colors."""

    return _check_profile_image(
        python_path,
        rust_path,
        label="profile lane distribution",
        expected_size=(350, lane_count * 40 + 60),
        grid=(10, 10),
        margin=0,
        minimum_grid_iou=PROFILE_LANE_MIN_FOREGROUND_GRID_IOU,
        minimum_count_ratio=PROFILE_LANE_MIN_FOREGROUND_COUNT_RATIO,
        max_mae=PROFILE_LANE_MAX_MAE,
        max_rms=PROFILE_LANE_MAX_RMS,
        semantic_colors={
            "roaming": (233, 30, 99, 255),
            "safe": (76, 175, 80, 255),
            "mid": (33, 150, 243, 255),
            "off": (255, 152, 0, 255),
            "jungle": (156, 39, 176, 255),
        },
    )


def check_profile_hero_performance(
    python_path: Path, rust_path: Path, hero_count: int
) -> list[str]:
    """Compare profile hero rows, win-rate colors, and game-count bars."""

    return _check_profile_image(
        python_path,
        rust_path,
        label="profile hero performance",
        expected_size=(450, 38 * hero_count + 65),
        grid=(10, 10),
        margin=0,
        minimum_grid_iou=PROFILE_HERO_MIN_FOREGROUND_GRID_IOU,
        minimum_count_ratio=PROFILE_HERO_MIN_FOREGROUND_COUNT_RATIO,
        max_mae=PROFILE_HERO_MAX_MAE,
        max_rms=PROFILE_HERO_MAX_RMS,
        semantic_colors={
            "green": (87, 242, 135, 255),
            "yellow": (254, 231, 92, 255),
            "red": (237, 66, 69, 255),
        },
    )


def check_profile_recent_matches(
    python_path: Path, rust_path: Path, row_count: int
) -> list[str]:
    """Compare recent-match rows, result states, and missing-value rendering."""

    return _check_profile_image(
        python_path,
        rust_path,
        label="profile recent matches",
        expected_size=(370, 36 * row_count + 52),
        grid=(10, 10),
        margin=0,
        minimum_grid_iou=PROFILE_RECENT_MIN_FOREGROUND_GRID_IOU,
        minimum_count_ratio=PROFILE_RECENT_MIN_FOREGROUND_COUNT_RATIO,
        max_mae=PROFILE_RECENT_MAX_MAE,
        max_rms=PROFILE_RECENT_MAX_RMS,
        semantic_colors={
            "accent": (88, 101, 242, 255),
            "green": (87, 242, 135, 255),
            "red": (237, 66, 69, 255),
            "grey": (185, 187, 190, 255),
        },
    )


def _gamba_layer_structure(
    rgba: bytes,
    size: tuple[int, int],
    expected: tuple[int, int, int],
    region: tuple[int, int, int, int],
    *,
    distance: int = PROFILE_GAMBA_COLOR_DISTANCE,
    grid: tuple[int, int] = (14, 8),
) -> tuple[int, set[tuple[int, int]]]:
    """Locate one authored Gamba layer independently of whole-frame pixels."""

    width, height = size
    left, top, right, bottom = region
    columns, rows = grid
    count = 0
    cells: set[tuple[int, int]] = set()
    for pixel_index in range(width * height):
        x = pixel_index % width
        y = pixel_index // width
        if not left <= x < right or not top <= y < bottom:
            continue
        offset = pixel_index * 4
        pixel = rgba[offset : offset + 3]
        if sum(abs(channel - target) for channel, target in zip(pixel, expected)) > distance:
            continue
        count += 1
        cells.add((x * columns // width, y * rows // height))
    return count, cells


def _compare_gamba_layer(
    reference: bytes,
    candidate: bytes,
    size: tuple[int, int],
    *,
    label: str,
    expected: tuple[int, int, int],
    region: tuple[int, int, int, int],
    minimum_count_ratio: float = PROFILE_GAMBA_MIN_LAYER_COUNT_RATIO,
    minimum_grid_iou: float = PROFILE_GAMBA_MIN_LAYER_GRID_IOU,
) -> tuple[float, float]:
    reference_count, reference_cells = _gamba_layer_structure(reference, size, expected, region)
    candidate_count, candidate_cells = _gamba_layer_structure(candidate, size, expected, region)
    if reference_count == 0 or not reference_cells:
        raise AssertionError(f"profile gamba {label} reference layer is empty")
    ratio = candidate_count / reference_count
    union = reference_cells | candidate_cells
    iou = len(reference_cells & candidate_cells) / len(union) if union else 1.0
    if ratio < minimum_count_ratio:
        raise AssertionError(
            f"profile gamba {label} layer is missing: count ratio {ratio:.3f} "
            f"< {minimum_count_ratio:.3f}"
        )
    if iou < minimum_grid_iou:
        raise AssertionError(
            f"profile gamba {label} layer layout drifted: grid IoU {iou:.3f} "
            f"< {minimum_grid_iou:.3f}"
        )
    return ratio, iou


def _gamba_marker_specs(
    gamba: dict[str, Any],
) -> dict[str, list[tuple[int, int, str]]]:
    """Project fixture marker centers and preserve their authored shape/color."""

    series = gamba["series"]
    values = [float(point["cumulative"]) for point in series]
    logs = [
        (1.0 if value > 0 else -1.0 if value < 0 else 0.0) * math.log1p(abs(value))
        for value in values
    ] or [0.0]
    minimum = min(min(logs), 0.0)
    maximum = max(max(logs), 0.0)
    span = max(abs(maximum - minimum), 0.1)
    minimum -= span * 0.1
    maximum += span * 0.1
    log_span = max(maximum - minimum, 1e-9)
    result: dict[str, list[tuple[int, int, str]]] = {}
    for point in series:
        x = 60 + int(
            (int(point["event_number"]) - 1) / max(len(series) - 1, 1) * 614
        )
        value = float(point["cumulative"])
        signed = (1.0 if value > 0 else -1.0 if value < 0 else 0.0) * math.log1p(abs(value))
        y = 88 + int((maximum - signed) / log_span * 222)
        source = str(point["source"])
        if source == "double_or_nothing":
            kind = "double_or_nothing"
        elif source == "wheel":
            kind = "wheel"
        elif int(point.get("leverage", 1)) > 1:
            kind = "leverage"
        else:
            kind = "bet"
        result.setdefault(kind, []).append((x, y, str(point.get("outcome", "lost"))))
    return result


def _gamba_marker_signature(
    rgba: bytes,
    size: tuple[int, int],
    specs: list[tuple[int, int, str]],
    *,
    radius: int = 7,
) -> tuple[dict[str, int], set[tuple[int, str, int, int]]]:
    """Capture local shape and semantic-color occupancy for one marker kind."""

    width, height = size
    palette = {
        "white": (255, 255, 255),
        "dark": (47, 49, 54),
        "won": (87, 242, 135),
        "lost": (237, 66, 69),
        "neutral": (185, 187, 190),
    }
    counts = {"outcome": 0, "dark": 0, "white": 0}
    cells: set[tuple[int, str, int, int]] = set()
    for marker_index, (center_x, center_y, outcome) in enumerate(specs):
        outcome_color = palette.get(outcome, palette["lost"])
        for y in range(max(0, center_y - radius), min(height, center_y + radius + 1)):
            for x in range(max(0, center_x - radius), min(width, center_x + radius + 1)):
                offset = (y * width + x) * 4
                pixel = rgba[offset : offset + 3]
                outcome_distance = sum(
                    abs(channel - expected)
                    for channel, expected in zip(pixel, outcome_color)
                )
                dark_distance = sum(
                    abs(channel - expected)
                    for channel, expected in zip(pixel, palette["dark"])
                )
                white_distance = sum(
                    abs(channel - expected)
                    for channel, expected in zip(pixel, palette["white"])
                )
                if outcome_distance <= PROFILE_GAMBA_MARKER_COLOR_DISTANCE:
                    role = "outcome"
                elif dark_distance <= PROFILE_GAMBA_MARKER_DARK_DISTANCE:
                    role = "dark"
                elif white_distance <= PROFILE_GAMBA_MARKER_WHITE_DISTANCE:
                    role = "white"
                else:
                    continue
                counts[role] += 1
                cells.add(
                    (
                        marker_index,
                        role,
                        (x - center_x + radius) // 2,
                        (y - center_y + radius) // 2,
                    )
                )
    return counts, cells


def _compare_gamba_marker_kind(
    reference: bytes,
    candidate: bytes,
    size: tuple[int, int],
    *,
    kind: str,
    specs: list[tuple[int, int, str]],
) -> dict[str, float]:
    """Reject erased, recolored, or shape-substituted markers by kind."""

    reference_counts, reference_cells = _gamba_marker_signature(reference, size, specs)
    candidate_counts, candidate_cells = _gamba_marker_signature(candidate, size, specs)
    required_roles = {
        "bet": ("outcome",),
        "wheel": ("outcome", "dark", "white"),
        "leverage": ("outcome", "dark"),
        "double_or_nothing": ("outcome", "dark"),
    }[kind]
    role_ratios: dict[str, float] = {}
    for role in required_roles:
        reference_count = reference_counts[role]
        candidate_count = candidate_counts[role]
        ratio = candidate_count / max(reference_count, 1)
        role_ratios[role] = ratio
        if reference_count == 0 or ratio < PROFILE_GAMBA_MIN_MARKER_ROLE_RATIO:
            raise AssertionError(
                f"profile gamba {kind} marker {role} occupancy is missing: "
                f"ratio {ratio:.3f} < {PROFILE_GAMBA_MIN_MARKER_ROLE_RATIO:.3f}"
            )
    if kind != "wheel" and candidate_counts["white"] > reference_counts["white"] + 2:
        raise AssertionError(
            f"profile gamba {kind} marker has forbidden white-spoke occupancy: "
            f"{candidate_counts['white']} pixels"
        )
    union = reference_cells | candidate_cells
    shape_iou = len(reference_cells & candidate_cells) / len(union) if union else 1.0
    if shape_iou < PROFILE_GAMBA_MIN_MARKER_SHAPE_IOU:
        raise AssertionError(
            f"profile gamba {kind} marker shape was erased or substituted: "
            f"IoU {shape_iou:.3f} < {PROFILE_GAMBA_MIN_MARKER_SHAPE_IOU:.3f}"
        )
    return {"shape_iou": shape_iou, **role_ratios}


def check_profile_gamba(
    python_path: Path,
    rust_path: Path,
    gamba: dict[str, Any] | None = None,
) -> list[str]:
    """Compare the live `/profile` Gamba chart by independent authored layers."""

    python_size, python_pixels = rgba_pixels(python_path)
    rust_size, rust_pixels = rgba_pixels(rust_path)
    if python_size != (700, 400) or rust_size != python_size:
        raise AssertionError(
            f"profile gamba dimensions differ or are invalid: Python {python_size}, "
            f"Rust {rust_size}; expected (700, 400)"
        )
    mae, rms, exact = pixel_metrics(python_pixels, rust_pixels)
    foreground_ratio, foreground_iou = compare_foreground_structure(
        python_pixels,
        rust_pixels,
        python_size,
        grid=(14, 8),
        margin=0,
        minimum_grid_iou=PROFILE_GAMBA_MIN_FOREGROUND_GRID_IOU,
        minimum_count_ratio=PROFILE_GAMBA_MIN_FOREGROUND_COUNT_RATIO,
        label="profile gamba",
    )
    plot = (60, 88, 674, 310)
    strip = (60, 346, 640, 389)
    # The fill colors are intentionally translucent over Discord's background;
    # a broad distance captures Pillow's alpha-composited pixels and native
    # integer blending while excluding the unrelated white/grey text layers.
    layers = {
        "positive fill": ((75, 120, 80), plot),
        "negative fill": ((105, 70, 75), plot),
        "event markers": ((87, 242, 135), plot),
        "callouts": ((47, 49, 54), plot),
        "axes": ((185, 187, 190), plot),
        "stat strip": ((47, 49, 54), strip),
    }
    layer_metrics = {
        label: _compare_gamba_layer(
            python_pixels,
            rust_pixels,
            python_size,
            label=label,
            expected=color,
            region=region,
        )
        for label, (color, region) in layers.items()
    }
    # Explicit red marker/callout sensitivity is separate from green because a
    # chart containing only wins can otherwise pass a green-only mask.
    layer_metrics["red markers/callouts"] = _compare_gamba_layer(
        python_pixels,
        rust_pixels,
        python_size,
        label="red markers/callouts",
        expected=(237, 66, 69),
        region=plot,
    )
    marker_metrics = {}
    if gamba is not None:
        marker_specs = _gamba_marker_specs(gamba)
        for kind in ("bet", "wheel", "leverage", "double_or_nothing"):
            specs = marker_specs.get(kind, [])
            if not specs:
                continue
            marker_metrics[kind] = _compare_gamba_marker_kind(
                python_pixels,
                rust_pixels,
                python_size,
                kind=kind,
                specs=specs,
            )
    print(
        f"profile gamba: size={python_size[0]}x{python_size[1]} "
        f"MAE={mae:.5f} RMS={rms:.5f} exact_channels={exact:.3%} "
        f"foreground_ratio={foreground_ratio:.3f} grid_IoU={foreground_iou:.3f} "
        f"layers={layer_metrics} markers={marker_metrics} python_sha={sha256(python_pixels)} "
        f"rust_sha={sha256(rust_pixels)}"
    )
    if mae > PROFILE_GAMBA_MAX_MAE or rms > PROFILE_GAMBA_MAX_RMS:
        raise AssertionError(
            f"profile gamba pixel drift exceeds threshold: MAE {mae:.5f} <= "
            f"{PROFILE_GAMBA_MAX_MAE:.5f}, RMS {rms:.5f} <= {PROFILE_GAMBA_MAX_RMS:.5f}"
        )
    return []


def _assert_native_bitmap_copy(
    rgba: bytes,
    size: tuple[int, int],
    *,
    text: str,
    left: int,
    top: int,
    scale: int,
    color: tuple[int, int, int, int],
    label: str,
) -> None:
    """Verify case-sensitive/dot cells in one deterministic native text run."""

    width, height = size
    if len(rgba) != width * height * 4:
        raise ValueError(f"{label} received a malformed RGBA buffer")
    checked = 0
    for index, character in enumerate(text):
        glyph = (
            _NATIVE_MIDDLE_DOT_GLYPH
            if character == "·"
            else _NATIVE_CASE_GLYPHS.get(character)
        )
        if glyph is None:
            continue
        checked += 1
        glyph_left = left + index * 6 * scale
        for row, bits in enumerate(glyph):
            for column in range(5):
                expected_on = bool(bits & (1 << (4 - column)))
                for offset_y in range(scale):
                    for offset_x in range(scale):
                        x = glyph_left + column * scale + offset_x
                        y = top + row * scale + offset_y
                        if not (0 <= x < width and 0 <= y < height):
                            raise AssertionError(f"{label} falls outside the native canvas")
                        offset = (y * width + x) * 4
                        actual = tuple(rgba[offset : offset + 4])
                        if expected_on and actual != color:
                            raise AssertionError(
                                f"{label} authored copy drifted at {character!r} "
                                f"row {row} column {column}: expected native "
                                f"foreground {color}, got {actual}"
                            )
                        if not expected_on and actual == color:
                            raise AssertionError(
                                f"{label} authored copy drifted at {character!r} "
                                f"row {row} column {column}: unexpected native "
                                "foreground pixel"
                            )
    if checked == 0:
        raise AssertionError(f"{label} contains no case-sensitive or middle-dot glyphs")


def _assert_native_wrapped_gamba_copy(
    rgba: bytes,
    size: tuple[int, int],
    wrapped_gamba: dict[str, Any],
) -> None:
    """Guard the authored text regions without coupling to one raster font."""

    if not str(wrapped_gamba["footer"]):
        raise AssertionError("wrapped Gamba fixture footer is empty")
    width, height = size
    if len(rgba) != width * height * 4:
        raise ValueError("wrapped Gamba copy received a malformed RGBA buffer")

    def require_color(
        label: str,
        region: tuple[int, int, int, int],
        color: tuple[int, int, int, int],
    ) -> None:
        left, top, right, bottom = region
        count = 0
        for y in range(top, bottom):
            for x in range(left, right):
                offset = (y * width + x) * 4
                if tuple(rgba[offset : offset + 4]) == color:
                    count += 1
        if count < 8:
            raise AssertionError(f"{label} authored copy is missing")

    require_color(
        "wrapped Gamba footer", (150, 545, 650, 590), _NATIVE_WRAPPED_GREY
    )
    require_color(
        "wrapped Gamba chart title", (90, 50, 550, 82), _NATIVE_GAMBA_WHITE
    )
    require_color(
        "wrapped Gamba chart subtitle", (90, 78, 550, 112), _NATIVE_GAMBA_GREY
    )


def check_wrapped_gamba(
    python_path: Path,
    rust_path: Path,
    wrapped_gamba: dict[str, Any] | None = None,
    *,
    require_native_copy: bool = False,
) -> list[str]:
    """Compare the separate `/wrapped` Gamba story attachment boundary."""

    python_size, python_pixels = rgba_pixels(python_path)
    rust_size, rust_pixels = rgba_pixels(rust_path)
    if python_size != (800, 600) or rust_size != python_size:
        raise AssertionError(
            f"wrapped gamba dimensions differ or are invalid: Python {python_size}, "
            f"Rust {rust_size}; expected (800, 600)"
        )
    if require_native_copy:
        if wrapped_gamba is None:
            raise ValueError("native Wrapped Gamba copy requires typed fixture metadata")
        _assert_native_wrapped_gamba_copy(rust_pixels, rust_size, wrapped_gamba)
    mae, rms, exact = pixel_metrics(python_pixels, rust_pixels)
    foreground_ratio, foreground_iou = compare_foreground_structure(
        python_pixels,
        rust_pixels,
        python_size,
        grid=(16, 12),
        margin=0,
        minimum_grid_iou=WRAPPED_GAMBA_MIN_FOREGROUND_GRID_IOU,
        minimum_count_ratio=WRAPPED_GAMBA_MIN_FOREGROUND_COUNT_RATIO,
        label="wrapped gamba",
    )
    chart_region = (110, 133, 724, 355)
    strip_region = (110, 391, 690, 434)
    layer_metrics = {
        "positive fill": _compare_gamba_layer(
            python_pixels,
            rust_pixels,
            python_size,
            label="wrapped gamba positive fill",
            expected=(75, 120, 80),
            region=chart_region,
        ),
        "negative fill": _compare_gamba_layer(
            python_pixels,
            rust_pixels,
            python_size,
            label="wrapped gamba negative fill",
            expected=(105, 70, 75),
            region=chart_region,
        ),
        "stat strip": _compare_gamba_layer(
            python_pixels,
            rust_pixels,
            python_size,
            label="wrapped gamba stat strip",
            expected=(47, 49, 54),
            region=strip_region,
        ),
        "story accent": _compare_gamba_layer(
            python_pixels,
            rust_pixels,
            python_size,
            label="wrapped gamba story accent",
            expected=(241, 196, 15),
            region=(0, 0, 800, 45),
        ),
    }
    marker_metrics = {}
    if wrapped_gamba is not None:
        payload = wrapped_gamba.get("gamba", wrapped_gamba)
        marker_specs = _gamba_marker_specs(payload)
        translated = {
            kind: [(x + 50, y + 45, outcome) for x, y, outcome in specs]
            for kind, specs in marker_specs.items()
        }
        for kind in ("bet", "wheel", "leverage", "double_or_nothing"):
            specs = translated.get(kind, [])
            if specs:
                marker_metrics[kind] = _compare_gamba_marker_kind(
                    python_pixels,
                    rust_pixels,
                    python_size,
                    kind=kind,
                    specs=specs,
                )
    print(
        f"wrapped gamba: size={python_size[0]}x{python_size[1]} "
        f"MAE={mae:.5f} RMS={rms:.5f} exact_channels={exact:.3%} "
        f"foreground_ratio={foreground_ratio:.3f} grid_IoU={foreground_iou:.3f} "
        f"layers={layer_metrics} markers={marker_metrics} "
        f"python_sha={sha256(python_pixels)} rust_sha={sha256(rust_pixels)}"
    )
    if mae > WRAPPED_GAMBA_MAX_MAE or rms > WRAPPED_GAMBA_MAX_RMS:
        raise AssertionError(
            f"wrapped gamba pixel drift exceeds threshold: MAE {mae:.5f} <= "
            f"{WRAPPED_GAMBA_MAX_MAE:.5f}, RMS {rms:.5f} <= {WRAPPED_GAMBA_MAX_RMS:.5f}"
        )
    return []


def check_terminal_crash(python_path: Path, rust_path: Path) -> list[str]:
    """Compare the production Python/Rust bankruptcy crash renderers.

    Both implementations intentionally use different raster backends and
    fonts.  The gate therefore checks exact playback metadata plus bounded
    pixel/layout drift, while rejecting a blank or contentless replacement.
    """

    python_size, python_loop, python_durations, python_frames = gif_frames(python_path)
    rust_size, rust_loop, rust_durations, rust_frames = gif_frames(rust_path)
    if python_size != (400, 300) or rust_size != python_size:
        raise AssertionError(
            f"terminal crash dimensions differ or are invalid: Python {python_size}, Rust {rust_size}"
        )
    if python_loop != 1 or rust_loop != python_loop:
        raise AssertionError(
            f"terminal crash loop count differs: Python {python_loop}, Rust {rust_loop}"
        )
    if len(python_frames) != 58 or len(rust_frames) != len(python_frames):
        raise AssertionError(
            f"terminal crash frame count differs: Python {len(python_frames)}, Rust {len(rust_frames)}"
        )
    if rust_durations != python_durations:
        raise AssertionError(
            f"terminal crash durations/order differ: Python {python_durations}, Rust {rust_durations}"
        )

    metrics = [pixel_metrics(left, right) for left, right in zip(python_frames, rust_frames)]
    structures = []
    for index, (left, right) in enumerate(zip(python_frames, rust_frames)):
        reference_count, reference_cells = foreground_structure(
            left, python_size, (10, 10), 24
        )
        candidate_count, candidate_cells = foreground_structure(
            right, rust_size, (10, 10), 24
        )
        if reference_count == 0:
            if candidate_count != 0 or candidate_cells:
                raise AssertionError(
                    f"terminal crash frame {index} adds content during the intentional blank hold"
                )
            continue
        structures.append(
            compare_foreground_structure(
                left,
                right,
                python_size,
                grid=(10, 10),
                margin=24,
                minimum_grid_iou=TERMINAL_CRASH_MIN_FOREGROUND_GRID_IOU,
                minimum_count_ratio=TERMINAL_CRASH_MIN_FOREGROUND_COUNT_RATIO,
                label=f"terminal crash frame {index}",
            )
        )
    if not structures:
        raise AssertionError("terminal crash contains no nonblank foreground frames")
    mean_mae = sum(metric[0] for metric in metrics) / len(metrics)
    mean_rms = sum(metric[1] for metric in metrics) / len(metrics)
    max_mae = max(metric[0] for metric in metrics)
    minimum_foreground_ratio = min(structure[0] for structure in structures)
    minimum_foreground_iou = min(structure[1] for structure in structures)
    print(
        f"terminal_crash: size={python_size[0]}x{python_size[1]} frames={len(python_frames)} "
        f"loop={python_loop} durations={python_durations} "
        f"mean_MAE={mean_mae:.5f} mean_RMS={mean_rms:.5f} max_frame_MAE={max_mae:.5f} "
        f"min_foreground_ratio={minimum_foreground_ratio:.3f} "
        f"min_grid_IoU={minimum_foreground_iou:.3f} "
        f"python_sha={sha256(python_frames[0])} rust_sha={sha256(rust_frames[0])}"
    )
    if (
        mean_mae > TERMINAL_CRASH_MAX_MEAN_FRAME_MAE
        or mean_rms > TERMINAL_CRASH_MAX_MEAN_FRAME_RMS
        or max_mae > TERMINAL_CRASH_MAX_FRAME_MAE
    ):
        raise AssertionError(
            "terminal crash pixel drift exceeds threshold: "
            f"mean MAE {mean_mae:.5f} <= {TERMINAL_CRASH_MAX_MEAN_FRAME_MAE:.5f}, "
            f"mean RMS {mean_rms:.5f} <= {TERMINAL_CRASH_MAX_MEAN_FRAME_RMS:.5f}, "
            f"max frame MAE {max_mae:.5f} <= {TERMINAL_CRASH_MAX_FRAME_MAE:.5f}"
        )
    return []


def check_pinnacle(python_path: Path, rust_path: Path) -> list[str]:
    """Compare the production Python/Rust pinnacle phase-3 renderers."""

    python_size, python_loop, python_durations, python_frames = gif_frames(python_path)
    rust_size, rust_loop, rust_durations, rust_frames = gif_frames(rust_path)
    expected_durations = [90] * 7 + [1_500]
    if python_size != (512, 288) or rust_size != python_size:
        raise AssertionError(
            f"pinnacle dimensions differ or are invalid: Python {python_size}, Rust {rust_size}"
        )
    if python_loop is not None or rust_loop != python_loop:
        raise AssertionError(
            f"pinnacle loop count differs: Python {python_loop}, Rust {rust_loop}"
        )
    if len(python_frames) != 8 or len(rust_frames) != len(python_frames):
        raise AssertionError(
            f"pinnacle frame count differs: Python {len(python_frames)}, Rust {len(rust_frames)}"
        )
    if python_durations != expected_durations or rust_durations != python_durations:
        raise AssertionError(
            f"pinnacle durations/order differ: Python {python_durations}, Rust {rust_durations}"
        )

    metrics = [pixel_metrics(left, right) for left, right in zip(python_frames, rust_frames)]
    structures = [
        compare_foreground_structure(
            left,
            right,
            python_size,
            grid=(10, 10),
            margin=24,
            minimum_grid_iou=PINNACLE_MIN_FOREGROUND_GRID_IOU,
            minimum_count_ratio=PINNACLE_MIN_FOREGROUND_COUNT_RATIO,
            label=f"pinnacle frame {index}",
        )
        for index, (left, right) in enumerate(zip(python_frames, rust_frames))
    ]
    mean_mae = sum(metric[0] for metric in metrics) / len(metrics)
    mean_rms = sum(metric[1] for metric in metrics) / len(metrics)
    max_mae = max(metric[0] for metric in metrics)
    minimum_foreground_ratio = min(structure[0] for structure in structures)
    minimum_foreground_iou = min(structure[1] for structure in structures)
    print(
        f"pinnacle: size={python_size[0]}x{python_size[1]} frames={len(python_frames)} "
        f"loop={python_loop} durations={python_durations} "
        f"mean_MAE={mean_mae:.5f} mean_RMS={mean_rms:.5f} max_frame_MAE={max_mae:.5f} "
        f"min_foreground_ratio={minimum_foreground_ratio:.3f} "
        f"min_grid_IoU={minimum_foreground_iou:.3f} "
        f"python_sha={sha256(python_frames[0])} rust_sha={sha256(rust_frames[0])}"
    )
    if (
        mean_mae > PINNACLE_MAX_MEAN_FRAME_MAE
        or mean_rms > PINNACLE_MAX_MEAN_FRAME_RMS
        or max_mae > PINNACLE_MAX_FRAME_MAE
    ):
        raise AssertionError(
            "pinnacle pixel drift exceeds threshold: "
            f"mean MAE {mean_mae:.5f} <= {PINNACLE_MAX_MEAN_FRAME_MAE:.5f}, "
            f"mean RMS {mean_rms:.5f} <= {PINNACLE_MAX_MEAN_FRAME_RMS:.5f}, "
            f"max frame MAE {max_mae:.5f} <= {PINNACLE_MAX_FRAME_MAE:.5f}"
        )
    return []


def check_balance(python_path: Path, rust_path: Path) -> list[str]:
    python_size, python_pixels = rgba_pixels(python_path)
    rust_size, rust_pixels = rgba_pixels(rust_path)
    if python_size != rust_size:
        raise AssertionError(f"balance dimensions differ: Python {python_size}, Rust {rust_size}")
    mae, rms, exact = pixel_metrics(python_pixels, rust_pixels)
    foreground_ratio, foreground_iou = compare_foreground_structure(
        python_pixels,
        rust_pixels,
        python_size,
        grid=(10, 10),
        margin=24,
        minimum_grid_iou=BALANCE_MIN_FOREGROUND_GRID_IOU,
        label="balance",
        minimum_count_ratio=BALANCE_MIN_FOREGROUND_COUNT_RATIO,
    )
    print(
        f"balance: size={python_size[0]}x{python_size[1]} "
        f"MAE={mae:.5f} RMS={rms:.5f} exact_channels={exact:.3%} "
        f"foreground_ratio={foreground_ratio:.3f} grid_IoU={foreground_iou:.3f} "
        f"python_sha={sha256(python_pixels)} rust_sha={sha256(rust_pixels)}"
    )
    if mae > BALANCE_MAX_MAE or rms > BALANCE_MAX_RMS:
        raise AssertionError(
            f"balance pixel drift exceeds threshold: MAE {mae:.5f} <= {BALANCE_MAX_MAE:.5f}, "
            f"RMS {rms:.5f} <= {BALANCE_MAX_RMS:.5f}"
        )
    return []


def check_advantage(python_path: Path, rust_path: Path) -> list[str]:
    """Compare the live OpenDota gold/XP advantage renderer."""

    python_size, python_pixels = rgba_pixels(python_path)
    rust_size, rust_pixels = rgba_pixels(rust_path)
    if python_size != (790, 340) or rust_size != python_size:
        raise AssertionError(
            f"advantage dimensions differ or are invalid: Python {python_size}, Rust {rust_size}"
        )
    mae, rms, exact = pixel_metrics(python_pixels, rust_pixels)
    foreground_ratio, foreground_iou = compare_foreground_structure(
        python_pixels,
        rust_pixels,
        python_size,
        grid=(8, 8),
        margin=12,
        minimum_grid_iou=0.80,
        # The native renderer uses an embedded bitmap font, so its text has
        # fewer antialiased foreground pixels than Matplotlib while the chart
        # geometry remains strongly constrained by the coarse-grid IoU gate.
        minimum_count_ratio=0.25,
        label="advantage",
    )
    print(
        f"advantage: size={python_size[0]}x{python_size[1]} "
        f"MAE={mae:.5f} RMS={rms:.5f} exact_channels={exact:.3%} "
        f"foreground_ratio={foreground_ratio:.3f} grid_IoU={foreground_iou:.3f} "
        f"python_sha={sha256(python_pixels)} rust_sha={sha256(rust_pixels)}"
    )
    if mae > 0.090 or rms > 0.220:
        raise AssertionError(
            f"advantage pixel drift exceeds threshold: MAE {mae:.5f} <= 0.09000, "
            f"RMS {rms:.5f} <= 0.22000"
        )
    return []


def check_pet(python_path: Path, rust_path: Path) -> list[str]:
    """Compare one live production pet-card attachment.

    Pet layers are authored/shared assets in Python and a native PNG decoder
    plus the same HybridPetRenderer policy in Rust.  The gate therefore
    checks exact card geometry/PNG mode through Pillow, bounded perceptual
    drift, and a coarse foreground layout/count guard against a blank or
    misregistered card.
    """

    for path in (python_path, rust_path):
        with Image.open(path) as image:
            if image.format != "PNG" or image.mode != "RGBA":
                raise AssertionError(
                    f"pet attachment must be an RGBA PNG: {path.name} "
                    f"format={image.format!r} mode={image.mode!r}"
                )
    python_size, python_pixels = rgba_pixels(python_path)
    rust_size, rust_pixels = rgba_pixels(rust_path)
    if python_size != (512, 288) or rust_size != python_size:
        raise AssertionError(
            f"pet dimensions differ or are invalid: Python {python_size}, Rust {rust_size}"
        )
    mae, rms, exact = pixel_metrics(python_pixels, rust_pixels)
    foreground_ratio, foreground_iou = compare_foreground_structure(
        python_pixels,
        rust_pixels,
        python_size,
        grid=(16, 9),
        margin=12,
        minimum_grid_iou=PET_MIN_FOREGROUND_GRID_IOU,
        minimum_count_ratio=PET_MIN_FOREGROUND_COUNT_RATIO,
        label="pet",
    )
    print(
        f"pet: size={python_size[0]}x{python_size[1]} format=RGBA PNG "
        f"MAE={mae:.5f} RMS={rms:.5f} exact_channels={exact:.3%} "
        f"foreground_ratio={foreground_ratio:.3f} grid_IoU={foreground_iou:.3f} "
        f"python_sha={sha256(python_pixels)} rust_sha={sha256(rust_pixels)}"
    )
    if not PET_MIN_FOREGROUND_COUNT_RATIO <= foreground_ratio <= PET_MAX_FOREGROUND_COUNT_RATIO:
        raise AssertionError(
            f"pet foreground count drifted: ratio {foreground_ratio:.3f} must be between "
            f"{PET_MIN_FOREGROUND_COUNT_RATIO:.3f} and {PET_MAX_FOREGROUND_COUNT_RATIO:.3f}"
        )
    if mae > PET_MAX_MAE or rms > PET_MAX_RMS:
        raise AssertionError(
            f"pet pixel drift exceeds threshold: MAE {mae:.5f} <= {PET_MAX_MAE:.5f}, "
            f"RMS {rms:.5f} <= {PET_MAX_RMS:.5f}"
        )
    return []


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", type=Path, default=DEFAULT_FIXTURE)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--target-dir", type=Path)
    parser.add_argument("--python-only", action="store_true")
    parser.add_argument("--rust-only", action="store_true")
    args = parser.parse_args(argv)
    if args.python_only and args.rust_only:
        parser.error("--python-only and --rust-only are mutually exclusive")
    return args


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    fixture = load_fixture(args.fixture.resolve())
    owns_output = args.output_dir is None
    output_dir = (
        Path(tempfile.mkdtemp(prefix="cama-visual-equivalence-"))
        if owns_output
        else args.output_dir.resolve()
    )
    output_dir.mkdir(parents=True, exist_ok=True)
    try:
        if not args.rust_only:
            render_python(fixture, output_dir, args.fixture.resolve())
        if not args.python_only:
            run_rust(args.fixture.resolve(), output_dir, args.target_dir)
        if not args.rust_only and not args.python_only:
            check_png(output_dir / "python_chart.png", output_dir / "rust_chart.png")
            check_balance(output_dir / "python_balance.png", output_dir / "rust_balance.png")
            check_png(
                output_dir / "python_rating_history.png",
                output_dir / "rust_rating_history.png",
                label="rating_history",
            )
            check_rating_distribution(
                output_dir / "python_rating_distribution.png",
                output_dir / "rust_rating_distribution.png",
            )
            check_rating_analysis_comparison(
                output_dir / "python_rating_analysis_comparison.png",
                output_dir / "rust_rating_analysis_comparison.png",
            )
            check_rating_analysis_calibration(
                output_dir / "python_rating_analysis_calibration.png",
                output_dir / "rust_rating_analysis_calibration.png",
            )
            check_rating_analysis_trend(
                output_dir / "python_rating_analysis_trend.png",
                output_dir / "rust_rating_analysis_trend.png",
            )
            check_advantage(
                output_dir / "python_advantage.png",
                output_dir / "rust_advantage.png",
            )
            check_pet(output_dir / "python_pet.png", output_dir / "rust_pet.png")
            check_wheel(output_dir / "python_wheel.gif", output_dir / "rust_wheel.gif")
            check_explosion(
                output_dir / "python_explosion.gif",
                output_dir / "rust_explosion.gif",
            )
            check_blame_luke(
                output_dir / "python_blame_luke.gif",
                output_dir / "rust_blame_luke.gif",
            )
            check_scout(
                output_dir / "python_scout.png",
                output_dir / "rust_scout.png",
                expected_rows=len(fixture["scout"]["heroes"]),
            )
            check_hero_grid(
                output_dir / "python_hero_grid.png",
                output_dir / "rust_hero_grid.png",
                expected_players=len(fixture["hero_grid"]["players"]),
                expected_heroes=5,
            )
            check_profile_role_graph(
                output_dir / "python_profile_role_graph.png",
                output_dir / "rust_profile_role_graph.png",
            )
            check_profile_lane_distribution(
                output_dir / "python_profile_lane_distribution.png",
                output_dir / "rust_profile_lane_distribution.png",
                lane_count=len(fixture["profile"]["lanes"]),
            )
            check_profile_hero_performance(
                output_dir / "python_profile_hero_performance.png",
                output_dir / "rust_profile_hero_performance.png",
                hero_count=len(fixture["profile"]["hero_performance"]),
            )
            check_profile_recent_matches(
                output_dir / "python_profile_recent_matches.png",
                output_dir / "rust_profile_recent_matches.png",
                row_count=len(fixture["profile"]["recent_matches"]),
            )
            check_profile_gamba(
                output_dir / "python_profile_gamba.png",
                output_dir / "rust_profile_gamba.png",
                fixture["gamba"],
            )
            check_wrapped_gamba(
                output_dir / "python_wrapped_gamba.png",
                output_dir / "rust_wrapped_gamba.png",
                fixture["wrapped_gamba"],
                require_native_copy=True,
            )
            check_gif(output_dir / "python_animation.gif", output_dir / "rust_animation.gif")
            check_terminal_crash(
                output_dir / "python_terminal_crash.gif",
                output_dir / "rust_terminal_crash.gif",
            )
            check_dig_neon(
                output_dir / "python_dig_terminal.gif",
                output_dir / "rust_dig_terminal.gif",
                mode="terminal",
            )
            check_dig_neon(
                output_dir / "python_dig_prestige.gif",
                output_dir / "rust_dig_prestige.gif",
                mode="prestige",
            )
            check_pinnacle(
                output_dir / "python_pinnacle_phase3.gif",
                output_dir / "rust_pinnacle_phase3.gif",
            )
        print(f"visual-equivalence artifacts: {output_dir}")
    finally:
        if owns_output:
            print("temporary artifacts retained for inspection; remove when no longer needed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
