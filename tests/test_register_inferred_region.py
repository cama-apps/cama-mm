"""Region inference at registration + the startup backfill (PlayerService)."""

from types import SimpleNamespace

import pytest

from services.player_service import PlayerService
from tests.conftest import TEST_GUILD_ID


class FakeRepo:
    """Duck-typed player repo capturing region writes."""

    def __init__(self):
        self.inferred_writes = []  # (discord_id, guild_id, region)
        self.backfill_rows = []

    def get_by_id(self, _discord_id, _guild_id):
        return None  # treat as a new registration

    def get_steam_id_owner(self, _steam_id):
        return None

    def add_steam_id(self, discord_id, steam_id, is_primary=False):
        pass

    def add(self, discord_id, discord_username, guild_id, dotabuff_url=None, steam_id=None,
            initial_mmr=None, preferred_roles=None, main_role=None, glicko_rating=None,
            glicko_rd=None, glicko_volatility=None, os_mu=None, os_sigma=None):
        pass

    def update_inferred_region(self, discord_id, guild_id, region):
        self.inferred_writes.append((discord_id, guild_id, region))

    def get_players_needing_region_backfill(self):
        return self.backfill_rows


class DummyAPI:
    """Stand-in OpenDota client; records counts lookups."""

    def __init__(self, counts=None):
        self._counts = counts
        self.counts_calls = []

    def get_player_data(self, _steam_id):
        return {"profile": {}}

    def get_player_mmr_from_data(self, _player_data):
        return 4000

    def get_player_counts(self, steam_id):
        self.counts_calls.append(steam_id)
        return self._counts


class TestRegistrationInference:
    """register_player caches an inferred region off the manual-MMR path."""

    def test_writes_inferred_region_from_counts(self, monkeypatch):
        """Registration computes and stores the inferred region from /counts."""
        api = DummyAPI(counts={"region": {"2": {"games": 50}, "1": {"games": 10}}})
        monkeypatch.setattr("services.player_service.OpenDotaAPI", lambda: api)
        repo = FakeRepo()

        PlayerService(repo).register_player(
            discord_id=1, discord_username="u", guild_id=TEST_GUILD_ID, steam_id=42
        )

        assert repo.inferred_writes == [(1, TEST_GUILD_ID, "USE")]
        assert api.counts_calls == [42]

    def test_writes_sentinel_when_no_us_games(self, monkeypatch):
        """A profile with no US play is marked NONE (checked), not left NULL."""
        api = DummyAPI(counts={"region": {"3": {"games": 200}}})
        monkeypatch.setattr("services.player_service.OpenDotaAPI", lambda: api)
        repo = FakeRepo()

        PlayerService(repo).register_player(
            discord_id=2, discord_username="u", guild_id=TEST_GUILD_ID, steam_id=42
        )

        assert repo.inferred_writes == [(2, TEST_GUILD_ID, "NONE")]

    def test_skips_inference_on_mmr_override(self, monkeypatch):
        """The manual-MMR path skips OpenDota entirely — no counts call, no write."""
        api = DummyAPI()
        monkeypatch.setattr("services.player_service.OpenDotaAPI", lambda: api)
        repo = FakeRepo()

        PlayerService(repo).register_player(
            discord_id=3, discord_username="u", guild_id=TEST_GUILD_ID,
            steam_id=42, mmr_override=3500,
        )

        assert repo.inferred_writes == []
        assert api.counts_calls == []

    def test_no_write_when_counts_unavailable(self, monkeypatch):
        """A failed/rate-limited /counts (None) leaves the row unwritten, to retry later."""
        api = DummyAPI(counts=None)
        monkeypatch.setattr("services.player_service.OpenDotaAPI", lambda: api)
        repo = FakeRepo()

        PlayerService(repo).register_player(
            discord_id=4, discord_username="u", guild_id=TEST_GUILD_ID, steam_id=42
        )

        assert api.counts_calls == [42]  # attempted
        assert repo.inferred_writes == []  # but nothing recorded


class TestBackfill:
    """backfill_inferred_regions fills NULL rows and dedupes by steam_id."""

    def test_fills_rows_and_dedupes_by_steam_id(self):
        """One /counts call per distinct steam_id; every pending row gets written."""
        api = DummyAPI(counts={"region": {"1": {"games": 7}}})  # -> USW
        repo = FakeRepo()
        # Two rows share steam_id 99 (same user across guilds); one distinct user.
        repo.backfill_rows = [
            {"discord_id": 1, "guild_id": 100, "steam_id": 99},
            {"discord_id": 1, "guild_id": 200, "steam_id": 99},
            {"discord_id": 2, "guild_id": 100, "steam_id": 77},
        ]

        updated = PlayerService(repo).backfill_inferred_regions(api=api)

        assert updated == 3
        assert api.counts_calls == [99, 77]  # 99 fetched once, not twice
        assert (1, 100, "USW") in repo.inferred_writes
        assert (1, 200, "USW") in repo.inferred_writes
        assert (2, 100, "USW") in repo.inferred_writes

    def test_skips_rows_when_counts_unavailable(self):
        """A None /counts (rate-limited) leaves the row NULL — not written, not counted."""
        api = DummyAPI(counts=None)
        repo = FakeRepo()
        repo.backfill_rows = [{"discord_id": 1, "guild_id": 100, "steam_id": 99}]

        updated = PlayerService(repo).backfill_inferred_regions(api=api)

        assert updated == 0
        assert repo.inferred_writes == []
        assert api.counts_calls == [99]  # attempted once


class RegionRepo:
    """Fake repo holding one player, for set_region / get_region_info tests."""

    def __init__(self, player):
        self._player = player
        self.preferred_writes = []

    def get_by_id(self, _discord_id, _guild_id):
        return self._player

    def update_preferred_region(self, discord_id, guild_id, region):
        self.preferred_writes.append((discord_id, guild_id, region))
        if self._player is not None:
            self._player.preferred_region = region


class TestSetRegion:
    """set_region validates the code and that the player exists before writing."""

    def test_valid_pick_persists(self):
        """A valid region code is written through to the repo."""
        repo = RegionRepo(SimpleNamespace(preferred_region=None, inferred_region=None))
        PlayerService(repo).set_region(1, TEST_GUILD_ID, "USW")
        assert repo.preferred_writes == [(1, TEST_GUILD_ID, "USW")]

    def test_invalid_code_rejected_before_write(self):
        """An unknown region code raises and never reaches the database."""
        repo = RegionRepo(SimpleNamespace(preferred_region=None, inferred_region=None))
        with pytest.raises(ValueError):
            PlayerService(repo).set_region(1, TEST_GUILD_ID, "EU")
        assert repo.preferred_writes == []

    def test_unregistered_player_rejected(self):
        """Setting a region for an unregistered player raises and writes nothing."""
        repo = RegionRepo(None)
        with pytest.raises(ValueError):
            PlayerService(repo).set_region(1, TEST_GUILD_ID, "USE")
        assert repo.preferred_writes == []


class TestGetRegionInfo:
    """get_region_info classifies the source as set / inferred / none for display."""

    def test_explicit_pick_reports_set(self):
        """An explicit pick reports source 'set' and beats any inferred value."""
        repo = RegionRepo(SimpleNamespace(preferred_region="USE", inferred_region="USW"))
        assert PlayerService(repo).get_region_info(1, TEST_GUILD_ID) == {
            "code": "USE", "name": "US East", "source": "set",
        }

    def test_inferred_only_reports_inferred(self):
        """With no explicit pick, the inferred region reports source 'inferred'."""
        repo = RegionRepo(SimpleNamespace(preferred_region=None, inferred_region="USW"))
        assert PlayerService(repo).get_region_info(1, TEST_GUILD_ID) == {
            "code": "USW", "name": "US West", "source": "inferred",
        }

    def test_sentinel_reports_none(self):
        """The NONE sentinel (checked, no US) reports source 'none' with no server."""
        repo = RegionRepo(SimpleNamespace(preferred_region=None, inferred_region="NONE"))
        assert PlayerService(repo).get_region_info(1, TEST_GUILD_ID) == {
            "code": None, "name": None, "source": "none",
        }
