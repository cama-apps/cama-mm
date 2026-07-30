"""Tests for the /pet brawl command and its interactive views."""

from __future__ import annotations

import asyncio
import sqlite3
import time
from dataclasses import replace
from unittest.mock import AsyncMock, MagicMock, patch

import discord
import pytest

from commands.pet import PetCommands
from commands.pet_helpers import brawl_embeds
from commands.pet_helpers import embeds as pet_embeds
from commands.pet_helpers.brawl_views import PetBrawlBattleView
from domain.models.pet import PetStage
from domain.pet_brawl import (
    MAX_ROUNDS,
    SAFE_MOVE,
    PetBrawlMove,
    build_duelist,
    initial_state,
)
from tests.conftest import TEST_GUILD_ID

NOW = int(time.time())
DAY = 86400

CHALLENGER = 100
RECIPIENT = 200
STRANGER = 999
PET_CHANNEL = 777_000


def make_interaction(user_id=CHALLENGER, channel_id=PET_CHANNEL):
    inter = MagicMock()
    inter.user.id = user_id
    inter.user.display_name = "Owner"
    inter.user.bot = False
    inter.guild.id = TEST_GUILD_ID
    inter.channel.id = channel_id
    inter.channel.parent = None
    inter.channel_id = channel_id
    inter.response.defer = AsyncMock()
    inter.response.send_message = AsyncMock()
    inter.followup.send = AsyncMock(return_value=MagicMock())
    return inter


@pytest.fixture
def brawl_cog(repo_db_path, monkeypatch):
    from repositories.pet_brawl_repository import PetBrawlRepository
    from repositories.pet_repository import PetRepository
    from repositories.player_repository import PlayerRepository
    from services.pet_brawl_service import PetBrawlService
    from services.pet_service import PetService

    monkeypatch.setattr(
        "commands.pet.GLOBAL_RATE_LIMITER.check",
        lambda **kwargs: MagicMock(allowed=True, retry_after_seconds=0),
    )
    import config

    monkeypatch.setattr(config, "PET_CHANNEL_ID", PET_CHANNEL)

    player_repo = PlayerRepository(repo_db_path)
    for discord_id in (CHALLENGER, RECIPIENT):
        player_repo.add(
            discord_id=discord_id,
            discord_username=f"Player {discord_id}",
            guild_id=TEST_GUILD_ID,
        )
        player_repo.update_balance(discord_id, TEST_GUILD_ID, 100)
    service = PetService(
        PetRepository(repo_db_path), player_repo, decay_per_day=20
    )
    bot = MagicMock()
    bot.reminder_service = None
    cog = PetCommands.__new__(PetCommands)
    cog.bot = bot
    cog.pet_service = service
    cog.pet_brawl_service = PetBrawlService(
        service, PetBrawlRepository(repo_db_path)
    )
    cog._brawl_sessions = {}

    with sqlite3.connect(repo_db_path) as conn:
        for discord_id in (CHALLENGER, RECIPIENT):
            conn.execute(
                "INSERT INTO pets (discord_id, guild_id, name, species, "
                "adopted_at, hatched_at, adopt_fee, last_fed_at, "
                "hunger_at_last_fed) VALUES (?, ?, 'Brawler', 'common_cama', "
                "?, ?, 20, ?, 100)",
                (discord_id, TEST_GUILD_ID, NOW - 10 * DAY, NOW - 9 * DAY, NOW),
            )
    return cog


def brawl_status(cog, brawl_id):
    return cog.pet_brawl_service.pet_brawl_repo.get_brawl(
        brawl_id, TEST_GUILD_ID
    ).status


async def issue_challenge(cog, wager=None):
    inter = make_interaction()
    target = MagicMock()
    target.id = RECIPIENT
    target.bot = False
    await cog.brawl.callback(cog, inter, target, wager)
    kwargs = inter.followup.send.await_args.kwargs
    return kwargs["view"], kwargs


class TestSoloTrainingCommand:
    @pytest.mark.asyncio
    async def test_training_cashes_in_banked_sessions(self, brawl_cog):
        inter = make_interaction()

        await brawl_cog.train.callback(brawl_cog, inter)

        kwargs = inter.followup.send.await_args.kwargs
        assert "3 banked sessions" in kwargs["content"]
        assert "+3 XP" in kwargs["content"]
        assert kwargs["ephemeral"] is True
        pet = brawl_cog.pet_service.pet_repo.get_active_pet(
            CHALLENGER, TEST_GUILD_ID
        )
        assert pet.training_xp == 3
        assert pet.solo_training_sessions == 0


class TestBrawlCommand:
    @pytest.mark.asyncio
    async def test_happy_path_posts_challenge(self, brawl_cog):
        view, kwargs = await issue_challenge(brawl_cog)
        assert "challenge" in kwargs["embed"].title.lower()
        assert (
            kwargs["embed"].footer.text
            == "No coin at stake — fullness and honor only."
        )
        assert brawl_status(brawl_cog, view.brawl.brawl_id) == "pending"

    @pytest.mark.asyncio
    async def test_optional_wager_posts_stakes_and_escrows_challenger(
        self, brawl_cog
    ):
        view, kwargs = await issue_challenge(brawl_cog, wager=20)

        assert "20" in kwargs["embed"].footer.text
        assert "40" in kwargs["embed"].footer.text
        assert "fee: 1" in kwargs["embed"].footer.text.lower()
        assert (
            brawl_cog.pet_service.player_repo.get_balance(
                CHALLENGER, TEST_GUILD_ID
            )
            == 79
        )
        assert view.brawl.wager == 20

    @pytest.mark.asyncio
    async def test_failed_challenge_delivery_voids_and_refunds(
        self, brawl_cog
    ):
        inter = make_interaction()
        inter.followup.send = AsyncMock(return_value=None)
        target = MagicMock(id=RECIPIENT, bot=False)

        await brawl_cog.brawl.callback(brawl_cog, inter, target, 20)

        with sqlite3.connect(brawl_cog.pet_service.pet_repo.db_path) as conn:
            status = conn.execute(
                "SELECT status FROM pet_brawls ORDER BY brawl_id DESC LIMIT 1"
            ).fetchone()[0]
        assert status == "void"
        assert (
            brawl_cog.pet_service.player_repo.get_balance(
                CHALLENGER, TEST_GUILD_ID
            )
            == 100
        )

    @pytest.mark.asyncio
    async def test_rejected_outside_pet_channel(self, brawl_cog):
        inter = make_interaction(channel_id=123)
        target = MagicMock()
        target.id = RECIPIENT
        target.bot = False
        await brawl_cog.brawl.callback(brawl_cog, inter, target)
        inter.response.send_message.assert_awaited_once()
        assert "arena" in inter.response.send_message.await_args.args[0]
        inter.response.defer.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_rejects_bot_target(self, brawl_cog):
        inter = make_interaction()
        target = MagicMock()
        target.id = RECIPIENT
        target.bot = True
        await brawl_cog.brawl.callback(brawl_cog, inter, target)
        assert "Bots" in inter.response.send_message.await_args.args[0]

    def test_status_embed_shows_training_stats_rank_and_build(self, brawl_cog):
        with sqlite3.connect(brawl_cog.pet_service.pet_repo.db_path) as conn:
            conn.execute(
                "UPDATE pets SET training_xp = 20, training_str = 2, "
                "training_int = 1, training_dex = 1 "
                "WHERE discord_id = ? AND guild_id = ?",
                (CHALLENGER, TEST_GUILD_ID),
            )
        status = brawl_cog.pet_service.get_status(
            CHALLENGER, TEST_GUILD_ID
        ).value
        career = brawl_cog.pet_brawl_service.career_summary(
            status.pet, wins=10, losses=2
        )

        embed, _ = pet_embeds.build_status_embed(
            status,
            brawl_cog.pet_service.decay_per_day,
            NOW,
            owner_name="Owner",
            next_fee=20,
            brawl_record=(10, 2),
            career=career,
            solo_training_sessions=3,
        )

        training = next(field for field in embed.fields if field.name == "🏋️ Training")
        assert "20/20 XP" in training.value
        assert "STR 2" in training.value
        assert "INT 1" in training.value
        assert "DEX 1" in training.value
        assert "Champion" in training.value
        assert "Bruiser" in training.value
        assert "3/3 banked" in training.value


class TestChallengeView:
    @pytest.mark.asyncio
    async def test_accept_recipient_only(self, brawl_cog):
        view, _ = await issue_challenge(brawl_cog)
        view.message = MagicMock(edit=AsyncMock())
        inter = make_interaction(user_id=STRANGER)
        await view.accept.callback(inter)
        assert "isn't yours" in inter.response.send_message.await_args.args[0]
        assert brawl_status(brawl_cog, view.brawl.brawl_id) == "pending"

    @pytest.mark.asyncio
    async def test_accept_starts_battle(self, brawl_cog):
        view, _ = await issue_challenge(brawl_cog)
        view.message = MagicMock(edit=AsyncMock())
        inter = make_interaction(user_id=RECIPIENT)
        await view.accept.callback(inter)
        assert brawl_status(brawl_cog, view.brawl.brawl_id) == "active"
        assert view.brawl.brawl_id in brawl_cog._brawl_sessions
        edit_kwargs = view.message.edit.await_args.kwargs
        assert isinstance(edit_kwargs["view"], PetBrawlBattleView)
        assert "Round 1" in edit_kwargs["embed"].title
        # Round 1 explains the rules so nobody has to ask "is this RPS?".
        field_names = [field.name for field in edit_kwargs["embed"].fields]
        assert "How it works" in field_names

    @pytest.mark.asyncio
    async def test_decline(self, brawl_cog):
        view, _ = await issue_challenge(brawl_cog)
        view.message = MagicMock(edit=AsyncMock())
        inter = make_interaction(user_id=RECIPIENT)
        await view.decline.callback(inter)
        assert brawl_status(brawl_cog, view.brawl.brawl_id) == "declined"

    @pytest.mark.asyncio
    async def test_withdraw_challenger_only(self, brawl_cog):
        view, _ = await issue_challenge(brawl_cog)
        view.message = MagicMock(edit=AsyncMock())
        inter = make_interaction(user_id=RECIPIENT)
        await view.withdraw.callback(inter)
        assert "challenger" in inter.response.send_message.await_args.args[0]
        inter = make_interaction(user_id=CHALLENGER)
        await view.withdraw.callback(inter)
        assert brawl_status(brawl_cog, view.brawl.brawl_id) == "void"

    @pytest.mark.asyncio
    async def test_timeout_voids(self, brawl_cog):
        view, _ = await issue_challenge(brawl_cog)
        view.message = MagicMock(edit=AsyncMock())
        await view.on_timeout()
        assert brawl_status(brawl_cog, view.brawl.brawl_id) == "void"


async def start_battle(brawl_cog, wager=None):
    challenge_view, _ = await issue_challenge(brawl_cog, wager=wager)
    challenge_view.message = MagicMock(edit=AsyncMock())
    inter = make_interaction(user_id=RECIPIENT)
    await challenge_view.accept.callback(inter)
    battle_view = challenge_view.message.edit.await_args.kwargs["view"]
    battle_view.message = challenge_view.message
    session = brawl_cog._brawl_sessions[challenge_view.brawl.brawl_id]
    session.message = battle_view.message
    return battle_view, session


def press(view, move: PetBrawlMove):
    for child in view.children:
        if child.custom_id.endswith(move.value):
            return child.callback
    raise AssertionError(f"no button for {move}")


class TestBattleView:
    @pytest.mark.asyncio
    async def test_stranger_cannot_fight(self, brawl_cog):
        view, _ = await start_battle(brawl_cog)
        inter = make_interaction(user_id=STRANGER)
        await press(view, SAFE_MOVE)(inter)
        assert "duelists" in inter.response.send_message.await_args.args[0]

    @pytest.mark.asyncio
    async def test_double_click_guard(self, brawl_cog):
        view, session = await start_battle(brawl_cog)
        inter = make_interaction(user_id=CHALLENGER)
        await press(view, SAFE_MOVE)(inter)
        assert "picked" in inter.followup.send.await_args.args[0]
        inter2 = make_interaction(user_id=CHALLENGER)
        await press(view, PetBrawlMove.HUNKER)(inter2)
        assert "already locked" in inter2.followup.send.await_args.args[0]
        assert session.picks["a"] is SAFE_MOVE

    @pytest.mark.asyncio
    async def test_both_picks_resolve_round(self, brawl_cog):
        view, session = await start_battle(brawl_cog)
        await press(view, SAFE_MOVE)(make_interaction(user_id=CHALLENGER))
        await press(view, SAFE_MOVE)(make_interaction(user_id=RECIPIENT))
        assert session.state.round_no == 1
        assert session.picks == {}
        edit_kwargs = view.message.edit.await_args.kwargs
        assert "Round 2" in edit_kwargs["embed"].title
        assert isinstance(edit_kwargs["view"], PetBrawlBattleView)

    @pytest.mark.asyncio
    async def test_timeout_autopicks_for_laggard_only(self, brawl_cog):
        view, session = await start_battle(brawl_cog)
        await press(view, PetBrawlMove.HUNKER)(make_interaction(user_id=CHALLENGER))
        await view.on_timeout()
        assert session.state.round_no == 1
        assert session.consecutive_full_timeouts == 0
        # The hunker log line proves the challenger's own pick was kept.
        assert any("hunkers" in line for line in session.last_log)
        # The laggard's auto-pick is called out, not passed off as a choice.
        assert any("never picked" in line for line in session.last_log)

    @pytest.mark.asyncio
    async def test_double_full_timeout_voids(self, brawl_cog):
        view, session = await start_battle(brawl_cog)
        await view.on_timeout()
        assert session.consecutive_full_timeouts == 1
        assert session.state.round_no == 1  # round still played with safe moves
        next_view = PetBrawlBattleView(brawl_cog, session)
        next_view.message = view.message
        await next_view.on_timeout()
        assert brawl_status(brawl_cog, session.brawl_id) == "void"
        assert session.brawl_id not in brawl_cog._brawl_sessions

    @pytest.mark.asyncio
    async def test_knockout_settles_and_reports(self, brawl_cog):
        view, session = await start_battle(brawl_cog)
        # Put the recipient's pet on death's door so one spit ends it.
        session.state = type(session.state)(
            a=session.state.a,
            b=replace(session.state.b, hp=1),
            round_no=session.state.round_no,
            winner=None,
        )
        await press(view, SAFE_MOVE)(make_interaction(user_id=CHALLENGER))
        await press(view, SAFE_MOVE)(make_interaction(user_id=RECIPIENT))
        assert brawl_status(brawl_cog, session.brawl_id) == "done"
        assert session.brawl_id not in brawl_cog._brawl_sessions
        edit_kwargs = view.message.edit.await_args.kwargs
        assert "wins the brawl" in edit_kwargs["embed"].title
        assert "XP" in edit_kwargs["embed"].description
        assert "Rookie" in edit_kwargs["embed"].description
        assert edit_kwargs["view"] is None
        # Answers the "was there any JC reward?" question at the finish line.
        assert "jopacoin" in edit_kwargs["embed"].footer.text.lower()

    @pytest.mark.asyncio
    async def test_wagered_result_footer_reports_the_payout(self, brawl_cog):
        view, session = await start_battle(brawl_cog, wager=20)
        session.state = type(session.state)(
            a=session.state.a,
            b=replace(session.state.b, hp=1),
            round_no=session.state.round_no,
            winner=None,
        )

        await press(view, SAFE_MOVE)(make_interaction(user_id=CHALLENGER))
        await press(view, SAFE_MOVE)(make_interaction(user_id=RECIPIENT))

        footer = view.message.edit.await_args.kwargs["embed"].footer.text
        assert "40" in footer
        assert "no jopacoins" not in footer.lower()

    @pytest.mark.asyncio
    async def test_equal_hp_round_cap_reports_draw_and_refunds_wager(
        self, brawl_cog
    ):
        view, session = await start_battle(brawl_cog, wager=20)
        session.state = type(session.state)(
            a=session.state.a,
            b=session.state.b,
            round_no=MAX_ROUNDS - 1,
            winner=None,
        )

        await press(view, PetBrawlMove.HUNKER)(
            make_interaction(user_id=CHALLENGER)
        )
        await press(view, PetBrawlMove.HUNKER)(
            make_interaction(user_id=RECIPIENT)
        )

        assert brawl_status(brawl_cog, session.brawl_id) == "done"
        assert session.brawl_id not in brawl_cog._brawl_sessions
        embed = view.message.edit.await_args.kwargs["embed"]
        assert "draw" in embed.title.lower()
        assert "refunded" in embed.footer.text.lower()
        assert (
            brawl_cog.pet_service.player_repo.get_balance(
                CHALLENGER, TEST_GUILD_ID
            )
            == 100
        )
        assert (
            brawl_cog.pet_service.player_repo.get_balance(
                RECIPIENT, TEST_GUILD_ID
            )
            == 100
        )

    @pytest.mark.asyncio
    async def test_round_resolves_even_if_pick_ack_fails(self, brawl_cog):
        """The ephemeral ack precedes resolution; if it raises, the recorded
        second pick must still resolve the round instead of stalling it
        until the view timeout."""
        view, session = await start_battle(brawl_cog)
        await press(view, SAFE_MOVE)(make_interaction(user_id=CHALLENGER))
        inter = make_interaction(user_id=RECIPIENT)
        inter.followup.send = AsyncMock(
            side_effect=discord.HTTPException(MagicMock(status=404), "gone")
        )
        await press(view, SAFE_MOVE)(inter)
        assert session.state.round_no == 1
        assert session.picks == {}

    @pytest.mark.asyncio
    async def test_round_views_use_distinct_custom_ids(self, brawl_cog):
        """Identical ids across rounds let the old round's stop() deregister
        the new round's buttons in discord.py's ViewStore (keyed by
        custom_id, popped with no identity check) — every click after round
        one then dies as 'This interaction failed'."""
        view, session = await start_battle(brawl_cog)
        old_ids = {child.custom_id for child in view.children}
        await press(view, SAFE_MOVE)(make_interaction(user_id=CHALLENGER))
        await press(view, SAFE_MOVE)(make_interaction(user_id=RECIPIENT))
        next_view = view.message.edit.await_args.kwargs["view"]
        new_ids = {child.custom_id for child in next_view.children}
        assert old_ids.isdisjoint(new_ids)

    @pytest.mark.asyncio
    async def test_click_is_acked_while_round_resolves(self, brawl_cog):
        """The ack must happen before waiting on the session lock — a round
        resolving on the other side can hold it past Discord's 3s window."""
        view, session = await start_battle(brawl_cog)
        inter = make_interaction(user_id=CHALLENGER)
        async with session.lock:
            task = asyncio.ensure_future(press(view, SAFE_MOVE)(inter))
            for _ in range(20):
                await asyncio.sleep(0)
                if inter.response.defer.await_count:
                    break
            assert inter.response.defer.await_count == 1
            assert not task.done()  # still parked on the lock, already acked
        await task
        assert "picked" in inter.followup.send.await_args.args[0]


def _duelist(species: str, name: str, owner_id: int, pet_id: int):
    return build_duelist(
        pet_id=pet_id,
        owner_id=owner_id,
        name=name,
        species_id=species,
        stage=PetStage.ADULT,
        hunger=100,
        happy=False,
        aegis_used=0,
    )


class TestRoundOnePrimer:
    def test_quirks_field_lists_traits_and_shell(self):
        state = initial_state(
            _duelist("rama", "Spitfire", CHALLENGER, 1),
            _duelist("aegis_cama", "Shelly", RECIPIENT, 2),
        )
        embed = brawl_embeds.build_battle_embed(state, (), {})
        quirks = next(f for f in embed.fields if f.name == "Quirks")
        assert "Spitfire" in quirks.value and "crits" in quirks.value
        assert "Shelly" in quirks.value and "1 HP" in quirks.value

    def test_quirkless_pair_gets_rules_but_no_quirks_field(self):
        state = initial_state(
            _duelist("common_cama", "Plain", CHALLENGER, 1),
            _duelist("common_cama", "Simple", RECIPIENT, 2),
        )
        embed = brawl_embeds.build_battle_embed(state, (), {})
        field_names = [f.name for f in embed.fields]
        assert "How it works" in field_names
        assert "Quirks" not in field_names

    def test_jopacama_quirk_describes_fullness_insurance(self):
        state = initial_state(
            _duelist("jopacama", "Gilded", CHALLENGER, 1),
            _duelist("common_cama", "Plain", RECIPIENT, 2),
        )

        embed = brawl_embeds.build_battle_embed(state, (), {})

        quirks = next(field for field in embed.fields if field.name == "Quirks")
        assert "loses less fullness in defeat" in quirks.value
        assert "hunger" not in quirks.value

    def test_result_describes_fullness_stakes(self):
        winner = _duelist("common_cama", "Winner", CHALLENGER, 1)
        loser = _duelist("common_cama", "Loser", RECIPIENT, 2)
        embed, _file = brawl_embeds.build_result_embed(
            {
                "winner": winner,
                "loser": loser,
                "records": {},
                "winner_delta": 10,
                "loser_delta": -15,
            },
            rounds=3,
            final_log=(),
        )

        assert "fullness +10" in embed.description
        assert "fullness -15" in embed.description
        assert "fullness and honor" in embed.footer.text

    def test_zero_delta_result_does_not_guess_why_fullness_was_unchanged(self):
        winner = _duelist("common_cama", "Winner", CHALLENGER, 1)
        loser = _duelist("common_cama", "Loser", RECIPIENT, 2)
        embed, _file = brawl_embeds.build_result_embed(
            {
                "winner": winner,
                "loser": loser,
                "records": {},
                "winner_delta": 0,
                "loser_delta": 0,
            },
            rounds=3,
            final_log=(),
        )

        assert "gets no fullness from this victory" in embed.description
        assert "loses no fullness from this defeat" in embed.description
        assert "too well-fed" not in embed.description
        assert "too hungry" not in embed.description

    def test_draw_result_describes_unchanged_fullness(self):
        a = _duelist("common_cama", "First", CHALLENGER, 1)
        b = _duelist("common_cama", "Second", RECIPIENT, 2)

        embed, _file = brawl_embeds.build_result_embed(
            {
                "draw": True,
                "duelists": (a, b),
                "records": {},
            },
            rounds=8,
            final_log=(),
        )

        assert "No fullness, records, or training XP change." in embed.description
        assert "hunger" not in embed.description.lower()

    @pytest.mark.asyncio
    async def test_failed_settlement_immediately_voids_and_refunds_wager(
        self, brawl_cog, monkeypatch
    ):
        from services.result import Result

        view, session = await start_battle(brawl_cog, wager=20)
        session.state = type(session.state)(
            a=session.state.a,
            b=replace(session.state.b, hp=1),
            round_no=session.state.round_no,
            winner=None,
        )
        monkeypatch.setattr(
            brawl_cog.pet_brawl_service,
            "settle",
            lambda *args, **kwargs: Result.fail("database unavailable"),
        )

        await press(view, SAFE_MOVE)(make_interaction(user_id=CHALLENGER))
        await press(view, SAFE_MOVE)(make_interaction(user_id=RECIPIENT))

        assert brawl_status(brawl_cog, session.brawl_id) == "void"
        assert (
            brawl_cog.pet_service.player_repo.get_balance(
                CHALLENGER, TEST_GUILD_ID
            ),
            brawl_cog.pet_service.player_repo.get_balance(
                RECIPIENT, TEST_GUILD_ID
            ),
        ) == (100, 100)


class TestSweepHook:
    @pytest.mark.asyncio
    async def test_sweep_loop_calls_brawl_sweep(self, brawl_cog):
        with patch.object(
            brawl_cog.pet_brawl_service, "sweep_stale", return_value={}
        ) as sweep:
            await brawl_cog._pet_sweep_loop.coro(brawl_cog)
        sweep.assert_called_once()
