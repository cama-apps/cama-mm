#!/usr/bin/env python3
"""Cross-language visual-equivalence gate for representative media.

The normal Rust/Python unit suites stay hermetic and do not invoke one
another.  Run this explicit, dependency-aware gate from the repository root:

    uv run --locked python scripts/visual_equivalence.py

It renders production prediction-market, balance-journey, rating-history,
OpenDota advantage, betting wheel/explosion, Blame Luke, post-match,
terminal-crash, and pinnacle phase-3 artifacts from the shared JSON fixture, then decodes both
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
ANIMATION_MIN_FOREGROUND_GRID_IOU = 0.65
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
        "pinnacle",
        "balance",
        "rating_history",
        "advantage",
        "wheel",
        "explosion",
        "blame_luke",
    }:
        raise ValueError(
            "fixture must contain exactly chart, animation, terminal_crash, pinnacle, balance, rating_history, advantage, wheel, explosion, and blame_luke objects"
        )
    return fixture


def render_python(
    fixture: dict[str, Any],
    output_dir: Path,
    fixture_path: Path | None = None,
) -> None:
    from utils import dig_drawing
    from utils.drawing import draw_advantage_graph, draw_balance_chart, draw_rating_history_chart
    from utils.drawing.predictions import draw_market_fair_history
    from utils.neon_drawing import create_post_match_gif, create_terminal_crash_gif
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

    rating_history = fixture["rating_history"]
    rating_history_bytes = draw_rating_history_chart(
        str(rating_history["username"]),
        list(rating_history["entries"]),
    ).getvalue()
    (output_dir / "python_rating_history.png").write_bytes(rating_history_bytes)

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
    environment = None
    if target_dir is not None:
        import os

        environment = os.environ.copy()
        environment["CARGO_TARGET_DIR"] = str(target_dir)

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
            check_advantage(
                output_dir / "python_advantage.png",
                output_dir / "rust_advantage.png",
            )
            check_wheel(output_dir / "python_wheel.gif", output_dir / "rust_wheel.gif")
            check_explosion(
                output_dir / "python_explosion.gif",
                output_dir / "rust_explosion.gif",
            )
            check_blame_luke(
                output_dir / "python_blame_luke.gif",
                output_dir / "rust_blame_luke.gif",
            )
            check_gif(output_dir / "python_animation.gif", output_dir / "rust_animation.gif")
            check_terminal_crash(
                output_dir / "python_terminal_crash.gif",
                output_dir / "rust_terminal_crash.gif",
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
