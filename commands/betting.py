"""
Betting commands for jopacoin wagers.
"""

from __future__ import annotations

import asyncio
import functools
import logging
import math
import random
import time
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands

if TYPE_CHECKING:
    from services.flavor_text_service import FlavorTextService

from services.flavor_text_service import FlavorEvent

from config import (
    BANKRUPTCY_PENALTY_RATE,
    GARNISHMENT_PERCENTAGE,
    JOPACOIN_MIN_BET,
    LIGHTNING_BOLT_MIN_TAX,
    LIGHTNING_BOLT_PCT_MAX,
    LIGHTNING_BOLT_PCT_MIN,
    LOAN_FEE_RATE,
    MAX_DEBT,
    TIP_FEE_RATE,
    WHEEL_BANKRUPT_PENALTY,
    WHEEL_COOLDOWN_SECONDS,
    WHEEL_GOLDEN_TOP_N,
    WHEEL_LOSE_PENALTY_COOLDOWN,
    REBELLION_DEFENDER_STAKE,
    REBELLION_VOTE_WINDOW_SECONDS,
    REBELLION_META_BET_WINDOW_SECONDS,
    REBELLION_META_BET_MAX,
    REBELLION_RETRIBUTION_STEAL,
    REBELLION_GAMBA_COOLDOWN_PENALTY,
    REBELLION_FIZZLE_SPIN_MAX_WIN,
)
from config import DISBURSE_MIN_FUND
from services.bankruptcy_service import BankruptcyService
from services.betting_service import BettingService
from services.disburse_service import DisburseService
from services.gambling_stats_service import GamblingStatsService
from services.loan_service import LoanService
from services.match_service import MatchService
from services.permissions import has_admin_permission
from services.player_service import PlayerService
from services.tip_service import TipService
from commands.checks import require_gamba_channel
from utils.formatting import JOPACOIN_EMOTE, TOMBSTONE_EMOJI, format_betting_display
from utils.interaction_safety import safe_defer
from utils.neon_helpers import get_neon_service, send_neon_result
from utils.rate_limiter import GLOBAL_RATE_LIMITER
from utils.wheel_drawing import (
    WHEEL_WEDGES,
    GOLDEN_WHEEL_WEDGES,
    create_wheel_gif,
    create_explosion_gif,
    get_wedge_at_index,
    get_wheel_wedges,
    get_wedge_at_index_for_player,
    compute_live_golden_wedges,
    apply_war_effects,
    apply_mana_wedge,
)

# 1% chance for the wheel to explode
WHEEL_EXPLOSION_CHANCE = 0.01
WHEEL_EXPLOSION_REWARD = 67

logger = logging.getLogger("cama_bot.commands.betting")


# Snarky messages for those who don't deserve bankruptcy
BANKRUPTCY_DENIED_MESSAGES = [
    "You're not actually in debt. Nice try, freeloader.",
    "Bankruptcy is for degenerates who lost it all. You still have coins.",
    "You're trying to declare bankruptcy while being solvent? The audacity.",
    "ERROR: Wealth detected. Cannot process bankruptcy request.",
    "Your application for financial ruin has been denied. You're too rich.",
    "Sorry, this service is exclusively for people who made terrible decisions.",
    "The Jopacoin Bankruptcy Court rejects your attempt to game the system.",
    "Imagine trying to go bankrupt when you have money. Couldn't be you.",
]

BANKRUPTCY_COOLDOWN_MESSAGES = [
    "You already declared bankruptcy recently. The court isn't buying it again so soon.",
    "Nice try, but your credit score hasn't recovered from the last bankruptcy.",
    "The Jopacoin Financial Recovery Board says you need to wait longer.",
    "Bankruptcy addiction is real. Seek help. And try again later.",
    "One bankruptcy per week, please. We have standards.",
    "Your previous bankruptcy paperwork hasn't even finished processing yet.",
    "The judge remembers you. Come back when they've forgotten.",
]

BANKRUPTCY_SUCCESS_MESSAGES = [
    "Congratulations on your complete financial ruin. Your debt has been erased, but at what cost?",
    "The court has granted your bankruptcy. Your ancestors weep.",
    "Chapter 7 approved. Your jopacoin legacy dies here.",
    "Debt cleared. Dignity? Also cleared. You must WIN {games} games to escape low priority.",
    "The Jopacoin Federal Reserve takes note of another fallen gambler. Debt erased.",
    "Your bankruptcy filing has been accepted. The house always wins, but at least you don't owe it anymore.",
    "Financial rock bottom achieved. Welcome to the Bankruptcy Hall of Shame.",
    "Your debt of {debt} jopacoin has been forgiven. You're now starting from almost nothing. Again.",
]

LOAN_SUCCESS_MESSAGES = [
    "The bank approves your request. {amount} {emote} deposited. You now owe {owed}. Good luck.",
    "Money acquired. Dignity sacrificed. {amount} {emote} in, {owed} to repay. The cycle continues.",
    "Loan approved. {amount} {emote} hits your account. Don't spend it all in one bet. (You will.)",
    "The Jopacoin Lending Co. smiles upon you. {amount} {emote} granted. {fee} {emote} goes to charity.",
    "Fresh jopacoin, fresh start, same gambling addiction. {amount} {emote} received.",
]

LOAN_DENIED_COOLDOWN_MESSAGES = [
    "You just took a loan! The bank needs time to process your crippling debt.",
    "One loan every 3 days. We have to pretend we're responsible lenders.",
    "Your loan application is on cooldown. Maybe reflect on your choices.",
    "The Jopacoin Bank says: 'Come back later, we're still counting your last loan's fees.'",
]

LOAN_DENIED_DEBT_MESSAGES = [
    "You're already too deep in debt. Even we have standards.",
    "Loan denied. Your credit is worse than your gambling decisions.",
    "The bank has reviewed your finances and respectfully declined to make things worse.",
    "ERROR: Maximum debt capacity reached. Try bankruptcy first.",
]

# Special messages for peak degen behavior: taking a loan while already in debt
NEGATIVE_LOAN_MESSAGES = [
    "You... you took out a loan while already in debt. The money went straight to your creditors. "
    "You're now even MORE in debt. Congratulations, you absolute degenerate.",
    "LEGENDARY MOVE: Borrowing money just to owe MORE money. "
    "Your financial advisor has left the country. True degen behavior.",
    "The loan was approved and immediately garnished. You gained nothing but more debt and our respect. "
    "This is galaxy-brain degeneracy.",
    "You borrowed {amount} {emote} while broke. Net result: deeper in the hole. "
    "The degen energy radiating from this decision is immeasurable.",
    "This is advanced degeneracy. You can't even gamble with this money because you're still negative. "
    "But you did it anyway. We're impressed and horrified.",
]


GAMBA_GIF_URL = "https://tenor.com/view/uncut-gems-sports-betting-sports-acting-adam-sandler-gif-11474547316651780959"


def _wedge_ev(wedge: tuple) -> float:
    """Return a rough EV for a bankrupt wheel wedge (used to find 'worst' option)."""
    from utils.wheel_drawing import _SPECIAL_WEDGE_EST_EVS, _load_special_wedge_evs
    _load_special_wedge_evs()
    _, v, _ = wedge
    if isinstance(v, int):
        return float(v)
    return _SPECIAL_WEDGE_EST_EVS.get(v, 0.0)


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


class TownTrialView(discord.ui.View):
    """Server-wide vote view for the TOWN_TRIAL bankrupt wheel mechanic."""

    def __init__(self, options: list[tuple], *, timeout: float = 300.0):
        super().__init__(timeout=timeout)
        self.options = options
        self.votes: dict[int, int] = {}  # discord_id -> option index

        for i, (label, value, _color) in enumerate(options):
            display = label if isinstance(value, str) else f"{label} JC" if isinstance(value, int) and value > 0 else label
            button = discord.ui.Button(
                label=display,
                style=discord.ButtonStyle.secondary,
                custom_id=f"tt_{i}",
            )
            button.callback = self._make_callback(i)
            self.add_item(button)

    def _make_callback(self, idx: int):
        async def callback(interaction: discord.Interaction):
            self.votes[interaction.user.id] = idx
            label = self.options[idx][0]
            await interaction.response.send_message(
                f"Voted for **{label}**!", ephemeral=True
            )
        return callback

    def get_winner(self) -> int | None:
        """Return the winning option index, or None if no votes."""
        if not self.votes:
            return None
        from collections import Counter
        counts = Counter(self.votes.values())
        max_votes = max(counts.values())
        winners = [idx for idx, cnt in counts.items() if cnt == max_votes]
        return random.choice(winners)


class DiscoverView(discord.ui.View):
    """Spinner-choice view for the DISCOVER bankrupt wheel mechanic."""

    def __init__(self, options: list[tuple], spinner_id: int, *, timeout: float = 60.0):
        super().__init__(timeout=timeout)
        self.options = options
        self.spinner_id = spinner_id
        self.chosen_idx: int | None = None

        for i, (label, value, _color) in enumerate(options):
            display = label if isinstance(value, str) else f"{label} JC" if isinstance(value, int) and value > 0 else label
            button = discord.ui.Button(
                label=display,
                style=discord.ButtonStyle.secondary,
                custom_id=f"disc_{i}",
            )
            button.callback = self._make_callback(i)
            self.add_item(button)

    def _make_callback(self, idx: int):
        async def callback(interaction: discord.Interaction):
            if interaction.user.id != self.spinner_id:
                await interaction.response.send_message(
                    "This choice isn't yours to make.", ephemeral=True
                )
                return
            self.chosen_idx = idx
            self.stop()
            label = self.options[idx][0]
            await interaction.response.send_message(
                f"You chose **{label}**!", ephemeral=True
            )
        return callback


class ScryingView(discord.ui.View):
    """Blue mana scrying: choose between two wheel outcomes."""

    def __init__(self, option_a: str, option_b: str, user_id: int, **kwargs):
        super().__init__(**kwargs)
        self.option_a = option_a
        self.option_b = option_b
        self.user_id = user_id
        self.chosen: str | None = None
        # Update button labels
        self.children[0].label = f"A: {option_a}"
        self.children[1].label = f"B: {option_b}"

    @discord.ui.button(label="A", style=discord.ButtonStyle.primary)
    async def choose_a(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your scrying!", ephemeral=True)
            return
        self.chosen = "A"
        await interaction.response.defer()
        self.stop()

    @discord.ui.button(label="B", style=discord.ButtonStyle.primary)
    async def choose_b(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your scrying!", ephemeral=True)
            return
        self.chosen = "B"
        await interaction.response.defer()
        self.stop()


class BettingCommands(commands.Cog):
    """Slash commands to place and view wagers."""

    def __init__(
        self,
        bot: commands.Bot,
        betting_service: BettingService,
        match_service: MatchService,
        player_service: PlayerService,
        bankruptcy_service: BankruptcyService | None = None,
        gambling_stats_service: GamblingStatsService | None = None,
        loan_service: LoanService | None = None,
        disburse_service: DisburseService | None = None,
        flavor_text_service: FlavorTextService | None = None,
        tip_service: TipService | None = None,
        rebellion_service=None,
    ):
        self.bot = bot
        self.betting_service = betting_service
        self.match_service = match_service
        self.player_service = player_service
        self.bankruptcy_service = bankruptcy_service
        self.flavor_text_service = flavor_text_service
        self.gambling_stats_service = gambling_stats_service
        self.loan_service = loan_service
        self.disburse_service = disburse_service
        self.tip_service = tip_service
        self.rebellion_service = rebellion_service

    def _get_neon_service(self):
        """Get the NeonDegenService from the bot, or None if unavailable."""
        return get_neon_service(self.bot)

    async def match_autocomplete(
        self, interaction: discord.Interaction, current: str
    ) -> list[app_commands.Choice[int]]:
        """Autocomplete for pending match IDs."""
        guild_id = interaction.guild.id if interaction.guild else None
        try:
            pending = await asyncio.to_thread(
                self.match_service.state_service.get_all_pending_matches, guild_id
            )
        except Exception:
            return []

        if not pending:
            return []

        choices = []
        for match in pending:
            pmid = match.get("pending_match_id")
            if pmid is None:
                continue
            match_label = f"Match #{pmid}"
            if current and current.lower() not in match_label.lower():
                continue
            choices.append(app_commands.Choice(name=match_label, value=pmid))
        return choices[:25]  # Discord limit

    async def _send_first_neon_result(self, interaction, *event_fns):
        """Evaluate neon event callables in order, send only the FIRST non-None result."""
        for fn in event_fns:
            try:
                result = await fn()
                if result is not None:
                    await send_neon_result(interaction, result)
                    return
            except Exception as e:
                logger.debug("Failed to send neon result: %s", e)

    async def _update_shuffle_message_wagers(
        self, guild_id: int | None, pending_match_id: int | None = None
    ) -> None:
        """
        Refresh the shuffle message's wager field with current totals.
        Updates both the main channel message and the thread copy.

        Args:
            guild_id: Guild ID
            pending_match_id: Optional specific match ID for concurrent match support
        """
        pending_state = await asyncio.to_thread(
            self.match_service.get_last_shuffle, guild_id, pending_match_id
        )
        if not pending_state:
            return

        # Get betting display info
        totals = await asyncio.to_thread(
            functools.partial(self.betting_service.get_pot_odds, guild_id, pending_state=pending_state)
        )
        lock_until = pending_state.get("bet_lock_until")
        betting_mode = pending_state.get("betting_mode", "pool")
        field_name, field_value = format_betting_display(
            totals["radiant"], totals["dire"], betting_mode, lock_until
        )

        # Update main channel message (lobby channel)
        message_info = await asyncio.to_thread(
            self.match_service.state_service.get_shuffle_message_info, guild_id, pending_match_id
        )
        message_id = message_info.get("message_id") if message_info else None
        channel_id = message_info.get("channel_id") if message_info else None
        if message_id and channel_id:
            await self._update_embed_betting_field(channel_id, message_id, field_name, field_value)

        # Update command channel message if it exists (different from lobby channel)
        cmd_message_id = message_info.get("cmd_message_id") if message_info else None
        cmd_channel_id = message_info.get("cmd_channel_id") if message_info else None
        if cmd_message_id and cmd_channel_id:
            await self._update_embed_betting_field(cmd_channel_id, cmd_message_id, field_name, field_value)

        # Update thread message if it exists
        thread_message_id = pending_state.get("thread_shuffle_message_id")
        thread_id = pending_state.get("thread_shuffle_thread_id")
        if thread_message_id and thread_id:
            await self._update_embed_betting_field(thread_id, thread_message_id, field_name, field_value)

    async def _update_embed_betting_field(
        self, channel_id: int, message_id: int, field_name: str, field_value: str
    ) -> None:
        """Helper to update the betting field in an embed message."""
        try:
            channel = self.bot.get_channel(channel_id)
            if channel is None:
                channel = await self.bot.fetch_channel(channel_id)
            if channel is None:
                return

            message = await channel.fetch_message(message_id)
            if not message or not message.embeds:
                return

            embed = message.embeds[0]
            embed_dict = embed.to_dict()
            fields = embed_dict.get("fields", [])

            # Known wager field names to look for
            wager_field_names = {"💰 Pool Betting", "💰 House Betting (1:1)", "💰 Betting"}

            # Find and update wager field, remove duplicates
            updated = False
            new_fields = []
            for field in fields:
                fname = field.get("name", "")
                if fname in wager_field_names:
                    if not updated:
                        # Update the first matching wager field
                        field["name"] = field_name
                        field["value"] = field_value
                        new_fields.append(field)
                        updated = True
                    # Skip duplicates (don't add them to new_fields)
                else:
                    new_fields.append(field)

            if not updated:
                new_fields.append({"name": field_name, "value": field_value, "inline": False})
            embed_dict["fields"] = new_fields

            new_embed = discord.Embed.from_dict(embed_dict)
            await message.edit(embed=new_embed, allowed_mentions=discord.AllowedMentions.none())
        except Exception as exc:
            logger.warning(f"Failed to update shuffle wagers: {exc}", exc_info=True)

    async def _send_betting_reminder(
        self,
        guild_id: int | None,
        *,
        reminder_type: str,
        lock_until: int | None,
        pending_match_id: int | None = None,
    ) -> None:
        """
        Send a reminder message replying to the shuffle embed with current bet totals.

        reminder_type: "warning" (5 minutes left) or "closed" (betting closed).
        pending_match_id: Specific match ID for concurrent match support.
        """
        pending_state = await asyncio.to_thread(
            self.match_service.get_last_shuffle, guild_id, pending_match_id=pending_match_id
        )
        if not pending_state:
            return

        message_info = await asyncio.to_thread(
            self.match_service.get_shuffle_message_info, guild_id, pending_match_id=pending_match_id
        )
        message_id = message_info.get("message_id") if message_info else None
        channel_id = message_info.get("channel_id") if message_info else None
        thread_message_id = message_info.get("thread_message_id") if message_info else None
        thread_id = message_info.get("thread_id") if message_info else None

        totals = await asyncio.to_thread(
            functools.partial(self.betting_service.get_pot_odds, guild_id, pending_state=pending_state)
        )
        betting_mode = pending_state.get("betting_mode", "pool")

        # Format bets with odds for pool mode
        _, totals_text = format_betting_display(
            totals["radiant"], totals["dire"], betting_mode, lock_until=None
        )
        mode_label = "Pool" if betting_mode == "pool" else "House (1:1)"

        if reminder_type == "warning":
            if not lock_until:
                return
            content = (
                f"⏰ **5 minutes remaining until betting closes!** (<t:{int(lock_until)}:R>)\n"
                f"Mode: {mode_label}\n\n"
                f"Current bets:\n{totals_text}"
            )
        elif reminder_type == "closed":
            content = (
                f"🔒 **Betting is now closed!**\n"
                f"Mode: {mode_label}\n\n"
                f"Final bets:\n{totals_text}"
            )
        else:
            return

        # Post to origin channel (stored in shuffle message info, since reset_lobby clears it)
        try:
            # Get origin_channel_id from shuffle message info (lobby_service's is cleared by reset_lobby)
            origin_channel_id = message_info.get("origin_channel_id") if message_info else None
            target_channel_id = origin_channel_id if origin_channel_id else channel_id

            if target_channel_id:
                target_channel = self.bot.get_channel(target_channel_id)
                if target_channel is None:
                    target_channel = await self.bot.fetch_channel(target_channel_id)
                if target_channel:
                    await target_channel.send(content, allowed_mentions=discord.AllowedMentions.none())
        except Exception as exc:
            logger.warning(f"Failed to send betting reminder to channel: {exc}", exc_info=True)

        # Post to thread
        if thread_message_id and thread_id:
            try:
                thread = self.bot.get_channel(thread_id)
                if thread is None:
                    thread = await self.bot.fetch_channel(thread_id)
                if thread:
                    thread_message = await thread.fetch_message(thread_message_id)
                    if thread_message:
                        await thread_message.reply(content, allowed_mentions=discord.AllowedMentions.none())
            except Exception as exc:
                logger.warning(f"Failed to send betting reminder to thread: {exc}", exc_info=True)

    def _create_wheel_gif_file(
        self, target_idx: int, display_name: str | None = None,
        is_bankrupt: bool = False, is_golden: bool = False
    ) -> discord.File:
        """Create a wheel animation and return as discord.File."""
        buffer = create_wheel_gif(
            target_idx=target_idx, size=500, display_name=display_name,
            is_bankrupt=is_bankrupt, is_golden=is_golden
        )
        return discord.File(buffer, filename="wheel.gif")

    def _create_explosion_gif_file(self, display_name: str | None = None) -> discord.File:
        """Create an explosion animation and return as discord.File."""
        buffer = create_explosion_gif(size=500, display_name=display_name)
        return discord.File(buffer, filename="explosion.gif")

    def _wheel_result_embed(
        self,
        result: tuple,
        new_balance: int,
        garnished: int,
        next_spin_time: int,
        shell_victim: discord.Member | None = None,
        shell_victim_new_balance: int | None = None,
        shell_amount: int = 0,
        shell_self_hit: bool = False,
        shell_missed: bool = False,
        lightning_total: int = 0,
        lightning_count: int = 0,
        lightning_victims: list | None = None,
        extend_games_added: int = 0,
        extend_new_total: int = 0,
        is_bankrupt: bool = False,
        is_golden: bool = False,
        jailbreak_new_total: int = 0,
        chain_value: int | None = None,
        chain_username: str = "someone",
        emergency_count: int = 0,
        emergency_total: int = 0,
        commune_total: int = 0,
        commune_count: int = 0,
        pardon_consumed: bool = False,
        heist_total: int = 0,
        heist_count: int = 0,
        market_crash_total: int = 0,
        market_crash_count: int = 0,
        compound_amount: int = 0,
        trickle_total: int = 0,
        trickle_count: int = 0,
        dividend_amount: int = 0,
        takeover_amount: int = 0,
        takeover_victim_name: str = "rank #4",
        takeover_missed: bool = False,
    ) -> discord.Embed:
        """Build the final result embed after the wheel stops."""
        label, value = result[0], result[1]  # (label, value, color)

        if value == "JAILBREAK":
            title = "🔓 JAILBREAK! 🔓"
            color = discord.Color.from_str("#0a2a0a")
            description = (
                f"**JAIL**\n\n"
                f"You found a crack in the cell wall.\n\n"
                f"**−1 penalty game** removed!\n\n"
                f"Penalty games remaining: **{jailbreak_new_total}**\n\n"
                f"*Don't celebrate yet. You're still in here.*"
            )

        elif value == "CHAIN_REACTION":
            title = "⛓️ CHAIN REACTION! ⛓️"
            color = discord.Color.from_str("#1a1a3a")
            if chain_value is None:
                description = (
                    f"**CHAIN**\n\n"
                    f"⛓️ The chain reaches back... but finds nothing.\n\n"
                    f"*No prior normal wheel spin found. Fallback: nothing happens.*"
                )
            elif chain_value > 0:
                description = (
                    f"**CHAIN**\n\n"
                    f"⛓️ You copied **{chain_username}**'s last spin: **+{chain_value} JC**!\n\n"
                    f"*Their luck became yours.*"
                )
            elif chain_value < 0:
                description = (
                    f"**CHAIN**\n\n"
                    f"⛓️ You copied **{chain_username}**'s last spin: **{chain_value} JC**.\n\n"
                    f"*Their misfortune became yours. Tragic.*"
                )
            else:
                description = (
                    f"**CHAIN**\n\n"
                    f"⛓️ You copied **{chain_username}**'s last spin: **nothing happened**.\n\n"
                    f"*The chain found only silence.*"
                )

        elif value == "EMERGENCY":
            title = "🚨 EMERGENCY! 🚨"
            color = discord.Color.from_str("#2a1a00")
            description = (
                f"**SOS**\n\n"
                f"🚨 Economic crisis triggered!\n\n"
                f"**{emergency_count}** players each lost up to **10** {JOPACOIN_EMOTE}.\n"
                f"Total drained: **{emergency_total}** {JOPACOIN_EMOTE} (vanished).\n\n"
                f"*No one is safe. Not even you.*"
            )

        elif value == "COMMUNE":
            title = "🫳 SEIZE THE MEANS! 🫳"
            color = discord.Color.from_str("#1a2a1a")
            description = (
                f"**SEIZE**\n\n"
                f"{commune_count} players each donated 1 JC. "
                f"You received **+{commune_total} JC** from the collective.\n\n"
                f"*From each according to their balance, to you.*"
            )

        elif value == "COMEBACK":
            title = "🃏 CLUTCH SAVE! 🃏"
            color = discord.Color.from_str("#0a1a2a")
            description = (
                f"**CLUTCH**\n\n"
                f"Fortune smiles — once. Your next BANKRUPT will be converted to a LOSE instead. "
                f"*Don't waste it.*"
            )

        elif value in ("EXTEND_1", "EXTEND_2"):
            # Bankruptcy penalty extension (only appears on bankrupt wheel)
            color = discord.Color.dark_red()
            if extend_games_added == 0:
                title = "⛓️ Nothing to Extend"
                description = (
                    f"**{label}**\n\n"
                    f"Your debt is punishment enough. No penalty games to extend.\n\n"
                    f"*Pay off your debts.*"
                )
            else:
                title = "⛓️ PENALTY EXTENDED! ⛓️"
                description = (
                    f"**{label} GAME{'S' if extend_games_added > 1 else ''}**\n\n"
                    f"Your bankruptcy penalty has been extended by **{extend_games_added}** game{'s' if extend_games_added > 1 else ''}!\n\n"
                    f"New penalty games remaining: **{extend_new_total}**\n\n"
                    f"*The wheel remembers your sins... keep winning to escape!*"
                )

        elif value == "RED_SHELL":
            # Mario Kart Red Shell outcome
            if shell_missed:
                title = "🔴 RED SHELL MISSED! 🔴"
                color = discord.Color.dark_gray()
                description = (
                    f"**{label}**\n\n"
                    f"The Red Shell circles the track but finds no target!\n\n"
                    f"*You're already in 1st place... there's no one ahead to hit.*"
                )
            else:
                title = "🔴 RED SHELL HIT! 🔴"
                color = discord.Color.red()
                victim_name = shell_victim.mention if shell_victim else "the player above"
                description = (
                    f"**{label}**\n\n"
                    f"💥 Red Shell locked onto {victim_name}!\n"
                    f"You stole **{shell_amount}** {JOPACOIN_EMOTE}!\n\n"
                    f"*Victim's new balance: **{shell_victim_new_balance}** {JOPACOIN_EMOTE}*"
                )

        elif value == "BLUE_SHELL":
            # Mario Kart Blue Shell outcome
            if shell_missed:
                # Edge case: no players in leaderboard (shouldn't happen in practice)
                title = "🔵 BLUE SHELL MISSED! 🔵"
                color = discord.Color.dark_gray()
                description = (
                    f"**{label}**\n\n"
                    f"The Blue Shell circles the track but finds no target!\n\n"
                    f"*There's no one to hit...*"
                )
            elif shell_self_hit:
                title = "🔵 BLUE SHELL... SELF-HIT! 🔵"
                color = discord.Color.dark_blue()
                description = (
                    f"**{label}**\n\n"
                    f"💥 The Blue Shell targets the leader... **THAT'S YOU!**\n"
                    f"You lost **{shell_amount}** {JOPACOIN_EMOTE}!\n\n"
                    f"*The price of being on top... maybe diversify next time.*"
                )
            else:
                title = "🔵 BLUE SHELL STRIKE! 🔵"
                color = discord.Color.blue()
                victim_name = shell_victim.mention if shell_victim else "the richest player"
                description = (
                    f"**{label}**\n\n"
                    f"💥 Blue Shell targets the leader: {victim_name}!\n"
                    f"You stole **{shell_amount}** {JOPACOIN_EMOTE}!\n\n"
                    f"*Victim's new balance: **{shell_victim_new_balance}** {JOPACOIN_EMOTE}*"
                )

        elif value == "LIGHTNING_BOLT":
            title = "⚡ LIGHTNING BOLT! ⚡"
            color = discord.Color.from_str("#f39c12")
            victim_lines = ""
            if lightning_victims:
                for vname, vamt, _ in lightning_victims[:3]:
                    victim_lines += f"⚡ **{vname}** lost **{vamt}** JC\n"
            description = (
                f"**{label}**\n\n"
                f"Lightning strikes the entire server!\n\n"
                f"**{lightning_count}** players hit for a total of **{lightning_total}** {JOPACOIN_EMOTE}\n"
                f"All funds sent to the nonprofit fund.\n\n"
                f"{victim_lines}\n"
                f"*No one is safe.*"
            )

        # --- Golden Wheel mechanics ---
        elif value == "HEIST":
            title = "🥇 HEIST! 🥇"
            color = discord.Color.from_str("#7a5c00")
            if heist_count == 0:
                description = (
                    f"**HEIST**\n\n"
                    f"You cased the joint... but the bottom 30 are already broke.\n\n"
                    f"*Consolation prize: **+20** {JOPACOIN_EMOTE}.*"
                )
            else:
                description = (
                    f"**HEIST**\n\n"
                    f"💰 You robbed **{heist_count}** players at the bottom of the ladder!\n"
                    f"Total stolen: **{heist_total}** {JOPACOIN_EMOTE}\n\n"
                    f"*Crime pays — when you're already on top.*"
                )

        elif value == "MARKET_CRASH":
            title = "📉 MARKET CRASH! 📉"
            color = discord.Color.from_str("#8a4000")
            if market_crash_count == 0:
                description = (
                    f"**CRASH**\n\n"
                    f"You triggered a crash... but you're the only whale. No one to tax.\n\n"
                    f"*Consolation prize: **+25** {JOPACOIN_EMOTE}.*"
                )
            else:
                description = (
                    f"**CRASH**\n\n"
                    f"📉 Market crash! You taxed **{market_crash_count}** fellow top-3 players.\n"
                    f"Total collected: **{market_crash_total}** {JOPACOIN_EMOTE}\n\n"
                    f"*The rich get richer. That's just economics.*"
                )

        elif value == "COMPOUND_INTEREST":
            title = "📈 COMPOUND INTEREST! 📈"
            color = discord.Color.from_str("#6b8c00")
            description = (
                f"**COMPOUND**\n\n"
                f"📈 Your wealth grows on itself!\n"
                f"Earned **{compound_amount}** {JOPACOIN_EMOTE} (8% of your balance).\n\n"
                f"*The most boring way to get rich — and the most reliable.*"
            )

        elif value == "TRICKLE_DOWN":
            title = "💧 TRICKLE DOWN! 💧"
            color = discord.Color.from_str("#5c7a00")
            if trickle_count == 0:
                description = (
                    f"**TRICKLE**\n\n"
                    f"Trickle down economics... but there's no one else to tax.\n\n"
                    f"*Nothing happens. As usual.*"
                )
            else:
                description = (
                    f"**TRICKLE**\n\n"
                    f"💧 You taxed **{trickle_count}** players 1-3% of their balance.\n"
                    f"Total received: **{trickle_total}** {JOPACOIN_EMOTE}\n\n"
                    f"*It trickled up, actually.*"
                )

        elif value == "DIVIDEND":
            title = "💎 DIVIDEND! 💎"
            color = discord.Color.from_str("#4a7000")
            description = (
                f"**DIVIDEND**\n\n"
                f"💎 The server's collective wealth pays out!\n"
                f"Earned **{dividend_amount}** {JOPACOIN_EMOTE} (0.5% of total guild wealth).\n\n"
                f"*Being rich in a rich server pays dividends. Literally.*"
            )

        elif value == "HOSTILE_TAKEOVER":
            title = "🏴 HOSTILE TAKEOVER! 🏴"
            color = discord.Color.from_str("#6a2a80")
            if takeover_missed:
                description = (
                    f"**TAKEOVER**\n\n"
                    f"🏴 You targeted rank #4... but they're broke or don't exist.\n\n"
                    f"*Consolation prize: **+40** {JOPACOIN_EMOTE}.*"
                )
            else:
                description = (
                    f"**TAKEOVER**\n\n"
                    f"🏴 Corporate raid on **{takeover_victim_name}** (rank #4)!\n"
                    f"Seized **{takeover_amount}** {JOPACOIN_EMOTE}.\n\n"
                    f"*So close to the top — and yet, so far.*"
                )

        # --- Mana bonus wedge embeds ---
        elif value == "ERUPTION":
            title = "⛰️🔥 ERUPTION!"
            color = discord.Color.from_str("#ff4500")
            description = (
                f"**ERUPTION**\n\n"
                f"The Mountain erupts! You gain **2x** the last spinner's result.\n\n"
                f"*Red mana burns bright.*"
            )

        elif value == "FROZEN_ASSETS":
            title = "🏝️❄️ FROZEN ASSETS"
            color = discord.Color.from_str("#1e90ff")
            description = (
                f"**FROZEN**\n\n"
                f"Your assets are frozen. Win 0 now, but your next gamba "
                f"is guaranteed to land on a 50+ JC wedge.\n\n"
                f"*The Island remembers.*"
            )

        elif value == "OVERGROWTH":
            title = "🌲🌿 OVERGROWTH!"
            color = discord.Color.from_str("#228b22")
            description = (
                f"**OVERGROWTH**\n\n"
                f"The Forest rewards consistency. You earn 10 JC per game played this week.\n\n"
                f"*Slow and steady wins the race.*"
            )

        elif value == "SANCTUARY":
            title = "🌾✨ SANCTUARY"
            color = discord.Color.from_str("#f5f5dc")
            description = (
                f"**SANCTUARY**\n\n"
                f"A blessing radiates outward. Win 0, but all players who spin "
                f"the wheel in the next 24 hours get +5 JC added to their result.\n\n"
                f"*The Plains protect all.*"
            )

        elif value == "DECAY":
            title = "🌿💀 DECAY!"
            color = discord.Color.from_str("#4b0082")
            description = (
                f"**DECAY**\n\n"
                f"Rot spreads to the wealthy. The top 3 wealthiest lose 40 JC each, "
                f"rank #4 loses 50 JC. You consume the remains.\n\n"
                f"*The Swamp claims what it is owed.*"
            )

        elif isinstance(value, int) and value > 0:
            # Win
            if is_bankrupt and value == 1:
                title = "🪙 One Coin. One."
                color = discord.Color.from_str("#3a3a1a")
                description = f"**1**\n\nThe wheel took pity on you. One coin.\n\n*It's still technically a win.*"
            elif is_bankrupt and value == 2:
                title = "🪙 Two Coins."
                color = discord.Color.from_str("#3a3500")
                description = f"**2**\n\nEven charity has standards. Here's 2.\n\n*Don't spend it all in one place.*"
            elif is_golden and value == 250:
                title = "👑 CROWN JEWEL! 👑"
                color = discord.Color.from_str("#ffd700")
                description = (
                    f"**CROWN**\n\n"
                    f"✨ **+250 {JOPACOIN_EMOTE} JACKPOT!** ✨\n\n"
                    f"The golden wheel's ultimate prize.\n"
                    f"The crown jewel of Jopacoin fortune.\n\n"
                    f"*The server weeps. You reign.*"
                )
            elif value == 100:
                title = "🌟 JACKPOT! 🌟"
                color = discord.Color.gold()
                description = f"**{label}**\n\nYou won **{value}** {JOPACOIN_EMOTE}!"
            elif is_golden:
                title = "👑 Golden Win!"
                color = discord.Color.from_str("#daa520")
                description = f"**+{value} JC**\n\nYou won **{value}** {JOPACOIN_EMOTE} from the Golden Wheel!"
            else:
                title = "🎉 Winner!"
                color = discord.Color.green()
                description = f"**+{value} JC**\n\nYou won **{value}** {JOPACOIN_EMOTE}!"

            if garnished > 0:
                description += f"\n\n*{garnished} {JOPACOIN_EMOTE} went to debt repayment.*"

        elif isinstance(value, int) and value < 0:
            if is_golden:
                # OVEREXTENDED — golden wheel's penalty wedge
                title = "📉 OVEREXTENDED! 📉"
                color = discord.Color.from_str("#4a3000")
                description = (
                    f"**OVEREXTENDED**\n\n"
                    f"You flew too close to the sun.\n\n"
                    f"Lost **{abs(value)}** {JOPACOIN_EMOTE}.\n\n"
                    f"*Pride goes before the fall.*"
                )
            else:
                title = "💀 BANKRUPT! 💀"
                color = discord.Color.red()
                description = (
                    f"**{label}**\n\n"
                    f"You lost **{abs(value)}** {JOPACOIN_EMOTE}!\n\n"
                    f"*The wheel shows no mercy...*"
                )
        elif pardon_consumed:
            # COMEBACK pardon absorbed the BANKRUPT
            title = "🃏 CLUTCH ACTIVATED! — BANKRUPT SAVED 🃏"
            color = discord.Color.from_str("#0a1a2a")
            description = (
                f"**CLUTCH**\n\n"
                f"You were about to go BANKRUPT... but your CLUTCH token saved you. Treated as LOSE."
            )
        else:
            # Lose a Turn (0) - 5 day penalty cooldown
            title = "🚫 LOSE A TURN 🚫"
            color = discord.Color.dark_gray()
            description = (
                f"**{label}**\n\n"
                f"No jopacoin lost... but you just got **5-day timeout'd** from the wheel.\n\n"
                f"*Imagine being this unlucky. Go outside. Touch grass. "
                f"Reflect on your gambling addiction.*"
            )

        embed = discord.Embed(
            title=title,
            description=description,
            color=color,
        )

        embed.add_field(
            name="New Balance",
            value=f"**{new_balance}** {JOPACOIN_EMOTE}",
            inline=False,
        )

        embed.add_field(
            name="Next Spin",
            value=f"<t:{next_spin_time}:R>",
            inline=False,
        )

        return embed

    def _wheel_explosion_embed(
        self, new_balance: int, garnished: int, next_spin_time: int
    ) -> discord.Embed:
        """Build the result embed when the wheel explodes."""
        title = "💥 THE WHEEL EXPLODED! 💥"
        color = discord.Color.orange()

        description = (
            f"**KABOOM!**\n\n"
            f"The wheel has exploded! Fortunately, no one was hurt.\n\n"
            f"We sincerely apologize for the inconvenience. "
            f"As compensation, you've been awarded **{WHEEL_EXPLOSION_REWARD}** {JOPACOIN_EMOTE}."
        )

        if garnished > 0:
            description += f"\n\n*{garnished} {JOPACOIN_EMOTE} went to debt repayment.*"

        embed = discord.Embed(
            title=title,
            description=description,
            color=color,
        )

        embed.add_field(
            name="New Balance",
            value=f"**{new_balance}** {JOPACOIN_EMOTE}",
            inline=False,
        )

        embed.add_field(
            name="Next Spin",
            value=f"<t:{next_spin_time}:R>",
            inline=False,
        )

        embed.set_footer(text="Our engineers are working on a replacement wheel.")

        return embed

    @app_commands.command(
        name="bet",
        description="Place a jopacoin bet on a match (check balance with /balance)",
    )
    @app_commands.describe(
        team="Radiant or Dire",
        amount="Amount of jopacoin to wager (view balance with /balance)",
        leverage="Leverage multiplier (2x, 3x, 5x) - can cause debt!",
        match="Match to bet on (optional - auto-selects if you're a participant or only one match exists)",
    )
    @app_commands.choices(
        team=[
            app_commands.Choice(name="Radiant", value="radiant"),
            app_commands.Choice(name="Dire", value="dire"),
        ],
        leverage=[
            app_commands.Choice(name="None (1x)", value=1),
            app_commands.Choice(name="2x", value=2),
            app_commands.Choice(name="3x", value=3),
            app_commands.Choice(name="5x", value=5),
            app_commands.Choice(name="10x", value=10),
        ],
    )
    @app_commands.autocomplete(match=match_autocomplete)
    async def bet(
        self,
        interaction: discord.Interaction,
        team: app_commands.Choice[str],
        amount: int,
        leverage: app_commands.Choice[int] = None,
        match: int = None,
    ):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="bet",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=5,
            per_seconds=20,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"⏳ Please wait {rl.retry_after_seconds}s before using `/bet` again.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=True):
            return
        guild_id = interaction.guild.id if interaction.guild else None
        user_id = interaction.user.id

        if amount < JOPACOIN_MIN_BET:
            await interaction.followup.send(
                f"Minimum bet is {JOPACOIN_MIN_BET} {JOPACOIN_EMOTE}.", ephemeral=True
            )
            return

        # Handle match selection for concurrent match support
        pending_state = None
        pending_match_id = match  # Optional match ID from parameter

        if pending_match_id is not None:
            # Explicit match ID provided - use that specific match
            pending_state = await asyncio.to_thread(
                self.match_service.state_service.get_last_shuffle, guild_id, pending_match_id
            )
            if not pending_state:
                await interaction.followup.send(
                    f"❌ Match #{pending_match_id} not found or already completed.", ephemeral=True
                )
                return
        else:
            # No match specified - try auto-detection
            all_pending = await asyncio.to_thread(
                self.match_service.state_service.get_all_pending_matches, guild_id
            )
            if not all_pending:
                await interaction.followup.send("❌ No active match to bet on.", ephemeral=True)
                return

            if len(all_pending) == 1:
                # Single match - use it (backward compatible)
                pending_state = all_pending[0]
            else:
                # Multiple matches - try to find one the user is in
                player_match = await asyncio.to_thread(
                    self.match_service.state_service.get_pending_match_for_player, guild_id, user_id
                )
                if player_match:
                    pending_state = player_match
                else:
                    # User is a spectator with multiple matches - require explicit selection
                    match_list = ", ".join(f"Match #{m.get('pending_match_id')}" for m in all_pending if m.get('pending_match_id'))
                    await interaction.followup.send(
                        f"❌ Multiple matches in progress ({match_list}). "
                        "Please specify which match to bet on using the `match` parameter.",
                        ephemeral=True,
                    )
                    return

        if not pending_state:
            await interaction.followup.send("❌ No active match to bet on.", ephemeral=True)
            return

        pending_match_id = pending_state.get("pending_match_id")

        # Unified betting through BettingService (works for both shuffle and draft modes)
        lev = leverage.value if leverage else 1

        # Red mana: unlock 10x leverage
        if lev == 10:
            _mana_fx = getattr(self.bot, "mana_effects_service", None)
            _has_10x = False
            if _mana_fx:
                try:
                    from domain.models.mana_effects import ManaEffects as _MEBet
                    _bet_effects = await asyncio.to_thread(_mana_fx.get_effects, user_id, guild_id)
                    if isinstance(_bet_effects, _MEBet):
                        _has_10x = _bet_effects.red_10x_leverage
                except Exception:
                    pass
            if not _has_10x:
                await interaction.followup.send(
                    "❌ 10x leverage is exclusive to **Red mana (Mountain)** players!", ephemeral=True
                )
                return

        effective_bet = amount * lev

        try:
            await asyncio.to_thread(
                functools.partial(
                    self.betting_service.place_bet,
                    guild_id, user_id, team.value, amount, pending_state, leverage=lev,
                )
            )
        except ValueError as exc:
            await interaction.followup.send(f"❌ {exc}", ephemeral=True)
            return

        await self._update_shuffle_message_wagers(guild_id, pending_match_id)

        # Build response message
        betting_mode = pending_state.get("betting_mode", "pool") if pending_state else "pool"
        pool_warning = ""
        if betting_mode == "pool":
            pool_warning = "\n⚠️ Pool mode: odds may shift as more bets come in. Use `/mybets` to check current EV."

        # Include match ID note if there's a pending_match_id
        match_note = f" (Match #{pending_match_id})" if pending_match_id else ""

        if lev > 1:
            await interaction.followup.send(
                f"Bet placed{match_note}: {amount} {JOPACOIN_EMOTE} on {team.name} at {lev}x leverage "
                f"(effective: {effective_bet} {JOPACOIN_EMOTE}).{pool_warning}",
                ephemeral=True,
            )
        else:
            await interaction.followup.send(
                f"Bet placed{match_note}: {amount} {JOPACOIN_EMOTE} on {team.name}.{pool_warning}",
                ephemeral=True,
            )

        # Neon Degen Terminal hooks - at most ONE neon event per /bet action
        neon = self._get_neon_service()
        if neon:
            try:
                # Pre-fetch data needed by multiple event checks
                player_repo = self.betting_service.player_repo
                player = await asyncio.to_thread(
                    player_repo.get_by_id, user_id, guild_id
                )
                balance_before = (player.jopacoin_balance + amount) if player else 0

                # Increment total bets (side-effect needed regardless of which event fires)
                total_bets = await asyncio.to_thread(
                    player_repo.increment_total_bets_placed, user_id, guild_id
                ) if player else 0

                # Compute seconds remaining for last-second check
                seconds_remaining = 0
                if pending_state:
                    lock_time = pending_state.get("lock_time", 0)
                    import time as _time
                    seconds_remaining = max(0, int(lock_time - _time.time()))

                # Build candidate event lambdas, rarest/one-time first
                candidates = []

                # First leverage bet (one-time)
                if player and lev > 1 and not player.first_leverage_used:
                    async def _first_leverage():
                        result = await neon.on_first_leverage_bet(user_id, guild_id, lev)
                        if result is not None:
                            await asyncio.to_thread(
                                player_repo.mark_first_leverage_used, user_id, guild_id
                            )
                        return result
                    candidates.append(_first_leverage)

                # 100 bets milestone (one-time)
                if total_bets == 100:
                    candidates.append(
                        lambda: neon.on_100_bets_milestone(user_id, guild_id, total_bets)
                    )

                # All-in bet
                if player and balance_before > 0:
                    candidates.append(
                        lambda: neon.on_all_in_bet(user_id, guild_id, amount, balance_before)
                    )

                # Last-second bet
                if 0 < seconds_remaining <= 60:
                    candidates.append(
                        lambda: neon.on_last_second_bet(user_id, guild_id, seconds_remaining)
                    )

                # Standard bet placed (most common, lowest priority)
                candidates.append(
                    lambda: neon.on_bet_placed(user_id, guild_id, amount, lev, team.value)
                )

                await self._send_first_neon_result(interaction, *candidates)
            except Exception as e:
                logger.debug(f"Easter egg event hooks error: {e}")

    @app_commands.command(name="mybets", description="Show your active bets")
    async def mybets(self, interaction: discord.Interaction):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="mybets",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=5,
            per_seconds=10,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"⏳ Please wait {rl.retry_after_seconds}s before using `/mybets` again.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id if interaction.guild else None

        # Get all pending bets for the user (across all matches)
        all_bets = await asyncio.to_thread(
            self.betting_service.bet_repo.get_all_player_pending_bets,
            guild_id, interaction.user.id
        )
        if not all_bets:
            await interaction.followup.send("You have no active bets.", ephemeral=True)
            return

        # Get all pending matches for context
        all_pending = await asyncio.to_thread(
            self.match_service.state_service.get_all_pending_matches, guild_id
        )
        pending_by_id = {m.get("pending_match_id"): m for m in all_pending if m.get("pending_match_id")}

        # Group bets by pending_match_id
        bets_by_match: dict[int | None, list[dict]] = {}
        for bet in all_bets:
            pmid = bet.get("pending_match_id")
            if pmid not in bets_by_match:
                bets_by_match[pmid] = []
            bets_by_match[pmid].append(bet)

        # Build output for each match
        output_sections = []
        for pmid, bets in bets_by_match.items():
            pending_state = pending_by_id.get(pmid) if pmid else None

            # Calculate totals for this match
            total_amount = sum(b["amount"] for b in bets)
            total_effective = sum(b["amount"] * (b.get("leverage", 1) or 1) for b in bets)
            team_name = bets[0]["team_bet_on"].title()

            # Build bet lines
            bet_lines = []
            for i, bet in enumerate(bets, 1):
                leverage = bet.get("leverage", 1) or 1
                effective = bet["amount"] * leverage
                time_str = f"<t:{int(bet['bet_time'])}:t>"
                is_blind = bet.get("is_blind", 0)
                auto_tag = " (auto)" if is_blind else ""
                if leverage > 1:
                    bet_lines.append(
                        f"{i}. {bet['amount']} {JOPACOIN_EMOTE} at {leverage}x "
                        f"(effective: {effective} {JOPACOIN_EMOTE}){auto_tag} — {time_str}"
                    )
                else:
                    bet_lines.append(f"{i}. {bet['amount']} {JOPACOIN_EMOTE}{auto_tag} — {time_str}")

            # Header with match ID if multiple matches
            match_label = f" (Match #{pmid})" if pmid and len(bets_by_match) > 1 else ""
            if len(bets) == 1:
                header = f"**Active bet on {team_name}{match_label}:**"
            else:
                header = f"**Active bets on {team_name}{match_label}** ({len(bets)} bets):"

            # Show total if multiple bets
            if len(bets) > 1:
                if total_amount != total_effective:
                    bet_lines.append(
                        f"\n**Total:** {total_amount} {JOPACOIN_EMOTE} "
                        f"(effective: {total_effective} {JOPACOIN_EMOTE})"
                    )
                else:
                    bet_lines.append(f"\n**Total:** {total_amount} {JOPACOIN_EMOTE}")

            section_msg = header + "\n" + "\n".join(bet_lines)

            # Add EV info for pool mode
            betting_mode = pending_state.get("betting_mode", "pool") if pending_state else "pool"
            if betting_mode == "pool" and pending_state:
                totals = await asyncio.to_thread(
                    functools.partial(self.betting_service.get_pot_odds, guild_id, pending_state=pending_state)
                )
                total_pool = totals["radiant"] + totals["dire"]
                my_team_total = totals[bets[0]["team_bet_on"]]

                if my_team_total > 0 and total_pool > 0:
                    my_share = total_effective / my_team_total
                    potential_payout = int(total_pool * my_share)
                    other_team = "dire" if bets[0]["team_bet_on"] == "radiant" else "radiant"
                    odds_ratio = totals[other_team] / my_team_total if my_team_total > 0 else 0

                    section_msg += (
                        f"\n\n📊 **Current Pool Odds** (may change):"
                        f"\nTotal pool: {total_pool} {JOPACOIN_EMOTE}"
                        f"\nYour team ({team_name}): {my_team_total} {JOPACOIN_EMOTE}"
                        f"\nIf you win: ~{potential_payout} {JOPACOIN_EMOTE} ({odds_ratio:.2f}:1 odds)"
                    )
            elif betting_mode == "house":
                # House mode: 1:1 payout
                potential_payout = total_effective * 2
                section_msg += f"\n\nIf you win: {potential_payout} {JOPACOIN_EMOTE} (1:1 odds)"

            output_sections.append(section_msg)

        # Join all sections with a separator if multiple matches
        if len(output_sections) > 1:
            base_msg = "\n\n---\n\n".join(output_sections)
        else:
            base_msg = output_sections[0]

        await interaction.followup.send(base_msg, ephemeral=True)

    @app_commands.command(name="bets", description="Show all bets in the current pool")
    @app_commands.describe(
        match="Match to view bets for (auto-selects if only one match exists)",
    )
    @app_commands.autocomplete(match=match_autocomplete)
    async def bets(self, interaction: discord.Interaction, match: int = None):
        """View all bets in the current pool."""
        if not has_admin_permission(interaction):
            guild = interaction.guild if interaction.guild else None
            rl_gid = guild.id if guild else 0
            rl = GLOBAL_RATE_LIMITER.check(
                scope="bets",
                guild_id=rl_gid,
                user_id=interaction.user.id,
                limit=1,
                per_seconds=60,
            )
            if not rl.allowed:
                await interaction.response.send_message(
                    f"⏳ Please wait {rl.retry_after_seconds}s before using `/bets` again.",
                    ephemeral=True,
                )
                return

        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id if interaction.guild else None
        user_id = interaction.user.id

        # Handle match selection for concurrent match support
        pending_state = None
        pending_match_id = match  # Optional match ID from parameter

        if pending_match_id is not None:
            # Explicit match ID provided - use that specific match
            pending_state = await asyncio.to_thread(
                self.match_service.state_service.get_last_shuffle, guild_id, pending_match_id
            )
            if not pending_state:
                await interaction.followup.send(
                    f"❌ Match #{pending_match_id} not found or already completed.", ephemeral=True
                )
                return
        else:
            # No match specified - try auto-detection
            all_pending = await asyncio.to_thread(
                self.match_service.state_service.get_all_pending_matches, guild_id
            )
            if not all_pending:
                await interaction.followup.send("No active match to show bets for.", ephemeral=True)
                return

            if len(all_pending) == 1:
                # Single match - use it (backward compatible)
                pending_state = all_pending[0]
            else:
                # Multiple matches - try to find one the user is in
                player_match = await asyncio.to_thread(
                    self.match_service.state_service.get_pending_match_for_player, guild_id, user_id
                )
                if player_match:
                    pending_state = player_match
                else:
                    # User is a spectator with multiple matches - require explicit selection
                    match_list = ", ".join(f"Match #{m.get('pending_match_id')}" for m in all_pending if m.get('pending_match_id'))
                    await interaction.followup.send(
                        f"❌ Multiple matches in progress ({match_list}). "
                        "Please specify which match to view using the `match` parameter.",
                        ephemeral=True,
                    )
                    return

        if not pending_state:
            await interaction.followup.send("No active match to show bets for.", ephemeral=True)
            return

        pending_match_id = pending_state.get("pending_match_id")

        all_bets = await asyncio.to_thread(
            functools.partial(self.betting_service.get_all_pending_bets, guild_id, pending_state=pending_state)
        )
        if not all_bets:
            await interaction.followup.send("No bets placed yet.", ephemeral=True)
            return

        # Get current odds
        totals = await asyncio.to_thread(
            functools.partial(self.betting_service.get_pot_odds, guild_id, pending_state=pending_state)
        )
        total_pool = totals["radiant"] + totals["dire"]
        radiant_mult = total_pool / totals["radiant"] if totals["radiant"] > 0 else None
        dire_mult = total_pool / totals["dire"] if totals["dire"] > 0 else None

        # Build embed
        match_label = f"Match #{pending_match_id} — " if pending_match_id else ""
        embed = discord.Embed(
            title=f"📊 {match_label}Pool Bets ({len(all_bets)} bets)",
            color=discord.Color.gold(),
        )

        # Current odds header
        lock_until = pending_state.get("bet_lock_until")
        radiant_odds_str = f"{radiant_mult:.2f}x" if radiant_mult else "—"
        dire_odds_str = f"{dire_mult:.2f}x" if dire_mult else "—"
        odds_text = (
            f"🟢 Radiant: {totals['radiant']} {JOPACOIN_EMOTE} ({radiant_odds_str}) | "
            f"🔴 Dire: {totals['dire']} {JOPACOIN_EMOTE} ({dire_odds_str})"
        )
        if lock_until:
            odds_text += f"\nBetting closes <t:{lock_until}:R>"
        embed.add_field(name="Current Odds", value=odds_text, inline=False)

        # Group bets by team
        radiant_bets = [b for b in all_bets if b["team_bet_on"] == "radiant"]
        dire_bets = [b for b in all_bets if b["team_bet_on"] == "dire"]

        # Check if betting is still open and if user is admin
        is_admin = has_admin_permission(interaction)
        betting_open = lock_until and int(time.time()) < lock_until
        show_names = is_admin or not betting_open

        # Format bet line helper
        def format_bet_line(bet: dict, index: int) -> str:
            leverage = bet.get("leverage", 1) or 1
            is_blind = bet.get("is_blind", 0)
            odds_at_placement = bet.get("odds_at_placement")

            # Base amount - hide names for non-admins while betting is open
            if show_names:
                line = f"<@{bet['discord_id']}> • {bet['amount']}"
            else:
                line = f"Bettor #{index} • {bet['amount']}"

            # Auto tag
            if is_blind:
                line += " (auto)"

            # Leverage notation
            if leverage > 1:
                effective = bet["amount"] * leverage
                line += f" at {leverage}x → {effective} eff"

            # Odds at placement
            if odds_at_placement:
                line += f" • {odds_at_placement:.2f}x"

            return line

        # Radiant bets section
        if radiant_bets:
            radiant_lines = [format_bet_line(b, i + 1) for i, b in enumerate(radiant_bets)]
            # Truncate if too long
            radiant_text = "\n".join(radiant_lines[:15])
            if len(radiant_bets) > 15:
                radiant_text += f"\n... +{len(radiant_bets) - 15} more"
            embed.add_field(
                name=f"🟢 Radiant Bets ({len(radiant_bets)})",
                value=radiant_text or "None",
                inline=False,
            )

        # Dire bets section
        if dire_bets:
            dire_lines = [format_bet_line(b, i + 1) for i, b in enumerate(dire_bets)]
            dire_text = "\n".join(dire_lines[:15])
            if len(dire_bets) > 15:
                dire_text += f"\n... +{len(dire_bets) - 15} more"
            embed.add_field(
                name=f"🔴 Dire Bets ({len(dire_bets)})",
                value=dire_text or "None",
                inline=False,
            )

        # Pool summary
        radiant_pct = (totals["radiant"] / total_pool * 100) if total_pool > 0 else 0
        dire_pct = (totals["dire"] / total_pool * 100) if total_pool > 0 else 0
        summary_text = (
            f"**Total:** {total_pool} {JOPACOIN_EMOTE} effective\n"
            f"Radiant: {totals['radiant']} ({radiant_pct:.0f}%) | Dire: {totals['dire']} ({dire_pct:.0f}%)"
        )
        embed.add_field(name="Pool Summary", value=summary_text, inline=False)

        await interaction.followup.send(embed=embed, ephemeral=True)

    @app_commands.command(name="balance", description="Check your jopacoin balance")
    async def balance(self, interaction: discord.Interaction):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="balance",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=5,
            per_seconds=10,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"Please wait {rl.retry_after_seconds}s before using `/balance` again.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=True):
            return

        user_id = interaction.user.id
        guild_id = guild.id if guild else None
        balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)

        # Check for bankruptcy penalty
        penalty_info = ""
        if self.bankruptcy_service:
            state = await asyncio.to_thread(self.bankruptcy_service.get_state, user_id, guild_id)
            if state.penalty_games_remaining > 0:
                penalty_rate_pct = int(BANKRUPTCY_PENALTY_RATE * 100)
                penalty_info = (
                    f"\n**Bankruptcy penalty:** {penalty_rate_pct}% win bonus "
                    f"for {state.penalty_games_remaining} more win(s)"
                )

        # Check for loan info
        loan_info = ""
        if self.loan_service:
            loan_state = await asyncio.to_thread(self.loan_service.get_state, user_id, guild_id)
            # Show outstanding loan prominently
            if loan_state.has_outstanding_loan:
                loan_info = (
                    f"\n⚠️ **Outstanding loan:** {loan_state.outstanding_total} {JOPACOIN_EMOTE} "
                    f"(repaid after next match)"
                )
            if loan_state.total_loans_taken > 0:
                loan_info += f"\n**Loans taken:** {loan_state.total_loans_taken} (fees paid: {loan_state.total_fees_paid})"
            if loan_state.is_on_cooldown and loan_state.cooldown_ends_at:
                import time
                remaining = loan_state.cooldown_ends_at - int(time.time())
                hours = remaining // 3600
                minutes = (remaining % 3600) // 60
                loan_info += f"\n**Loan cooldown:** {hours}h {minutes}m remaining"

        if balance >= 0:
            await interaction.followup.send(
                f"{interaction.user.mention} has {balance} {JOPACOIN_EMOTE}.{penalty_info}{loan_info}",
                ephemeral=True,
            )
        else:
            # Show debt information
            garnishment_pct = int(GARNISHMENT_PERCENTAGE * 100)

            await interaction.followup.send(
                f"{interaction.user.mention} has **{balance}** {JOPACOIN_EMOTE} (in debt)\n"
                f"Garnishment: {garnishment_pct}% of winnings go to debt repayment{penalty_info}{loan_info}\n\n"
                f"Use `/bankruptcy` to clear your debt (with penalties).\n"
                f"Use `/loan` to borrow more jopacoin (with a fee).",
                ephemeral=True,
            )

        # Neon Degen Terminal hook
        neon = self._get_neon_service()
        if neon:
            neon_result = await neon.on_balance_check(user_id, guild_id, balance)
            await send_neon_result(interaction, neon_result)

    @app_commands.command(name="gamba", description="Spin the Wheel of Fortune! (once per day)")
    async def gamba(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None
        now = time.time()

        # Check if player is registered
        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
        if not player:
            await interaction.response.send_message(
                "You need to `/player register` before you can spin the wheel.",
                ephemeral=True,
            )
            return

        # Check cooldown (persisted in database) - admins bypass cooldown
        is_admin = has_admin_permission(interaction)
        if not is_admin:
            # Atomic check-and-claim: prevents race condition where concurrent
            # requests could both pass the cooldown check
            claimed = await asyncio.to_thread(
                self.player_service.try_claim_wheel_spin,
                user_id, guild_id, int(now), WHEEL_COOLDOWN_SECONDS,
            )
            if not claimed:
                # Check for free celebration spin (attackers_win war effect)
                _rebellion_svc = self.rebellion_service or getattr(self.bot, "rebellion_service", None)
                _celebration_granted = False
                if _rebellion_svc:
                    _active_war_cs = await asyncio.to_thread(
                        _rebellion_svc.get_active_war_effect, guild_id
                    )
                    if _active_war_cs and _active_war_cs.get("outcome") == "attackers_win":
                        _celebration_granted = await asyncio.to_thread(
                            _rebellion_svc.check_and_use_celebration_spin,
                            _active_war_cs["war_id"], user_id, guild_id,
                        )

                if not _celebration_granted:
                    # Spin was not claimed - still on cooldown. Get remaining time.
                    last_spin = await asyncio.to_thread(
                        self.player_service.get_last_wheel_spin, user_id, guild_id
                    )
                    if last_spin:
                        remaining = WHEEL_COOLDOWN_SECONDS - (now - last_spin)
                        hours = int(remaining // 3600)
                        minutes = int((remaining % 3600) // 60)
                    else:
                        hours, minutes = 24, 0  # Fallback
                    await interaction.response.send_message(
                        f"You already spun the wheel today! Try again in **{hours}h {minutes}m**.",
                        ephemeral=True,
                    )
                    # Neon Degen Terminal hook (cooldown hit)
                    neon = self._get_neon_service()
                    if neon:
                        try:
                            neon_result = await neon.on_cooldown_hit(user_id, guild_id, "gamba")
                            await send_neon_result(interaction, neon_result)
                        except Exception as e:
                            logger.debug("Failed to send gamba cooldown neon result: %s", e)
                    return
                # Celebration spin granted — bypass cooldown, continue with spin
        else:
            # Admin bypass - still set the timestamp for consistency
            await asyncio.to_thread(
                self.player_service.set_last_wheel_spin, user_id, guild_id, int(now)
            )

        # Check for 1% explosion chance (overrides normal result)
        is_explosion = random.random() < WHEEL_EXPLOSION_CHANCE

        if is_explosion:
            # THE WHEEL EXPLODES!
            await interaction.response.defer()

            # Generate explosion animation
            user_display = interaction.user.name
            gif_file = await asyncio.to_thread(self._create_explosion_gif_file, user_display)
            message = await interaction.followup.send(file=gif_file, wait=True)

            # Wait for explosion animation (~8 seconds)
            # 20 spin frames * 50ms + 15 shake frames * ~100ms + 25 explosion * 70ms + 20 aftermath * 100ms
            await asyncio.sleep(8.0)

            # Apply explosion reward (67 JC)
            garnished_amount = 0
            new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)

            garnishment_service = getattr(self.bot, "garnishment_service", None)
            if garnishment_service and new_balance < 0:
                result = await asyncio.to_thread(
                    garnishment_service.add_income, user_id, WHEEL_EXPLOSION_REWARD, guild_id
                )
                garnished_amount = result.get("garnished", 0)
                new_balance = result.get("new_balance", new_balance + WHEEL_EXPLOSION_REWARD)
            else:
                await asyncio.to_thread(
                    self.player_service.adjust_balance, user_id, guild_id, WHEEL_EXPLOSION_REWARD
                )
                new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)

            next_spin_time = int(now) + WHEEL_COOLDOWN_SECONDS

            # Log the explosion as a special result
            await asyncio.to_thread(
                functools.partial(
                    self.player_service.log_wheel_spin,
                    discord_id=user_id,
                    guild_id=guild_id,
                    result=WHEEL_EXPLOSION_REWARD,
                    spin_time=int(now),
                )
            )

            await asyncio.sleep(0.5)
            result_embed = self._wheel_explosion_embed(new_balance, garnished_amount, next_spin_time)
            await message.edit(embed=result_embed)
            return

        # Use bankrupt wheel for negative balance OR formal bankruptcy penalty
        balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
        is_eligible_for_bad_gamba = balance < 0
        penalty_games_remaining = 0
        bankruptcy_service: BankruptcyService | None = getattr(self.bot, "bankruptcy_service", None)
        if bankruptcy_service:
            state = await asyncio.to_thread(
                bankruptcy_service.get_state, user_id, guild_id
            )
            if state:
                penalty_games_remaining = state.penalty_games_remaining
                if penalty_games_remaining > 0:
                    is_eligible_for_bad_gamba = True

        # Golden Wheel eligibility: top-N balance holders get the golden wheel
        # Bankrupt/penalty wheel always takes priority — golden wheel only for non-bad-gamba
        is_golden = False
        if not is_eligible_for_bad_gamba:
            top_n = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=WHEEL_GOLDEN_TOP_N)
            )
            top_n_ids = {p.discord_id for p in top_n}
            is_golden = user_id in top_n_ids

        # Public announcement when a top-N player spins the golden wheel
        if is_golden and interaction.channel:
            top_3_lines = "\n".join(
                f"**#{i+1}** {p.name} — {p.jopacoin_balance} {JOPACOIN_EMOTE}"
                for i, p in enumerate(top_n)
            )
            announce_embed = discord.Embed(
                title="👑 GOLDEN WHEEL INCOMING 👑",
                description=(
                    f"👑 **{interaction.user.mention} is spinning the GOLDEN WHEEL!**\n"
                    f"They are among the top {WHEEL_GOLDEN_TOP_N} wealthiest players in the server...\n\n"
                    f"**Current top-{WHEEL_GOLDEN_TOP_N}:**\n{top_3_lines}"
                ),
                color=discord.Color.from_str("#ffd700"),
            )
            try:
                await interaction.channel.send(embed=announce_embed)
            except Exception as e:
                logger.debug("Failed to send golden wheel announcement: %s", e)

        # Pre-determine the result (use bad gamba wheel for negative balance or penalty)
        if is_golden:
            # Fetch live data to compute OVEREXTENDED dynamically so EV stays pinned to target
            # as server wealth changes (TRICKLE_DOWN, DIVIDEND, COMPOUND all scale with balances)
            top_n_extended = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=WHEEL_GOLDEN_TOP_N + 1)
            )
            rank_next_live = top_n_extended[WHEEL_GOLDEN_TOP_N] if len(top_n_extended) > WHEEL_GOLDEN_TOP_N else None
            rank_next_balance_live = (
                rank_next_live.jopacoin_balance
                if rank_next_live and rank_next_live.jopacoin_balance > 0
                else None
            )
            total_positive_live = await asyncio.to_thread(
                self.player_service.get_total_positive_balance, guild_id
            )
            bottom_players_live = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard_bottom, guild_id, limit=30, min_balance=1)
            )
            bottom_balances_live = [p.jopacoin_balance for p in bottom_players_live if p.discord_id != user_id]
            other_top_balances_live = [
                p.jopacoin_balance for p in top_n if p.discord_id != user_id and p.jopacoin_balance > 0
            ]
            wedges = compute_live_golden_wedges(
                spinner_balance=balance,
                other_top_balances=other_top_balances_live,
                rank_next_balance=rank_next_balance_live,
                total_positive_balance=total_positive_live,
                bottom_player_balances=bottom_balances_live,
            )
        else:
            wedges = get_wheel_wedges(is_eligible_for_bad_gamba, is_golden)

        # Apply active war effects to normal (non-golden, non-bankrupt) wheel
        _active_war_state = None
        _active_war_id = None
        _rebellion_svc_gamba = self.rebellion_service or getattr(self.bot, "rebellion_service", None)
        if _rebellion_svc_gamba and not is_golden and not is_eligible_for_bad_gamba:
            _active_war_state = await asyncio.to_thread(
                _rebellion_svc_gamba.get_active_war_effect, guild_id
            )
            if _active_war_state:
                _active_war_id = _active_war_state["war_id"]
                wedges = apply_war_effects(wedges, _active_war_state)

        # Mana effects
        mana_effects_service = getattr(self.bot, "mana_effects_service", None)
        effects = None
        if mana_effects_service:
            try:
                _fx = await asyncio.to_thread(mana_effects_service.get_effects, user_id, guild_id)
                # Only use effects if it's a real ManaEffects object with a color
                from domain.models.mana_effects import ManaEffects as _ManaEffectsType
                if isinstance(_fx, _ManaEffectsType):
                    effects = _fx
            except Exception:
                effects = None

        # Green variance compression
        if effects and effects.color == "Green" and not is_golden and not is_eligible_for_bad_gamba:
            compressed = []
            for label, value, color in wedges:
                if isinstance(value, int):
                    if value < effects.green_bankrupt_penalty:
                        compressed.append((str(effects.green_bankrupt_penalty), effects.green_bankrupt_penalty, color))
                    elif value > effects.green_max_wheel_win:
                        compressed.append((str(effects.green_max_wheel_win), effects.green_max_wheel_win, color))
                    else:
                        compressed.append((label, value, color))
                else:
                    compressed.append((label, value, color))
            wedges = compressed

        # Plains max wheel win cap
        if effects and effects.color == "White" and effects.plains_max_wheel_win is not None and not is_golden and not is_eligible_for_bad_gamba:
            capped = []
            for label, value, color in wedges:
                if isinstance(value, int) and value > effects.plains_max_wheel_win:
                    capped.append((str(effects.plains_max_wheel_win), effects.plains_max_wheel_win, color))
                else:
                    capped.append((label, value, color))
            wedges = capped

        # Mana bonus wedge: replace one generic wedge with color-specific bonus
        if effects and effects.color and not is_golden and not is_eligible_for_bad_gamba:
            wedges = apply_mana_wedge(wedges, effects.color)

        # Blue Gamba Scrying: show 2 outcomes, player picks
        _scrying_active = effects and effects.color == "Blue" and effects.blue_gamba_scrying and not is_golden and not is_eligible_for_bad_gamba
        if _scrying_active:
            idx_a = random.randint(0, len(wedges) - 1)
            idx_b = random.randint(0, len(wedges) - 1)
            while idx_b == idx_a and len(wedges) > 1:
                idx_b = random.randint(0, len(wedges) - 1)
            wedge_a = wedges[idx_a]
            wedge_b = wedges[idx_b]

            # Defer and present choice
            await interaction.response.defer()

            def _wedge_display(w):
                label, val, _ = w
                if isinstance(val, int):
                    return f"{'+' if val > 0 else ''}{val} JC" if val != 0 else "LOSE (0 JC)"
                return str(label)

            scry_view = ScryingView(
                option_a=_wedge_display(wedge_a),
                option_b=_wedge_display(wedge_b),
                user_id=user_id,
                timeout=30.0,
            )
            scry_embed = discord.Embed(
                title="\U0001f3dd\ufe0f MANA SCRYING",
                description=(
                    f"\U0001f52e {interaction.user.mention}, the Island reveals two fates:\n\n"
                    f"**A:** {_wedge_display(wedge_a)}\n"
                    f"**B:** {_wedge_display(wedge_b)}\n\n"
                    f"Choose wisely. *(Blue mana: winnings reduced by 25%)*"
                ),
                color=discord.Color.blue(),
            )
            scry_msg = await interaction.followup.send(embed=scry_embed, view=scry_view, wait=True)
            await scry_view.wait()

            if scry_view.chosen == "A":
                result_idx = idx_a
                result_wedge = wedge_a
            elif scry_view.chosen == "B":
                result_idx = idx_b
                result_wedge = wedge_b
            else:
                # Timeout: random pick
                result_idx = random.choice([idx_a, idx_b])
                result_wedge = wedges[result_idx]

            # Clean up scrying message
            try:
                await scry_msg.delete()
            except Exception:
                pass

            # Skip the normal defer (already deferred above)
            _scrying_deferred = True
        else:
            _scrying_deferred = False

        if not _scrying_active:
            result_idx = random.randint(0, len(wedges) - 1)
            result_wedge = wedges[result_idx % len(wedges)]

        # Plains Guardian Aura: BANKRUPT -> LOSE
        _guardian_activated = False
        if effects and effects.plains_guardian_aura and isinstance(result_wedge[1], int) and result_wedge[1] < 0:
            result_wedge = ("LOSE", 0, "#4a4a4a")
            _guardian_activated = True

        # Consume war spin if active
        if _active_war_id and _rebellion_svc_gamba:
            await asyncio.to_thread(
                _rebellion_svc_gamba.consume_war_spin, _active_war_id, guild_id, user_id
            )

        # Defer first - GIF generation can take a few seconds
        if not _scrying_deferred:
            await interaction.response.defer()

        # Generate the complete animation GIF (plays once, ~20 seconds)
        user_display = interaction.user.name
        gif_file = await asyncio.to_thread(
            self._create_wheel_gif_file, result_idx, user_display, is_eligible_for_bad_gamba, is_golden
        )

        # Send via followup (since we deferred)
        message = await interaction.followup.send(file=gif_file, wait=True)

        # Wait for GIF animation to complete before showing result
        # Animation timing:
        # - Fast spin: 45 frames * 50ms = 2.25s
        # - Medium: 15 frames * 100ms = 1.5s
        # - Fast: 40 frames * ~35ms = 1.4s
        # - Medium: 20 frames * 65ms = 1.3s
        # - Slow: 23 frames * 100ms = 2.3s
        # - Creep: 15 frames * ~150ms avg = 2.3s + pause
        # Total spinning: ~8-15s depending on ending style
        await asyncio.sleep(15.0)

        # Apply the result
        result_value = result_wedge[1]
        garnished_amount = 0
        new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)

        # Pre-resolution for interactive mechanics (TOWN_TRIAL / DISCOVER resolve to a final wedge)
        if result_value == "TOWN_TRIAL" and is_eligible_for_bad_gamba and interaction.channel:
            from utils.wheel_drawing import get_wheel_wedges as _gww
            eligible = [w for w in _gww(is_bankrupt=True)
                        if w[1] not in ("TOWN_TRIAL", "DISCOVER", "CHAIN_REACTION")]
            options = random.sample(eligible, min(3, len(eligible)))
            view = TownTrialView(options, timeout=300.0)
            trial_embed = discord.Embed(
                title="⚖️ TOWN TRIAL",
                description=(
                    f"⚖️ **TOWN TRIAL** — The town has **5 minutes** to decide "
                    f"{interaction.user.mention}'s fate!\n\nVote for a result:"
                ),
                color=discord.Color.from_str("#2a1a1a"),
            )
            trial_msg = await interaction.channel.send(embed=trial_embed, view=view)
            await view.wait()
            winner_idx = view.get_winner()
            if winner_idx is None:
                result_wedge = ("LOSE", 0, "#4a4a4a")
            else:
                result_wedge = options[winner_idx]
            result_value = result_wedge[1]
            winner_embed = discord.Embed(
                title="⚖️ THE TOWN HAS SPOKEN",
                description=(
                    f"The town decided: **{result_wedge[0]}** for {interaction.user.mention}!"
                ),
                color=discord.Color.red(),
            )
            await trial_msg.edit(embed=winner_embed, view=None)

        elif result_value == "DISCOVER" and is_eligible_for_bad_gamba and interaction.channel:
            from utils.wheel_drawing import get_wheel_wedges as _gww
            eligible = [w for w in _gww(is_bankrupt=True)
                        if w[1] not in ("TOWN_TRIAL", "DISCOVER", "CHAIN_REACTION")]
            options = random.sample(eligible, min(3, len(eligible)))
            view = DiscoverView(options, spinner_id=user_id, timeout=60.0)
            discover_embed = discord.Embed(
                title="🃏 DISCOVER",
                description=(
                    f"🃏 **DISCOVER** — {interaction.user.mention} must choose their fate!\n\n"
                    "You have **60 seconds** to choose:"
                ),
                color=discord.Color.from_str("#1a2a2a"),
            )
            discover_msg = await interaction.channel.send(embed=discover_embed, view=view)
            await view.wait()
            if view.chosen_idx is not None:
                result_wedge = options[view.chosen_idx]
            else:
                # Timeout: apply worst of 3
                result_wedge = min(options, key=_wedge_ev)
                timeout_embed = discord.Embed(
                    title="🃏 DISCOVER — TIMEOUT",
                    description=(
                        f"{interaction.user.mention} didn't choose in time. "
                        f"The worst fate applies: **{result_wedge[0]}**!"
                    ),
                    color=discord.Color.red(),
                )
                await discover_msg.edit(embed=timeout_embed, view=None)
            result_value = result_wedge[1]

        # Shell outcome tracking for embed
        shell_victim: discord.Member | None = None
        shell_victim_new_balance: int | None = None
        shell_amount: int = 0
        shell_self_hit: bool = False
        shell_missed: bool = False

        # New mechanic tracking for embed
        jailbreak_new_total: int = 0
        chain_value: int | None = None
        chain_username: str = "someone"
        emergency_count: int = 0
        emergency_total: int = 0
        commune_total: int = 0
        commune_count: int = 0
        pardon_consumed: bool = False

        # Golden wheel mechanic tracking
        heist_total: int = 0
        heist_count: int = 0
        market_crash_total: int = 0
        market_crash_count: int = 0
        compound_amount: int = 0
        trickle_total: int = 0
        trickle_count: int = 0
        dividend_amount: int = 0
        takeover_amount: int = 0
        takeover_victim_name: str = "rank #4"
        takeover_missed: bool = False

        if result_value == "RETRIBUTION":
            # War effect: steal from attackers, LOSE for everyone else
            _spinner_is_attacker = False
            if _active_war_id and _rebellion_svc_gamba:
                _spinner_is_attacker = await asyncio.to_thread(
                    _rebellion_svc_gamba.is_attacker, _active_war_id, user_id
                )
            if _spinner_is_attacker:
                await asyncio.to_thread(
                    self.player_service.adjust_balance, user_id, guild_id, -REBELLION_RETRIBUTION_STEAL
                )
                new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
            else:
                result_value = "LOSE"  # Non-attackers just get LOSE
                result_wedge = ("RETRIBUTION (miss)", 0, "#4a4a4a")

        elif result_value == "WAR SCAR 💀":
            # Broken wedge — value is already 0, nothing to pay out
            pass

        elif result_value == "WAR TROPHY 🏆":
            # Positive JC win handled by normal numeric path below
            pass

        elif result_value == "JAILBREAK":
            # Remove 1 penalty game (clamped at 0 inside add_penalty_games)
            if bankruptcy_service:
                jailbreak_new_total = await asyncio.to_thread(
                    bankruptcy_service.add_penalty_games, user_id, guild_id, -1
                )
            # No balance change

        elif result_value == "CHAIN_REACTION":
            # Copy the last normal-wheel spin result
            last_spin = await asyncio.to_thread(
                self.player_service.get_last_normal_wheel_spin, guild_id
            )
            if last_spin:
                chain_value = last_spin["result"]
                chained_uid = last_spin["discord_id"]
                if interaction.guild:
                    chained_member = interaction.guild.get_member(chained_uid)
                    chain_username = chained_member.display_name if chained_member else f"<@{chained_uid}>"
                else:
                    chain_username = f"<@{chained_uid}>"
                if isinstance(chain_value, int) and chain_value > 0:
                    garnishment_service_chain = getattr(self.bot, "garnishment_service", None)
                    if garnishment_service_chain and new_balance < 0:
                        result_chain = await asyncio.to_thread(
                            garnishment_service_chain.add_income, user_id, chain_value, guild_id
                        )
                        garnished_amount = result_chain.get("garnished", 0)
                        new_balance = result_chain.get("new_balance", new_balance + chain_value)
                    else:
                        await asyncio.to_thread(
                            self.player_service.adjust_balance, user_id, guild_id, chain_value
                        )
                        new_balance = await asyncio.to_thread(
                            self.player_service.get_balance, user_id, guild_id
                        )
                elif isinstance(chain_value, int) and chain_value < 0:
                    await asyncio.to_thread(
                        self.player_service.adjust_balance, user_id, guild_id, chain_value
                    )
                    new_balance = await asyncio.to_thread(
                        self.player_service.get_balance, user_id, guild_id
                    )
            # chain_value=None means no prior spin → no effect

        elif result_value == "EMERGENCY":
            # All players with positive balance lose min(balance, 10) JC; amount vanishes
            all_players_em = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=9999)
            )
            for p in all_players_em:
                if p.jopacoin_balance > 0:
                    loss = min(p.jopacoin_balance, 10)
                    await asyncio.to_thread(
                        self.player_service.adjust_balance, p.discord_id, guild_id, -loss
                    )
                    emergency_total += loss
                    emergency_count += 1
            # Re-fetch spinner's balance (may have changed)
            new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)

        elif result_value == "COMMUNE":
            # All positive-balance players donate 1 JC to the spinner
            all_players_cm = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=9999)
            )
            for p in all_players_cm:
                if p.discord_id != user_id and p.jopacoin_balance > 0:
                    await asyncio.to_thread(
                        self.player_service.adjust_balance, p.discord_id, guild_id, -1
                    )
                    commune_total += 1
                    commune_count += 1
            if commune_total > 0:
                garnishment_service_cm = getattr(self.bot, "garnishment_service", None)
                if garnishment_service_cm and new_balance < 0:
                    result_cm = await asyncio.to_thread(
                        garnishment_service_cm.add_income, user_id, commune_total, guild_id
                    )
                    garnished_amount = result_cm.get("garnished", 0)
                    new_balance = result_cm.get("new_balance", new_balance + commune_total)
                else:
                    await asyncio.to_thread(
                        self.player_service.adjust_balance, user_id, guild_id, commune_total
                    )
                    new_balance = await asyncio.to_thread(
                        self.player_service.get_balance, user_id, guild_id
                    )

        elif result_value == "COMEBACK":
            # Grant one-use pardon token: next BANKRUPT becomes LOSE
            await asyncio.to_thread(
                self.player_service.set_wheel_pardon, user_id, guild_id, 1
            )
            # No balance change

        # --- Mana bonus wedge outcomes ---
        elif result_value == "ERUPTION":
            # Red: Win 2x what previous spinner won (or 50 JC fallback)
            last_spin = await asyncio.to_thread(
                self.player_service.get_last_normal_wheel_spin, guild_id
            )
            eruption_amount = 50  # fallback
            if last_spin and isinstance(last_spin.get("result"), int):
                eruption_amount = abs(last_spin["result"]) * 2
                if eruption_amount == 0:
                    eruption_amount = 50
            garnishment_service = getattr(self.bot, "garnishment_service", None)
            if garnishment_service and new_balance < 0:
                _res = await asyncio.to_thread(
                    garnishment_service.add_income, user_id, eruption_amount, guild_id
                )
                garnished_amount = _res.get("garnished", 0)
                new_balance = _res.get("new_balance", new_balance + eruption_amount)
            else:
                await asyncio.to_thread(
                    self.player_service.adjust_balance, user_id, guild_id, eruption_amount
                )
                new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)

        elif result_value == "FROZEN_ASSETS":
            # Blue: Win 0 now, but next gamba guaranteed 50+ wedge (stored as pardon-like token)
            # For simplicity, store a "frozen_assets" flag in mana_shop_items
            db = getattr(self.bot, "db", None)
            if db:
                import time as _time_fa
                await asyncio.to_thread(
                    lambda: db.execute_write(
                        "INSERT INTO mana_shop_items (discord_id, guild_id, item_type, purchased_at, data) VALUES (?, ?, ?, ?, ?)",
                        (user_id, interaction.guild.id if interaction.guild else 0, "frozen_assets", int(_time_fa.time()), "pending"),
                    )
                )
            # No balance change

        elif result_value == "OVERGROWTH":
            # Green: Win 10 JC per game played this week
            player_obj = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
            games_this_week = 0
            if player_obj:
                import time as _time_og
                week_ago = int(_time_og.time()) - 7 * 24 * 3600
                try:
                    recent = await asyncio.to_thread(
                        functools.partial(self.player_service.get_recent_matches, user_id, guild_id, since=week_ago)
                    )
                    games_this_week = len(recent) if recent else 0
                except Exception:
                    games_this_week = max(1, (player_obj.wins + player_obj.losses) // 10)
            overgrowth_amount = max(10, games_this_week * 10)  # min 10 JC
            # Apply green gain cap
            if effects and effects.green_gain_cap is not None:
                overgrowth_amount = min(overgrowth_amount, effects.green_gain_cap)
            garnishment_service = getattr(self.bot, "garnishment_service", None)
            if garnishment_service and new_balance < 0:
                _res = await asyncio.to_thread(
                    garnishment_service.add_income, user_id, overgrowth_amount, guild_id
                )
                garnished_amount = _res.get("garnished", 0)
                new_balance = _res.get("new_balance", new_balance + overgrowth_amount)
            else:
                await asyncio.to_thread(
                    self.player_service.adjust_balance, user_id, guild_id, overgrowth_amount
                )
                new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)

        elif result_value == "SANCTUARY":
            # White: Win 0, but all spinners in next 24h get +5 JC
            db = getattr(self.bot, "db", None)
            if db:
                import time as _time_sc
                now_ts = int(_time_sc.time())
                expires = now_ts + 24 * 3600
                await asyncio.to_thread(
                    lambda: db.execute_write(
                        "INSERT INTO mana_shop_items (discord_id, guild_id, item_type, purchased_at, expires_at, data) VALUES (?, ?, ?, ?, ?, ?)",
                        (user_id, interaction.guild.id if interaction.guild else 0, "sanctuary", now_ts, expires, "active"),
                    )
                )
            # No balance change

        elif result_value == "DECAY":
            # Black: Top 3 wealthiest lose 40 JC each, #4 loses 50, spinner gains total
            top_4 = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=4)
            )
            decay_total = 0
            for i, p in enumerate(top_4):
                if p.discord_id == user_id:
                    continue
                loss = 50 if i == 3 else 40
                loss = min(loss, max(0, p.jopacoin_balance))
                if loss > 0:
                    await asyncio.to_thread(
                        self.player_service.adjust_balance, p.discord_id, guild_id, -loss
                    )
                    decay_total += loss
            if decay_total > 0:
                garnishment_service = getattr(self.bot, "garnishment_service", None)
                if garnishment_service and new_balance < 0:
                    _res = await asyncio.to_thread(
                        garnishment_service.add_income, user_id, decay_total, guild_id
                    )
                    garnished_amount = _res.get("garnished", 0)
                    new_balance = _res.get("new_balance", new_balance + decay_total)
                else:
                    await asyncio.to_thread(
                        self.player_service.adjust_balance, user_id, guild_id, decay_total
                    )
                    new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)

        elif result_value == "RED_SHELL":
            # Mario Kart Red Shell: Steal 1-5% of balance from player ranked above
            player_above = await asyncio.to_thread(
                self.player_service.get_player_above, user_id, guild_id
            )

            if player_above:
                pct_amount = max(1, int(player_above.jopacoin_balance * random.uniform(0.01, 0.05)))
                flat_amount = random.randint(2, 10)
                shell_amount = max(pct_amount, flat_amount)
                # Atomic steal from player above (can push victim below MAX_DEBT - intentional)
                steal_result = await asyncio.to_thread(
                    functools.partial(
                        self.player_service.steal_atomic,
                        thief_discord_id=user_id,
                        victim_discord_id=player_above.discord_id,
                        guild_id=guild_id,
                        amount=shell_amount,
                    )
                )
                shell_victim_new_balance = steal_result["victim_new_balance"]
                new_balance = steal_result["thief_new_balance"]
                # Try to get Discord member for mention
                if interaction.guild:
                    shell_victim = interaction.guild.get_member(player_above.discord_id)
            else:
                # User is #1 - shell misses
                shell_missed = True
                shell_amount = 0

        elif result_value == "BLUE_SHELL":
            # Mario Kart Blue Shell: Steal 1-5% of balance from richest player
            leaderboard = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=1)
            )

            if leaderboard and leaderboard[0].discord_id == user_id:
                # Self-hit! User is the richest - LOSE coins (can go below MAX_DEBT - intentional)
                shell_self_hit = True
                pct_amount = max(1, int(new_balance * random.uniform(0.01, 0.05)))
                flat_amount = random.randint(4, 20)
                shell_amount = max(pct_amount, flat_amount)
                await asyncio.to_thread(
                    self.player_service.adjust_balance, user_id, guild_id, -shell_amount
                )
                new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
                # Credit nonprofit fund with the self-hit loss
                if self.loan_service:
                    try:
                        await asyncio.to_thread(self.loan_service.add_to_nonprofit_fund, guild_id, shell_amount)
                    except Exception:
                        logger.warning("Failed to add blue shell self-hit to nonprofit fund")
            elif leaderboard:
                # Atomic steal from richest (can push victim below MAX_DEBT - intentional)
                richest = leaderboard[0]
                pct_amount = max(1, int(richest.jopacoin_balance * random.uniform(0.01, 0.05)))
                flat_amount = random.randint(4, 20)
                shell_amount = max(pct_amount, flat_amount)
                steal_result = await asyncio.to_thread(
                    functools.partial(
                        self.player_service.steal_atomic,
                        thief_discord_id=user_id,
                        victim_discord_id=richest.discord_id,
                        guild_id=guild_id,
                        amount=shell_amount,
                    )
                )
                shell_victim_new_balance = steal_result["victim_new_balance"]
                new_balance = steal_result["thief_new_balance"]
                # Try to get Discord member for mention
                if interaction.guild:
                    shell_victim = interaction.guild.get_member(richest.discord_id)
            else:
                # No players (shouldn't happen) - shell misses
                shell_missed = True
                shell_amount = 0

        elif result_value == "LIGHTNING_BOLT":
            # Lightning Bolt: tax ALL players in the guild, send to nonprofit
            all_players = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=9999)
            )
            lightning_pct = random.uniform(LIGHTNING_BOLT_PCT_MIN, LIGHTNING_BOLT_PCT_MAX)
            lightning_total = 0
            lightning_count = 0
            lightning_victims = []  # (name, amount, discord_id) for embed
            for p in all_players:
                if p.jopacoin_balance <= 0:
                    continue
                tax = max(LIGHTNING_BOLT_MIN_TAX, int(p.jopacoin_balance * lightning_pct))
                await asyncio.to_thread(
                    self.player_service.adjust_balance, p.discord_id, guild_id, -tax
                )
                lightning_total += tax
                lightning_count += 1
                lightning_victims.append((p.name, tax, p.discord_id))
            # Send total to nonprofit
            if self.loan_service and lightning_total > 0:
                try:
                    await asyncio.to_thread(
                        self.loan_service.add_to_nonprofit_fund, guild_id, lightning_total
                    )
                except Exception:
                    logger.warning("Failed to add lightning bolt tax to nonprofit fund")
            # Sort victims by amount descending, keep top 3
            lightning_victims.sort(key=lambda x: x[1], reverse=True)
            # Re-fetch spinner balance (they got taxed too)
            new_balance = await asyncio.to_thread(
                self.player_service.get_balance, user_id, guild_id
            )

        # --- Golden Wheel outcome handlers ---
        elif result_value == "HEIST":
            # Steal 3-8% (min 1 JC) from each of the bottom 30 positive-balance players
            bottom_players = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard_bottom, guild_id, limit=30, min_balance=1)
            )
            # Exclude the spinner themselves
            victims = [p for p in bottom_players if p.discord_id != user_id]
            heist_total = 0
            heist_count = 0
            for victim in victims:
                steal_amt = max(1, int(victim.jopacoin_balance * random.uniform(0.03, 0.08)))
                try:
                    await asyncio.to_thread(
                        functools.partial(
                            self.player_service.steal_atomic,
                            thief_discord_id=user_id,
                            victim_discord_id=victim.discord_id,
                            guild_id=guild_id,
                            amount=steal_amt,
                        )
                    )
                    heist_total += steal_amt
                    heist_count += 1
                except Exception as e:
                    logger.warning("Failed to execute heist steal from victim %s: %s", victim.discord_id, e)
            if heist_count == 0:
                # Fallback: no eligible victims
                heist_total = 20
                await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, 20)
            new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)

        elif result_value == "MARKET_CRASH":
            # Tax the other top-3 players 5-10% each; coins go to spinner
            top_3 = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=WHEEL_GOLDEN_TOP_N)
            )
            crash_victims = [p for p in top_3 if p.discord_id != user_id and p.jopacoin_balance > 0]
            market_crash_total = 0
            market_crash_count = 0
            for victim in crash_victims:
                tax_amt = max(1, int(victim.jopacoin_balance * random.uniform(0.05, 0.10)))
                try:
                    await asyncio.to_thread(
                        functools.partial(
                            self.player_service.steal_atomic,
                            thief_discord_id=user_id,
                            victim_discord_id=victim.discord_id,
                            guild_id=guild_id,
                            amount=tax_amt,
                        )
                    )
                    market_crash_total += tax_amt
                    market_crash_count += 1
                except Exception as e:
                    logger.warning("Failed to execute market crash tax on victim %s: %s", victim.discord_id, e)
            if market_crash_count == 0:
                # Fallback: spinner is only top-3 player
                market_crash_total = 25
                await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, 25)
            new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)

        elif result_value == "COMPOUND_INTEREST":
            # Earn 8% of spinner's own balance (min 5, max 150 JC)
            compound_amount = max(5, min(150, int(new_balance * 0.08)))
            garnishment_service = getattr(self.bot, "garnishment_service", None)
            if garnishment_service and new_balance < 0:
                result = await asyncio.to_thread(
                    garnishment_service.add_income, user_id, compound_amount, guild_id
                )
                garnished_amount = result.get("garnished", 0)
                new_balance = result.get("new_balance", new_balance + compound_amount)
            else:
                await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, compound_amount)
                new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)

        elif result_value == "TRICKLE_DOWN":
            # Tax all positive-balance players (spinner exempt) 1-3%, min 1 JC; coins go to spinner
            all_players_td = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=9999)
            )
            trickle_pct = random.uniform(LIGHTNING_BOLT_PCT_MIN, LIGHTNING_BOLT_PCT_MAX)
            trickle_total = 0
            trickle_count = 0
            for p in all_players_td:
                if p.discord_id == user_id or p.jopacoin_balance <= 0:
                    continue
                tax = max(1, int(p.jopacoin_balance * trickle_pct))
                await asyncio.to_thread(self.player_service.adjust_balance, p.discord_id, guild_id, -tax)
                trickle_total += tax
                trickle_count += 1
            if trickle_total > 0:
                garnishment_service = getattr(self.bot, "garnishment_service", None)
                if garnishment_service and new_balance < 0:
                    result = await asyncio.to_thread(
                        garnishment_service.add_income, user_id, trickle_total, guild_id
                    )
                    garnished_amount = result.get("garnished", 0)
                    new_balance = result.get("new_balance", new_balance + trickle_total)
                else:
                    await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, trickle_total)
                    new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)

        elif result_value == "DIVIDEND":
            # Earn 0.5% of total positive JC in guild (min 10 JC)
            total_guild_wealth = await asyncio.to_thread(
                self.player_service.get_total_positive_balance, guild_id
            )
            dividend_amount = max(10, int(total_guild_wealth * 0.005))
            garnishment_service = getattr(self.bot, "garnishment_service", None)
            if garnishment_service and new_balance < 0:
                result = await asyncio.to_thread(
                    garnishment_service.add_income, user_id, dividend_amount, guild_id
                )
                garnished_amount = result.get("garnished", 0)
                new_balance = result.get("new_balance", new_balance + dividend_amount)
            else:
                await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, dividend_amount)
                new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)

        elif result_value == "HOSTILE_TAKEOVER":
            # Steal 5-10% from rank #4 (just outside top 3)
            leaderboard_4 = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=WHEEL_GOLDEN_TOP_N + 1)
            )
            rank4 = leaderboard_4[WHEEL_GOLDEN_TOP_N] if len(leaderboard_4) > WHEEL_GOLDEN_TOP_N else None
            takeover_missed = False
            takeover_amount = 0
            takeover_victim_name = "rank #4"
            if rank4 and rank4.jopacoin_balance > 0:
                takeover_amount = max(1, int(rank4.jopacoin_balance * random.uniform(0.05, 0.10)))
                try:
                    steal_result = await asyncio.to_thread(
                        functools.partial(
                            self.player_service.steal_atomic,
                            thief_discord_id=user_id,
                            victim_discord_id=rank4.discord_id,
                            guild_id=guild_id,
                            amount=takeover_amount,
                        )
                    )
                    new_balance = steal_result["thief_new_balance"]
                    if interaction.guild:
                        rank4_member = interaction.guild.get_member(rank4.discord_id)
                        takeover_victim_name = rank4_member.mention if rank4_member else rank4.name
                except Exception:
                    takeover_missed = True
                    takeover_amount = 0
            else:
                # No rank 4 or rank 4 is in debt
                takeover_missed = True
                takeover_amount = 40
                await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, 40)
                new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)

        elif result_value in ("EXTEND_1", "EXTEND_2"):
            # Bankruptcy penalty extension slices (only appear on bankrupt wheel)
            games_to_add = 1 if result_value == "EXTEND_1" else 2
            if bankruptcy_service and penalty_games_remaining > 0:
                new_penalty_total = await asyncio.to_thread(
                    bankruptcy_service.add_penalty_games, user_id, guild_id, games_to_add
                )
            else:
                # Debt-only player (no formal penalty) — EXTEND is a no-op
                games_to_add = 0
                new_penalty_total = 0
            # No balance change, but penalty games increased (for penalty players)

        elif isinstance(result_value, int) and result_value > 0:
            # Blue mana: 25% reduction on gamba winnings
            if effects and effects.blue_gamba_reduction > 0 and result_value > 0:
                reduction = int(result_value * effects.blue_gamba_reduction)
                result_value = result_value - reduction

            # Positive result: use garnishment service if available
            garnishment_service = getattr(self.bot, "garnishment_service", None)
            if garnishment_service and new_balance < 0:
                # Player is in debt, apply garnishment
                result = await asyncio.to_thread(
                    garnishment_service.add_income, user_id, result_value, guild_id
                )
                garnished_amount = result.get("garnished", 0)
                new_balance = result.get("new_balance", new_balance + result_value)
            else:
                # Not in debt, add directly
                await asyncio.to_thread(
                    self.player_service.adjust_balance, user_id, guild_id, result_value
                )
                new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
        elif isinstance(result_value, int) and result_value < 0:
            # Check for COMEBACK pardon token before applying BANKRUPT penalty
            if is_eligible_for_bad_gamba:
                pardon_active = await asyncio.to_thread(
                    self.player_service.get_wheel_pardon, user_id, guild_id
                )
                if pardon_active:
                    await asyncio.to_thread(
                        self.player_service.set_wheel_pardon, user_id, guild_id, 0
                    )
                    result_value = 0  # Convert to LOSE (no balance change, normal cooldown)
                    result_wedge = (result_wedge[0], 0, result_wedge[2])
                    pardon_consumed = True
            if not pardon_consumed:
                # Bankrupt: subtract penalty (ignores MAX_DEBT floor - can go deeper into debt)
                await asyncio.to_thread(
                    self.player_service.adjust_balance, user_id, guild_id, result_value
                )
                new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
                # Add losses to nonprofit fund
                if self.loan_service:
                    try:
                        await asyncio.to_thread(
                            self.loan_service.add_to_nonprofit_fund, guild_id, abs(int(result_value))
                        )
                    except Exception:
                        logger.warning("Failed to add wheel loss to nonprofit fund")
        # result_value == 0: "Lose a Turn" - no balance change, but extended cooldown
        if result_value == 0:
            # Apply the 1-week penalty cooldown for "Lose a Turn"
            # Set the spin time forward so the effective cooldown is the penalty duration
            penalty_spin_time = int(now) + (WHEEL_LOSE_PENALTY_COOLDOWN - WHEEL_COOLDOWN_SECONDS)
            await asyncio.to_thread(
                self.player_service.set_last_wheel_spin, user_id, guild_id, penalty_spin_time
            )
            next_spin_time = int(now) + WHEEL_LOSE_PENALTY_COOLDOWN
        else:
            next_spin_time = int(now) + WHEEL_COOLDOWN_SECONDS

        # Log the wheel spin for history tracking
        # For shell outcomes, log the actual amount gained/lost
        if result_value == "RED_SHELL":
            log_result = shell_amount if not shell_missed else 0
        elif result_value == "BLUE_SHELL":
            if shell_missed:
                log_result = 0
            elif shell_self_hit:
                log_result = -shell_amount
            else:
                log_result = shell_amount
        elif result_value == "LIGHTNING_BOLT":
            log_result = 0
        elif result_value in ("EXTEND_1", "EXTEND_2"):
            log_result = 0
        elif result_value in ("JAILBREAK", "CHAIN_REACTION", "TOWN_TRIAL", "DISCOVER",
                              "EMERGENCY", "COMEBACK"):
            log_result = 0
        elif result_value == "COMMUNE":
            log_result = commune_total
        elif result_value == "HEIST":
            log_result = heist_total
        elif result_value == "MARKET_CRASH":
            log_result = market_crash_total
        elif result_value == "COMPOUND_INTEREST":
            log_result = compound_amount
        elif result_value == "TRICKLE_DOWN":
            log_result = trickle_total
        elif result_value == "DIVIDEND":
            log_result = dividend_amount
        elif result_value == "HOSTILE_TAKEOVER":
            log_result = 0 if takeover_missed else takeover_amount
        else:
            log_result = result_value if isinstance(result_value, int) else 0

        await asyncio.to_thread(
            functools.partial(
                self.player_service.log_wheel_spin,
                discord_id=user_id,
                guild_id=guild_id,
                result=log_result,
                spin_time=int(now),
                is_bankrupt=is_eligible_for_bad_gamba,
                is_golden=is_golden,
            )
        )

        # Send final result embed
        await asyncio.sleep(0.5)  # Brief pause before result reveal
        # For extension slices, pass the new penalty total
        extend_games_added = 0
        extend_new_total = 0
        if result_value in ("EXTEND_1", "EXTEND_2"):
            extend_games_added = games_to_add
            extend_new_total = new_penalty_total if "new_penalty_total" in locals() else extend_games_added

        result_embed = self._wheel_result_embed(
            result_wedge, new_balance, garnished_amount, next_spin_time,
            shell_victim=shell_victim,
            shell_victim_new_balance=shell_victim_new_balance,
            shell_amount=shell_amount,
            shell_self_hit=shell_self_hit,
            shell_missed=shell_missed,
            lightning_total=lightning_total if result_value == "LIGHTNING_BOLT" else 0,
            lightning_count=lightning_count if result_value == "LIGHTNING_BOLT" else 0,
            lightning_victims=lightning_victims if result_value == "LIGHTNING_BOLT" else None,
            extend_games_added=extend_games_added,
            extend_new_total=extend_new_total,
            is_bankrupt=is_eligible_for_bad_gamba,
            is_golden=is_golden,
            jailbreak_new_total=jailbreak_new_total,
            chain_value=chain_value,
            chain_username=chain_username,
            emergency_count=emergency_count,
            emergency_total=emergency_total,
            commune_total=commune_total,
            commune_count=commune_count,
            pardon_consumed=pardon_consumed,
            heist_total=heist_total,
            heist_count=heist_count,
            market_crash_total=market_crash_total,
            market_crash_count=market_crash_count,
            compound_amount=compound_amount,
            trickle_total=trickle_total,
            trickle_count=trickle_count,
            dividend_amount=dividend_amount,
            takeover_amount=takeover_amount,
            takeover_victim_name=takeover_victim_name,
            takeover_missed=takeover_missed,
        )

        # Add Guardian Aura notification if it triggered
        if _guardian_activated:
            result_embed.add_field(
                name="🌾 Guardian Aura",
                value="Plains mana converted BANKRUPT to LOSE!",
                inline=False,
            )

        # Add Blue reduction note if applicable
        if effects and effects.blue_gamba_reduction > 0 and isinstance(result_value, int) and result_value > 0:
            result_embed.add_field(
                name="🏝️ Blue Mana Tax",
                value=f"Winnings reduced by {int(effects.blue_gamba_reduction * 100)}%",
                inline=False,
            )

        await message.edit(embed=result_embed)

        # Neon Degen Terminal hook - at most ONE neon event per /gamba action
        neon = self._get_neon_service()
        if neon:
            candidates = []

            # Wheel result (for BANKRUPT results)
            if isinstance(result_wedge[1], int) and result_wedge[1] < 0:
                candidates.append(
                    lambda: neon.on_wheel_result(
                        user_id, guild_id,
                        result_value=result_wedge[1],
                        new_balance=new_balance,
                    )
                )

            # Lightning Bolt neon hook
            if result_value == "LIGHTNING_BOLT" and lightning_total > 0:
                _lt = lightning_total
                _lc = lightning_count
                candidates.append(
                    lambda: neon.on_lightning_bolt(user_id, guild_id, _lt, _lc)
                )

            # Degen milestone check after gamba
            degen_score = neon._get_degen_score(user_id, guild_id)
            if degen_score is not None and degen_score >= 90:
                candidates.append(
                    lambda: neon.on_degen_milestone(user_id, guild_id, degen_score)
                )

            if candidates:
                await self._send_first_neon_result(interaction, *candidates)

    @app_commands.command(name="badgamba", description="Test spin the bankruptcy wheel (visual only, no effects)")
    async def badgamba(self, interaction: discord.Interaction):
        """Spin the bankruptcy penalty wheel for testing - no balance or penalty changes."""
        if not await require_gamba_channel(interaction):
            return

        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None

        # Check if player is registered
        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
        if not player:
            await interaction.response.send_message(
                "You need to `/player register` before you can test the wheel.",
                ephemeral=True,
            )
            return

        # Always use bad gamba wheel for this test command
        is_eligible_for_bad_gamba = True
        wedges = get_wheel_wedges(is_eligible_for_bad_gamba)
        result_idx = random.randint(0, len(wedges) - 1)
        result_wedge = get_wedge_at_index_for_player(result_idx, is_eligible_for_bad_gamba)

        await interaction.response.defer()

        # Generate the bad gamba wheel animation
        user_display = interaction.user.name
        gif_file = await asyncio.to_thread(
            self._create_wheel_gif_file, result_idx, user_display, is_eligible_for_bad_gamba
        )

        message = await interaction.followup.send(file=gif_file, wait=True)

        # Wait for animation
        await asyncio.sleep(15.0)

        # Build a test result embed (no actual changes)
        result_value = result_wedge[1]
        label = result_wedge[0]

        if result_value == "JAILBREAK":
            title = "🔓 [TEST] JAILBREAK! 🔓"
            description = f"**{label}**\n\nThis would remove 1 penalty game.\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.from_str("#0a2a0a")
        elif result_value == "CHAIN_REACTION":
            title = "⛓️ [TEST] CHAIN REACTION! ⛓️"
            description = f"**{label}**\n\nThis would copy the last normal wheel spin.\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.from_str("#1a1a3a")
        elif result_value == "TOWN_TRIAL":
            title = "⚖️ [TEST] TOWN TRIAL! ⚖️"
            description = f"**{label}**\n\nThis would trigger a 5-minute server vote (3 options).\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.from_str("#2a1a1a")
        elif result_value == "DISCOVER":
            title = "🃏 [TEST] DISCOVER! 🃏"
            description = f"**{label}**\n\nThis would show 3 options for you to pick from (60s).\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.from_str("#1a2a2a")
        elif result_value == "EMERGENCY":
            title = "🚨 [TEST] EMERGENCY! 🚨"
            description = f"**{label}**\n\nThis would drain up to 10 JC from all positive-balance players.\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.from_str("#2a1a00")
        elif result_value == "COMMUNE":
            title = "🫳 [TEST] SEIZE THE MEANS! 🫳"
            description = f"**{label}**\n\nThis would collect 1 JC from every positive-balance player and give it to you.\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.from_str("#1a2a1a")
        elif result_value == "COMEBACK":
            title = "🃏 [TEST] CLUTCH SAVE! 🃏"
            description = f"**{label}**\n\nThis would grant you a one-use pardon: your next BANKRUPT becomes a LOSE instead.\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.from_str("#0a1a2a")
        elif result_value in ("EXTEND_1", "EXTEND_2"):
            games = 1 if result_value == "EXTEND_1" else 2
            title = "⛓️ [TEST] PENALTY EXTENDED! ⛓️"
            description = (
                f"**{label} GAME{'S' if games > 1 else ''}**\n\n"
                f"This would add **{games}** penalty game{'s' if games > 1 else ''}!\n\n"
                f"*This is a test spin - no changes applied.*"
            )
            color = discord.Color.dark_red()
        elif result_value == "RED_SHELL":
            title = "🔴 [TEST] RED SHELL! 🔴"
            description = f"**{label}**\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.red()
        elif result_value == "BLUE_SHELL":
            title = "🔵 [TEST] BLUE SHELL! 🔵"
            description = f"**{label}**\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.blue()
        elif result_value == "LIGHTNING_BOLT":
            title = "⚡ [TEST] LIGHTNING BOLT! ⚡"
            description = f"**{label}**\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.from_str("#f39c12")
        elif isinstance(result_value, int) and result_value > 0:
            if result_value == 1:
                title = "🪙 [TEST] One Coin."
                description = f"**1**\n\nThe wheel took pity on you. One coin.\n\n*This is a test spin - no changes applied.*"
            elif result_value == 2:
                title = "🪙 [TEST] Two Coins."
                description = f"**2**\n\nEven charity has standards. Here's 2.\n\n*This is a test spin - no changes applied.*"
            else:
                title = "🎉 [TEST] Winner!"
                description = f"**+{result_value} JC**\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.green()
        elif isinstance(result_value, int) and result_value < 0:
            title = "💀 [TEST] BANKRUPT! 💀"
            description = f"**{label}**\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.red()
        else:
            title = "🚫 [TEST] LOSE A TURN 🚫"
            description = f"**{label}**\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.dark_gray()

        embed = discord.Embed(title=title, description=description, color=color)
        embed.set_footer(text="Bankruptcy Wheel Test - No effects applied")

        await asyncio.sleep(0.5)
        await message.edit(embed=embed)

    @app_commands.command(name="ashfansgamba", description="Test spin the golden wheel (visual only, no effects)")
    async def ashfansgamba(self, interaction: discord.Interaction):
        """Spin the golden wheel for testing - no balance changes applied."""
        if not await require_gamba_channel(interaction):
            return

        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None

        # Check if player is registered
        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
        if not player:
            await interaction.response.send_message(
                "You need to `/player register` before you can test the wheel.",
                ephemeral=True,
            )
            return

        # Always use golden wheel for this test command, with live OVEREXTENDED calculation
        top_n_test = await asyncio.to_thread(
            functools.partial(self.player_service.get_leaderboard, guild_id, limit=WHEEL_GOLDEN_TOP_N + 1)
        )
        rank_next_test = top_n_test[WHEEL_GOLDEN_TOP_N] if len(top_n_test) > WHEEL_GOLDEN_TOP_N else None
        rank_next_bal_test = (
            rank_next_test.jopacoin_balance
            if rank_next_test and rank_next_test.jopacoin_balance > 0
            else None
        )
        total_positive_test = await asyncio.to_thread(
            self.player_service.get_total_positive_balance, guild_id
        )
        bottom_players_test = await asyncio.to_thread(
            functools.partial(self.player_service.get_leaderboard_bottom, guild_id, limit=30, min_balance=1)
        )
        bottom_bals_test = [p.jopacoin_balance for p in bottom_players_test if p.discord_id != user_id]
        other_top_bals_test = [
            p.jopacoin_balance for p in top_n_test[:WHEEL_GOLDEN_TOP_N]
            if p.discord_id != user_id and p.jopacoin_balance > 0
        ]
        wedges = compute_live_golden_wedges(
            spinner_balance=player.jopacoin_balance,
            other_top_balances=other_top_bals_test,
            rank_next_balance=rank_next_bal_test,
            total_positive_balance=total_positive_test,
            bottom_player_balances=bottom_bals_test,
        )
        result_idx = random.randint(0, len(wedges) - 1)
        result_wedge = wedges[result_idx % len(wedges)]

        await interaction.response.defer()

        user_display = interaction.user.name
        gif_file = await asyncio.to_thread(
            self._create_wheel_gif_file, result_idx, user_display, False, True
        )

        message = await interaction.followup.send(file=gif_file, wait=True)

        # Wait for animation
        await asyncio.sleep(15.0)

        result_value = result_wedge[1]
        label = result_wedge[0]

        if result_value == "HEIST":
            title = "🥷 [TEST] HEIST! 🥷"
            description = f"**{label}**\n\nThis would steal 3–8% (min 1 JC) from each of the bottom 30 positive-balance players.\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.from_str("#7a5c00")
        elif result_value == "MARKET_CRASH":
            title = "📉 [TEST] MARKET CRASH! 📉"
            description = f"**{label}**\n\nThis would tax the other top-3 players 5–10% each, sending the coins to you.\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.from_str("#8a4000")
        elif result_value == "COMPOUND_INTEREST":
            title = "📈 [TEST] COMPOUND INTEREST! 📈"
            description = f"**{label}**\n\nThis would earn 8% of your own balance (min 5, max 150 JC).\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.from_str("#6b8c00")
        elif result_value == "TRICKLE_DOWN":
            title = "🌧️ [TEST] TRICKLE DOWN! 🌧️"
            description = f"**{label}**\n\nThis would tax all positive-balance players 1–3% (min 1 JC each), sending the coins to you.\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.from_str("#5c7a00")
        elif result_value == "DIVIDEND":
            title = "💰 [TEST] DIVIDEND! 💰"
            description = f"**{label}**\n\nThis would earn 0.5% of the total positive JC in the server (min 10 JC).\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.from_str("#4a7000")
        elif result_value == "HOSTILE_TAKEOVER":
            title = "🏦 [TEST] HOSTILE TAKEOVER! 🏦"
            description = f"**{label}**\n\nThis would steal 5–10% from the player ranked just outside the top 3.\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.from_str("#6a2a80")
        elif result_value == "RED_SHELL":
            title = "🔴 [TEST] RED SHELL! 🔴"
            description = f"**{label}**\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.from_str("#cc6600")
        elif result_value == "BLUE_SHELL":
            title = "🔵 [TEST] BLUE SHELL! 🔵"
            description = f"**{label}**\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.from_str("#4080c0")
        elif result_value == 250:
            title = "👑 [TEST] CROWN JEWEL! 👑"
            description = f"**+250 JC**\n\nThe ultimate golden prize.\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.from_str("#fffacd")
        elif isinstance(result_value, int) and result_value > 0:
            title = f"✨ [TEST] +{result_value} JC ✨"
            description = f"**+{result_value} JC**\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.from_str("#daa520")
        elif isinstance(result_value, int) and result_value < 0:
            title = "📊 [TEST] OVEREXTENDED! 📊"
            description = f"**{result_value} JC**\n\nPride goes before the fall.\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.from_str("#4a3000")
        else:
            title = "🚫 [TEST] Unknown outcome"
            description = f"**{label}**\n\n*This is a test spin - no changes applied.*"
            color = discord.Color.dark_gray()

        embed = discord.Embed(title=title, description=description, color=color)
        embed.set_footer(text="Golden Wheel Test - No effects applied")

        await asyncio.sleep(0.5)
        await message.edit(embed=embed)

    @app_commands.command(name="tip", description="Give jopacoin to another player")
    @app_commands.describe(
        player="Player to tip",
        amount="Amount of jopacoin to give",
    )
    async def tip(
        self,
        interaction: discord.Interaction,
        player: discord.Member,
        amount: int,
    ):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="tip",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=5,
            per_seconds=10,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"Please wait {rl.retry_after_seconds}s before using `/tip` again.",
                ephemeral=True,
            )
            return

        # Always public since giving to another player
        if not await safe_defer(interaction, ephemeral=False):
            return

        # Validate amount
        if amount <= 0:
            await interaction.followup.send(
                "Amount must be positive.",
                ephemeral=True,
            )
            return

        # Check if tipping themselves
        if player.id == interaction.user.id:
            await interaction.followup.send(
                "You cannot tip yourself.",
                ephemeral=True,
            )
            return

        # Extract guild_id early for consistent audit trail
        guild_id = interaction.guild.id if interaction.guild else None

        # Check if both players are registered
        sender = await asyncio.to_thread(self.player_service.get_player, interaction.user.id, guild_id)
        recipient = await asyncio.to_thread(self.player_service.get_player, player.id, guild_id)

        if not sender:
            await interaction.followup.send(
                "You need to `/player register` before you can tip.",
                ephemeral=True,
            )
            return

        if not recipient:
            await interaction.followup.send(
                f"{player.mention} is not registered.",
                ephemeral=True,
            )
            return

        # Get mana effects for sender
        mana_effects_service = getattr(self.bot, "mana_effects_service", None)
        effects = None
        if mana_effects_service:
            try:
                from domain.models.mana_effects import ManaEffects as _METip
                _fx_tip = await asyncio.to_thread(mana_effects_service.get_effects, interaction.user.id, guild_id)
                if isinstance(_fx_tip, _METip):
                    effects = _fx_tip
            except Exception:
                effects = None

        # Calculate fee (1% minimum 1 coin, rounded up)
        # Plains: free tips (0% fee)
        if effects and effects.color == "White" and effects.plains_tip_fee_rate is not None:
            fee = 0
        else:
            fee = max(1, math.ceil(amount * TIP_FEE_RATE))
        total_cost = amount + fee

        # Check sender balance first (most fundamental constraint)
        sender_balance = await asyncio.to_thread(self.player_service.get_balance, interaction.user.id, guild_id)
        if sender_balance < total_cost:
            await interaction.followup.send(
                f"Insufficient balance. You need {total_cost} {JOPACOIN_EMOTE} "
                f"({amount} tip + {fee} fee). You have {sender_balance} {JOPACOIN_EMOTE}.",
                ephemeral=True,
            )
            return

        # Check if sender has outstanding loan (blocked from tipping)
        if self.loan_service:
            loan_state = await asyncio.to_thread(self.loan_service.get_state, interaction.user.id, guild_id)
            if loan_state.has_outstanding_loan:
                await interaction.followup.send(
                    f"You cannot tip while you have an outstanding loan. "
                    f"Play a match to repay your loan ({loan_state.outstanding_total} {JOPACOIN_EMOTE}).",
                    ephemeral=True,
                )
                return

        # Perform atomic transfer (fee goes to nonprofit)
        try:
            result = await asyncio.to_thread(
                functools.partial(
                    self.player_service.tip_atomic,
                    from_discord_id=interaction.user.id,
                    to_discord_id=player.id,
                    guild_id=guild_id,
                    amount=amount,
                    fee=fee,
                )
            )
        except ValueError as exc:
            # Transfer failed - user error (insufficient funds, not found, etc.)
            await interaction.followup.send(f"{exc}", ephemeral=True)
            return
        except Exception as exc:
            # Unexpected error during transfer
            logger.error(f"Failed to process tip transfer: {exc}", exc_info=True)
            await interaction.followup.send(
                "Failed to process tip. Please try again.",
                ephemeral=True,
            )
            return

        # Add fee to nonprofit fund (non-critical - failure here doesn't affect the tip)
        if self.loan_service and fee > 0:
            try:
                await asyncio.to_thread(self.loan_service.add_to_nonprofit_fund, guild_id, fee)
            except Exception as nonprofit_exc:
                logger.warning(f"Failed to add tip fee to nonprofit fund: {nonprofit_exc}")

        # Mana post-effects on tip
        mana_notes = []
        if effects and mana_effects_service:
            # Green steady bonus: recipient gets +1 JC
            if effects.green_steady_bonus > 0:
                await asyncio.to_thread(self.player_service.adjust_balance, player.id, guild_id, effects.green_steady_bonus)
                mana_notes.append(f"🌲 +{effects.green_steady_bonus} bonus to recipient")

            # Swamp self-tax
            if effects.swamp_self_tax > 0:
                await asyncio.to_thread(self.player_service.adjust_balance, interaction.user.id, guild_id, -effects.swamp_self_tax)
                mana_notes.append(f"🌿 Swamp tax: -{effects.swamp_self_tax}")

            # Swamp siphon
            if effects.swamp_siphon:
                siphon = await asyncio.to_thread(mana_effects_service.execute_siphon, interaction.user.id, guild_id)
                if siphon:
                    mana_notes.append(f"🌿 Siphon: +{siphon['amount']}")

            # Plains tithe on the tip received
            if effects.plains_tithe_rate > 0:
                tithe = max(1, int(amount * effects.plains_tithe_rate))
                await asyncio.to_thread(self.player_service.adjust_balance, interaction.user.id, guild_id, -tithe)
                # Add tithe to nonprofit
                if self.loan_service:
                    try:
                        await asyncio.to_thread(self.loan_service.add_to_nonprofit_fund, guild_id, tithe)
                    except Exception:
                        pass
                mana_notes.append(f"🌾 Tithe: -{tithe}")

        mana_suffix = ""
        if mana_notes:
            mana_suffix = "\n" + " | ".join(mana_notes)

        # Transfer succeeded - send success message
        await interaction.followup.send(
            f"{interaction.user.mention} tipped {amount} {JOPACOIN_EMOTE} to {player.mention}! "
            f"({fee} {JOPACOIN_EMOTE} fee to nonprofit){mana_suffix}",
            ephemeral=False,
        )

        # Neon Degen Terminal hook
        neon = self._get_neon_service()
        if neon:
            neon_result = await neon.on_tip(
                interaction.user.id, guild_id,
                sender_name=interaction.user.name,
                recipient_name=player.name,
                amount=amount,
                fee=fee,
            )
            await send_neon_result(interaction, neon_result)

        # Log the transaction (non-critical - failure here doesn't affect the tip)
        if self.tip_service:
            try:
                await asyncio.to_thread(
                    functools.partial(
                        self.tip_service.log_tip,
                        sender_id=interaction.user.id,
                        recipient_id=player.id,
                        amount=amount,
                        fee=fee,
                        guild_id=guild_id,
                    )
                )
            except Exception as log_exc:
                # Log failure but don't notify user - tip already succeeded
                logger.warning(f"Failed to log tip transaction: {log_exc}")

    @app_commands.command(name="paydebt", description="Help another player pay off their debt")
    @app_commands.describe(
        player="Player whose debt to pay",
        amount="Amount of jopacoin to pay toward their debt",
    )
    async def paydebt(
        self,
        interaction: discord.Interaction,
        player: discord.Member,
        amount: int,
    ):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="paydebt",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=5,
            per_seconds=10,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"Please wait {rl.retry_after_seconds}s before using `/paydebt` again.",
                ephemeral=True,
            )
            return

        # Always public since helping another player
        if not await safe_defer(interaction, ephemeral=False):
            return

        guild_id = guild.id if guild else None
        try:
            result = await asyncio.to_thread(
                functools.partial(
                    self.player_service.pay_debt_atomic,
                    from_discord_id=interaction.user.id,
                    to_discord_id=player.id,
                    guild_id=guild_id,
                    amount=amount,
                )
            )

            await interaction.followup.send(
                f"{interaction.user.mention} paid {result['amount_paid']} {JOPACOIN_EMOTE} "
                f"toward {player.mention}'s debt!",
                ephemeral=False,
            )
        except ValueError as exc:
            await interaction.followup.send(f"{exc}", ephemeral=True)

    @app_commands.command(
        name="bankruptcy",
        description="Declare bankruptcy to clear your debt (once per week, with penalties)",
    )
    async def bankruptcy(self, interaction: discord.Interaction):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="bankruptcy",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=2,
            per_seconds=30,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"The bankruptcy court requires you to wait {rl.retry_after_seconds}s "
                "before filing again.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=False):
            return

        if not self.bankruptcy_service:
            await interaction.followup.send("Bankruptcy service is not available.", ephemeral=True)
            return

        user_id = interaction.user.id
        guild_id = guild.id if guild else None

        # Check if player is registered
        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
        if not player:
            await interaction.followup.send(
                "You need to `/player register` before you can declare bankruptcy. "
                "Though maybe that's a good sign you shouldn't gamble.",
                ephemeral=True,
            )
            return

        # Check if bankruptcy is allowed
        check = await asyncio.to_thread(self.bankruptcy_service.validate_bankruptcy, user_id, guild_id)

        if not check.success:
            from services import error_codes
            if check.error_code == error_codes.NOT_IN_DEBT:
                message = random.choice(BANKRUPTCY_DENIED_MESSAGES)
                balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
                await interaction.followup.send(
                    f"{interaction.user.mention} tried to declare bankruptcy...\n\n"
                    f"{message}\n\nTheir balance: {balance} {JOPACOIN_EMOTE}",
                    ephemeral=False,
                )
                return
            elif check.error_code == error_codes.BANKRUPTCY_COOLDOWN:
                message = random.choice(BANKRUPTCY_COOLDOWN_MESSAGES)
                state = await asyncio.to_thread(self.bankruptcy_service.get_state, user_id, guild_id)
                cooldown_ends = state.cooldown_ends_at
                cooldown_str = f"<t:{cooldown_ends}:R>" if cooldown_ends else "soon"
                await interaction.followup.send(
                    f"{interaction.user.mention} tried to declare bankruptcy again...\n\n"
                    f"{message}\n\nThey can file again {cooldown_str}.",
                    ephemeral=False,
                )
                # Neon Degen Terminal hook (cooldown hit)
                neon = self._get_neon_service()
                if neon:
                    try:
                        neon_result = await neon.on_cooldown_hit(user_id, guild_id, "bankruptcy")
                        await send_neon_result(interaction, neon_result)
                    except Exception as e:
                        logger.debug("Failed to send bankruptcy cooldown neon result: %s", e)
                return

        # Declare bankruptcy
        result = await asyncio.to_thread(self.bankruptcy_service.execute_bankruptcy, user_id, guild_id)

        if not result.success:
            await interaction.followup.send(
                "Something went wrong with your bankruptcy filing. The universe is cruel.",
                ephemeral=True,
            )
            return

        decl = result.value

        # Swamp mana: reduced bankruptcy penalty (3 games instead of 5)
        _mana_fx_bk = getattr(self.bot, "mana_effects_service", None)
        if _mana_fx_bk:
            try:
                from domain.models.mana_effects import ManaEffects as _MEBk
                _bk_effects = await asyncio.to_thread(_mana_fx_bk.get_effects, user_id, guild_id)
                if not isinstance(_bk_effects, _MEBk):
                    _bk_effects = None
            except Exception:
                _bk_effects = None
            if _bk_effects and _bk_effects.color == "Black" and _bk_effects.swamp_bankruptcy_games < decl.penalty_games:
                # Reduce penalty games to swamp level
                reduction = decl.penalty_games - _bk_effects.swamp_bankruptcy_games
                await asyncio.to_thread(
                    self.bankruptcy_service.add_penalty_games, user_id, guild_id, -reduction
                )
                decl = type(decl)(
                    debt_cleared=decl.debt_cleared,
                    penalty_games=_bk_effects.swamp_bankruptcy_games,
                    penalty_rate=decl.penalty_rate,
                    new_balance=decl.new_balance,
                )

        # Format success message
        message = random.choice(BANKRUPTCY_SUCCESS_MESSAGES).format(
            debt=decl.debt_cleared,
            games=decl.penalty_games,
            rate=int(decl.penalty_rate * 100),
        )

        # Try to get AI-generated flavor text
        ai_flavor = None
        if self.flavor_text_service:
            try:
                ai_flavor = await self.flavor_text_service.generate_event_flavor(
                    guild_id=guild_id,
                    event=FlavorEvent.BANKRUPTCY_DECLARED,
                    discord_id=user_id,
                    event_details={
                        "debt_cleared": decl.debt_cleared,
                        "penalty_games": decl.penalty_games,
                        "penalty_rate": decl.penalty_rate,
                    },
                )
            except Exception as e:
                logger.warning(f"Failed to generate AI flavor for bankruptcy: {e}")

        penalty_rate_pct = int(decl.penalty_rate * 100)
        flavor_line = f"\n\n*{ai_flavor}*" if ai_flavor else ""
        await interaction.followup.send(
            f"**{interaction.user.mention} HAS DECLARED BANKRUPTCY**\n\n"
            f"{message}{flavor_line}\n\n"
            f"**Details:**\n"
            f"Debt cleared: {decl.debt_cleared} {JOPACOIN_EMOTE}\n"
            f"Penalty: {penalty_rate_pct}% win bonus until you **WIN** {decl.penalty_games} games\n"
            f"New balance: 0 {JOPACOIN_EMOTE}",
            ephemeral=False,
        )

        # Neon Degen Terminal hook - at most ONE neon event per /bankruptcy action
        neon = self._get_neon_service()
        if neon:
            filing_number = await self._get_bankruptcy_filing_number(user_id, guild_id)
            degen_score = neon._get_degen_score(user_id, guild_id)

            candidates = [
                lambda: neon.on_bankruptcy(
                    user_id, guild_id,
                    debt_cleared=decl.debt_cleared,
                    filing_number=filing_number,
                ),
            ]
            if degen_score is not None and degen_score >= 90:
                candidates.append(
                    lambda: neon.on_degen_milestone(user_id, guild_id, degen_score)
                )

            await self._send_first_neon_result(interaction, *candidates)

    async def _get_bankruptcy_filing_number(self, discord_id: int, guild_id: int | None) -> int:
        """Get the current bankruptcy filing number for a user."""
        try:
            gambling_stats = getattr(self.bot, "gambling_stats_service", None)
            if gambling_stats:
                return await asyncio.to_thread(
                    gambling_stats.get_player_bankruptcy_count, discord_id, guild_id
                )
        except Exception as e:
            logger.warning("Failed to get bankruptcy filing number: %s", e)
        return 1

    @app_commands.command(name="loan", description="Borrow jopacoin (with a fee)")
    @app_commands.describe(amount="Amount to borrow (max 100)")
    async def loan(
        self,
        interaction: discord.Interaction,
        amount: int,
    ):
        """Take out a loan. You receive the full amount but owe amount + fee."""
        if not self.loan_service:
            await interaction.response.send_message(
                "Loan service is not available.", ephemeral=True
            )
            return

        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None

        # Check if registered
        if not await asyncio.to_thread(self.player_service.get_player, user_id, guild_id):
            await interaction.response.send_message(
                "You need to `/player register` before taking loans.", ephemeral=True
            )
            return

        # Defer early - AI flavor text calls below can take several seconds
        await interaction.response.defer()

        # Check eligibility
        from services import error_codes as _ec
        check = await asyncio.to_thread(self.loan_service.validate_loan, user_id, amount, guild_id)

        if not check.success:
            if check.error_code == _ec.LOAN_ALREADY_EXISTS:
                state = await asyncio.to_thread(self.loan_service.get_state, user_id, guild_id)
                await interaction.followup.send(
                    f"You already have an outstanding loan of **{state.outstanding_total}** {JOPACOIN_EMOTE} "
                    f"(principal: {state.outstanding_principal}, fee: {state.outstanding_fee}).\n\n"
                    "Repay it by playing in a match first!",
                )
                return
            elif check.error_code == _ec.COOLDOWN_ACTIVE:
                state = await asyncio.to_thread(self.loan_service.get_state, user_id, guild_id)
                remaining = state.cooldown_ends_at - int(__import__("time").time())
                hours = remaining // 3600
                minutes = (remaining % 3600) // 60
                # Try AI flavor, fallback to static message
                msg = None
                if self.flavor_text_service:
                    try:
                        msg = await self.flavor_text_service.generate_event_flavor(
                            guild_id=guild_id,
                            event=FlavorEvent.LOAN_COOLDOWN,
                            discord_id=user_id,
                            event_details={
                                "cooldown_remaining_hours": hours,
                                "cooldown_remaining_minutes": minutes,
                                "requested_amount": amount,
                            },
                        )
                    except Exception as e:
                        logger.warning(f"Failed to generate AI flavor for loan cooldown: {e}")
                if not msg:
                    msg = random.choice(LOAN_DENIED_COOLDOWN_MESSAGES)
                await interaction.followup.send(
                    f"{msg}\n\n⏳ Cooldown ends in **{hours}h {minutes}m**.",
                )
                # Neon Degen Terminal hook (cooldown hit)
                neon = self._get_neon_service()
                if neon:
                    try:
                        neon_result = await neon.on_cooldown_hit(user_id, guild_id, "loan")
                        await send_neon_result(interaction, neon_result)
                    except Exception as e:
                        logger.debug("Failed to send loan cooldown neon result: %s", e)
                return
            elif check.error_code == _ec.LOAN_AMOUNT_EXCEEDED:
                await interaction.followup.send(check.error)
                return
            else:
                await interaction.followup.send(check.error)
                return

        # Take the loan
        loan_result = await asyncio.to_thread(self.loan_service.execute_loan, user_id, amount, guild_id)

        if not loan_result.success:
            await interaction.followup.send(
                "Failed to process loan. Please try again.", ephemeral=True
            )
            return

        result = loan_result.value

        fee_pct = int(LOAN_FEE_RATE * 100)

        # Try to get AI-generated flavor text
        ai_flavor = None
        if self.flavor_text_service:
            event_type = (
                FlavorEvent.NEGATIVE_LOAN
                if result.was_negative_loan
                else FlavorEvent.LOAN_TAKEN
            )
            try:
                ai_flavor = await self.flavor_text_service.generate_event_flavor(
                    guild_id=guild_id,
                    event=event_type,
                    discord_id=user_id,
                    event_details={
                        "amount": result.amount,
                        "fee": result.fee,
                        "total_owed": result.total_owed,
                        "new_balance": result.new_balance,
                        "total_loans_taken": result.total_loans_taken,
                        "was_negative_loan": result.was_negative_loan,
                    },
                )
            except Exception as e:
                logger.warning(f"Failed to generate AI flavor for loan: {e}")

        # Check if this was a negative loan (peak degen behavior)
        if result.was_negative_loan:
            # Use AI flavor as main message if available, otherwise fallback to static
            if ai_flavor:
                msg = ai_flavor
            else:
                msg = random.choice(NEGATIVE_LOAN_MESSAGES).format(
                    amount=result.amount,
                    emote=JOPACOIN_EMOTE,
                )
            embed = discord.Embed(
                title="🎪 LEGENDARY DEGEN MOVE 🎪",
                description=msg,
                color=0x9B59B6,  # Purple for peak degen
            )
            embed.add_field(
                name="The Damage",
                value=(
                    f"Borrowed: **{result.amount}** {JOPACOIN_EMOTE}\n"
                    f"Fee ({fee_pct}%): **{result.fee}** {JOPACOIN_EMOTE}\n"
                    f"Total Owed: **{result.total_owed}** {JOPACOIN_EMOTE}\n"
                    f"New Balance: **{result.new_balance}** {JOPACOIN_EMOTE}"
                ),
                inline=False,
            )
            embed.add_field(
                name="⚠️ Repayment",
                value="You will repay the full amount **after your next match**.",
                inline=False,
            )
            embed.set_footer(
                text="Loan #{} | Go bet it all, you beautiful degen".format(
                    result.total_loans_taken
                )
            )
        else:
            # Use AI flavor as main message if available, otherwise fallback to static
            if ai_flavor:
                msg = ai_flavor
            else:
                msg = random.choice(LOAN_SUCCESS_MESSAGES).format(
                    amount=result.amount,
                    owed=result.total_owed,
                    fee=result.fee,
                    emote=JOPACOIN_EMOTE,
                )
            embed = discord.Embed(
                title="🏦 Loan Approved",
                description=msg,
                color=0x2ECC71,  # Green
            )
            embed.add_field(
                name="Details",
                value=(
                    f"Borrowed: **{result.amount}** {JOPACOIN_EMOTE}\n"
                    f"Fee ({fee_pct}%): **{result.fee}** {JOPACOIN_EMOTE}\n"
                    f"Total Owed: **{result.total_owed}** {JOPACOIN_EMOTE}\n"
                    f"New Balance: **{result.new_balance}** {JOPACOIN_EMOTE}"
                ),
                inline=False,
            )
            embed.add_field(
                name="📅 Repayment",
                value="You will repay the full amount **after your next match**.",
                inline=False,
            )
            embed.set_footer(
                text=f"Loan #{result.total_loans_taken} | Fee donated to Gambling Addiction Nonprofit"
            )

        # Mana post-effects on loan
        mana_notes_loan = []
        _mana_fx_loan = getattr(self.bot, "mana_effects_service", None)
        _loan_effects = None
        if _mana_fx_loan:
            try:
                from domain.models.mana_effects import ManaEffects as _MELoan
                _loan_effects_raw = await asyncio.to_thread(_mana_fx_loan.get_effects, user_id, guild_id)
                if isinstance(_loan_effects_raw, _MELoan):
                    _loan_effects = _loan_effects_raw
            except Exception:
                pass
            if _loan_effects:
                # Swamp self-tax
                if _loan_effects.swamp_self_tax > 0:
                    await asyncio.to_thread(self.player_service.adjust_balance, user_id, guild_id, -_loan_effects.swamp_self_tax)
                    mana_notes_loan.append(f"🌿 Swamp tax: -{_loan_effects.swamp_self_tax}")
                # Swamp siphon
                if _loan_effects.swamp_siphon:
                    siphon = await asyncio.to_thread(_mana_fx_loan.execute_siphon, user_id, guild_id)
                    if siphon:
                        mana_notes_loan.append(f"🌿 Siphon: +{siphon['amount']}")
        _loan_mana_suffix = ""
        if mana_notes_loan:
            _loan_mana_suffix = "\n" + " | ".join(mana_notes_loan)

        if _loan_mana_suffix:
            embed.add_field(name="Mana Effects", value=_loan_mana_suffix.strip(), inline=False)

        await interaction.followup.send(embed=embed)

        # Neon Degen Terminal hook
        neon = self._get_neon_service()
        if neon:
            neon_result = await neon.on_loan(
                user_id, guild_id,
                amount=result.amount,
                total_owed=result.total_owed,
                is_negative=result.was_negative_loan,
            )
            await send_neon_result(interaction, neon_result)

    @app_commands.command(name="nonprofit", description="View the Gambling Addiction Nonprofit fund")
    async def nonprofit(self, interaction: discord.Interaction):
        """View how much has been collected for the nonprofit."""
        if not self.loan_service:
            await interaction.response.send_message(
                "Loan service is not available.", ephemeral=True
            )
            return

        guild_id = interaction.guild.id if interaction.guild else None
        total = await asyncio.to_thread(self.loan_service.get_nonprofit_fund, guild_id)

        # Check for active proposal with reserved funds
        reserved = 0
        if self.disburse_service:
            proposal = await asyncio.to_thread(self.disburse_service.get_proposal, guild_id)
            if proposal:
                reserved = proposal.fund_amount

        embed = discord.Embed(
            title="💝 Jopacoin Nonprofit for Gambling Addiction",
            description=(
                "All loan fees are donated to help those with negative balance.\n\n"
                "*\"We're here to help... by taking a cut of every loan.\"*"
            ),
            color=0xE91E63,  # Pink
        )

        if reserved > 0:
            embed.add_field(
                name="Available Funds",
                value=f"**{total}** {JOPACOIN_EMOTE}",
                inline=True,
            )
            embed.add_field(
                name="Reserved for Proposal",
                value=f"**{reserved}** {JOPACOIN_EMOTE}",
                inline=True,
            )
            embed.add_field(
                name="Total",
                value=f"**{total + reserved}** {JOPACOIN_EMOTE}",
                inline=True,
            )
        else:
            embed.add_field(
                name="Available Funds",
                value=f"**{total}** {JOPACOIN_EMOTE}",
                inline=False,
            )

        # Show status based on fund level (including reserved)
        effective_total = total + reserved
        if effective_total >= DISBURSE_MIN_FUND:
            if reserved > 0:
                status_value = f"Proposal active ({reserved} reserved)"
            else:
                status_value = f"Ready for disbursement! (min: {DISBURSE_MIN_FUND})"
        else:
            status_value = f"Collecting... ({effective_total}/{DISBURSE_MIN_FUND} needed)"

        embed.add_field(
            name="Status",
            value=status_value,
            inline=True,
        )

        # Show last disbursement info if available
        if self.disburse_service:
            last_disburse = await asyncio.to_thread(self.disburse_service.get_last_disbursement, guild_id)
            if last_disburse:
                import datetime

                dt = datetime.datetime.fromtimestamp(
                    last_disburse["disbursed_at"], tz=datetime.timezone.utc
                )
                time_str = f"<t:{last_disburse['disbursed_at']}:R>"

                # Format recipients
                recipients = last_disburse["recipients"]
                if recipients:
                    # Show up to 3 recipients
                    recipient_strs = []
                    for discord_id, amount in recipients[:3]:
                        recipient_strs.append(f"<@{discord_id}>: +{amount}")
                    if len(recipients) > 3:
                        recipient_strs.append(f"+{len(recipients) - 3} more")
                    recipients_text = "\n".join(recipient_strs)
                else:
                    recipients_text = "No recipients"

                method_labels = {
                    "even": "Even Split",
                    "proportional": "Proportional",
                    "neediest": "Neediest First",
                }
                method_label = method_labels.get(
                    last_disburse["method"], last_disburse["method"]
                )

                embed.add_field(
                    name="Last Disbursement",
                    value=(
                        f"**{last_disburse['total_amount']}** {JOPACOIN_EMOTE} "
                        f"via {method_label}\n{time_str}\n{recipients_text}"
                    ),
                    inline=False,
                )

        embed.set_footer(text="Use /disburse propose to start a distribution vote!")

        await interaction.response.send_message(embed=embed)

    @app_commands.command(
        name="disburse", description="Propose or manage nonprofit fund distribution"
    )
    @app_commands.describe(action="Action to perform")
    @app_commands.choices(
        action=[
            app_commands.Choice(name="propose", value="propose"),
            app_commands.Choice(name="status", value="status"),
            app_commands.Choice(name="reset", value="reset"),
            app_commands.Choice(name="votes", value="votes"),
            app_commands.Choice(name="execute", value="execute"),
        ]
    )
    async def disburse(
        self,
        interaction: discord.Interaction,
        action: app_commands.Choice[str] | None = None,
    ):
        """Propose, view, or reset nonprofit fund distribution voting."""
        if not self.disburse_service:
            await interaction.response.send_message(
                "Disbursement service is not available.", ephemeral=True
            )
            return

        guild_id = interaction.guild.id if interaction.guild else None
        action_value = action.value if action else "status"

        if action_value == "propose":
            await self._disburse_propose(interaction, guild_id)
        elif action_value == "status":
            await self._disburse_status(interaction, guild_id)
        elif action_value == "reset":
            await self._disburse_reset(interaction, guild_id)
        elif action_value == "votes":
            await self._disburse_votes(interaction, guild_id)
        elif action_value == "execute":
            await self._disburse_execute(interaction, guild_id)

    async def _disburse_propose(
        self, interaction: discord.Interaction, guild_id: int | None
    ):
        """Create a new disbursement proposal."""
        can, reason = await asyncio.to_thread(self.disburse_service.can_propose, guild_id)
        if not can:
            if reason == "active_proposal_exists":
                await interaction.response.send_message(
                    "A disbursement vote is already active. Use `/disburse status` to see it.",
                    ephemeral=True,
                )
            elif reason.startswith("insufficient_fund:"):
                parts = reason.split(":")
                current = int(parts[1])
                needed = int(parts[2])
                await interaction.response.send_message(
                    f"Insufficient funds. Current: **{current}** {JOPACOIN_EMOTE}, "
                    f"minimum required: **{needed}** {JOPACOIN_EMOTE}",
                    ephemeral=True,
                )
            elif reason == "no_debtors":
                await interaction.response.send_message(
                    "No players with negative balance to receive funds.", ephemeral=True
                )
            else:
                await interaction.response.send_message(
                    f"Cannot create proposal: {reason}", ephemeral=True
                )
            return

        try:
            proposal = await asyncio.to_thread(self.disburse_service.create_proposal, guild_id)
        except ValueError as e:
            await interaction.response.send_message(str(e), ephemeral=True)
            return

        # Create embed and view
        embed = self._create_disburse_embed(proposal)
        view = DisburseVoteView(self.disburse_service, self)

        await interaction.response.send_message(embed=embed, view=view)

        # Store message ID for updates
        msg = await interaction.original_response()
        await asyncio.to_thread(
            self.disburse_service.set_proposal_message,
            guild_id, msg.id, interaction.channel_id,
        )

    async def _disburse_status(
        self, interaction: discord.Interaction, guild_id: int | None
    ):
        """Show current proposal status, replacing the old message to keep it visible."""
        proposal = await asyncio.to_thread(self.disburse_service.get_proposal, guild_id)
        if not proposal:
            await interaction.response.send_message(
                "No active disbursement proposal. Use `/disburse propose` to create one.",
                ephemeral=True,
            )
            return

        # Delete the old message if it exists (to avoid it getting lost in chat)
        if proposal.message_id and proposal.channel_id:
            try:
                old_channel = self.bot.get_channel(proposal.channel_id)
                if old_channel:
                    old_message = await old_channel.fetch_message(proposal.message_id)
                    if old_message:
                        await old_message.delete()
            except discord.errors.NotFound:
                pass  # Message already deleted
            except Exception as e:
                logger.warning(f"Failed to delete old disburse message: {e}")

        # Send new message with embed and voting buttons
        embed = self._create_disburse_embed(proposal)
        view = DisburseVoteView(self.disburse_service, self)
        await interaction.response.send_message(embed=embed, view=view)

        # Update stored message reference to point to the new message
        msg = await interaction.original_response()
        await asyncio.to_thread(
            self.disburse_service.set_proposal_message,
            guild_id, msg.id, interaction.channel_id,
        )

    async def _disburse_reset(
        self, interaction: discord.Interaction, guild_id: int | None
    ):
        """Reset (cancel) the active proposal. Admin only."""
        # Check admin
        if interaction.user.id not in self.bot.ADMIN_USER_IDS:
            await interaction.response.send_message(
                "Only admins can reset disbursement proposals.", ephemeral=True
            )
            return

        success = await asyncio.to_thread(self.disburse_service.reset_proposal, guild_id)
        if success:
            await interaction.response.send_message(
                "Disbursement proposal has been reset.", ephemeral=False
            )
        else:
            await interaction.response.send_message(
                "No active proposal to reset.", ephemeral=True
            )

    async def _disburse_votes(
        self, interaction: discord.Interaction, guild_id: int | None
    ):
        """Show detailed voting information with voter identities. Admin only."""
        # Check admin
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "Only admins can view detailed voting information.", ephemeral=True
            )
            return

        proposal = await asyncio.to_thread(self.disburse_service.get_proposal, guild_id)
        if not proposal:
            await interaction.response.send_message(
                "No active disbursement proposal. Use `/disburse status` to check.",
                ephemeral=True,
            )
            return

        # Create admin-only embed with voter details
        embed = await self._create_disburse_votes_embed(proposal)
        await interaction.response.send_message(embed=embed, ephemeral=True)

    async def _disburse_execute(
        self, interaction: discord.Interaction, guild_id: int | None
    ):
        """Force-execute the active proposal using the current leading method. Admin only."""
        if interaction.user.id not in self.bot.ADMIN_USER_IDS:
            await interaction.response.send_message(
                "Only admins can force-execute disbursement proposals.", ephemeral=True
            )
            return

        # Show current state before executing
        proposal = await asyncio.to_thread(self.disburse_service.get_proposal, guild_id)
        if not proposal:
            await interaction.response.send_message(
                "No active disbursement proposal.", ephemeral=True
            )
            return

        if not await safe_defer(interaction):
            return

        try:
            disbursement = await asyncio.to_thread(
                self.disburse_service.force_execute, guild_id
            )
        except ValueError as e:
            await interaction.followup.send(
                content=f"Cannot execute: {e}", ephemeral=True
            )
            return

        # Handle cancel
        if disbursement.get("cancelled"):
            embed = discord.Embed(
                title="❌ Proposal Cancelled (Admin)",
                description=disbursement.get("message", "Proposal cancelled."),
                color=0xFF6B6B,
            )
            await interaction.followup.send(embed=embed)
        elif disbursement["total_disbursed"] == 0:
            embed = discord.Embed(
                title="💝 Disbursement Complete (Admin)",
                description=disbursement.get("message", "No funds were distributed."),
                color=0x00FF00,
            )
            await interaction.followup.send(embed=embed)
        else:
            recipients = disbursement["distributions"]
            recipient_lines = []
            for discord_id, amount in recipients[:10]:
                recipient_lines.append(f"<@{discord_id}>: +{amount}")
            if len(recipients) > 10:
                recipient_lines.append(f"...and {len(recipients) - 10} more")

            result_msg = (
                f"**{disbursement['total_disbursed']}** {JOPACOIN_EMOTE} "
                f"distributed via **{disbursement['method_label']}** to "
                f"{disbursement['recipient_count']} player(s):\n"
                + "\n".join(recipient_lines)
            )

            embed = discord.Embed(
                title="💝 Disbursement Complete (Admin)",
                description=result_msg,
                color=0x00FF00,
            )
            embed.set_footer(text=f"Force-executed by {interaction.user.display_name}")
            await interaction.followup.send(embed=embed)

        # Disable buttons on the original voting message
        try:
            if proposal.message_id and proposal.channel_id:
                channel = self.bot.get_channel(proposal.channel_id)
                if channel:
                    msg = await channel.fetch_message(proposal.message_id)
                    disabled_view = discord.ui.View(timeout=None)
                    for method in ["even", "proportional", "neediest", "stimulus", "lottery", "social_security", "cancel"]:
                        label = self.disburse_service.METHOD_LABELS[method]
                        emoji = {"even": "📊", "proportional": "📈", "neediest": "🎯",
                                 "stimulus": "💸", "lottery": "🎲",
                                 "social_security": "👴", "cancel": "❌"}.get(method)
                        style = discord.ButtonStyle.danger if method == "cancel" else discord.ButtonStyle.secondary
                        btn = discord.ui.Button(
                            label=label, emoji=emoji, style=style,
                            disabled=True, custom_id=f"disburse:{method}",
                        )
                        disabled_view.add_item(btn)
                    await msg.edit(view=disabled_view)
        except Exception as e:
            logger.warning(f"Failed to disable vote buttons after force-execute: {e}")

    def _create_disburse_embed(self, proposal) -> discord.Embed:
        """Create embed for disbursement proposal."""
        votes = proposal.votes
        total_votes = proposal.total_votes
        quorum = proposal.quorum_required
        progress = proposal.quorum_progress

        embed = discord.Embed(
            title="💝 Nonprofit Fund Disbursement Vote",
            description=(
                f"Vote on how to distribute **{proposal.fund_amount}** {JOPACOIN_EMOTE}.\n\n"
                "Click a button below to vote!"
            ),
            color=0xE91E63,  # Pink
        )

        # Voting options with counts
        embed.add_field(
            name="📊 Even Split",
            value=f"Split equally to debtors\n**{votes['even']}** votes",
            inline=True,
        )
        embed.add_field(
            name="📈 Proportional",
            value=f"More debt = more funds\n**{votes['proportional']}** votes",
            inline=True,
        )
        embed.add_field(
            name="🎯 Neediest First",
            value=f"All to most indebted\n**{votes['neediest']}** votes",
            inline=True,
        )
        embed.add_field(
            name="💸 Stimulus",
            value=f"Even split to active non-top-3\n**{votes['stimulus']}** votes",
            inline=True,
        )
        embed.add_field(
            name="🎲 Lottery",
            value=f"Random active player wins all\n**{votes.get('lottery', 0)}** votes",
            inline=True,
        )
        embed.add_field(
            name="👴 Social Security",
            value=f"By games played (excl. top 3)\n**{votes.get('social_security', 0)}** votes",
            inline=True,
        )
        embed.add_field(
            name="❌ Cancel",
            value=f"Keep funds in nonprofit\n**{votes.get('cancel', 0)}** votes",
            inline=True,
        )

        # Progress bar
        bar_length = 20
        filled = int(progress * bar_length)
        bar = "█" * filled + "░" * (bar_length - filled)
        embed.add_field(
            name="Quorum Progress",
            value=f"`{bar}` {total_votes}/{quorum} ({int(progress * 100)}%)",
            inline=False,
        )

        if proposal.quorum_reached:
            embed.add_field(
                name="✅ Quorum Reached!",
                value="The next vote will trigger automatic disbursement.",
                inline=False,
            )

        embed.set_footer(text="Ties are broken in favor of Even Split")

        return embed

    async def _create_disburse_votes_embed(self, proposal) -> discord.Embed:
        """Create admin-only embed showing detailed voter information."""
        votes = proposal.votes
        total_votes = proposal.total_votes
        quorum = proposal.quorum_required
        progress = proposal.quorum_progress

        embed = discord.Embed(
            title="🔍 Disbursement Vote Details (Admin Only)",
            description=f"Fund Amount: **{proposal.fund_amount}** {JOPACOIN_EMOTE}",
            color=0x9C27B0,  # Purple (admin color)
        )

        # Proposal info
        embed.add_field(
            name="📋 Proposal Status",
            value=(
                f"**Quorum:** {total_votes}/{quorum} ({int(progress * 100)}%)\n"
                f"**Status:** {'✅ Ready' if proposal.quorum_reached else '⏳ Voting'}"
            ),
            inline=False,
        )

        # Vote breakdown
        vote_lines = []
        for method in ["even", "proportional", "neediest", "stimulus", "lottery", "social_security", "cancel"]:
            count = votes.get(method, 0)
            pct = (count / total_votes * 100) if total_votes > 0 else 0
            label = self.disburse_service.METHOD_LABELS[method]
            vote_lines.append(f"**{label}:** {count} ({pct:.0f}%)")

        embed.add_field(
            name="📊 Vote Breakdown",
            value="\n".join(vote_lines),
            inline=False,
        )

        # Individual votes
        guild_id = proposal.guild_id if proposal.guild_id != 0 else None
        individual_votes = await asyncio.to_thread(
            self.disburse_service.get_individual_votes, guild_id
        )

        if individual_votes:
            voter_lines = []
            for vote in individual_votes:
                discord_id = vote["discord_id"]
                method = vote["vote_method"]
                method_label = self.disburse_service.METHOD_LABELS.get(method, method)
                voter_lines.append(f"• <@{discord_id}> → {method_label}")

            voters_text = "\n".join(voter_lines)
        else:
            voters_text = "*No votes yet*"

        # Truncate if too long (Discord field limit is 1024 chars)
        if len(voters_text) > 1024:
            voters_text = voters_text[:1021] + "..."

        embed.add_field(
            name="👥 Individual Votes",
            value=voters_text,
            inline=False,
        )

        embed.set_footer(text="This information is only visible to you")

        return embed

    async def update_disburse_message(self, guild_id: int | None):
        """Update the disbursement proposal message with current vote counts."""
        proposal = await asyncio.to_thread(self.disburse_service.get_proposal, guild_id)
        if not proposal or not proposal.message_id or not proposal.channel_id:
            return

        try:
            channel = self.bot.get_channel(proposal.channel_id)
            if not channel:
                return

            message = await channel.fetch_message(proposal.message_id)
            if not message:
                return

            embed = self._create_disburse_embed(proposal)
            await message.edit(embed=embed)
        except discord.errors.NotFound:
            pass
        except Exception as e:
            logger.warning(f"Failed to update disburse message: {e}")

    # ------------------------------------------------------------------
    # /incite — Wheel War rebellion command
    # ------------------------------------------------------------------

    @app_commands.command(
        name="incite",
        description="Rise against the Wheel of Fortune! (Requires recent bankruptcy or penalty games)",
    )
    async def incite(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None

        rebellion_service = self.rebellion_service or getattr(self.bot, "rebellion_service", None)
        if not rebellion_service:
            await interaction.response.send_message(
                "The rebellion system is not available.", ephemeral=True
            )
            return

        # Check eligibility
        eligibility = await asyncio.to_thread(
            rebellion_service.check_incite_eligibility, user_id, guild_id
        )
        if not eligibility["eligible"]:
            await interaction.response.send_message(
                f"**The court rejects your petition.** {eligibility['reason']}", ephemeral=True
            )
            return

        # Defer - this command runs for up to 15+ minutes
        await interaction.response.defer()

        # Create the rebellion
        war_info = await asyncio.to_thread(
            rebellion_service.create_rebellion, user_id, guild_id
        )
        war_id = war_info["war_id"]
        vote_closes_at = war_info["vote_closes_at"]
        bankruptcy_count = war_info["bankruptcy_count"]
        is_veteran = bankruptcy_count >= 2

        inciter_name = interaction.user.display_name
        veteran_note = f" *(Veteran Rebel — {bankruptcy_count} bankruptcies, 1.5 votes!)*" if is_veteran else ""

        # Build initial vote embed
        vote_view = RebellionVoteView(
            war_id=war_id,
            guild_id=guild_id,
            inciter_id=user_id,
            rebellion_service=rebellion_service,
        )
        embed = vote_view.build_embed(
            effective_attack=1.5 if is_veteran else 1.0,
            effective_defend=0.0,
            attack_voter_count=1,
            defend_voter_count=0,
            inciter_name=interaction.user.mention,
        )
        embed.set_footer(text=f"{inciter_name} has risen.{veteran_note}")

        vote_msg = await interaction.followup.send(embed=embed, view=vote_view, wait=True)
        vote_view.message = vote_msg

        # Wait for vote window
        await asyncio.sleep(REBELLION_VOTE_WINDOW_SECONDS)

        # Stop the vote view
        vote_view.stop()

        # Evaluate result
        vote_result = await asyncio.to_thread(rebellion_service.resolve_vote, war_id)

        # ----------------------------------------------------------------
        # FIZZLE PATH
        # ----------------------------------------------------------------
        if vote_result["outcome"] == "fizzled":
            fizzle_info = await asyncio.to_thread(
                rebellion_service.resolve_fizzle, war_id, guild_id
            )

            import json
            war = await asyncio.to_thread(rebellion_service.rebellion_repo.get_war, war_id)
            eff_atk = war["effective_attack_count"] if war else vote_result.get("effective_attack_count", 0)
            eff_def = war["effective_defend_count"] if war else vote_result.get("effective_defend_count", 0)

            fizzle_embed = discord.Embed(
                title="💨 THE REBELLION FIZZLES",
                description=(
                    f"*The Wheel watches. The Wheel laughs.*\n\n"
                    f"**{interaction.user.mention}'s rebellion has failed to reach quorum.**\n"
                    f"{vote_result.get('reason', 'The people have spoken... or rather, they have not.')}\n\n"
                    f"⚔️ **{eff_atk:.1f}** effective attack vs 🛡️ **{eff_def:.1f}** effective defend.\n\n"
                    f"*The Wheel offers you a consolation spin, inciter. Don't spend it all in one place.*"
                ),
                color=discord.Color.from_str("#4a4a4a"),
            )
            try:
                await vote_msg.edit(embed=fizzle_embed, view=None)
            except Exception:
                if interaction.channel:
                    await interaction.channel.send(embed=fizzle_embed)

            # Give inciter a weakened consolation spin (max REBELLION_FIZZLE_SPIN_MAX_WIN JC win)
            await self._do_fizzle_consolation_spin(interaction, user_id, guild_id)
            return

        # ----------------------------------------------------------------
        # WAR DECLARED PATH
        # ----------------------------------------------------------------
        eff_atk = vote_result["effective_attack_count"]
        eff_def = vote_result["effective_defend_count"]
        attack_ids = [v["discord_id"] for v in vote_result["attack_voter_ids"]]
        defend_ids = list(vote_result["defend_voter_ids"])

        victory_threshold = rebellion_service.calculate_threshold(eff_atk, eff_def)

        war_embed = discord.Embed(
            title="⚔️ THE WHEEL TAKES THE FIELD ⚔️",
            description=(
                f"**WAR HAS BEEN DECLARED!**\n\n"
                f"The realm has spoken. **{eff_atk:.1f}** rebels rise against **{eff_def:.1f}** defenders.\n\n"
                f"**The Wheel rolls to battle. If it rolls ≥ {victory_threshold}, the Wheel survives.**\n\n"
                f"*Stakes for the victors:*\n"
                f"⚔️ **Rebel win:** +{15} JC each, inciter penalty halved, WAR SCAR on wheel\n"
                f"🛡️ **Wheel win:** Defenders get stake back + 20 JC, inciter +1 penalty, WAR TROPHY on wheel\n\n"
                f"**Meta-bets open for {REBELLION_META_BET_WINDOW_SECONDS // 60} minutes!**"
            ),
            color=discord.Color.from_str("#8b0000"),
        )

        # Open meta-bet window
        await asyncio.to_thread(
            rebellion_service.rebellion_repo.set_meta_bet_window,
            war_id,
            int(time.time()) + REBELLION_META_BET_WINDOW_SECONDS,
        )

        bet_view = WarBetView(
            war_id=war_id,
            guild_id=guild_id,
            rebellion_service=rebellion_service,
            player_service=self.player_service,
        )

        try:
            war_msg = await vote_msg.edit(embed=war_embed, view=bet_view)
        except Exception:
            war_msg = None
            if interaction.channel:
                war_msg = await interaction.channel.send(embed=war_embed, view=bet_view)

        # Wait for meta-bet window
        await asyncio.sleep(REBELLION_META_BET_WINDOW_SECONDS)
        bet_view.stop()

        # 5-second countdown
        countdown_msg = war_msg
        for i in range(5, 0, -1):
            countdown_embed = discord.Embed(
                title=f"⚔️ BATTLE COMMENCES IN {i}... ⚔️",
                description=(
                    f"The armies are assembled. The Wheel trembles.\n"
                    f"Victory threshold: **{victory_threshold}**"
                ),
                color=discord.Color.from_str("#ff4444"),
            )
            try:
                if countdown_msg:
                    await countdown_msg.edit(embed=countdown_embed, view=None)
                elif interaction.channel:
                    countdown_msg = await interaction.channel.send(embed=countdown_embed)
            except Exception as e:
                logger.debug("Failed to update rebellion countdown message: %s", e)
            await asyncio.sleep(1.0)

        # BATTLE ROLL
        battle_roll = rebellion_service.roll_battle()

        # Resolve all outcomes
        resolution = await asyncio.to_thread(
            rebellion_service.resolve_battle,
            war_id, guild_id, battle_roll, victory_threshold,
        )

        # Settle meta-bets
        outcome = resolution["outcome"]
        winning_side = "rebels" if outcome == "attackers_win" else "wheel"
        meta_bet_result = await asyncio.to_thread(
            rebellion_service.rebellion_repo.settle_meta_bets, war_id, winning_side
        )

        # Build result embed
        if outcome == "attackers_win":
            result_embed = self._build_attacker_win_embed(
                interaction=interaction,
                battle_roll=battle_roll,
                victory_threshold=victory_threshold,
                resolution=resolution,
                meta_bet_result=meta_bet_result,
            )
            # Apply attacker penalty to defenders: +48h cooldown (stored in player last_wheel_spin)
            # Note: we do NOT apply cooldown to attackers — they *won*
        else:  # defenders_win
            result_embed = self._build_defender_win_embed(
                interaction=interaction,
                battle_roll=battle_roll,
                victory_threshold=victory_threshold,
                resolution=resolution,
                meta_bet_result=meta_bet_result,
                inciter_name=inciter_name,
                bankruptcy_count=bankruptcy_count,
            )
            # Attackers get +48h gamba cooldown as punishment
            now_ts = int(time.time())
            for did in attack_ids:
                last_spin = await asyncio.to_thread(self.player_service.get_last_wheel_spin, did, guild_id)
                penalized_spin = max(now_ts - 86400 + REBELLION_GAMBA_COOLDOWN_PENALTY, last_spin or 0)
                await asyncio.to_thread(self.player_service.set_last_wheel_spin, did, guild_id, penalized_spin)

        try:
            if countdown_msg:
                await countdown_msg.edit(embed=result_embed, view=None)
            elif interaction.channel:
                await interaction.channel.send(embed=result_embed)
        except Exception as e:
            logger.debug("Failed to edit countdown with result, falling back to new message: %s", e)
            if interaction.channel:
                try:
                    await interaction.channel.send(embed=result_embed)
                except Exception as e2:
                    logger.warning("Failed to send rebellion result embed: %s", e2)

        # Pin shame embed if defenders won
        if outcome == "defenders_win" and interaction.channel:
            shame_embed = discord.Embed(
                title="📌 HALL OF SHAME",
                description=(
                    f"**{interaction.user.mention}** ({bankruptcy_count} bankruptcies) tried to incite "
                    f"a rebellion against the Wheel... and LOST.\n\n"
                    f"*\"You thought you could stop me? Spin again, coward.\" — The Wheel*"
                ),
                color=discord.Color.from_str("#4a0000"),
            )
            try:
                shame_msg = await interaction.channel.send(embed=shame_embed)
                await shame_msg.pin()
            except Exception as e:
                logger.debug("Failed to send or pin shame embed: %s", e)

    def _build_attacker_win_embed(
        self,
        interaction: discord.Interaction,
        battle_roll: int,
        victory_threshold: int,
        resolution: dict,
        meta_bet_result: dict,
    ) -> discord.Embed:
        war_scar = resolution.get("war_scar_label", "unknown")
        embed = discord.Embed(
            title="🎉 THE REBELS TRIUMPH! THE WHEEL IS HUMILIATED! 🎉",
            description=(
                f"**The Wheel rolled {battle_roll}. Victory threshold was {victory_threshold}.**\n"
                f"*The Wheel crumbles before the righteous fury of the people!*\n\n"
                f"**Rewards:**\n"
                f"⚔️ **Inciter:** +{resolution.get('inciter_reward', 30)} JC — "
                f"Penalty games cut from {resolution.get('inciter_penalty_before', 0)} to {resolution.get('inciter_penalty_after', 0)}\n"
                f"⚔️ **All Attackers:** +{resolution.get('attacker_flat_reward', 15)} JC + equal share of defender stakes\n\n"
                f"**Wheel Effects (next 10 guild spins):**\n"
                f"💀 **WAR SCAR:** The {war_scar} JC wedge becomes 0 JC\n"
                f"🩹 **BANKRUPT weakened** (-25%)\n"
                f"🎁 **Free spin** for all guild members within 24 hours!\n\n"
                f"*Meta-bet pool: {meta_bet_result.get('total_pool', 0)} JC settled.*"
            ),
            color=discord.Color.green(),
        )
        return embed

    def _build_defender_win_embed(
        self,
        interaction: discord.Interaction,
        battle_roll: int,
        victory_threshold: int,
        resolution: dict,
        meta_bet_result: dict,
        inciter_name: str,
        bankruptcy_count: int,
    ) -> discord.Embed:
        embed = discord.Embed(
            title="🎰 THE WHEEL STANDS VICTORIOUS! THE REBELLION IS CRUSHED! 🎰",
            description=(
                f"**The Wheel rolled {battle_roll}. Victory threshold was {victory_threshold}.**\n"
                f"*The Wheel's iron grip is unbroken. The rebels scatter in disgrace.*\n\n"
                f"**Outcomes:**\n"
                f"🛡️ **Defenders:** Stake returned + 20 JC each\n"
                f"🏆 **Champion Defender:** Additional +10 JC\n"
                f"😤 **Inciter ({inciter_name}):** +1 penalty game (now {resolution.get('inciter_penalty_added', 1)} added)\n"
                f"⏰ **All Attackers:** +48h gamba cooldown as punishment\n\n"
                f"**Wheel Effects (next 10 guild spins):**\n"
                f"🏆 **WAR TROPHY** wedge (+80 JC) added\n"
                f"⚔️ **RETRIBUTION** wedge added (steals from attackers)\n"
                f"💪 **BANKRUPT emboldened** (+50%)\n\n"
                f"*Meta-bet pool: {meta_bet_result.get('total_pool', 0)} JC settled.*"
            ),
            color=discord.Color.from_str("#8b0000"),
        )
        return embed

    async def _do_fizzle_consolation_spin(
        self,
        interaction: discord.Interaction,
        user_id: int,
        guild_id: int | None,
    ) -> None:
        """Give the inciter a weakened consolation spin after a fizzle."""
        try:
            # Get normal wedges and cap wins at REBELLION_FIZZLE_SPIN_MAX_WIN
            wedges = get_wheel_wedges(is_bankrupt=False, is_golden=False)
            # Weaken all positive wedges
            weakened = []
            for label, value, color in wedges:
                if isinstance(value, int) and value > 0:
                    weakened.append((label, min(value, REBELLION_FIZZLE_SPIN_MAX_WIN), color))
                else:
                    weakened.append((label, value, color))
            result_idx = random.randint(0, len(weakened) - 1)
            result_wedge = weakened[result_idx]
            result_value = result_wedge[1]

            # Apply the result
            if isinstance(result_value, int) and result_value > 0:
                await asyncio.to_thread(
                    self.player_service.adjust_balance, user_id, guild_id, result_value
                )
                new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
                embed = discord.Embed(
                    title="🎰 The Wheel's Consolation",
                    description=(
                        f"*'Not today, little rebel. But here, have a crumb.'*\n\n"
                        f"{interaction.user.mention} spins the weakened wheel... and gets **{result_value} JC**.\n"
                        f"Balance: {new_balance} JC"
                    ),
                    color=discord.Color.from_str("#4a4a4a"),
                )
            elif isinstance(result_value, int) and result_value < 0:
                await asyncio.to_thread(
                    self.player_service.adjust_balance, user_id, guild_id, result_value
                )
                new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
                embed = discord.Embed(
                    title="🎰 The Wheel's Consolation",
                    description=(
                        f"*'I do not offer gifts, fool.'*\n\n"
                        f"{interaction.user.mention} lands on **{result_wedge[0]}** — loses {abs(result_value)} JC.\n"
                        f"Balance: {new_balance} JC"
                    ),
                    color=discord.Color.red(),
                )
            else:
                embed = discord.Embed(
                    title="🎰 The Wheel's Consolation",
                    description=(
                        f"*'Even in defeat, you land on something useless.'*\n\n"
                        f"{interaction.user.mention} lands on **{result_wedge[0]}**."
                    ),
                    color=discord.Color.from_str("#4a4a4a"),
                )

            if interaction.channel:
                await interaction.channel.send(embed=embed)
        except Exception as e:
            logger.error(f"Fizzle consolation spin error: {e}")


class DisburseVoteView(discord.ui.View):
    """Persistent view for disbursement voting."""

    def __init__(self, disburse_service: DisburseService, cog: "BettingCommands"):
        super().__init__(timeout=None)  # Persistent - no timeout
        self.disburse_service = disburse_service
        self.cog = cog

    async def _handle_vote(
        self, interaction: discord.Interaction, method: str, label: str
    ):
        """Handle a vote button press."""
        guild_id = interaction.guild.id if interaction.guild else None

        # Check if user is registered
        player = await asyncio.to_thread(self.cog.player_service.get_player, interaction.user.id, guild_id)
        if not player:
            await interaction.response.send_message(
                "You must be registered to vote. Use `/player register` first.",
                ephemeral=True,
            )
            return

        # Check for active proposal
        proposal = await asyncio.to_thread(self.disburse_service.get_proposal, guild_id)
        if not proposal:
            await interaction.response.send_message(
                "This vote has ended or been reset.", ephemeral=True
            )
            return

        try:
            result = await asyncio.to_thread(
                self.disburse_service.add_vote,
                guild_id, interaction.user.id, method,
            )
        except ValueError as e:
            await interaction.response.send_message(str(e), ephemeral=True)
            return

        # Check if quorum reached and execute
        if result["quorum_reached"]:
            # Execute disbursement
            try:
                disbursement = await asyncio.to_thread(self.disburse_service.execute_disbursement, guild_id)

                # Handle cancel specially
                if disbursement.get("cancelled"):
                    embed = discord.Embed(
                        title="❌ Proposal Cancelled",
                        description=disbursement.get("message", "Proposal cancelled by vote."),
                        color=0xFF6B6B,  # Red
                    )
                    await interaction.response.send_message(embed=embed)
                # Build result message
                elif disbursement["total_disbursed"] == 0:
                    result_msg = disbursement.get(
                        "message", "No funds were distributed."
                    )
                    embed = discord.Embed(
                        title="💝 Disbursement Complete!",
                        description=result_msg,
                        color=0x00FF00,  # Green
                    )
                    await interaction.response.send_message(embed=embed)
                else:
                    recipients = disbursement["distributions"]
                    recipient_lines = []
                    for discord_id, amount in recipients[:10]:
                        recipient_lines.append(f"<@{discord_id}>: +{amount}")
                    if len(recipients) > 10:
                        recipient_lines.append(f"...and {len(recipients) - 10} more")

                    result_msg = (
                        f"**{disbursement['total_disbursed']}** {JOPACOIN_EMOTE} "
                        f"distributed via **{disbursement['method_label']}** to "
                        f"{disbursement['recipient_count']} player(s):\n"
                        + "\n".join(recipient_lines)
                    )

                    # Send result as new message
                    embed = discord.Embed(
                        title="💝 Disbursement Complete!",
                        description=result_msg,
                        color=0x00FF00,  # Green
                    )
                    await interaction.response.send_message(embed=embed)

                # Disable buttons on the original message
                try:
                    if proposal.message_id and proposal.channel_id:
                        channel = self.cog.bot.get_channel(proposal.channel_id)
                        if channel:
                            msg = await channel.fetch_message(proposal.message_id)
                            # Create disabled view
                            disabled_view = discord.ui.View(timeout=None)
                            for item in self.children:
                                if isinstance(item, discord.ui.Button):
                                    new_btn = discord.ui.Button(
                                        label=item.label,
                                        emoji=item.emoji,
                                        style=discord.ButtonStyle.secondary,
                                        disabled=True,
                                        custom_id=item.custom_id,
                                    )
                                    disabled_view.add_item(new_btn)
                            await msg.edit(view=disabled_view)
                except Exception as e:
                    logger.warning(f"Failed to disable vote buttons: {e}")

            except ValueError as e:
                await interaction.response.send_message(
                    f"Disbursement failed: {e}", ephemeral=True
                )
        else:
            # Just acknowledge the vote
            await interaction.response.send_message(
                f"Your vote for **{label}** has been recorded! "
                f"({result['total_votes']}/{result['quorum_required']} for quorum)",
                ephemeral=True,
            )

            # Update the embed
            await self.cog.update_disburse_message(guild_id)

    @discord.ui.button(
        label="Even Split",
        emoji="📊",
        style=discord.ButtonStyle.primary,
        custom_id="disburse:even",
    )
    async def vote_even(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "even", "Even Split")

    @discord.ui.button(
        label="Proportional",
        emoji="📈",
        style=discord.ButtonStyle.primary,
        custom_id="disburse:proportional",
    )
    async def vote_proportional(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "proportional", "Proportional")

    @discord.ui.button(
        label="Neediest First",
        emoji="🎯",
        style=discord.ButtonStyle.primary,
        custom_id="disburse:neediest",
    )
    async def vote_neediest(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "neediest", "Neediest First")

    @discord.ui.button(
        label="Stimulus",
        emoji="💸",
        style=discord.ButtonStyle.primary,
        custom_id="disburse:stimulus",
    )
    async def vote_stimulus(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "stimulus", "Stimulus")

    @discord.ui.button(
        label="Lottery",
        emoji="🎲",
        style=discord.ButtonStyle.primary,
        custom_id="disburse:lottery",
    )
    async def vote_lottery(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "lottery", "Lottery")

    @discord.ui.button(
        label="Social Security",
        emoji="👴",
        style=discord.ButtonStyle.primary,
        custom_id="disburse:social_security",
    )
    async def vote_social_security(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "social_security", "Social Security")

    @discord.ui.button(
        label="Richest",
        emoji="💎",
        style=discord.ButtonStyle.primary,
        custom_id="disburse:richest",
    )
    async def vote_richest(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "richest", "Richest")

    @discord.ui.button(
        label="Cancel",
        emoji="❌",
        style=discord.ButtonStyle.danger,
        custom_id="disburse:cancel",
    )
    async def vote_cancel(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "cancel", "Cancel")


async def setup(bot: commands.Bot):
    betting_service = getattr(bot, "betting_service", None)
    if betting_service is None:
        raise RuntimeError("Betting service not registered on bot.")
    match_service = getattr(bot, "match_service", None)
    if match_service is None:
        raise RuntimeError("Match service not registered on bot.")
    player_service = getattr(bot, "player_service", None)
    if player_service is None:
        raise RuntimeError("Player service not registered on bot.")
    bankruptcy_service = getattr(bot, "bankruptcy_service", None)
    gambling_stats_service = getattr(bot, "gambling_stats_service", None)
    loan_service = getattr(bot, "loan_service", None)
    disburse_service = getattr(bot, "disburse_service", None)
    flavor_text_service = getattr(bot, "flavor_text_service", None)
    tip_service = getattr(bot, "tip_service", None)
    rebellion_service = getattr(bot, "rebellion_service", None)
    # optional services: bankruptcy_service, gambling_stats_service, loan_service, disburse_service, flavor_text_service, tip_service, rebellion_service

    cog = BettingCommands(
        bot,
        betting_service,
        match_service,
        player_service,
        bankruptcy_service,
        gambling_stats_service,
        loan_service,
        disburse_service,
        flavor_text_service,
        tip_service,
        rebellion_service=rebellion_service,
    )
    await bot.add_cog(cog)

    # Register persistent view for disbursement voting
    if disburse_service:
        bot.add_view(DisburseVoteView(disburse_service, cog))
