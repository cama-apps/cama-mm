"""Persistence tests for guild-scoped referral enrollment."""

from __future__ import annotations

import json
import sqlite3
import threading
from concurrent.futures import ThreadPoolExecutor
from unittest.mock import AsyncMock, Mock

import pytest

from commands.match import MatchCommands
from commands.registration import RegistrationCommands
from repositories.base_repository import BaseRepository
from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.match_service import MatchService
from services.player_service import PlayerService
from tests.conftest import TEST_GUILD_ID
from utils.formatting import JOPACOIN_EMOTE


def _add_player(player_repository, discord_id: int, guild_id: int = TEST_GUILD_ID) -> None:
    player_repository.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        guild_id=guild_id,
        initial_mmr=1500,
        glicko_rating=1500.0,
        glicko_rd=200.0,
        glicko_volatility=0.06,
    )


def _record_core_match(
    match_repository,
    *,
    team1_ids: list[int],
    team2_ids: list[int],
    guild_id: int = TEST_GUILD_ID,
    pending_match_id: int,
    referral_rewards_out: list[dict] | None = None,
) -> int:
    return match_repository.record_match_core_atomic(
        team1_ids=team1_ids,
        team2_ids=team2_ids,
        winning_team=1,
        guild_id=guild_id,
        dotabuff_match_id=None,
        lobby_type="shuffle",
        lobby_kind="open",
        balancing_rating_system="glicko",
        winning_ids=team1_ids,
        losing_ids=team2_ids,
        glicko_updates=[],
        openskill_updates=[],
        rating_history_rows=[],
        match_prediction={
            "radiant_rating": 1500.0,
            "dire_rating": 1500.0,
            "radiant_rd": 200.0,
            "dire_rd": 200.0,
            "expected_radiant_win_prob": 0.5,
            "openskill_radiant_win_prob": 0.5,
            "openskill_raw_radiant_win_prob": 0.5,
        },
        last_match_date_iso="2026-08-07T00:00:00+00:00",
        first_calibration_ids=[],
        first_calibration_unix=1786051200,
        effective_avoid_ids=[],
        effective_deal_ids=[],
        pending_match_id=pending_match_id,
        referral_rewards_out=referral_rewards_out,
    )


def _referral_ledger_rows(repo_db_path: str) -> list[sqlite3.Row]:
    with sqlite3.connect(repo_db_path) as conn:
        conn.row_factory = sqlite3.Row
        return conn.execute(
            """
            SELECT account_id, delta, source, related_type, related_id, metadata
            FROM economy_ledger_entries
            WHERE source = 'referral_reward'
            ORDER BY ledger_id
            """
        ).fetchall()


def test_referrals_schema_is_guild_scoped(repo_db_path):
    with sqlite3.connect(repo_db_path) as conn:
        columns = {row[1] for row in conn.execute("PRAGMA table_info(referrals)")}

    assert columns == {
        "guild_id",
        "referred_id",
        "referrer_id",
        "created_at",
        "rewarded_match_id",
        "reward_amount",
        "rewarded_at",
    }


def test_create_referral_requires_registered_referrer(player_repository):
    player_repository.add(discord_id=10, discord_username="Recruiter", guild_id=1)

    player_repository.create_referral(10, 20, 1)

    with pytest.raises(ValueError, match="already has a referral"):
        player_repository.create_referral(10, 20, 1)


def test_create_referral_allows_registered_target_without_recorded_games(player_repository):
    player_repository.add(discord_id=10, discord_username="Recruiter", guild_id=1)
    player_repository.add(discord_id=20, discord_username="New player", guild_id=1)

    player_repository.create_referral(10, 20, 1)

    with pytest.raises(ValueError, match="already has a referral"):
        player_repository.create_referral(10, 20, 1)


def test_create_referral_rejects_unregistered_referrer(player_repository):
    with pytest.raises(ValueError, match="Referrer must be registered"):
        player_repository.create_referral(10, 20, 1)


@pytest.mark.parametrize(("wins", "losses"), [(1, 0), (0, 1)])
def test_create_referral_rejects_target_with_recorded_game(
    player_repository, repo_db_path, wins, losses
):
    player_repository.add(discord_id=10, discord_username="Recruiter", guild_id=1)
    player_repository.add(discord_id=20, discord_username="New player", guild_id=1)
    with sqlite3.connect(repo_db_path) as conn:
        conn.execute(
            "UPDATE players SET wins = ?, losses = ? WHERE discord_id = ? AND guild_id = ?",
            (wins, losses, 20, 1),
        )

    with pytest.raises(ValueError, match="already played"):
        player_repository.create_referral(10, 20, 1)


def test_create_referral_rejects_self_referral(player_repository):
    player_repository.add(discord_id=10, discord_username="Recruiter", guild_id=1)

    with pytest.raises(ValueError, match="Self-referrals are not allowed"):
        player_repository.create_referral(10, 10, 1)


def test_create_referral_isolated_by_guild(player_repository):
    player_repository.add(discord_id=10, discord_username="Recruiter", guild_id=1)
    player_repository.add(discord_id=10, discord_username="Recruiter", guild_id=2)

    player_repository.create_referral(10, 20, 1)
    player_repository.create_referral(10, 20, 2)


def test_create_referral_allows_exactly_one_concurrent_enrollment(
    player_repository, repo_db_path
):
    player_repository.add(discord_id=10, discord_username="Recruiter", guild_id=1)
    start = threading.Barrier(2)

    def create_referral() -> str | None:
        contender = PlayerRepository(repo_db_path)
        start.wait(timeout=5)
        try:
            contender.create_referral(10, 20, 1)
            return None
        except ValueError as exc:
            return str(exc)

    with ThreadPoolExecutor(max_workers=2) as executor:
        outcomes = list(executor.map(lambda _index: create_referral(), range(2)))

    assert outcomes.count(None) == 1
    assert outcomes.count("That player already has a referral.") == 1


def test_player_service_creates_referral_through_repository(player_repository):
    player_repository.add(discord_id=10, discord_username="Recruiter", guild_id=1)

    PlayerService(player_repository).create_referral(10, 20, 1)

    with pytest.raises(ValueError, match="already has a referral"):
        player_repository.create_referral(10, 20, 1)


def test_first_recorded_match_atomically_rewards_both_referral_accounts(
    player_repository, match_repository, repo_db_path
):
    _add_player(player_repository, 10)
    _add_player(player_repository, 20)
    player_repository.create_referral(10, 20, TEST_GUILD_ID)
    _add_player(player_repository, 30)
    starting_balances = {
        player_id: player_repository.get_balance(player_id, TEST_GUILD_ID)
        for player_id in (10, 20)
    }

    match_id = _record_core_match(
        match_repository,
        team1_ids=[20],
        team2_ids=[30],
        pending_match_id=1001,
    )

    assert isinstance(match_id, int)
    assert player_repository.get_balance(10, TEST_GUILD_ID) == starting_balances[10] + 100
    assert player_repository.get_balance(20, TEST_GUILD_ID) == starting_balances[20] + 100

    ledger_rows = _referral_ledger_rows(repo_db_path)
    assert [(row["account_id"], row["delta"]) for row in ledger_rows] == [
        (20, 100),
        (10, 100),
    ]
    assert all(row["source"] == "referral_reward" for row in ledger_rows)
    assert all(row["related_type"] == "referral" for row in ledger_rows)
    assert all(row["related_id"] == "20" for row in ledger_rows)
    assert [json.loads(row["metadata"]) for row in ledger_rows] == [
        {
            "match_id": match_id,
            "referrer_id": 10,
            "referred_id": 20,
            "beneficiary_role": "referred",
        },
        {
            "match_id": match_id,
            "referrer_id": 10,
            "referred_id": 20,
            "beneficiary_role": "referrer",
        },
    ]

    with sqlite3.connect(repo_db_path) as conn:
        referral = conn.execute(
            """
            SELECT rewarded_match_id, reward_amount, rewarded_at
            FROM referrals
            WHERE guild_id = ? AND referred_id = ?
            """,
            (TEST_GUILD_ID, 20),
        ).fetchone()
        ledger_context_count = conn.execute(
            "SELECT COUNT(*) FROM economy_ledger_context"
        ).fetchone()[0]
    assert referral[0] == match_id
    assert referral[1] == 100
    assert isinstance(referral[2], int)
    assert ledger_context_count == 0
    assert match_repository.get_referral_rewards_for_match(
        match_id, TEST_GUILD_ID
    ) == [
        {
            "referred_id": 20,
            "referrer_id": 10,
            "reward_amount": 100,
        }
    ]
def test_later_match_does_not_pay_referral_again(
    player_repository, match_repository, repo_db_path
):
    _add_player(player_repository, 40)
    player_repository.create_referral(40, 41, TEST_GUILD_ID)
    _add_player(player_repository, 41)
    _add_player(player_repository, 42)
    starting_balances = {
        player_id: player_repository.get_balance(player_id, TEST_GUILD_ID)
        for player_id in (40, 41)
    }

    first_match_id = _record_core_match(
        match_repository,
        team1_ids=[41],
        team2_ids=[42],
        pending_match_id=1002,
    )
    second_match_id = _record_core_match(
        match_repository,
        team1_ids=[41],
        team2_ids=[42],
        pending_match_id=1003,
    )

    assert player_repository.get_balance(40, TEST_GUILD_ID) == starting_balances[40] + 100
    assert player_repository.get_balance(41, TEST_GUILD_ID) == starting_balances[41] + 100
    assert len(_referral_ledger_rows(repo_db_path)) == 2
    assert match_repository.get_referral_rewards_for_match(
        first_match_id, TEST_GUILD_ID
    ) == [
        {
            "referred_id": 41,
            "referrer_id": 40,
            "reward_amount": 100,
        }
    ]
    assert match_repository.get_referral_rewards_for_match(
        second_match_id, TEST_GUILD_ID
    ) == []


def test_idempotent_match_retry_does_not_pay_referral_again(
    player_repository, match_repository, repo_db_path
):
    _add_player(player_repository, 50)
    player_repository.create_referral(50, 51, TEST_GUILD_ID)
    _add_player(player_repository, 51)
    _add_player(player_repository, 52)
    starting_balances = {
        player_id: player_repository.get_balance(player_id, TEST_GUILD_ID)
        for player_id in (50, 51)
    }
    first_rewards: list[dict] = []
    retried_rewards: list[dict] = []

    first_match_id = _record_core_match(
        match_repository,
        team1_ids=[51],
        team2_ids=[52],
        pending_match_id=1004,
        referral_rewards_out=first_rewards,
    )
    retried_match_id = _record_core_match(
        match_repository,
        team1_ids=[51],
        team2_ids=[52],
        pending_match_id=1004,
        referral_rewards_out=retried_rewards,
    )

    assert retried_match_id == first_match_id
    expected_rewards = [
        {
            "referred_id": 51,
            "referrer_id": 50,
            "reward_amount": 100,
        }
    ]
    assert first_rewards == expected_rewards
    assert retried_rewards == expected_rewards
    assert player_repository.get_balance(50, TEST_GUILD_ID) == starting_balances[50] + 100
    assert player_repository.get_balance(51, TEST_GUILD_ID) == starting_balances[51] + 100
    assert len(_referral_ledger_rows(repo_db_path)) == 2
    with sqlite3.connect(repo_db_path) as conn:
        assert conn.execute("SELECT COUNT(*) FROM matches").fetchone()[0] == 1


def test_result_correction_does_not_add_or_reverse_referral_rewards(
    player_repository, match_repository, repo_db_path
):
    _add_player(player_repository, 60)
    player_repository.create_referral(60, 61, TEST_GUILD_ID)
    participant_ids = list(range(61, 71))
    for player_id in participant_ids:
        _add_player(player_repository, player_id)

    match_service = MatchService(
        player_repo=player_repository,
        match_repo=match_repository,
    )
    match_service.shuffle_players(participant_ids, guild_id=TEST_GUILD_ID)
    recorded = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
    match_id = recorded["match_id"]
    balances_after_record = {
        player_id: player_repository.get_balance(player_id, TEST_GUILD_ID)
        for player_id in (60, 61)
    }

    assert recorded["referral_rewards"] == [
        {
            "referred_id": 61,
            "referrer_id": 60,
            "reward_amount": 100,
        }
    ]
    expected_referral_changes = {
        61: {"referral": 100},
        60: {"referral": 100},
    }
    assert recorded["jc_changes"] == expected_referral_changes
    assert match_repository.get_match_jc_changes(
        match_id, TEST_GUILD_ID
    ) == expected_referral_changes
    match_service.correct_match_result(
        match_id=match_id,
        new_winning_team="dire",
        guild_id=TEST_GUILD_ID,
        corrected_by=999,
    )

    assert {
        player_id: player_repository.get_balance(player_id, TEST_GUILD_ID)
        for player_id in (60, 61)
    } == balances_after_record
    assert len(_referral_ledger_rows(repo_db_path)) == 2
    assert match_repository.get_referral_rewards_for_match(
        match_id, TEST_GUILD_ID
    ) == [
        {
            "referred_id": 61,
            "referrer_id": 60,
            "reward_amount": 100,
        }
    ]
    assert match_repository.get_match_jc_changes(
        match_id, TEST_GUILD_ID
    ) == expected_referral_changes


def test_referral_recording_preserves_six_connection_path(repo_db_path, monkeypatch):
    player_repository = PlayerRepository(repo_db_path)
    match_repository = MatchRepository(repo_db_path)
    _add_player(player_repository, 100)
    player_repository.create_referral(100, 101, TEST_GUILD_ID)
    participant_ids = list(range(101, 111))
    for player_id in participant_ids:
        _add_player(player_repository, player_id)

    match_service = MatchService(
        player_repo=player_repository,
        match_repo=match_repository,
        use_glicko=False,
        betting_service=None,
    )
    match_service.shuffle_players(participant_ids, guild_id=TEST_GUILD_ID)

    connection_count = 0
    original_get_connection = BaseRepository.get_connection

    def counted_get_connection(repository):
        nonlocal connection_count
        connection_count += 1
        return original_get_connection(repository)

    monkeypatch.setattr(BaseRepository, "get_connection", counted_get_connection)

    result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

    assert result["referral_rewards"] == [
        {
            "referred_id": 101,
            "referrer_id": 100,
            "reward_amount": 100,
        }
    ]
    assert connection_count == 6


def test_referral_jc_component_survives_post_match_bonus_accounting(repo_db_path):
    player_repository = PlayerRepository(repo_db_path)
    match_repository = MatchRepository(repo_db_path)
    betting_service = BettingService(
        BetRepository(repo_db_path),
        player_repository,
    )
    _add_player(player_repository, 110)
    player_repository.create_referral(110, 111, TEST_GUILD_ID)
    participant_ids = list(range(111, 121))
    for player_id in participant_ids:
        _add_player(player_repository, player_id)

    match_service = MatchService(
        player_repo=player_repository,
        match_repo=match_repository,
        use_glicko=False,
        betting_service=betting_service,
    )
    match_service.shuffle_players(participant_ids, guild_id=TEST_GUILD_ID)
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)

    result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

    expected_payout = 10 if 111 in pending.radiant_team_ids else 5
    assert result["jc_changes"][111] == {
        "referral": 100,
        "payout": expected_payout,
    }
    assert result["jc_changes"][110] == {"referral": 100}
    stored_changes = match_repository.get_match_jc_changes(
        result["match_id"], TEST_GUILD_ID
    )
    assert stored_changes[111] == result["jc_changes"][111]
    assert stored_changes[110] == result["jc_changes"][110]


def test_missing_referrer_skips_partial_reward_but_commits_match(
    player_repository, match_repository, repo_db_path
):
    _add_player(player_repository, 70)
    player_repository.create_referral(70, 71, TEST_GUILD_ID)
    with sqlite3.connect(repo_db_path) as conn:
        conn.execute(
            "DELETE FROM players WHERE discord_id = ? AND guild_id = ?",
            (70, TEST_GUILD_ID),
        )
    _add_player(player_repository, 71)
    _add_player(player_repository, 72)
    starting_balance = player_repository.get_balance(71, TEST_GUILD_ID)

    match_id = _record_core_match(
        match_repository,
        team1_ids=[71],
        team2_ids=[72],
        pending_match_id=1005,
    )

    assert match_repository.get_match(match_id, TEST_GUILD_ID) is not None
    assert player_repository.get_balance(71, TEST_GUILD_ID) == starting_balance
    assert _referral_ledger_rows(repo_db_path) == []
    with sqlite3.connect(repo_db_path) as conn:
        referral = conn.execute(
            """
            SELECT rewarded_match_id, reward_amount, rewarded_at
            FROM referrals
            WHERE guild_id = ? AND referred_id = ?
            """,
            (TEST_GUILD_ID, 71),
        ).fetchone()
    assert referral == (None, None, None)


def test_missing_referred_account_skips_referrer_reward_but_commits_match(
    player_repository, match_repository, repo_db_path
):
    _add_player(player_repository, 73)
    player_repository.create_referral(73, 74, TEST_GUILD_ID)
    _add_player(player_repository, 75)
    starting_balance = player_repository.get_balance(73, TEST_GUILD_ID)

    match_id = _record_core_match(
        match_repository,
        team1_ids=[74],
        team2_ids=[75],
        pending_match_id=1008,
    )

    assert match_repository.get_match(match_id, TEST_GUILD_ID) is not None
    assert player_repository.get_balance(73, TEST_GUILD_ID) == starting_balance
    assert _referral_ledger_rows(repo_db_path) == []
    with sqlite3.connect(repo_db_path) as conn:
        referral = conn.execute(
            """
            SELECT rewarded_match_id, reward_amount, rewarded_at
            FROM referrals
            WHERE guild_id = ? AND referred_id = ?
            """,
            (TEST_GUILD_ID, 74),
        ).fetchone()
    assert referral == (None, None, None)


def test_same_match_settles_multiple_referrals_independently(
    player_repository, match_repository, repo_db_path
):
    for referrer_id in (80, 81):
        _add_player(player_repository, referrer_id)
    player_repository.create_referral(80, 82, TEST_GUILD_ID)
    player_repository.create_referral(81, 83, TEST_GUILD_ID)
    for player_id in (82, 83, 84):
        _add_player(player_repository, player_id)
    starting_balances = {
        player_id: player_repository.get_balance(player_id, TEST_GUILD_ID)
        for player_id in (80, 81, 82, 83)
    }

    match_id = _record_core_match(
        match_repository,
        team1_ids=[82, 83],
        team2_ids=[84],
        pending_match_id=1006,
    )

    assert {
        player_id: player_repository.get_balance(player_id, TEST_GUILD_ID)
        for player_id in (80, 81, 82, 83)
    } == {
        player_id: starting_balances[player_id] + 100
        for player_id in (80, 81, 82, 83)
    }
    assert len(_referral_ledger_rows(repo_db_path)) == 4
    assert match_repository.get_referral_rewards_for_match(
        match_id, TEST_GUILD_ID
    ) == [
        {
            "referred_id": 82,
            "referrer_id": 80,
            "reward_amount": 100,
        },
        {
            "referred_id": 83,
            "referrer_id": 81,
            "reward_amount": 100,
        },
    ]


def test_referral_settlement_is_guild_scoped(
    player_repository, match_repository, repo_db_path
):
    for guild_id in (TEST_GUILD_ID, TEST_GUILD_ID + 1):
        _add_player(player_repository, 90, guild_id)
    player_repository.create_referral(90, 91, TEST_GUILD_ID)
    _add_player(player_repository, 91, TEST_GUILD_ID + 1)
    _add_player(player_repository, 92, TEST_GUILD_ID + 1)
    starting_balances = {
        (player_id, guild_id): player_repository.get_balance(player_id, guild_id)
        for player_id, guild_id in (
            (90, TEST_GUILD_ID),
            (90, TEST_GUILD_ID + 1),
            (91, TEST_GUILD_ID + 1),
        )
    }

    match_id = _record_core_match(
        match_repository,
        team1_ids=[91],
        team2_ids=[92],
        guild_id=TEST_GUILD_ID + 1,
        pending_match_id=1007,
    )

    assert (
        player_repository.get_balance(90, TEST_GUILD_ID)
        == starting_balances[(90, TEST_GUILD_ID)]
    )
    assert (
        player_repository.get_balance(90, TEST_GUILD_ID + 1)
        == starting_balances[(90, TEST_GUILD_ID + 1)]
    )
    assert (
        player_repository.get_balance(91, TEST_GUILD_ID + 1)
        == starting_balances[(91, TEST_GUILD_ID + 1)]
    )
    assert _referral_ledger_rows(repo_db_path) == []
    assert match_repository.get_referral_rewards_for_match(
        match_id, TEST_GUILD_ID
    ) == []


class TestReferralRewardFormatting:
    def test_referral_rewards_join_the_netted_jc_summary(self):
        assert MatchCommands._format_jc_changes(
            None,
            {
                "winning_player_ids": [20],
                "losing_player_ids": [],
                "excluded_player_ids": [],
                "jc_changes": {
                    20: {"payout": 10, "referral": 100},
                    10: {"referral": 100},
                },
            },
        ) == (
            "\n\n🪙 **JC Changes:**\n"
            "**Match Players:**\n"
            f"<@20>: **+110** {JOPACOIN_EMOTE} (win +10, referral +100)\n"
            "**Other Rewards:**\n"
            f"<@10>: **+100** {JOPACOIN_EMOTE} (referral +100)"
        )


class TestReferCommand:
    """Callback behavior for the public /refer onboarding command."""

    @staticmethod
    def _make_interaction(user_id: int = 10):
        interaction = Mock()
        interaction.user.id = user_id
        interaction.guild.id = TEST_GUILD_ID
        interaction.response = AsyncMock()
        interaction.followup = AsyncMock()
        return interaction

    @staticmethod
    def _make_member(member_id: int = 20, *, bot: bool = False):
        member = Mock()
        member.id = member_id
        member.bot = bot
        member.mention = f"<@{member_id}>"
        return member

    @staticmethod
    def _make_cog(player_service=None):
        return RegistrationCommands(bot=Mock(), player_service=player_service or Mock())

    @pytest.mark.asyncio
    async def test_referral_uses_private_ack_before_public_onboarding(
        self, monkeypatch
    ):
        monkeypatch.setattr("commands.registration.LOBBY_CHANNEL_ID", 111)
        monkeypatch.setattr(
            "commands.registration.LOWSKILL_LOBBY_CHANNEL_ID", 222
        )
        player_service = Mock()
        cog = self._make_cog(player_service)
        interaction = self._make_interaction()
        lobby_channel = Mock(mention="<#111>")
        jv_lobby_channel = Mock(mention="<#222>")
        interaction.guild.get_channel.side_effect = {
            111: lobby_channel,
            222: jv_lobby_channel,
        }.get
        player = self._make_member()

        await cog.refer.callback(cog, interaction, player)

        player_service.create_referral.assert_called_once_with(10, 20, TEST_GUILD_ID)
        interaction.response.defer.assert_awaited_once_with(ephemeral=True, thinking=False)
        assert interaction.followup.send.await_count == 2

        private_ack = interaction.followup.send.await_args_list[0].kwargs
        assert private_ack["ephemeral"] is True
        assert private_ack["content"] == "✅ Referral created."

        kwargs = interaction.followup.send.await_args_list[1].kwargs
        assert kwargs["ephemeral"] is False
        assert kwargs["content"] == (
            "**Referral: <@20>**\n"
            "1. If needed, run `/player register` with your Steam32 ID "
            "(the number in your Dotabuff URL).\n"
            "2. Run `/player roles` with your Dota positions (1-5).\n"
            "3. React in <#111> or <#222> to join an active lobby, "
            "or run `/lobby` to start one."
        )
        allowed_mentions = kwargs["allowed_mentions"]
        assert allowed_mentions.users == [player]
        assert allowed_mentions.roles is False
        assert allowed_mentions.everyone is False
        assert allowed_mentions.replied_user is False

    @pytest.mark.asyncio
    async def test_referral_onboarding_falls_back_when_lobby_channels_do_not_resolve(
        self, monkeypatch
    ):
        monkeypatch.setattr("commands.registration.LOBBY_CHANNEL_ID", 111)
        monkeypatch.setattr(
            "commands.registration.LOWSKILL_LOBBY_CHANNEL_ID", 222
        )
        cog = self._make_cog(Mock())
        interaction = self._make_interaction()
        interaction.guild.get_channel.return_value = None

        await cog.refer.callback(cog, interaction, self._make_member())

        public_onboarding = interaction.followup.send.await_args_list[1].kwargs["content"]
        assert public_onboarding.endswith(
            "3. React in a lobby channel to join an active lobby, "
            "or run `/lobby` to start one."
        )

    @pytest.mark.asyncio
    async def test_bot_target_is_rejected_ephemerally(self):
        player_service = Mock()
        cog = self._make_cog(player_service)
        interaction = self._make_interaction()

        await cog.refer.callback(cog, interaction, self._make_member(bot=True))

        player_service.create_referral.assert_not_called()
        kwargs = interaction.followup.send.await_args.kwargs
        assert kwargs["ephemeral"] is True
        assert "human players" in kwargs["content"]

    @pytest.mark.asyncio
    async def test_self_target_is_rejected_ephemerally(self):
        player_service = Mock()
        cog = self._make_cog(player_service)
        interaction = self._make_interaction()

        await cog.refer.callback(cog, interaction, self._make_member(member_id=10))

        player_service.create_referral.assert_not_called()
        kwargs = interaction.followup.send.await_args.kwargs
        assert kwargs["ephemeral"] is True
        assert "yourself" in kwargs["content"]

    @pytest.mark.asyncio
    async def test_unregistered_referrer_error_is_ephemeral(self):
        player_service = Mock()
        player_service.create_referral.side_effect = ValueError(
            "Referrer must be registered in this guild."
        )
        cog = self._make_cog(player_service)
        interaction = self._make_interaction()
        player = self._make_member()

        await cog.refer.callback(cog, interaction, player)

        player_service.create_referral.assert_called_once_with(10, 20, TEST_GUILD_ID)
        kwargs = interaction.followup.send.await_args.kwargs
        assert kwargs["ephemeral"] is True
        assert "Referrer must be registered" in kwargs["content"]

    @pytest.mark.asyncio
    async def test_played_target_error_is_ephemeral(self):
        player_service = Mock()
        player_service.create_referral.side_effect = ValueError(
            "That player has already played a game in this guild."
        )
        cog = self._make_cog(player_service)
        interaction = self._make_interaction()
        player = self._make_member()

        await cog.refer.callback(cog, interaction, player)

        player_service.create_referral.assert_called_once_with(10, 20, TEST_GUILD_ID)
        kwargs = interaction.followup.send.await_args.kwargs
        assert kwargs["ephemeral"] is True
        assert "already played" in kwargs["content"]

    @pytest.mark.asyncio
    async def test_duplicate_referral_error_does_not_expose_original_referrer(self):
        player_service = Mock()
        player_service.create_referral.side_effect = ValueError("That player already has a referral.")
        cog = self._make_cog(player_service)
        interaction = self._make_interaction()
        player = self._make_member()

        await cog.refer.callback(cog, interaction, player)

        player_service.create_referral.assert_called_once_with(10, 20, TEST_GUILD_ID)
        kwargs = interaction.followup.send.await_args.kwargs
        assert kwargs["ephemeral"] is True
        assert kwargs["content"] == "❌ That player already has a referral."
