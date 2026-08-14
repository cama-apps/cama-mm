#!/usr/bin/env python3
"""Cross-language visual-equivalence gate for representative media.

The normal Rust/Python unit suites stay hermetic and do not invoke one
another.  Run this explicit, dependency-aware gate from the repository root:

    uv run --locked python scripts/visual_equivalence.py

It renders one production prediction-market PNG and one production post-match
GIF from the shared JSON fixture, then decodes both sides to RGBA.  Geometry,
animation metadata, and ordered frame correspondence are checked separately
from pixel similarity.  The native Rust renderers intentionally use an
embedded bitmap font, while Python uses its configured Pillow font, so exact
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
DEFAULT_FIXTURE = ROOT / "scripts" / "visual_equivalence_fixture.json"
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
ANIMATION_MAX_MEAN_FRAME_MAE = 0.130
ANIMATION_MAX_MEAN_FRAME_RMS = 0.260
ANIMATION_MAX_FRAME_MAE = 0.260
FOREGROUND_CHANNEL_THRESHOLD = 80
CHART_MIN_FOREGROUND_GRID_IOU = 0.80
ANIMATION_MIN_FOREGROUND_GRID_IOU = 0.65
MIN_FOREGROUND_COUNT_RATIO = 0.50
MIN_FOREGROUND_PIXELS = 200


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
    if not isinstance(fixture, dict) or set(fixture) != {"chart", "animation"}:
        raise ValueError("fixture must contain exactly chart and animation objects")
    return fixture


def render_python(fixture: dict[str, Any], output_dir: Path) -> None:
    from utils.drawing.predictions import draw_market_fair_history
    from utils.neon_drawing import create_post_match_gif

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


def run_rust(fixture_path: Path, output_dir: Path, target_dir: Path | None) -> None:
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
    environment = None
    if target_dir is not None:
        import os

        environment = os.environ.copy()
        environment["CARGO_TARGET_DIR"] = str(target_dir)
    subprocess.run(command, cwd=ROOT, env=environment, check=True)


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
) -> tuple[float, float]:
    """Reject blank/border-only output hidden by a shared dark background."""

    reference_count, reference_cells = foreground_structure(reference, size, grid, margin)
    candidate_count, candidate_cells = foreground_structure(candidate, size, grid, margin)
    if reference_count == 0 or not reference_cells:
        raise AssertionError(f"{label} reference contains no foreground structure")
    minimum_count = max(MIN_FOREGROUND_PIXELS, reference_count * MIN_FOREGROUND_COUNT_RATIO)
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


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()[:16]


def check_png(python_path: Path, rust_path: Path) -> list[str]:
    python_size, python_pixels = rgba_pixels(python_path)
    rust_size, rust_pixels = rgba_pixels(rust_path)
    if python_size != rust_size:
        raise AssertionError(f"chart dimensions differ: Python {python_size}, Rust {rust_size}")
    mae, rms, exact = pixel_metrics(python_pixels, rust_pixels)
    foreground_ratio, foreground_iou = compare_foreground_structure(
        python_pixels,
        rust_pixels,
        python_size,
        grid=(max(1, python_size[0] // 20), max(1, python_size[1] // 20)),
        margin=0,
        minimum_grid_iou=CHART_MIN_FOREGROUND_GRID_IOU,
        label="chart",
    )
    print(
        f"chart: size={python_size[0]}x{python_size[1]} "
        f"MAE={mae:.5f} RMS={rms:.5f} exact_channels={exact:.3%} "
        f"foreground_ratio={foreground_ratio:.3f} grid_IoU={foreground_iou:.3f} "
        f"python_sha={sha256(python_pixels)} rust_sha={sha256(rust_pixels)}"
    )
    if mae > CHART_MAX_MAE or rms > CHART_MAX_RMS:
        raise AssertionError(
            f"chart pixel drift exceeds threshold: MAE {mae:.5f} <= {CHART_MAX_MAE:.5f}, "
            f"RMS {rms:.5f} <= {CHART_MAX_RMS:.5f}"
        )
    return []


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
            render_python(fixture, output_dir)
        if not args.python_only:
            run_rust(args.fixture.resolve(), output_dir, args.target_dir)
        if not args.rust_only and not args.python_only:
            check_png(output_dir / "python_chart.png", output_dir / "rust_chart.png")
            check_gif(output_dir / "python_animation.gif", output_dir / "rust_animation.gif")
        print(f"visual-equivalence artifacts: {output_dir}")
    finally:
        if owns_output:
            print("temporary artifacts retained for inspection; remove when no longer needed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
