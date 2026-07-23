"""Regression tests for shared-palette dig animation encoding."""

from unittest.mock import patch

import pytest
from PIL import Image

from utils import dig_drawing


def test_dig_palette_sampling_is_bounded_for_longest_animation():
    sample_width, sample_height = dig_drawing._dig_palette_sample_size(
        (dig_drawing.SCENE_WIDTH, dig_drawing.SCENE_HEIGHT),
        34,
    )

    assert (sample_width, sample_height) == (114, 64)
    assert (
        sample_width * sample_height * 34
        <= dig_drawing._DIG_GIF_PALETTE_SAMPLE_PIXEL_BUDGET
    )


def test_dig_gif_builds_one_shared_palette_and_preserves_timing():
    adaptive_builds = []
    shared_remaps = []
    original_quantize = Image.Image.quantize
    frames = [
        Image.new("RGB", (32, 18), (index * 20, index * 10, index * 5))
        for index in range(4)
    ]
    durations = [70, 80, 90, 60_000]

    def recording_quantize(image, *args, **kwargs):
        palette = kwargs.get("palette")
        if palette is None:
            adaptive_builds.append(
                (
                    kwargs.get("colors"),
                    kwargs.get("method"),
                    kwargs.get("dither"),
                )
            )
        else:
            shared_remaps.append((palette, kwargs.get("dither")))
        return original_quantize(image, *args, **kwargs)

    with (
        patch.object(Image.Image, "quantize", new=recording_quantize),
        patch.object(Image.Image, "save") as save_mock,
    ):
        dig_drawing._save_dig_gif(frames, durations)

    assert adaptive_builds == [
        (
            256,
            Image.Quantize.MEDIANCUT,
            Image.Dither.NONE,
        )
    ]
    assert len(shared_remaps) == len(frames)
    assert len({id(palette) for palette, _dither in shared_remaps}) == 1
    assert all(
        dither == Image.Dither.NONE
        for _palette, dither in shared_remaps
    )

    save_kwargs = save_mock.call_args.kwargs
    assert len(save_kwargs["append_images"]) == 3
    assert save_kwargs["duration"] == durations
    assert save_kwargs["loop"] == 1
    assert save_kwargs["optimize"] is False
    for frame in frames:
        with pytest.raises(ValueError, match="closed image"):
            frame.load()
