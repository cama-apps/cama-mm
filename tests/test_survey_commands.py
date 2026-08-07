"""Focused Discord command and UI coverage for one-off surveys."""

from __future__ import annotations

import asyncio
from dataclasses import replace
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, call

import discord
import pytest

from commands import survey as survey_commands
from domain.models.survey import (
    Survey,
    SurveyDeliveryStatus,
    SurveyDraftAnswer,
    SurveyQuestion,
    SurveyQuestionResult,
    SurveyQuestionType,
    SurveyRecipient,
    SurveyResults,
    SurveySession,
    SurveyStatus,
)
from utils.embed_safety import validate_embed


def _survey(*, survey_id: int = 11, status: SurveyStatus = SurveyStatus.OPEN) -> Survey:
    return Survey(
        survey_id=survey_id,
        guild_id=77,
        title="Cama feedback",
        status=status,
        target_type=None,
        registered_only=False,
        created_at=1,
        opened_at=2 if status is not SurveyStatus.DRAFT else None,
        closed_at=3 if status is SurveyStatus.CLOSED else None,
    )


def _question(
    question_id: int,
    position: int,
    question_type: SurveyQuestionType,
    *,
    required: bool = True,
    prompt: str | None = None,
) -> SurveyQuestion:
    return SurveyQuestion(
        question_id=question_id,
        survey_id=11,
        position=position,
        prompt=prompt or f"Question {position}?",
        question_type=question_type,
        required=required,
        created_at=1,
    )


def _recipient(
    *,
    recipient_id: int = 501,
    discord_id: int = 42,
    current_question_id: int | None = 101,
    in_review: bool = False,
    submitted_at: int | None = None,
    delivery_status: SurveyDeliveryStatus = SurveyDeliveryStatus.SENT,
    dm_message_id: int | None = 7001,
    ui_message_id: int | None = None,
) -> SurveyRecipient:
    return SurveyRecipient(
        recipient_id=recipient_id,
        survey_id=11,
        guild_id=77,
        discord_id=discord_id,
        delivery_status=delivery_status,
        attempt_count=1,
        claimed_at=None,
        dm_channel_id=6001,
        dm_message_id=dm_message_id,
        ui_channel_id=6001 if ui_message_id is not None else None,
        ui_message_id=ui_message_id,
        current_question_id=current_question_id,
        in_review=in_review,
        started_at=5,
        submitted_at=submitted_at,
        last_error=None,
        created_at=2,
        updated_at=5,
    )


def _session(
    *,
    recipient: SurveyRecipient | None = None,
    questions: tuple[SurveyQuestion, ...] | None = None,
    answers: tuple[SurveyDraftAnswer, ...] = (),
) -> SurveySession:
    if questions is None:
        questions = (
            _question(101, 1, SurveyQuestionType.NPS),
            _question(102, 2, SurveyQuestionType.TEXT, required=False),
        )
    return SurveySession(
        survey=_survey(),
        recipient=recipient or _recipient(),
        questions=questions,
        answers=answers,
    )


def _member(
    discord_id: int,
    guild,
    *,
    bot: bool = False,
):
    return SimpleNamespace(id=discord_id, guild=guild, bot=bot)


def _interaction(*, user_id: int = 42, guild=None):
    response = SimpleNamespace(
        is_done=MagicMock(return_value=False),
        send_message=AsyncMock(),
        edit_message=AsyncMock(),
        defer=AsyncMock(),
        send_modal=AsyncMock(),
    )
    return SimpleNamespace(
        user=SimpleNamespace(id=user_id),
        guild=guild,
        response=response,
        followup=SimpleNamespace(send=AsyncMock()),
        edit_original_response=AsyncMock(),
    )


def _cog(*, bot=None, survey_service=None, player_service=None):
    bot = bot or SimpleNamespace(
        get_user=MagicMock(),
        fetch_user=AsyncMock(),
        add_view=MagicMock(),
    )
    return survey_commands.SurveyCommands(
        bot,
        survey_service or MagicMock(),
        player_service or MagicMock(),
    )


def test_survey_commands_are_guild_only_without_discord_visibility_gate():
    commands = (
        survey_commands.SurveyCommands.create,
        survey_commands.SurveyCommands.list_surveys,
        survey_commands.SurveyCommands.preview,
        survey_commands.SurveyCommands.delete,
        survey_commands.SurveyCommands.send,
        survey_commands.SurveyCommands.retry,
        survey_commands.SurveyCommands.results,
        survey_commands.SurveyCommands.close,
        survey_commands.SurveyCommands.question_add,
        survey_commands.SurveyCommands.question_edit,
        survey_commands.SurveyCommands.question_remove,
        survey_commands.SurveyCommands.question_move,
    )
    assert survey_commands.SurveyCommands.survey.guild_only is True
    assert survey_commands.SurveyCommands.survey.default_permissions is None
    assert all(command.default_permissions is None for command in commands)


def test_delivery_history_match_uses_persistent_component_ids_only():
    message = SimpleNamespace(
        nonce=None,
        components=[
            SimpleNamespace(
                children=[SimpleNamespace(custom_id="survey:11:question:101:score")]
            )
        ],
    )

    assert survey_commands._message_matches_delivery(message, 11) is True
    assert survey_commands._message_matches_delivery(message, 12) is False

    # History-fetched messages never expose the send-time nonce, so a nonce
    # alone (with no components) must never identify a delivery.
    nonce_only = SimpleNamespace(
        nonce=survey_commands._delivery_nonce(501),
        components=[],
    )
    assert survey_commands._message_matches_delivery(nonce_only, 11) is False


@pytest.mark.asyncio
async def test_runtime_admin_guard_blocks_service_access(monkeypatch):
    guild = SimpleNamespace(id=77, members=[])
    interaction = _interaction(user_id=901, guild=guild)
    service = MagicMock()
    cog = _cog(survey_service=service)
    service.reset_mock()
    monkeypatch.setattr(survey_commands, "has_admin_permission", lambda _i: False)

    await cog.create.callback(cog, interaction, "Private survey")

    assert service.mock_calls == []
    interaction.response.send_message.assert_awaited_once()
    assert interaction.response.send_message.await_args.kwargs["ephemeral"] is True
    assert "Admin only" in interaction.response.send_message.await_args.args[0]


@pytest.mark.asyncio
async def test_all_registered_filters_bots_fake_players_and_departed_players():
    guild = SimpleNamespace(id=77, members=[])
    current = _member(1, guild)
    bot = _member(2, guild, bot=True)
    unregistered = _member(3, guild)
    guild.members = [current, bot, unregistered]
    player_service = MagicMock()
    player_service.get_all.return_value = [
        SimpleNamespace(discord_id=1),
        SimpleNamespace(discord_id=2),
        SimpleNamespace(discord_id=4),  # Registered, but no longer in the guild.
        SimpleNamespace(discord_id=-1),  # Synthetic/fake player.
    ]
    cog = _cog(player_service=player_service)

    recipients = await cog._resolve_recipients(
        guild,
        survey_commands.SurveyTargetType.ALL_REGISTERED,
        member=None,
        role=None,
        registered_only=False,
    )

    assert recipients == [1]
    player_service.get_all.assert_called_once_with(77)


@pytest.mark.asyncio
async def test_member_target_allows_unregistered_unless_filter_requested():
    guild = SimpleNamespace(id=77, members=[])
    target = _member(3, guild)
    guild.members = [target]
    player_service = MagicMock()
    player_service.get_all.return_value = []
    cog = _cog(player_service=player_service)

    assert await cog._resolve_recipients(
        guild,
        survey_commands.SurveyTargetType.MEMBER,
        member=target,
        role=None,
        registered_only=False,
    ) == [3]

    with pytest.raises(ValueError, match="not a registered"):
        await cog._resolve_recipients(
            guild,
            survey_commands.SurveyTargetType.MEMBER,
            member=target,
            role=None,
            registered_only=True,
        )


@pytest.mark.asyncio
async def test_member_target_rejects_bots_and_members_from_another_guild():
    guild = SimpleNamespace(id=77, members=[])
    other_guild = SimpleNamespace(id=88, members=[])
    cog = _cog()

    with pytest.raises(ValueError, match="Bots"):
        await cog._resolve_recipients(
            guild,
            survey_commands.SurveyTargetType.MEMBER,
            member=_member(2, guild, bot=True),
            role=None,
            registered_only=False,
        )
    with pytest.raises(ValueError, match="not in this server"):
        await cog._resolve_recipients(
            guild,
            survey_commands.SurveyTargetType.MEMBER,
            member=_member(3, other_guild),
            role=None,
            registered_only=False,
        )
    with pytest.raises(ValueError, match="not in this server"):
        await cog._resolve_recipients(
            guild,
            survey_commands.SurveyTargetType.MEMBER,
            member=_member(4, guild),
            role=None,
            registered_only=False,
        )


@pytest.mark.asyncio
async def test_role_target_filters_bots_and_honors_registered_only():
    guild = SimpleNamespace(id=77, members=[])
    registered = _member(1, guild)
    unregistered = _member(3, guild)
    bot = _member(2, guild, bot=True)
    guild.members = [registered, unregistered, bot]
    role = SimpleNamespace(id=555, guild=guild, members=[registered, unregistered, bot])
    player_service = MagicMock()
    player_service.get_all.return_value = [SimpleNamespace(discord_id=1)]
    cog = _cog(player_service=player_service)

    assert await cog._resolve_recipients(
        guild,
        survey_commands.SurveyTargetType.ROLE,
        member=None,
        role=role,
        registered_only=False,
    ) == [1, 3]
    assert await cog._resolve_recipients(
        guild,
        survey_commands.SurveyTargetType.ROLE,
        member=None,
        role=role,
        registered_only=True,
    ) == [1]

    role.members.append(_member(4, guild))  # Stale/departed cached role member.
    recipients = await cog._resolve_recipients(
        guild,
        survey_commands.SurveyTargetType.ROLE,
        member=None,
        role=role,
        registered_only=False,
    )
    assert recipients == [1, 3]


@pytest.mark.asyncio
async def test_successful_dm_marks_delivery_sent_with_persistent_controls():
    claimed = _recipient(
        delivery_status=SurveyDeliveryStatus.SENDING,
        dm_message_id=None,
    )
    session = _session(recipient=claimed)
    service = MagicMock()
    service.get_response_session.return_value = session
    user = SimpleNamespace(
        send=AsyncMock(
            return_value=SimpleNamespace(id=7001, channel=SimpleNamespace(id=6001))
        )
    )
    bot = SimpleNamespace(
        get_user=MagicMock(return_value=user),
        fetch_user=AsyncMock(),
        add_view=MagicMock(),
    )
    cog = _cog(bot=bot, survey_service=service)

    await cog._deliver_recipient(claimed)

    sent_kwargs = user.send.await_args.kwargs
    assert isinstance(sent_kwargs["view"], survey_commands.SurveyQuestionView)
    assert sent_kwargs["view"].timeout is None
    assert sent_kwargs["nonce"] == survey_commands._delivery_nonce(501)
    assert sent_kwargs["allowed_mentions"] == survey_commands._ALLOWED_MENTIONS
    service.mark_delivery_sent.assert_called_once_with(501, 1, 6001, 7001)
    service.mark_delivery_failed.assert_not_called()
    bot.fetch_user.assert_not_awaited()


@pytest.mark.asyncio
async def test_successful_dm_is_not_relabelled_failed_when_receipt_persistence_fails():
    claimed = _recipient(
        delivery_status=SurveyDeliveryStatus.SENDING,
        dm_message_id=None,
    )
    service = MagicMock()
    service.get_response_session.return_value = _session(recipient=claimed)
    service.mark_delivery_sent.side_effect = RuntimeError("database unavailable")
    user = SimpleNamespace(
        send=AsyncMock(
            return_value=SimpleNamespace(id=7001, channel=SimpleNamespace(id=6001))
        )
    )
    bot = SimpleNamespace(
        get_user=MagicMock(return_value=user),
        fetch_user=AsyncMock(),
        add_view=MagicMock(),
    )
    cog = _cog(bot=bot, survey_service=service)

    await cog._deliver_recipient(claimed)

    service.mark_delivery_sent.assert_called_once_with(501, 1, 6001, 7001)
    service.mark_delivery_failed.assert_not_called()
    assert user.send.await_args.kwargs["nonce"] == survey_commands._delivery_nonce(501)
    assert survey_commands._delivery_nonce(501) == survey_commands._delivery_nonce(501)


@pytest.mark.asyncio
async def test_late_post_close_dm_receipt_is_attached_and_controls_removed():
    claimed = _recipient(
        delivery_status=SurveyDeliveryStatus.SENDING,
        dm_message_id=None,
    )
    open_session = _session(recipient=claimed)
    reconciled_recipient = replace(
        claimed,
        delivery_status=SurveyDeliveryStatus.SENT,
        dm_channel_id=6001,
        dm_message_id=7001,
        ui_channel_id=6001,
        ui_message_id=7001,
    )
    closed_session = SurveySession(
        survey=_survey(status=SurveyStatus.CLOSED),
        recipient=reconciled_recipient,
        questions=open_session.questions,
        answers=(),
    )
    sent_message = SimpleNamespace(id=7001, channel=SimpleNamespace(id=6001))
    persisted_message = SimpleNamespace(edit=AsyncMock())
    dm_channel = SimpleNamespace(
        get_partial_message=MagicMock(return_value=persisted_message)
    )
    user = SimpleNamespace(
        send=AsyncMock(return_value=sent_message),
        create_dm=AsyncMock(return_value=dm_channel),
    )
    service = MagicMock()
    service.get_response_session.side_effect = [
        open_session,
        closed_session,
        closed_session,
    ]
    service.mark_delivery_sent.side_effect = RuntimeError("survey closed")
    bot = SimpleNamespace(
        get_user=MagicMock(return_value=user),
        fetch_user=AsyncMock(),
        add_view=MagicMock(),
    )
    cog = _cog(bot=bot, survey_service=service)

    await cog._deliver_recipient(claimed)

    service.reconcile_delivery_sent.assert_called_once_with(501, 1, 6001, 7001)
    edit_kwargs = persisted_message.edit.await_args.kwargs
    assert edit_kwargs["embed"].title == "Survey closed"
    assert edit_kwargs["view"] is None


@pytest.mark.asyncio
async def test_delayed_recovery_finds_existing_dm_in_history_without_resending():
    claimed = _recipient(
        delivery_status=SurveyDeliveryStatus.SENDING,
        dm_message_id=None,
    )
    message = SimpleNamespace(
        id=7001,
        nonce=None,
        components=[
            SimpleNamespace(
                children=[SimpleNamespace(custom_id="survey:11:question:101:score")]
            )
        ],
        author=SimpleNamespace(id=999),
    )

    async def history_iter():
        yield message

    channel = SimpleNamespace(
        id=6001,
        history=MagicMock(return_value=history_iter()),
    )
    message.channel = channel
    user = SimpleNamespace(create_dm=AsyncMock(return_value=channel))
    service = MagicMock()
    service.list_unconfirmed_deliveries.return_value = [claimed]
    bot = SimpleNamespace(
        user=SimpleNamespace(id=999),
        get_user=MagicMock(return_value=user),
        fetch_user=AsyncMock(),
        add_view=MagicMock(),
    )
    cog = _cog(bot=bot, survey_service=service)

    assert await cog.reconcile_unconfirmed_delivery_receipts(77, 11) == 1

    service.list_unconfirmed_deliveries.assert_called_once_with(
        77,
        11,
        survey_commands._DELIVERY_STALE_SECONDS,
        survey_commands._DELIVERY_RECEIPT_GRACE_SECONDS,
    )
    service.reconcile_delivery_sent.assert_called_once_with(501, 1, 6001, 7001)
    service.mark_delivery_receipt_checked.assert_not_called()
    history_kwargs = channel.history.call_args.kwargs
    assert history_kwargs["limit"] == survey_commands._DELIVERY_RECEIPT_SCAN_LIMIT
    assert history_kwargs["after"].tzinfo is not None
    assert history_kwargs["oldest_first"] is False
    assert not hasattr(user, "send")


@pytest.mark.asyncio
async def test_no_history_receipt_uses_the_exact_delivery_snapshot_cas():
    claimed = replace(
        _recipient(
            delivery_status=SurveyDeliveryStatus.SENDING,
            dm_message_id=None,
        ),
        claimed_at=4,
        updated_at=5,
    )

    async def empty_history():
        if False:
            yield None

    channel = SimpleNamespace(
        id=6001,
        history=MagicMock(return_value=empty_history()),
    )
    user = SimpleNamespace(create_dm=AsyncMock(return_value=channel))
    service = MagicMock()
    service.list_unconfirmed_deliveries.return_value = [claimed]
    bot = SimpleNamespace(
        user=SimpleNamespace(id=999),
        get_user=MagicMock(return_value=user),
        fetch_user=AsyncMock(),
        add_view=MagicMock(),
    )
    cog = _cog(bot=bot, survey_service=service)

    assert await cog.reconcile_unconfirmed_delivery_receipts(77, 11) == 1

    service.mark_delivery_receipt_checked.assert_called_once_with(
        501,
        1,
        4,
        SurveyDeliveryStatus.SENDING,
        5,
    )


@pytest.mark.asyncio
async def test_failed_dm_persists_only_sanitized_failure_category():
    claimed = _recipient(
        delivery_status=SurveyDeliveryStatus.SENDING,
        dm_message_id=None,
    )
    service = MagicMock()
    service.get_response_session.return_value = _session(recipient=claimed)
    forbidden = discord.Forbidden(
        SimpleNamespace(status=403, reason="Forbidden"),
        {"code": 50007, "message": "Cannot send messages to this user"},
    )
    user = SimpleNamespace(send=AsyncMock(side_effect=forbidden))
    bot = SimpleNamespace(
        get_user=MagicMock(return_value=user),
        fetch_user=AsyncMock(),
        add_view=MagicMock(),
    )
    cog = _cog(bot=bot, survey_service=service)

    await cog._deliver_recipient(claimed)

    service.mark_delivery_sent.assert_not_called()
    service.mark_delivery_failed.assert_called_once_with(501, 1, "Forbidden")


@pytest.mark.asyncio
async def test_interrupted_dm_keeps_claim_for_reconnect_recovery():
    claimed = _recipient(
        delivery_status=SurveyDeliveryStatus.SENDING,
        dm_message_id=None,
    )
    service = MagicMock()
    service.get_response_session.return_value = _session(recipient=claimed)
    user = SimpleNamespace(send=AsyncMock(side_effect=asyncio.CancelledError()))
    bot = SimpleNamespace(
        get_user=MagicMock(return_value=user),
        fetch_user=AsyncMock(),
        add_view=MagicMock(),
    )
    cog = _cog(bot=bot, survey_service=service)

    with pytest.raises(asyncio.CancelledError):
        await cog._deliver_recipient(claimed)

    service.mark_delivery_sent.assert_not_called()
    service.mark_delivery_failed.assert_not_called()


@pytest.mark.asyncio
async def test_delivery_dispatch_is_bounded_to_five_concurrent_dms():
    recipients = [
        _recipient(
            recipient_id=500 + index,
            discord_id=40 + index,
            delivery_status=SurveyDeliveryStatus.SENDING,
            dm_message_id=None,
        )
        for index in range(8)
    ]
    sessions = {
        recipient.discord_id: _session(recipient=recipient) for recipient in recipients
    }
    service = MagicMock()
    service.claim_deliveries.side_effect = [recipients, []]
    service.get_response_session.side_effect = lambda _survey_id, user_id: sessions[user_id]
    active = 0
    peak = 0
    # Deterministic overlap: sends block on an event instead of a wall-clock
    # sleep, so the semaphore's bound is observed regardless of machine load.
    release = asyncio.Event()
    bound_reached = asyncio.Event()

    async def send(**_kwargs):
        nonlocal active, peak
        active += 1
        peak = max(peak, active)
        if active == survey_commands._DELIVERY_CONCURRENCY:
            bound_reached.set()
        await release.wait()
        active -= 1
        return SimpleNamespace(id=7001, channel=SimpleNamespace(id=6001))

    users = {
        recipient.discord_id: SimpleNamespace(send=AsyncMock(side_effect=send))
        for recipient in recipients
    }
    bot = SimpleNamespace(
        get_user=MagicMock(side_effect=lambda user_id: users[user_id]),
        fetch_user=AsyncMock(),
        add_view=MagicMock(),
    )
    cog = _cog(bot=bot, survey_service=service)

    dispatch = asyncio.create_task(cog.dispatch_pending())
    # Wait on the condition itself (bounded only as a hang backstop): the
    # dispatcher needs real thread-pool hops before five sends are in flight,
    # so an iteration-count spin would race machine load.
    await asyncio.wait_for(bound_reached.wait(), timeout=10)
    assert active == survey_commands._DELIVERY_CONCURRENCY
    release.set()
    assert await dispatch == 8
    assert peak == survey_commands._DELIVERY_CONCURRENCY
    assert service.mark_delivery_sent.call_count == 8
    assert service.claim_deliveries.call_args_list == [
        call(survey_commands._DELIVERY_BATCH_SIZE, survey_commands._DELIVERY_STALE_SECONDS),
        call(survey_commands._DELIVERY_BATCH_SIZE, survey_commands._DELIVERY_STALE_SECONDS),
    ]


@pytest.mark.asyncio
async def test_retry_requeues_only_failed_deliveries_and_dispatches(monkeypatch):
    guild = SimpleNamespace(id=77, members=[])
    interaction = _interaction(user_id=900, guild=guild)
    service = MagicMock()
    service.retry_failed_deliveries.return_value = 2
    service.get_results.return_value = SimpleNamespace(
        delivered_count=5,
        recipient_count=6,
    )
    cog = _cog(survey_service=service)
    cog.reconcile_unconfirmed_delivery_receipts = AsyncMock()
    cog._dispatch_pending_locked = AsyncMock(return_value=2)
    followup = AsyncMock()
    monkeypatch.setattr(survey_commands, "has_admin_permission", lambda _i: True)
    monkeypatch.setattr(survey_commands, "safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr(survey_commands, "safe_followup", followup)

    await cog.retry.callback(cog, interaction, 11)

    service.retry_failed_deliveries.assert_called_once_with(77, 11)
    cog.reconcile_unconfirmed_delivery_receipts.assert_not_awaited()
    cog._dispatch_pending_locked.assert_awaited_once()
    assert "Retry requested for **2**" in followup.await_args.kwargs["content"]
    assert followup.await_args.kwargs["ephemeral"] is True


@pytest.mark.asyncio
async def test_retry_dispatches_and_reconciles_views_when_none_failed(
    monkeypatch,
):
    interaction = _interaction(user_id=900, guild=SimpleNamespace(id=77, members=[]))
    service = MagicMock()
    service.retry_failed_deliveries.return_value = 0
    service.get_results.return_value = SimpleNamespace(
        delivered_count=1,
        recipient_count=1,
    )
    cog = _cog(survey_service=service)
    cog.reconcile_unconfirmed_delivery_receipts = AsyncMock(return_value=1)
    cog._dispatch_pending_locked = AsyncMock(return_value=1)
    cog.reconcile_response_messages = AsyncMock(return_value=1)
    cog._schedule_if_unconfirmed_deliveries = AsyncMock()
    monkeypatch.setattr(survey_commands, "has_admin_permission", lambda _i: True)
    monkeypatch.setattr(survey_commands, "safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr(survey_commands, "safe_followup", AsyncMock())

    await cog.retry.callback(cog, interaction, 11)

    cog._dispatch_pending_locked.assert_awaited_once()
    cog.reconcile_response_messages.assert_awaited_once()
    cog._schedule_if_unconfirmed_deliveries.assert_awaited_once()


@pytest.mark.asyncio
async def test_retry_post_intent_dispatch_failure_arms_recovery(monkeypatch):
    interaction = _interaction(
        user_id=900,
        guild=SimpleNamespace(id=77, members=[]),
    )
    service = MagicMock()
    service.retry_failed_deliveries.return_value = 1
    cog = _cog(survey_service=service)
    cog._dispatch_pending_locked = AsyncMock(side_effect=RuntimeError("db unavailable"))
    cog._schedule_delivery_recovery = MagicMock()
    cog.respond_error = AsyncMock()
    monkeypatch.setattr(survey_commands, "has_admin_permission", lambda _i: True)
    monkeypatch.setattr(survey_commands, "safe_defer", AsyncMock(return_value=True))

    await cog.retry.callback(cog, interaction, 11)

    service.retry_failed_deliveries.assert_called_once_with(77, 11)
    cog._schedule_delivery_recovery.assert_called_once_with(
        survey_commands._DELIVERY_RECEIPT_GRACE_SECONDS
    )
    cog.respond_error.assert_awaited_once_with(
        interaction,
        "Couldn't retry that survey right now.",
    )


@pytest.mark.asyncio
async def test_response_edit_failure_schedules_persistent_recovery():
    session = _session()
    message = SimpleNamespace(edit=AsyncMock(side_effect=discord.HTTPException(
        SimpleNamespace(status=503, reason="Unavailable"),
        "try later",
    )))
    channel = SimpleNamespace(get_partial_message=MagicMock(return_value=message))
    user = SimpleNamespace(create_dm=AsyncMock(return_value=channel))
    service = MagicMock()
    service.get_response_session.return_value = session
    bot = SimpleNamespace(
        get_user=MagicMock(return_value=user),
        fetch_user=AsyncMock(),
        add_view=MagicMock(),
    )
    cog = _cog(bot=bot, survey_service=service)
    cog._schedule_delivery_recovery = MagicMock()

    await cog._reconcile_response_message(session)

    cog._schedule_delivery_recovery.assert_called_once_with(
        survey_commands._DELIVERY_RECEIPT_GRACE_SECONDS
    )


@pytest.mark.asyncio
async def test_on_ready_failure_schedules_another_recovery_pass():
    cog = _cog()
    cog._dispatch_pending_locked = AsyncMock(side_effect=RuntimeError("db unavailable"))
    cog._schedule_delivery_recovery = MagicMock()

    await cog.on_ready()

    cog._schedule_delivery_recovery.assert_any_call(
        survey_commands._DELIVERY_RECEIPT_GRACE_SECONDS
    )


@pytest.mark.asyncio
async def test_dispatch_failure_schedules_recovery_for_committed_campaign_state():
    service = MagicMock()
    service.list_unconfirmed_deliveries.side_effect = RuntimeError("db unavailable")
    cog = _cog(survey_service=service)
    cog._schedule_delivery_recovery = MagicMock()

    with pytest.raises(RuntimeError, match="db unavailable"):
        await cog.dispatch_pending()

    cog._schedule_delivery_recovery.assert_called_once_with(
        survey_commands._DELIVERY_RECEIPT_GRACE_SECONDS
    )


@pytest.mark.asyncio
async def test_send_post_open_dispatch_failure_arms_recovery_in_background(monkeypatch):
    interaction = _interaction(
        user_id=900,
        guild=SimpleNamespace(id=77, members=[]),
    )
    service = MagicMock()
    cog = _cog(survey_service=service)
    cog._resolve_recipients = AsyncMock(return_value=[42])
    cog._dispatch_pending_locked = AsyncMock(side_effect=RuntimeError("db unavailable"))
    cog._schedule_delivery_recovery = MagicMock()
    cog.respond_error = AsyncMock()
    followup = AsyncMock()
    monkeypatch.setattr(survey_commands, "has_admin_permission", lambda _i: True)
    monkeypatch.setattr(survey_commands, "safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr(survey_commands, "safe_followup", followup)

    target = discord.app_commands.Choice(
        name="One member",
        value=survey_commands.SurveyTargetType.MEMBER.value,
    )
    await cog.send.callback(cog, interaction, 11, target)
    await asyncio.gather(*cog._send_dispatch_tasks, return_exceptions=True)

    service.open_survey.assert_called_once()
    # The campaign is durably pending: the admin got the opened confirmation
    # and the background dispatch failure armed recovery instead of surfacing
    # a bogus command error.
    assert "opened for **1** recipients" in followup.await_args.kwargs["content"]
    cog._schedule_delivery_recovery.assert_called_once_with(
        survey_commands._DELIVERY_RECEIPT_GRACE_SECONDS
    )
    cog.respond_error.assert_not_awaited()


@pytest.mark.asyncio
async def test_send_responds_immediately_and_dispatches_in_background(monkeypatch):
    interaction = _interaction(
        user_id=900,
        guild=SimpleNamespace(id=77, members=[]),
    )
    service = MagicMock()
    service.list_unconfirmed_deliveries.return_value = []
    cog = _cog(survey_service=service)
    cog._resolve_recipients = AsyncMock(return_value=[42, 43])
    started = asyncio.Event()
    release = asyncio.Event()

    async def slow_dispatch(*_args, **_kwargs):
        started.set()
        await release.wait()
        return 2

    cog._dispatch_pending_locked = AsyncMock(side_effect=slow_dispatch)
    followup = AsyncMock()
    monkeypatch.setattr(survey_commands, "has_admin_permission", lambda _i: True)
    monkeypatch.setattr(survey_commands, "safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr(survey_commands, "safe_followup", followup)

    target = discord.app_commands.Choice(
        name="A role",
        value=survey_commands.SurveyTargetType.ROLE.value,
    )
    await cog.send.callback(cog, interaction, 11, target)

    # The admin already has the confirmation while delivery is still running.
    assert "opened for **2** recipients" in followup.await_args.kwargs["content"]
    service.open_survey.assert_called_once()
    await asyncio.wait_for(started.wait(), timeout=5)
    assert not release.is_set()
    release.set()
    await asyncio.gather(*cog._send_dispatch_tasks)
    # The background dispatch scopes receipt reconciliation to this campaign.
    cog._dispatch_pending_locked.assert_awaited_once_with(77, 11)


@pytest.mark.asyncio
async def test_dispatch_scopes_receipt_reconciliation_to_the_given_survey():
    service = MagicMock()
    service.claim_deliveries.return_value = []
    cog = _cog(survey_service=service)
    cog.reconcile_unconfirmed_delivery_receipts = AsyncMock(return_value=0)

    await cog._dispatch_pending_locked(77, 11)
    cog.reconcile_unconfirmed_delivery_receipts.assert_awaited_once_with(77, 11)

    cog.reconcile_unconfirmed_delivery_receipts.reset_mock()
    await cog._dispatch_pending_locked()
    cog.reconcile_unconfirmed_delivery_receipts.assert_awaited_once_with(None, None)


@pytest.mark.asyncio
async def test_respond_error_swallows_expired_interaction_token():
    cog = _cog()
    interaction = _interaction(user_id=42)
    interaction.response.is_done = MagicMock(return_value=True)
    interaction.followup.send = AsyncMock(
        side_effect=discord.NotFound(
            SimpleNamespace(status=404, reason="Not Found"),
            {"message": "Unknown Webhook"},
        )
    )

    await cog.respond_error(interaction, "boom")

    interaction.followup.send.assert_awaited_once()


@pytest.mark.asyncio
async def test_cog_unload_cancels_owned_timer_and_worker():
    cog = _cog()
    cog._schedule_delivery_recovery(60)
    handle = cog._delivery_recovery_handle
    worker = asyncio.create_task(asyncio.Event().wait())
    cog._delivery_recovery_task = worker

    await cog.cog_unload()
    await asyncio.sleep(0)

    assert cog._unloaded is True
    assert handle is not None and handle.cancelled()
    assert worker.cancelled()
    assert cog._delivery_recovery_handle is None
    assert cog._delivery_recovery_task is None
    cog._schedule_delivery_recovery(1)
    assert cog._delivery_recovery_handle is None


@pytest.mark.asyncio
async def test_close_serializes_delivery_and_removes_persisted_dm_controls(monkeypatch):
    guild = SimpleNamespace(id=77, members=[])
    interaction = _interaction(user_id=900, guild=guild)
    closed_survey = _survey(status=SurveyStatus.CLOSED)
    closed_session = SurveySession(
        survey=closed_survey,
        recipient=_recipient(current_question_id=101, dm_message_id=7001),
        questions=(_question(101, 1, SurveyQuestionType.NPS),),
        answers=(),
    )
    message = SimpleNamespace(edit=AsyncMock())
    channel = SimpleNamespace(get_partial_message=MagicMock(return_value=message))
    user = SimpleNamespace(create_dm=AsyncMock(return_value=channel))
    service = MagicMock()
    service.close_survey.return_value = closed_survey
    service.list_recoverable_response_sessions.return_value = [closed_session]
    service.get_response_session.return_value = closed_session
    bot = SimpleNamespace(
        get_user=MagicMock(return_value=user),
        fetch_user=AsyncMock(),
        add_view=MagicMock(),
    )
    cog = _cog(bot=bot, survey_service=service)
    defer = AsyncMock(return_value=True)
    followup = AsyncMock()
    monkeypatch.setattr(survey_commands, "has_admin_permission", lambda _i: True)
    monkeypatch.setattr(survey_commands, "safe_defer", defer)
    monkeypatch.setattr(survey_commands, "safe_followup", followup)

    await cog.close.callback(cog, interaction, 11)

    defer.assert_awaited_once_with(interaction, ephemeral=True, thinking=True)
    service.close_survey.assert_called_once_with(77, 11)
    edit_kwargs = message.edit.await_args.kwargs
    assert edit_kwargs["embed"].title == "Survey closed"
    assert edit_kwargs["view"] is None
    service.finalize_response_ui.assert_called_once_with(11, 42, 7001)
    assert followup.await_args.kwargs["ephemeral"] is True


@pytest.mark.asyncio
async def test_close_post_commit_reconciliation_failure_arms_recovery(monkeypatch):
    interaction = _interaction(
        user_id=900,
        guild=SimpleNamespace(id=77, members=[]),
    )
    service = MagicMock()
    service.close_survey.return_value = _survey(status=SurveyStatus.CLOSED)
    cog = _cog(survey_service=service)
    cog.reconcile_unconfirmed_delivery_receipts = AsyncMock(
        side_effect=[0, RuntimeError("db unavailable")]
    )
    cog._schedule_delivery_recovery = MagicMock()
    cog.respond_error = AsyncMock()
    monkeypatch.setattr(survey_commands, "has_admin_permission", lambda _i: True)
    monkeypatch.setattr(survey_commands, "safe_defer", AsyncMock(return_value=True))

    await cog.close.callback(cog, interaction, 11)

    service.close_survey.assert_called_once_with(77, 11)
    cog._schedule_delivery_recovery.assert_called_once_with(
        survey_commands._DELIVERY_RECEIPT_GRACE_SECONDS
    )
    cog.respond_error.assert_awaited_once_with(
        interaction,
        "Couldn't close that survey right now.",
    )


@pytest.mark.asyncio
async def test_results_defers_before_loading_anonymous_report(monkeypatch):
    interaction = _interaction(guild=SimpleNamespace(id=77))
    report = SurveyResults(
        survey_id=11,
        title="Feedback",
        status=SurveyStatus.OPEN,
        recipient_count=0,
        delivered_count=0,
        started_count=0,
        completed_count=0,
        delivery_rate=0.0,
        started_rate=0.0,
        completion_rate=0.0,
        response_rate=0.0,
        questions=(),
    )
    service = MagicMock()
    service.get_results.return_value = report
    cog = _cog(survey_service=service)
    defer = AsyncMock(return_value=True)
    followup = AsyncMock()
    monkeypatch.setattr(survey_commands, "has_admin_permission", lambda _i: True)
    monkeypatch.setattr(survey_commands, "safe_defer", defer)
    monkeypatch.setattr(survey_commands, "safe_followup", followup)

    await cog.results.callback(cog, interaction, 11)

    defer.assert_awaited_once_with(interaction, ephemeral=True, thinking=True)
    service.get_results.assert_called_once_with(77, 11)
    assert followup.await_args.kwargs["ephemeral"] is True
    assert followup.await_args.kwargs["embed"].title == "Survey results — Feedback"


@pytest.mark.asyncio
async def test_question_views_match_question_type_and_optional_skip_behavior():
    cog = _cog()
    nps = _question(101, 1, SurveyQuestionType.NPS)
    rating = _question(102, 2, SurveyQuestionType.RATING, required=False)
    text = _question(103, 3, SurveyQuestionType.TEXT, required=False)
    questions = (nps, rating, text)

    nps_view = survey_commands.SurveyQuestionView(
        cog,
        _session(questions=questions),
        nps,
    )
    rating_view = survey_commands.SurveyQuestionView(
        cog,
        _session(
            recipient=_recipient(current_question_id=102),
            questions=questions,
        ),
        rating,
    )
    text_view = survey_commands.SurveyQuestionView(
        cog,
        _session(
            recipient=_recipient(current_question_id=103),
            questions=questions,
        ),
        text,
    )

    assert [option.value for option in nps_view.children[0].options] == [
        str(value) for value in range(11)
    ]
    assert not any(isinstance(item, survey_commands.SurveySkipButton) for item in nps_view.children)
    assert [option.value for option in rating_view.children[0].options] == [
        str(value) for value in range(1, 6)
    ]
    assert any(isinstance(item, survey_commands.SurveySkipButton) for item in rating_view.children)
    assert isinstance(text_view.children[0], survey_commands.SurveyTextButton)
    assert any(isinstance(item, survey_commands.SurveySkipButton) for item in text_view.children)
    assert all(view.timeout is None for view in (nps_view, rating_view, text_view))


@pytest.mark.asyncio
async def test_review_question_picker_escapes_stored_prompts():
    question = _question(
        101,
        1,
        SurveyQuestionType.TEXT,
        prompt="Tell @everyone *why*",
    )
    session = _session(
        recipient=_recipient(current_question_id=None, in_review=True),
        questions=(question,),
    )

    view = survey_commands.SurveyReviewView(_cog(), session)
    description = view.children[0].options[0].description

    assert description is not None
    assert "@everyone" not in description
    assert "@\u200beveryone" in description
    assert "\\*why\\*" in description


@pytest.mark.asyncio
@pytest.mark.parametrize("view_type", [survey_commands.SurveyQuestionView, survey_commands.SurveyReviewView])
async def test_response_views_reject_a_different_discord_user(view_type):
    cog = _cog()
    cog.respond_error = AsyncMock()
    session = _session(
        recipient=_recipient(
            current_question_id=None if view_type is survey_commands.SurveyReviewView else 101,
            in_review=view_type is survey_commands.SurveyReviewView,
        )
    )
    view = (
        view_type(cog, session)
        if view_type is survey_commands.SurveyReviewView
        else view_type(cog, session, session.questions[0])
    )
    interaction = _interaction(user_id=999)

    assert await view.interaction_check(interaction) is False
    cog.respond_error.assert_awaited_once_with(
        interaction,
        "This survey response isn't yours.",
    )


@pytest.mark.asyncio
async def test_answer_progresses_to_next_question_then_final_review():
    questions = (
        _question(101, 1, SurveyQuestionType.NPS),
        _question(102, 2, SurveyQuestionType.TEXT, required=False),
    )
    next_session = _session(
        recipient=_recipient(current_question_id=102),
        questions=questions,
    )
    review_session = _session(
        recipient=_recipient(current_question_id=None, in_review=True),
        questions=questions,
        answers=(
            SurveyDraftAnswer(101, 9, None, False, 10),
            SurveyDraftAnswer(102, None, None, True, 11),
        ),
    )
    service = MagicMock()
    service.answer_question.side_effect = [next_session, review_session]
    cog = _cog(survey_service=service)
    first_interaction = _interaction(user_id=42)
    second_interaction = _interaction(user_id=42)

    await cog.answer_question(first_interaction, 11, 101, 9)
    await cog.answer_question(second_interaction, 11, 102, "fine")

    first_view = first_interaction.edit_original_response.await_args.kwargs["view"]
    second_view = second_interaction.edit_original_response.await_args.kwargs["view"]
    assert isinstance(first_view, survey_commands.SurveyQuestionView)
    assert first_view.question.question_id == 102
    assert isinstance(second_view, survey_commands.SurveyReviewView)
    assert service.answer_question.call_args_list == [
        call(11, 42, 101, 9),
        call(11, 42, 102, "fine"),
    ]


@pytest.mark.asyncio
async def test_racing_answer_acknowledges_before_waiting_for_response_lock():
    next_session = _session(recipient=_recipient(current_question_id=102))
    service = MagicMock()
    service.answer_question.return_value = next_session
    cog = _cog(survey_service=service)
    interaction = _interaction(user_id=42)
    lock = cog._response_lock(11, 42)
    await lock.acquire()

    task = asyncio.create_task(cog.answer_question(interaction, 11, 101, 9))
    await asyncio.sleep(0)

    interaction.response.defer.assert_awaited_once_with(
        ephemeral=False,
        thinking=False,
    )
    service.answer_question.assert_not_called()
    lock.release()
    await task

    service.answer_question.assert_called_once_with(11, 42, 101, 9)
    interaction.edit_original_response.assert_awaited_once()


def test_member_targeted_question_dm_does_not_promise_anonymity():
    questions = (_question(101, 1, SurveyQuestionType.NPS),)
    member_session = SurveySession(
        survey=replace(_survey(), target_type=survey_commands.SurveyTargetType.MEMBER),
        recipient=_recipient(),
        questions=questions,
        answers=(),
    )
    role_session = SurveySession(
        survey=replace(_survey(), target_type=survey_commands.SurveyTargetType.ROLE),
        recipient=_recipient(),
        questions=questions,
        answers=(),
    )

    member_privacy = next(
        field.value
        for field in survey_commands._question_embed(member_session, questions[0]).fields
        if field.name == "Privacy"
    )
    role_privacy = next(
        field.value
        for field in survey_commands._question_embed(role_session, questions[0]).fields
        if field.name == "Privacy"
    )

    # A single-recipient survey must tell the respondent the truth: the
    # admins can connect the answers to them.
    assert "shared with the survey admins" in member_privacy
    assert "not anonymous" in member_privacy
    assert "anonymous, per-question results" not in member_privacy
    assert "anonymous, per-question results" in role_privacy

    member_submitted = survey_commands._submitted_embed(
        SurveySession(
            survey=member_session.survey,
            recipient=_recipient(submitted_at=100, current_question_id=None),
            questions=questions,
            answers=(),
        )
    )
    role_submitted = survey_commands._submitted_embed(
        SurveySession(
            survey=role_session.survey,
            recipient=_recipient(submitted_at=100, current_question_id=None),
            questions=questions,
            answers=(),
        )
    )
    assert "anonymously" not in member_submitted.description
    assert "anonymously" in role_submitted.description


@pytest.mark.asyncio
async def test_saved_answer_is_not_reported_as_failed_when_dm_edit_fails():
    next_session = _session(recipient=_recipient(current_question_id=102))
    service = MagicMock()
    service.answer_question.return_value = next_session
    cog = _cog(survey_service=service)
    cog._schedule_delivery_recovery = MagicMock()
    cog.respond_error = AsyncMock()
    interaction = _interaction(user_id=42)
    interaction.edit_original_response.side_effect = discord.HTTPException(
        SimpleNamespace(status=503, reason="Unavailable"),
        "try later",
    )

    await cog.answer_question(interaction, 11, 101, 9)

    # The answer is durably saved; a Discord re-render failure must not tell
    # the respondent their answer wasn't saved.
    service.answer_question.assert_called_once_with(11, 42, 101, 9)
    cog.respond_error.assert_not_awaited()
    cog._schedule_delivery_recovery.assert_called_once_with(
        survey_commands._DELIVERY_RECEIPT_GRACE_SECONDS
    )


@pytest.mark.asyncio
async def test_saved_skip_is_not_reported_as_failed_when_dm_edit_fails():
    next_session = _session(recipient=_recipient(current_question_id=102))
    service = MagicMock()
    service.skip_question.return_value = next_session
    cog = _cog(survey_service=service)
    cog._schedule_delivery_recovery = MagicMock()
    cog.respond_error = AsyncMock()
    interaction = _interaction(user_id=42)
    interaction.edit_original_response.side_effect = discord.HTTPException(
        SimpleNamespace(status=503, reason="Unavailable"),
        "try later",
    )

    await cog.skip_question(interaction, 11, 102)

    service.skip_question.assert_called_once_with(11, 42, 102)
    cog.respond_error.assert_not_awaited()


@pytest.mark.asyncio
async def test_submitted_response_is_not_reported_as_failed_when_dm_edit_fails():
    submitted_session = _session(
        recipient=_recipient(
            current_question_id=None,
            in_review=True,
            submitted_at=100,
        )
    )
    service = MagicMock()
    service.submit_response.return_value = True
    service.get_response_session.return_value = submitted_session
    cog = _cog(survey_service=service)
    cog._schedule_delivery_recovery = MagicMock()
    cog.respond_error = AsyncMock()
    interaction = _interaction(user_id=42)
    interaction.edit_original_response.side_effect = discord.HTTPException(
        SimpleNamespace(status=503, reason="Unavailable"),
        "try later",
    )

    await cog.submit_response(interaction, 11)

    # The submission is durably committed and counted in results; a Discord
    # re-render failure must not tell the respondent the submit failed.
    service.submit_response.assert_called_once_with(11, 42)
    cog.respond_error.assert_not_awaited()
    cog._schedule_delivery_recovery.assert_called_once_with(
        survey_commands._DELIVERY_RECEIPT_GRACE_SECONDS
    )


@pytest.mark.asyncio
async def test_review_navigation_is_not_reported_as_failed_when_dm_edit_fails():
    review_session = _session(
        recipient=_recipient(current_question_id=None, in_review=True),
    )
    service = MagicMock()
    service.select_response_question.return_value = _session()
    service.begin_review.return_value = review_session
    cog = _cog(survey_service=service)
    cog._schedule_delivery_recovery = MagicMock()
    cog.respond_error = AsyncMock()
    interaction = _interaction(user_id=42)
    interaction.edit_original_response.side_effect = discord.HTTPException(
        SimpleNamespace(status=503, reason="Unavailable"),
        "try later",
    )

    await cog.edit_response_question(interaction, 11, 101)
    await cog.return_to_review(interaction, 11)

    service.select_response_question.assert_called_once_with(11, 42, 101)
    service.begin_review.assert_called_once_with(11, 42)
    cog.respond_error.assert_not_awaited()


@pytest.mark.asyncio
async def test_committed_answer_schedules_recovery_when_discord_edit_fails():
    next_session = _session(recipient=_recipient(current_question_id=102))
    service = MagicMock()
    service.answer_question.return_value = next_session
    cog = _cog(survey_service=service)
    cog._schedule_delivery_recovery = MagicMock()
    interaction = _interaction(user_id=42)
    interaction.edit_original_response.side_effect = discord.HTTPException(
        SimpleNamespace(status=503, reason="Unavailable"),
        "try later",
    )

    await cog.answer_question(interaction, 11, 101, 9)

    service.answer_question.assert_called_once_with(11, 42, 101, 9)
    cog._schedule_delivery_recovery.assert_called_once_with(
        survey_commands._DELIVERY_RECEIPT_GRACE_SECONDS
    )


@pytest.mark.asyncio
async def test_submit_is_atomic_and_removes_all_response_controls():
    submitted_session = _session(
        recipient=_recipient(
            current_question_id=None,
            in_review=True,
            submitted_at=100,
        )
    )
    service = MagicMock()
    service.submit_response.return_value = True
    service.get_response_session.return_value = submitted_session
    cog = _cog(survey_service=service)
    interaction = _interaction(user_id=42)

    await cog.submit_response(interaction, 11)

    service.submit_response.assert_called_once_with(11, 42)
    service.get_response_session.assert_called_once_with(11, 42)
    kwargs = interaction.edit_original_response.await_args.kwargs
    assert kwargs["view"] is None
    assert kwargs["embed"].title == "Survey submitted"


def test_results_pages_show_aggregates_and_unattributed_comments_without_identity_data():
    results = SurveyResults(
        survey_id=11,
        title="Feedback @everyone",
        status=SurveyStatus.CLOSED,
        recipient_count=6,
        delivered_count=5,
        started_count=4,
        completed_count=3,
        delivery_rate=5 / 6,
        started_rate=4 / 5,
        completion_rate=3 / 4,
        response_rate=3 / 5,
        questions=(
            SurveyQuestionResult(
                question_id=101,
                position=1,
                prompt="Would you recommend Cama?",
                question_type=SurveyQuestionType.NPS,
                response_count=3,
                skipped_count=0,
                nps_score=33,
                promoters=2,
                passives=0,
                detractors=1,
                rating_average=None,
                rating_distribution=(0, 0, 0, 0, 0),
                text_responses=(),
            ),
            SurveyQuestionResult(
                question_id=102,
                position=2,
                prompt="Rate match quality",
                question_type=SurveyQuestionType.RATING,
                response_count=3,
                skipped_count=0,
                nps_score=None,
                promoters=0,
                passives=0,
                detractors=0,
                rating_average=4.0,
                rating_distribution=(0, 0, 1, 1, 1),
                text_responses=(),
            ),
            SurveyQuestionResult(
                question_id=103,
                position=3,
                prompt="What should change?",
                question_type=SurveyQuestionType.TEXT,
                response_count=2,
                skipped_count=1,
                nps_score=None,
                promoters=0,
                passives=0,
                detractors=0,
                rating_average=None,
                rating_distribution=(0, 0, 0, 0, 0),
                text_responses=("More captains", "No @everyone pings"),
            ),
        ),
    )

    pages = survey_commands._results_pages(results)
    rendered = "\n".join(str(page.to_dict()) for page in pages)

    assert len(pages) == 4
    assert "33" in rendered
    assert "Promoters" in rendered
    assert "4.00" in rendered
    assert "1: 0" in rendered and "5: 1" in rendered
    assert "More captains" in rendered
    assert "Anonymous response" in rendered
    assert r"@\u200beveryone" in rendered
    assert "discord_id" not in rendered
    assert "<@" not in rendered
    assert "deanonym" not in rendered.lower()
    assert "small cohort" not in rendered.lower()


@pytest.mark.asyncio
async def test_results_pagination_is_locked_to_requesting_admin():
    pages = [SimpleNamespace(), SimpleNamespace()]
    view = survey_commands.SurveyResultsView(owner_id=900, pages=pages)
    other = _interaction(user_id=901)
    owner = _interaction(user_id=900)

    assert await view.interaction_check(other) is False
    assert other.response.send_message.await_args.kwargs["ephemeral"] is True
    assert await view.interaction_check(owner) is True


@pytest.mark.asyncio
async def test_results_view_timeout_disables_pagination_buttons():
    pages = [SimpleNamespace(), SimpleNamespace()]
    interaction = _interaction(user_id=900)
    view = survey_commands.SurveyResultsView(
        owner_id=900,
        pages=pages,
        interaction=interaction,
    )

    await view.on_timeout()

    assert all(child.disabled for child in view.children)
    interaction.edit_original_response.assert_awaited_once()
    assert interaction.edit_original_response.await_args.kwargs["view"] is view

    # A vanished ephemeral report must not raise out of the timeout handler.
    interaction.edit_original_response.side_effect = discord.NotFound(
        SimpleNamespace(status=404, reason="Not Found"),
        {"message": "Unknown Message"},
    )
    await view.on_timeout()


@pytest.mark.asyncio
async def test_results_view_timeout_edits_via_the_freshest_pagination_token():
    """Clicks push the view timeout past the original token's 15m lifetime,
    so on_timeout must edit with the latest component interaction instead."""
    pages = [SimpleNamespace(), SimpleNamespace()]
    original = _interaction(user_id=900)
    view = survey_commands.SurveyResultsView(
        owner_id=900,
        pages=pages,
        interaction=original,
    )
    later_click = _interaction(user_id=900)

    await view.next.callback(later_click)
    await view.on_timeout()

    later_click.edit_original_response.assert_awaited_once()
    assert later_click.edit_original_response.await_args.kwargs["view"] is view
    original.edit_original_response.assert_not_awaited()


@pytest.mark.asyncio
async def test_results_view_failed_click_retires_via_the_previous_token():
    """A click whose edit fails was never acknowledged, yet it still pushed
    the view timeout past the kept token's 15m lifetime — so the view must
    retire immediately through the token that still works."""
    pages = [SimpleNamespace(), SimpleNamespace()]
    original = _interaction(user_id=900)
    view = survey_commands.SurveyResultsView(
        owner_id=900,
        pages=pages,
        interaction=original,
    )
    failing_click = _interaction(user_id=900)
    failing_click.response.edit_message.side_effect = discord.NotFound(
        SimpleNamespace(status=404, reason="Not Found"),
        {"message": "Unknown interaction"},
    )

    with pytest.raises(discord.NotFound):
        await view.next.callback(failing_click)

    original.edit_original_response.assert_awaited_once()
    failing_click.edit_original_response.assert_not_awaited()
    assert view.is_finished()
    assert all(child.disabled for child in view.children)


@pytest.mark.asyncio
async def test_results_command_wires_timeout_interaction_into_pagination(monkeypatch):
    interaction = _interaction(user_id=900, guild=SimpleNamespace(id=77))
    report = SurveyResults(
        survey_id=11,
        title="Feedback",
        status=SurveyStatus.OPEN,
        recipient_count=3,
        delivered_count=3,
        started_count=1,
        completed_count=1,
        delivery_rate=1.0,
        started_rate=1 / 3,
        completion_rate=1.0,
        response_rate=1 / 3,
        questions=(
            SurveyQuestionResult(
                question_id=101,
                position=1,
                prompt="Recommend?",
                question_type=SurveyQuestionType.NPS,
                response_count=1,
                skipped_count=0,
                nps_score=100,
                promoters=1,
                passives=0,
                detractors=0,
                rating_average=None,
                rating_distribution=(0, 0, 0, 0, 0),
                text_responses=(),
            ),
        ),
    )
    service = MagicMock()
    service.get_results.return_value = report
    cog = _cog(survey_service=service)
    followup = AsyncMock()
    monkeypatch.setattr(survey_commands, "has_admin_permission", lambda _i: True)
    monkeypatch.setattr(survey_commands, "safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr(survey_commands, "safe_followup", followup)

    await cog.results.callback(cog, interaction, 11)

    view = followup.await_args.kwargs["view"]
    assert isinstance(view, survey_commands.SurveyResultsView)
    assert view._interaction is interaction


@pytest.mark.asyncio
async def test_reconnect_reconciliation_skips_unchanged_response_messages():
    session = _session()
    message = SimpleNamespace(edit=AsyncMock())
    channel = SimpleNamespace(get_partial_message=MagicMock(return_value=message))
    user = SimpleNamespace(create_dm=AsyncMock(return_value=channel))
    service = MagicMock()
    service.get_response_session.return_value = session
    bot = SimpleNamespace(
        get_user=MagicMock(return_value=user),
        fetch_user=AsyncMock(),
        add_view=MagicMock(),
    )
    cog = _cog(bot=bot, survey_service=service)

    await cog._reconcile_response_message(session)
    await cog._reconcile_response_message(session)

    # The second pass re-rendered nothing: same message, same committed state.
    assert message.edit.await_count == 1
    assert user.create_dm.await_count == 1

    changed = _session(
        recipient=replace(session.recipient, current_question_id=102, updated_at=6),
    )
    service.get_response_session.return_value = changed
    await cog._reconcile_response_message(session)
    assert message.edit.await_count == 2

    # A survey status flip must re-render even with identical recipient state.
    closed = SurveySession(
        survey=_survey(status=SurveyStatus.CLOSED),
        recipient=changed.recipient,
        questions=changed.questions,
        answers=(),
    )
    service.get_response_session.return_value = closed
    await cog._reconcile_response_message(session)
    assert message.edit.await_count == 3


@pytest.mark.asyncio
async def test_reconciliation_cache_holds_no_answer_text_and_evicts_on_finalize():
    secret = "my private free-text feedback"
    session = _session(
        answers=(SurveyDraftAnswer(102, None, secret, False, 5),),
    )
    message = SimpleNamespace(edit=AsyncMock())
    channel = SimpleNamespace(get_partial_message=MagicMock(return_value=message))
    user = SimpleNamespace(create_dm=AsyncMock(return_value=channel))
    service = MagicMock()
    service.get_response_session.return_value = session
    bot = SimpleNamespace(
        get_user=MagicMock(return_value=user),
        fetch_user=AsyncMock(),
        add_view=MagicMock(),
    )
    cog = _cog(bot=bot, survey_service=service)

    await cog._reconcile_response_message(session)

    # The skip cache must retain a digest, never respondents' raw answers.
    assert len(cog._reconciled_response_fingerprints) == 1
    assert secret not in repr(cog._reconciled_response_fingerprints)

    submitted = _session(
        recipient=replace(session.recipient, submitted_at=100, updated_at=6),
        answers=session.answers,
    )
    service.get_response_session.return_value = submitted
    await cog._reconcile_response_message(session)

    # Finalizing removes the session from the recoverable set for good, so
    # its fingerprint entry must not outlive it.
    service.finalize_response_ui.assert_called_once()
    assert cog._reconciled_response_fingerprints == {}


@pytest.mark.asyncio
async def test_setup_restores_question_and_review_views_by_persisted_message_id():
    question_session = _session(
        recipient=_recipient(dm_message_id=7001, ui_message_id=None),
    )
    review_session = _session(
        recipient=_recipient(
            recipient_id=502,
            discord_id=43,
            current_question_id=None,
            in_review=True,
            dm_message_id=7002,
            ui_message_id=7999,
        )
    )
    service = MagicMock()
    service.list_recoverable_response_sessions.return_value = [
        question_session,
        review_session,
    ]
    bot = SimpleNamespace(
        survey_service=service,
        player_service=MagicMock(),
        add_cog=AsyncMock(),
        add_view=MagicMock(),
    )

    await survey_commands.setup(bot)

    bot.add_cog.assert_awaited_once()
    assert bot.add_view.call_count == 2
    first_view = bot.add_view.call_args_list[0].args[0]
    second_view = bot.add_view.call_args_list[1].args[0]
    assert isinstance(first_view, survey_commands.SurveyQuestionView)
    assert isinstance(second_view, survey_commands.SurveyReviewView)
    assert bot.add_view.call_args_list[0].kwargs == {"message_id": 7001}
    assert bot.add_view.call_args_list[1].kwargs == {"message_id": 7999}
    assert first_view.is_persistent()
    assert second_view.is_persistent()


@pytest.mark.asyncio
async def test_setup_schedules_delivery_recovery_for_interrupted_campaigns(
    monkeypatch,
):
    """cog_unload cancels in-flight dispatch tasks, so a reloaded cog must
    resume pending deliveries instead of waiting for the next on_ready."""
    schedule = MagicMock()
    monkeypatch.setattr(
        survey_commands.SurveyCommands,
        "_schedule_delivery_recovery",
        schedule,
    )
    service = MagicMock()
    service.list_recoverable_response_sessions.return_value = []
    bot = SimpleNamespace(
        survey_service=service,
        player_service=MagicMock(),
        add_cog=AsyncMock(),
        add_view=MagicMock(),
    )

    await survey_commands.setup(bot)

    schedule.assert_called_once_with(survey_commands._DELIVERY_RECEIPT_GRACE_SECONDS)


@pytest.mark.asyncio
async def test_ready_recovery_reconciles_stale_discord_components_to_committed_question():
    """A crash after saving Q1 must replace Q1's old controls with persisted Q2."""
    questions = (
        _question(101, 1, SurveyQuestionType.NPS),
        _question(102, 2, SurveyQuestionType.TEXT, required=False),
    )
    committed_session = _session(
        recipient=_recipient(current_question_id=102, dm_message_id=7001),
        questions=questions,
        answers=(SurveyDraftAnswer(101, 10, None, False, 10),),
    )
    stale_listed_session = _session(
        recipient=_recipient(current_question_id=101, dm_message_id=7001),
        questions=questions,
    )
    message = SimpleNamespace(edit=AsyncMock())
    channel = SimpleNamespace(
        get_partial_message=MagicMock(return_value=message),
    )
    user = SimpleNamespace(create_dm=AsyncMock(return_value=channel))
    service = MagicMock()
    service.list_recoverable_response_sessions.return_value = [stale_listed_session]
    service.get_response_session.return_value = committed_session
    service.claim_deliveries.return_value = []
    bot = SimpleNamespace(
        get_user=MagicMock(return_value=user),
        fetch_user=AsyncMock(),
        add_view=MagicMock(),
    )
    cog = _cog(bot=bot, survey_service=service)

    await cog.on_ready()

    user.create_dm.assert_awaited_once()
    channel.get_partial_message.assert_called_once_with(7001)
    edit_kwargs = message.edit.await_args.kwargs
    assert isinstance(edit_kwargs["view"], survey_commands.SurveyQuestionView)
    assert edit_kwargs["view"].question.question_id == 102
    assert edit_kwargs["embed"].footer.text.startswith("Question 2/2")
    service.recover_stale_deliveries.assert_called_once_with(
        survey_commands._DELIVERY_STALE_SECONDS
    )
    service.claim_deliveries.assert_called_once_with(
        survey_commands._DELIVERY_BATCH_SIZE,
        survey_commands._DELIVERY_STALE_SECONDS,
    )

    # A later reconnect runs recovery again after the first pass has completed.
    await cog.on_ready()
    assert service.recover_stale_deliveries.call_count == 2
    assert service.claim_deliveries.call_count == 2


def test_maximum_size_review_and_preview_embeds_stay_within_discord_limits():
    survey = replace(_survey(status=SurveyStatus.DRAFT), title="*" * 100)
    questions = tuple(
        _question(
            100 + position,
            position,
            SurveyQuestionType.TEXT,
            prompt="*" * 1_000,
        )
        for position in range(1, 11)
    )
    answers = tuple(
        SurveyDraftAnswer(
            question_id=question.question_id,
            numeric_value=None,
            text_value="*" * 1_000,
            skipped=False,
            updated_at=10,
        )
        for question in questions
    )
    review_session = SurveySession(
        survey=replace(survey, status=SurveyStatus.OPEN, opened_at=2),
        recipient=_recipient(current_question_id=None, in_review=True),
        questions=questions,
        answers=answers,
    )

    review = survey_commands._review_embed(review_session)
    preview_pages = survey_commands._preview_pages(survey, questions)

    assert len(review.fields) == 10
    assert validate_embed(review) == []
    rendered_prompts = "".join(
        field.value for page in preview_pages for field in page.fields
    )
    assert rendered_prompts == survey_commands.escape_discord_text("*" * 1_000) * 10
    assert all(validate_embed(page) == [] for page in preview_pages)
