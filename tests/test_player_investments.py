from concurrent.futures import ThreadPoolExecutor
from datetime import UTC, datetime
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest
from discord import app_commands

from commands.betting import BettingCommands
from commands.betting_helpers import economy_actions
from repositories.bet_repository import BetRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY


def _investment_services(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    bet_repo = BetRepository(repo_db_path)
    return player_repo, bet_repo, BettingService(bet_repo, player_repo)


def _seed_balances(player_repo, balances):
    for discord_id, balance in balances.items():
        player_repo.add(
            discord_id=discord_id,
            discord_username=f"Player{discord_id}",
            guild_id=TEST_GUILD_ID,
        )
        player_repo.update_balance(discord_id, TEST_GUILD_ID, balance)


def test_investment_replaces_one_position_per_target(bet_repository):
    bet_repository.set_investment(
        TEST_GUILD_ID,
        investor_id=101,
        target_id=201,
        direction="long",
        percentage=10,
    )
    bet_repository.set_investment(
        TEST_GUILD_ID,
        investor_id=101,
        target_id=201,
        direction="short",
        percentage=7,
    )

    assert bet_repository.get_investments(TEST_GUILD_ID, investor_id=101) == [
        {
            "guild_id": TEST_GUILD_ID,
            "investor_id": 101,
            "target_id": 201,
            "direction": "short",
            "percentage": 7,
        }
    ]


def test_investment_enforces_per_target_and_total_percentage_caps_atomically(
    bet_repository,
):
    for target_id in range(201, 206):
        bet_repository.set_investment(
            TEST_GUILD_ID,
            investor_id=101,
            target_id=target_id,
            direction="long",
            percentage=10,
        )

    with pytest.raises(ValueError, match="between 1% and 10%"):
        bet_repository.set_investment(
            TEST_GUILD_ID,
            investor_id=101,
            target_id=206,
            direction="long",
            percentage=11,
        )

    with pytest.raises(ValueError, match="cannot exceed 50%"):
        bet_repository.set_investment(
            TEST_GUILD_ID,
            investor_id=101,
            target_id=206,
            direction="short",
            percentage=1,
        )

    positions = bet_repository.get_investments(TEST_GUILD_ID, investor_id=101)
    assert len(positions) == 5
    assert sum(position["percentage"] for position in positions) == 50


def test_concurrent_investment_updates_cannot_exceed_total_cap(bet_repository):
    for target_id in range(201, 205):
        bet_repository.set_investment(
            TEST_GUILD_ID,
            investor_id=101,
            target_id=target_id,
            direction="long",
            percentage=10,
        )

    def add_position(target_id):
        try:
            bet_repository.set_investment(
                TEST_GUILD_ID,
                investor_id=101,
                target_id=target_id,
                direction="short",
                percentage=10,
            )
        except ValueError:
            return "rejected"
        return "created"

    with ThreadPoolExecutor(max_workers=2) as executor:
        outcomes = list(executor.map(add_position, (205, 206)))

    assert sorted(outcomes) == ["created", "rejected"]
    positions = bet_repository.get_investments(TEST_GUILD_ID, investor_id=101)
    assert sum(position["percentage"] for position in positions) == 50


def test_investment_remove_is_free_and_guild_scoped(bet_repository):
    for guild_id in (TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY):
        bet_repository.set_investment(
            guild_id,
            investor_id=101,
            target_id=201,
            direction="long",
            percentage=6,
        )

    assert bet_repository.remove_investment(
        TEST_GUILD_ID,
        investor_id=101,
        target_id=201,
    )
    assert not bet_repository.remove_investment(
        TEST_GUILD_ID,
        investor_id=101,
        target_id=201,
    )
    assert bet_repository.get_investments(TEST_GUILD_ID, investor_id=101) == []
    assert len(
        bet_repository.get_investments(TEST_GUILD_ID_SECONDARY, investor_id=101)
    ) == 1


def test_investment_bets_resolve_long_short_without_betting_against_self(
    repo_db_path,
):
    player_repo, bet_repo, betting_service = _investment_services(repo_db_path)
    _seed_balances(
        player_repo,
        {
            1: 100,
            2: 100,
            3: 100,
            4: 100,
            101: 100,
            102: 100,
        },
    )
    positions = [
        (101, 1, "long", 10),  # Spectator backs the target's Radiant team.
        (102, 1, "short", 8),  # Spectator fades the target onto Dire.
        (1, 1, "long", 7),  # A participant may invest long in themself.
        (2, 3, "short", 6),  # Fading an opponent backs the investor's team.
        (3, 1, "long", 5),  # Backing an opponent would oppose the investor.
        (4, 3, "short", 4),  # Fading a teammate would oppose the investor.
    ]
    for investor_id, target_id, direction, percentage in positions:
        bet_repo.set_investment(
            TEST_GUILD_ID,
            investor_id=investor_id,
            target_id=target_id,
            direction=direction,
            percentage=percentage,
        )

    prepared = betting_service.prepare_investments(
        TEST_GUILD_ID,
        target_ids=[1, 2, 3, 4],
    )
    result = betting_service.create_investment_bets(
        guild_id=TEST_GUILD_ID,
        radiant_ids=[1, 2],
        dire_ids=[3, 4],
        shuffle_timestamp=1_700_000_000,
        prepared_investments=prepared,
    )

    assert [
        (
            bet["investor_id"],
            bet["target_id"],
            bet["direction"],
            bet["team"],
            bet["amount"],
        )
        for bet in result["bets"]
    ] == [
        (1, 1, "long", "radiant", 7),
        (2, 3, "short", "radiant", 6),
        (101, 1, "long", "radiant", 10),
        (102, 1, "short", "dire", 8),
    ]
    assert [skip["investor_id"] for skip in result["skipped"]] == [3, 4]
    assert player_repo.get_balance(1, TEST_GUILD_ID) == 93
    assert player_repo.get_balance(2, TEST_GUILD_ID) == 94
    assert player_repo.get_balance(3, TEST_GUILD_ID) == 100
    assert player_repo.get_balance(4, TEST_GUILD_ID) == 100
    assert player_repo.get_balance(101, TEST_GUILD_ID) == 90
    assert player_repo.get_balance(102, TEST_GUILD_ID) == 92


def test_investment_bets_require_fifty_at_shuffle_start_and_floor_post_blind_balance(
    repo_db_path,
):
    player_repo, bet_repo, betting_service = _investment_services(repo_db_path)
    _seed_balances(player_repo, {1: 50, 2: 49, 3: 100})
    bet_repo.set_investment(
        TEST_GUILD_ID,
        investor_id=1,
        target_id=1,
        direction="long",
        percentage=10,
    )
    bet_repo.set_investment(
        TEST_GUILD_ID,
        investor_id=2,
        target_id=1,
        direction="long",
        percentage=10,
    )
    bet_repo.set_investment(
        TEST_GUILD_ID,
        investor_id=3,
        target_id=1,
        direction="short",
        percentage=7,
    )

    prepared = betting_service.prepare_investments(
        TEST_GUILD_ID,
        target_ids=[1, 2],
    )
    # Model the existing blind bet having already reduced participant balances.
    player_repo.update_balance(1, TEST_GUILD_ID, 45)
    player_repo.update_balance(2, TEST_GUILD_ID, 44)

    result = betting_service.create_investment_bets(
        guild_id=TEST_GUILD_ID,
        radiant_ids=[1, 2],
        dire_ids=[],
        shuffle_timestamp=1_700_000_001,
        prepared_investments=prepared,
    )

    assert [
        (bet["investor_id"], bet["amount"], bet["team"])
        for bet in result["bets"]
    ] == [
        (1, 4, "radiant"),
        (3, 7, "dire"),
    ]
    assert result["skipped"] == [
        {"investor_id": 2, "target_id": 1, "reason": "shuffle-start balance 49 < 50"}
    ]


def test_configure_investment_rejects_self_short_and_unregistered_target(repo_db_path):
    player_repo, _bet_repo, betting_service = _investment_services(repo_db_path)
    _seed_balances(player_repo, {1: 100})

    with pytest.raises(ValueError, match="cannot short yourself"):
        betting_service.configure_investment(
            TEST_GUILD_ID,
            investor_id=1,
            target_id=1,
            direction="short",
            percentage=5,
        )
    with pytest.raises(ValueError, match="Target player is not registered"):
        betting_service.configure_investment(
            TEST_GUILD_ID,
            investor_id=1,
            target_id=999,
            direction="long",
            percentage=5,
        )


@pytest.mark.asyncio
async def test_invest_action_sets_lists_and_removes_a_position(repo_db_path):
    player_repo, _bet_repo, betting_service = _investment_services(repo_db_path)
    _seed_balances(player_repo, {1: 100, 2: 100})
    cog = SimpleNamespace(betting_service=betting_service)
    interaction = MagicMock()
    interaction.guild = SimpleNamespace(id=TEST_GUILD_ID)
    interaction.user = SimpleNamespace(id=1)
    interaction.response = SimpleNamespace(send_message=AsyncMock())
    invest_action = economy_actions.invest_action

    await invest_action(
        cog,
        interaction,
        action=SimpleNamespace(value="set"),
        player=SimpleNamespace(id=2),
        direction=SimpleNamespace(value="long"),
        percentage=7,
    )

    assert betting_service.get_investments(TEST_GUILD_ID, investor_id=1)[0][
        "percentage"
    ] == 7
    assert "7% / 50%" in interaction.response.send_message.await_args.args[0]

    interaction.response.send_message.reset_mock()
    await invest_action(
        cog,
        interaction,
        action=SimpleNamespace(value="list"),
        player=None,
        direction=None,
        percentage=None,
    )
    listed = interaction.response.send_message.await_args.args[0]
    assert "LONG <@2> — 7%" in listed
    assert "Total: **7% / 50%**" in listed

    interaction.response.send_message.reset_mock()
    await invest_action(
        cog,
        interaction,
        action=SimpleNamespace(value="remove"),
        player=SimpleNamespace(id=2),
        direction=None,
        percentage=None,
    )
    assert betting_service.get_investments(TEST_GUILD_ID, investor_id=1) == []
    assert "Removed" in interaction.response.send_message.await_args.args[0]


@pytest.mark.asyncio
async def test_invest_action_lists_and_removes_a_departed_target_by_position(
    repo_db_path,
):
    player_repo, _bet_repo, betting_service = _investment_services(repo_db_path)
    _seed_balances(player_repo, {1: 100, 2: 100})
    betting_service.configure_investment(
        TEST_GUILD_ID,
        investor_id=1,
        target_id=2,
        direction="long",
        percentage=7,
    )
    cog = SimpleNamespace(betting_service=betting_service)
    interaction = MagicMock()
    interaction.guild = SimpleNamespace(id=TEST_GUILD_ID)
    interaction.user = SimpleNamespace(id=1)
    interaction.response = SimpleNamespace(send_message=AsyncMock())

    await economy_actions.invest_action(
        cog,
        interaction,
        action=SimpleNamespace(value="list"),
        player=None,
        direction=None,
        percentage=None,
    )
    assert "1. LONG <@2> — 7%" in interaction.response.send_message.await_args.args[0]

    interaction.response.send_message.reset_mock()
    try:
        await economy_actions.invest_action(
            cog,
            interaction,
            action=SimpleNamespace(value="remove"),
            player=None,
            position=1,
            direction=None,
            percentage=None,
        )
    except TypeError as exc:
        pytest.fail(f"numbered removal is not supported: {exc}")

    assert betting_service.get_investments(TEST_GUILD_ID, investor_id=1) == []
    assert "Removed" in interaction.response.send_message.await_args.args[0]


@pytest.mark.asyncio
async def test_invest_command_limits_each_user_to_five_calls_per_ten_seconds():
    command = BettingCommands.invest
    assert len(command.checks) == 1, "the invest command has no cooldown check"
    check = command.checks[0]
    interaction = SimpleNamespace(
        user=SimpleNamespace(id=9_876_543_210),
        created_at=datetime(2026, 8, 9, 12, 0, tzinfo=UTC),
    )

    for _ in range(5):
        assert await check(interaction)
    with pytest.raises(app_commands.CommandOnCooldown):
        await check(interaction)


@pytest.mark.asyncio
async def test_invest_command_reports_cooldown_retry_time():
    error_handler = BettingCommands.invest.on_error
    assert error_handler is not None, "the invest cooldown has no user-facing handler"
    interaction = SimpleNamespace(
        response=SimpleNamespace(send_message=AsyncMock()),
    )
    error = app_commands.CommandOnCooldown(
        app_commands.Cooldown(5, 10),
        retry_after=4.2,
    )

    await error_handler(SimpleNamespace(), interaction, error)

    assert interaction.response.send_message.await_args.args[0] == (
        "Slow down! Try configuring investments again in 4s."
    )
    assert interaction.response.send_message.await_args.kwargs == {"ephemeral": True}


def test_match_automatic_bets_size_investment_after_participant_blind(repo_db_path):
    player_repo, bet_repo, betting_service = _investment_services(repo_db_path)
    player_ids = list(range(1, 11))
    _seed_balances(player_repo, dict.fromkeys(player_ids, 100))
    bet_repo.set_investment(
        TEST_GUILD_ID,
        investor_id=1,
        target_id=1,
        direction="long",
        percentage=10,
    )

    result = betting_service.create_match_automatic_bets(
        guild_id=TEST_GUILD_ID,
        radiant_ids=player_ids[:5],
        dire_ids=player_ids[5:],
        shuffle_timestamp=1_700_000_002,
    )

    player_one_blind = next(
        bet for bet in result["blind_bets"]["bets"] if bet["discord_id"] == 1
    )
    player_one_investment = result["investment_bets"]["bets"][0]
    assert player_one_blind["amount"] == 10
    assert player_one_investment["amount"] == 9
    assert player_repo.get_balance(1, TEST_GUILD_ID) == 81
