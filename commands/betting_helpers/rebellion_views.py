"""Discord UI views and modals for the Wheel War (`/incite`) flow."""

from __future__ import annotations

import asyncio
import logging
import time

import discord

from config import (
    MAX_DEBT,
    REBELLION_DEFENDER_STAKE,
    REBELLION_META_BET_MAX,
    REBELLION_VOTE_WINDOW_SECONDS,
)

logger = logging.getLogger("cama_bot.commands.betting")


class RebellionVoteView(discord.ui.View):
    """15-minute vote view for the Wheel War rebellion."""

    def __init__(self, war_id: int, guild_id: int, inciter_id: int, rebellion_service, *, timeout: float = 900.0):
        super().__init__(timeout=timeout)
        self.war_id = war_id
        self.guild_id = guild_id
        self.inciter_id = inciter_id
        self.rebellion_service = rebellion_service
        self.message: discord.Message | None = None

    def build_embed(self, effective_attack: float, effective_defend: float, attack_voter_count: int, defend_voter_count: int, inciter_name: str) -> discord.Embed:
        from config import REBELLION_ATTACK_QUORUM
        embed = discord.Embed(
            title="⚔️ REBELLION AGAINST THE WHEEL ⚔️",
            description=(
                f"**{inciter_name}** has had enough of the Wheel's tyranny and calls the people to arms!\n\n"
                f"The Wheel has oppressed the gamblers long enough. Will the realm rise?\n\n"
                f"**⚔️ ATTACK** — Free. Join the rebellion.\n"
                f"**🛡️ DEFEND** — Costs **{REBELLION_DEFENDER_STAKE} JC**. Defend the Wheel's honor.\n\n"
                f"*Veteran rebels (2+ bankruptcies) count as 1.5 votes.*\n\n"
                f"⏱️ Vote window: {REBELLION_VOTE_WINDOW_SECONDS // 60} minutes\n"
                f"Quorum needed: **{REBELLION_ATTACK_QUORUM} effective ATTACK votes** with more ATTACK than DEFEND"
            ),
            color=discord.Color.from_str("#8b0000"),
        )
        embed.add_field(
            name="⚔️ ATTACK",
            value=f"{attack_voter_count} rebels ({effective_attack:.1f} effective votes)",
            inline=True,
        )
        embed.add_field(
            name="🛡️ DEFEND",
            value=f"{defend_voter_count} defenders ({effective_defend:.1f} effective votes)",
            inline=True,
        )
        return embed

    @discord.ui.button(label="⚔️ ATTACK", style=discord.ButtonStyle.danger, custom_id="rebellion:attack")
    async def attack_button(self, interaction: discord.Interaction, button: discord.ui.Button):
        result = await asyncio.to_thread(
            self.rebellion_service.process_attack_vote,
            self.war_id, interaction.user.id, self.guild_id,
        )
        if not result["success"]:
            await interaction.response.send_message(result["message"], ephemeral=True)
            return
        if result.get("duplicate"):
            await interaction.response.send_message("You already voted ATTACK, warrior.", ephemeral=True)
            return
        veteran_note = " *(Veteran Rebel — 1.5 votes!)*" if result.get("is_veteran") else ""
        await interaction.response.send_message(
            f"⚔️ You join the rebellion!{veteran_note}", ephemeral=True
        )
        await self._refresh_embed()

    @discord.ui.button(label="🛡️ DEFEND (10 JC)", style=discord.ButtonStyle.primary, custom_id="rebellion:defend")
    async def defend_button(self, interaction: discord.Interaction, button: discord.ui.Button):
        result = await asyncio.to_thread(
            self.rebellion_service.process_defend_vote,
            self.war_id, interaction.user.id, self.guild_id,
        )
        if not result["success"]:
            await interaction.response.send_message(result["message"], ephemeral=True)
            return
        if result.get("duplicate"):
            await interaction.response.send_message("You already pledged your sword to the Wheel.", ephemeral=True)
            return
        await interaction.response.send_message(
            f"🛡️ You stake **{REBELLION_DEFENDER_STAKE} JC** to defend the Wheel!", ephemeral=True
        )
        await self._refresh_embed()

    async def _refresh_embed(self):
        if not self.message:
            return
        try:
            import json
            war = await asyncio.to_thread(self.rebellion_service.rebellion_repo.get_war, self.war_id)
            if not war:
                return
            attack_voters = json.loads(war["attack_voter_ids"])
            defend_voters = json.loads(war["defend_voter_ids"])
            # Get inciter name from first attack voter (the inciter)
            embed = self.build_embed(
                effective_attack=war["effective_attack_count"],
                effective_defend=war["effective_defend_count"],
                attack_voter_count=len(attack_voters),
                defend_voter_count=len(defend_voters),
                inciter_name=f"<@{war['inciter_id']}>",
            )
            await self.message.edit(embed=embed)
        except Exception as e:
            logger.debug(f"RebellionVoteView embed refresh error: {e}")


class WarBetAmountModal(discord.ui.Modal):
    """Modal for entering meta-bet amount during a wheel war."""

    amount = discord.ui.TextInput(
        label="Bet Amount (1–50 JC)",
        placeholder="e.g., 25",
        min_length=1,
        max_length=3,
        required=True,
    )

    def __init__(self, war_id: int, guild_id: int, side: str, rebellion_service, player_service):
        super().__init__(title=f"Bet on {'REBELS ⚔️' if side == 'rebels' else 'THE WHEEL ⚙️'}")
        self.war_id = war_id
        self.guild_id = guild_id
        self.side = side
        self.rebellion_service = rebellion_service
        self.player_service = player_service

    async def on_submit(self, interaction: discord.Interaction):
        try:
            bet_amount = int(self.amount.value)
        except ValueError:
            await interaction.response.send_message("Enter a number between 1 and 50.", ephemeral=True)
            return

        if bet_amount < 1 or bet_amount > REBELLION_META_BET_MAX:
            await interaction.response.send_message(
                f"Bet must be between 1 and {REBELLION_META_BET_MAX} JC.", ephemeral=True
            )
            return

        try:
            await asyncio.to_thread(
                self.rebellion_service.rebellion_repo.place_meta_bet_atomic,
                self.war_id,
                self.guild_id,
                interaction.user.id,
                self.side,
                bet_amount,
                int(time.time()),
                MAX_DEBT,
            )
            side_name = "REBELS ⚔️" if self.side == "rebels" else "THE WHEEL ⚙️"
            await interaction.response.send_message(
                f"**{bet_amount} JC** wagered on **{side_name}**! May fortune favor the bold.",
                ephemeral=True,
            )
        except ValueError as e:
            await interaction.response.send_message(str(e), ephemeral=True)
        except Exception as e:
            logger.error(f"Meta-bet placement error: {e}")
            await interaction.response.send_message("Failed to place bet. Try again.", ephemeral=True)


class WarBetView(discord.ui.View):
    """2-minute meta-betting view during a declared wheel war."""

    def __init__(self, war_id: int, guild_id: int, rebellion_service, player_service, *, timeout: float = 120.0):
        super().__init__(timeout=timeout)
        self.war_id = war_id
        self.guild_id = guild_id
        self.rebellion_service = rebellion_service
        self.player_service = player_service

    @discord.ui.button(label="⚔️ Bet REBELS (1–50 JC)", style=discord.ButtonStyle.danger)
    async def bet_rebels(self, interaction: discord.Interaction, button: discord.ui.Button):
        modal = WarBetAmountModal(
            war_id=self.war_id,
            guild_id=self.guild_id,
            side="rebels",
            rebellion_service=self.rebellion_service,
            player_service=self.player_service,
        )
        await interaction.response.send_modal(modal)

    @discord.ui.button(label="⚙️ Bet WHEEL (1–50 JC)", style=discord.ButtonStyle.primary)
    async def bet_wheel(self, interaction: discord.Interaction, button: discord.ui.Button):
        modal = WarBetAmountModal(
            war_id=self.war_id,
            guild_id=self.guild_id,
            side="wheel",
            rebellion_service=self.rebellion_service,
            player_service=self.player_service,
        )
        await interaction.response.send_modal(modal)
