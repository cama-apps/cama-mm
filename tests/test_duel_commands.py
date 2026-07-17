from __future__ import annotations

from dataclasses import replace
from types import SimpleNamespace
from unittest.mock import ANY, AsyncMock, MagicMock

import discord
import pytest

from commands.duel import (
    DuelChallengeView,
    DuelCommands,
    DuelResponseButton,
    setup,
)
from domain.models.duel import (
    DuelChallenge,
    DuelDueKind,
    DuelDueResult,
    DuelStatus,
    DuelTrial,
)
from services.duel_flavor_service import DuelFlavorEvent

GUILD_ID = 100
CHANNEL_ID = 200
CHALLENGER_ID = 1
RECIPIENT_ID = 2


def make_challenge(**overrides) -> DuelChallenge:
    values = {
        "challenge_id": 7,
        "guild_id": GUILD_ID,
        "channel_id": CHANNEL_ID,
        "message_id": 300,
        "challenger_id": CHALLENGER_ID,
        "recipient_id": RECIPIENT_ID,
        "wager": 500,
        "issuance_fee": 50,
        "status": DuelStatus.PENDING,
        "trial_type": None,
        "challenger_glicko": 1400.0,
        "challenger_rd": 75.0,
        "recipient_glicko": 1550.0,
        "recipient_rd": 65.0,
        "created_at": 1_700_000_000,
        "expires_at": 1_700_604_800,
        "next_reminder_at": 1_700_432_000,
        "responded_at": None,
        "resolved_at": None,
        "winner_id": None,
        "resolution_actor_id": None,
    }
    values.update(overrides)
    return DuelChallenge(**values)


@pytest.fixture
def challenger():
    return SimpleNamespace(id=CHALLENGER_ID, mention=f"<@{CHALLENGER_ID}>", bot=False)


@pytest.fixture
def recipient():
    return SimpleNamespace(id=RECIPIENT_ID, mention=f"<@{RECIPIENT_ID}>", bot=False)


@pytest.fixture
def message():
    message = SimpleNamespace(id=300)
    message.edit = AsyncMock()
    return message


@pytest.fixture
def interaction(message, recipient):
    response = SimpleNamespace(
        defer=AsyncMock(),
        send_message=AsyncMock(),
        edit_message=AsyncMock(),
    )
    followup = SimpleNamespace(send=AsyncMock(return_value=message))
    channel = SimpleNamespace(
        id=CHANNEL_ID,
        fetch_message=AsyncMock(return_value=message),
        send=AsyncMock(return_value=message),
    )
    return SimpleNamespace(
        guild=SimpleNamespace(id=GUILD_ID),
        channel=channel,
        channel_id=CHANNEL_ID,
        user=recipient,
        response=response,
        followup=followup,
        client=None,
    )


@pytest.fixture
def duel_service():
    service = MagicMock()
    service.issue.return_value = make_challenge()
    service.respond.return_value = make_challenge(
        status=DuelStatus.ACCEPTED,
        trial_type=DuelTrial.TRIAL_BY_COMBAT,
        responded_at=1_700_000_100,
        next_reminder_at=None,
    )
    service.resolve.return_value = make_challenge(
        status=DuelStatus.VOIDED,
        resolved_at=1_700_000_200,
        next_reminder_at=None,
    )
    service.list_outstanding.return_value = [make_challenge()]
    service.list_pending_all.return_value = []
    return service


@pytest.fixture
def flavor_service():
    service = SimpleNamespace(generate=AsyncMock(return_value="The herald speaks."))
    return service


@pytest.fixture
def bot(duel_service, flavor_service, interaction):
    result = SimpleNamespace(
        duel_service=duel_service,
        duel_flavor_service=flavor_service,
        get_channel=MagicMock(return_value=interaction.channel),
        fetch_channel=AsyncMock(return_value=interaction.channel),
        add_cog=AsyncMock(),
        add_view=MagicMock(),
    )
    interaction.client = result
    return result


@pytest.fixture
def cog(bot, duel_service, flavor_service):
    return DuelCommands(bot, duel_service, flavor_service)


def _fields(embed: discord.Embed) -> dict[str, str]:
    return {field.name: field.value for field in embed.fields}


def _assert_mentions_disabled(allowed: discord.AllowedMentions) -> None:
    assert allowed.everyone is False
    assert allowed.roles is False
    assert allowed.users is False
    assert allowed.replied_user is False


def test_duel_is_one_top_level_group_with_approved_subcommands():
    assert DuelCommands.duel.name == "duel"
    assert DuelCommands.duel.description == "Challenges of honor"
    assert {command.name for command in DuelCommands.duel.commands} == {
        "issue",
        "respond",
        "list",
        "resolve",
    }


@pytest.mark.asyncio
async def test_response_buttons_have_durable_challenge_specific_ids(cog):
    view = DuelChallengeView(cog, 42)

    assert view.timeout is None
    assert [(item.label, item.custom_id) for item in view.children] == [
        ("Decline in Cowardice", "duel:42:decline"),
        ("Trial by Combat", "duel:42:trial_by_combat"),
        ("Trial of Five", "duel:42:trial_of_five"),
    ]


@pytest.mark.asyncio
async def test_button_calls_shared_response_handler(cog, interaction):
    button = DuelResponseButton(
        7,
        "trial_by_combat",
        label="Trial by Combat",
        style=discord.ButtonStyle.primary,
        emoji="\u2694\ufe0f",
    )
    view = DuelChallengeView(cog, 7)
    view.clear_items()
    view.add_item(button)
    cog.handle_response = AsyncMock()

    await button.callback(interaction)

    cog.handle_response.assert_awaited_once_with(interaction, 7, "trial_by_combat")


@pytest.mark.asyncio
async def test_issue_posts_scoped_ping_and_binds_message(
    cog, interaction, recipient, duel_service, flavor_service
):
    await DuelCommands.issue.callback(cog, interaction, recipient, 500)

    interaction.response.defer.assert_awaited_once()
    duel_service.issue.assert_called_once_with(
        GUILD_ID,
        CHANNEL_ID,
        RECIPIENT_ID,
        RECIPIENT_ID,
        500,
        recipient_is_bot=False,
    )
    flavor_service.generate.assert_awaited_once()
    assert flavor_service.generate.await_args.args[2]["issuance_fee"] == 50
    sent = interaction.followup.send.await_args
    assert sent.kwargs["content"] == recipient.mention
    assert isinstance(sent.kwargs["view"], DuelChallengeView)
    allowed = sent.kwargs["allowed_mentions"]
    assert allowed.everyone is False
    assert allowed.roles is False
    assert allowed.replied_user is False
    assert allowed.users == [recipient]
    duel_service.bind_message.assert_called_once_with(7, GUILD_ID, 300)


@pytest.mark.asyncio
async def test_issue_rejects_dms_ephemerally(cog, interaction, recipient, duel_service):
    interaction.guild = None

    await DuelCommands.issue.callback(cog, interaction, recipient, 500)

    duel_service.issue.assert_not_called()
    interaction.response.send_message.assert_awaited_once()
    assert interaction.response.send_message.await_args.kwargs["ephemeral"] is True


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("recipient_id", "recipient_is_bot", "message"),
    [
        (RECIPIENT_ID, False, "You cannot challenge yourself."),
        (3, True, "Bots cannot answer a challenge of honor."),
    ],
)
async def test_issue_surfaces_self_and_bot_rule_errors(
    cog,
    interaction,
    duel_service,
    recipient_id,
    recipient_is_bot,
    message,
):
    player = SimpleNamespace(
        id=recipient_id,
        mention=f"<@{recipient_id}>",
        bot=recipient_is_bot,
    )
    duel_service.issue.side_effect = ValueError(message)

    await DuelCommands.issue.callback(cog, interaction, player, 500)

    sent = interaction.followup.send.await_args
    assert sent.kwargs["content"] == message
    assert sent.kwargs["ephemeral"] is True
    _assert_mentions_disabled(sent.kwargs["allowed_mentions"])


@pytest.mark.asyncio
async def test_issue_refunds_escrow_when_initial_delivery_fails(
    cog, interaction, recipient, duel_service, monkeypatch
):
    send_error = discord.HTTPException(
        SimpleNamespace(status=500, reason="error"),
        "delivery failed",
    )
    monkeypatch.setattr(
        "commands.duel.safe_followup",
        AsyncMock(side_effect=send_error),
    )

    await DuelCommands.issue.callback(cog, interaction, recipient, 500)

    duel_service.mark_delivery_failed.assert_called_once_with(7, GUILD_ID, RECIPIENT_ID)
    duel_service.bind_message.assert_not_called()


@pytest.mark.asyncio
async def test_recipient_button_and_command_share_handler(cog, interaction, duel_service):
    await cog.handle_response(interaction, 7, "trial_by_combat")

    duel_service.respond.assert_called_once_with(GUILD_ID, RECIPIENT_ID, "trial_by_combat")

    duel_service.respond.reset_mock()
    interaction.response.defer.reset_mock()
    interaction.followup.send.reset_mock()
    interaction.channel.fetch_message.reset_mock()
    await DuelCommands.respond.callback(cog, interaction, "trial_by_combat")

    duel_service.respond.assert_called_once_with(GUILD_ID, RECIPIENT_ID, "trial_by_combat")


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("outstanding", "challenge_id"),
    [
        ([], 7),
        ([make_challenge(recipient_id=99)], 7),
        ([make_challenge(challenge_id=8)], 7),
        ([make_challenge(guild_id=999)], 7),
    ],
)
async def test_button_rejects_stale_unauthorized_or_cross_guild_challenge(
    cog, interaction, duel_service, outstanding, challenge_id
):
    duel_service.list_outstanding.return_value = outstanding

    await cog.handle_response(interaction, challenge_id, "decline")

    duel_service.respond.assert_not_called()
    interaction.response.send_message.assert_awaited_once()
    assert interaction.response.send_message.await_args.kwargs["ephemeral"] is True


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("trial", "detail"),
    [
        (
            DuelTrial.TRIAL_BY_COMBAT,
            "best-of-three, one-versus-one Dota mid",
        ),
        (DuelTrial.TRIAL_OF_FIVE, "a lobby using Immortal Draft"),
    ],
)
async def test_acceptance_edits_original_and_posts_approved_trial_detail(
    cog, interaction, duel_service, flavor_service, trial, detail
):
    accepted = make_challenge(
        status=DuelStatus.ACCEPTED,
        trial_type=trial,
        responded_at=1_700_000_100,
        next_reminder_at=None,
    )
    duel_service.respond.return_value = accepted

    await cog.handle_response(interaction, 7, trial.value)

    interaction.channel.fetch_message.assert_awaited_once_with(300)
    original = await interaction.channel.fetch_message(300)
    original.edit.assert_awaited_once()
    assert original.edit.await_args.kwargs["view"] is None
    assert _fields(original.edit.await_args.kwargs["embed"])["Status"] == "Accepted"
    flavor_service.generate.assert_awaited_with(
        DuelFlavorEvent.ACCEPTED_COMBAT
        if trial is DuelTrial.TRIAL_BY_COMBAT
        else DuelFlavorEvent.ACCEPTED_FIVE,
        GUILD_ID,
        ANY,
    )
    announcement = interaction.followup.send.await_args
    assert detail in announcement.kwargs["content"]
    _assert_mentions_disabled(announcement.kwargs["allowed_mentions"])


@pytest.mark.asyncio
async def test_decline_edits_original_disables_buttons_and_posts_detail(
    cog, interaction, duel_service
):
    duel_service.respond.return_value = make_challenge(
        status=DuelStatus.DECLINED,
        responded_at=1_700_000_100,
        next_reminder_at=None,
    )

    await cog.handle_response(interaction, 7, "decline")

    original = await interaction.channel.fetch_message(300)
    original.edit.assert_awaited_once()
    assert original.edit.await_args.kwargs["view"] is None
    assert _fields(original.edit.await_args.kwargs["embed"])["Status"] == "Declined"
    sent = interaction.followup.send.await_args
    assert "250 JC" in sent.kwargs["content"]
    _assert_mentions_disabled(sent.kwargs["allowed_mentions"])


@pytest.mark.asyncio
async def test_due_reminder_posts_only_the_recipient_ping(cog, bot, flavor_service):
    challenge = make_challenge()
    result = DuelDueResult(
        kind=DuelDueKind.REMINDER,
        challenge=challenge,
        remaining_seconds=172_800,
        ping_recipient=True,
    )

    await cog.deliver_due_result(result)

    flavor_service.generate.assert_awaited_once_with(
        DuelFlavorEvent.REMINDER,
        GUILD_ID,
        ANY,
    )
    channel = bot.get_channel.return_value
    sent = channel.send.await_args
    assert sent.kwargs["content"].startswith(f"<@{RECIPIENT_ID}>")
    allowed = sent.kwargs["allowed_mentions"]
    assert allowed.everyone is False
    assert allowed.roles is False
    assert allowed.replied_user is False
    assert [user.id for user in allowed.users] == [RECIPIENT_ID]


@pytest.mark.asyncio
async def test_due_expiry_edits_original_and_posts_without_mentions(
    cog, bot, flavor_service
):
    challenge = make_challenge(
        status=DuelStatus.EXPIRED,
        next_reminder_at=None,
    )
    result = DuelDueResult(kind=DuelDueKind.EXPIRED, challenge=challenge)

    await cog.deliver_due_result(result)

    flavor_service.generate.assert_awaited_once_with(
        DuelFlavorEvent.EXPIRED,
        GUILD_ID,
        ANY,
    )
    channel = bot.get_channel.return_value
    message = await channel.fetch_message(challenge.message_id)
    assert _fields(message.edit.await_args.kwargs["embed"])["Status"] == "Expired"
    assert message.edit.await_args.kwargs["view"] is None
    sent = channel.send.await_args
    assert "expired unanswered" in sent.kwargs["content"]
    _assert_mentions_disabled(sent.kwargs["allowed_mentions"])


@pytest.mark.parametrize(
    ("challenge", "expected_status", "extra_field", "extra_value"),
    [
        (make_challenge(), "Pending", "Response Deadline", "<t:1700604800:R>"),
        (
            make_challenge(
                status=DuelStatus.ACCEPTED,
                trial_type=DuelTrial.TRIAL_BY_COMBAT,
            ),
            "Accepted",
            "Trial",
            "Trial by Combat",
        ),
        (
            make_challenge(status=DuelStatus.DECLINED),
            "Declined",
            "Decline Penalty",
            "250 JC",
        ),
        (
            make_challenge(status=DuelStatus.EXPIRED),
            "Expired",
            "Decline Penalty",
            "250 JC",
        ),
        (
            make_challenge(
                status=DuelStatus.RESOLVED,
                winner_id=CHALLENGER_ID,
                trial_type=DuelTrial.TRIAL_OF_FIVE,
            ),
            "Resolved",
            "Winner",
            f"<@{CHALLENGER_ID}>",
        ),
        (
            make_challenge(
                status=DuelStatus.VOIDED,
                trial_type=DuelTrial.TRIAL_OF_FIVE,
            ),
            "Voided",
            "Refund",
            "500 JC stake to each player; issuance fee remains nonrefundable",
        ),
    ],
)
def test_challenge_embed_has_lifecycle_fields(
    cog, challenge, expected_status, extra_field, extra_value
):
    embed = cog.build_challenge_embed(challenge, "The herald speaks.")
    fields = _fields(embed)

    assert fields["Challenge"] == "#7"
    assert fields["Challenger"] == f"<@{CHALLENGER_ID}>"
    assert fields["Recipient"] == f"<@{RECIPIENT_ID}>"
    assert fields["Wager"] == "500 JC"
    assert fields["Issuance Fee"] == "50 JC — nonrefundable after delivery"
    assert fields["Status"] == expected_status
    assert fields[extra_field] == extra_value


@pytest.mark.asyncio
async def test_void_copy_says_only_stakes_are_refunded(
    cog, interaction, duel_service, monkeypatch
):
    monkeypatch.setattr("commands.duel.has_admin_permission", lambda _: True)
    duel_service.resolve.return_value = make_challenge(
        status=DuelStatus.VOIDED,
        trial_type=DuelTrial.TRIAL_BY_COMBAT,
        resolved_at=1_700_000_200,
        next_reminder_at=None,
    )

    await DuelCommands.resolve.callback(cog, interaction, 7, "void")

    detail = interaction.followup.send.await_args.kwargs["content"].lower()
    assert "stakes" in detail
    assert "issuance fee was not refunded" in detail


@pytest.mark.asyncio
async def test_list_splits_more_than_twenty_five_outstanding_challenges(
    cog, interaction, duel_service
):
    duel_service.list_outstanding.return_value = [
        replace(make_challenge(), challenge_id=index)
        for index in range(1, 28)
    ]

    await DuelCommands.list.callback(cog, interaction)

    assert interaction.followup.send.await_count == 2
    embeds = [call.kwargs["embed"] for call in interaction.followup.send.await_args_list]
    assert [len(embed.fields) for embed in embeds] == [25, 2]
    assert sum((field.name.startswith("#") for embed in embeds for field in embed.fields), 0) == 27
    for call in interaction.followup.send.await_args_list:
        _assert_mentions_disabled(call.kwargs["allowed_mentions"])


@pytest.mark.asyncio
async def test_list_has_clear_empty_state(cog, interaction, duel_service):
    duel_service.list_outstanding.return_value = []

    await DuelCommands.list.callback(cog, interaction)

    sent = interaction.followup.send.await_args
    assert "No pending or accepted duel challenges" in sent.kwargs["content"]
    assert sent.kwargs["ephemeral"] is True


@pytest.mark.asyncio
async def test_resolve_denies_non_admin(cog, interaction, duel_service, monkeypatch):
    monkeypatch.setattr("commands.duel.has_admin_permission", lambda _: False)

    await DuelCommands.resolve.callback(cog, interaction, 7, "void")

    duel_service.resolve.assert_not_called()
    interaction.response.send_message.assert_awaited_once()
    assert interaction.response.send_message.await_args.kwargs["ephemeral"] is True


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("outcome", "status", "winner_id"),
    [
        ("challenger_victory", DuelStatus.RESOLVED, CHALLENGER_ID),
        ("recipient_victory", DuelStatus.RESOLVED, RECIPIENT_ID),
        ("void", DuelStatus.VOIDED, None),
    ],
)
async def test_admin_resolve_supports_two_winners_and_void(
    cog,
    interaction,
    duel_service,
    monkeypatch,
    outcome,
    status,
    winner_id,
):
    monkeypatch.setattr("commands.duel.has_admin_permission", lambda _: True)
    duel_service.resolve.return_value = make_challenge(
        status=status,
        trial_type=DuelTrial.TRIAL_BY_COMBAT,
        winner_id=winner_id,
        resolved_at=1_700_000_200,
        next_reminder_at=None,
    )

    await DuelCommands.resolve.callback(cog, interaction, 7, outcome)

    duel_service.resolve.assert_called_once_with(GUILD_ID, RECIPIENT_ID, 7, outcome)
    original = await interaction.channel.fetch_message(300)
    assert original.edit.await_args.kwargs["view"] is None
    assert _fields(original.edit.await_args.kwargs["embed"])["Status"] == status.value.title()


@pytest.mark.asyncio
async def test_setup_restores_one_view_for_every_pending_challenge(
    bot, duel_service
):
    duel_service.list_pending_all.return_value = [
        make_challenge(challenge_id=7, message_id=300),
        make_challenge(challenge_id=8, message_id=None),
    ]

    await setup(bot)

    bot.add_cog.assert_awaited_once()
    assert bot.add_view.call_count == 2
    first, second = bot.add_view.call_args_list
    assert isinstance(first.args[0], DuelChallengeView)
    assert first.args[0].children[0].custom_id == "duel:7:decline"
    assert first.kwargs == {"message_id": 300}
    assert isinstance(second.args[0], DuelChallengeView)
    assert second.args[0].children[0].custom_id == "duel:8:decline"
    assert second.kwargs == {}


@pytest.mark.asyncio
async def test_setup_requires_both_duel_services(bot):
    del bot.duel_flavor_service

    with pytest.raises(RuntimeError, match="duel_flavor_service"):
        await setup(bot)
