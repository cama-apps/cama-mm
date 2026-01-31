"""Tests for betting impact stats feature."""

import time

import pytest

from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.gambling_stats_service import (
    BettingImpactStats,
    BettorProfile,
    GamblingStatsService,
)
from services.match_service import MatchService


@pytest.fixture
def services(repo_db_path):
    """Create test services using centralized fast fixture."""
    player_repo = PlayerRepository(repo_db_path)
    bet_repo = BetRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    betting_service = BettingService(bet_repo, player_repo)
    match_service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=True,
        betting_service=betting_service,
    )
    gambling_stats_service = GamblingStatsService(
        bet_repo=bet_repo,
        player_repo=player_repo,
        match_repo=match_repo,
    )

    yield {
        "match_service": match_service,
        "betting_service": betting_service,
        "gambling_stats_service": gambling_stats_service,
        "player_repo": player_repo,
        "bet_repo": bet_repo,
        "match_repo": match_repo,
        "db_path": repo_db_path,
    }


def _setup_players(player_repo, player_ids, spectator_ids):
    """Helper to setup players and spectators."""
    for pid in player_ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            dotabuff_url=f"https://dotabuff.com/players/{pid}",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
    for sid in spectator_ids:
        player_repo.add(
            discord_id=sid,
            discord_username=f"Spectator{sid}",
            dotabuff_url=f"https://dotabuff.com/players/{sid}",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )


def _ensure_betting_open(pending):
    """Ensure betting window is open."""
    if pending.get("bet_lock_until") is None or pending["bet_lock_until"] <= int(time.time()):
        pending["bet_lock_until"] = int(time.time()) + 600


class TestBetRepositoryGetBetsOnPlayerMatches:
    """Tests for BetRepository.get_bets_on_player_matches()."""

    def test_returns_empty_for_player_with_no_matches(self, services):
        """Player with no match participation returns empty list."""
        bet_repo = services["bet_repo"]
        result = bet_repo.get_bets_on_player_matches(99999)
        assert result == []

    def test_excludes_players_own_bets(self, services):
        """Player's own bets on their matches are excluded."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]
        bet_repo = services["bet_repo"]

        # Setup 10 players + 2 spectators
        player_ids = list(range(1000, 1010))
        spectator_ids = [2001, 2002]
        _setup_players(player_repo, player_ids, spectator_ids)

        # Give balances
        for sid in spectator_ids:
            player_repo.add_balance(sid, 100)

        # Shuffle and set up betting
        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="house")
        pending = match_service.get_last_shuffle(1)
        _ensure_betting_open(pending)

        # Target player is first on radiant
        target_player = pending["radiant_team_ids"][0]
        player_repo.add_balance(target_player, 100)

        # Target player bets on their own team
        betting_service.place_bet(1, target_player, "radiant", 10, pending)
        # Spectators bet
        betting_service.place_bet(1, 2001, "radiant", 20, pending)  # FOR target
        betting_service.place_bet(1, 2002, "dire", 15, pending)  # AGAINST target

        # Record match (radiant wins)
        match_service.record_match("radiant", guild_id=1)

        # Get bets on target player's matches
        bets = bet_repo.get_bets_on_player_matches(target_player)

        # Should only have spectator bets, not target's own bet
        assert len(bets) == 2
        bettor_ids = {bet["bettor_id"] for bet in bets}
        assert target_player not in bettor_ids
        assert 2001 in bettor_ids
        assert 2002 in bettor_ids

    def test_only_returns_settled_matches(self, services):
        """Only bets from settled matches are returned."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]
        bet_repo = services["bet_repo"]

        player_ids = list(range(1000, 1010))
        spectator_ids = [2001]
        _setup_players(player_repo, player_ids, spectator_ids)
        player_repo.add_balance(2001, 100)

        # First match - will be recorded
        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="house")
        pending1 = match_service.get_last_shuffle(1)
        _ensure_betting_open(pending1)
        betting_service.place_bet(1, 2001, "radiant", 10, pending1)
        match_service.record_match("radiant", guild_id=1)

        # Second match - pending (not recorded)
        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="house")
        pending2 = match_service.get_last_shuffle(1)
        _ensure_betting_open(pending2)
        betting_service.place_bet(1, 2001, "radiant", 20, pending2)
        # Don't record this match

        target_player = pending1["radiant_team_ids"][0]
        bets = bet_repo.get_bets_on_player_matches(target_player)
        # Should only have bet from first (settled) match
        assert len(bets) == 1
        assert bets[0]["effective_bet"] == 10

    def test_bet_direction_calculated_correctly(self, services):
        """bet_direction is 'for' when bettor bets on player's team, 'against' otherwise."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]
        bet_repo = services["bet_repo"]

        player_ids = list(range(1000, 1010))
        spectator_ids = [2001, 2002]
        _setup_players(player_repo, player_ids, spectator_ids)
        for sid in spectator_ids:
            player_repo.add_balance(sid, 100)

        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="house")
        pending = match_service.get_last_shuffle(1)
        _ensure_betting_open(pending)

        # Target player is on radiant
        target_player = pending["radiant_team_ids"][0]

        betting_service.place_bet(1, 2001, "radiant", 10, pending)  # FOR
        betting_service.place_bet(1, 2002, "dire", 15, pending)  # AGAINST

        match_service.record_match("radiant", guild_id=1)

        bets = bet_repo.get_bets_on_player_matches(target_player)

        for_bet = next(b for b in bets if b["bettor_id"] == 2001)
        against_bet = next(b for b in bets if b["bettor_id"] == 2002)

        assert for_bet["bet_direction"] == "for"
        assert against_bet["bet_direction"] == "against"


class TestGamblingStatsServiceBettingImpact:
    """Tests for GamblingStatsService.get_betting_impact_stats()."""

    def test_returns_none_for_no_external_bets(self, services):
        """Returns None when player has no external bets on their matches."""
        gambling_stats_service = services["gambling_stats_service"]
        result = gambling_stats_service.get_betting_impact_stats(99999)
        assert result is None

    def test_calculates_aggregate_totals(self, services):
        """Correctly calculates total wagered for/against and P&L."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]
        gambling_stats_service = services["gambling_stats_service"]

        player_ids = list(range(1000, 1010))
        spectator_ids = [2001, 2002, 2003]
        _setup_players(player_repo, player_ids, spectator_ids)
        for sid in spectator_ids:
            player_repo.add_balance(sid, 100)

        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="house")
        pending = match_service.get_last_shuffle(1)
        _ensure_betting_open(pending)

        target_player = pending["radiant_team_ids"][0]

        # Bets FOR target (radiant)
        betting_service.place_bet(1, 2001, "radiant", 10, pending)
        betting_service.place_bet(1, 2002, "radiant", 20, pending)
        # Bet AGAINST target (dire)
        betting_service.place_bet(1, 2003, "dire", 15, pending)

        # Radiant wins - supporters profit, haters lose
        match_service.record_match("radiant", guild_id=1)

        impact = gambling_stats_service.get_betting_impact_stats(target_player)

        assert impact is not None
        assert impact.total_wagered_for == 30  # 10 + 20
        assert impact.total_wagered_against == 15
        # Supporters win: 10*2 - 10 + 20*2 - 20 = 10 + 20 = 30 profit
        assert impact.supporters_net_pnl == 30
        # Haters lose: -15
        assert impact.haters_net_pnl == -15

    def test_finds_biggest_fan(self, services):
        """Biggest fan is the person who wagered most FOR the player."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]
        gambling_stats_service = services["gambling_stats_service"]

        player_ids = list(range(1000, 1010))
        spectator_ids = [2001, 2002]
        _setup_players(player_repo, player_ids, spectator_ids)
        player_repo.add_balance(2001, 100)
        player_repo.add_balance(2002, 100)

        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="house")
        pending = match_service.get_last_shuffle(1)
        _ensure_betting_open(pending)

        target_player = pending["radiant_team_ids"][0]

        betting_service.place_bet(1, 2001, "radiant", 10, pending)
        betting_service.place_bet(1, 2002, "radiant", 50, pending)  # Bigger bet

        match_service.record_match("radiant", guild_id=1)

        impact = gambling_stats_service.get_betting_impact_stats(target_player)

        assert impact.biggest_fan is not None
        assert impact.biggest_fan.discord_id == 2002
        assert impact.biggest_fan.total_wagered_for == 50

    def test_finds_biggest_hater(self, services):
        """Biggest hater is the person who wagered most AGAINST the player."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]
        gambling_stats_service = services["gambling_stats_service"]

        player_ids = list(range(1000, 1010))
        spectator_ids = [2001, 2002]
        _setup_players(player_repo, player_ids, spectator_ids)
        player_repo.add_balance(2001, 100)
        player_repo.add_balance(2002, 100)

        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="house")
        pending = match_service.get_last_shuffle(1)
        _ensure_betting_open(pending)

        target_player = pending["radiant_team_ids"][0]

        betting_service.place_bet(1, 2001, "dire", 15, pending)
        betting_service.place_bet(1, 2002, "dire", 40, pending)  # Bigger bet

        match_service.record_match("radiant", guild_id=1)

        impact = gambling_stats_service.get_betting_impact_stats(target_player)

        assert impact.biggest_hater is not None
        assert impact.biggest_hater.discord_id == 2002
        assert impact.biggest_hater.total_wagered_against == 40

    def test_market_favorability_calculation(self, services):
        """Market favorability is percentage of bets FOR the player."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]
        gambling_stats_service = services["gambling_stats_service"]

        player_ids = list(range(1000, 1010))
        spectator_ids = [2001, 2002]
        _setup_players(player_repo, player_ids, spectator_ids)
        player_repo.add_balance(2001, 100)
        player_repo.add_balance(2002, 100)

        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="house")
        pending = match_service.get_last_shuffle(1)
        _ensure_betting_open(pending)

        target_player = pending["radiant_team_ids"][0]

        betting_service.place_bet(1, 2001, "radiant", 75, pending)  # FOR
        betting_service.place_bet(1, 2002, "dire", 25, pending)  # AGAINST

        match_service.record_match("radiant", guild_id=1)

        impact = gambling_stats_service.get_betting_impact_stats(target_player)

        # 75 / (75 + 25) = 0.75
        assert impact.market_favorability == 0.75

    def test_counts_unique_supporters_and_haters(self, services):
        """Counts unique supporters and haters across all matches."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]
        gambling_stats_service = services["gambling_stats_service"]

        player_ids = list(range(1000, 1010))
        spectator_ids = [2001, 2002, 2003]
        _setup_players(player_repo, player_ids, spectator_ids)
        for sid in spectator_ids:
            player_repo.add_balance(sid, 100)

        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="house")
        pending = match_service.get_last_shuffle(1)
        _ensure_betting_open(pending)

        target_player = pending["radiant_team_ids"][0]

        betting_service.place_bet(1, 2001, "radiant", 10, pending)  # Supporter
        betting_service.place_bet(1, 2002, "radiant", 10, pending)  # Supporter
        betting_service.place_bet(1, 2003, "dire", 10, pending)  # Hater

        match_service.record_match("radiant", guild_id=1)

        impact = gambling_stats_service.get_betting_impact_stats(target_player)

        assert impact.unique_supporters == 2
        assert impact.unique_haters == 1

    def test_blessing_only_set_if_positive(self, services):
        """blessing is None if all supporters lost money."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]
        gambling_stats_service = services["gambling_stats_service"]

        player_ids = list(range(1000, 1010))
        spectator_ids = [2001]
        _setup_players(player_repo, player_ids, spectator_ids)
        player_repo.add_balance(2001, 100)

        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="house")
        pending = match_service.get_last_shuffle(1)
        _ensure_betting_open(pending)

        target_player = pending["radiant_team_ids"][0]

        betting_service.place_bet(1, 2001, "radiant", 10, pending)  # FOR target

        # Record dire win - supporter loses
        match_service.record_match("dire", guild_id=1)

        impact = gambling_stats_service.get_betting_impact_stats(target_player)

        # Supporter lost, so blessing should be None
        assert impact.blessing is None

    def test_luckiest_hater_only_set_if_positive(self, services):
        """luckiest_hater is None if all haters lost money."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]
        gambling_stats_service = services["gambling_stats_service"]

        player_ids = list(range(1000, 1010))
        spectator_ids = [2001]
        _setup_players(player_repo, player_ids, spectator_ids)
        player_repo.add_balance(2001, 100)

        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="house")
        pending = match_service.get_last_shuffle(1)
        _ensure_betting_open(pending)

        target_player = pending["radiant_team_ids"][0]

        betting_service.place_bet(1, 2001, "dire", 10, pending)  # AGAINST target

        # Record radiant win - hater loses
        match_service.record_match("radiant", guild_id=1)

        impact = gambling_stats_service.get_betting_impact_stats(target_player)

        # Hater lost, so luckiest_hater should be None
        assert impact.luckiest_hater is None

    def test_total_bets_count(self, services):
        """total_bets counts all external bets on player's matches."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]
        gambling_stats_service = services["gambling_stats_service"]

        player_ids = list(range(1000, 1010))
        spectator_ids = [2001, 2002, 2003]
        _setup_players(player_repo, player_ids, spectator_ids)
        for sid in spectator_ids:
            player_repo.add_balance(sid, 100)

        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="house")
        pending = match_service.get_last_shuffle(1)
        _ensure_betting_open(pending)

        target_player = pending["radiant_team_ids"][0]

        # 3 bets total
        betting_service.place_bet(1, 2001, "radiant", 10, pending)
        betting_service.place_bet(1, 2002, "radiant", 20, pending)
        betting_service.place_bet(1, 2003, "dire", 15, pending)

        match_service.record_match("radiant", guild_id=1)

        impact = gambling_stats_service.get_betting_impact_stats(target_player)

        assert impact.total_bets == 3

    def test_win_rates_calculated_correctly(self, services):
        """supporter_win_rate and hater_win_rate track bet outcomes."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]
        gambling_stats_service = services["gambling_stats_service"]

        player_ids = list(range(1000, 1010))
        spectator_ids = [2001, 2002, 2003, 2004]
        _setup_players(player_repo, player_ids, spectator_ids)
        for sid in spectator_ids:
            player_repo.add_balance(sid, 200)

        # First match - radiant wins
        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="house")
        pending = match_service.get_last_shuffle(1)
        _ensure_betting_open(pending)
        target_player = pending["radiant_team_ids"][0]

        betting_service.place_bet(1, 2001, "radiant", 10, pending)  # FOR - wins
        betting_service.place_bet(1, 2002, "radiant", 10, pending)  # FOR - wins
        betting_service.place_bet(1, 2003, "dire", 10, pending)  # AGAINST - loses
        betting_service.place_bet(1, 2004, "dire", 10, pending)  # AGAINST - loses

        match_service.record_match("radiant", guild_id=1)

        impact = gambling_stats_service.get_betting_impact_stats(target_player)

        # Supporters: 2 bets, 2 wins = 100%
        assert impact.supporter_win_rate == 1.0
        assert impact.supporter_bets_count == 2
        # Haters: 2 bets, 0 wins = 0%
        assert impact.hater_win_rate == 0.0
        assert impact.hater_bets_count == 2

    def test_most_consistent_fan_by_bet_count(self, services):
        """most_consistent_fan is the bettor with most bets FOR player."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]
        gambling_stats_service = services["gambling_stats_service"]

        player_ids = list(range(1000, 1010))
        spectator_ids = [2001, 2002]
        _setup_players(player_repo, player_ids, spectator_ids)
        player_repo.add_balance(2001, 200)
        player_repo.add_balance(2002, 200)

        # First match
        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="house")
        pending1 = match_service.get_last_shuffle(1)
        _ensure_betting_open(pending1)
        target_player = pending1["radiant_team_ids"][0]

        betting_service.place_bet(1, 2001, "radiant", 10, pending1)  # 2001 bets FOR target
        betting_service.place_bet(1, 2002, "radiant", 5, pending1)  # 2002 bets FOR target
        match_service.record_match("radiant", guild_id=1)

        # Second match - find which team target is on
        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="house")
        pending2 = match_service.get_last_shuffle(1)
        _ensure_betting_open(pending2)

        # Bet on whichever team the target is on
        target_team2 = "radiant" if target_player in pending2["radiant_team_ids"] else "dire"
        betting_service.place_bet(1, 2002, target_team2, 5, pending2)  # 2002 bets FOR target again
        match_service.record_match(target_team2, guild_id=1)  # Target's team wins

        impact = gambling_stats_service.get_betting_impact_stats(target_player)

        # 2002 has more bets (2) vs 2001 (1)
        assert impact.most_consistent_fan is not None
        assert impact.most_consistent_fan.discord_id == 2002
        assert impact.most_consistent_fan.bets_for_count == 2

    def test_blessing_and_jinx(self, services):
        """blessing profits most betting FOR, jinx loses most betting FOR."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]
        gambling_stats_service = services["gambling_stats_service"]

        player_ids = list(range(1000, 1010))
        spectator_ids = [2001, 2002]
        _setup_players(player_repo, player_ids, spectator_ids)
        player_repo.add_balance(2001, 200)
        player_repo.add_balance(2002, 200)

        # First match - target's team wins
        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="house")
        pending1 = match_service.get_last_shuffle(1)
        _ensure_betting_open(pending1)
        target_player = pending1["radiant_team_ids"][0]

        # Both bet FOR target (on radiant)
        betting_service.place_bet(1, 2001, "radiant", 50, pending1)  # Wins 50
        betting_service.place_bet(1, 2002, "radiant", 10, pending1)  # Wins 10
        match_service.record_match("radiant", guild_id=1)

        # Second match - target's team loses
        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="house")
        pending2 = match_service.get_last_shuffle(1)
        _ensure_betting_open(pending2)

        # Find which team target is on, then bet FOR them (they will lose)
        target_team2 = "radiant" if target_player in pending2["radiant_team_ids"] else "dire"
        losing_team = target_team2
        winning_team = "dire" if target_team2 == "radiant" else "radiant"

        betting_service.place_bet(1, 2001, target_team2, 10, pending2)  # Loses 10 (betting FOR target)
        betting_service.place_bet(1, 2002, target_team2, 50, pending2)  # Loses 50 (betting FOR target)
        match_service.record_match(winning_team, guild_id=1)  # Target's team loses

        impact = gambling_stats_service.get_betting_impact_stats(target_player)

        # 2001: +50 -10 = +40 net (betting FOR target)
        # 2002: +10 -50 = -40 net (betting FOR target)
        # Blessing = 2001 (most profit betting FOR)
        assert impact.blessing is not None
        assert impact.blessing.discord_id == 2001
        assert impact.blessing.net_pnl_for == 40

        # Jinx = 2002 (most loss betting FOR)
        assert impact.jinx is not None
        assert impact.jinx.discord_id == 2002
        assert impact.jinx.net_pnl_for == -40

    def test_biggest_single_win_and_loss(self, services):
        """biggest_single_win and biggest_single_loss track extremes."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]
        gambling_stats_service = services["gambling_stats_service"]

        player_ids = list(range(1000, 1010))
        spectator_ids = [2001, 2002]
        _setup_players(player_repo, player_ids, spectator_ids)
        player_repo.add_balance(2001, 200)
        player_repo.add_balance(2002, 200)

        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="house")
        pending = match_service.get_last_shuffle(1)
        _ensure_betting_open(pending)

        target_player = pending["radiant_team_ids"][0]

        # Winner wins 50, loser loses 30
        betting_service.place_bet(1, 2001, "radiant", 50, pending)  # Wins +50
        betting_service.place_bet(1, 2002, "dire", 30, pending)  # Loses -30

        match_service.record_match("radiant", guild_id=1)

        impact = gambling_stats_service.get_betting_impact_stats(target_player)

        assert impact.biggest_single_win == 50
        assert impact.biggest_single_loss == -30
