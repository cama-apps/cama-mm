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
    DEFAULT_FIXTURE,
    compare_foreground_structure,
    gif_frames,
    load_fixture,
    pixel_metrics,
    render_python,
)


def test_visual_fixture_has_typed_chart_and_animation_inputs():
    fixture = load_fixture(DEFAULT_FIXTURE)
    chart = fixture["chart"]
    animation = fixture["animation"]
    assert isinstance(chart["market_id"], int)
    assert isinstance(chart["snapshots"], list)
    assert all(len(snapshot) == 2 for snapshot in chart["snapshots"])
    assert isinstance(animation["name"], str)
    assert isinstance(animation["value"], int)
    assert isinstance(animation["theme"], str)


def test_python_fixture_render_is_deterministic_and_seekable(tmp_path: Path):
    fixture = load_fixture(DEFAULT_FIXTURE)
    first = tmp_path / "first"
    second = tmp_path / "second"
    first.mkdir()
    second.mkdir()
    render_python(fixture, first)
    render_python(fixture, second)

    assert (first / "python_chart.png").read_bytes() == (second / "python_chart.png").read_bytes()
    assert (first / "python_animation.gif").read_bytes() == (
        second / "python_animation.gif"
    ).read_bytes()
    size, loop, durations, frames = gif_frames(first / "python_animation.gif")
    assert size == (400, 300)
    assert loop == 1
    assert len(frames) == 18
    assert durations == [80] * 17 + [60_000]


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
