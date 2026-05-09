"""
Tests for the /matchup command (commands/advstats.py).

These exercise the AdvancedStatsCommands cog against a real
PairingsService + PairingsRepository (using the schema'd test DB) but with
mocked Discord interactions.

The non-trivial logic in /matchup is the canonical-order resolution for
"player1_wins_against" and the formatting that follows; this is what the
tests focus on.
"""

import types

import pytest

from commands.advstats import AdvancedStatsCommands
from services.pairings_service import PairingsService
from tests.conftest import TEST_GUILD_ID

# ---------------------------------------------------------------------------
# Discord interaction shims
# ---------------------------------------------------------------------------


class FakeFollowup:
    def __init__(self):
        self.messages: list[dict] = []

    async def send(
        self,
        content=None,
        embed=None,
        ephemeral=None,
        file=None,
        files=None,
        view=None,
        allowed_mentions=None,
    ):
        self.messages.append(
            {"content": content, "embed": embed, "ephemeral": ephemeral}
        )


class FakeResponse:
    def __init__(self):
        self._done = False

    async def defer(self, ephemeral=False):
        self._done = True

    def is_done(self):
        return self._done


class FakeMember:
    def __init__(self, member_id: int, display_name: str):
        self.id = member_id
        self.display_name = display_name
        self.mention = f"<@{member_id}>"
        self.bot = False


class FakeInteraction:
    _next_id = 7000

    def __init__(self, *, user_id: int = 99, guild_id: int | None = TEST_GUILD_ID):
        FakeInteraction._next_id += 1
        self.id = FakeInteraction._next_id
        self.user = types.SimpleNamespace(id=user_id)
        self.guild = types.SimpleNamespace(id=guild_id) if guild_id is not None else None
        self.response = FakeResponse()
        self.followup = FakeFollowup()
        self.channel = None


@pytest.fixture(autouse=True)
def patch_safe_io(monkeypatch):
    async def _safe_defer(interaction, ephemeral=False):
        interaction.response._done = True
        return True

    async def _safe_followup(interaction, **kw):
        await interaction.followup.send(**kw)

    monkeypatch.setattr("commands.advstats.safe_defer", _safe_defer)
    monkeypatch.setattr("commands.advstats.safe_followup", _safe_followup)


class FakePlayerService:
    """Pure-Python player service stand-in keyed on discord_id."""

    def __init__(self, registered_ids: set[int]):
        self._registered = registered_ids

    def get_player(self, discord_id, guild_id=None):
        if discord_id in self._registered:
            return types.SimpleNamespace(discord_id=discord_id, name=f"P{discord_id}")
        return None


def _make_cog(pairings_service, player_service):
    bot = types.SimpleNamespace()
    return AdvancedStatsCommands(bot, pairings_service, player_service)


def _record_match(match_repository, *, team1_ids, team2_ids, winning_team, guild_id=TEST_GUILD_ID):
    """Helper: record a finished match for pairings rebuild."""
    return match_repository.record_match(
        team1_ids=team1_ids,
        team2_ids=team2_ids,
        winning_team=winning_team,
        guild_id=guild_id,
    )


# ---------------------------------------------------------------------------
# Validation
# ---------------------------------------------------------------------------


class TestMatchupValidation:
    @pytest.mark.asyncio
    async def test_self_comparison_rejected(self, pairings_repository, player_repository):
        pairings_service = PairingsService(pairings_repository)
        # Just one player, comparing against themselves
        player_repository.add(discord_id=1, discord_username="Alice", guild_id=TEST_GUILD_ID)
        cog = _make_cog(pairings_service, FakePlayerService({1}))

        interaction = FakeInteraction()
        member = FakeMember(1, "Alice")

        await cog.matchup.callback(cog, interaction, member, member)

        assert interaction.followup.messages
        msg = interaction.followup.messages[-1]
        assert msg["ephemeral"] is True
        assert "Cannot compare a player with themselves" in msg["content"]

    @pytest.mark.asyncio
    async def test_unregistered_player1(self, pairings_repository, player_repository):
        pairings_service = PairingsService(pairings_repository)
        player_repository.add(discord_id=2, discord_username="Bob", guild_id=TEST_GUILD_ID)
        cog = _make_cog(pairings_service, FakePlayerService({2}))

        interaction = FakeInteraction()
        member1 = FakeMember(1, "Alice")
        member2 = FakeMember(2, "Bob")

        await cog.matchup.callback(cog, interaction, member1, member2)

        assert interaction.followup.messages
        msg = interaction.followup.messages[-1]
        assert msg["ephemeral"] is True
        assert "Alice is not registered" in msg["content"]

    @pytest.mark.asyncio
    async def test_unregistered_player2(self, pairings_repository, player_repository):
        pairings_service = PairingsService(pairings_repository)
        player_repository.add(discord_id=1, discord_username="Alice", guild_id=TEST_GUILD_ID)
        cog = _make_cog(pairings_service, FakePlayerService({1}))

        interaction = FakeInteraction()
        member1 = FakeMember(1, "Alice")
        member2 = FakeMember(2, "Bob")

        await cog.matchup.callback(cog, interaction, member1, member2)

        msg = interaction.followup.messages[-1]
        assert msg["ephemeral"] is True
        assert "Bob is not registered" in msg["content"]


# ---------------------------------------------------------------------------
# No history found — empty embed branch
# ---------------------------------------------------------------------------


class TestMatchupNoHistory:
    @pytest.mark.asyncio
    async def test_no_pairing_data_renders_empty_state(
        self, pairings_repository, player_repository
    ):
        # Both registered, but no matches recorded between them.
        player_repository.add(discord_id=1, discord_username="Alice", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=2, discord_username="Bob", guild_id=TEST_GUILD_ID)
        # Stub head-to-head returning None
        pairings_service = types.SimpleNamespace(
            get_head_to_head=lambda *_a, **_kw: None
        )
        cog = _make_cog(pairings_service, FakePlayerService({1, 2}))

        interaction = FakeInteraction()
        await cog.matchup.callback(
            cog, interaction, FakeMember(1, "Alice"), FakeMember(2, "Bob")
        )

        # An embed with the empty-state description should be sent.
        msg = interaction.followup.messages[-1]
        embed = msg["embed"]
        assert embed is not None
        assert "Alice vs Bob" in embed.title
        assert "No games played" in embed.description


# ---------------------------------------------------------------------------
# Teammate / opponent embed rendering
# ---------------------------------------------------------------------------


class TestMatchupRendering:
    @pytest.mark.asyncio
    async def test_teammates_only_renders_correct_winrate(
        self, pairings_repository, match_repository, player_repository
    ):
        # Alice and Bob teamed up 4 times, won 3.
        for pid, name in [(1, "Alice"), (2, "Bob"), (3, "Other1"), (4, "Other2")]:
            player_repository.add(discord_id=pid, discord_username=name, guild_id=TEST_GUILD_ID)
        for i in range(4):
            _record_match(
                match_repository,
                team1_ids=[1, 2],
                team2_ids=[3, 4],
                winning_team=1 if i < 3 else 2,
            )
        # Recompute pairwise stats from match history.
        pairings_repository.rebuild_all_pairings(TEST_GUILD_ID)

        pairings_service = PairingsService(pairings_repository)
        cog = _make_cog(pairings_service, FakePlayerService({1, 2}))
        interaction = FakeInteraction()

        await cog.matchup.callback(
            cog, interaction, FakeMember(1, "Alice"), FakeMember(2, "Bob")
        )

        embed = interaction.followup.messages[-1]["embed"]
        assert embed is not None
        teammate_field = next(f for f in embed.fields if f.name == "As Teammates")
        # 4 games, 3 wins, 75%
        assert "4 games" in teammate_field.value
        assert "3 wins" in teammate_field.value
        assert "75%" in teammate_field.value

        # Never played as opponents → empty value
        opp_field = next(f for f in embed.fields if f.name == "As Opponents")
        assert "Never played against" in opp_field.value

    @pytest.mark.asyncio
    async def test_opponents_canonical_order_swap_correct(
        self, pairings_repository, match_repository, player_repository
    ):
        """Verify that the win-attribution flips when the lower-id player is on team1."""
        for pid, name in [(1, "Alice"), (2, "Bob")]:
            player_repository.add(discord_id=pid, discord_username=name, guild_id=TEST_GUILD_ID)
        # Alice (id=1) on team1, Bob (id=2) on team2. team2 wins 3 of 5.
        for i in range(5):
            _record_match(
                match_repository,
                team1_ids=[1],
                team2_ids=[2],
                winning_team=2 if i < 3 else 1,
            )
        pairings_repository.rebuild_all_pairings(TEST_GUILD_ID)

        pairings_service = PairingsService(pairings_repository)
        cog = _make_cog(pairings_service, FakePlayerService({1, 2}))

        # ---- Case 1: argument order matches canonical (Alice first, lower id) ----
        interaction = FakeInteraction()
        await cog.matchup.callback(
            cog, interaction, FakeMember(1, "Alice"), FakeMember(2, "Bob")
        )
        embed = interaction.followup.messages[-1]["embed"]
        opp_field = next(f for f in embed.fields if f.name == "As Opponents")
        # 5 games; Alice won 2, Bob won 3
        assert "5 games" in opp_field.value
        assert "Alice: 2 wins" in opp_field.value
        assert "Bob: 3 wins" in opp_field.value

        # ---- Case 2: argument order reversed (Bob first) — wins must still be attributed correctly ----
        interaction2 = FakeInteraction()
        await cog.matchup.callback(
            cog, interaction2, FakeMember(2, "Bob"), FakeMember(1, "Alice")
        )
        embed2 = interaction2.followup.messages[-1]["embed"]
        opp2 = next(f for f in embed2.fields if f.name == "As Opponents")
        # Wins are unchanged regardless of arg order
        assert "Alice: 2 wins" in opp2.value
        assert "Bob: 3 wins" in opp2.value


# ---------------------------------------------------------------------------
# PairingsService.get_head_to_head — the only data path the command actually
# calls — verified against a populated DB.
# ---------------------------------------------------------------------------


class TestPairingsServiceHeadToHead:
    def test_returns_none_when_no_history(self, pairings_repository):
        svc = PairingsService(pairings_repository)
        assert svc.get_head_to_head(101, 102, TEST_GUILD_ID) is None

    def test_records_teammate_and_opponent_counts(
        self, pairings_repository, match_repository, player_repository
    ):
        for pid, name in [(1, "A"), (2, "B"), (3, "C")]:
            player_repository.add(discord_id=pid, discord_username=name, guild_id=TEST_GUILD_ID)

        # Two as teammates, A wins both
        for _ in range(2):
            _record_match(match_repository, team1_ids=[1, 2], team2_ids=[3], winning_team=1)
        # Three as opponents, A on team1; A wins 1, B wins 2
        for i in range(3):
            _record_match(
                match_repository,
                team1_ids=[1],
                team2_ids=[2, 3],
                winning_team=1 if i == 0 else 2,
            )
        pairings_repository.rebuild_all_pairings(TEST_GUILD_ID)

        svc = PairingsService(pairings_repository)
        h2h = svc.get_head_to_head(1, 2, TEST_GUILD_ID)
        assert h2h is not None
        assert h2h["games_together"] == 2
        assert h2h["wins_together"] == 2
        assert h2h["games_against"] == 3
        # canonical: lower id is player1 (1=A)
        assert h2h["player1_id"] == 1
        # A won 1 of 3 head-to-heads
        assert h2h["player1_wins_against"] == 1
