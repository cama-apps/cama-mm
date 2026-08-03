"""
Tests for the /extendbetting admin command functionality.

Tests verify that:
1. Betting window can be extended
2. Extended state is persisted to database
3. Extended window survives restart
4. Bets can be placed after extension reopens a closed window
"""

import time
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import discord
import pytest

from commands.admin import AdminCommands
from domain.models.lobby import LobbyKind
from domain.models.pending_match_state import PendingMatchState
from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID


def _seed_players(player_repo: PlayerRepository, player_ids: list[int], balance: int = 100):
    """Create test players with roles and balance."""
    roles_cycle = [["1"], ["2"], ["3"], ["4"], ["5"]]
    for i, pid in enumerate(player_ids):
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            guild_id=TEST_GUILD_ID,
            initial_mmr=3000 + (i * 100),
            glicko_rating=1500.0,
            glicko_rd=200.0,
            glicko_volatility=0.06,
            preferred_roles=roles_cycle[i % 5],
        )
        player_repo.update_balance(pid, TEST_GUILD_ID, balance)


class TestExtendBetting:
    """Tests for extending the betting window."""

    @pytest.fixture
    def db_path(self, repo_db_path):
        """Use centralized fast fixture for database path."""
        return repo_db_path

    def test_extend_betting_updates_lock_until(self, db_path):
        """Test that extending betting updates bet_lock_until in state."""
        player_repo = PlayerRepository(db_path)
        match_repo = MatchRepository(db_path)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=False,
        )

        player_ids = list(range(1000, 1010))
        _seed_players(player_repo, player_ids)

        # Shuffle to create pending match
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)

        # Get original lock time
        state = match_service.get_last_shuffle(TEST_GUILD_ID)
        original_lock = state.bet_lock_until

        # Simulate extending by 10 minutes
        now_ts = int(time.time())
        extension_minutes = 10
        new_lock_until = max(original_lock, now_ts) + (extension_minutes * 60)

        state.bet_lock_until = new_lock_until
        match_service.set_last_shuffle(TEST_GUILD_ID, state)
        match_service._persist_match_state(TEST_GUILD_ID, state)

        # Verify state is updated
        updated_state = match_service.get_last_shuffle(TEST_GUILD_ID)
        assert updated_state.bet_lock_until == new_lock_until
        assert updated_state.bet_lock_until > original_lock

    def test_extended_betting_persists_to_database(self, db_path):
        """Test that extended betting window is saved to database."""
        player_repo = PlayerRepository(db_path)
        match_repo = MatchRepository(db_path)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=False,
        )

        player_ids = list(range(2000, 2010))
        _seed_players(player_repo, player_ids)

        # Shuffle
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)

        # Extend betting
        state = match_service.get_last_shuffle(TEST_GUILD_ID)
        now_ts = int(time.time())
        new_lock_until = now_ts + 1800  # 30 minutes from now

        state.bet_lock_until = new_lock_until
        match_service.set_last_shuffle(TEST_GUILD_ID, state)
        match_service._persist_match_state(TEST_GUILD_ID, state)

        # Verify directly in database (bypassing in-memory cache)
        db_state = match_repo.get_pending_match(TEST_GUILD_ID)
        assert db_state is not None
        assert db_state["bet_lock_until"] == new_lock_until

    def test_extended_betting_survives_restart(self, db_path):
        """Test that extended betting window is restored after restart."""
        # --- Phase 1: Shuffle and extend ---
        player_repo_1 = PlayerRepository(db_path)
        match_repo_1 = MatchRepository(db_path)
        match_service_1 = MatchService(
            player_repo=player_repo_1,
            match_repo=match_repo_1,
            use_glicko=False,
        )

        player_ids = list(range(3000, 3010))
        _seed_players(player_repo_1, player_ids)

        match_service_1.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)

        # Extend betting
        state = match_service_1.get_last_shuffle(TEST_GUILD_ID)
        now_ts = int(time.time())
        new_lock_until = now_ts + 3600  # 1 hour from now

        state.bet_lock_until = new_lock_until
        match_service_1.set_last_shuffle(TEST_GUILD_ID, state)
        match_service_1._persist_match_state(TEST_GUILD_ID, state)

        # --- Phase 2: Simulate restart ---
        player_repo_2 = PlayerRepository(db_path)
        match_repo_2 = MatchRepository(db_path)
        match_service_2 = MatchService(
            player_repo=player_repo_2,
            match_repo=match_repo_2,
            use_glicko=False,
        )

        # Verify extended time is restored
        restored_state = match_service_2.get_last_shuffle(TEST_GUILD_ID)
        assert restored_state is not None
        assert restored_state.bet_lock_until == new_lock_until

    def test_extend_reopens_closed_betting_window(self, db_path):
        """Test that extending betting allows new bets after window was closed."""
        player_repo = PlayerRepository(db_path)
        match_repo = MatchRepository(db_path)
        bet_repo = BetRepository(db_path)
        betting_service = BettingService(bet_repo, player_repo)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=False,
            betting_service=betting_service,
        )

        player_ids = list(range(4000, 4010))
        _seed_players(player_repo, player_ids, balance=100)

        # Create spectator
        spectator_id = 9999
        player_repo.add(
            discord_id=spectator_id,
            discord_username="Spectator",
            guild_id=TEST_GUILD_ID,
            initial_mmr=3000,
            glicko_rating=1500.0,
            glicko_rd=200.0,
            glicko_volatility=0.06,
        )
        player_repo.update_balance(spectator_id, TEST_GUILD_ID, 100)

        # Shuffle
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="house")

        # Set betting window to expired (in the past)
        state = match_service.get_last_shuffle(TEST_GUILD_ID)
        now_ts = int(time.time())
        state.bet_lock_until = now_ts - 60  # Expired 1 minute ago
        state.shuffle_timestamp = now_ts - 600  # Shuffled 10 minutes ago
        match_service._persist_match_state(TEST_GUILD_ID, state)

        # Verify betting is closed
        with pytest.raises(ValueError, match="closed"):
            bet_repo.place_bet_against_pending_match_atomic(
                guild_id=TEST_GUILD_ID,
                discord_id=spectator_id,
                team="radiant",
                amount=10,
                bet_time=now_ts,
            )

        # Extend betting by 10 minutes
        new_lock_until = now_ts + 600
        state.bet_lock_until = new_lock_until
        match_service._persist_match_state(TEST_GUILD_ID, state)

        # Now betting should work
        bet_id = bet_repo.place_bet_against_pending_match_atomic(
            guild_id=TEST_GUILD_ID,
            discord_id=spectator_id,
            team="radiant",
            amount=10,
            bet_time=now_ts,
        )
        assert bet_id is not None

    def test_extend_from_current_time_if_already_expired(self, db_path):
        """Test that extending from expired window uses current time as base."""
        player_repo = PlayerRepository(db_path)
        match_repo = MatchRepository(db_path)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=False,
        )

        player_ids = list(range(5000, 5010))
        _seed_players(player_repo, player_ids)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)

        # Set to expired
        state = match_service.get_last_shuffle(TEST_GUILD_ID)
        now_ts = int(time.time())
        expired_lock = now_ts - 300  # Expired 5 minutes ago
        state.bet_lock_until = expired_lock

        # Calculate new lock using max(current_lock, now) + extension
        # This mirrors the logic in /extendbetting command
        extension_minutes = 5
        base_time = max(expired_lock, now_ts)
        new_lock_until = base_time + (extension_minutes * 60)

        # Should extend from now, not from the expired time
        assert base_time == now_ts  # Base should be now since window expired
        assert new_lock_until > now_ts
        assert new_lock_until == now_ts + 300  # 5 minutes from now

    def test_multiple_extensions_accumulate(self, db_path):
        """Test that multiple extensions can be applied."""
        player_repo = PlayerRepository(db_path)
        match_repo = MatchRepository(db_path)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=False,
        )

        player_ids = list(range(6000, 6010))
        _seed_players(player_repo, player_ids)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)

        state = match_service.get_last_shuffle(TEST_GUILD_ID)
        original_lock = state.bet_lock_until

        # First extension: +5 minutes
        now_ts = int(time.time())
        first_extension = max(original_lock, now_ts) + 300
        state.bet_lock_until = first_extension
        match_service._persist_match_state(TEST_GUILD_ID, state)

        # Second extension: +5 more minutes (from the first extended time)
        state = match_service.get_last_shuffle(TEST_GUILD_ID)
        second_extension = max(state.bet_lock_until, now_ts) + 300
        state.bet_lock_until = second_extension
        match_service._persist_match_state(TEST_GUILD_ID, state)

        # Verify total extension
        final_state = match_service.get_last_shuffle(TEST_GUILD_ID)
        assert final_state.bet_lock_until == second_extension
        assert final_state.bet_lock_until >= original_lock + 600  # At least 10 min more


@pytest.mark.asyncio
async def test_command_extends_only_the_selected_pending_match(monkeypatch):
    """Every command side effect must stay bound to the explicit pending match ID."""
    guild_id = 77
    target = PendingMatchState(
        pending_match_id=22,
        bet_lock_until=1_000,
        betting_mode="pool",
        lobby_kind=LobbyKind.LOWSKILL.value,
        shuffle_message_id=222,
        shuffle_channel_id=2_222,
        shuffle_message_jump_url="https://discord.com/channels/77/2222/222",
    )
    sibling = PendingMatchState(
        pending_match_id=11,
        bet_lock_until=2_000,
        betting_mode="house",
        shuffle_message_id=111,
        shuffle_channel_id=1_111,
    )

    match_service = MagicMock()
    match_service.get_last_shuffle.side_effect = (
        lambda requested_guild_id, pending_match_id=None: (
            target
            if requested_guild_id == guild_id and pending_match_id == target.pending_match_id
            else None
        )
    )
    betting_service = MagicMock()
    betting_service.get_pot_odds.return_value = {"radiant": 30, "dire": 20}
    match_cog = SimpleNamespace(
        _cancel_betting_tasks=MagicMock(),
        _schedule_betting_reminders=AsyncMock(),
    )
    betting_cog = SimpleNamespace(
        _update_shuffle_message_wagers=AsyncMock(),
    )

    embed = discord.Embed()
    embed.add_field(name="💰 Pool Betting", value="old window", inline=False)
    message = SimpleNamespace(embeds=[embed], edit=AsyncMock())
    channel = SimpleNamespace(fetch_message=AsyncMock(return_value=message))
    bot = SimpleNamespace(
        match_service=match_service,
        betting_service=betting_service,
        get_cog=MagicMock(
            side_effect=lambda name: {
                "MatchCommands": match_cog,
                "BettingCommands": betting_cog,
            }.get(name)
        ),
        get_channel=MagicMock(return_value=channel),
    )
    interaction = SimpleNamespace(
        guild=SimpleNamespace(id=guild_id),
        user=SimpleNamespace(id=900, __str__=lambda self: "Admin"),
        response=SimpleNamespace(send_message=AsyncMock()),
    )
    cog = AdminCommands(bot, lobby_service=None, player_service=None)
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _interaction: True)
    monkeypatch.setattr("commands.admin.time.time", lambda: 900)

    await cog.extendbetting.callback(
        cog,
        interaction,
        minutes=5,
        pending_match_id=target.pending_match_id,
    )

    assert target.bet_lock_until == 1_300
    assert sibling.bet_lock_until == 2_000
    match_service.get_last_shuffle.assert_called_once_with(guild_id, target.pending_match_id)
    match_service.set_last_shuffle.assert_called_once_with(guild_id, target)
    match_service._persist_match_state.assert_called_once_with(guild_id, target)
    match_cog._cancel_betting_tasks.assert_called_once_with(
        guild_id,
        pending_match_id=target.pending_match_id,
    )
    match_cog._schedule_betting_reminders.assert_awaited_once_with(
        guild_id,
        1_300,
        pending_match_id=target.pending_match_id,
        lobby_kind=LobbyKind.LOWSKILL,
    )
    betting_cog._update_shuffle_message_wagers.assert_awaited_once_with(
        guild_id,
        pending_match_id=target.pending_match_id,
    )
    sent = interaction.response.send_message.await_args.args[0]
    assert LobbyKind.LOWSKILL.display_name in sent
    assert f"Match #{target.pending_match_id}" in sent
    assert target.shuffle_message_jump_url in sent
