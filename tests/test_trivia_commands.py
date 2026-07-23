"""Tests for trivia cooldown and economy integration."""

import asyncio
import time
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import discord
import pytest

from repositories.player_repository import PlayerRepository
from services.player_service import PlayerService
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def player_service(repo_db_path):
    repo = PlayerRepository(repo_db_path)
    return PlayerService(repo)


@pytest.fixture
def registered_player(player_service):
    """Register a test player and return their discord_id."""
    discord_id = 100001
    player_service.register_player(
        discord_id=discord_id,
        discord_username="trivia_tester",
        guild_id=TEST_GUILD_ID,
        steam_id=12345,
        mmr_override=3000,
    )
    return discord_id


def _trivia_question():
    from services.trivia_questions import TriviaQuestion

    return TriviaQuestion(
        text="Which hero says this?",
        options=["Axe", "Crystal Maiden", "Pudge", "Invoker"],
        correct_index=1,
        difficulty="easy",
        image_url=None,
        category="test",
        explanation=None,
    )


def _trivia_user(discord_id):
    user = MagicMock()
    user.id = discord_id
    user.display_name = "trivia_tester"
    user.display_avatar.url = "https://example.com/avatar.png"
    return user


def _transition_view(trivia_mod, *, user_id=100001):
    player_service = SimpleNamespace(
        adjust_balance=MagicMock(),
        record_trivia_session=MagicMock(),
    )
    bot = SimpleNamespace(
        player_service=player_service,
        mana_effects_service=None,
        bankruptcy_service=None,
    )
    cog = trivia_mod.TriviaCog(bot)
    current_message = SimpleNamespace(edit=AsyncMock())
    previous_message = SimpleNamespace(delete=AsyncMock())
    session = trivia_mod.TriviaSession(
        user_id=user_id,
        guild_id=TEST_GUILD_ID,
        user=_trivia_user(user_id),
        message=current_message,
        prev_message=previous_message,
    )
    cog._sessions[(user_id, TEST_GUILD_ID)] = session
    view = trivia_mod.TriviaView(session, _trivia_question(), 1, cog)
    return view, session, cog, current_message, previous_message


class TestTriviaPayout:
    def test_correct_answer_pays_base_plus_streak_bonuses(self):
        import commands.trivia as trivia_mod

        assert trivia_mod._jc_for_streak(1) == 1
        assert trivia_mod._jc_for_streak(2) == 1
        assert trivia_mod._jc_for_streak(3) == 2
        assert trivia_mod._jc_for_streak(6) == 2
        assert trivia_mod._jc_for_streak(7) == 1
        assert trivia_mod._jc_for_streak(10) == 3
        assert trivia_mod._jc_for_streak(14) == 2

    @pytest.mark.asyncio
    async def test_correct_answer_awards_one_jc_before_streak_bonus(
        self, player_service, registered_player, monkeypatch
    ):
        import commands.trivia as trivia_mod

        bot = SimpleNamespace(
            player_service=player_service,
            mana_effects_service=None,
            bankruptcy_service=None,
        )
        cog = trivia_mod.TriviaCog(bot)
        session = trivia_mod.TriviaSession(
            user_id=registered_player,
            guild_id=TEST_GUILD_ID,
            user=_trivia_user(registered_player),
        )
        cog._sessions[(registered_player, TEST_GUILD_ID)] = session
        view = trivia_mod.TriviaView(session, _trivia_question(), 1, cog)
        interaction = MagicMock()
        interaction.user.id = registered_player
        interaction.response.edit_message = AsyncMock()
        interaction.followup.send = AsyncMock()
        monkeypatch.setattr(trivia_mod, "generate_question", lambda _streak, _recent: None)

        initial = player_service.get_balance(registered_player, TEST_GUILD_ID)
        await view._handle_answer(interaction, 1)

        assert session.streak == 1
        assert session.total_jc == 1
        assert player_service.get_balance(registered_player, TEST_GUILD_ID) == initial + 1

    @pytest.mark.asyncio
    async def test_correct_answer_awards_streak_bonus_on_milestone(
        self, player_service, registered_player, monkeypatch
    ):
        import commands.trivia as trivia_mod

        bot = SimpleNamespace(
            player_service=player_service,
            mana_effects_service=None,
            bankruptcy_service=None,
        )
        cog = trivia_mod.TriviaCog(bot)
        session = trivia_mod.TriviaSession(
            user_id=registered_player,
            guild_id=TEST_GUILD_ID,
            user=_trivia_user(registered_player),
            streak=2,
        )
        cog._sessions[(registered_player, TEST_GUILD_ID)] = session
        view = trivia_mod.TriviaView(session, _trivia_question(), 3, cog)
        interaction = MagicMock()
        interaction.user.id = registered_player
        interaction.response.edit_message = AsyncMock()
        interaction.followup.send = AsyncMock()
        monkeypatch.setattr(trivia_mod, "generate_question", lambda _streak, _recent: None)

        initial = player_service.get_balance(registered_player, TEST_GUILD_ID)
        await view._handle_answer(interaction, 1)

        assert session.streak == 3
        assert session.total_jc == 2
        assert player_service.get_balance(registered_player, TEST_GUILD_ID) == initial + 2

    @pytest.mark.asyncio
    async def test_correct_answer_keeps_larger_payout_at_neutral_scale(
        self, player_service, registered_player, monkeypatch
    ):
        import commands.trivia as trivia_mod

        bot = SimpleNamespace(
            player_service=player_service,
            mana_effects_service=None,
            bankruptcy_service=None,
        )
        cog = trivia_mod.TriviaCog(bot)
        session = trivia_mod.TriviaSession(
            user_id=registered_player,
            guild_id=TEST_GUILD_ID,
            user=_trivia_user(registered_player),
            streak=9,
        )
        cog._sessions[(registered_player, TEST_GUILD_ID)] = session
        view = trivia_mod.TriviaView(session, _trivia_question(), 10, cog)
        interaction = MagicMock()
        interaction.user.id = registered_player
        interaction.response.edit_message = AsyncMock()
        interaction.followup.send = AsyncMock()
        monkeypatch.setattr(trivia_mod, "generate_question", lambda _streak, _recent: None)

        initial = player_service.get_balance(registered_player, TEST_GUILD_ID)
        await view._handle_answer(interaction, 1)

        assert session.streak == 10
        assert session.total_jc == 3
        assert player_service.get_balance(registered_player, TEST_GUILD_ID) == initial + 3

    @pytest.mark.asyncio
    async def test_daily_event_runs_after_single_central_scale(
        self, player_service, registered_player, monkeypatch
    ):
        import commands.trivia as trivia_mod

        class EventService:
            def __init__(self):
                self.calls = []

            def adjust_reward(self, guild_id, amount):
                self.calls.append((guild_id, amount))
                return amount // 2

        event_service = EventService()
        bot = SimpleNamespace(
            player_service=player_service,
            mana_effects_service=None,
            bankruptcy_service=None,
            economy_event_service=event_service,
        )
        cog = trivia_mod.TriviaCog(bot)
        session = trivia_mod.TriviaSession(
            user_id=registered_player,
            guild_id=TEST_GUILD_ID,
            user=_trivia_user(registered_player),
        )
        cog._sessions[(registered_player, TEST_GUILD_ID)] = session
        view = trivia_mod.TriviaView(session, _trivia_question(), 1, cog)
        interaction = MagicMock()
        interaction.user.id = registered_player
        interaction.response.edit_message = AsyncMock()
        interaction.followup.send = AsyncMock()
        monkeypatch.setattr(trivia_mod, "TRIVIA_REWARD_PER_QUESTION", 10)
        scale_spy = MagicMock(wraps=trivia_mod.scale_minigame_jc_delta)
        monkeypatch.setattr(trivia_mod, "scale_minigame_jc_delta", scale_spy)
        monkeypatch.setattr(
            trivia_mod,
            "generate_question",
            lambda _streak, _recent: None,
        )

        initial = player_service.get_balance(registered_player, TEST_GUILD_ID)
        await view._handle_answer(interaction, 1)

        scale_spy.assert_called_once_with(10)
        assert event_service.calls == [(TEST_GUILD_ID, 10)]
        assert session.total_jc == 5
        assert player_service.get_balance(registered_player, TEST_GUILD_ID) == initial + 5

    def test_missing_bot_or_event_service_is_neutral(self):
        import commands.trivia as trivia_mod

        assert trivia_mod._apply_daily_reward_event(None, TEST_GUILD_ID, 8) == 8
        assert (
            trivia_mod._apply_daily_reward_event(
                SimpleNamespace(),
                TEST_GUILD_ID,
                8,
            )
            == 8
        )


class TestTriviaCooldown:
    def test_first_session_succeeds(self, player_service, registered_player):
        now = int(time.time())
        claimed = player_service.try_claim_trivia_session(
            registered_player, TEST_GUILD_ID, now, 21600
        )
        assert claimed is True

    def test_second_session_blocked_by_cooldown(self, player_service, registered_player):
        now = int(time.time())
        # First claim succeeds
        assert player_service.try_claim_trivia_session(registered_player, TEST_GUILD_ID, now, 21600)
        # Second claim within cooldown fails
        assert not player_service.try_claim_trivia_session(registered_player, TEST_GUILD_ID, now + 100, 21600)

    def test_session_available_after_cooldown(self, player_service, registered_player):
        now = int(time.time())
        assert player_service.try_claim_trivia_session(registered_player, TEST_GUILD_ID, now, 21600)
        # After cooldown expires
        future = now + 21601
        assert player_service.try_claim_trivia_session(registered_player, TEST_GUILD_ID, future, 21600)

    def test_get_last_trivia_session_none_initially(self, player_service, registered_player):
        result = player_service.get_last_trivia_session(registered_player, TEST_GUILD_ID)
        assert result is None

    def test_get_last_trivia_session_after_claim(self, player_service, registered_player):
        now = int(time.time())
        player_service.try_claim_trivia_session(registered_player, TEST_GUILD_ID, now, 21600)
        result = player_service.get_last_trivia_session(registered_player, TEST_GUILD_ID)
        assert result == now


class TestTriviaEconomy:
    def test_balance_increases_on_correct_answer(self, player_service, registered_player):
        initial = player_service.get_balance(registered_player, TEST_GUILD_ID)
        player_service.adjust_balance(registered_player, TEST_GUILD_ID, 1)
        after = player_service.get_balance(registered_player, TEST_GUILD_ID)
        assert after == initial + 1

    def test_multiple_correct_answers(self, player_service, registered_player):
        initial = player_service.get_balance(registered_player, TEST_GUILD_ID)
        for _ in range(5):
            player_service.adjust_balance(registered_player, TEST_GUILD_ID, 1)
        after = player_service.get_balance(registered_player, TEST_GUILD_ID)
        assert after == initial + 5


class TestTriviaCooldownGuildIsolation:
    def test_cooldown_per_guild(self, player_service, registered_player):
        """Trivia cooldown in one guild shouldn't affect another."""
        guild_a = TEST_GUILD_ID
        guild_b = 99999

        # Register in second guild
        player_service.register_player(
            discord_id=registered_player,
            discord_username="trivia_tester",
            guild_id=guild_b,
            steam_id=12345,
            mmr_override=3000,
        )

        now = int(time.time())
        # Claim in guild A
        assert player_service.try_claim_trivia_session(registered_player, guild_a, now, 21600)
        # Should still be available in guild B
        assert player_service.try_claim_trivia_session(registered_player, guild_b, now, 21600)


class TestTriviaResetCooldown:
    def test_reset_clears_cooldown(self, player_service, registered_player):
        now = int(time.time())
        # Claim session (sets cooldown)
        assert player_service.try_claim_trivia_session(registered_player, TEST_GUILD_ID, now, 21600)
        # Blocked within cooldown
        assert not player_service.try_claim_trivia_session(registered_player, TEST_GUILD_ID, now + 100, 21600)
        # Reset cooldown
        assert player_service.reset_trivia_cooldown(registered_player, TEST_GUILD_ID)
        # Should be able to claim again
        assert player_service.try_claim_trivia_session(registered_player, TEST_GUILD_ID, now + 100, 21600)

    def test_reset_returns_false_for_missing_player(self, player_service):
        assert not player_service.reset_trivia_cooldown(999999, TEST_GUILD_ID)

    def test_reset_returns_false_when_no_cooldown_set(self, player_service, registered_player):
        """A registered player with no active cooldown has nothing to clear, so
        reset reports False (not True from a matched-but-unchanged row)."""
        assert player_service.get_last_trivia_session(registered_player, TEST_GUILD_ID) is None
        assert not player_service.reset_trivia_cooldown(registered_player, TEST_GUILD_ID)

    def test_get_last_trivia_session_none_after_reset(self, player_service, registered_player):
        now = int(time.time())
        player_service.try_claim_trivia_session(registered_player, TEST_GUILD_ID, now, 21600)
        assert player_service.get_last_trivia_session(registered_player, TEST_GUILD_ID) == now
        player_service.reset_trivia_cooldown(registered_player, TEST_GUILD_ID)
        assert player_service.get_last_trivia_session(registered_player, TEST_GUILD_ID) is None


class TestTriviaSessionLifecycle:
    @pytest.mark.asyncio
    async def test_end_session_awaits_leaderboard_write(self, player_service, registered_player):
        """_end_session must await the leaderboard write, not fire-and-forget it.

        An ensure_future task can be GC'd before running, silently dropping the
        streak. Awaiting guarantees the row is persisted (and visible on the
        leaderboard) by the time _end_session returns.
        """
        import commands.trivia as trivia_mod

        bot = MagicMock()
        bot.player_service = player_service
        cog = trivia_mod.TriviaCog(bot)
        session = trivia_mod.TriviaSession(
            user_id=registered_player, guild_id=TEST_GUILD_ID, user=MagicMock(), streak=5, total_jc=5
        )
        cog._sessions[(registered_player, TEST_GUILD_ID)] = session

        await cog._end_session(session)

        lb = player_service.get_trivia_leaderboard(TEST_GUILD_ID)
        assert any(e["discord_id"] == registered_player and e["best_streak"] == 5 for e in lb)
        assert (registered_player, TEST_GUILD_ID) not in cog._sessions

    @pytest.mark.asyncio
    async def test_failed_defer_releases_claimed_cooldown(
        self, player_service, registered_player, monkeypatch
    ):
        """If the cooldown is claimed but the defer then fails, no session starts,
        so the cooldown must be released rather than burning the player's run."""
        import commands.trivia as trivia_mod

        bot = MagicMock()
        bot.player_service = player_service
        cog = trivia_mod.TriviaCog(bot)

        interaction = MagicMock()
        interaction.guild.id = TEST_GUILD_ID
        interaction.user.id = registered_player

        monkeypatch.setattr(trivia_mod, "require_gamba_channel", AsyncMock(return_value=True))
        monkeypatch.setattr(trivia_mod, "has_admin_permission", MagicMock(return_value=False))
        monkeypatch.setattr(trivia_mod, "safe_defer", AsyncMock(return_value=False))

        assert player_service.get_last_trivia_session(registered_player, TEST_GUILD_ID) is None
        await cog.trivia.callback(cog, interaction)
        # The claim was rolled back, so the player can play again immediately.
        assert player_service.get_last_trivia_session(registered_player, TEST_GUILD_ID) is None


class TestTriviaTransitionConcurrency:
    @pytest.mark.asyncio
    async def test_correct_answer_overlaps_delete_edit_and_next_question_preparation(
        self,
        monkeypatch,
    ):
        import commands.trivia as trivia_mod

        view, session, _, current_message, previous_message = _transition_view(
            trivia_mod
        )
        next_question = _trivia_question()
        next_message = SimpleNamespace(id=222)
        delete_started = asyncio.Event()
        edit_started = asyncio.Event()
        generation_started = asyncio.Event()
        image_started = asyncio.Event()
        release_delete = asyncio.Event()
        release_edit = asyncio.Event()
        release_generation = asyncio.Event()
        release_image = asyncio.Event()
        next_sent = asyncio.Event()
        order = []

        async def delete_previous():
            delete_started.set()
            await release_delete.wait()
            order.append("delete_done")

        async def edit_current(**kwargs):
            edit_started.set()
            await release_edit.wait()
            order.append("edit_done")

        def generate_next(streak, recent_categories):
            assert streak == 1
            assert recent_categories == ["test"]
            return next_question

        async def controlled_to_thread(func, *args, **kwargs):
            if func is generate_next:
                generation_started.set()
                await release_generation.wait()
                return func(*args, **kwargs)
            if func is trivia_mod._prepare_question:
                image_started.set()
                await release_image.wait()
                result = func(*args, **kwargs)
                order.append("image_done")
                return result
            return func(*args, **kwargs)

        async def send_next(**kwargs):
            order.append("next_send")
            next_sent.set()
            return next_message

        previous_message.delete = AsyncMock(side_effect=delete_previous)
        interaction = SimpleNamespace(
            user=SimpleNamespace(id=session.user_id),
            response=SimpleNamespace(edit_message=AsyncMock(side_effect=edit_current)),
            followup=SimpleNamespace(send=AsyncMock(side_effect=send_next)),
        )
        monkeypatch.setattr(trivia_mod, "generate_question", generate_next)
        monkeypatch.setattr(trivia_mod.asyncio, "to_thread", controlled_to_thread)

        answer_task = asyncio.create_task(view._handle_answer(interaction, 1))
        await asyncio.wait_for(
            asyncio.gather(
                delete_started.wait(),
                edit_started.wait(),
                generation_started.wait(),
            ),
            timeout=1,
        )

        # The next-image step follows generation, but all three top-level
        # branches began before any of them was released.
        interaction.followup.send.assert_not_awaited()
        assert not answer_task.done()

        release_generation.set()
        await asyncio.wait_for(image_started.wait(), timeout=1)
        release_image.set()
        await asyncio.sleep(0)
        interaction.followup.send.assert_not_awaited()

        release_edit.set()
        await asyncio.sleep(0)
        interaction.followup.send.assert_not_awaited()

        release_delete.set()
        await asyncio.wait_for(next_sent.wait(), timeout=1)
        await answer_task

        assert order.index("edit_done") < order.index("next_send")
        assert order.index("image_done") < order.index("next_send")
        assert session.prev_message is current_message
        assert session.message is next_message
        assert session.active is True

    @pytest.mark.asyncio
    async def test_correct_edit_error_waits_for_cleanup_and_preparation_then_propagates(
        self,
        monkeypatch,
    ):
        import commands.trivia as trivia_mod

        view, session, cog, current_message, previous_message = _transition_view(
            trivia_mod
        )
        original_previous = previous_message
        delete_started = asyncio.Event()
        image_started = asyncio.Event()
        release_delete = asyncio.Event()
        release_image = asyncio.Event()
        prepared_file = SimpleNamespace(close=MagicMock())
        edit_error = discord.HTTPException(
            SimpleNamespace(status=500, reason="error"),
            "edit failed",
        )

        async def delete_previous():
            delete_started.set()
            await release_delete.wait()

        def generate_next(streak, recent_categories):
            return _trivia_question()

        async def controlled_to_thread(func, *args, **kwargs):
            if func is trivia_mod._prepare_question:
                image_started.set()
                await release_image.wait()
                return prepared_file
            return func(*args, **kwargs)

        previous_message.delete = AsyncMock(side_effect=delete_previous)
        interaction = SimpleNamespace(
            user=SimpleNamespace(id=session.user_id),
            response=SimpleNamespace(
                edit_message=AsyncMock(side_effect=edit_error),
            ),
            followup=SimpleNamespace(send=AsyncMock()),
        )
        monkeypatch.setattr(trivia_mod, "generate_question", generate_next)
        monkeypatch.setattr(trivia_mod.asyncio, "to_thread", controlled_to_thread)

        answer_task = asyncio.create_task(view._handle_answer(interaction, 1))
        await asyncio.wait_for(
            asyncio.gather(delete_started.wait(), image_started.wait()),
            timeout=1,
        )

        # The required edit has failed, but sibling work is still fully awaited.
        await asyncio.sleep(0)
        assert not answer_task.done()

        release_delete.set()
        release_image.set()
        with pytest.raises(discord.HTTPException):
            await answer_task

        interaction.followup.send.assert_not_awaited()
        prepared_file.close.assert_called_once_with()
        assert session.prev_message is original_previous
        assert session.message is current_message
        assert session.active is True
        assert (session.user_id, session.guild_id) in cog._sessions

    @pytest.mark.asyncio
    async def test_wrong_answer_overlaps_best_effort_delete_with_required_edit(
        self,
    ):
        import commands.trivia as trivia_mod

        view, session, cog, _, previous_message = _transition_view(trivia_mod)
        delete_started = asyncio.Event()
        edit_started = asyncio.Event()
        release_delete = asyncio.Event()
        release_edit = asyncio.Event()

        async def delete_previous():
            delete_started.set()
            await release_delete.wait()
            raise discord.HTTPException(
                SimpleNamespace(status=500, reason="error"),
                "delete failed",
            )

        async def edit_current(**kwargs):
            edit_started.set()
            await release_edit.wait()

        previous_message.delete = AsyncMock(side_effect=delete_previous)
        interaction = SimpleNamespace(
            user=SimpleNamespace(id=session.user_id),
            response=SimpleNamespace(edit_message=AsyncMock(side_effect=edit_current)),
        )

        answer_task = asyncio.create_task(view._handle_answer(interaction, 0))
        await asyncio.wait_for(
            asyncio.gather(delete_started.wait(), edit_started.wait()),
            timeout=1,
        )

        assert session.active is True
        assert not answer_task.done()
        release_delete.set()
        release_edit.set()
        await answer_task

        assert session.active is False
        assert (session.user_id, session.guild_id) not in cog._sessions

    @pytest.mark.asyncio
    async def test_timeout_overlaps_delete_and_message_edit_before_session_end(self):
        import commands.trivia as trivia_mod

        view, session, cog, current_message, previous_message = _transition_view(
            trivia_mod
        )
        delete_started = asyncio.Event()
        edit_started = asyncio.Event()
        release_delete = asyncio.Event()
        release_edit = asyncio.Event()

        async def delete_previous():
            delete_started.set()
            await release_delete.wait()

        async def edit_current(**kwargs):
            edit_started.set()
            await release_edit.wait()

        previous_message.delete = AsyncMock(side_effect=delete_previous)
        current_message.edit = AsyncMock(side_effect=edit_current)

        timeout_task = asyncio.create_task(view.on_timeout())
        await asyncio.wait_for(
            asyncio.gather(delete_started.wait(), edit_started.wait()),
            timeout=1,
        )

        assert view.answered is True
        assert session.active is True
        release_edit.set()
        await asyncio.sleep(0)
        assert not timeout_task.done()

        release_delete.set()
        await timeout_task

        assert session.active is False
        assert (session.user_id, session.guild_id) not in cog._sessions


class TestTriviaSessionRecording:
    def test_record_and_leaderboard(self, player_service, registered_player):
        # Record a few sessions
        player_service.record_trivia_session(registered_player, TEST_GUILD_ID, streak=5, jc_earned=5)

        # Register a second player and record
        discord_id_2 = 100002
        player_service.register_player(
            discord_id=discord_id_2,
            discord_username="trivia_tester_2",
            guild_id=TEST_GUILD_ID,
            steam_id=12346,
            mmr_override=3000,
        )
        player_service.record_trivia_session(discord_id_2, TEST_GUILD_ID, streak=10, jc_earned=10)

        # Leaderboard should return player 2 first (higher streak)
        lb = player_service.get_trivia_leaderboard(TEST_GUILD_ID)
        assert len(lb) == 2
        assert lb[0]["discord_id"] == discord_id_2
        assert lb[0]["best_streak"] == 10
        assert lb[1]["discord_id"] == registered_player
        assert lb[1]["best_streak"] == 5

    def test_leaderboard_uses_max_streak(self, player_service, registered_player):
        """If a player has multiple sessions, leaderboard shows their best."""
        player_service.record_trivia_session(registered_player, TEST_GUILD_ID, streak=3, jc_earned=3)
        player_service.record_trivia_session(registered_player, TEST_GUILD_ID, streak=8, jc_earned=8)
        player_service.record_trivia_session(registered_player, TEST_GUILD_ID, streak=2, jc_earned=2)

        lb = player_service.get_trivia_leaderboard(TEST_GUILD_ID)
        assert len(lb) == 1
        assert lb[0]["best_streak"] == 8

    def test_leaderboard_empty_when_no_sessions(self, player_service):
        lb = player_service.get_trivia_leaderboard(TEST_GUILD_ID)
        assert lb == []

    def test_leaderboard_respects_time_window(self, player_service, registered_player):
        """Sessions older than the window should not appear."""
        repo = player_service.player_repo
        old_time = int(time.time()) - 8 * 86400  # 8 days ago
        repo.record_trivia_session(registered_player, TEST_GUILD_ID, streak=20, jc_earned=20, played_at=old_time)

        lb = player_service.get_trivia_leaderboard(TEST_GUILD_ID, days=7)
        assert lb == []

    def test_leaderboard_limit(self, player_service):
        """Leaderboard should respect the limit parameter."""
        for i in range(5):
            discord_id = 200000 + i
            player_service.register_player(
                discord_id=discord_id,
                discord_username=f"lb_test_{i}",
                guild_id=TEST_GUILD_ID,
                steam_id=50000 + i,
                mmr_override=3000,
            )
            player_service.record_trivia_session(discord_id, TEST_GUILD_ID, streak=i + 1, jc_earned=i + 1)

        lb = player_service.get_trivia_leaderboard(TEST_GUILD_ID, limit=3)
        assert len(lb) == 3
        # Top 3 by streak should be players with streaks 5, 4, 3
        assert lb[0]["best_streak"] == 5
        assert lb[1]["best_streak"] == 4
        assert lb[2]["best_streak"] == 3

    def test_leaderboard_uses_discord_id_for_streak_ties(self, player_service):
        """Equal best streaks have a stable final Discord-ID ordering."""
        for discord_id in (300002, 300001):
            player_service.register_player(
                discord_id=discord_id,
                discord_username=f"lb_tie_{discord_id}",
                guild_id=TEST_GUILD_ID,
                steam_id=discord_id,
                mmr_override=3000,
            )
            player_service.record_trivia_session(
                discord_id,
                TEST_GUILD_ID,
                streak=7,
                jc_earned=7,
            )

        leaderboard = player_service.get_trivia_leaderboard(TEST_GUILD_ID)
        assert [entry["discord_id"] for entry in leaderboard] == [300001, 300002]
