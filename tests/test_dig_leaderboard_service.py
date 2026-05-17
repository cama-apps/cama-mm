"""Read-only display aggregators for the dig minigame.

Covers ``DigLeaderboardService``: leaderboard ASCII rendering, hall of fame
filtering/ordering, artifact collection grouping, the guild museum registry,
and aggregate guild stats. These are pure ``dig_repo`` reads, so the tests
seed real rows and assert the shaping the embed layer depends on.
"""

import time

import pytest

from repositories.dig_repository import DigRepository
from services.dig_constants import ARTIFACT_POOL
from services.dig_leaderboard_service import DigLeaderboardService


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def lb_service(dig_repo):
    # Built the way DigService.__init__ wires it: dig_repo only.
    return DigLeaderboardService(dig_repo)


def _make_tunnel(dig_repo, discord_id, guild_id, name, **fields):
    """Create a tunnel and apply optional column overrides."""
    dig_repo.create_tunnel(discord_id, guild_id, name)
    if fields:
        dig_repo.update_tunnel(discord_id, guild_id, **fields)


# ─────────────────────────────────────────────────────────────────────────
# get_leaderboard
# ─────────────────────────────────────────────────────────────────────────


class TestGetLeaderboard:
    """get_leaderboard returns ranked tunnels plus an ASCII bar chart."""

    def test_empty_guild_has_no_rows_or_art(self, lb_service, guild_id):
        """A guild with no tunnels yields empty tunnels and empty ASCII art."""
        result = lb_service.get_leaderboard(guild_id)
        assert result["tunnels"] == []
        assert result["ascii_art"] == ""

    def test_tunnels_ordered_by_prestige_then_depth(self, lb_service, dig_repo, guild_id):
        """Ordering is prestige DESC, then depth DESC (get_top_tunnels contract)."""
        _make_tunnel(dig_repo, 1, guild_id, "Shallow", depth=10, prestige_level=0)
        _make_tunnel(dig_repo, 2, guild_id, "Deep", depth=500, prestige_level=0)
        _make_tunnel(dig_repo, 3, guild_id, "Prestiged", depth=5, prestige_level=2)

        result = lb_service.get_leaderboard(guild_id)

        # Prestige 2 outranks any prestige-0 tunnel regardless of depth.
        ids = [t["discord_id"] for t in result["tunnels"]]
        assert ids == [3, 2, 1]

    def test_ascii_bar_scales_to_deepest_tunnel(self, lb_service, dig_repo, guild_id):
        """The deepest tunnel gets a full 40-char bar; others scale down."""
        _make_tunnel(dig_repo, 1, guild_id, "Deepest", depth=100)
        _make_tunnel(dig_repo, 2, guild_id, "Half", depth=50)

        lines = lb_service.get_leaderboard(guild_id)["ascii_art"].split("\n")

        assert len(lines) == 2
        # max_depth == 100 -> deepest bar is 40 blocks, half-depth is 20.
        assert lines[0].count("█") == 40
        assert lines[1].count("█") == 20
        # Each line ends with the literal depth in metres.
        assert lines[0].endswith("100m")
        assert lines[1].endswith("50m")

    def test_ascii_bar_has_minimum_length_one(self, lb_service, dig_repo, guild_id):
        """A zero-depth tunnel still renders at least one bar block."""
        _make_tunnel(dig_repo, 1, guild_id, "Brand New", depth=0)

        lines = lb_service.get_leaderboard(guild_id)["ascii_art"].split("\n")

        # int(40 * 0 / max_depth) == 0, but max(1, ...) guarantees >= 1.
        assert lines[0].count("█") == 1
        assert lines[0].endswith("0m")

    def test_long_tunnel_name_truncated_to_15_chars(self, lb_service, dig_repo, guild_id):
        """Tunnel names longer than 15 chars are clipped in the ASCII art."""
        _make_tunnel(dig_repo, 1, guild_id, "A" * 40, depth=10)

        line = lb_service.get_leaderboard(guild_id)["ascii_art"]

        # The full 40-char name must not appear; only the 15-char prefix does.
        assert "A" * 40 not in line
        assert "A" * 15 in line


# ─────────────────────────────────────────────────────────────────────────
# get_hall_of_fame
# ─────────────────────────────────────────────────────────────────────────


class TestGetHallOfFame:
    """get_hall_of_fame ranks tunnels by best_run_score, excluding zeros."""

    def test_empty_when_no_prestige_scores(self, lb_service, dig_repo, guild_id):
        """Tunnels with best_run_score == 0 are excluded entirely."""
        _make_tunnel(dig_repo, 1, guild_id, "Never Prestiged", best_run_score=0)

        result = lb_service.get_hall_of_fame(guild_id)

        assert result["success"]
        assert result["entries"] == []

    def test_entries_sorted_by_best_run_score_desc(self, lb_service, dig_repo, guild_id):
        """Entries come back highest best_run_score first."""
        _make_tunnel(dig_repo, 1, guild_id, "Low", best_run_score=100)
        _make_tunnel(dig_repo, 2, guild_id, "High", best_run_score=900)
        _make_tunnel(dig_repo, 3, guild_id, "Mid", best_run_score=400)

        entries = lb_service.get_hall_of_fame(guild_id)["entries"]

        assert [e["best_run_score"] for e in entries] == [900, 400, 100]
        assert entries[0]["tunnel_name"] == "High"

    def test_entry_carries_display_fields(self, lb_service, dig_repo, guild_id):
        """Each entry exposes the fields the hall-of-fame embed renders."""
        _make_tunnel(
            dig_repo, 42, guild_id, "Champion", best_run_score=500, prestige_level=3
        )

        entry = lb_service.get_hall_of_fame(guild_id)["entries"][0]

        assert entry["discord_id"] == 42
        assert entry["tunnel_name"] == "Champion"
        assert entry["prestige_level"] == 3
        assert entry["best_run_score"] == 500


# ─────────────────────────────────────────────────────────────────────────
# get_collection
# ─────────────────────────────────────────────────────────────────────────


class TestGetCollection:
    """get_collection groups a player's artifacts by rarity."""

    def test_empty_collection(self, lb_service, dig_repo, guild_id):
        """A player with no artifacts has an empty collection and total 0."""
        result = lb_service.get_collection(9999, guild_id)
        assert result == {"artifacts": {}, "total": 0}

    def test_collection_grouped_by_rarity(self, lb_service, dig_repo, guild_id):
        """Artifacts bucket under the rarity declared in ARTIFACT_POOL. The
        dig_artifacts row has no rarity column, so the service must resolve
        rarity by artifact_id — a flat 'common' default would be wrong."""
        rarity_by_id = {a["id"]: a["rarity"] for a in ARTIFACT_POOL}
        dig_repo.add_artifact(1, guild_id, "mole_claws", is_relic=True)
        dig_repo.add_artifact(1, guild_id, "crystal_compass", is_relic=True)

        result = lb_service.get_collection(1, guild_id)

        assert result["total"] == 2
        # Both relics are "Rare" in the pool — resolved as such, not "common".
        assert set(result["artifacts"]) == {"Rare"}
        for rarity, arts in result["artifacts"].items():
            for art in arts:
                assert rarity_by_id[art["artifact_id"]] == rarity

    def test_collection_is_player_scoped(self, lb_service, dig_repo, guild_id):
        """One player's artifacts never leak into another's collection."""
        dig_repo.add_artifact(1, guild_id, "mole_claws")
        dig_repo.add_artifact(2, guild_id, "crystal_compass")

        result = lb_service.get_collection(1, guild_id)

        assert result["total"] == 1


# ─────────────────────────────────────────────────────────────────────────
# get_museum
# ─────────────────────────────────────────────────────────────────────────


class TestGetMuseum:
    """get_museum returns the guild-wide artifact discovery registry."""

    def test_empty_museum_reports_total_possible(self, lb_service, dig_repo, guild_id):
        """An empty registry still reports the full pool as total_possible."""
        result = lb_service.get_museum(guild_id)

        assert result["entries"] == []
        assert result["total_discovered"] == 0
        # total_possible is the size of ARTIFACT_POOL regardless of finds.
        assert result["total_possible"] == len(ARTIFACT_POOL)
        assert result["by_layer"] == {}

    def test_museum_counts_discovered_artifacts(self, lb_service, dig_repo, guild_id):
        """Each registered artifact id shows up once in the registry."""
        now = int(time.time())
        dig_repo.register_artifact_find("mole_claws", guild_id, finder_id=1, found_at=now)
        dig_repo.register_artifact_find("crystal_compass", guild_id, finder_id=2, found_at=now)
        # A repeat find of the same artifact must not add a second entry.
        dig_repo.register_artifact_find("mole_claws", guild_id, finder_id=3, found_at=now)

        result = lb_service.get_museum(guild_id)

        assert result["total_discovered"] == 2
        found_ids = {e["artifact_id"] for e in result["entries"]}
        assert found_ids == {"mole_claws", "crystal_compass"}

    def test_museum_preserves_first_finder(self, lb_service, dig_repo, guild_id):
        """The registry keeps the original finder even after later finds."""
        now = int(time.time())
        dig_repo.register_artifact_find("mole_claws", guild_id, finder_id=1, found_at=now)
        dig_repo.register_artifact_find("mole_claws", guild_id, finder_id=2, found_at=now + 10)

        entry = lb_service.get_museum(guild_id)["entries"][0]

        assert entry["first_finder_id"] == 1
        assert entry["total_found"] == 2

    def test_museum_groups_entries_by_pool_layer(
        self, lb_service, dig_repo, guild_id
    ):
        """Registry entries bucket under the artifact's ARTIFACT_POOL layer."""
        now = int(time.time())
        # mole_claws is a "Dirt"-layer artifact in the pool.
        dig_repo.register_artifact_find("mole_claws", guild_id, finder_id=1, found_at=now)

        by_layer = lb_service.get_museum(guild_id)["by_layer"]

        assert list(by_layer) == ["Dirt"]
        assert len(by_layer["Dirt"]) == 1


# ─────────────────────────────────────────────────────────────────────────
# get_guild_stats
# ─────────────────────────────────────────────────────────────────────────


class TestGetGuildStats:
    """get_guild_stats aggregates totals and picks the top tunnels."""

    def test_empty_guild_returns_zeroed_stats(self, lb_service, guild_id):
        """A guild with no tunnels reports zeros and None for the leaders."""
        result = lb_service.get_guild_stats(guild_id)

        assert result["success"]
        assert result["total_digs"] == 0
        assert result["total_depth"] == 0
        assert result["total_jc_earned"] == 0
        assert result["tunnel_count"] == 0
        assert result["most_active"] is None
        assert result["deepest"] is None

    def test_stats_sum_across_tunnels(self, lb_service, dig_repo, guild_id):
        """total_* fields sum the matching column over every guild tunnel."""
        _make_tunnel(dig_repo, 1, guild_id, "A", total_digs=10, depth=100, total_jc_earned=50)
        _make_tunnel(dig_repo, 2, guild_id, "B", total_digs=5, depth=200, total_jc_earned=75)

        result = lb_service.get_guild_stats(guild_id)

        assert result["tunnel_count"] == 2
        assert result["total_digs"] == 15
        assert result["total_depth"] == 300
        assert result["total_jc_earned"] == 125

    def test_most_active_and_deepest_picked_correctly(self, lb_service, dig_repo, guild_id):
        """most_active is the highest total_digs; deepest is the highest depth."""
        _make_tunnel(dig_repo, 1, guild_id, "Busy", total_digs=99, depth=10)
        _make_tunnel(dig_repo, 2, guild_id, "Profound", total_digs=2, depth=999)

        result = lb_service.get_guild_stats(guild_id)

        assert result["most_active"]["discord_id"] == 1
        assert result["most_active"]["name"] == "Busy"
        assert result["most_active"]["total_digs"] == 99
        assert result["deepest"]["discord_id"] == 2
        assert result["deepest"]["name"] == "Profound"
        assert result["deepest"]["depth"] == 999

    def test_guild_stats_isolated_by_guild(self, lb_service, dig_repo, guild_id):
        """Tunnels in another guild are excluded from the aggregate."""
        _make_tunnel(dig_repo, 1, guild_id, "Ours", total_digs=10, depth=100)
        _make_tunnel(dig_repo, 2, 99999, "Theirs", total_digs=999, depth=9999)

        result = lb_service.get_guild_stats(guild_id)

        assert result["tunnel_count"] == 1
        assert result["total_digs"] == 10
        assert result["deepest"]["discord_id"] == 1
