from __future__ import annotations

import ast
import asyncio
import inspect
import logging
from dataclasses import replace
from types import SimpleNamespace
from unittest.mock import ANY, AsyncMock, MagicMock

import discord
import pytest

import commands.duel as duel_module
from commands.duel import (
    TRIAL_BY_COMBAT_RULES,
    DuelChallengeView,
    DuelCommands,
    DuelResponseButton,
    setup,
)
from domain.models.duel import (
    DuelChallenge,
    DuelDueKind,
    DuelDueResult,
    DuelRecipientFundingError,
    DuelStatus,
    DuelTrial,
)
from services.duel_flavor_service import DuelFlavorEvent

GUILD_ID = 100
CHANNEL_ID = 200
CHALLENGER_ID = 1
RECIPIENT_ID = 2


def make_text_channel(
    channel_id: int,
    *,
    guild: SimpleNamespace | None = None,
    can_send: bool = True,
):
    guild = guild or SimpleNamespace(
        id=GUILD_ID,
        me=SimpleNamespace(id=55),
    )
    channel = MagicMock(spec=discord.TextChannel)
    channel.id = channel_id
    channel.name = "dota-mm"
    channel.guild = guild
    channel.permissions_for.return_value = SimpleNamespace(send_messages=can_send)
    channel.send = AsyncMock()
    return channel


def test_trial_by_combat_rules_match_agreed_format():
    assert TRIAL_BY_COMBAT_RULES == (
        "Mirror matchups: both duelists play the same hero each game.\n"
        "• Game 1 hero: recipient's pick\n"
        "• Game 2 hero: challenger's pick\n"
        "• Tiebreaker: Shadow Fiend, mid\n"
        "• Hero pool: all heroes, no bans\n"
        "• Victory: tower destruction, two kills, or opponent surrender\n"
        "• If neither player has won by 15:00, the higher score wins: creep "
        "score + (kills × 35)\n"
        "• Prohibited: farming in the jungle, destroying observer wards, visiting "
        "other lanes, blocking the first wave of creeps, collecting and using "
        "runes, Bottle, and Infused Raindrops\n"
        "Players can agree to additional game rules that do not conflict with "
        "existing regulations."
    )


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


def test_flavor_details_do_not_expose_discord_ids():
    challenger_id = 111_222_333_444_555_666
    recipient_id = 777_888_999_000_111_222

    details = DuelCommands._flavor_details(
        make_challenge(
            challenger_id=challenger_id,
            recipient_id=recipient_id,
        )
    )

    assert details["challenger"] == "the challenger"
    assert details["recipient"] == "the recipient"
    assert "challenger_id" not in details
    assert "recipient_id" not in details
    assert challenger_id not in details.values()
    assert recipient_id not in details.values()


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
    message.delete = AsyncMock()
    return message


@pytest.fixture
def interaction(message, recipient):
    response = SimpleNamespace(
        defer=AsyncMock(),
        send_message=AsyncMock(),
        edit_message=AsyncMock(),
    )
    followup = SimpleNamespace(send=AsyncMock(return_value=message))
    guild = SimpleNamespace(id=GUILD_ID, me=SimpleNamespace(id=55))
    channel = make_text_channel(CHANNEL_ID, guild=guild)
    guild.text_channels = [channel]
    channel.fetch_message = AsyncMock(return_value=message)
    channel.get_partial_message = MagicMock(return_value=message)
    channel.send = AsyncMock(return_value=message)
    return SimpleNamespace(
        guild=guild,
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
    service.get_challenge.return_value = make_challenge()
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
        get_guild=MagicMock(return_value=interaction.guild),
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


def test_duel_service_calls_are_offloaded_from_the_event_loop():
    """Every synchronous duel service entry point must run via to_thread."""
    tree = ast.parse(inspect.getsource(duel_module))
    parents = {
        child: parent
        for parent in ast.walk(tree)
        for child in ast.iter_child_nodes(parent)
    }
    violations = []

    for node in ast.walk(tree):
        if not isinstance(node, ast.Attribute):
            continue
        owner = node.value
        is_service_method = (
            isinstance(owner, ast.Name) and owner.id == "duel_service"
        ) or (
            isinstance(owner, ast.Attribute)
            and owner.attr == "duel_service"
            and isinstance(owner.value, ast.Name)
            and owner.value.id == "self"
        )
        if not is_service_method:
            continue

        parent = parents[node]
        is_offloaded = (
            isinstance(parent, ast.Call)
            and bool(parent.args)
            and parent.args[0] is node
            and isinstance(parent.func, ast.Attribute)
            and parent.func.attr == "to_thread"
            and isinstance(parent.func.value, ast.Name)
            and parent.func.value.id == "asyncio"
        )
        if not is_offloaded:
            violations.append(f"{node.attr} at line {node.lineno}")

    assert not violations, violations


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
    cog, interaction, recipient, duel_service, flavor_service, message
):
    events = []

    async def send_initial(**kwargs):
        events.append(("send", kwargs.get("view")))
        return message

    def bind_message(*_args):
        events.append(("bind", None))

    async def attach_view(**kwargs):
        events.append(("edit", kwargs.get("view")))

    interaction.followup.send.side_effect = send_initial
    duel_service.bind_message.side_effect = bind_message
    message.edit.side_effect = attach_view

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
    assert "view" not in sent.kwargs
    allowed = sent.kwargs["allowed_mentions"]
    assert allowed.everyone is False
    assert allowed.roles is False
    assert allowed.replied_user is False
    assert allowed.users == [recipient]
    duel_service.bind_message.assert_called_once_with(7, GUILD_ID, 300)
    message.edit.assert_awaited_once()
    assert isinstance(message.edit.await_args.kwargs["view"], DuelChallengeView)
    assert message.edit.await_args.kwargs["allowed_mentions"] == allowed
    assert [event[0] for event in events] == ["send", "bind", "edit"]


@pytest.mark.asyncio
async def test_issue_rejects_dms_ephemerally(cog, interaction, recipient, duel_service):
    interaction.guild = None

    await DuelCommands.issue.callback(cog, interaction, recipient, 500)

    duel_service.issue.assert_not_called()
    interaction.response.send_message.assert_awaited_once()
    assert interaction.response.send_message.await_args.kwargs["ephemeral"] is True


@pytest.mark.asyncio
async def test_issue_reports_recipient_funding_failure_publicly_without_creating_post(
    cog, interaction, challenger, recipient, duel_service, flavor_service
):
    interaction.user = challenger
    duel_service.issue.side_effect = DuelRecipientFundingError(RECIPIENT_ID, 500)

    await DuelCommands.issue.callback(cog, interaction, recipient, 500)

    sent = interaction.followup.send.await_args
    assert sent.kwargs["content"] == (
        f"The duel from <@{CHALLENGER_ID}> to <@{RECIPIENT_ID}> failed because "
        "the challenged player cannot cover the 500 JC wager."
    )
    assert sent.kwargs["ephemeral"] is False
    _assert_mentions_disabled(sent.kwargs["allowed_mentions"])
    flavor_service.generate.assert_not_awaited()
    duel_service.bind_message.assert_not_called()


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
    feedback = interaction.followup.send.await_args.kwargs["content"]
    assert "wager and 50 JC issuance fee were refunded" in feedback


@pytest.mark.asyncio
async def test_issue_bind_race_discards_duplicate_without_cancelling_challenge(
    cog, interaction, recipient, duel_service, message
):
    duel_service.bind_message.side_effect = ValueError("bind race")

    await DuelCommands.issue.callback(cog, interaction, recipient, 500)

    initial = interaction.followup.send.await_args_list[0]
    assert "view" not in initial.kwargs
    message.delete.assert_awaited_once()
    duel_service.mark_delivery_failed.assert_not_called()
    message.edit.assert_not_awaited()
    assert interaction.followup.send.await_count == 1


@pytest.mark.asyncio
async def test_issue_bind_error_deletes_inert_post_before_refund(
    cog, interaction, recipient, duel_service, message
):
    duel_service.bind_message.side_effect = RuntimeError("database unavailable")

    await DuelCommands.issue.callback(cog, interaction, recipient, 500)

    message.delete.assert_awaited_once()
    duel_service.mark_delivery_failed.assert_called_once_with(
        7, GUILD_ID, RECIPIENT_ID
    )
    feedback = interaction.followup.send.await_args_list[-1].kwargs["content"]
    assert "wager and 50 JC issuance fee were refunded" in feedback


@pytest.mark.asyncio
async def test_issue_delivery_failure_does_not_refund_a_concurrently_bound_challenge(
    cog, interaction, recipient, duel_service, monkeypatch
):
    monkeypatch.setattr(
        "commands.duel.safe_followup",
        AsyncMock(side_effect=discord.DiscordException("delivery failed")),
    )
    duel_service.mark_delivery_failed.side_effect = ValueError("already bound")

    await DuelCommands.issue.callback(cog, interaction, recipient, 500)

    duel_service.mark_delivery_failed.assert_called_once_with(
        7, GUILD_ID, RECIPIENT_ID
    )
    interaction.followup.send.assert_not_awaited()


@pytest.mark.asyncio
async def test_issue_view_edit_failure_does_not_refund_bound_challenge(
    cog, interaction, recipient, duel_service, message
):
    message.edit.side_effect = discord.HTTPException(
        SimpleNamespace(status=500, reason="error"),
        "component edit failed",
    )

    await DuelCommands.issue.callback(cog, interaction, recipient, 500)

    duel_service.bind_message.assert_called_once_with(7, GUILD_ID, 300)
    message.edit.assert_awaited_once()
    duel_service.mark_delivery_failed.assert_not_called()


@pytest.mark.asyncio
async def test_recipient_button_and_command_share_handler(cog, interaction, duel_service):
    await cog.handle_response(interaction, 7, "trial_by_combat")

    duel_service.respond.assert_called_once_with(GUILD_ID, RECIPIENT_ID, "trial_by_combat")

    duel_service.respond.reset_mock()
    interaction.response.defer.reset_mock()
    interaction.followup.send.reset_mock()
    interaction.channel.get_partial_message.reset_mock()
    interaction.channel.get_partial_message.return_value.edit.reset_mock()
    await DuelCommands.respond.callback(cog, interaction, "trial_by_combat")

    duel_service.respond.assert_called_once_with(GUILD_ID, RECIPIENT_ID, "trial_by_combat")


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("challenge", "challenge_id"),
    [
        (None, 7),
        (make_challenge(recipient_id=99), 7),
        (make_challenge(challenge_id=8), 7),
        (make_challenge(guild_id=999), 7),
        (make_challenge(status=DuelStatus.ACCEPTED), 7),
        (make_challenge(message_id=None), 7),
    ],
)
async def test_button_rejects_stale_unauthorized_or_cross_guild_challenge(
    cog, interaction, duel_service, challenge, challenge_id
):
    duel_service.get_challenge.return_value = challenge

    await cog.handle_response(interaction, challenge_id, "decline")

    duel_service.get_challenge.assert_called_once_with(challenge_id, GUILD_ID)
    duel_service.list_outstanding.assert_not_called()
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

    interaction.channel.get_partial_message.assert_called_once_with(300)
    interaction.channel.fetch_message.assert_not_awaited()
    original = interaction.channel.get_partial_message.return_value
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
    content = announcement.kwargs["content"]
    assert detail in content
    assert "Challenge #7" in content
    assert f"<@{CHALLENGER_ID}> vs <@{RECIPIENT_ID}>" in content
    assert "500 JC" in content
    fields = _fields(original.edit.await_args.kwargs["embed"])
    if trial is DuelTrial.TRIAL_BY_COMBAT:
        assert TRIAL_BY_COMBAT_RULES in content
        assert fields["Rules"] == TRIAL_BY_COMBAT_RULES
    else:
        assert "Rules" not in fields
        assert "Shadow Fiend" not in content
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

    interaction.channel.get_partial_message.assert_called_once_with(300)
    interaction.channel.fetch_message.assert_not_awaited()
    original = interaction.channel.get_partial_message.return_value
    original.edit.assert_awaited_once()
    assert original.edit.await_args.kwargs["view"] is None
    assert _fields(original.edit.await_args.kwargs["embed"])["Status"] == "Declined"
    sent = interaction.followup.send.await_args
    assert "250 JC" in sent.kwargs["content"]
    _assert_mentions_disabled(sent.kwargs["allowed_mentions"])


@pytest.mark.asyncio
async def test_acceptance_funding_failure_voids_post_and_explains_refund(
    cog, interaction, duel_service, flavor_service
):
    duel_service.respond.return_value = make_challenge(
        status=DuelStatus.VOIDED,
        responded_at=1_700_000_100,
        next_reminder_at=None,
    )

    await cog.handle_response(interaction, 7, "trial_by_combat")

    interaction.channel.get_partial_message.assert_called_once_with(300)
    interaction.channel.fetch_message.assert_not_awaited()
    original = interaction.channel.get_partial_message.return_value
    original.edit.assert_awaited_once()
    edit = original.edit.await_args.kwargs
    assert edit["view"] is None
    fields = _fields(edit["embed"])
    assert fields["Status"] == "Voided"
    assert fields["Funding Failure"] == (
        "The recipient could not fund the 500 JC wager."
    )
    assert fields["Refund"] == (
        "500 JC challenger stake refunded; "
        "50 JC issuance fee remains nonrefundable after delivery"
    )
    flavor_service.generate.assert_awaited_once_with(
        DuelFlavorEvent.VOIDED,
        GUILD_ID,
        ANY,
    )
    sent = interaction.followup.send.await_args
    assert sent.kwargs["ephemeral"] is False
    assert "recipient could not fund the 500 JC wager" in sent.kwargs["content"]
    assert "challenger's 500 JC stake was refunded" in sent.kwargs["content"]
    assert "50 JC issuance fee remains nonrefundable after delivery" in sent.kwargs[
        "content"
    ]
    _assert_mentions_disabled(sent.kwargs["allowed_mentions"])


@pytest.mark.asyncio
async def test_acceptance_funding_failure_still_announces_when_original_was_deleted(
    cog, interaction, duel_service, flavor_service
):
    duel_service.respond.return_value = make_challenge(
        status=DuelStatus.VOIDED,
        responded_at=1_700_000_100,
        next_reminder_at=None,
    )
    interaction.channel.get_partial_message.return_value.edit.side_effect = discord.NotFound(
        MagicMock(status=404),
        "message deleted",
    )

    await cog.handle_response(interaction, 7, "trial_by_combat")

    flavor_service.generate.assert_awaited_once_with(
        DuelFlavorEvent.VOIDED,
        GUILD_ID,
        ANY,
    )
    sent = interaction.followup.send.await_args
    assert "recipient could not fund the 500 JC wager" in sent.kwargs["content"]
    assert sent.kwargs["ephemeral"] is False
    interaction.channel.get_partial_message.assert_called_once_with(300)
    interaction.channel.fetch_message.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("remaining_seconds", [48 * 3600, 24 * 3600])
async def test_final_due_reminders_post_only_the_recipient_ping(
    cog, bot, flavor_service, remaining_seconds
):
    challenge = make_challenge()
    result = DuelDueResult(
        kind=DuelDueKind.REMINDER,
        challenge=challenge,
        remaining_seconds=remaining_seconds,
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
async def test_unresolved_daily_reminder_pings_both_participants(
    cog, bot, duel_service, flavor_service
):
    challenge = make_challenge(
        status=DuelStatus.ACCEPTED,
        trial_type=DuelTrial.TRIAL_BY_COMBAT,
        responded_at=1_700_000_100,
        next_reminder_at=1_700_086_500,
    )
    duel_service.get_challenge.return_value = challenge
    result = DuelDueResult(kind=DuelDueKind.UNRESOLVED, challenge=challenge)

    await cog.deliver_due_result(result)

    flavor_service.generate.assert_awaited_once_with(
        DuelFlavorEvent.UNRESOLVED,
        GUILD_ID,
        ANY,
    )
    sent = bot.get_channel.return_value.send.await_args
    content = sent.kwargs["content"]
    assert content.startswith(f"<@{CHALLENGER_ID}> <@{RECIPIENT_ID}>")
    assert "Challenge #7" in content
    assert "Trial by Combat" in content
    assert "/duel resolve" in content
    allowed = sent.kwargs["allowed_mentions"]
    assert allowed.everyone is False
    assert allowed.roles is False
    assert allowed.replied_user is False
    assert {user.id for user in allowed.users} == {CHALLENGER_ID, RECIPIENT_ID}


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("kind", "status"),
    [
        (DuelDueKind.REMINDER, DuelStatus.PENDING),
        (DuelDueKind.UNRESOLVED, DuelStatus.ACCEPTED),
    ],
)
async def test_due_reminders_post_in_main_channel_not_origin(
    cog, bot, duel_service, interaction, kind, status
):
    main_channel_id = 999
    main_channel = make_text_channel(main_channel_id, guild=interaction.guild)
    origin_channel = interaction.channel
    origin_channel.name = "gamba"
    interaction.guild.text_channels = [origin_channel, main_channel]
    challenge = make_challenge(
        status=status,
        trial_type=(
            DuelTrial.TRIAL_BY_COMBAT
            if status is DuelStatus.ACCEPTED
            else None
        ),
    )
    duel_service.get_challenge.return_value = challenge
    result = DuelDueResult(
        kind=kind,
        challenge=challenge,
        remaining_seconds=48 * 3600,
        ping_recipient=True,
    )

    await cog.deliver_due_result(result)

    main_channel.send.assert_awaited_once()
    origin_channel.send.assert_not_awaited()


@pytest.mark.asyncio
async def test_due_reminder_resolves_decorated_dota_mm_by_name(
    cog, bot, duel_service, interaction
):
    origin_channel = interaction.channel
    origin_channel.name = "gamba"
    main_channel = make_text_channel(999, guild=interaction.guild)
    main_channel.name = "⚔️dota-mm⚔️"
    interaction.guild.text_channels = [origin_channel, main_channel]
    bot.get_guild = MagicMock(return_value=interaction.guild)
    challenge = make_challenge()
    duel_service.get_challenge.return_value = challenge
    result = DuelDueResult(
        kind=DuelDueKind.REMINDER,
        challenge=challenge,
        remaining_seconds=48 * 3600,
        ping_recipient=True,
    )

    await cog.deliver_due_result(result)

    main_channel.send.assert_awaited_once()
    origin_channel.send.assert_not_awaited()


@pytest.mark.asyncio
async def test_due_reminder_falls_back_to_origin_when_dota_mm_is_unavailable(
    cog, duel_service, interaction
):
    interaction.channel.name = "gamba"
    challenge = make_challenge()
    duel_service.get_challenge.return_value = challenge
    result = DuelDueResult(
        kind=DuelDueKind.REMINDER,
        challenge=challenge,
        remaining_seconds=48 * 3600,
        ping_recipient=True,
    )

    await cog.deliver_due_result(result)

    interaction.channel.send.assert_awaited_once()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("kind", "stale_status"),
    [
        (DuelDueKind.UNRESOLVED, DuelStatus.RESOLVED),
        (DuelDueKind.REMINDER, DuelStatus.ACCEPTED),
    ],
)
async def test_stale_claimed_reminder_is_not_delivered(
    cog, bot, duel_service, flavor_service, kind, stale_status
):
    claimed = make_challenge(
        status=(
            DuelStatus.ACCEPTED
            if kind is DuelDueKind.UNRESOLVED
            else DuelStatus.PENDING
        ),
        trial_type=DuelTrial.TRIAL_BY_COMBAT,
    )
    duel_service.get_challenge.return_value = make_challenge(status=stale_status)
    result = DuelDueResult(
        kind=kind,
        challenge=claimed,
        remaining_seconds=48 * 3600,
        ping_recipient=True,
    )

    await cog.deliver_due_result(result)

    flavor_service.generate.assert_not_awaited()
    bot.get_channel.return_value.send.assert_not_awaited()


@pytest.mark.asyncio
async def test_missing_dota_mm_channel_uses_origin_for_reminder_claims(
    cog, bot, duel_service, interaction
):
    interaction.channel.name = "gamba"
    duel_service.process_due.return_value = None

    await cog.process_due_challenge(7, GUILD_ID, 1_700_432_000)

    duel_service.process_due.assert_called_once_with(
        7, GUILD_ID, 1_700_432_000, claim_reminders=True
    )


@pytest.mark.asyncio
async def test_duplicate_dota_mm_channels_use_origin_for_reminder_claims(
    cog, duel_service, interaction
):
    interaction.guild.text_channels.append(
        make_text_channel(999, guild=interaction.guild, can_send=False)
    )
    duel_service.process_due.return_value = None

    await cog.process_due_challenge(7, GUILD_ID, 1_700_432_000)

    duel_service.process_due.assert_called_once_with(
        7, GUILD_ID, 1_700_432_000, claim_reminders=True
    )


@pytest.mark.asyncio
async def test_dota_mm_channel_allows_reminder_claims_without_configuration(
    cog, duel_service
):
    duel_service.process_due.return_value = None

    await cog.process_due_challenge(7, GUILD_ID, 1_700_432_000)

    duel_service.process_due.assert_called_once_with(
        7, GUILD_ID, 1_700_432_000, claim_reminders=True
    )


@pytest.mark.asyncio
async def test_main_channel_from_different_guild_uses_origin_for_reminder_claims(
    cog, bot, duel_service
):
    channel = make_text_channel(
        999,
        guild=SimpleNamespace(id=GUILD_ID + 1, me=SimpleNamespace(id=55)),
    )
    channel.guild.text_channels = [channel]
    bot.get_guild.return_value = channel.guild
    duel_service.process_due.return_value = None

    await cog.process_due_challenge(7, GUILD_ID, 1_700_432_000)

    duel_service.process_due.assert_called_once_with(
        7, GUILD_ID, 1_700_432_000, claim_reminders=True
    )


@pytest.mark.asyncio
async def test_due_reminder_is_not_delivered_to_another_guild(
    cog, bot, interaction
):
    other_guild_channel = make_text_channel(
        999,
        guild=SimpleNamespace(id=GUILD_ID + 1, me=SimpleNamespace(id=55)),
    )
    other_guild_channel.guild.text_channels = [other_guild_channel]
    bot.get_guild.return_value = other_guild_channel.guild
    result = DuelDueResult(
        kind=DuelDueKind.REMINDER,
        challenge=make_challenge(),
        remaining_seconds=48 * 3600,
        ping_recipient=True,
    )

    await cog.deliver_due_result(result)

    other_guild_channel.send.assert_not_awaited()
    interaction.channel.send.assert_awaited_once()


@pytest.mark.asyncio
@pytest.mark.parametrize("invalid_channel", ["guildless", "non_text", "no_send"])
async def test_invalid_main_channel_uses_origin_for_reminder_claims(
    cog, bot, duel_service, invalid_channel
):
    if invalid_channel == "guildless":
        channel = SimpleNamespace(id=999, name="dota-mm", send=AsyncMock())
    elif invalid_channel == "non_text":
        channel = SimpleNamespace(
            id=999,
            name="dota-mm",
            guild=SimpleNamespace(id=GUILD_ID),
            send=AsyncMock(),
        )
    else:
        guild = SimpleNamespace(id=GUILD_ID, me=SimpleNamespace(id=55))
        channel = MagicMock(spec=discord.TextChannel)
        channel.id = 999
        channel.name = "dota-mm"
        channel.guild = guild
        channel.permissions_for.return_value = SimpleNamespace(send_messages=False)
    bot.get_guild.return_value.text_channels = [channel]
    duel_service.process_due.return_value = None

    await cog.process_due_challenge(7, GUILD_ID, 1_700_432_000)

    duel_service.process_due.assert_called_once_with(
        7, GUILD_ID, 1_700_432_000, claim_reminders=True
    )


@pytest.mark.asyncio
async def test_due_reminder_uses_origin_without_main_send_permission(
    cog, bot, interaction
):
    guild = bot.get_guild.return_value
    main_channel = make_text_channel(999, guild=guild, can_send=False)
    guild.text_channels = [main_channel]
    result = DuelDueResult(
        kind=DuelDueKind.REMINDER,
        challenge=make_challenge(),
        remaining_seconds=48 * 3600,
        ping_recipient=True,
    )

    await cog.deliver_due_result(result)

    main_channel.send.assert_not_awaited()
    interaction.channel.send.assert_awaited_once()


@pytest.mark.asyncio
async def test_unsendable_origin_channel_defers_reminder_claims(
    cog, bot, duel_service, flavor_service, interaction
):
    """A permanently unsendable origin must defer the claim, not hot-loop."""
    interaction.channel.permissions_for.return_value = SimpleNamespace(
        send_messages=False
    )
    duel_service.process_due.return_value = None

    await cog.process_due_challenge(7, GUILD_ID, 1_700_432_000)

    duel_service.process_due.assert_called_once_with(
        7, GUILD_ID, 1_700_432_000, claim_reminders=False
    )
    flavor_service.generate.assert_not_awaited()
    interaction.channel.send.assert_not_awaited()


@pytest.mark.asyncio
async def test_deliver_skips_unsendable_origin_channel(cog, bot, interaction):
    interaction.channel.permissions_for.return_value = SimpleNamespace(
        send_messages=False
    )
    result = DuelDueResult(
        kind=DuelDueKind.REMINDER,
        challenge=make_challenge(),
        remaining_seconds=48 * 3600,
        ping_recipient=True,
    )

    delivered = await cog.deliver_due_result(result)

    assert not delivered
    interaction.channel.send.assert_not_awaited()


def test_main_channel_prefers_exact_name_over_substring_matches(
    cog, interaction
):
    exact_channel = interaction.channel
    clips_channel = make_text_channel(999, guild=interaction.guild)
    clips_channel.name = "dota-mm-clips"
    interaction.guild.text_channels = [clips_channel, exact_channel]

    assert cog._get_main_channel(GUILD_ID) is exact_channel


def test_main_channel_keeps_unique_substring_match(cog, interaction):
    decorated_channel = interaction.channel
    decorated_channel.name = "the-dota-mm-hub"

    assert cog._get_main_channel(GUILD_ID) is decorated_channel


def test_main_channel_prefers_configured_duel_channel(
    cog, interaction, monkeypatch
):
    import config

    configured = make_text_channel(4242, guild=interaction.guild)
    configured.name = "honor-duels"
    interaction.guild.get_channel = MagicMock(return_value=configured)
    monkeypatch.setattr(config, "DUEL_CHANNEL_ID", 4242)

    assert cog._get_main_channel(GUILD_ID) is configured
    interaction.guild.get_channel.assert_called_once_with(4242)


def test_main_channel_scans_by_name_when_unconfigured(
    cog, interaction, monkeypatch
):
    import config

    interaction.guild.get_channel = MagicMock()
    monkeypatch.setattr(config, "DUEL_CHANNEL_ID", None)

    assert cog._get_main_channel(GUILD_ID) is interaction.channel
    interaction.guild.get_channel.assert_not_called()


@pytest.mark.parametrize("configured_state", ["missing", "unsendable"])
def test_main_channel_falls_back_to_name_scan_when_configured_is_unusable(
    cog, interaction, monkeypatch, configured_state
):
    import config

    if configured_state == "missing":
        configured = None
    else:
        configured = make_text_channel(
            4242, guild=interaction.guild, can_send=False
        )
    interaction.guild.get_channel = MagicMock(return_value=configured)
    monkeypatch.setattr(config, "DUEL_CHANNEL_ID", 4242)

    assert cog._get_main_channel(GUILD_ID) is interaction.channel


@pytest.mark.asyncio
async def test_earlier_daily_reminder_suppresses_recipient_ping(cog, bot):
    result = DuelDueResult(
        kind=DuelDueKind.REMINDER,
        challenge=make_challenge(),
        remaining_seconds=72 * 3600,
        ping_recipient=False,
    )

    await cog.deliver_due_result(result)

    sent = bot.get_channel.return_value.send.await_args
    _assert_mentions_disabled(sent.kwargs["allowed_mentions"])
    assert not sent.kwargs["content"].startswith(f"<@{RECIPIENT_ID}>")


@pytest.mark.asyncio
async def test_due_expiry_fetches_origin_channel_after_cache_miss(cog, bot, interaction):
    bot.get_channel.return_value = None
    challenge = make_challenge(status=DuelStatus.EXPIRED, next_reminder_at=None)
    result = DuelDueResult(
        kind=DuelDueKind.EXPIRED,
        challenge=challenge,
    )

    await cog.deliver_due_result(result)

    assert bot.fetch_channel.await_count == 2
    assert all(
        awaited.args == (CHANNEL_ID,)
        for awaited in bot.fetch_channel.await_args_list
    )
    interaction.channel.send.assert_awaited_once()


@pytest.mark.asyncio
async def test_due_expiry_channel_fetch_failure_logs_challenge_guild_and_channel(
    cog, bot, caplog
):
    bot.get_guild.return_value.text_channels = []
    bot.get_channel.return_value = None
    bot.fetch_channel.side_effect = discord.DiscordException("forbidden")
    challenge = make_challenge(status=DuelStatus.EXPIRED, next_reminder_at=None)
    result = DuelDueResult(
        kind=DuelDueKind.EXPIRED,
        challenge=challenge,
    )

    with caplog.at_level(logging.WARNING, logger="cama_bot.commands.duel"):
        await cog.deliver_due_result(result)

    messages = "\n".join(record.getMessage() for record in caplog.records)
    assert "challenge=7" in messages
    assert f"guild={GUILD_ID}" in messages
    assert f"channel={CHANNEL_ID}" in messages


@pytest.mark.asyncio
async def test_due_channel_send_failure_logs_challenge_guild_and_channel(
    cog, bot, caplog
):
    bot.get_channel.return_value.send.side_effect = discord.DiscordException("forbidden")
    result = DuelDueResult(
        kind=DuelDueKind.REMINDER,
        challenge=make_challenge(),
        remaining_seconds=48 * 3600,
        ping_recipient=True,
    )

    with caplog.at_level(logging.ERROR, logger="cama_bot.commands.duel"):
        await cog.deliver_due_result(result)

    messages = "\n".join(record.getMessage() for record in caplog.records)
    assert "challenge=7" in messages
    assert f"guild={GUILD_ID}" in messages
    assert f"channel={CHANNEL_ID}" in messages


@pytest.mark.asyncio
async def test_due_reminder_falls_back_to_origin_after_main_send_failure(
    cog, bot, duel_service, interaction
):
    origin_channel = interaction.channel
    origin_channel.name = "gamba"
    main_channel = make_text_channel(999, guild=interaction.guild)
    main_channel.send.side_effect = discord.DiscordException("forbidden")
    interaction.guild.text_channels = [origin_channel, main_channel]
    challenge = make_challenge()
    duel_service.get_challenge.return_value = challenge
    result = DuelDueResult(
        kind=DuelDueKind.REMINDER,
        challenge=challenge,
        remaining_seconds=48 * 3600,
        ping_recipient=True,
    )

    delivered = await cog.deliver_due_result(result)

    assert delivered
    main_channel.send.assert_awaited_once()
    origin_channel.send.assert_awaited_once()


@pytest.mark.asyncio
async def test_due_reminder_stops_fallback_when_status_changes_after_send_failure(
    cog, duel_service, interaction
):
    origin_channel = interaction.channel
    origin_channel.name = "gamba"
    main_channel = make_text_channel(999, guild=interaction.guild)
    main_channel.send.side_effect = discord.DiscordException("forbidden")
    interaction.guild.text_channels = [origin_channel, main_channel]
    challenge = make_challenge()
    duel_service.get_challenge.side_effect = [
        challenge,
        challenge,
        make_challenge(status=DuelStatus.ACCEPTED),
    ]
    result = DuelDueResult(
        kind=DuelDueKind.REMINDER,
        challenge=challenge,
        remaining_seconds=48 * 3600,
        ping_recipient=True,
    )

    delivered = await cog.deliver_due_result(result)

    assert not delivered
    main_channel.send.assert_awaited_once()
    origin_channel.send.assert_not_awaited()


@pytest.mark.asyncio
async def test_process_due_challenge_claims_off_loop_and_delivers(cog, duel_service):
    result = DuelDueResult(
        kind=DuelDueKind.REMINDER,
        challenge=make_challenge(),
        remaining_seconds=48 * 3600,
        ping_recipient=True,
    )
    duel_service.process_due.return_value = result
    cog.deliver_due_result = AsyncMock()

    await cog.process_due_challenge(7, GUILD_ID, 1_700_432_000)

    duel_service.process_due.assert_called_once_with(
        7, GUILD_ID, 1_700_432_000, claim_reminders=True
    )
    cog.deliver_due_result.assert_awaited_once_with(result)


@pytest.mark.asyncio
async def test_process_due_challenge_rearms_reminder_after_send_failure(
    cog, bot, duel_service
):
    challenge = make_challenge(next_reminder_at=1_700_518_400)
    result = DuelDueResult(
        kind=DuelDueKind.REMINDER,
        challenge=challenge,
        remaining_seconds=48 * 3600,
        ping_recipient=True,
        claimed_reminder_at=1_700_432_000,
    )
    duel_service.process_due.return_value = result
    bot.get_guild.return_value.text_channels[0].send.side_effect = (
        discord.DiscordException("forbidden")
    )

    await cog.process_due_challenge(7, GUILD_ID, 1_700_432_000)

    duel_service.rearm_reminder.assert_called_once_with(result)


@pytest.mark.asyncio
async def test_process_due_challenge_does_not_rearm_after_origin_fallback(
    cog, bot, duel_service, interaction
):
    origin_channel = interaction.channel
    origin_channel.name = "gamba"
    main_channel = make_text_channel(999, guild=interaction.guild)
    main_channel.send.side_effect = discord.DiscordException("forbidden")
    interaction.guild.text_channels = [origin_channel, main_channel]
    challenge = make_challenge(next_reminder_at=1_700_518_400)
    result = DuelDueResult(
        kind=DuelDueKind.REMINDER,
        challenge=challenge,
        remaining_seconds=48 * 3600,
        ping_recipient=True,
        claimed_reminder_at=1_700_432_000,
    )
    duel_service.process_due.return_value = result

    await cog.process_due_challenge(7, GUILD_ID, 1_700_432_000)

    origin_channel.send.assert_awaited_once()
    duel_service.rearm_reminder.assert_not_called()


@pytest.mark.asyncio
async def test_undelivered_expiry_settlement_logs_amounts(
    cog, bot, duel_service, caplog
):
    """Expiry moves coins; a lost announcement must at least log the transfer."""
    bot.get_guild.return_value.text_channels = []
    bot.get_channel.return_value = None
    bot.fetch_channel.side_effect = discord.DiscordException("gone")
    challenge = make_challenge(status=DuelStatus.EXPIRED, next_reminder_at=None)
    duel_service.process_due.return_value = DuelDueResult(
        kind=DuelDueKind.EXPIRED,
        challenge=challenge,
    )

    with caplog.at_level(logging.ERROR, logger="cama_bot.commands.duel"):
        await cog.process_due_challenge(7, GUILD_ID, 1_700_432_000)

    duel_service.rearm_reminder.assert_not_called()
    messages = "\n".join(
        record.getMessage()
        for record in caplog.records
        if record.levelno >= logging.ERROR
    )
    assert "challenge=7" in messages
    assert f"guild={GUILD_ID}" in messages
    assert f"challenger={CHALLENGER_ID}" in messages
    assert f"recipient={RECIPIENT_ID}" in messages
    assert f"{challenge.wager} JC escrow" in messages
    assert f"{challenge.decline_penalty} JC penalty" in messages


@pytest.mark.asyncio
async def test_delivered_expiry_announcement_is_confirmed(cog, duel_service):
    """A confirmed send retires the durable pending-announcement marker."""
    challenge = make_challenge(
        status=DuelStatus.EXPIRED,
        next_reminder_at=1_700_604_800,
    )
    result = DuelDueResult(kind=DuelDueKind.EXPIRED, challenge=challenge)
    duel_service.process_due.return_value = result

    await cog.process_due_challenge(7, GUILD_ID, 1_700_604_800)

    duel_service.confirm_expiry_announced.assert_called_once_with(result)
    duel_service.rearm_reminder.assert_not_called()


@pytest.mark.asyncio
async def test_failed_expiry_announcement_is_not_confirmed(
    cog, duel_service, interaction
):
    """A transient send failure must leave the marker armed for retry."""
    interaction.channel.send.side_effect = discord.DiscordException("timeout")
    challenge = make_challenge(
        status=DuelStatus.EXPIRED,
        next_reminder_at=1_700_604_800,
    )
    result = DuelDueResult(kind=DuelDueKind.EXPIRED, challenge=challenge)
    duel_service.process_due.return_value = result

    await cog.process_due_challenge(7, GUILD_ID, 1_700_604_800)

    duel_service.confirm_expiry_announced.assert_not_called()
    duel_service.rearm_reminder.assert_not_called()


@pytest.mark.asyncio
async def test_process_due_challenge_ignores_lost_claim(cog, duel_service):
    duel_service.process_due.return_value = None
    cog.deliver_due_result = AsyncMock()

    await cog.process_due_challenge(7, GUILD_ID, 1_700_432_000)

    cog.deliver_due_result.assert_not_awaited()


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
    channel.get_partial_message.assert_called_once_with(challenge.message_id)
    channel.fetch_message.assert_not_awaited()
    message = channel.get_partial_message.return_value
    assert _fields(message.edit.await_args.kwargs["embed"])["Status"] == "Expired"
    assert message.edit.await_args.kwargs["view"] is None
    sent = channel.send.await_args
    assert "declined in cowardice by silence" in sent.kwargs["content"]
    _assert_mentions_disabled(sent.kwargs["allowed_mentions"])


@pytest.mark.asyncio
async def test_due_expiry_announcement_prefers_main_channel(
    cog, bot, interaction
):
    main_channel_id = 999
    main_channel = make_text_channel(main_channel_id, guild=interaction.guild)
    origin_channel = interaction.channel
    origin_channel.name = "gamba"
    interaction.guild.text_channels = [origin_channel, main_channel]
    bot.get_channel.side_effect = lambda channel_id: (
        main_channel if channel_id == main_channel_id else origin_channel
    )
    challenge = make_challenge(
        status=DuelStatus.EXPIRED,
        next_reminder_at=None,
    )
    result = DuelDueResult(kind=DuelDueKind.EXPIRED, challenge=challenge)

    await cog.deliver_due_result(result)

    main_channel.send.assert_awaited_once()
    origin_channel.send.assert_not_awaited()
    origin_channel.get_partial_message.assert_called_once_with(challenge.message_id)


@pytest.mark.asyncio
async def test_due_expiry_does_not_edit_origin_channel_from_another_guild(
    cog, bot, interaction
):
    main_channel = make_text_channel(999, guild=interaction.guild)
    interaction.guild.text_channels = [main_channel]
    other_guild = SimpleNamespace(
        id=GUILD_ID + 1,
        me=SimpleNamespace(id=55),
    )
    other_guild_channel = make_text_channel(CHANNEL_ID, guild=other_guild)
    other_guild_channel.get_partial_message.return_value = SimpleNamespace(
        edit=AsyncMock()
    )
    bot.get_channel.return_value = other_guild_channel
    challenge = make_challenge(
        status=DuelStatus.EXPIRED,
        next_reminder_at=None,
    )
    result = DuelDueResult(kind=DuelDueKind.EXPIRED, challenge=challenge)

    delivered = await cog.deliver_due_result(result)

    assert delivered
    main_channel.send.assert_awaited_once()
    other_guild_channel.get_partial_message.assert_not_called()


@pytest.mark.asyncio
async def test_due_expiry_announcement_falls_back_to_origin_channel(
    cog, interaction
):
    interaction.channel.name = "gamba"
    challenge = make_challenge(
        status=DuelStatus.EXPIRED,
        next_reminder_at=None,
    )
    result = DuelDueResult(kind=DuelDueKind.EXPIRED, challenge=challenge)

    await cog.deliver_due_result(result)

    interaction.channel.send.assert_awaited_once()
    interaction.channel.get_partial_message.assert_called_once_with(
        challenge.message_id
    )


@pytest.mark.asyncio
async def test_due_expiry_announcement_falls_back_after_main_send_failure(
    cog, interaction
):
    origin_channel = interaction.channel
    origin_channel.name = "gamba"
    main_channel = make_text_channel(999, guild=interaction.guild)
    main_channel.send.side_effect = discord.DiscordException("forbidden")
    interaction.guild.text_channels = [origin_channel, main_channel]
    challenge = make_challenge(
        status=DuelStatus.EXPIRED,
        next_reminder_at=None,
    )
    result = DuelDueResult(kind=DuelDueKind.EXPIRED, challenge=challenge)

    delivered = await cog.deliver_due_result(result)

    assert delivered
    main_channel.send.assert_awaited_once()
    origin_channel.send.assert_awaited_once()


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
    interaction.channel.get_partial_message.assert_called_once_with(300)
    interaction.channel.fetch_message.assert_not_awaited()
    original = interaction.channel.get_partial_message.return_value
    assert original.edit.await_args.kwargs["view"] is None
    assert _fields(original.edit.await_args.kwargs["embed"])["Status"] == status.value.title()


@pytest.mark.asyncio
async def test_setup_restores_one_view_for_every_pending_challenge(
    bot, duel_service, message
):
    duel_service.list_pending_all.return_value = [
        make_challenge(challenge_id=7, message_id=300),
    ]

    await setup(bot)

    bot.add_cog.assert_awaited_once()
    assert bot.add_view.call_count == 1
    first = bot.add_view.call_args
    assert isinstance(first.args[0], DuelChallengeView)
    assert first.args[0].children[0].custom_id == "duel:7:decline"
    assert first.kwargs == {"message_id": 300}
    message.edit.assert_awaited_once()
    assert isinstance(message.edit.await_args.kwargs["view"], DuelChallengeView)
    _assert_mentions_disabled(message.edit.await_args.kwargs["allowed_mentions"])
    channel = bot.get_channel.return_value
    channel.get_partial_message.assert_called_once_with(300)
    channel.fetch_message.assert_not_awaited()


@pytest.mark.asyncio
async def test_setup_caps_bound_recovery_and_registers_all_views_first(
    bot, duel_service
):
    challenge_count = 12
    duel_service.list_pending_all.return_value = [
        make_challenge(challenge_id=index, message_id=300 + index)
        for index in range(challenge_count)
    ]

    release_edits = asyncio.Event()
    concurrency_reached = asyncio.Event()
    active_edits = 0
    peak_edits = 0
    edit_count = 0
    registered_counts: list[int] = []

    async def edit(**_kwargs):
        nonlocal active_edits, peak_edits, edit_count
        registered_counts.append(bot.add_view.call_count)
        active_edits += 1
        edit_count += 1
        peak_edits = max(peak_edits, active_edits)
        if active_edits == duel_module._RECOVERY_EDIT_CONCURRENCY:
            concurrency_reached.set()
        await release_edits.wait()
        active_edits -= 1

    messages = {
        challenge.message_id: SimpleNamespace(edit=AsyncMock(side_effect=edit))
        for challenge in duel_service.list_pending_all.return_value
    }
    channel = bot.get_channel.return_value
    channel.get_partial_message.side_effect = messages.__getitem__

    setup_task = asyncio.create_task(setup(bot))
    try:
        await asyncio.wait_for(concurrency_reached.wait(), timeout=1)
        assert bot.add_view.call_count == challenge_count
        assert edit_count == duel_module._RECOVERY_EDIT_CONCURRENCY
        assert peak_edits == duel_module._RECOVERY_EDIT_CONCURRENCY
    finally:
        release_edits.set()
        await setup_task

    assert edit_count == challenge_count
    assert peak_edits <= duel_module._RECOVERY_EDIT_CONCURRENCY
    assert registered_counts == [challenge_count] * challenge_count
    channel.fetch_message.assert_not_awaited()


@pytest.mark.asyncio
async def test_setup_deduplicates_cold_channel_resolution(bot, duel_service):
    duel_service.list_pending_all.return_value = [
        make_challenge(challenge_id=index, message_id=300 + index)
        for index in range(3)
    ]
    messages = {
        challenge.message_id: SimpleNamespace(edit=AsyncMock())
        for challenge in duel_service.list_pending_all.return_value
    }
    channel = bot.fetch_channel.return_value
    channel.get_partial_message.side_effect = messages.__getitem__
    bot.get_channel.return_value = None

    await setup(bot)

    bot.fetch_channel.assert_awaited_once_with(CHANNEL_ID)
    assert channel.get_partial_message.call_count == 3
    channel.fetch_message.assert_not_awaited()


@pytest.mark.asyncio
async def test_setup_isolates_bound_control_recovery_failures(bot, duel_service):
    duel_service.list_pending_all.return_value = [
        make_challenge(challenge_id=index, message_id=300 + index)
        for index in range(3)
    ]
    messages = {
        challenge.message_id: SimpleNamespace(edit=AsyncMock())
        for challenge in duel_service.list_pending_all.return_value
    }
    messages[301].edit.side_effect = discord.DiscordException("edit failed")
    channel = bot.get_channel.return_value
    channel.get_partial_message.side_effect = messages.__getitem__

    await setup(bot)

    messages[300].edit.assert_awaited_once()
    messages[301].edit.assert_awaited_once()
    messages[302].edit.assert_awaited_once()
    assert bot.add_view.call_count == 3


@pytest.mark.asyncio
async def test_setup_posts_and_binds_replacement_for_unbound_challenge(
    bot, duel_service, flavor_service, interaction
):
    replacement = SimpleNamespace(id=301, edit=AsyncMock())
    interaction.channel.send.return_value = replacement
    duel_service.list_pending_all.return_value = [
        make_challenge(challenge_id=8, message_id=None),
    ]

    await setup(bot)

    flavor_service.generate.assert_awaited_once_with(
        DuelFlavorEvent.ISSUED,
        GUILD_ID,
        ANY,
    )
    sent = interaction.channel.send.await_args.kwargs
    assert sent["content"] == f"<@{RECIPIENT_ID}>"
    assert sent.get("view") is None
    assert sent["allowed_mentions"].everyone is False
    assert sent["allowed_mentions"].roles is False
    assert [user.id for user in sent["allowed_mentions"].users] == [RECIPIENT_ID]
    assert sent["allowed_mentions"].replied_user is False
    duel_service.bind_message.assert_called_once_with(8, GUILD_ID, 301)
    duel_service.mark_delivery_failed.assert_not_called()
    assert bot.add_view.call_args.kwargs == {"message_id": 301}
    assert isinstance(bot.add_view.call_args.args[0], DuelChallengeView)
    replacement.edit.assert_awaited_once()
    assert isinstance(replacement.edit.await_args.kwargs["view"], DuelChallengeView)


@pytest.mark.asyncio
async def test_setup_refunds_when_replacement_delivery_fails(
    bot, duel_service, interaction
):
    duel_service.list_pending_all.return_value = [
        make_challenge(challenge_id=8, message_id=None),
    ]
    interaction.channel.send.side_effect = discord.DiscordException("no delivery")

    await setup(bot)

    duel_service.mark_delivery_failed.assert_called_once_with(
        8, GUILD_ID, CHALLENGER_ID
    )
    duel_service.bind_message.assert_not_called()
    bot.add_view.assert_not_called()


@pytest.mark.asyncio
async def test_setup_discards_replacement_when_another_process_binds_first(
    bot, duel_service, interaction
):
    replacement = SimpleNamespace(id=301, edit=AsyncMock(), delete=AsyncMock())
    interaction.channel.send.return_value = replacement
    duel_service.list_pending_all.return_value = [
        make_challenge(challenge_id=8, message_id=None),
    ]
    duel_service.bind_message.side_effect = ValueError("already bound")

    await setup(bot)

    replacement.delete.assert_awaited_once()
    duel_service.mark_delivery_failed.assert_not_called()
    bot.add_view.assert_not_called()


@pytest.mark.asyncio
async def test_setup_bind_error_deletes_replacement_and_refunds_unbound_challenge(
    bot, duel_service, interaction
):
    replacement = SimpleNamespace(id=301, edit=AsyncMock(), delete=AsyncMock())
    interaction.channel.send.return_value = replacement
    duel_service.list_pending_all.return_value = [
        make_challenge(challenge_id=8, message_id=None),
    ]
    duel_service.bind_message.side_effect = RuntimeError("database unavailable")

    await setup(bot)

    replacement.delete.assert_awaited_once()
    duel_service.mark_delivery_failed.assert_called_once_with(
        8, GUILD_ID, CHALLENGER_ID
    )
    bot.add_view.assert_not_called()


@pytest.mark.asyncio
async def test_setup_requires_both_duel_services(bot):
    del bot.duel_flavor_service

    with pytest.raises(RuntimeError, match="duel_flavor_service"):
        await setup(bot)
