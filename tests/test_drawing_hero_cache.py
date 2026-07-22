"""Focused tests for scout hero-image memory/disk/HTTP caching."""

import threading
import time
from collections import Counter
from io import BytesIO

import pytest
from PIL import Image

import utils.drawing.heroes as heroes


@pytest.fixture(autouse=True)
def clear_memory_cache():
    heroes._hero_image_cache.clear()
    yield
    heroes._hero_image_cache.clear()


def _png_bytes(size=(96, 54), color=(12, 34, 56, 255)) -> bytes:
    output = BytesIO()
    Image.new("RGBA", size, color).save(output, format="PNG")
    return output.getvalue()


class _Response:
    def __init__(self, content: bytes):
        self.content = content

    def raise_for_status(self):
        return None


def test_fetch_persists_original_and_reloads_without_http(tmp_path, monkeypatch):
    cache_dir = tmp_path / "heroes"
    monkeypatch.setattr(heroes, "_HERO_IMAGE_DISK_CACHE_DIR", cache_dir)
    monkeypatch.setattr(
        "utils.hero_lookup.get_hero_image_url",
        lambda hero_id: f"https://cdn.test/{hero_id}.png",
    )
    calls = []

    def get(url, timeout):
        calls.append((url, timeout))
        return _Response(_png_bytes())

    monkeypatch.setattr("requests.get", get)

    first = heroes._fetch_hero_image(7, (48, 27))

    assert first is not None
    assert first.size == (48, 27)
    assert calls == [("https://cdn.test/7.png", 5)]
    assert heroes._hero_image_disk_path(7).is_file()
    assert heroes._hero_image_cache[7].size == (96, 54)

    heroes._hero_image_cache.clear()

    def unexpected_http(*_args, **_kwargs):
        raise AssertionError("disk hit performed an HTTP request")

    monkeypatch.setattr("requests.get", unexpected_http)
    second = heroes._fetch_hero_image(7, (32, 18))

    assert second is not None
    assert second.size == (32, 18)
    assert second.getpixel((0, 0)) == (12, 34, 56, 255)
    assert heroes._hero_image_cache[7].size == (96, 54)


def test_corrupt_disk_entry_is_replaced_by_valid_http_image(tmp_path, monkeypatch):
    monkeypatch.setattr(
        heroes, "_HERO_IMAGE_DISK_CACHE_DIR", tmp_path / "heroes"
    )
    path = heroes._hero_image_disk_path(9)
    path.parent.mkdir(parents=True)
    path.write_bytes(b"not an image")
    payload = _png_bytes(color=(90, 80, 70, 255))
    monkeypatch.setattr(
        "utils.hero_lookup.get_hero_image_url",
        lambda _hero_id: "https://cdn.test/9.png",
    )
    monkeypatch.setattr("requests.get", lambda *_args, **_kwargs: _Response(payload))

    image = heroes._fetch_hero_image(9)

    assert image is not None
    assert image.getpixel((0, 0)) == (90, 80, 70, 255)
    assert path.read_bytes() == payload


def test_batch_deduplicates_and_bounds_parallel_missing_fetches(monkeypatch):
    monkeypatch.setattr(heroes, "_HERO_IMAGE_FETCH_WORKERS", 2)
    heroes._hero_image_cache[99] = Image.new("RGBA", (60, 40), "white")
    lock = threading.Lock()
    calls = []
    active = 0
    max_active = 0

    def fetch(hero_id, size):
        nonlocal active, max_active
        with lock:
            calls.append(hero_id)
            active += 1
            max_active = max(max_active, active)
        time.sleep(0.03)
        with lock:
            active -= 1
        if hero_id == 3:
            return None
        return Image.new("RGBA", size, (hero_id, 0, 0, 255))

    monkeypatch.setattr(heroes, "_fetch_hero_image", fetch)

    result = heroes._get_hero_images_batch(
        [1, 2, 1, 99, 3, 2, 4], size=(24, 12)
    )

    assert Counter(calls) == Counter({1: 1, 2: 1, 3: 1, 4: 1})
    assert 1 < max_active <= 2
    assert list(result) == [1, 2, 99, 4]
    assert all(image.size == (24, 12) for image in result.values())
    assert result[1].getpixel((0, 0)) == (1, 0, 0, 255)
