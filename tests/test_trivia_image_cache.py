"""Tests for services/trivia_image_cache.py — disk cache for trivia images."""

import os
import tempfile
from pathlib import Path
from unittest.mock import MagicMock, patch

import discord
import pytest

from services.trivia_image_cache import _url_to_path, ensure_cached, get_trivia_image


class TestUrlToPath:
    def test_hero_url(self):
        url = "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes/antimage.png"
        path = _url_to_path(url)
        assert path.name == "antimage.png"
        assert "heroes" in str(path)

    def test_ability_url(self):
        url = "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/abilities/antimage_mana_break.png"
        path = _url_to_path(url)
        assert path.name == "antimage_mana_break.png"
        assert "abilities" in str(path)

    def test_item_url(self):
        url = "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/items/blink.png"
        path = _url_to_path(url)
        assert path.name == "blink.png"
        assert "items" in str(path)

    def test_unknown_url_uses_hash(self):
        url = "https://example.com/some/random/image.png"
        path = _url_to_path(url)
        assert "other" in str(path)
        assert path.suffix == ".png"


class TestEnsureCached:
    def test_returns_existing_path(self, tmp_path):
        url = "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes/test_hero.png"
        with patch("services.trivia_image_cache.CACHE_DIR", tmp_path / "cache"):
            # Pre-create the file
            path = _url_to_path(url)
            # _url_to_path uses CACHE_DIR which is now patched
            with patch("services.trivia_image_cache._url_to_path") as mock_utp:
                cached_path = tmp_path / "cache" / "heroes" / "test_hero.png"
                cached_path.parent.mkdir(parents=True, exist_ok=True)
                cached_path.write_bytes(b"\x89PNG fake image data")
                mock_utp.return_value = cached_path

                result = ensure_cached(url)
                assert result == cached_path

    def test_downloads_on_miss(self, tmp_path):
        url = "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes/new_hero.png"
        cached_path = tmp_path / "cache" / "heroes" / "new_hero.png"

        mock_resp = MagicMock()
        mock_resp.content = b"\x89PNG fake"
        mock_resp.raise_for_status = MagicMock()

        with patch("services.trivia_image_cache._url_to_path", return_value=cached_path):
            with patch("services.trivia_image_cache.requests.get", return_value=mock_resp):
                result = ensure_cached(url)
                assert result == cached_path
                assert cached_path.exists()
                assert cached_path.read_bytes() == b"\x89PNG fake"

    def test_returns_none_on_failure(self, tmp_path):
        url = "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes/bad.png"
        cached_path = tmp_path / "cache" / "heroes" / "bad.png"

        with patch("services.trivia_image_cache._url_to_path", return_value=cached_path):
            with patch("services.trivia_image_cache.requests.get", side_effect=Exception("timeout")):
                result = ensure_cached(url)
                assert result is None


class TestGetTriviaImage:
    def test_returns_file_from_cache(self, tmp_path):
        url = "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes/cached.png"
        cached_path = tmp_path / "heroes" / "cached.png"
        cached_path.parent.mkdir(parents=True, exist_ok=True)
        cached_path.write_bytes(b"\x89PNG data")

        with patch("services.trivia_image_cache._url_to_path", return_value=cached_path):
            result = get_trivia_image(url)
            assert result is not None
            assert isinstance(result, discord.File)
            assert result.filename == "cached.png"

    def test_returns_none_when_not_cached(self, tmp_path):
        url = "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes/missing.png"
        cached_path = tmp_path / "heroes" / "missing.png"

        with patch("services.trivia_image_cache._url_to_path", return_value=cached_path):
            with patch("services.trivia_image_cache.ensure_cached", return_value=None):
                result = get_trivia_image(url)
                assert result is None
