"""Slash commands and persistent controls for duel challenges."""

from __future__ import annotations

import logging
from collections.abc import Iterable

import discord
from discord import app_commands
from discord.ext import commands

from domain.models.duel import (
    DuelChallenge,
    DuelDueKind,
    DuelDueResult,
    DuelStatus,
    DuelTrial,
)
from services.duel_flavor_service import DuelFlavorEvent
from services.permissions import has_admin_permission
from utils.interaction_safety import safe_defer, safe_followup

logger = logging.getLogger("cama_bot.commands.duel")

TRIAL_DETAILS = {
    DuelTrial.TRIAL_BY_COMBAT: (
        "Trial by Combat: best-of-three, one-versus-one Dota mid."
    ),
    DuelTrial.TRIAL_OF_FIVE: "Trial of Five: a lobby using Immortal Draft.",
}


class DuelResponseButton(discord.ui.Button):
    """A durable response button tied to one challenge and action."""

    def __init__(
        self,
        challenge_id: int,
        choice: str,
        *,
        label: str,
        style: discord.ButtonStyle,
        emoji: str,
    ) -> None:
        super().__init__(
            label=label,
            style=style,
            emoji=emoji,
            custom_id=f"duel:{challenge_id}:{choice}",
        )
        self.challenge_id = challenge_id
        self.choice = choice

    async def callback(self, interaction: discord.Interaction) -> None:
        view = self.view
        if not isinstance(view, DuelChallengeView):
            return
        await view.cog.handle_response(interaction, self.challenge_id, self.choice)


class DuelChallengeView(discord.ui.View):
    """Persistent response controls for a pending duel challenge."""

    def __init__(self, cog: DuelCommands, challenge_id: int) -> None:
        super().__init__(timeout=None)
        self.cog = cog
        self.add_item(
            DuelResponseButton(
                challenge_id,
                "decline",
                label="Decline in Cowardice",
                style=discord.ButtonStyle.danger,
                emoji="\U0001f3f3\ufe0f",
            )
        )
        self.add_item(
            DuelResponseButton(
                challenge_id,
                "trial_by_combat",
                label="Trial by Combat",
                style=discord.ButtonStyle.primary,
                emoji="\u2694\ufe0f",
            )
        )
        self.add_item(
            DuelResponseButton(
                challenge_id,
                "trial_of_five",
                label="Trial of Five",
                style=discord.ButtonStyle.success,
                emoji="\U0001f6e1\ufe0f",
            )
        )


class DuelCommands(commands.Cog):
    """Discord-facing duel challenge lifecycle."""

    duel = app_commands.Group(name="duel", description="Challenges of honor")

    def __init__(self, bot, duel_service, flavor_service) -> None:
        self.bot = bot
        self.duel_service = duel_service
        self.flavor_service = flavor_service

    @duel.command(name="issue", description="Challenge a player to a duel of honor")
    async def issue(
        self,
        interaction: discord.Interaction,
        player: discord.Member,
        wager: app_commands.Range[int, 500, 1000],
    ) -> None:
        if interaction.guild is None or interaction.channel_id is None:
            await self._send_immediate_error(
                interaction, "This command must be used in a server."
            )
            return
        if not await safe_defer(interaction):
            return

        guild_id = interaction.guild.id
        actor_id = interaction.user.id
        try:
            challenge = self.duel_service.issue(
                guild_id,
                interaction.channel_id,
                actor_id,
                player.id,
                wager,
                recipient_is_bot=player.bot,
            )
        except ValueError as exc:
            await self._send_deferred_error(interaction, str(exc))
            return

        flavor = await self.flavor_service.generate(
            DuelFlavorEvent.ISSUED,
            guild_id,
            self._flavor_details(challenge),
        )
        embed = self.build_challenge_embed(challenge, flavor)
        allowed_mentions = discord.AllowedMentions(
            everyone=False,
            roles=False,
            users=[player],
            replied_user=False,
        )
        try:
            message = await safe_followup(
                interaction,
                content=player.mention,
                embed=embed,
                allowed_mentions=allowed_mentions,
            )
            if message is None:
                raise discord.DiscordException("Initial duel message was not delivered.")
        except Exception:
            await self._refund_failed_delivery(
                interaction,
                challenge,
                guild_id,
                actor_id,
            )
            return

        try:
            self.duel_service.bind_message(
                challenge.challenge_id,
                guild_id,
                message.id,
            )
        except Exception:
            logger.exception("Unable to bind delivered duel challenge message")
            await self._refund_failed_delivery(
                interaction,
                challenge,
                guild_id,
                actor_id,
            )
            return

        try:
            await message.edit(
                view=DuelChallengeView(self, challenge.challenge_id),
                allowed_mentions=allowed_mentions,
            )
        except discord.DiscordException:
            logger.exception("Unable to attach duel challenge response controls")

    @duel.command(name="respond", description="Answer your pending duel challenge")
    @app_commands.choices(
        choice=[
            app_commands.Choice(name="Decline in Cowardice", value="decline"),
            app_commands.Choice(name="Trial by Combat", value="trial_by_combat"),
            app_commands.Choice(name="Trial of Five", value="trial_of_five"),
        ]
    )
    async def respond(self, interaction: discord.Interaction, choice: str) -> None:
        await self.handle_response(interaction, None, choice)

    @duel.command(name="list", description="List outstanding duel challenges")
    async def list(self, interaction: discord.Interaction) -> None:
        if interaction.guild is None:
            await self._send_immediate_error(
                interaction, "This command must be used in a server."
            )
            return
        if not await safe_defer(interaction):
            return

        challenges = self.duel_service.list_outstanding(interaction.guild.id)
        if not challenges:
            await safe_followup(
                interaction,
                content="No pending or accepted duel challenges were found.",
                ephemeral=True,
                allowed_mentions=discord.AllowedMentions.none(),
            )
            return

        pages = list(self._chunks(challenges, 25))
        for page_number, page in enumerate(pages, start=1):
            embed = discord.Embed(
                title="Outstanding Duel Challenges",
                color=discord.Color.gold(),
            )
            if len(pages) > 1:
                embed.set_footer(text=f"Page {page_number} of {len(pages)}")
            for challenge in page:
                trial = (
                    f" • {self._trial_label(challenge.trial_type)}"
                    if challenge.trial_type is not None
                    else f" • responds <t:{challenge.expires_at}:R>"
                )
                embed.add_field(
                    name=f"#{challenge.challenge_id} • {self._status_label(challenge.status)}",
                    value=(
                        f"<@{challenge.challenger_id}> vs <@{challenge.recipient_id}>"
                        f" • {challenge.wager} JC{trial}"
                    ),
                    inline=False,
                )
            await safe_followup(
                interaction,
                embed=embed,
                allowed_mentions=discord.AllowedMentions.none(),
            )

    @duel.command(name="resolve", description="Resolve an accepted duel challenge")
    @app_commands.choices(
        outcome=[
            app_commands.Choice(
                name="Challenger Victory", value="challenger_victory"
            ),
            app_commands.Choice(
                name="Recipient Victory", value="recipient_victory"
            ),
            app_commands.Choice(name="Void", value="void"),
        ]
    )
    async def resolve(
        self,
        interaction: discord.Interaction,
        challenge_id: int,
        outcome: str,
    ) -> None:
        if interaction.guild is None:
            await self._send_immediate_error(
                interaction, "This command must be used in a server."
            )
            return
        if not has_admin_permission(interaction):
            await self._send_immediate_error(
                interaction, "You do not have permission to resolve duel challenges."
            )
            return
        if not await safe_defer(interaction):
            return

        guild_id = interaction.guild.id
        try:
            challenge = self.duel_service.resolve(
                guild_id,
                interaction.user.id,
                challenge_id,
                outcome,
            )
        except ValueError as exc:
            await self._send_deferred_error(interaction, str(exc))
            return

        event = (
            DuelFlavorEvent.VOIDED
            if challenge.status is DuelStatus.VOIDED
            else DuelFlavorEvent.RESOLVED
        )
        flavor = await self.flavor_service.generate(
            event,
            guild_id,
            self._flavor_details(challenge),
        )
        await self._edit_original(challenge, flavor)
        if challenge.status is DuelStatus.VOIDED:
            detail = (
                f"Challenge #{challenge.challenge_id} was voided. "
                f"The {challenge.wager} JC stakes were refunded to each player; "
                "the issuance fee was not refunded."
            )
        else:
            detail = (
                f"Challenge #{challenge.challenge_id} is resolved: "
                f"<@{challenge.winner_id}> wins {challenge.wager * 2} JC."
            )
        await safe_followup(
            interaction,
            content=f"{flavor}\n{detail}",
            allowed_mentions=discord.AllowedMentions.none(),
        )

    async def handle_response(
        self,
        interaction: discord.Interaction,
        challenge_id: int | None,
        choice: str,
    ) -> None:
        """Run command and button responses through one guarded path."""
        if interaction.guild is None:
            await self._send_immediate_error(
                interaction, "This command must be used in a server."
            )
            return

        guild_id = interaction.guild.id
        if challenge_id is not None:
            pending = next(
                (
                    challenge
                    for challenge in self.duel_service.list_outstanding(guild_id)
                    if challenge.guild_id == guild_id
                    and challenge.status is DuelStatus.PENDING
                    and challenge.recipient_id == interaction.user.id
                ),
                None,
            )
            if pending is None or pending.challenge_id != challenge_id:
                await self._send_immediate_error(
                    interaction,
                    "This duel challenge is unavailable or is not yours to answer.",
                )
                return

        if not await safe_defer(interaction):
            return
        try:
            challenge = self.duel_service.respond(
                guild_id,
                interaction.user.id,
                choice,
            )
        except ValueError as exc:
            await self._send_deferred_error(interaction, str(exc))
            return

        event = self._response_event(challenge)
        flavor = await self.flavor_service.generate(
            event,
            guild_id,
            self._flavor_details(challenge),
        )
        await self._edit_original(challenge, flavor)
        if challenge.status is DuelStatus.DECLINED:
            detail = (
                f"Challenge #{challenge.challenge_id} was declined. "
                f"The {challenge.decline_penalty} JC penalty was paid to the challenger."
            )
        else:
            detail = TRIAL_DETAILS[challenge.trial_type]
        await safe_followup(
            interaction,
            content=f"{flavor}\n{detail}",
            allowed_mentions=discord.AllowedMentions.none(),
        )

    async def deliver_due_result(self, result: DuelDueResult) -> None:
        """Deliver an already-claimed reminder or expiry result."""
        challenge = result.challenge
        event = (
            DuelFlavorEvent.REMINDER
            if result.kind is DuelDueKind.REMINDER
            else DuelFlavorEvent.EXPIRED
        )
        flavor = await self.flavor_service.generate(
            event,
            challenge.guild_id,
            self._flavor_details(challenge),
        )
        channel = await self._get_channel(challenge.channel_id)
        if channel is None:
            return

        if result.kind is DuelDueKind.EXPIRED:
            await self._edit_original(challenge, flavor)
            content = (
                f"{flavor}\nChallenge #{challenge.challenge_id} expired unanswered. "
                f"The {challenge.decline_penalty} JC penalty was paid to the challenger."
            )
            allowed_mentions = discord.AllowedMentions.none()
        else:
            mention = f"<@{challenge.recipient_id}>" if result.ping_recipient else ""
            content = (
                f"{mention}\n{flavor}\nChallenge #{challenge.challenge_id} "
                f"expires <t:{challenge.expires_at}:R>."
            ).strip()
            allowed_mentions = (
                discord.AllowedMentions(
                    everyone=False,
                    roles=False,
                    users=[discord.Object(id=challenge.recipient_id)],
                    replied_user=False,
                )
                if result.ping_recipient
                else discord.AllowedMentions.none()
            )
        try:
            await channel.send(
                content=content,
                allowed_mentions=allowed_mentions,
            )
        except discord.DiscordException:
            logger.exception("Unable to deliver duel lifecycle message")

    def build_challenge_embed(
        self,
        challenge: DuelChallenge,
        flavor: str,
    ) -> discord.Embed:
        """Render the durable mechanical state of a challenge."""
        color = {
            DuelStatus.PENDING: discord.Color.gold(),
            DuelStatus.ACCEPTED: discord.Color.blue(),
            DuelStatus.DECLINED: discord.Color.red(),
            DuelStatus.EXPIRED: discord.Color.dark_grey(),
            DuelStatus.RESOLVED: discord.Color.green(),
            DuelStatus.VOIDED: discord.Color.dark_grey(),
            DuelStatus.DELIVERY_FAILED: discord.Color.dark_grey(),
        }[challenge.status]
        embed = discord.Embed(
            title="Challenge of Honor",
            description=flavor,
            color=color,
        )
        embed.add_field(name="Challenge", value=f"#{challenge.challenge_id}")
        embed.add_field(
            name="Status", value=self._status_label(challenge.status)
        )
        embed.add_field(
            name="Wager", value=f"{challenge.wager} JC"
        )
        embed.add_field(
            name="Issuance Fee",
            value=(
                f"{challenge.issuance_fee} JC — nonrefundable after delivery"
            ),
        )
        embed.add_field(
            name="Challenger", value=f"<@{challenge.challenger_id}>"
        )
        embed.add_field(
            name="Challenger Rating",
            value=f"{challenge.challenger_glicko:.0f} ± {challenge.challenger_rd:.0f}",
        )
        embed.add_field(
            name="Recipient", value=f"<@{challenge.recipient_id}>"
        )
        embed.add_field(
            name="Recipient Rating",
            value=f"{challenge.recipient_glicko:.0f} ± {challenge.recipient_rd:.0f}",
        )
        embed.add_field(
            name="Decline Penalty", value=f"{challenge.decline_penalty} JC"
        )

        if challenge.status is DuelStatus.PENDING:
            embed.add_field(
                name="Response Deadline",
                value=f"<t:{challenge.expires_at}:R>",
            )
            embed.add_field(
                name="Trials",
                value=(
                    "Trial by Combat — best-of-three, one-versus-one Dota mid\n"
                    "Trial of Five — a lobby using Immortal Draft"
                ),
                inline=False,
            )
        if challenge.trial_type is not None:
            embed.add_field(
                name="Trial",
                value=self._trial_label(challenge.trial_type),
            )
        if challenge.status is DuelStatus.RESOLVED:
            embed.add_field(name="Winner", value=f"<@{challenge.winner_id}>")
        if challenge.status is DuelStatus.VOIDED:
            embed.add_field(
                name="Refund",
                value=(
                    f"{challenge.wager} JC stake to each player; "
                    "issuance fee remains nonrefundable"
                ),
            )
        return embed

    async def _edit_original(self, challenge: DuelChallenge, flavor: str) -> None:
        if challenge.message_id is None:
            return
        channel = await self._get_channel(challenge.channel_id)
        if channel is None:
            return
        try:
            message = await channel.fetch_message(challenge.message_id)
            await message.edit(
                embed=self.build_challenge_embed(challenge, flavor),
                view=None,
                allowed_mentions=discord.AllowedMentions.none(),
            )
        except discord.DiscordException:
            logger.exception("Unable to update duel challenge message")

    async def _get_channel(self, channel_id: int):
        channel = self.bot.get_channel(channel_id)
        if channel is not None:
            return channel
        try:
            return await self.bot.fetch_channel(channel_id)
        except discord.DiscordException:
            logger.exception("Unable to fetch duel challenge channel")
            return None

    @staticmethod
    async def _send_immediate_error(
        interaction: discord.Interaction,
        message: str,
    ) -> None:
        await interaction.response.send_message(
            message,
            ephemeral=True,
            allowed_mentions=discord.AllowedMentions.none(),
        )

    @staticmethod
    async def _send_deferred_error(
        interaction: discord.Interaction,
        message: str,
    ) -> None:
        await safe_followup(
            interaction,
            content=message,
            ephemeral=True,
            allowed_mentions=discord.AllowedMentions.none(),
        )

    async def _refund_failed_delivery(
        self,
        interaction: discord.Interaction,
        challenge: DuelChallenge,
        guild_id: int,
        actor_id: int,
    ) -> None:
        self.duel_service.mark_delivery_failed(
            challenge.challenge_id,
            guild_id,
            actor_id,
        )
        try:
            await interaction.followup.send(
                content=(
                    "The challenge could not be delivered; the wager and "
                    f"{challenge.issuance_fee} JC issuance fee were refunded."
                ),
                ephemeral=True,
                allowed_mentions=discord.AllowedMentions.none(),
            )
        except discord.DiscordException:
            logger.exception("Unable to report failed duel delivery")

    @staticmethod
    def _response_event(challenge: DuelChallenge) -> DuelFlavorEvent:
        if challenge.status is DuelStatus.DECLINED:
            return DuelFlavorEvent.DECLINED
        if challenge.trial_type is DuelTrial.TRIAL_BY_COMBAT:
            return DuelFlavorEvent.ACCEPTED_COMBAT
        return DuelFlavorEvent.ACCEPTED_FIVE

    @staticmethod
    def _flavor_details(challenge: DuelChallenge) -> dict[str, object]:
        return {
            "challenge_id": challenge.challenge_id,
            "challenger_id": challenge.challenger_id,
            "recipient_id": challenge.recipient_id,
            "wager": challenge.wager,
            "issuance_fee": challenge.issuance_fee,
            "status": challenge.status.value,
            "trial": challenge.trial_type.value if challenge.trial_type else None,
        }

    @staticmethod
    def _status_label(status: DuelStatus) -> str:
        return status.value.replace("_", " ").title()

    @staticmethod
    def _trial_label(trial: DuelTrial) -> str:
        return {
            DuelTrial.TRIAL_BY_COMBAT: "Trial by Combat",
            DuelTrial.TRIAL_OF_FIVE: "Trial of Five",
        }[trial]

    @staticmethod
    def _chunks(
        values: list[DuelChallenge], size: int
    ) -> Iterable[list[DuelChallenge]]:
        for index in range(0, len(values), size):
            yield values[index : index + size]


async def setup(bot: commands.Bot) -> None:
    duel_service = getattr(bot, "duel_service", None)
    if duel_service is None:
        raise RuntimeError("duel_service must be initialized before commands.duel")
    flavor_service = getattr(bot, "duel_flavor_service", None)
    if flavor_service is None:
        raise RuntimeError(
            "duel_flavor_service must be initialized before commands.duel"
        )

    cog = DuelCommands(bot, duel_service, flavor_service)
    await bot.add_cog(cog)
    for challenge in duel_service.list_pending_all():
        if challenge.message_id is None:
            try:
                duel_service.mark_delivery_failed(
                    challenge.challenge_id,
                    challenge.guild_id,
                    challenge.challenger_id,
                )
            except ValueError:
                logger.info(
                    "Duel challenge %s changed before startup refund",
                    challenge.challenge_id,
                )
            continue

        view = DuelChallengeView(cog, challenge.challenge_id)
        bot.add_view(view, message_id=challenge.message_id)
        channel = await cog._get_channel(challenge.channel_id)
        if channel is None:
            continue
        try:
            message = await channel.fetch_message(challenge.message_id)
            await message.edit(
                view=view,
                allowed_mentions=discord.AllowedMentions.none(),
            )
        except discord.DiscordException:
            logger.exception(
                "Unable to restore controls for duel challenge %s",
                challenge.challenge_id,
            )
