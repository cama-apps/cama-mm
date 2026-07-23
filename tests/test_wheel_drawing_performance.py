"""Regression tests for low-overhead wheel frame rendering."""

import math
import random
from unittest.mock import patch

import pytest
from PIL import Image, ImageChops, ImageDraw, ImageStat

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
def test_cached_wedge_label_sprites_stay_within_visual_bound(wedges, rotation):
    wheel_drawing._get_wedge_label_layouts.cache_clear()
    wheel_drawing._get_wedge_label_sprite.cache_clear()
    reference = Image.new("RGBA", (500, 500), (30, 30, 35, 255))
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
    difference_stats = ImageStat.Stat(difference)
    mean_absolute_error = sum(difference_stats.mean) / 3
    root_mean_square_error = (
        sum(value * value for value in difference_stats.rms) / 3
    ) ** 0.5
    assert mean_absolute_error < 0.6
    assert root_mean_square_error < 9.0


def test_wedge_label_sprite_cache_stays_within_decoded_gif_visual_bound():
    def draw_reference(
        image,
        draw,
        size,
        rotation,
        is_bankrupt=False,
        is_golden=False,
        wedges=None,
    ):
        wedge_rows = (
            wedges
            if wedges is not None
            else wheel_drawing.get_wheel_wedges(is_bankrupt, is_golden)
        )
        _draw_wedge_labels_reference(
            image,
            draw,
            size,
            rotation,
            wedge_rows,
        )

    # Prime random rain-column data before resetting the matched render seed.
    wheel_drawing._get_rain_columns(500)
    random.seed(99602)
    with patch.object(
        wheel_drawing,
        "_draw_wedge_labels",
        side_effect=draw_reference,
    ):
        reference = wheel_drawing.create_wheel_gif(
            target_idx=7,
            size=500,
            display_name="Visual Bound",
        )

    wheel_drawing._get_wedge_label_sprite.cache_clear()
    random.seed(99602)
    optimized = wheel_drawing.create_wheel_gif(
        target_idx=7,
        size=500,
        display_name="Visual Bound",
    )
    assert len(optimized.getbuffer()) < 4 * 1024 * 1024

    def decode(buffer):
        with Image.open(buffer) as gif:
            frames = []
            durations = []
            for frame_index in range(gif.n_frames):
                gif.seek(frame_index)
                frames.append(gif.convert("RGB"))
                durations.append(gif.info["duration"])
        return frames, durations

    reference_frames, reference_durations = decode(reference)
    optimized_frames, optimized_durations = decode(optimized)
    assert optimized_durations == reference_durations
    assert len(optimized_frames) == len(reference_frames) == 70

    frame_errors = []
    for reference_frame, optimized_frame in zip(
        reference_frames,
        optimized_frames,
        strict=True,
    ):
        difference = ImageChops.difference(reference_frame, optimized_frame)
        frame_errors.append(sum(ImageStat.Stat(difference).mean) / 3)

    assert sum(frame_errors) / len(frame_errors) < 1.0
    assert max(frame_errors) < 1.2


def test_wedge_label_layout_is_measured_once_for_all_animation_frames():
    wheel_drawing._get_wedge_label_layouts.cache_clear()
    wheel_drawing._get_wedge_label_sprite.cache_clear()
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


def test_wedge_label_sprite_cache_hits_for_repeated_positions():
    wheel_drawing._get_wedge_label_sprite.cache_clear()
    image = Image.new("RGBA", (500, 500), (30, 30, 35, 255))

    wheel_drawing._draw_wedge_labels(
        image,
        ImageDraw.Draw(image),
        500,
        rotation=33.75,
        wedges=wheel_drawing.WHEEL_WEDGES,
    )
    first = wheel_drawing._get_wedge_label_sprite.cache_info()
    wheel_drawing._draw_wedge_labels(
        image,
        ImageDraw.Draw(image),
        500,
        rotation=33.75,
        wedges=wheel_drawing.WHEEL_WEDGES,
    )
    second = wheel_drawing._get_wedge_label_sprite.cache_info()

    assert first.misses > 0
    assert second.misses == first.misses
    assert second.hits - first.hits == len(wheel_drawing.WHEEL_WEDGES)
    assert second.maxsize == wheel_drawing._WEDGE_LABEL_SPRITE_CACHE_SIZE


def test_wedge_label_sprite_cache_is_custom_safe_and_bounded():
    wheel_drawing._get_wedge_label_sprite.cache_clear()
    custom_a = [("ALPHA", 0, "#123456"), ("BETA", 0, "#654321")]
    custom_b = [("OMEGA", 0, "#123456"), ("BETA", 0, "#654321")]
    first = Image.new("RGBA", (500, 500), (30, 30, 35, 255))
    second = first.copy()

    wheel_drawing._draw_wedge_labels(
        first,
        ImageDraw.Draw(first),
        500,
        rotation=0,
        wedges=custom_a,
    )
    wheel_drawing._draw_wedge_labels(
        second,
        ImageDraw.Draw(second),
        500,
        rotation=0,
        wedges=custom_b,
    )

    assert (
        ImageChops.difference(first, second).convert("RGB").getbbox()
        is not None
    )
    first_key = tuple(custom_a)
    second_key = tuple(custom_b)
    first_layout = wheel_drawing._get_wedge_label_layouts(500, first_key)[0]
    second_layout = wheel_drawing._get_wedge_label_layouts(500, second_key)[0]
    first_sprite = wheel_drawing._get_wedge_label_sprite(
        500,
        first_key,
        first_layout,
        0,
        0,
        wheel_drawing._WEDGE_LABEL_TEXT_STYLE,
    )[0]
    second_sprite = wheel_drawing._get_wedge_label_sprite(
        500,
        second_key,
        second_layout,
        0,
        0,
        wheel_drawing._WEDGE_LABEL_TEXT_STYLE,
    )[0]
    assert (first_sprite.size, first_sprite.tobytes()) != (
        second_sprite.size,
        second_sprite.tobytes(),
    )

    for custom_index in range(
        wheel_drawing._WEDGE_LABEL_SPRITE_CACHE_SIZE + 32
    ):
        wedge_key = ((f"CUSTOM {custom_index}", 0, "#123456"),)
        layout = wheel_drawing._get_wedge_label_layouts(500, wedge_key)[0]
        wheel_drawing._get_wedge_label_sprite(
            500,
            wedge_key,
            layout,
            custom_index % wheel_drawing._WEDGE_LABEL_PHASE_STEPS,
            (custom_index // wheel_drawing._WEDGE_LABEL_PHASE_STEPS)
            % wheel_drawing._WEDGE_LABEL_PHASE_STEPS,
            wheel_drawing._WEDGE_LABEL_TEXT_STYLE,
        )

    cache_info = wheel_drawing._get_wedge_label_sprite.cache_info()
    assert cache_info.currsize == wheel_drawing._WEDGE_LABEL_SPRITE_CACHE_SIZE


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


def test_explosion_palette_sampling_is_bounded_at_production_size():
    sample_width, sample_height = wheel_drawing._explosion_palette_sample_size(
        (500, 500),
        56,
    )

    assert (sample_width, sample_height) == (94, 94)
    assert (
        sample_width * sample_height * 56
        <= wheel_drawing._EXPLOSION_PALETTE_SAMPLE_PIXEL_BUDGET
    )


def test_explosion_gif_builds_one_shared_palette_without_changing_timing():
    adaptive_builds = []
    shared_remaps = []
    closed_indexed_frame_ids = []
    original_quantize = Image.Image.quantize
    original_close = Image.Image.close

    def fake_frame(size, *_args, frame_idx=0, **_kwargs):
        return Image.new(
            "RGBA",
            (size, size),
            (frame_idx, frame_idx * 2, frame_idx * 3, 255),
        )

    def recording_quantize(image, *args, **kwargs):
        palette = kwargs.get("palette")
        if palette is None:
            adaptive_builds.append(
                (
                    image.size,
                    kwargs.get("colors"),
                    kwargs.get("method"),
                    kwargs.get("dither"),
                )
            )
        else:
            shared_remaps.append((palette, kwargs.get("dither")))
        return original_quantize(image, *args, **kwargs)

    def recording_close(image):
        if image.mode == "P" and image.size == (64, 64):
            closed_indexed_frame_ids.append(id(image))
        return original_close(image)

    random.seed(20260722)
    with (
        patch.object(
            wheel_drawing,
            "create_wheel_frame_for_gif",
            side_effect=fake_frame,
        ),
        patch.object(Image.Image, "quantize", new=recording_quantize),
        patch.object(Image.Image, "close", new=recording_close),
        patch.object(Image.Image, "save") as save_mock,
    ):
        wheel_drawing.create_explosion_gif(size=64, display_name="Palette Test")

    assert adaptive_builds == [
        (
            (64, 64 * 56),
            256,
            Image.Quantize.FASTOCTREE,
            Image.Dither.NONE,
        )
    ]
    assert len(shared_remaps) == 56
    assert len({id(palette) for palette, _dither in shared_remaps}) == 1
    assert len(set(closed_indexed_frame_ids)) == 56
    assert all(
        dither == Image.Dither.NONE
        for _palette, dither in shared_remaps
    )

    save_kwargs = save_mock.call_args.kwargs
    expected_durations = (
        [50] * 14
        + list(range(60, 160, 10))
        + [60] * 4
        + [80] * 14
        + [100] * 13
        + [60_000]
    )
    assert len(save_kwargs["append_images"]) == 55
    assert save_kwargs["duration"] == expected_durations
    assert save_kwargs["loop"] == 1
    assert save_kwargs["optimize"] is False


@pytest.mark.parametrize("seed", [20260722, 20260723, 20260724])
def test_explosion_gif_stays_under_discord_upload_limit(seed):
    random.seed(seed)
    output = wheel_drawing.create_explosion_gif(
        size=500,
        display_name="Upload Limit Test",
    )

    assert len(output.getbuffer()) < 4 * 1024 * 1024
    with Image.open(output) as gif:
        assert gif.n_frames == 56
        durations = []
        for frame_index in range(gif.n_frames):
            gif.seek(frame_index)
            durations.append(gif.info["duration"])

    assert durations == (
        [50] * 14
        + list(range(60, 160, 10))
        + [60] * 4
        + [80] * 14
        + [100] * 13
        + [60_000]
    )
