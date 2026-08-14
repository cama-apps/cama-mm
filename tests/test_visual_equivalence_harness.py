"""Hermetic checks for the explicit cross-language visual gate.

These tests exercise only fixture validation, Python rendering, and metric
calculation.  The Rust subprocess is intentionally reserved for the explicit
``scripts/visual_equivalence.py`` command so the ordinary Python suite stays
network-free and does not depend on a compiled Rust target.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from scripts.visual_equivalence import (
    ANIMATION_MIN_FOREGROUND_GRID_IOU,
    BALANCE_MIN_FOREGROUND_COUNT_RATIO,
    BALANCE_MIN_FOREGROUND_GRID_IOU,
    DEFAULT_FIXTURE,
    compare_foreground_structure,
    gif_frames,
    load_fixture,
    pixel_metrics,
    render_python,
    rgba_pixels,
)


def test_visual_fixture_has_typed_chart_and_animation_inputs():
    fixture = load_fixture(DEFAULT_FIXTURE)
    chart = fixture["chart"]
    animation = fixture["animation"]
    pinnacle = fixture["pinnacle"]
    balance = fixture["balance"]
    rating_history = fixture["rating_history"]
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
    assert rating_history["username"] == "Client 47"
    assert len(rating_history["entries"]) == 6
    assert any(entry["rating"] is None for entry in rating_history["entries"])
    assert any(entry["os_mu_after"] is None for entry in rating_history["entries"])


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
    assert (first / "python_animation.gif").read_bytes() == (
        second / "python_animation.gif"
    ).read_bytes()
    assert (first / "python_terminal_crash.gif").read_bytes() == (
        second / "python_terminal_crash.gif"
    ).read_bytes()
    assert (first / "python_pinnacle_phase3.gif").read_bytes() == (
        second / "python_pinnacle_phase3.gif"
    ).read_bytes()
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
    size, loop, durations, frames = gif_frames(first / "python_pinnacle_phase3.gif")
    assert size == (512, 288)
    assert loop is None
    assert len(frames) == 8
    assert durations == [90] * 7 + [1_500]


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
