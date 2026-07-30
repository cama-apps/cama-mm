import io

import pytest
from PIL import Image

from utils.blame_luke_drawing import BLAME_LUKE_REASONS, create_blame_luke_gif


def test_blame_luke_gif_scans_every_reason_and_holds_on_winner():
    selected = BLAME_LUKE_REASONS[4]

    buffer = create_blame_luke_gif(selected)

    assert isinstance(buffer, io.BytesIO)
    assert buffer.tell() == 0
    with Image.open(buffer) as image:
        assert image.format == "GIF"
        assert image.size == (720, 420)
        assert image.n_frames == len(BLAME_LUKE_REASONS) + 4
        assert "loop" not in image.info
        image.seek(image.n_frames - 1)
        assert image.info["duration"] == 8_000


def test_blame_luke_gif_rejects_reason_outside_candidate_list():
    with pytest.raises(ValueError, match="present in reasons"):
        create_blame_luke_gif("It was obviously Dave.", reasons=("Luke did it.",))
