"""Hermetic checks for the explicit cross-language visual gate.

These tests exercise only fixture validation, Python rendering, and metric
calculation.  The Rust subprocess is intentionally reserved for the explicit
``scripts/visual_equivalence.py`` command so the ordinary Python suite stays
network-free and does not depend on a compiled Rust target.
"""

from __future__ import annotations

from pathlib import Path

import pytest
from PIL import Image, ImageDraw

from scripts.visual_equivalence import (
    _NATIVE_CASE_GLYPHS,
    _NATIVE_GAMBA_GREY,
    _NATIVE_GAMBA_WHITE,
    _NATIVE_MIDDLE_DOT_GLYPH,
    _NATIVE_WRAPPED_GREY,
    ANIMATION_MIN_FOREGROUND_GRID_IOU,
    BALANCE_MIN_FOREGROUND_COUNT_RATIO,
    BALANCE_MIN_FOREGROUND_GRID_IOU,
    DEFAULT_FIXTURE,
    RATING_DISTRIBUTION_COLOR_DISTANCE,
    RATING_DISTRIBUTION_COLOR_VARIANTS,
    _assert_native_wrapped_gamba_copy,
    _gamba_marker_specs,
    check_blame_luke,
    check_explosion,
    check_hero_grid,
    check_pet,
    check_profile_gamba,
    check_profile_hero_performance,
    check_profile_lane_distribution,
    check_profile_recent_matches,
    check_profile_role_graph,
    check_rating_analysis_calibration,
    check_rating_analysis_comparison,
    check_rating_analysis_trend,
    check_rating_distribution,
    check_scout,
    check_wheel,
    check_wrapped_gamba,
    compare_foreground_structure,
    gif_frames,
    load_fixture,
    pixel_metrics,
    render_python,
    rgba_pixels,
)
from utils.drawing._common import (
    DISCORD_BG,
    DISCORD_GREEN,
    DISCORD_GREY,
    DISCORD_RED,
)
from utils.drawing.gamba import _draw_event_marker, _marker_radius


def test_visual_fixture_has_typed_chart_and_animation_inputs():
    fixture = load_fixture(DEFAULT_FIXTURE)
    chart = fixture["chart"]
    animation = fixture["animation"]
    pinnacle = fixture["pinnacle"]
    balance = fixture["balance"]
    gamba = fixture["gamba"]
    wrapped_gamba = fixture["wrapped_gamba"]
    rating_history = fixture["rating_history"]
    rating_distribution = fixture["rating_distribution"]
    rating_analysis = fixture["rating_analysis"]
    advantage = fixture["advantage"]
    pet = fixture["pet"]
    assert isinstance(chart["market_id"], int)
    assert isinstance(chart["snapshots"], list)
    assert all(len(snapshot) == 2 for snapshot in chart["snapshots"])
    assert isinstance(animation["name"], str)
    assert isinstance(animation["value"], int)
    assert isinstance(animation["theme"], str)
    terminal_crash = fixture["terminal_crash"]
    assert terminal_crash["name"] == "Client 47"
    assert terminal_crash["filing_number"] == 5
    assert pinnacle["source_path"].endswith("lantern_engine_encounter.png")
    assert pinnacle["boss_id"] == "lantern_engine"
    assert pinnacle["secret"] is True
    assert isinstance(balance["username"], str)
    assert isinstance(balance["series"], list)
    assert all(len(point) == 3 for point in balance["series"])
    assert isinstance(balance["source_totals"], dict)
    assert gamba["username"] == "Visual Gambler"
    assert len(gamba["series"]) == 8
    assert {point["source"] for point in gamba["series"]} == {
        "bet",
        "wheel",
        "double_or_nothing",
    }
    assert any(point["cumulative"] > 0 for point in gamba["series"])
    assert any(point["cumulative"] < 0 for point in gamba["series"])
    assert gamba["stats"]["total_bets"] == 6
    assert wrapped_gamba["title"] == "Gamba (All-Time)"
    assert wrapped_gamba["footer"] == "+60 JC · 6 bets · Degen Score: 73"
    assert wrapped_gamba["gamba"] == gamba
    assert rating_history["username"] == "Client 47"
    assert len(rating_history["entries"]) == 6
    assert any(entry["rating"] is None for entry in rating_history["entries"])
    assert any(entry["os_mu_after"] is None for entry in rating_history["entries"])
    assert rating_distribution["ratings"] == [
        1400.0,
        1500.0,
        1520.0,
        1600.0,
        1700.0,
        1450.0,
    ]
    assert rating_distribution["median_rating"] == 1510.0
    assert rating_analysis["comparison"]["matches_analyzed"] == 25
    assert rating_analysis["comparison"]["glicko"] == {
        "brier_score": 0.21,
        "accuracy": 0.64,
        "log_loss": 0.61,
    }
    assert rating_analysis["comparison"]["openskill"] == {
        "brier_score": 0.18,
        "accuracy": 0.72,
        "log_loss": 0.56,
    }
    assert rating_analysis["calibration"]["glicko"] == [
        [0.12, 0.18, 8],
        [0.27, 0.31, 12],
        [0.46, 0.42, 15],
        [0.64, 0.71, 11],
        [0.83, 0.78, 9],
    ]
    assert rating_analysis["calibration"]["openskill"][-1] == [0.86, 0.88, 8]
    assert rating_analysis["trend"]["window"] == 20
    assert len(rating_analysis["trend"]["match_data"]) == 28
    assert advantage["match_id"] == 4242
    assert len(advantage["radiant_gold_adv"]) == len(advantage["radiant_xp_adv"]) == 7
    assert pet == {
        "species_id": "common_cama",
        "stage": "adult",
        "mood": "happy",
        "seed": 7,
        "accessory": "red_bow",
        "components_path": "../assets/pets/components",
        "attachment_filename": "pet_common_cama_adult_happy.png",
        "embed_image": "attachment://pet_common_cama_adult_happy.png",
    }
    assert fixture["wheel"] == {
        "target_index": 7,
        "size": 500,
        "seed": 13280327,
        "is_bankrupt": False,
        "is_golden": False,
    }
    assert fixture["explosion"] == {"size": 500, "seed": 12648430}
    assert fixture["blame_luke"] == {"selected_index": 4}
    assert fixture["scout"]["player_count"] == 3
    assert fixture["scout"]["total_matches"] == 24
    assert fixture["scout"]["player_names"] == ["Ada", "Linus", "Grace"]
    assert fixture["scout"]["title"] == "SCOUT: Radiant"
    assert len(fixture["scout"]["heroes"]) == 3
    assert fixture["scout"]["portrait_mode"] == "cache_miss_fallback"
    assert fixture["hero_grid"]["title"] == "Hero Grid: Visual Fixture"
    assert fixture["hero_grid"]["min_games"] == 2
    assert len(fixture["hero_grid"]["players"]) == 4
    assert len(fixture["hero_grid"]["stats"]) == 18
    assert fixture["hero_grid"]["players"][0] == {"discord_id": 101, "name": "Ada Lovelace"}
    assert fixture["profile"]["username"] == "Visual Profile"
    assert fixture["profile"]["roles"] == {"Carry": 50, "Support": 30, "Nuker": 20}
    assert [(row["name"], row["value"]) for row in fixture["profile"]["lanes"]] == [
        ("Roaming", 20),
        ("Safe Lane", 30),
        ("Mid", 25),
        ("Off Lane", 15),
        ("Jungle", 10),
    ]
    assert [(row["games"], row["wins"]) for row in fixture["profile"]["hero_performance"]] == [
        (8, 5),
        (6, 3),
        (5, 2),
        (4, 1),
    ]
    assert fixture["profile"]["recent_matches"][0]["hero_name"] == "Outworld Destroyer"
    assert fixture["profile"]["recent_matches"][-1]["won"] is None
    assert fixture["profile"]["recent_matches"][-1]["duration"] is None


def test_python_fixture_render_is_deterministic_and_seekable(tmp_path: Path):
    fixture = load_fixture(DEFAULT_FIXTURE)
    first = tmp_path / "first"
    second = tmp_path / "second"
    first.mkdir()
    second.mkdir()
    render_python(fixture, first)
    render_python(fixture, second)

    assert (first / "python_chart.png").read_bytes() == (second / "python_chart.png").read_bytes()
    assert (first / "python_balance.png").read_bytes() == (
        second / "python_balance.png"
    ).read_bytes()
    assert (first / "python_rating_history.png").read_bytes() == (
        second / "python_rating_history.png"
    ).read_bytes()
    assert (first / "python_rating_distribution.png").read_bytes() == (
        second / "python_rating_distribution.png"
    ).read_bytes()
    assert (first / "python_rating_analysis_comparison.png").read_bytes() == (
        second / "python_rating_analysis_comparison.png"
    ).read_bytes()
    assert (first / "python_rating_analysis_calibration.png").read_bytes() == (
        second / "python_rating_analysis_calibration.png"
    ).read_bytes()
    assert (first / "python_rating_analysis_trend.png").read_bytes() == (
        second / "python_rating_analysis_trend.png"
    ).read_bytes()
    assert (first / "python_advantage.png").read_bytes() == (
        second / "python_advantage.png"
    ).read_bytes()
    assert (first / "python_pet.png").read_bytes() == (second / "python_pet.png").read_bytes()
    assert (first / "python_animation.gif").read_bytes() == (
        second / "python_animation.gif"
    ).read_bytes()
    assert (first / "python_terminal_crash.gif").read_bytes() == (
        second / "python_terminal_crash.gif"
    ).read_bytes()
    assert (first / "python_pinnacle_phase3.gif").read_bytes() == (
        second / "python_pinnacle_phase3.gif"
    ).read_bytes()
    assert (first / "python_wheel.gif").read_bytes() == (second / "python_wheel.gif").read_bytes()
    assert (first / "python_explosion.gif").read_bytes() == (
        second / "python_explosion.gif"
    ).read_bytes()
    assert (first / "python_blame_luke.gif").read_bytes() == (
        second / "python_blame_luke.gif"
    ).read_bytes()
    assert (first / "python_scout.png").read_bytes() == (second / "python_scout.png").read_bytes()
    assert (first / "python_hero_grid.png").read_bytes() == (
        second / "python_hero_grid.png"
    ).read_bytes()
    for filename in (
        "python_profile_role_graph.png",
        "python_profile_lane_distribution.png",
        "python_profile_hero_performance.png",
        "python_profile_recent_matches.png",
    ):
        assert (first / filename).read_bytes() == (second / filename).read_bytes()
    size, loop, durations, frames = gif_frames(first / "python_animation.gif")
    assert size == (400, 300)
    assert loop == 1
    assert len(frames) == 18
    assert durations == [80] * 17 + [60_000]
    size, loop, durations, frames = gif_frames(first / "python_terminal_crash.gif")
    assert size == (400, 300)
    assert loop == 1
    assert len(frames) == 58
    assert durations == (
        [120] * 10
        + [80, 80, 90, 90, 100, 100, 110, 110, 120, 120,
           130, 130, 140, 140, 150, 150, 160, 160, 170, 170]
        + [60] * 20
        + [1100, 300, 300, 300, 300, 600, 300, 60000]
    )
    rating_size, rating_pixels = rgba_pixels(first / "python_rating_history.png")
    assert rating_size == (700, 400)
    assert max(rating_pixels) == 255
    distribution_size, distribution_pixels = rgba_pixels(
        first / "python_rating_distribution.png"
    )
    assert distribution_size == (640, 390)
    assert max(distribution_pixels) == 255
    rust_distribution = first / "rust_rating_distribution.png"
    rust_distribution.write_bytes(
        (first / "python_rating_distribution.png").read_bytes()
    )
    check_rating_distribution(
        first / "python_rating_distribution.png", rust_distribution
    )
    rating_analysis_size, rating_analysis_pixels = rgba_pixels(
        first / "python_rating_analysis_comparison.png"
    )
    assert rating_analysis_size == (989, 413)
    assert max(rating_analysis_pixels) == 255
    rust_rating_analysis = first / "rust_rating_analysis_comparison.png"
    rust_rating_analysis.write_bytes(
        (first / "python_rating_analysis_comparison.png").read_bytes()
    )
    check_rating_analysis_comparison(
        first / "python_rating_analysis_comparison.png", rust_rating_analysis
    )
    calibration_size, calibration_pixels = rgba_pixels(
        first / "python_rating_analysis_calibration.png"
    )
    assert calibration_size == (640, 490)
    assert max(calibration_pixels) == 255
    rust_calibration = first / "rust_rating_analysis_calibration.png"
    rust_calibration.write_bytes(
        (first / "python_rating_analysis_calibration.png").read_bytes()
    )
    check_rating_analysis_calibration(
        first / "python_rating_analysis_calibration.png", rust_calibration
    )
    trend_size, trend_pixels = rgba_pixels(first / "python_rating_analysis_trend.png")
    assert trend_size == (789, 390)
    assert max(trend_pixels) == 255
    rust_trend = first / "rust_rating_analysis_trend.png"
    rust_trend.write_bytes((first / "python_rating_analysis_trend.png").read_bytes())
    check_rating_analysis_trend(first / "python_rating_analysis_trend.png", rust_trend)
    advantage_size, advantage_pixels = rgba_pixels(first / "python_advantage.png")
    assert advantage_size == (790, 340)
    assert max(advantage_pixels) == 255
    pet_size, pet_pixels = rgba_pixels(first / "python_pet.png")
    assert pet_size == (512, 288)
    assert max(pet_pixels) == 255
    rust_pet = first / "rust_pet.png"
    rust_pet.write_bytes((first / "python_pet.png").read_bytes())
    check_pet(first / "python_pet.png", rust_pet)
    size, loop, durations, frames = gif_frames(first / "python_pinnacle_phase3.gif")
    assert size == (512, 288)
    assert loop is None
    assert len(frames) == 8
    assert durations == [90] * 7 + [1_500]
    wheel_size, wheel_loop, wheel_durations, wheel_frames = gif_frames(first / "python_wheel.gif")
    assert wheel_size == (500, 500)
    assert wheel_loop == 1
    assert 68 <= len(wheel_frames) <= 70
    assert wheel_durations[:58] == [30] * 14 + [40] * 14 + [70] * 14 + [110] * 16
    assert wheel_durations[-2:] == [60_000, 60_000]
    explosion_size, explosion_loop, explosion_durations, explosion_frames = gif_frames(
        first / "python_explosion.gif"
    )
    assert explosion_size == (500, 500)
    assert explosion_loop == 1
    assert len(explosion_frames) == 56
    assert explosion_durations == (
        [50] * 14
        + [60, 70, 80, 90, 100, 110, 120, 130, 140, 150]
        + [60] * 4
        + [80] * 14
        + [100] * 13
        + [60_000]
    )
    check_wheel(first / "python_wheel.gif", first / "python_wheel.gif")
    check_explosion(first / "python_explosion.gif", first / "python_explosion.gif")
    rust_blame_luke = first / "rust_blame_luke.gif"
    rust_blame_luke.write_bytes((first / "python_blame_luke.gif").read_bytes())
    check_blame_luke(first / "python_blame_luke.gif", rust_blame_luke)
    rust_scout = first / "rust_scout.png"
    rust_scout.write_bytes((first / "python_scout.png").read_bytes())
    check_scout(first / "python_scout.png", rust_scout, expected_rows=3)
    rust_hero_grid = first / "rust_hero_grid.png"
    rust_hero_grid.write_bytes((first / "python_hero_grid.png").read_bytes())
    check_hero_grid(first / "python_hero_grid.png", rust_hero_grid, 4, 5)
    rust_profile_role = first / "rust_profile_role_graph.png"
    rust_profile_role.write_bytes((first / "python_profile_role_graph.png").read_bytes())
    check_profile_role_graph(
        first / "python_profile_role_graph.png",
        rust_profile_role,
    )
    rust_profile_lane = first / "rust_profile_lane_distribution.png"
    rust_profile_lane.write_bytes((first / "python_profile_lane_distribution.png").read_bytes())
    check_profile_lane_distribution(
        first / "python_profile_lane_distribution.png",
        rust_profile_lane,
        lane_count=5,
    )
    rust_profile_hero = first / "rust_profile_hero_performance.png"
    rust_profile_hero.write_bytes((first / "python_profile_hero_performance.png").read_bytes())
    check_profile_hero_performance(
        first / "python_profile_hero_performance.png",
        rust_profile_hero,
        hero_count=4,
    )
    rust_profile_recent = first / "rust_profile_recent_matches.png"
    rust_profile_recent.write_bytes((first / "python_profile_recent_matches.png").read_bytes())
    check_profile_recent_matches(
        first / "python_profile_recent_matches.png",
        rust_profile_recent,
        row_count=3,
    )


def test_profile_recent_duration_is_rendered_at_python_boundary(tmp_path: Path):
    """The visual driver must consume the live service field named ``duration``."""

    fixture = load_fixture(DEFAULT_FIXTURE)
    first = tmp_path / "first"
    second = tmp_path / "second"
    first.mkdir()
    second.mkdir()
    render_python(fixture, first)
    fixture["profile"]["recent_matches"][0]["duration"] = 2401
    render_python(fixture, second)
    assert (first / "python_profile_recent_matches.png").read_bytes() != (
        second / "python_profile_recent_matches.png"
    ).read_bytes()


def test_profile_gamba_fixture_renders_all_event_types_and_is_sensitive_to_stats(
    tmp_path: Path,
):
    fixture = load_fixture(DEFAULT_FIXTURE)
    first = tmp_path / "first"
    second = tmp_path / "second"
    first.mkdir()
    second.mkdir()
    render_python(fixture, first)
    fixture["gamba"]["stats"]["net_pnl"] = -123
    fixture["gamba"]["stats"]["roi"] = -0.75
    render_python(fixture, second)
    check_profile_gamba(
        first / "python_profile_gamba.png",
        first / "python_profile_gamba.png",
        fixture["gamba"],
    )
    assert (first / "python_profile_gamba.png").read_bytes() != (
        second / "python_profile_gamba.png"
    ).read_bytes()


def test_wrapped_gamba_fixture_renders_separate_story_canvas_and_footer(tmp_path: Path):
    fixture = load_fixture(DEFAULT_FIXTURE)
    first = tmp_path / "first"
    second = tmp_path / "second"
    first.mkdir()
    second.mkdir()
    render_python(fixture, first)
    check_wrapped_gamba(
        first / "python_wrapped_gamba.png",
        first / "python_wrapped_gamba.png",
        fixture["wrapped_gamba"],
    )
    fixture["wrapped_gamba"]["footer"] = "+999 JC · 6 bets · Degen Score: 73"
    render_python(fixture, second)
    assert (first / "python_wrapped_gamba.png").read_bytes() != (
        second / "python_wrapped_gamba.png"
    ).read_bytes()


def test_wrapped_gamba_native_copy_gate_rejects_uppercase_and_dot_fallbacks():
    fixture = load_fixture(DEFAULT_FIXTURE)["wrapped_gamba"]
    payload = fixture["gamba"]
    footer = fixture["footer"]
    chart_title = f"{payload['username']}'s Gamba Journey"
    subtitle = (
        f"Degen Score {payload['degen_score']}  ·  {payload['degen_title']}"
    )

    def render_native_copy(
        *,
        footer_overrides: dict[int, tuple[int, ...]] | None = None,
        subtitle_overrides: dict[int, tuple[int, ...]] | None = None,
    ) -> bytes:
        image = Image.new("RGBA", (800, 600), (1, 2, 3, 255))
        draw = ImageDraw.Draw(image)

        def paint(
            text: str,
            left: int,
            top: int,
            color: tuple[int, int, int, int],
            overrides: dict[int, tuple[int, ...]] | None = None,
        ) -> None:
            for index, character in enumerate(text):
                glyph = (overrides or {}).get(index)
                if glyph is None:
                    glyph = (
                        _NATIVE_MIDDLE_DOT_GLYPH
                        if character == "·"
                        else _NATIVE_CASE_GLYPHS.get(character)
                    )
                if glyph is None:
                    continue
                for row, bits in enumerate(glyph):
                    for column in range(5):
                        if bits & (1 << (4 - column)):
                            x = left + index * 12 + column * 2
                            y = top + row * 2
                            draw.rectangle((x, y, x + 1, y + 1), fill=color)

        paint(
            footer,
            (800 - len(footer) * 12) // 2,
            560,
            _NATIVE_WRAPPED_GREY,
            footer_overrides,
        )
        paint(chart_title, 110, 57, _NATIVE_GAMBA_WHITE)
        paint(subtitle, 110, 85, _NATIVE_GAMBA_GREY, subtitle_overrides)
        return image.tobytes()

    correct = render_native_copy()
    _assert_native_wrapped_gamba_copy(correct, (800, 600), fixture)

    footer_lowercase = footer.index("b")
    uppercase_b = (30, 17, 17, 30, 17, 17, 30)
    with pytest.raises(AssertionError, match="footer authored copy drifted"):
        _assert_native_wrapped_gamba_copy(
            render_native_copy(footer_overrides={footer_lowercase: uppercase_b}),
            (800, 600),
            fixture,
        )

    question_mark = (14, 17, 1, 2, 4, 0, 4)
    footer_dot = footer.index("·")
    with pytest.raises(AssertionError, match="footer authored copy drifted"):
        _assert_native_wrapped_gamba_copy(
            render_native_copy(footer_overrides={footer_dot: question_mark}),
            (800, 600),
            fixture,
        )

    subtitle_dot = subtitle.index("·")
    with pytest.raises(AssertionError, match="chart subtitle authored copy drifted"):
        _assert_native_wrapped_gamba_copy(
            render_native_copy(subtitle_overrides={subtitle_dot: question_mark}),
            (800, 600),
            fixture,
        )


def test_profile_gamba_gate_rejects_missing_positive_fill(tmp_path: Path):
    fixture = load_fixture(DEFAULT_FIXTURE)
    render_python(fixture, tmp_path)
    reference = tmp_path / "python_profile_gamba.png"
    with Image.open(reference) as source:
        image = source.convert("RGBA")
        pixels = list(image.get_flattened_data())
        for index, pixel in enumerate(pixels):
            x = index % image.width
            y = index // image.width
            if not (60 <= x < 674 and 88 <= y < 310):
                continue
            if (
                sum(
                    abs(channel - expected)
                    for channel, expected in zip(pixel[:3], (75, 120, 80))
                )
                <= 72
            ):
                # Preserve foreground occupancy so the independent semantic
                # fill mask, rather than the coarse blank-image guard, fails.
                pixels[index] = (88, 101, 242, 255)
        image.putdata(pixels)
        missing_fill = tmp_path / "missing_positive_fill.png"
        image.save(missing_fill)
    with pytest.raises(AssertionError, match="positive fill layer is missing"):
        check_profile_gamba(reference, missing_fill, fixture["gamba"])


def test_profile_gamba_gate_rejects_erased_and_substituted_marker_shapes(
    tmp_path: Path,
):
    fixture = load_fixture(DEFAULT_FIXTURE)
    render_python(fixture, tmp_path)
    reference = tmp_path / "python_profile_gamba.png"
    marker_specs = _gamba_marker_specs(fixture["gamba"])

    for kind in ("bet", "wheel", "leverage", "double_or_nothing"):
        for mutation, replacement in (
            ("erased", (54, 57, 63, 255)),
            ("substituted", (88, 101, 242, 255)),
        ):
            with Image.open(reference) as source:
                image = source.convert("RGBA")
                pixels = list(image.get_flattened_data())
                for center_x, center_y, _outcome in marker_specs[kind]:
                    for y in range(center_y - 7, center_y + 8):
                        for x in range(center_x - 7, center_x + 8):
                            if (x - center_x) ** 2 + (y - center_y) ** 2 <= 7**2:
                                pixels[y * image.width + x] = replacement
                image.putdata(pixels)
                candidate = tmp_path / f"{mutation}_{kind}.png"
                image.save(candidate)
            with pytest.raises(AssertionError, match=rf"{kind} marker"):
                check_profile_gamba(reference, candidate, fixture["gamba"])


def test_profile_gamba_gate_rejects_every_cross_kind_marker_substitution(
    tmp_path: Path,
):
    fixture = load_fixture(DEFAULT_FIXTURE)
    render_python(fixture, tmp_path)
    reference = tmp_path / "python_profile_gamba.png"
    marker_specs = _gamba_marker_specs(fixture["gamba"])
    replacement_infos = {
        "bet": {"source": "bet", "leverage": 1},
        "wheel": {"source": "wheel", "leverage": 1},
        "leverage": {"source": "bet", "leverage": 2},
        "double_or_nothing": {"source": "double_or_nothing", "leverage": 1},
    }
    outcome_colors = {
        "won": DISCORD_GREEN,
        "lost": DISCORD_RED,
        "neutral": DISCORD_GREY,
    }

    for target_kind, specs in marker_specs.items():
        for replacement_kind, replacement_info in replacement_infos.items():
            if target_kind == replacement_kind:
                continue
            with Image.open(reference) as source:
                image = source.convert("RGBA")
                draw = ImageDraw.Draw(image)
                for center_x, center_y, outcome in specs:
                    draw.ellipse(
                        (center_x - 7, center_y - 7, center_x + 7, center_y + 7),
                        fill=DISCORD_BG,
                    )
                    _draw_event_marker(
                        draw,
                        (center_x, center_y),
                        replacement_info,
                        outcome_colors[outcome],
                        _marker_radius(replacement_info),
                    )
                candidate = tmp_path / f"{target_kind}_as_{replacement_kind}.png"
                image.save(candidate)
            with pytest.raises(AssertionError, match=rf"{target_kind} marker"):
                check_profile_gamba(reference, candidate, fixture["gamba"])


def test_rating_distribution_median_is_used_at_python_boundary(tmp_path: Path):
    fixture = load_fixture(DEFAULT_FIXTURE)
    first = tmp_path / "first"
    second = tmp_path / "second"
    first.mkdir()
    second.mkdir()
    render_python(fixture, first)
    fixture["rating_distribution"]["median_rating"] = 1650.0
    render_python(fixture, second)
    assert (first / "python_rating_distribution.png").read_bytes() != (
        second / "python_rating_distribution.png"
    ).read_bytes()


def test_rating_distribution_gate_rejects_missing_median_and_wrong_geometry(
    tmp_path: Path,
):
    fixture = load_fixture(DEFAULT_FIXTURE)
    render_python(fixture, tmp_path)
    reference = tmp_path / "python_rating_distribution.png"

    with Image.open(reference) as source:
        image = source.convert("RGBA")
        median_variants = RATING_DISTRIBUTION_COLOR_VARIANTS["median"]
        mean_variants = RATING_DISTRIBUTION_COLOR_VARIANTS["mean"]
        image.putdata(
            [
                (47, 49, 54, 255)
                if min(
                    sum(
                        abs(channel - expected)
                        for channel, expected in zip(pixel[:3], median)
                    )
                    for median in median_variants
                )
                <= RATING_DISTRIBUTION_COLOR_DISTANCE
                and min(
                    sum(
                        abs(channel - expected)
                        for channel, expected in zip(pixel[:3], mean)
                    )
                    for mean in mean_variants
                )
                > RATING_DISTRIBUTION_COLOR_DISTANCE
                else pixel
                for pixel in image.get_flattened_data()
            ]
        )
        missing_median = tmp_path / "missing_median.png"
        image.save(missing_median)

    with pytest.raises(AssertionError, match=r"median (?:is missing|layout drifted)"):
        check_rating_distribution(reference, missing_median)

    with Image.open(reference) as source:
        wrong_geometry = tmp_path / "wrong_geometry.png"
        source.resize((639, 390)).save(wrong_geometry)
    with pytest.raises(AssertionError, match="dimensions differ"):
        check_rating_distribution(reference, wrong_geometry)


def test_pixel_metrics_are_normalized_and_exact_for_identical_rgba():
    assert pixel_metrics(bytes([0, 10, 20, 255]), bytes([0, 10, 20, 255])) == (0.0, 0.0, 1.0)
    mae, rms, exact = pixel_metrics(bytes([0, 0, 0, 255]), bytes([255, 0, 0, 255]))
    assert mae == 0.25
    assert rms == 0.5
    assert exact == 0.75


def test_foreground_gate_rejects_contentless_animation(tmp_path: Path):
    fixture = load_fixture(DEFAULT_FIXTURE)
    render_python(fixture, tmp_path)
    size, _, _, frames = gif_frames(tmp_path / "python_animation.gif")
    blank = bytes((5, 5, 8, 255)) * (size[0] * size[1])

    with pytest.raises(AssertionError, match="foreground is missing"):
        compare_foreground_structure(
            frames[0],
            blank,
            size,
            grid=(10, 10),
            margin=24,
            minimum_grid_iou=ANIMATION_MIN_FOREGROUND_GRID_IOU,
            label="blank regression frame",
        )


def test_balance_gate_rejects_contentless_candidate(tmp_path: Path):
    fixture = load_fixture(DEFAULT_FIXTURE)
    render_python(fixture, tmp_path)
    size, reference = rgba_pixels(tmp_path / "python_balance.png")
    blank = bytes((5, 5, 8, 255)) * (size[0] * size[1])
    border = bytearray(blank)
    width, height = size
    for y in range(height):
        for x in range(width):
            if x < 3 or x >= width - 3 or y < 3 or y >= height - 3:
                offset = (y * width + x) * 4
                border[offset : offset + 4] = bytes((255, 255, 255, 255))

    for candidate, label in ((blank, "blank"), (bytes(border), "border-only")):
        with pytest.raises(AssertionError, match="foreground is missing"):
            compare_foreground_structure(
                reference,
                candidate,
                size,
                grid=(10, 10),
                margin=24,
                minimum_grid_iou=BALANCE_MIN_FOREGROUND_GRID_IOU,
                minimum_count_ratio=BALANCE_MIN_FOREGROUND_COUNT_RATIO,
                label=f"{label} balance regression",
            )
