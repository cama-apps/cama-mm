"""Import-boundary regression tests for drawing utilities."""

import subprocess
import sys
from pathlib import Path

from PIL import Image

from utils.drawing import (
    draw_calibration_curve,
    draw_rating_comparison_chart,
    draw_rating_distribution,
)


def test_import_drawing_does_not_load_scientific_stack():
    """Importing the drawing facade must not eagerly load heavy chart libraries."""
    repo_root = Path(__file__).resolve().parents[1]
    script = """
import sys

import utils.drawing

heavy_packages = ("matplotlib", "numpy", "scipy")
loaded = sorted(
    name
    for name in sys.modules
    if any(name == package or name.startswith(f"{package}.") for package in heavy_packages)
)
assert not loaded, loaded
"""

    completed = subprocess.run(
        [sys.executable, "-c", script],
        cwd=repo_root,
        capture_output=True,
        text=True,
        check=False,
        timeout=30,
    )

    assert completed.returncode == 0, (
        "Fresh import of utils.drawing loaded scientific packages:\n"
        f"{completed.stderr or completed.stdout}"
    )


def test_lazy_rating_plotters_render_pngs():
    """Deferred scientific imports must still support first-use chart rendering."""
    charts = [
        draw_rating_distribution(
            [1200, 1350, 1475, 1600, 1725, 1900], median_rating=1537.5
        ),
        draw_calibration_curve(
            [(0.4, 0.35, 12), (0.6, 0.65, 12)],
            [(0.4, 0.42, 12), (0.6, 0.58, 12)],
        ),
        draw_rating_comparison_chart(
            {
                "matches_analyzed": 24,
                "glicko": {
                    "brier_score": 0.21,
                    "accuracy": 0.62,
                    "log_loss": 0.64,
                },
                "openskill": {
                    "brier_score": 0.19,
                    "accuracy": 0.67,
                    "log_loss": 0.59,
                },
            }
        ),
    ]

    for chart in charts:
        with Image.open(chart) as image:
            assert image.format == "PNG"
            assert image.width > 0
            assert image.height > 0
