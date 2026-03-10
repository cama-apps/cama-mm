"""Tests for solo grinder detection via OpenDota API."""

from unittest.mock import MagicMock, patch

import pytest

from services.opendota_player_service import OpenDotaPlayerService


class TestCheckIsSoloGrinder:
    """Test check_is_solo_grinder API parsing."""

    def _make_service(self):
        player_repo = MagicMock()
        svc = OpenDotaPlayerService(player_repo)
        return svc

    def test_solo_match_found(self):
        """Returns True when a party_size=1 match is found."""
        svc = self._make_service()
        mock_response = MagicMock()
        mock_response.status_code = 200
        mock_response.json.return_value = [
            {"party_size": 2},
            {"party_size": 1},  # Solo match
            {"party_size": 3},
        ]
        with patch.object(svc.api, "_make_request", return_value=mock_response):
            assert svc.check_is_solo_grinder(12345) is True

    def test_party_only(self):
        """Returns False when all matches are party queue."""
        svc = self._make_service()
        mock_response = MagicMock()
        mock_response.status_code = 200
        mock_response.json.return_value = [
            {"party_size": 2},
            {"party_size": 3},
            {"party_size": 5},
        ]
        with patch.object(svc.api, "_make_request", return_value=mock_response):
            assert svc.check_is_solo_grinder(12345) is False

    def test_empty_history(self):
        """Returns False when no ranked matches found."""
        svc = self._make_service()
        mock_response = MagicMock()
        mock_response.status_code = 200
        mock_response.json.return_value = []
        with patch.object(svc.api, "_make_request", return_value=mock_response):
            assert svc.check_is_solo_grinder(12345) is False

    def test_api_failure_returns_false(self):
        """Returns False on API error (safe default)."""
        svc = self._make_service()
        with patch.object(svc.api, "_make_request", side_effect=Exception("API down")):
            assert svc.check_is_solo_grinder(12345) is False

    def test_api_returns_none(self):
        """Returns False when API returns None response."""
        svc = self._make_service()
        with patch.object(svc.api, "_make_request", return_value=None):
            assert svc.check_is_solo_grinder(12345) is False

    def test_missing_party_size_field(self):
        """Returns False when party_size field is missing from matches."""
        svc = self._make_service()
        mock_response = MagicMock()
        mock_response.status_code = 200
        mock_response.json.return_value = [
            {"hero_id": 1},  # No party_size
            {"hero_id": 2},
        ]
        with patch.object(svc.api, "_make_request", return_value=mock_response):
            assert svc.check_is_solo_grinder(12345) is False


class TestUpdateSoloGrinderStatus:
    """Test update_solo_grinder_status persistence."""

    def test_updates_player_repo(self):
        """Should persist grinder status via player_repo."""
        player_repo = MagicMock()
        player_repo.get_steam_id.return_value = 12345
        svc = OpenDotaPlayerService(player_repo)

        mock_response = MagicMock()
        mock_response.status_code = 200
        mock_response.json.return_value = [{"party_size": 1}]

        with patch.object(svc.api, "_make_request", return_value=mock_response):
            result = svc.update_solo_grinder_status(discord_id=999, guild_id=0)

        assert result is True
        player_repo.update_solo_grinder_status.assert_called_once()
        call_args = player_repo.update_solo_grinder_status.call_args
        assert call_args[0][0] == 999  # discord_id
        assert call_args[0][1] == 0  # guild_id
        assert call_args[0][2] is True  # is_grinder

    def test_no_steam_id_returns_false(self):
        """Should return False when player has no steam_id."""
        player_repo = MagicMock()
        player_repo.get_steam_id.return_value = None
        svc = OpenDotaPlayerService(player_repo)

        result = svc.update_solo_grinder_status(discord_id=999, guild_id=0)
        assert result is False
        player_repo.update_solo_grinder_status.assert_not_called()
