"""Rare bonus events awarded after a completed dig."""

from __future__ import annotations

import asyncio
import logging
import random
from collections.abc import Callable, Sequence
from typing import Any

import discord

from config import TRIVIA_ANSWER_TIMEOUT_SECONDS
from services.trivia_questions import TriviaQuestion, generate_question
from utils.interaction_safety import safe_followup

logger = logging.getLogger("cama_bot.commands.dig.bonus_events")


def pick_dig_bonus(roll: float) -> str | None:
    """Map one random roll to at most one one-percent dig bonus."""
    if roll < 0.01:
        return "wheel"
    if roll < 0.02:
        return "package_deal"
    if roll < 0.03:
        return "trivia"
    return None


def choose_package_candidates(
    players: Sequence[Any],
    digger_id: int,
    *,
    sample: Callable[[Sequence[Any], int], list[Any]] = random.sample,
) -> list[Any]:
    """Choose four distinct active guild members other than the digger."""
    eligible = []
    seen_ids = set()
    for player in players:
        player_id = int(player.id)
        if player_id == digger_id or player_id in seen_ids:
            continue
        seen_ids.add(player_id)
        eligible.append(player)
    if len(eligible) < 4:
        return []
    return sample(eligible, 4)


class PackageDealView(discord.ui.View):
    """Four user-bound buttons that settle a free three-game package deal."""

    def __init__(
        self,
        *,
        buyer_id: int,
        guild_id: int,
        candidates: Sequence[Any],
        package_deal_service: Any,
    ) -> None:
        super().__init__(timeout=60.0)
        self.buyer_id = buyer_id
        self.guild_id = guild_id
        self.package_deal_service = package_deal_service
        self.resolved = False
        self.message: discord.Message | None = None

        for candidate in candidates:
            button = discord.ui.Button(
                label=str(candidate.display_name)[:80],
                style=discord.ButtonStyle.primary,
                custom_id=f"dig_package_{buyer_id}_{candidate.id}",
            )

            async def callback(
                interaction: discord.Interaction,
                selected: Any = candidate,
            ) -> None:
                await self.select_partner(interaction, selected)

            button.callback = callback
            self.add_item(button)

    async def interaction_check(self, interaction: discord.Interaction) -> bool:
        return interaction.user.id == self.buyer_id

    async def on_timeout(self) -> None:
        if self.resolved:
            return
        for child in self.children:
            if isinstance(child, discord.ui.Button):
                child.disabled = True
        if self.message is not None:
            try:
                await self.message.edit(
                    content="*The mini Package Deal expired.*",
                    embed=None,
                    view=self,
                )
            except (discord.NotFound, discord.HTTPException):
                pass

    async def select_partner(
        self, interaction: discord.Interaction, candidate: Any,
    ) -> None:
        if self.resolved:
            await interaction.response.defer()
            return
        self.resolved = True
        self.stop()

        try:
            deal = await asyncio.to_thread(
                self.package_deal_service.create_or_extend_deal,
                guild_id=self.guild_id,
                buyer_id=self.buyer_id,
                partner_id=candidate.id,
                games=3,
                cost=0,
            )
        except Exception:
            logger.exception("Failed to activate dig package deal bonus")
            try:
                await interaction.response.edit_message(
                    content="The package deal could not be activated. Try again later.",
                    embed=None,
                    view=None,
                )
            except (discord.NotFound, discord.HTTPException):
                pass
            return

        try:
            await interaction.response.edit_message(
                content=(
                    f"Mini Package Deal active with **{candidate.display_name}**: "
                    f"**3 games added**, **{deal.games_remaining} games remaining**."
                ),
                embed=None,
                view=None,
            )
        except (discord.NotFound, discord.HTTPException):
            pass


class DigTriviaView(discord.ui.View):
    """A single Dota trivia question with fixed zero-random-guess-EV stakes."""

    def __init__(
        self,
        *,
        user_id: int,
        guild_id: int,
        question: TriviaQuestion,
        player_service: Any,
    ) -> None:
        super().__init__(timeout=TRIVIA_ANSWER_TIMEOUT_SECONDS)
        self.user_id = user_id
        self.guild_id = guild_id
        self.question = question
        self.player_service = player_service
        self.resolved = False
        self.message: discord.Message | None = None

        for index, option in enumerate(question.options):
            button = discord.ui.Button(
                label=f"{'ABCD'[index]}. {option}"[:80],
                style=discord.ButtonStyle.primary,
                custom_id=f"dig_trivia_{user_id}_{index}",
                row=index // 2,
            )

            async def callback(
                interaction: discord.Interaction,
                choice_index: int = index,
            ) -> None:
                await self.answer(interaction, choice_index)

            button.callback = callback
            self.add_item(button)

    async def interaction_check(self, interaction: discord.Interaction) -> bool:
        return interaction.user.id == self.user_id

    def _result_embed(self, *, correct: bool, timed_out: bool) -> discord.Embed:
        correct_answer = self.question.options[self.question.correct_index]
        if timed_out:
            title = "Dig Trivia — Time's up! -5 JC"
        elif correct:
            title = "Dig Trivia — Correct! +15 JC"
        else:
            title = "Dig Trivia — Wrong! -5 JC"

        description = f"The correct answer was **{correct_answer}**."
        if self.question.explanation:
            description += f"\n\n*{self.question.explanation}*"
        return discord.Embed(
            title=title,
            description=description,
            color=0x43A047 if correct else 0xE53935,
        )

    @staticmethod
    def _settlement_error_embed() -> discord.Embed:
        return discord.Embed(
            title="Dig Trivia — Settlement Error",
            description=(
                "The JC result could not be confirmed. Please contact an admin "
                "to verify the balance ledger."
            ),
            color=0xE53935,
        )

    async def _settle(self, *, correct: bool, timed_out: bool) -> discord.Embed:
        delta = 15 if correct else -5
        await asyncio.to_thread(
            self.player_service.adjust_balance,
            self.user_id,
            self.guild_id,
            delta,
            source="dig",
            actor_id=self.user_id,
            related_type="dig_bonus_trivia",
            related_id=self.question.category,
            reason=(
                "dig bonus trivia correct answer"
                if correct
                else "dig bonus trivia wrong answer"
            ),
            metadata={"correct": correct, "timed_out": timed_out},
        )
        return self._result_embed(correct=correct, timed_out=timed_out)

    async def answer(
        self, interaction: discord.Interaction, choice_index: int,
    ) -> None:
        if self.resolved:
            await interaction.response.defer()
            return
        self.resolved = True
        self.stop()
        correct = choice_index == self.question.correct_index
        try:
            embed = await self._settle(correct=correct, timed_out=False)
        except Exception:
            logger.exception("Failed to settle dig trivia answer")
            embed = self._settlement_error_embed()
        try:
            await interaction.response.edit_message(
                embed=embed,
                view=None,
                attachments=[],
            )
        except (discord.NotFound, discord.HTTPException):
            pass

    async def on_timeout(self) -> None:
        if self.resolved:
            return
        self.resolved = True
        self.stop()
        try:
            embed = await self._settle(correct=False, timed_out=True)
        except Exception:
            logger.exception("Failed to settle dig trivia timeout")
            embed = self._settlement_error_embed()
        if self.message is not None:
            try:
                await self.message.edit(embed=embed, view=None, attachments=[])
            except (discord.NotFound, discord.HTTPException):
                pass


def _question_embed(question: TriviaQuestion) -> discord.Embed:
    embed = discord.Embed(
        title="Dig Bonus — Dota 2 Trivia",
        description=question.text,
        color=0x5865F2,
    )
    embed.add_field(
        name="Choose one",
        value="\n".join(
            f"**{'ABCD'[index]}.** {option}"
            for index, option in enumerate(question.options)
        ),
        inline=False,
    )
    if question.image_url:
        embed.set_thumbnail(url=question.image_url)
    embed.set_footer(
        text=(
            f"+15 JC correct • -5 JC wrong or timeout • "
            f"{TRIVIA_ANSWER_TIMEOUT_SECONDS}s"
        ),
    )
    return embed


async def send_dig_bonus(
    bot: Any, interaction: discord.Interaction, bonus: str,
) -> None:
    """Present and settle one already-selected dig bonus."""
    if bonus == "wheel":
        betting_cog = bot.get_cog("BettingCommands")
        if betting_cog is None:
            raise RuntimeError("BettingCommands cog is unavailable")
        await betting_cog._gamba_action(interaction, bonus_spin=True)
        return

    if bonus == "package_deal":
        guild_id = interaction.guild.id
        rows = await asyncio.to_thread(
            bot.player_service.get_all_registered_players_for_lottery,
            guild_id,
        )
        members = []
        for row in rows:
            member = interaction.guild.get_member(int(row["discord_id"]))
            if member is not None and not getattr(member, "bot", False):
                members.append(member)
        candidates = choose_package_candidates(members, interaction.user.id)
        if not candidates:
            await safe_followup(
                interaction,
                content="A mini Package Deal appeared, but there were not four eligible active players.",
                ephemeral=True,
            )
            return

        view = PackageDealView(
            buyer_id=interaction.user.id,
            guild_id=guild_id,
            candidates=candidates,
            package_deal_service=bot.package_deal_service,
        )
        embed = discord.Embed(
            title="Dig Bonus — Mini Package Deal",
            description=(
                "Choose one active player. Your pick creates or extends a "
                "free Package Deal for **3 games**."
            ),
            color=0xF1C40F,
        )
        view.message = await safe_followup(interaction, embed=embed, view=view)
        return

    if bonus == "trivia":
        question = await asyncio.to_thread(generate_question, 0, [])
        if question is None:
            raise RuntimeError("No Dota trivia question could be generated")
        view = DigTriviaView(
            user_id=interaction.user.id,
            guild_id=interaction.guild.id,
            question=question,
            player_service=bot.player_service,
        )
        message = await safe_followup(
            interaction,
            embed=_question_embed(question),
            view=view,
        )
        view.message = message
        return

    raise ValueError(f"Unknown dig bonus: {bonus}")


async def maybe_send_dig_bonus(
    bot: Any,
    interaction: discord.Interaction,
    result: Any,
    *,
    roll: float | None = None,
) -> None:
    """Roll and dispatch one bonus after a result that consumed a dig."""
    if isinstance(result, dict):
        dig_consumed = bool(result.get("dig_consumed", False))
    else:
        dig_consumed = bool(getattr(result, "dig_consumed", False))
    if not dig_consumed:
        return

    bonus = pick_dig_bonus(random.random() if roll is None else roll)
    if bonus is None:
        return
    try:
        await send_dig_bonus(bot, interaction, bonus)
    except Exception:
        logger.exception("Failed to dispatch %s dig bonus", bonus)
        try:
            await safe_followup(
                interaction,
                content=(
                    "The bonus could not be displayed completely. Any JC change "
                    "may already be recorded in the ledger."
                ),
                ephemeral=True,
            )
        except Exception:
            logger.exception("Failed to report %s dig bonus failure", bonus)
