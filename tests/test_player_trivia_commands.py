"""Command and component-flow tests for the daily player trivia game."""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from tests.conftest import TEST_GUILD_ID


def _question(number: int, *, correct_index: int = 1):
    from services.player_trivia_service import PlayerTriviaQuestion

    return PlayerTriviaQuestion(
        key=f"matches:wins:{number}",
        category="matches",
        text=f"Who won match question {number}?",
        options=("Alpha", "Bravo", "Charlie", "Delta"),
        correct_index=correct_index,
        explanation="Computed from recorded inhouse matches.",
    )


def _user(user_id: int = 101):
    avatar = SimpleNamespace(url="https://example.com/avatar.png")
    return SimpleNamespace(
        id=user_id,
        display_name="Trivia Player",
        display_avatar=avatar,
        bot=False,
    )


def _interaction(user_id: int = 101):
    response = SimpleNamespace(
        send_message=AsyncMock(),
        edit_message=AsyncMock(),
        defer=AsyncMock(),
        is_done=MagicMock(return_value=False),
    )
    return SimpleNamespace(
        user=_user(user_id),
        guild=SimpleNamespace(id=TEST_GUILD_ID, members=[_user(user_id)]),
        channel=SimpleNamespace(name="gamba"),
        response=response,
        followup=SimpleNamespace(send=AsyncMock()),
        message=SimpleNamespace(edit=AsyncMock()),
    )


class FakePlayerTriviaService:
    def __init__(self, questions=None):
        self.questions = questions if questions is not None else [_question(i) for i in range(10)]
        self.last_started = None
        self.started = []
        self.settlements = []
        self.finished = []
        self.cancelled = []
        self.next_result = {
            "is_correct": False,
            "reward": 0,
            "score": 0,
            "jc_earned": 0,
            "completed": False,
        }

    def generate_questions(
        self,
        user_id,
        guild_id,
        current_member_ids,
        count,
        include_spicy,
        recent_days,
    ):
        self.generated_with = (
            user_id,
            guild_id,
            current_member_ids,
            count,
            include_spicy,
            recent_days,
        )
        return self.questions

    def get_last_session_started(self, user_id, guild_id):
        return self.last_started

    def try_start_session(self, user_id, guild_id, questions, now, cooldown):
        self.started.append((user_id, guild_id, questions, now, cooldown))
        return 42

    def settle_answer(self, session_id, question_number, selected_index, reward, answered_at):
        self.settlements.append((session_id, question_number, selected_index, reward, answered_at))
        return self.next_result

    def finish_session(self, session_id, status, completed_at):
        self.finished.append((session_id, status, completed_at))
        return True

    def cancel_session_if_unanswered(self, session_id):
        self.cancelled.append(session_id)
        return True


def _cog(service: FakePlayerTriviaService):
    from commands.player_trivia import PlayerTriviaCog

    bot = SimpleNamespace(
        player_trivia_service=service,
        player_service=SimpleNamespace(get_player=lambda _user, _guild: object()),
    )
    return PlayerTriviaCog(bot)


@pytest.mark.asyncio
async def test_command_starts_frozen_daily_set_with_spicy_disabled(monkeypatch):
    import commands.player_trivia as module

    service = FakePlayerTriviaService()
    cog = _cog(service)
    interaction = _interaction()
    message = SimpleNamespace(edit=AsyncMock())
    followup = AsyncMock(return_value=message)
    monkeypatch.setattr(module, "require_gamba_channel", AsyncMock(return_value=True))
    monkeypatch.setattr(module, "safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr(module, "safe_followup", followup)

    await cog.player_trivia.callback(cog, interaction)

    assert service.generated_with[4] is False
    assert service.generated_with[3] == module.PLAYER_TRIVIA_QUESTION_COUNT
    assert len(service.started) == 1
    assert len(service.started[0][2]) == module.PLAYER_TRIVIA_QUESTION_COUNT
    assert (interaction.user.id, TEST_GUILD_ID) in cog._sessions
    assert cog._sessions[(interaction.user.id, TEST_GUILD_ID)].message is message


@pytest.mark.asyncio
async def test_insufficient_bank_does_not_claim_cooldown(monkeypatch):
    import commands.player_trivia as module

    service = FakePlayerTriviaService(questions=[_question(1)])
    cog = _cog(service)
    interaction = _interaction()
    followup = AsyncMock()
    monkeypatch.setattr(module, "require_gamba_channel", AsyncMock(return_value=True))
    monkeypatch.setattr(module, "safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr(module, "safe_followup", followup)

    await cog.player_trivia.callback(cog, interaction)

    assert service.started == []
    assert service.last_started is None
    assert "No cooldown was used" in followup.await_args.kwargs["content"]


@pytest.mark.asyncio
async def test_first_message_failure_cancels_unanswered_session(monkeypatch):
    import commands.player_trivia as module

    service = FakePlayerTriviaService()
    cog = _cog(service)
    interaction = _interaction()
    monkeypatch.setattr(module, "require_gamba_channel", AsyncMock(return_value=True))
    monkeypatch.setattr(module, "safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr(module, "safe_followup", AsyncMock(side_effect=RuntimeError("send failed")))

    await cog.player_trivia.callback(cog, interaction)

    assert service.cancelled == [42]
    assert cog._sessions == {}


@pytest.mark.asyncio
async def test_wrong_answer_continues_to_next_question():
    from commands.player_trivia import (
        PlayerTriviaSession,
        PlayerTriviaView,
    )

    service = FakePlayerTriviaService(questions=[_question(1), _question(2)])
    service.next_result = {
        "is_correct": False,
        "reward": 0,
        "score": 0,
        "jc_earned": 0,
        "completed": False,
    }
    cog = _cog(service)
    session = PlayerTriviaSession(
        session_id=42,
        user_id=101,
        guild_id=TEST_GUILD_ID,
        user=_user(),
        questions=service.questions,
    )
    cog._sessions[(101, TEST_GUILD_ID)] = session
    interaction = _interaction()
    view = PlayerTriviaView(session, cog)

    await view._handle_answer(interaction, 0)

    assert session.active is True
    assert session.current_index == 1
    assert service.finished == []
    assert service.settlements[0][1:4] == (1, 0, 1)
    edited_view = interaction.response.edit_message.await_args.kwargs["view"]
    assert isinstance(edited_view, PlayerTriviaView)


@pytest.mark.asyncio
async def test_last_correct_answer_shows_summary_and_ends_in_memory_session():
    from commands.player_trivia import PlayerTriviaSession, PlayerTriviaView

    service = FakePlayerTriviaService(questions=[_question(1)])
    service.next_result = {
        "is_correct": True,
        "reward": 1,
        "score": 1,
        "jc_earned": 1,
        "completed": True,
    }
    cog = _cog(service)
    session = PlayerTriviaSession(
        session_id=42,
        user_id=101,
        guild_id=TEST_GUILD_ID,
        user=_user(),
        questions=service.questions,
    )
    cog._sessions[(101, TEST_GUILD_ID)] = session
    interaction = _interaction()

    await PlayerTriviaView(session, cog)._handle_answer(interaction, 1)

    assert session.active is False
    assert session.score == 1
    assert session.total_jc == 1
    assert cog._sessions == {}
    assert service.finished == []  # final-answer settlement completes atomically
    assert interaction.response.edit_message.await_args.kwargs["view"] is None


@pytest.mark.asyncio
async def test_other_player_is_rejected_ephemerally():
    from commands.player_trivia import PlayerTriviaSession, PlayerTriviaView

    service = FakePlayerTriviaService(questions=[_question(1)])
    cog = _cog(service)
    session = PlayerTriviaSession(
        session_id=42,
        user_id=101,
        guild_id=TEST_GUILD_ID,
        user=_user(),
        questions=service.questions,
    )
    interaction = _interaction(user_id=999)

    await PlayerTriviaView(session, cog)._handle_answer(interaction, 0)

    assert service.settlements == []
    interaction.response.send_message.assert_awaited_once_with(
        "This isn't your player-trivia session!", ephemeral=True
    )


@pytest.mark.asyncio
async def test_timeout_finishes_run_and_keeps_daily_cooldown():
    from commands.player_trivia import PlayerTriviaSession, PlayerTriviaView

    service = FakePlayerTriviaService(questions=[_question(1), _question(2)])
    cog = _cog(service)
    message = SimpleNamespace(edit=AsyncMock())
    session = PlayerTriviaSession(
        session_id=42,
        user_id=101,
        guild_id=TEST_GUILD_ID,
        user=_user(),
        questions=service.questions,
        message=message,
    )
    cog._sessions[(101, TEST_GUILD_ID)] = session

    await PlayerTriviaView(session, cog).on_timeout()

    assert session.active is False
    assert service.finished[0][0:2] == (42, "timed_out")
    assert service.cancelled == []
    message.edit.assert_awaited_once()
