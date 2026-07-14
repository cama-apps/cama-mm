import pytest

from domain.models.team import Team
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID


def _seed_players(repo: PlayerRepository, count: int = 10, *, os_mu=None, os_sigma=None):
    for i in range(count):
        pid = 1000 + i
        repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            guild_id=TEST_GUILD_ID,
            preferred_roles=["1", "2", "3", "4", "5"],
            initial_mmr=3000,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        if os_mu is not None:
            repo.update_openskill_rating(pid, TEST_GUILD_ID, os_mu, os_sigma or 8.333)
    return [1000 + i for i in range(count)]


def test_match_service_repo_injected_shuffle_and_record(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=False,
        betting_service=None,
    )

    player_ids = _seed_players(player_repo, 10)

    shuffle_result = service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    assert shuffle_result["radiant_team"]
    pending = match_repo.get_pending_match(TEST_GUILD_ID)
    assert pending is not None

    result = service.record_match("radiant", guild_id=TEST_GUILD_ID)
    assert result["match_id"] > 0
    assert match_repo.get_pending_match(TEST_GUILD_ID) is None

    recorded = match_repo.get_match(result["match_id"], TEST_GUILD_ID)
    assert recorded is not None
    assert recorded["winning_team"] in (1, 2)


def test_shuffle_and_abort_leave_exclusion_factors_unchanged(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=False,
        betting_service=None,
    )
    player_ids = _seed_players(player_repo, 12)
    before = player_repo.get_exclusion_counts(player_ids, TEST_GUILD_ID)

    service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    pending = service.get_last_shuffle(TEST_GUILD_ID)

    assert pending is not None
    assert pending.exclusion_updates_deferred is True
    assert set(pending.full_exclusion_increment_ids) == set(pending.excluded_player_ids)
    assert player_repo.get_exclusion_counts(player_ids, TEST_GUILD_ID) == before

    service.clear_last_shuffle(TEST_GUILD_ID, pending.pending_match_id)

    assert player_repo.get_exclusion_counts(player_ids, TEST_GUILD_ID) == before


def test_record_applies_deferred_exclusion_factor_updates(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=False,
        betting_service=None,
    )
    player_ids = _seed_players(player_repo, 12)
    conditional_id = 2000
    player_repo.add(
        discord_id=conditional_id,
        discord_username="Conditional",
        guild_id=TEST_GUILD_ID,
        preferred_roles=["1", "2", "3", "4", "5"],
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    tracked_ids = [*player_ids, conditional_id]
    before = player_repo.get_exclusion_counts(tracked_ids, TEST_GUILD_ID)

    service.shuffle_players(
        player_ids,
        guild_id=TEST_GUILD_ID,
        excluded_conditional_ids=[conditional_id],
    )
    pending = service.get_last_shuffle(TEST_GUILD_ID)
    assert pending is not None
    assert pending.excluded_conditional_player_ids == [conditional_id]
    assert pending.half_exclusion_increment_ids == [conditional_id]

    service.record_match(
        "radiant",
        guild_id=TEST_GUILD_ID,
        pending_match_id=pending.pending_match_id,
    )

    after = player_repo.get_exclusion_counts(tracked_ids, TEST_GUILD_ID)
    for pid in pending.radiant_team_ids + pending.dire_team_ids:
        assert after[pid] == before[pid] // 2
    for pid in pending.full_exclusion_increment_ids:
        assert after[pid] == before[pid] + 6
    assert after[conditional_id] == before[conditional_id] + 1


def test_goodness_score_respects_role_matchup_weight(repo_db_path, monkeypatch):
    """Ensure goodness_score uses the weighted role delta (0.19 default)."""
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=False,
        betting_service=None,
    )

    # Seed deterministic players with fixed roles (no off-role penalties).
    team1_defs = [
        (8001, "RadiantCarry", 2000, ["1"]),
        (8002, "RadiantMid", 1500, ["2"]),
        (8003, "RadiantOfflane", 1000, ["3"]),
        (8004, "RadiantSoft", 1000, ["4"]),
        (8005, "RadiantHard", 1000, ["5"]),
    ]
    team2_defs = [
        (8006, "DireCarry", 1400, ["1"]),
        (8007, "DireMid", 1500, ["2"]),
        (8008, "DireOfflane", 1900, ["3"]),
        (8009, "DireSoft", 1000, ["4"]),
        (8010, "DireHard", 1000, ["5"]),
    ]
    all_defs = team1_defs + team2_defs
    for pid, name, mmr, roles in all_defs:
        player_repo.add(
            discord_id=pid,
            discord_username=name,
            guild_id=TEST_GUILD_ID,
            preferred_roles=roles,
            initial_mmr=mmr,
            glicko_rating=None,
            glicko_rd=None,
            glicko_volatility=None,
        )

    player_ids = [pid for pid, _, _, _ in all_defs]

    # Build deterministic teams using the exact player objects provided to shuffle_players
    # (so player_id_map resolves correctly by object identity).
    def fake_shuffle(_players):
        team1_players = _players[:5]
        team2_players = _players[5:]
        return (
            Team(team1_players, role_assignments=["1", "2", "3", "4", "5"]),
            Team(team2_players, role_assignments=["1", "2", "3", "4", "5"]),
        )

    monkeypatch.setattr(service.shuffler, "shuffle", fake_shuffle)

    result = service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)

    # value diff = |6500 - 6800| = 300
    # role delta = sum(100, 400, 0, 0, 0) = 500; weighted by 0.19 -> 95
    # rating spread = (2000 - 1000) / 10 = 100
    # off-role penalty and exclusion penalty = 0
    # lobby rating bonus = average team total / 100 = 13,300 / 2 / 100 = 66.5
    assert result["goodness_score"] == pytest.approx(428.5)


def test_openskill_falls_back_to_glicko_when_player_missing_os_mu(repo_db_path):
    """OpenSkill shuffle silently falls back to Glicko when any player lacks os_mu."""
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=True,
        betting_service=None,
    )

    # Seed 10 players, none with OpenSkill ratings
    player_ids = _seed_players(player_repo, 10)

    result = service.shuffle_players(
        player_ids, guild_id=TEST_GUILD_ID, rating_system="openskill"
    )
    assert result["balancing_rating_system"] == "glicko"


def test_openskill_used_when_all_players_have_os_mu(repo_db_path):
    """OpenSkill shuffle proceeds when all players have os_mu."""
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=True,
        betting_service=None,
    )

    # Seed 10 players WITH OpenSkill ratings
    player_ids = _seed_players(player_repo, 10, os_mu=30.0, os_sigma=8.0)

    result = service.shuffle_players(
        player_ids, guild_id=TEST_GUILD_ID, rating_system="openskill"
    )
    assert result["balancing_rating_system"] == "openskill"


def test_region_shuffle_mode_splits_usw_and_use(repo_db_path):
    """Region shuffle mode separates resolved USW and USE players when possible."""
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=False,
        betting_service=None,
    )

    player_ids = _seed_players(player_repo, 10)
    for pid in player_ids[:5]:
        player_repo.update_preferred_region(pid, TEST_GUILD_ID, "USW")
    for pid in player_ids[5:]:
        player_repo.update_preferred_region(pid, TEST_GUILD_ID, "USE")

    result = service.shuffle_players(
        player_ids,
        guild_id=TEST_GUILD_ID,
        shuffle_mode="region",
    )

    assert result["shuffle_mode"] == "region"
    assert result["region_split_penalty"] == 0
    team_region_sets = {
        frozenset(player.preferred_region for player in team.players)
        for team in (result["radiant_team"], result["dire_team"])
    }
    assert team_region_sets == {frozenset({"USW"}), frozenset({"USE"})}


def test_get_last_match_participant_ids_passes_guild_id(repo_db_path):
    """get_last_match_participant_ids forwards guild_id to the repo without TypeError.

    Regression: the service method called the repo with zero args while the repo
    requires guild_id, raising TypeError on every real call (herogrid fallback).
    """
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=False,
        betting_service=None,
    )

    # No matches yet -> empty result, but the call itself must not raise.
    assert service.get_last_match_participant_ids(TEST_GUILD_ID) == []

    # Record a match and confirm the participants come back for that guild.
    player_ids = _seed_players(player_repo, 10)
    service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    service.record_match("radiant", guild_id=TEST_GUILD_ID)

    participants = service.get_last_match_participant_ids(TEST_GUILD_ID)
    assert set(participants) == set(player_ids)
