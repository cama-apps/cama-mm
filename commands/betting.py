"""
Betting commands for jopacoin wagers.

The cog stays in this file so Discord's command tree picks up the
``@app_commands.command`` decorators. Larger blocks of view classes,
embed builders, and action helpers live in ``commands/betting_helpers/``.
"""

from __future__ import annotations

import asyncio
import functools
import logging
import random
import time
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands

if TYPE_CHECKING:
    from services.flavor_text_service import FlavorTextService

from commands.betting_helpers.bet_actions import (
    balance_action as _balance_action,
)
from commands.betting_helpers.bet_actions import (
    bet_action as _bet_action,
)
from commands.betting_helpers.bet_actions import (
    bets_action as _bets_action,
)
from commands.betting_helpers.bet_actions import (
    mybets_action as _mybets_action,
)
from commands.betting_helpers.bet_messaging import (
    send_betting_reminder as _send_betting_reminder_helper,
)
from commands.betting_helpers.bet_messaging import (
    update_embed_betting_field as _update_embed_betting_field_helper,
)
from commands.betting_helpers.bet_messaging import (
    update_shuffle_message_wagers as _update_shuffle_message_wagers_helper,
)
from commands.betting_helpers.disburse_actions import (
    disburse_execute as _disburse_execute_action,
)
from commands.betting_helpers.disburse_actions import (
    disburse_propose as _disburse_propose_action,
)
from commands.betting_helpers.disburse_actions import (
    disburse_reset as _disburse_reset_action,
)
from commands.betting_helpers.disburse_actions import (
    disburse_status as _disburse_status_action,
)
from commands.betting_helpers.disburse_actions import (
    disburse_votes as _disburse_votes_action,
)
from commands.betting_helpers.disburse_actions import (
    update_disburse_message as _update_disburse_message_helper,
)
from commands.betting_helpers.disburse_views import DisburseVoteView
from commands.betting_helpers.economy_actions import (
    bankruptcy_action as _bankruptcy_action,
)
from commands.betting_helpers.economy_actions import (
    loan_action as _loan_action,
)
from commands.betting_helpers.economy_actions import (
    nonprofit_action as _nonprofit_action,
)
from commands.betting_helpers.economy_actions import (
    paydebt_action as _paydebt_action,
)
from commands.betting_helpers.economy_actions import (
    tip_action as _tip_action,
)
from commands.betting_helpers.messages import (
    WHEEL_EXPLOSION_CHANCE,
    WHEEL_EXPLOSION_REWARD,
)
from commands.betting_helpers.rebellion_actions import (
    incite_action as _incite_action,
)
from commands.betting_helpers.wheel_embeds import (
    build_wheel_explosion_embed,
    build_wheel_result_embed,
)
from commands.betting_helpers.wheel_embeds import (
    wedge_ev as _wedge_ev,
)
from commands.betting_helpers.wheel_views import (
    DiscoverView,
    ScryingView,
    TownTrialView,
    WheelRerollView,
)
from commands.checks import require_gamba_channel, require_guild
from config import (
    LIGHTNING_BOLT_MIN_TAX,
    LIGHTNING_BOLT_PCT_MAX,
    LIGHTNING_BOLT_PCT_MIN,
    REBELLION_RETRIBUTION_STEAL,
    WHEEL_COOLDOWN_SECONDS,
    WHEEL_GOLDEN_TOP_N,
    WHEEL_LOSE_PENALTY_COOLDOWN,
)
from services.bankruptcy_service import BankruptcyService
from services.betting_service import BettingService
from services.disburse_service import DisburseService
from services.gambling_stats_service import GamblingStatsService
from services.loan_service import LoanService
from services.match_service import MatchService
from services.permissions import has_admin_permission
from services.player_service import PlayerService
from services.tip_service import TipService
from utils.formatting import JOPACOIN_EMOTE
from utils.neon_helpers import get_neon_service, send_neon_result
from utils.wheel_drawing import (
    apply_mana_wedge,
    apply_war_effects,
    compute_live_golden_wedges,
    create_explosion_gif,
    create_wheel_gif,
    get_wheel_wedges,
)

logger = logging.getLogger("cama_bot.commands.betting")


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
            pmid = match.pending_match_id
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
        """Refresh the shuffle message's wager field with current totals."""
        await _update_shuffle_message_wagers_helper(self, guild_id, pending_match_id)

    async def _update_embed_betting_field(
        self, channel_id: int, message_id: int, field_name: str, field_value: str
    ) -> None:
        """Helper to update the betting field in an embed message."""
        await _update_embed_betting_field_helper(self, channel_id, message_id, field_name, field_value)

    async def _send_betting_reminder(
        self,
        guild_id: int | None,
        *,
        reminder_type: str,
        lock_until: int | None,
        pending_match_id: int | None = None,
    ) -> None:
        """Send a reminder message replying to the shuffle embed with current bet totals."""
        await _send_betting_reminder_helper(
            self,
            guild_id,
            reminder_type=reminder_type,
            lock_until=lock_until,
            pending_match_id=pending_match_id,
        )

    def _create_wheel_gif_file(
        self, target_idx: int, display_name: str | None = None,
        is_bankrupt: bool = False, is_golden: bool = False,
        wedges: list[tuple[str, int | str, str]] | None = None,
    ) -> discord.File:
        """Create a wheel animation and return as discord.File."""
        buffer = create_wheel_gif(
            target_idx=target_idx, size=500, display_name=display_name,
            is_bankrupt=is_bankrupt, is_golden=is_golden, wedges=wedges,
        )
        return discord.File(buffer, filename="wheel.gif")

    def _create_explosion_gif_file(self, display_name: str | None = None) -> discord.File:
        """Create an explosion animation and return as discord.File."""
        buffer = create_explosion_gif(size=500, display_name=display_name)
        return discord.File(buffer, filename="explosion.gif")

    def _wheel_result_embed(self, *args, **kwargs) -> discord.Embed:
        """Thin wrapper around build_wheel_result_embed."""
        return build_wheel_result_embed(*args, **kwargs)

    def _wheel_explosion_embed(
        self, new_balance: int, garnished: int, next_spin_time: int
    ) -> discord.Embed:
        """Thin wrapper around build_wheel_explosion_embed."""
        return build_wheel_explosion_embed(new_balance, garnished, next_spin_time)

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
        await _bet_action(self, interaction, team, amount, leverage, match)
    @app_commands.command(name="mybets", description="Show your active bets")
    async def mybets(self, interaction: discord.Interaction):
        await _mybets_action(self, interaction)
    @app_commands.command(name="bets", description="Show all bets in the current pool")
    @app_commands.describe(
        match="Match to view bets for (auto-selects if only one match exists)",
    )
    @app_commands.autocomplete(match=match_autocomplete)
    async def bets(self, interaction: discord.Interaction, match: int = None):
        await _bets_action(self, interaction, match)
    @app_commands.command(name="balance", description="Check your jopacoin balance")
    async def balance(self, interaction: discord.Interaction):
        await _balance_action(self, interaction)
    @app_commands.command(name="gamba", description="Spin the Wheel of Fortune! (once per day)")
    @require_guild
    async def gamba(self, interaction: discord.Interaction):
        if not await require_gamba_channel(interaction):
            return

        user_id = interaction.user.id
        guild_id = interaction.guild.id
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
            reminder_svc = getattr(self.bot, "reminder_service", None)
            if reminder_svc:
                reminder_svc.schedule_wheel_reminder(self.bot, user_id, guild_id, next_spin_time)

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

        # Green mana: bankrupt insurance — first BANKRUPT per mana day downgraded to LOSE
        _insurance_activated = False
        if (
            not _guardian_activated
            and effects
            and effects.color == "Green"
            and is_eligible_for_bad_gamba
            and isinstance(result_wedge[1], int)
            and result_wedge[1] < 0
        ):
            mana_service_ins = getattr(self.bot, "mana_service", None)
            mana_repo_ins = getattr(mana_service_ins, "mana_repo", None) if mana_service_ins else None
            if mana_repo_ins is not None:
                try:
                    if await asyncio.to_thread(
                        mana_repo_ins.claim_bankrupt_buff_atomic, user_id, guild_id, "insurance"
                    ):
                        result_wedge = ("LOSE", 0, "#4a4a4a")
                        _insurance_activated = True
                except Exception:
                    logger.debug("Failed to claim Green insurance", exc_info=True)

        # Consume war spin if active
        if _active_war_id and _rebellion_svc_gamba:
            await asyncio.to_thread(
                _rebellion_svc_gamba.consume_war_spin, _active_war_id, guild_id, user_id
            )

        # Defer first - GIF generation can take a few seconds
        if not _scrying_deferred:
            await interaction.response.defer()

        # Blue mana: bankrupt-wheel peek — surface 3 random wedges before the spin
        if effects and effects.color == "Blue" and is_eligible_for_bad_gamba:
            sample = random.sample(wedges, min(3, len(wedges)))

            def _peek_label(w):
                label, val, _ = w
                if isinstance(val, int) and val > 0:
                    return f"{val} JC"
                return str(label)

            preview = ", ".join(_peek_label(w) for w in sample)
            peek_embed = discord.Embed(
                title="Wheel preview",
                description=f"Blue mana reveals: {preview}",
                color=discord.Color.blue(),
            )
            try:
                await interaction.followup.send(embed=peek_embed)
            except Exception:
                logger.debug("Failed to send Blue mana peek", exc_info=True)

        # Generate the complete animation GIF (plays once, ~20 seconds)
        user_display = interaction.user.name
        gif_file = await asyncio.to_thread(
            self._create_wheel_gif_file,
            result_idx,
            user_display,
            is_eligible_for_bad_gamba,
            is_golden,
            wedges=wedges,
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

        # Red mana: bankrupt re-roll on LOSE/EXTEND_1/EXTEND_2 (one per mana day).
        # Runs BEFORE applying the result so we never need to reverse side effects.
        _reroll_used = False
        _candidate_value = result_wedge[1]
        _reroll_eligible = (
            effects
            and effects.color == "Red"
            and is_eligible_for_bad_gamba
            and (_candidate_value == 0 or _candidate_value in ("EXTEND_1", "EXTEND_2"))
        )
        if _reroll_eligible:
            mana_service_rr = getattr(self.bot, "mana_service", None)
            mana_repo_rr = getattr(mana_service_rr, "mana_repo", None) if mana_service_rr else None
            if mana_repo_rr is not None:
                try:
                    already_used = await asyncio.to_thread(
                        mana_repo_rr.is_bankrupt_buff_used, user_id, guild_id, "reroll"
                    )
                except Exception:
                    already_used = True
                    logger.debug("Failed to read reroll flag", exc_info=True)
                if not already_used:
                    view = WheelRerollView(user_id=user_id, timeout=30.0)
                    prompt_embed = discord.Embed(
                        title="Re-roll available",
                        description=(
                            f"{interaction.user.mention} — Red mana lets you re-roll once. "
                            f"30 seconds to decide."
                        ),
                        color=discord.Color.red(),
                    )
                    prompt_msg = None
                    try:
                        prompt_msg = await interaction.followup.send(embed=prompt_embed, view=view)
                        view.message = prompt_msg
                        await view.wait()
                    except Exception:
                        logger.debug("Failed to present re-roll prompt", exc_info=True)
                    if view.clicked:
                        try:
                            claimed = await asyncio.to_thread(
                                mana_repo_rr.claim_bankrupt_buff_atomic,
                                user_id, guild_id, "reroll",
                            )
                        except Exception:
                            claimed = False
                            logger.debug("Failed to claim re-roll", exc_info=True)
                        if claimed:
                            result_idx = random.randint(0, len(wedges) - 1)
                            result_wedge = wedges[result_idx]
                            new_gif = await asyncio.to_thread(
                                self._create_wheel_gif_file,
                                result_idx,
                                user_display,
                                is_eligible_for_bad_gamba,
                                is_golden,
                                wedges=wedges,
                            )
                            try:
                                await message.edit(attachments=[new_gif])
                            except Exception:
                                logger.debug("Failed to edit message with re-roll GIF", exc_info=True)
                            await asyncio.sleep(15.0)
                            _reroll_used = True
                    if prompt_msg is not None:
                        try:
                            await prompt_msg.delete()
                        except Exception:
                            pass

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
        recession_total: int = 0
        recession_count: int = 0
        recession_self_loss: int = 0

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
            # All players with positive balance lose min(balance, 20) JC; amount vanishes
            all_players_em = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=9999)
            )
            for p in all_players_em:
                if p.jopacoin_balance > 0:
                    loss = min(p.jopacoin_balance, 20)
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
            if effects and mana_effects_service:
                overgrowth_amount = await asyncio.to_thread(
                    mana_effects_service.apply_green_cap, effects, overgrowth_amount
                )
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

        elif result_value == "DECAY":
            # Black: Top 3 wealthiest lose 60 JC each, #4 loses 80, spinner gains total
            top_4 = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=4)
            )
            decay_total = 0
            for i, p in enumerate(top_4):
                if p.discord_id == user_id:
                    continue
                loss = 80 if i == 3 else 60
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
            # Mario Kart Red Shell: Steal 2-7% of balance from player ranked above
            player_above = await asyncio.to_thread(
                self.player_service.get_player_above, user_id, guild_id
            )

            if player_above:
                pct_amount = max(1, int(player_above.jopacoin_balance * random.uniform(0.02, 0.07)))
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
            # Mario Kart Blue Shell: Steal 2-7% of balance from richest player
            leaderboard = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=1)
            )

            if leaderboard and leaderboard[0].discord_id == user_id:
                # Self-hit! User is the richest - LOSE coins (can go below MAX_DEBT - intentional)
                shell_self_hit = True
                pct_amount = max(1, int(new_balance * random.uniform(0.02, 0.07)))
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
                pct_amount = max(1, int(richest.jopacoin_balance * random.uniform(0.02, 0.07)))
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
            # Steal 5-12% (min 1 JC) from each of the bottom 30 positive-balance players
            bottom_players = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard_bottom, guild_id, limit=30, min_balance=1)
            )
            # Exclude the spinner themselves
            victims = [p for p in bottom_players if p.discord_id != user_id]
            heist_total = 0
            heist_count = 0
            for victim in victims:
                steal_amt = max(1, int(victim.jopacoin_balance * random.uniform(0.05, 0.12)))
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
            # Tax the other top-3 players 8-15% each; coins go to spinner
            top_3 = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=WHEEL_GOLDEN_TOP_N)
            )
            crash_victims = [p for p in top_3 if p.discord_id != user_id and p.jopacoin_balance > 0]
            market_crash_total = 0
            market_crash_count = 0
            for victim in crash_victims:
                tax_amt = max(1, int(victim.jopacoin_balance * random.uniform(0.08, 0.15)))
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
            # Tax all positive-balance players (spinner exempt) 2-5%, min 1 JC; coins go to spinner
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
            # Steal 8-15% from rank #4 (just outside top 3)
            leaderboard_4 = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=WHEEL_GOLDEN_TOP_N + 1)
            )
            rank4 = leaderboard_4[WHEEL_GOLDEN_TOP_N] if len(leaderboard_4) > WHEEL_GOLDEN_TOP_N else None
            takeover_missed = False
            takeover_amount = 0
            takeover_victim_name = "rank #4"
            if rank4 and rank4.jopacoin_balance > 0:
                takeover_amount = max(1, int(rank4.jopacoin_balance * random.uniform(0.08, 0.15)))
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

        elif result_value == "RECESSION":
            # Server-wide deflation: every positive-balance player loses a % of
            # their balance, scaled by wealth tier. Funds vanish into nonprofit.
            # Spinner is in top-N so they take the top-tier loss themselves.
            from config import (
                WHEEL_GOLDEN_RECESSION_MID_PCT,
                WHEEL_GOLDEN_RECESSION_MID_RANK_END,
                WHEEL_GOLDEN_RECESSION_REST_PCT,
                WHEEL_GOLDEN_RECESSION_TOP_PCT,
            )
            all_players_rec = await asyncio.to_thread(
                functools.partial(self.player_service.get_leaderboard, guild_id, limit=9999)
            )
            recession_total = 0
            recession_count = 0
            spinner_balance_before = new_balance
            top_n = WHEEL_GOLDEN_TOP_N
            mid_end = WHEEL_GOLDEN_RECESSION_MID_RANK_END
            for rank_idx, p in enumerate(all_players_rec):
                if p.jopacoin_balance <= 0:
                    continue
                if rank_idx < top_n:
                    pct = WHEEL_GOLDEN_RECESSION_TOP_PCT
                    min_loss = 50
                elif rank_idx < mid_end:
                    pct = WHEEL_GOLDEN_RECESSION_MID_PCT
                    min_loss = 10
                else:
                    pct = WHEEL_GOLDEN_RECESSION_REST_PCT
                    min_loss = 1
                loss = min(p.jopacoin_balance, max(min_loss, int(p.jopacoin_balance * pct)))
                if loss <= 0:
                    continue
                await asyncio.to_thread(
                    self.player_service.adjust_balance, p.discord_id, guild_id, -loss
                )
                recession_total += loss
                recession_count += 1
            if self.loan_service and recession_total > 0:
                try:
                    await asyncio.to_thread(
                        self.loan_service.add_to_nonprofit_fund, guild_id, recession_total
                    )
                except Exception:
                    logger.warning("Failed to add recession losses to nonprofit fund")
            new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
            # Spinner's actual loss: derived from balance delta so we never
            # report a misleading 0 if the leaderboard scan missed them.
            recession_self_loss = max(0, spinner_balance_before - new_balance)

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
                result_wedge = (str(result_value), result_value, result_wedge[2])

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
        reminder_svc = getattr(self.bot, "reminder_service", None)
        if reminder_svc:
            reminder_svc.schedule_wheel_reminder(self.bot, user_id, guild_id, next_spin_time)

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
        elif result_value == "LIGHTNING_BOLT" or result_value in ("EXTEND_1", "EXTEND_2") or result_value in ("JAILBREAK", "CHAIN_REACTION", "TOWN_TRIAL", "DISCOVER",
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
        elif result_value == "RECESSION":
            log_result = -recession_self_loss
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
            recession_total=recession_total,
            recession_count=recession_count,
            recession_self_loss=recession_self_loss,
        )

        # Add Guardian Aura notification if it triggered
        if _guardian_activated:
            result_embed.add_field(
                name="🌾 Guardian Aura",
                value="Plains mana converted BANKRUPT to LOSE!",
                inline=False,
            )

        # Add Green insurance notification if it triggered
        if _insurance_activated:
            result_embed.add_field(
                name="Insurance",
                value="Insurance applied: BANKRUPT → LOSE",
                inline=False,
            )

        # Add Red re-roll notification if it triggered
        if _reroll_used:
            result_embed.add_field(
                name="Re-roll",
                value="Red mana re-roll used",
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

        # Witch's Curse: wheel result for the spinner.
        # Positive integer payout → win, negative integer → loss, special wedges → neutral.
        curse_service = getattr(self.bot, "curse_service", None)
        if curse_service is not None and interaction.channel is not None:
            from services.curse_service import spawn_curse_flame
            if isinstance(result_value, int):
                wheel_outcome = "win" if result_value > 0 else ("loss" if result_value < 0 else "neutral")
            else:
                wheel_outcome = "neutral"
            spawn_curse_flame(
                curse_service,
                interaction.channel,
                target_id=user_id,
                guild_id=guild_id,
                system="wheel",
                outcome=wheel_outcome,
                event_context={"wedge": result_wedge[0], "value": result_value, "new_balance": new_balance},
                target_display_name=getattr(interaction.user, "display_name", None),
            )

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
        await _tip_action(self, interaction, player, amount)
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
        await _paydebt_action(self, interaction, player, amount)
    @app_commands.command(
        name="bankruptcy",
        description="Declare bankruptcy to clear your debt (once per week, with penalties)",
    )
    async def bankruptcy(self, interaction: discord.Interaction):
        await _bankruptcy_action(self, interaction)
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
        await _loan_action(self, interaction, amount)
    @app_commands.command(name="nonprofit", description="View the Gambling Addiction Nonprofit fund")
    async def nonprofit(self, interaction: discord.Interaction):
        await _nonprofit_action(self, interaction)
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
    @require_guild
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

        guild_id = interaction.guild.id
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
        await _disburse_propose_action(self, interaction, guild_id)

    async def _disburse_status(
        self, interaction: discord.Interaction, guild_id: int | None
    ):
        await _disburse_status_action(self, interaction, guild_id)

    async def _disburse_reset(
        self, interaction: discord.Interaction, guild_id: int | None
    ):
        await _disburse_reset_action(self, interaction, guild_id)

    async def _disburse_votes(
        self, interaction: discord.Interaction, guild_id: int | None
    ):
        await _disburse_votes_action(self, interaction, guild_id)

    async def _disburse_execute(
        self, interaction: discord.Interaction, guild_id: int | None
    ):
        await _disburse_execute_action(self, interaction, guild_id)

    async def update_disburse_message(self, guild_id: int | None):
        """Update the disbursement proposal message with current vote counts."""
        await _update_disburse_message_helper(self, guild_id)

    @app_commands.command(
        name="incite",
        description="Rise against the Wheel of Fortune! (Requires recent bankruptcy or penalty games)",
    )
    async def incite(self, interaction: discord.Interaction):
        await _incite_action(self, interaction)
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
