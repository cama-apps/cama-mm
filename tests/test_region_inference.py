"""Tests for server-region inference, resolution, and aggregation (utils/region.py)."""

from types import SimpleNamespace

from utils.region import (
    DEFAULT_REGION_NAME,
    SENTINEL_NONE,
    infer_region_from_counts,
    region_split_mismatches,
    resolve_region,
    summarize_region,
)


def _player(preferred=None, inferred=None):
    """A minimal stand-in for a Player (resolve_region only reads two attrs)."""
    return SimpleNamespace(preferred_region=preferred, inferred_region=inferred)


class TestInferRegionFromCounts:
    """infer_region_from_counts turns an OpenDota /counts payload into a code."""

    def test_more_us_east_games_infers_use(self):
        """US East ("2") with more games wins even against a huge other-region total."""
        counts = {"region": {"3": {"games": 1000}, "2": {"games": 30}, "1": {"games": 20}}}
        assert infer_region_from_counts(counts) == "USE"

    def test_more_us_west_games_infers_usw(self):
        """US West ("1") with more games wins."""
        counts = {"region": {"1": {"games": 50}, "2": {"games": 12}}}
        assert infer_region_from_counts(counts) == "USW"

    def test_equal_us_games_defaults_west(self):
        """An exact US East/West tie leans US West, matching the lobby tie-break."""
        counts = {"region": {"1": {"games": 40}, "2": {"games": 40}}}
        assert infer_region_from_counts(counts) == "USW"

    def test_no_us_games_returns_sentinel(self):
        """EU-only play (no US games) yields the checked-but-none sentinel, not a guess."""
        counts = {"region": {"3": {"games": 500}}}
        assert infer_region_from_counts(counts) == SENTINEL_NONE

    def test_empty_payload_returns_sentinel(self):
        """A real payload with no US play is 'checked, nothing to infer'."""
        assert infer_region_from_counts({"region": {}}) == SENTINEL_NONE
        assert infer_region_from_counts({}) == SENTINEL_NONE

    def test_missing_payload_returns_none_for_retry(self):
        """No payload (None = API failed/rate-limited) returns None so the row stays unchecked."""
        assert infer_region_from_counts(None) is None

    def test_handles_non_dict_region_values(self):
        """Defensive: region entries that are bare ints still count as games."""
        counts = {"region": {"2": 5, "1": 3}}
        assert infer_region_from_counts(counts) == "USE"


class TestResolveRegion:
    """resolve_region picks the effective code a player votes with."""

    def test_explicit_pick_wins_over_inferred(self):
        """A player's explicit choice overrides the OpenDota guess."""
        assert resolve_region(_player(preferred="USW", inferred="USE")) == "USW"

    def test_inferred_used_when_no_explicit(self):
        """With no explicit pick, the inferred value is used."""
        assert resolve_region(_player(preferred=None, inferred="USE")) == "USE"

    def test_sentinel_and_unset_resolve_to_none(self):
        """Sentinel (checked-no-US) and unset both mean 'no vote'."""
        assert resolve_region(_player(inferred=SENTINEL_NONE)) is None
        assert resolve_region(_player()) is None


class TestSummarizeRegion:
    """summarize_region recommends a server name for a group of players."""

    def test_majority_wins(self):
        """The region with the most votes is recommended."""
        players = [_player(preferred="USE")] * 6 + [_player(preferred="USW")] * 4
        assert summarize_region(players) == "US East"

    def test_tie_defaults_to_west(self):
        """A 5–5 split recommends US West per the chosen tie-break."""
        players = [_player(preferred="USE")] * 5 + [_player(preferred="USW")] * 5
        assert summarize_region(players) == "US West"

    def test_no_votes_defaults_to_west(self):
        """All-unset (including sentinel) defaults the lobby server to US West."""
        players = [_player(), _player(inferred=SENTINEL_NONE)]
        assert summarize_region(players) == DEFAULT_REGION_NAME

    def test_inferred_votes_count(self):
        """Inferred regions count toward the tally alongside explicit picks."""
        players = [_player(inferred="USE"), _player(inferred="USE"), _player(preferred="USW")]
        assert summarize_region(players) == "US East"


class TestRegionSplitMismatches:
    """region_split_mismatches scores team purity independent of team labels."""

    def test_clean_split_has_no_mismatches(self):
        team1 = [_player(preferred="USW") for _ in range(5)]
        team2 = [_player(preferred="USE") for _ in range(5)]
        assert region_split_mismatches(team1, team2) == 0
        assert region_split_mismatches(team2, team1) == 0

    def test_mixed_split_counts_best_orientation(self):
        team1 = [_player(preferred="USW") for _ in range(4)] + [_player(preferred="USE")]
        team2 = [_player(preferred="USE") for _ in range(4)] + [_player(preferred="USW")]
        assert region_split_mismatches(team1, team2) == 2

    def test_unset_players_do_not_count(self):
        team1 = [_player(preferred="USW"), _player()]
        team2 = [_player(preferred="USE"), _player(inferred=SENTINEL_NONE)]
        assert region_split_mismatches(team1, team2) == 0
