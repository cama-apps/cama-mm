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


@pytest.mark.asyncio
async def test_long_and_mention_options_remain_in_embed_not_button_labels():
    from commands.player_trivia import (
        PlayerTriviaSession,
        PlayerTriviaView,
        _question_embed,
    )
    from services.player_trivia_service import PlayerTriviaQuestion

    options = (
        "Market #8 — Will Team Radiant win the next inhouse match?",
        "Market #14 — Will the game last longer than 45 minutes?",
        "<@202> — Trivia Player's server profile",
        "<@303> — Another Player's server profile",
    )
    question = PlayerTriviaQuestion(
        key="predictions:profit:101",
        category="predictions",
        text="Which resolved market did <@101> finish with a profit on?",
        options=options,
        correct_index=0,
        explanation="Computed from resolved prediction markets.",
    )
    service = FakePlayerTriviaService(questions=[question])
    cog = _cog(service)
    session = PlayerTriviaSession(
        session_id=42,
        user_id=101,
        guild_id=TEST_GUILD_ID,
        user=_user(),
        questions=[question],
    )

    embed = _question_embed(session)
    view = PlayerTriviaView(session, cog)

    choose_field = next(field for field in embed.fields if field.name == "Choose one")
    assert all(option in choose_field.value for option in options)
    assert [button.label for button in view.children] == ["A", "B", "C", "D"]


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

    def try_start_session(
        self, user_id, guild_id, questions, now, cooldown, *, bypass=False
    ):
        self.started.append((user_id, guild_id, questions, now, cooldown, bypass))
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
    embed = followup.await_args.kwargs["embed"]
    assert embed.fields[0].name == "Choose one"
    assert embed.fields[1].name == "Your daily set"
    assert embed.fields[1].value == (
        "Each player gets an independently generated set; some questions may overlap."
    )


@pytest.mark.asyncio
async def test_admin_bypasses_player_trivia_cooldown(monkeypatch):
    import commands.player_trivia as module

    now = 1_700_000_000
    service = FakePlayerTriviaService()
    service.last_started = now - 60
    cog = _cog(service)
    interaction = _interaction()
    followup = AsyncMock(return_value=SimpleNamespace(edit=AsyncMock()))
    monkeypatch.setattr(module, "require_gamba_channel", AsyncMock(return_value=True))
    monkeypatch.setattr(module, "has_admin_permission", MagicMock(return_value=True))
    monkeypatch.setattr(module.time, "time", lambda: now)
    monkeypatch.setattr(module, "safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr(module, "safe_followup", followup)

    await cog.player_trivia.callback(cog, interaction)

    interaction.response.send_message.assert_not_awaited()
    assert len(service.started) == 1
    assert service.started[0][3:] == (
        now,
        module.PLAYER_TRIVIA_COOLDOWN_SECONDS,
        True,
    )
    assert (interaction.user.id, TEST_GUILD_ID) in cog._sessions


@pytest.mark.asyncio
async def test_non_admin_remains_subject_to_player_trivia_cooldown(monkeypatch):
    import commands.player_trivia as module

    now = 1_700_000_000
    service = FakePlayerTriviaService()
    service.last_started = now - 60
    service.generate_questions = MagicMock(wraps=service.generate_questions)
    cog = _cog(service)
    interaction = _interaction()
    monkeypatch.setattr(module, "require_gamba_channel", AsyncMock(return_value=True))
    monkeypatch.setattr(module, "has_admin_permission", MagicMock(return_value=False))
    monkeypatch.setattr(module.time, "time", lambda: now)

    await cog.player_trivia.callback(cog, interaction)

    interaction.response.send_message.assert_awaited_once_with(
        "Player trivia is on cooldown! Your next set unlocks "
        f"<t:{service.last_started + module.PLAYER_TRIVIA_COOLDOWN_SECONDS}:R>.",
        ephemeral=True,
    )
    service.generate_questions.assert_not_called()
    assert service.started == []
    assert cog._sessions == {}


@pytest.mark.asyncio
async def test_command_generates_and_tracks_separate_sets_per_player(monkeypatch):
    import commands.player_trivia as module

    service = FakePlayerTriviaService()
    service.generate_questions = MagicMock(
        side_effect=lambda user_id, *_args: [
            _question(user_id * 100 + index) for index in range(module.PLAYER_TRIVIA_QUESTION_COUNT)
        ]
    )
    service.try_start_session = MagicMock(side_effect=[42, 43])
    cog = _cog(service)
    first_interaction = _interaction(user_id=101)
    second_interaction = _interaction(user_id=202)
    monkeypatch.setattr(module, "require_gamba_channel", AsyncMock(return_value=True))
    monkeypatch.setattr(module, "safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr(
        module,
        "safe_followup",
        AsyncMock(
            side_effect=[
                SimpleNamespace(edit=AsyncMock()),
                SimpleNamespace(edit=AsyncMock()),
            ]
        ),
    )

    await cog.player_trivia.callback(cog, first_interaction)
    await cog.player_trivia.callback(cog, second_interaction)

    assert [call.args[0] for call in service.generate_questions.call_args_list] == [101, 202]
    first_session = cog._sessions[(101, TEST_GUILD_ID)]
    second_session = cog._sessions[(202, TEST_GUILD_ID)]
    assert first_session.session_id == 42
    assert second_session.session_id == 43
    assert first_session.questions[0].key != second_session.questions[0].key
    assert first_session.questions is not second_session.questions


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
    edit_kwargs = interaction.response.edit_message.await_args.kwargs
    edited_view = edit_kwargs["view"]
    assert isinstance(edited_view, PlayerTriviaView)
    assert [button.label for button in edited_view.children] == ["A", "B", "C", "D"]
    embed = edit_kwargs["embed"]
    assert embed.description == service.questions[1].text
    assert [field.name for field in embed.fields] == [
        "Choose one",
        "Previous question result",
    ]
    assert "Bravo" in embed.fields[1].value
    assert service.questions[0].text not in embed.description


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
