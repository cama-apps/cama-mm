"""Regression tests for two /dig display surfaces."""

from __future__ import annotations

import pytest

from repositories.dig_repository import DigRepository
from services.dig_constants import format_relic_label
from services.dig_service import DigService


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


def _seed_tunnel(
    dig_repo: DigRepository,
    discord_id: int,
    guild_id: int,
    *,
    depth: int,
    prestige_level: int = 0,
) -> None:
    dig_repo.create_tunnel(discord_id, guild_id, tunnel_name=f"T{discord_id}")
    dig_repo.update_tunnel(
        discord_id, guild_id, depth=depth, prestige_level=prestige_level,
    )


class TestLeaderboardPrestigeOrder:
    def test_higher_prestige_outranks_higher_depth(self, dig_repo, guild_id):
        _seed_tunnel(dig_repo, 1, guild_id, depth=250, prestige_level=0)
        _seed_tunnel(dig_repo, 2, guild_id, depth=100, prestige_level=3)
        _seed_tunnel(dig_repo, 3, guild_id, depth=300, prestige_level=1)

        top = dig_repo.get_top_tunnels(guild_id, limit=10)

        assert [t["discord_id"] for t in top] == [2, 3, 1]

    def test_depth_breaks_ties_at_same_prestige(self, dig_repo, guild_id):
        _seed_tunnel(dig_repo, 1, guild_id, depth=50, prestige_level=2)
        _seed_tunnel(dig_repo, 2, guild_id, depth=200, prestige_level=2)
        _seed_tunnel(dig_repo, 3, guild_id, depth=125, prestige_level=2)

        top = dig_repo.get_top_tunnels(guild_id, limit=10)

        assert [t["discord_id"] for t in top] == [2, 3, 1]

    def test_player_rank_matches_top_tunnels(self, dig_repo, guild_id):
        _seed_tunnel(dig_repo, 1, guild_id, depth=250, prestige_level=0)
        _seed_tunnel(dig_repo, 2, guild_id, depth=100, prestige_level=3)
        _seed_tunnel(dig_repo, 3, guild_id, depth=300, prestige_level=1)

        assert dig_repo.get_player_rank(2, guild_id) == 1
        assert dig_repo.get_player_rank(3, guild_id) == 2
        assert dig_repo.get_player_rank(1, guild_id) == 3

    def test_full_ties_break_deterministically_by_discord_id(self, dig_repo, guild_id):
        # Two tunnels with identical (prestige, depth) must surface in a
        # stable order, and get_player_rank must report positions that
        # match that order — never both "rank 1".
        _seed_tunnel(dig_repo, 5, guild_id, depth=120, prestige_level=2)
        _seed_tunnel(dig_repo, 7, guild_id, depth=120, prestige_level=2)
        _seed_tunnel(dig_repo, 9, guild_id, depth=120, prestige_level=2)

        top = dig_repo.get_top_tunnels(guild_id, limit=10)
        assert [t["discord_id"] for t in top] == [5, 7, 9]

        assert dig_repo.get_player_rank(5, guild_id) == 1
        assert dig_repo.get_player_rank(7, guild_id) == 2
        assert dig_repo.get_player_rank(9, guild_id) == 3


class TestDigInfoRelicShape:
    def test_equipped_relics_carry_artifact_id(
        self, dig_repo, dig_service, player_repository, guild_id,
    ):
        discord_id = 99001
        player_repository.add(
            discord_id=discord_id,
            discord_username="RelicWearer",
            guild_id=guild_id,
            initial_mmr=3000,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        dig_repo.create_tunnel(discord_id, guild_id, tunnel_name="Vault")

        for relic_id in ("mole_claws", "crystal_compass"):
            db_id = dig_repo.add_artifact(
                discord_id, guild_id, relic_id, is_relic=True,
            )
            dig_repo.equip_relic(db_id, discord_id, guild_id, equipped=True)

        info = dig_service.get_tunnel_info(discord_id, guild_id)
        relics = info["relics"]

        assert len(relics) == 2
        ids = sorted(r.get("artifact_id") for r in relics)
        assert ids == ["crystal_compass", "mole_claws"]

        rendered = ", ".join(
            format_relic_label(r.get("artifact_id", "")) for r in relics
        )
        assert "?" not in rendered
        assert "Mole Claws" in rendered
        assert "Crystal Compass" in rendered
