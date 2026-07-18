"""Focused daily economy-event hooks for match bets and /gamba."""

import time
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from commands.betting_helpers.economy_actions import (
    apply_gamba_event_multiplier,
    bounded_economy_multiplier,
    get_gamba_event_multipliers,
)
from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID


class _EventService:
    def __init__(self, **effects):
        self.effects = SimpleNamespace(**effects)

    def get_effects(self, guild_id):
        assert guild_id == TEST_GUILD_ID
        return self.effects


def _seed_player(player_repo, discord_id, *, balance=100):
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        guild_id=TEST_GUILD_ID,
        initial_mmr=1500,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repo.add_balance(discord_id, TEST_GUILD_ID, balance)


def _services(repo_db_path, *, mode, payout_multiplier):
    player_repo = PlayerRepository(repo_db_path)
    bet_repo = BetRepository(repo_db_path)
    match_service = MatchService(
        player_repo=player_repo,
        match_repo=MatchRepository(repo_db_path),
        use_glicko=True,
    )
    event_service = _EventService(bet_payout_multiplier=payout_multiplier)
    betting_service = BettingService(
        bet_repo,
        player_repo,
        economy_event_service=event_service,
    )
    match_service.betting_service = betting_service

    participants = list(range(81000, 81010))
    for player_id in participants:
        _seed_player(player_repo, player_id, balance=0)
    match_service.shuffle_players(
        participants,
        guild_id=TEST_GUILD_ID,
        betting_mode=mode,
    )
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)
    pending.bet_lock_until = int(time.time()) + 600
    return player_repo, betting_service, pending


def test_house_event_scales_winning_gross_payout(repo_db_path):
    player_repo, betting_service, pending = _services(
        repo_db_path, mode="house", payout_multiplier=0.5
    )
    winner = 81100
    _seed_player(player_repo, winner)

    betting_service.place_bet(TEST_GUILD_ID, winner, "radiant", 20, pending)
    post_stake = player_repo.get_balance(winner, TEST_GUILD_ID)
    result = betting_service.settle_bets(
        9910, TEST_GUILD_ID, "radiant", pending_state=pending
    )

    # Normal gross payout is 40; the event scales that payout to 20 without
    # changing the 20-JC stake that was already placed.
    assert result["winners"][0]["payout"] == 20
    assert player_repo.get_balance(winner, TEST_GUILD_ID) == post_stake + 20


def test_pool_event_scales_once_per_winner_and_persists_payout(repo_db_path):
    player_repo, betting_service, pending = _services(
        repo_db_path, mode="pool", payout_multiplier=0.5
    )
    winner, loser = 81200, 81201
    _seed_player(player_repo, winner)
    _seed_player(player_repo, loser)

    betting_service.place_bet(TEST_GUILD_ID, winner, "radiant", 30, pending)
    betting_service.place_bet(TEST_GUILD_ID, loser, "dire", 20, pending)
    post_stake = player_repo.get_balance(winner, TEST_GUILD_ID)
    result = betting_service.settle_bets(
        9911, TEST_GUILD_ID, "radiant", pending_state=pending
    )

    # The normal 50-JC pool payout is scaled to 25. Returned data, credited
    # balance, and the stored bet payout all agree.
    assert result["winners"][0]["payout"] == 25
    assert player_repo.get_balance(winner, TEST_GUILD_ID) == post_stake + 25
    with player_repo.connection() as conn:
        stored = conn.execute(
            "SELECT payout FROM bets WHERE discord_id = ? AND match_id = ?",
            (winner, 9911),
        ).fetchone()
    assert stored["payout"] == 25


def test_invalid_bet_event_multiplier_falls_back_to_default(repo_db_path):
    player_repo, betting_service, pending = _services(
        repo_db_path, mode="house", payout_multiplier=float("nan")
    )
    winner = 81300
    _seed_player(player_repo, winner)
    betting_service.place_bet(TEST_GUILD_ID, winner, "radiant", 10, pending)

    result = betting_service.settle_bets(
        9912, TEST_GUILD_ID, "radiant", pending_state=pending
    )
    assert result["winners"][0]["payout"] == 20


@pytest.mark.asyncio
async def test_gamba_effects_fetch_and_scale_exact_display_amounts():
    event_service = _EventService(
        gamba_win_multiplier=0.6,
        gamba_loss_multiplier=1.5,
    )
    cog = SimpleNamespace(bot=SimpleNamespace(economy_event_service=event_service))

    win_multiplier, loss_multiplier = await get_gamba_event_multipliers(
        cog, TEST_GUILD_ID
    )

    assert apply_gamba_event_multiplier(
        25,
        win_multiplier=win_multiplier,
        loss_multiplier=loss_multiplier,
    ) == 15
    assert apply_gamba_event_multiplier(
        -20,
        win_multiplier=win_multiplier,
        loss_multiplier=loss_multiplier,
    ) == -30


def test_gamba_multiplier_bounds_and_preserves_numeric_wedge_semantics():
    assert bounded_economy_multiplier(float("inf")) == 1.0
    assert bounded_economy_multiplier(-3) == 0.0
    assert bounded_economy_multiplier(100) == 10.0
    # A tiny/zero multiplier must not turn a numeric result into the wheel's
    # unrelated zero-value "lose a turn" cooldown outcome.
    assert apply_gamba_event_multiplier(
        1, win_multiplier=0.0, loss_multiplier=1.0
    ) == 1
    assert apply_gamba_event_multiplier(
        -1, win_multiplier=1.0, loss_multiplier=0.0
    ) == -1


def _wheel_processor(outcome, *, win_multiplier=0.5, player_service=None):
    from commands.betting_helpers.wheel_outcomes import (
        WheelOutcomeContext,
        WheelOutcomeProcessor,
        WheelOutcomeState,
    )

    player_service = player_service or MagicMock()
    command = SimpleNamespace(
        player_service=player_service,
        loan_service=None,
        _credit_gamba_outcome=AsyncMock(return_value=(999, 0)),
        _adjust_gamba_balance=MagicMock(return_value=999),
        _apply_hostile_gamba_loss=AsyncMock(),
    )
    context = WheelOutcomeContext(
        command=command,
        interaction=SimpleNamespace(guild=None),
        user_id=42,
        guild_id=TEST_GUILD_ID,
        bankruptcy_service=None,
        penalty_games_remaining=0,
        effects=None,
        mana_effects_service=None,
        is_bad_gamba=False,
        hostile_event_prefix="wheel:test",
        gamba_win_multiplier=win_multiplier,
        gamba_loss_multiplier=1.0,
    )
    state = WheelOutcomeState((outcome, outcome, "#123456"), new_balance=100)
    return WheelOutcomeProcessor(context, state), command, state


@pytest.mark.asyncio
async def test_special_minted_rewards_apply_event_after_central_scaling():
    # The neutral central scale preserves base rewards, then the 0.5x daily
    # event applies: Eruption fallback 50 -> 50 -> 25; Compound 100 -> 100 -> 50.
    eruption_player_service = MagicMock()
    eruption_player_service.get_last_normal_wheel_spin.return_value = None
    eruption, eruption_command, _ = _wheel_processor(
        "ERUPTION", player_service=eruption_player_service
    )
    await eruption.process()
    assert eruption_command._credit_gamba_outcome.await_args.args[3] == 25

    compound, compound_command, compound_state = _wheel_processor(
        "COMPOUND_INTEREST"
    )
    await compound.process()
    assert compound_state.compound_amount == 50
    assert compound_command._credit_gamba_outcome.await_args.args[3] == 50


@pytest.mark.asyncio
async def test_chain_reaction_scales_only_the_newly_minted_copy():
    player_service = MagicMock()
    player_service.get_last_normal_wheel_spin.return_value = {
        "result": 30,
        "discord_id": 77,
    }
    processor, command, state = _wheel_processor(
        "CHAIN_REACTION", player_service=player_service
    )

    await processor.process()

    assert state.chain_value == 15
    assert command._credit_gamba_outcome.await_args.args[3] == 15


@pytest.mark.asyncio
async def test_dynamic_minted_rewards_apply_event_and_report_adjusted_amount():
    overgrowth_player_service = MagicMock()
    overgrowth_player_service.get_player.return_value = SimpleNamespace(wins=1, losses=1)
    overgrowth_player_service.get_recent_matches.return_value = [object(), object()]
    overgrowth, overgrowth_command, _ = _wheel_processor(
        "OVERGROWTH", player_service=overgrowth_player_service
    )
    await overgrowth.process()
    # 2 games * 10 = 20; neutral central scale = 20; event 0.5x = 10.
    assert overgrowth_command._credit_gamba_outcome.await_args.args[3] == 10

    dividend_player_service = MagicMock()
    dividend_player_service.get_total_positive_balance.return_value = 10_000
    dividend, dividend_command, dividend_state = _wheel_processor(
        "DIVIDEND", player_service=dividend_player_service
    )
    await dividend.process()
    # 0.5% wealth = 50; neutral central scale = 50; event 0.5x = 25.
    assert dividend_state.dividend_amount == 25
    assert dividend_command._credit_gamba_outcome.await_args.args[3] == 25


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("outcome", "state_field", "expected"),
    [
        ("HEIST", "heist_total", 10),  # 20 -> 20 -> 10
        ("MARKET_CRASH", "market_crash_total", 12),  # 25 -> 25 -> 12
        ("HOSTILE_TAKEOVER", "takeover_amount", 20),  # 40 -> 40 -> 20
    ],
)
async def test_minted_fallbacks_are_scaled_but_not_transfer_paths(
    outcome, state_field, expected
):
    player_service = MagicMock()
    player_service.get_leaderboard_bottom.return_value = []
    player_service.get_leaderboard.return_value = []
    player_service.get_balance.return_value = 100 + expected
    processor, command, state = _wheel_processor(
        outcome, player_service=player_service
    )

    await processor.process()

    assert getattr(state, state_field) == expected
    command._adjust_gamba_balance.assert_called_once()
    assert command._adjust_gamba_balance.call_args.args[3] == expected


@pytest.mark.asyncio
async def test_numeric_and_player_transfer_rewards_are_not_double_scaled():
    numeric, numeric_command, _ = _wheel_processor(20)
    await numeric.process()
    # commands/betting.py already scales numeric wedges before processor dispatch.
    assert numeric_command._credit_gamba_outcome.await_args.args[3] == 20

    transfer_player_service = MagicMock()
    victim = SimpleNamespace(discord_id=77, jopacoin_balance=100, name="Victim")
    transfer_player_service.get_leaderboard.return_value = [victim]
    transfer, transfer_command, _ = _wheel_processor(
        "COMMUNE", player_service=transfer_player_service
    )
    transfer_command._apply_hostile_gamba_loss.return_value = SimpleNamespace(
        applied=12,
        absorbed=0,
        centralized=False,
    )

    await transfer.process()

    # This credit is funded by the victim's settled debit, not newly minted JC.
    assert transfer_command._credit_gamba_outcome.await_args.args[3] == 12
