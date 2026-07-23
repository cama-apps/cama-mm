"""Regression tests for low-overhead wheel frame rendering."""

import math
import random
from unittest.mock import patch

import pytest
from PIL import Image, ImageChops, ImageDraw

from utils import wheel_drawing


def _draw_wedge_labels_reference(
    img: Image.Image,
    draw: ImageDraw.ImageDraw,
    size: int,
    rotation: float,
    wedges: list[tuple[str, int | str, str]],
) -> None:
    """Draw labels using the uncached implementation for pixel-parity checks."""
    center = size // 2
    radius = size // 2 - 30
    angle_per_wedge = 360 / len(wedges)
    base_font_size = max(12, size // 30)
    text_radius_frac = 0.68
    arc_width = 2 * math.pi * (radius * text_radius_frac) * (angle_per_wedge / 360)
    max_label_width = arc_width * 0.85

    for i, (label, value, _color) in enumerate(wedges):
        text = label if isinstance(value, str) or value <= 0 else str(value)
        is_shell = isinstance(value, str)

        font_size = base_font_size
        font = wheel_drawing._get_cached_font(font_size, "small", bold=True)
        bbox = draw.textbbox((0, 0), text, font=font)
        text_width = bbox[2] - bbox[0]
        text_height = bbox[3] - bbox[1]
        icon_size = int(text_height * 1.8) if is_shell else 0
        icon_gap = icon_size + 3 if is_shell else 0
        total_width = text_width + icon_gap

        while total_width > max_label_width and font_size > 8:
            font_size -= 1
            font = wheel_drawing._get_cached_font(font_size, "small", bold=True)
            bbox = draw.textbbox((0, 0), text, font=font)
            text_width = bbox[2] - bbox[0]
            text_height = bbox[3] - bbox[1]
            icon_size = int(text_height * 1.8) if is_shell else 0
            icon_gap = icon_size + 3 if is_shell else 0
            total_width = text_width + icon_gap

        mid_angle_deg = i * angle_per_wedge + rotation - 90 + angle_per_wedge / 2
        mid_angle_rad = math.radians(mid_angle_deg)
        text_radius = radius * text_radius_frac
        text_cx = center + text_radius * math.cos(mid_angle_rad)
        text_cy = center + text_radius * math.sin(mid_angle_rad)
        tx = text_cx - total_width / 2
        ty = text_cy - text_height / 2

        if is_shell:
            shell_icon = wheel_drawing._get_shell_icon(value, icon_size)
            img.paste(
                shell_icon,
                (int(tx), int(text_cy - icon_size / 2)),
                shell_icon,
            )

        text_x = tx + icon_gap
        draw.text((text_x + 1, ty + 1), text, fill="#000000", font=font)
        draw.text((text_x, ty), text, fill="#ffffff", font=font)


@pytest.mark.parametrize(
    ("wedges", "rotation"),
    [
        (wheel_drawing.WHEEL_WEDGES, 0.0),
        (wheel_drawing.BANKRUPT_WHEEL_WEDGES, 47.25),
        (wheel_drawing.GOLDEN_WHEEL_WEDGES, 271.875),
    ],
    ids=["normal", "bankrupt", "golden"],
)
def test_cached_wedge_label_layouts_are_pixel_identical(wedges, rotation):
    wheel_drawing._get_wedge_label_layouts.cache_clear()
    reference = Image.new("RGBA", (500, 500), (0, 0, 0, 0))
    optimized = reference.copy()

    _draw_wedge_labels_reference(
        reference,
        ImageDraw.Draw(reference),
        500,
        rotation,
        wedges,
    )
    wheel_drawing._draw_wedge_labels(
        optimized,
        ImageDraw.Draw(optimized),
        500,
        rotation,
        wedges=wedges,
    )

    difference = ImageChops.difference(reference, optimized).convert("RGB")
    assert difference.getbbox() is None


def test_wedge_label_layout_is_measured_once_for_all_animation_frames():
    wheel_drawing._get_wedge_label_layouts.cache_clear()
    img = Image.new("RGBA", (500, 500), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)

    for frame_idx in range(70):
        wheel_drawing._draw_wedge_labels(
            img,
            draw,
            500,
            rotation=frame_idx * 11.75,
            wedges=wheel_drawing.WHEEL_WEDGES,
        )

    cache_info = wheel_drawing._get_wedge_label_layouts.cache_info()
    assert cache_info.misses == 1
    assert cache_info.hits == 69
    assert cache_info.maxsize == 64


def test_wheel_gif_reuses_palette_seed_without_changing_timing():
    rendered_frame_indices = []
    quantize_options = []
    original_quantize = Image.Image.quantize

    def fake_frame(*_args, frame_idx=0, **_kwargs):
        rendered_frame_indices.append(frame_idx)
        return Image.new("RGBA", (16, 16), (frame_idx, frame_idx, frame_idx, 255))

    def recording_quantize(image, *args, **kwargs):
        quantize_options.append((kwargs.get("palette"), kwargs.get("dither")))
        return original_quantize(image, *args, **kwargs)

    random.seed(20260722)
    with (
        patch.object(
            wheel_drawing,
            "create_wheel_frame_for_gif",
            side_effect=fake_frame,
        ),
        patch.object(Image.Image, "quantize", new=recording_quantize),
        patch.object(Image.Image, "save") as save_mock,
    ):
        wheel_drawing.create_wheel_gif(target_idx=0, size=16)

    assert rendered_frame_indices == list(range(70))
    assert len(quantize_options) == 70
    assert len({id(palette) for palette, _dither in quantize_options}) == 1
    assert all(
        dither == Image.Dither.NONE
        for _palette, dither in quantize_options
    )
    save_kwargs = save_mock.call_args.kwargs
    assert len(save_kwargs["append_images"]) == 69
    assert len(save_kwargs["duration"]) == 70
    assert save_kwargs["duration"][:14] == [30] * 14
    assert save_kwargs["duration"][-1] == 60_000
    assert save_kwargs["loop"] == 1
    assert save_kwargs["optimize"] is False
