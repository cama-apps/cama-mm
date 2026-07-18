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
import uuid
from types import SimpleNamespace
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
    apply_gamba_event_multiplier,
    get_gamba_event_multipliers,
)
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
from commands.betting_helpers.wheel_embeds import (
    build_wheel_explosion_embed,
    build_wheel_result_embed,
)
from commands.betting_helpers.wheel_embeds import (
    wedge_ev as _wedge_ev,
)
from commands.betting_helpers.wheel_outcomes import (
    ALL_GUILD_LEADERBOARD_ENTRIES_LIMIT,
    WheelOutcomeContext,
    WheelOutcomeProcessor,
    WheelOutcomeState,
    eruption_reward,
    filter_visible_leaderboard,
    has_guild_member_snapshot,
)
from commands.betting_helpers.wheel_views import (
    DiscoverView,
    ScryingView,
    TownTrialView,
    WheelRerollView,
)
from commands.checks import require_gamba_channel, require_guild
from config import (
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
from utils.economy_scaling import scale_minigame_jc_delta
from utils.formatting import JOPACOIN_EMOTE
from utils.neon_helpers import get_neon_service, send_neon_result
from utils.wheel_drawing import (
    apply_mana_wedge,
    compute_live_golden_wedges,
    create_explosion_gif,
    create_wheel_gif,
    get_wheel_wedges,
)

logger = logging.getLogger("cama_bot.commands.betting")


def _eruption_reward(last_spin: dict | None) -> int:
    """Backward-compatible import for scaling tests and external callers."""
    return eruption_reward(last_spin)


def _canonical_wheel_outcome_code(
    landed_wedge: tuple[str, int | str, str],
    resolved_value: int | str,
    *,
    is_bankrupt: bool,
    is_golden: bool,
) -> str:
    """Return a stable outcome identifier independent of rendered labels.

    Dynamic BANKRUPT and OVEREXTENDED wedges replace their display labels with
    the computed numeric loss. CROWN is likewise rendered as its scaled numeric
    payout. Infer those three identities from wheel context/color so future
    history preserves the actual wedge rather than collapsing them into an
    ordinary numeric result.
    """
    label, landed_value, color = landed_wedge
    normalized_label = str(label).strip().upper().replace(" ", "_")

    if isinstance(landed_value, str):
        return str(resolved_value) if isinstance(resolved_value, str) else landed_value
    if landed_value < 0:
        return "OVEREXTENDED" if is_golden else "BANKRUPT"
    if landed_value == 0:
        return "LOSE"
    if normalized_label == "CROWN" or (is_golden and color.lower() == "#fffacd"):
        return "CROWN"
    return f"NUMERIC_{resolved_value}"


def _wheel_outcome_metadata(
    state: WheelOutcomeState,
    landed_wedge: tuple[str, int | str, str],
    outcome_code: str,
    logged_result: int,
) -> dict:
    """Build JSON-safe, non-identifying details for an exact wheel record."""
    metadata: dict[str, int | str | bool] = {
        "wedge_label": str(landed_wedge[0]),
        "wedge_value": landed_wedge[1],
        "resolved_value": state.result_value,
        "logged_result": logged_result,
    }

    if state.garnished_amount:
        metadata["garnished_amount"] = state.garnished_amount
    if state.shield_absorbed_total:
        metadata["shield_absorbed_total"] = state.shield_absorbed_total
    if state.shielded_count:
        metadata["shielded_count"] = state.shielded_count
    if state.pardon_consumed:
        metadata["pardon_consumed"] = True

    if outcome_code in {"RED_SHELL", "BLUE_SHELL"}:
        metadata.update(
            shell_amount=state.shell_amount,
            shell_missed=state.shell_missed,
            shell_self_hit=state.shell_self_hit,
        )
    elif outcome_code == "LIGHTNING_BOLT":
        metadata.update(
            lightning_total=state.lightning_total,
            lightning_count=state.lightning_count,
        )
    elif outcome_code == "EMERGENCY":
        metadata.update(
            emergency_total=state.emergency_total,
            emergency_count=state.emergency_count,
        )
    elif outcome_code == "COMMUNE":
        metadata.update(
            commune_total=state.commune_total,
            commune_count=state.commune_count,
        )
    elif outcome_code == "HEIST":
        metadata.update(heist_total=state.heist_total, heist_count=state.heist_count)
    elif outcome_code == "MARKET_CRASH":
        metadata.update(
            market_crash_total=state.market_crash_total,
            market_crash_count=state.market_crash_count,
        )
    elif outcome_code == "TRICKLE_DOWN":
        metadata.update(
            trickle_total=state.trickle_total,
            trickle_count=state.trickle_count,
        )
    elif outcome_code == "DIVIDEND":
        metadata["dividend_amount"] = state.dividend_amount
    elif outcome_code == "COMPOUND_INTEREST":
        metadata["compound_amount"] = state.compound_amount
    elif outcome_code == "HOSTILE_TAKEOVER":
        metadata.update(
            takeover_amount=state.takeover_amount,
            takeover_missed=state.takeover_missed,
        )
    elif outcome_code == "RECESSION":
        metadata.update(
            recession_total=state.recession_total,
            recession_count=state.recession_count,
            recession_self_loss=state.recession_self_loss,
        )
    elif outcome_code == "BANANA_PEEL":
        metadata.update(
            banana_victim_loss=state.banana_victim_loss,
            banana_missed=state.banana_missed,
        )
    elif outcome_code == "GREEN_SHELL":
        metadata.update(
            green_shell_amount=state.green_shell_amount,
            green_shell_missed=state.green_shell_missed,
        )
    elif outcome_code == "BOMB_OMB":
        metadata.update(
            bomb_omb_burn_total=state.bomb_omb_burn_total,
            bomb_omb_victim_count=len(state.bomb_omb_victims),
            bomb_omb_missed=state.bomb_omb_missed,
        )
    elif outcome_code in {"EXTEND_1", "EXTEND_2"}:
        metadata.update(
            extend_games_added=state.extend_games_added,
            extend_new_total=state.extend_new_total,
        )
    elif outcome_code == "JAILBREAK":
        metadata["jailbreak_new_total"] = state.jailbreak_new_total
    elif outcome_code == "CHAIN_REACTION" and state.chain_value is not None:
        metadata["chain_value"] = state.chain_value

    return metadata


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

    def _get_neon_service(self):
        """Get the NeonDegenService from the bot, or None if unavailable."""
        return get_neon_service(self.bot)

    async def _get_visible_balance_leaderboard(
        self,
        interaction: discord.Interaction,
        guild_id: int,
        *,
        limit: int,
    ) -> list:
        """Return balance leaders using the same membership rule as `/leaderboard`."""
        query_limit = (
            ALL_GUILD_LEADERBOARD_ENTRIES_LIMIT
            if has_guild_member_snapshot(interaction.guild)
            else limit
        )
        players = await asyncio.to_thread(
            functools.partial(
                self.player_service.get_leaderboard,
                guild_id,
                limit=query_limit,
            )
        )
        return filter_visible_leaderboard(players, interaction.guild, limit=limit)

    def _adjust_gamba_balance(
        self,
        actor_id: int,
        target_id: int,
        guild_id: int,
        delta: int,
        reason: str,
        outcome: str,
        metadata: dict | None = None,
    ) -> int:
        """Adjust a wheel balance with central-ledger context."""
        return self.player_service.adjust_balance(
            target_id,
            guild_id,
            delta,
            source="gamba",
            actor_id=actor_id,
            related_type="wheel_spin",
            related_id=outcome,
            reason=reason,
            metadata=metadata,
        )

    async def _apply_hostile_gamba_loss(
        self,
        *,
        victim_id: int,
        guild_id: int,
        amount: int,
        actor_id: int,
        event_key: str,
        outcome: str,
        destination: str = "burn",
        recipient_id: int | None = None,
        clamp_to_balance: bool = False,
        min_balance: int | None = None,
        victim_balance: int | None = None,
        legacy_aggregate_transfer: bool = False,
        metadata: dict | None = None,
    ):
        """Settle one already-scaled hostile wheel loss through White shields.

        The fallback preserves the pre-protection behavior for lightweight
        command tests and deployments that have not wired ProtectionService.
        """
        protection_service = getattr(self.bot, "protection_service", None)
        try:
            from services.protection_service import ProtectionService
        except ImportError:  # pragma: no cover - only during partial deployments
            ProtectionService = ()  # type: ignore[assignment,misc]

        if isinstance(protection_service, ProtectionService):
            result = await asyncio.to_thread(
                functools.partial(
                    protection_service.apply_hostile_loss,
                    victim_id,
                    guild_id,
                    amount,
                    outcome.lower(),
                    actor_id=actor_id,
                    event_key=event_key,
                    destination=destination,
                    recipient_id=recipient_id,
                    clamp_to_balance=clamp_to_balance,
                    min_balance=min_balance,
                    metadata={"outcome": outcome, **(metadata or {})},
                )
            )
            return SimpleNamespace(
                event_key=result.event_key,
                requested=result.requested,
                attempted=result.attempted,
                absorbed=result.absorbed,
                applied=result.applied,
                victim_balance_before=result.victim_balance_before,
                victim_balance_after=result.victim_balance_after,
                destination_balance_after=result.destination_balance_after,
                duplicate=result.duplicate,
                details=result.details,
                centralized=True,
            )

        attempted = max(0, int(amount))
        before = victim_balance
        if before is None:
            before = await asyncio.to_thread(
                self.player_service.get_balance, victim_id, guild_id
            )
        if min_balance is not None and before < min_balance:
            attempted = 0
        if clamp_to_balance:
            attempted = min(attempted, max(0, before))

        victim_after = before
        destination_after = None
        applied = attempted
        ledger_metadata = {"outcome": outcome, **(metadata or {})}
        if applied > 0 and destination == "player" and not legacy_aggregate_transfer:
            if recipient_id is None:
                raise ValueError("recipient_id is required for player destination")
            transfer = await asyncio.to_thread(
                functools.partial(
                    self.player_service.steal_atomic,
                    thief_discord_id=recipient_id,
                    victim_discord_id=victim_id,
                    guild_id=guild_id,
                    amount=applied,
                    source="gamba",
                    actor_id=actor_id,
                    related_type="wheel_spin",
                    related_id=outcome,
                    reason=f"gamba {outcome.lower().replace('_', ' ')} transfer",
                    metadata=ledger_metadata,
                )
            )
            applied = int(transfer.get("amount", applied))
            victim_after = transfer.get("victim_new_balance", before - applied)
            destination_after = transfer.get("thief_new_balance")
        elif applied > 0:
            victim_after = await asyncio.to_thread(
                self._adjust_gamba_balance,
                actor_id,
                victim_id,
                guild_id,
                -applied,
                f"gamba {outcome.lower().replace('_', ' ')} debit",
                outcome,
                ledger_metadata,
            )
            if (
                destination == "reserve"
                and self.loan_service is not None
                and not legacy_aggregate_transfer
            ):
                await asyncio.to_thread(
                    self.loan_service.add_to_nonprofit_fund,
                    guild_id,
                    applied,
                    source="gamba",
                    actor_id=actor_id,
                    related_type="wheel_spin",
                    related_id=outcome,
                    reason=f"gamba {outcome.lower().replace('_', ' ')} reserve credit",
                    metadata=ledger_metadata,
                )

        return SimpleNamespace(
            event_key=event_key,
            requested=max(0, int(amount)),
            attempted=attempted,
            absorbed=0,
            applied=applied,
            victim_balance_before=before,
            victim_balance_after=victim_after,
            destination_balance_after=destination_after,
            duplicate=False,
            details=(),
            centralized=False,
        )

    async def _credit_gamba_outcome(
        self,
        user_id: int,
        guild_id: int,
        current_balance: int,
        amount: int,
        related_id: str,
        reason: str,
        metadata: dict | None = None,
    ) -> tuple[int, int]:
        """Credit a positive wheel outcome to the spinner, honoring garnishment.

        Mirrors the per-branch credit policy used across the wheel handler:
        when the spinner is in debt (``current_balance < 0``) and a garnishment
        service is available, route the credit through ``add_income`` (which may
        garnish part of it); otherwise apply a direct gamba balance adjustment
        and re-fetch the balance. Returns ``(new_balance, garnished_amount)``.
        """
        garnishment_service = getattr(self.bot, "garnishment_service", None)
        if garnishment_service and current_balance < 0:
            result = await asyncio.to_thread(
                garnishment_service.add_income,
                user_id,
                amount,
                guild_id,
                source="gamba",
                actor_id=user_id,
                related_type="wheel_spin",
                related_id=related_id,
                reason=reason,
                metadata=metadata,
            )
            garnished_amount = result.get("garnished", 0)
            new_balance = result.get("new_balance", current_balance + amount)
            return new_balance, garnished_amount
        await asyncio.to_thread(
            self._adjust_gamba_balance,
            user_id,
            user_id,
            guild_id,
            amount,
            reason,
            related_id,
            metadata,
        )
        new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
        return new_balance, 0

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
        is_final_warning: bool = False,
    ) -> None:
        """Send a reminder message replying to the shuffle embed with current bet totals."""
        await _send_betting_reminder_helper(
            self,
            guild_id,
            reminder_type=reminder_type,
            lock_until=lock_until,
            pending_match_id=pending_match_id,
            is_final_warning=is_final_warning,
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
        self, new_balance: int, garnished: int, next_spin_time: int,
        reward: int = WHEEL_EXPLOSION_REWARD, bankruptcy_penalty: int = 0,
    ) -> discord.Embed:
        """Thin wrapper around build_wheel_explosion_embed."""
        return build_wheel_explosion_embed(
            new_balance, garnished, next_spin_time,
            reward=reward, bankruptcy_penalty=bankruptcy_penalty,
        )

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
        await self._gamba_action(interaction)

    async def _gamba_action(
        self, interaction: discord.Interaction, *, bonus_spin: bool = False,
    ):
        if not bonus_spin and not await require_gamba_channel(interaction):
            return

        user_id = interaction.user.id
        guild_id = interaction.guild.id
        now = time.time()
        raw_event_id = getattr(interaction, "id", None)
        wheel_event_id = (
            str(raw_event_id) if isinstance(raw_event_id, int) else uuid.uuid4().hex
        )
        gamba_win_multiplier, gamba_loss_multiplier = (
            await get_gamba_event_multipliers(self, guild_id)
        )

        # Check if player is registered
        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
        if not player:
            await interaction.response.send_message(
                "You need to `/player register` before you can spin the wheel.",
                ephemeral=True,
            )
            return

        async def regular_next_spin_time() -> int:
            last_regular_spin = await asyncio.to_thread(
                self.player_service.get_last_wheel_spin, user_id, guild_id
            )
            if last_regular_spin is None:
                return int(now)
            return max(
                int(now), int(last_regular_spin) + WHEEL_COOLDOWN_SECONDS,
            )

        # Check cooldown (persisted in database) - admins bypass cooldown
        if not bonus_spin and not has_admin_permission(interaction):
            # Atomic check-and-claim: prevents race condition where concurrent
            # requests could both pass the cooldown check
            claimed = await asyncio.to_thread(
                self.player_service.try_claim_wheel_spin,
                user_id, guild_id, int(now), WHEEL_COOLDOWN_SECONDS,
            )
            if not claimed:
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
        elif not bonus_spin:
            # Admin bypass - still set the timestamp for consistency
            await asyncio.to_thread(
                self.player_service.set_last_wheel_spin, user_id, guild_id, int(now)
            )

        # Check for 1% explosion chance (overrides normal result)
        is_explosion = random.random() < WHEEL_EXPLOSION_CHANCE

        if is_explosion:
            # THE WHEEL EXPLODES!
            if not bonus_spin:
                await interaction.response.defer()

            # Generate explosion animation
            user_display = interaction.user.name
            gif_file = await asyncio.to_thread(self._create_explosion_gif_file, user_display)
            message = await interaction.followup.send(file=gif_file, wait=True)

            # Wait for explosion animation (~8 seconds)
            # 20 spin frames * 50ms + 15 shake frames * ~100ms + 25 explosion * 70ms + 20 aftermath * 100ms
            await asyncio.sleep(8.0)

            # Apply explosion reward (debuffed if under bankruptcy penalty)
            garnished_amount = 0
            new_balance = await asyncio.to_thread(self.player_service.get_balance, user_id, guild_id)
            explosion_is_bankrupt = new_balance < 0
            explosion_is_golden = False
            if not explosion_is_bankrupt:
                top_n = await self._get_visible_balance_leaderboard(
                    interaction,
                    guild_id,
                    limit=WHEEL_GOLDEN_TOP_N,
                )
                explosion_is_golden = user_id in {p.discord_id for p in top_n}

            central_explosion_reward = scale_minigame_jc_delta(
                WHEEL_EXPLOSION_REWARD
            )
            explosion_reward = apply_gamba_event_multiplier(
                central_explosion_reward,
                win_multiplier=gamba_win_multiplier,
                loss_multiplier=gamba_loss_multiplier,
            )
            explosion_penalty = 0
            bankruptcy_service = getattr(self.bot, "bankruptcy_service", None)
            if bankruptcy_service:
                info = await asyncio.to_thread(
                    bankruptcy_service.apply_penalty_to_winnings,
                    user_id, explosion_reward, guild_id,
                )
                explosion_reward, explosion_penalty = info["penalized"], info["penalty_applied"]

            new_balance, garnished_amount = await self._credit_gamba_outcome(
                user_id,
                guild_id,
                new_balance,
                explosion_reward,
                "EXPLOSION",
                "gamba wheel explosion reward",
                {
                    "gross_reward": WHEEL_EXPLOSION_REWARD,
                    "bankruptcy_penalty": explosion_penalty,
                },
            )

            next_spin_time = (
                await regular_next_spin_time()
                if bonus_spin
                else int(now) + WHEEL_COOLDOWN_SECONDS
            )
            reminder_svc = getattr(self.bot, "reminder_service", None)
            if reminder_svc and not bonus_spin:
                reminder_svc.schedule_wheel_reminder(self.bot, user_id, guild_id, next_spin_time)

            # Log the explosion as a special result
            await asyncio.to_thread(
                functools.partial(
                    self.player_service.log_wheel_spin,
                    discord_id=user_id,
                    guild_id=guild_id,
                    # Log the nominal wheel outcome (gross), consistent with the
                    # main-path spin log and bets.payout; the bankruptcy debuff
                    # is a separate balance event surfaced in the embed.
                    result=WHEEL_EXPLOSION_REWARD,
                    spin_time=int(now),
                    is_bankrupt=explosion_is_bankrupt,
                    is_golden=explosion_is_golden,
                    outcome_code="EXPLOSION",
                    is_bonus=bonus_spin,
                    event_id=wheel_event_id,
                    outcome_metadata={
                        "gross_reward": WHEEL_EXPLOSION_REWARD,
                        "central_scaled_reward": central_explosion_reward,
                        "event_adjusted_reward": explosion_reward + explosion_penalty,
                        "event_win_multiplier": gamba_win_multiplier,
                        "credited_reward": explosion_reward,
                        "bankruptcy_penalty": explosion_penalty,
                        "garnished_amount": garnished_amount,
                    },
                )
            )

            await asyncio.sleep(0.5)
            result_embed = self._wheel_explosion_embed(
                new_balance, garnished_amount, next_spin_time,
                reward=explosion_reward, bankruptcy_penalty=explosion_penalty,
            )
            if bonus_spin:
                result_embed.add_field(
                    name="Dig Bonus",
                    value="Free spin — regular `/gamba` cooldown unchanged.",
                    inline=False,
                )
            await message.edit(embed=result_embed)
            return

        # Bankrupt wheel is reserved for players actually in debt (balance < 0).
        # Penalty-game state is still tracked, but a recovered player (balance >= 0)
        # spins the regular or golden wheel — no free re-roll on the bankrupt wheel's
        # higher EV just because their last bankruptcy hasn't expired.
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

        # Golden Wheel eligibility: top-N balance holders get the golden wheel
        # Bankrupt/penalty wheel always takes priority — golden wheel only for non-bad-gamba
        is_golden = False
        visible_players = []
        top_n = []
        if not is_eligible_for_bad_gamba:
            visible_players = await self._get_visible_balance_leaderboard(
                interaction,
                guild_id,
                limit=(
                    ALL_GUILD_LEADERBOARD_ENTRIES_LIMIT
                    if has_guild_member_snapshot(interaction.guild)
                    else WHEEL_GOLDEN_TOP_N
                ),
            )
            top_n = visible_players[:WHEEL_GOLDEN_TOP_N]
            top_n_ids = {p.discord_id for p in top_n}
            is_golden = user_id in top_n_ids

        # Public announcement when a top-N player spins the golden wheel
        if is_golden and interaction.channel:
            top_3_lines = "\n".join(
                f"**#{i+1}** <@{p.discord_id}> — {p.jopacoin_balance} {JOPACOIN_EMOTE}"
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
            if has_guild_member_snapshot(interaction.guild):
                top_n_extended = visible_players[:WHEEL_GOLDEN_TOP_N + 1]
                total_positive_live = sum(
                    max(0, player.jopacoin_balance)
                    for player in visible_players
                )
                bottom_players_live = [
                    player
                    for player in reversed(visible_players)
                    if player.jopacoin_balance >= 1
                ][:30]
            else:
                top_n_extended = await self._get_visible_balance_leaderboard(
                    interaction,
                    guild_id,
                    limit=WHEEL_GOLDEN_TOP_N + 1,
                )
                total_positive_live = await asyncio.to_thread(
                    self.player_service.get_total_positive_balance,
                    guild_id,
                )
                bottom_players_live = await asyncio.to_thread(
                    functools.partial(
                        self.player_service.get_leaderboard_bottom,
                        guild_id,
                        limit=30,
                        min_balance=1,
                    )
                )
            rank_next_live = top_n_extended[WHEEL_GOLDEN_TOP_N] if len(top_n_extended) > WHEEL_GOLDEN_TOP_N else None
            rank_next_balance_live = (
                rank_next_live.jopacoin_balance
                if rank_next_live and rank_next_live.jopacoin_balance > 0
                else None
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
            if not bonus_spin:
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

        # Green mana: bankrupt insurance — first BANKRUPT per mana day downgraded to LOSE
        _insurance_activated = False
        if (
            effects
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

        # Defer first - GIF generation can take a few seconds
        if not _scrying_deferred and not bonus_spin:
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

        # Daily-event economics apply after any interactive wedge resolves but
        # before the outcome processor makes its atomic wallet/Reserve moves.
        # Replacing the wedge keeps history and the result embed aligned with
        # the exact credited/debited integer amount.
        if isinstance(result_value, int) and result_value != 0:
            event_adjusted_value = apply_gamba_event_multiplier(
                result_value,
                win_multiplier=gamba_win_multiplier,
                loss_multiplier=gamba_loss_multiplier,
            )
            result_wedge = (
                result_wedge[0],
                event_adjusted_value,
                result_wedge[2],
            )
            result_value = event_adjusted_value

        # Anchor for post-outcome gain tracking (bankruptcy debuff / Blood Pact skim),
        # captured AFTER any interactive TOWN_TRIAL/DISCOVER wait so a concurrent
        # balance change during that window (a steal/tip/settlement landing on the
        # spinner) isn't misattributed as wheel winnings. Only fetched when a real
        # service could consume the anchor — an unconditional extra get_balance call
        # would change mock call counts in tests (and an exhausted mock raising
        # StopIteration deadlocks asyncio.to_thread).
        from services.buff_service import BuffService

        buff_service = getattr(self.bot, "buff_service", None)
        player_repo = getattr(self.bot, "player_repo", None)
        track_wheel_gains = (
            bankruptcy_service is not None
            or isinstance(buff_service, BuffService)
        )
        balance_before_outcome = None
        if track_wheel_gains:
            balance_before_outcome = await asyncio.to_thread(
                self.player_service.get_balance, user_id, guild_id
            )

        hostile_event_prefix = (
            f"wheel:{guild_id}:{wheel_event_id}:{str(result_value).lower()}"
        )

        landed_wedge = result_wedge
        outcome_state = WheelOutcomeState(
            result_wedge=result_wedge,
            new_balance=new_balance,
        )
        outcome_context = WheelOutcomeContext(
            command=self,
            interaction=interaction,
            user_id=user_id,
            guild_id=guild_id,
            bankruptcy_service=bankruptcy_service,
            penalty_games_remaining=penalty_games_remaining,
            effects=effects,
            mana_effects_service=mana_effects_service,
            is_bad_gamba=is_eligible_for_bad_gamba,
            hostile_event_prefix=hostile_event_prefix,
            gamba_win_multiplier=gamba_win_multiplier,
            gamba_loss_multiplier=gamba_loss_multiplier,
        )
        outcome_state = await WheelOutcomeProcessor(
            outcome_context, outcome_state
        ).process()
        result_wedge = outcome_state.result_wedge
        result_value = outcome_state.result_value
        new_balance = outcome_state.new_balance
        garnished_amount = outcome_state.garnished_amount
        # result_value == 0: "Lose a Turn" - no balance change, but extended cooldown
        if bonus_spin:
            next_spin_time = await regular_next_spin_time()
        elif result_value == 0:
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
        if reminder_svc and not bonus_spin:
            reminder_svc.schedule_wheel_reminder(self.bot, user_id, guild_id, next_spin_time)

        # Log the wheel spin for history tracking.
        log_result = outcome_state.log_result()
        outcome_code = _canonical_wheel_outcome_code(
            landed_wedge,
            result_value,
            is_bankrupt=is_eligible_for_bad_gamba,
            is_golden=is_golden,
        )
        outcome_metadata = _wheel_outcome_metadata(
            outcome_state,
            landed_wedge,
            outcome_code,
            log_result,
        )
        if _insurance_activated:
            outcome_metadata["mana_insurance_activated"] = True
        if _reroll_used:
            outcome_metadata["mana_reroll_used"] = True
        outcome_metadata["event_win_multiplier"] = gamba_win_multiplier
        outcome_metadata["event_loss_multiplier"] = gamba_loss_multiplier

        await asyncio.to_thread(
            functools.partial(
                self.player_service.log_wheel_spin,
                discord_id=user_id,
                guild_id=guild_id,
                result=log_result,
                spin_time=int(now),
                is_bankrupt=is_eligible_for_bad_gamba,
                is_golden=is_golden,
                outcome_code=outcome_code,
                is_bonus=bonus_spin,
                event_id=wheel_event_id,
                outcome_metadata=outcome_metadata,
            )
        )

        # Bankruptcy debuff: dock the configured share of the spinner's net wheel
        # winnings (any positive balance gain this spin). The wheel has no stake,
        # so the whole positive delta is winnings; it applies regardless of debt
        # and the penalty is a coin sink. Losses (gain <= 0) are untouched.
        wheel_bankruptcy_penalty = 0
        if bankruptcy_service:
            current_balance = await asyncio.to_thread(
                self.player_service.get_balance, user_id, guild_id
            )
            gain = current_balance - balance_before_outcome
            if gain > 0:
                _pen_info = await asyncio.to_thread(
                    bankruptcy_service.apply_penalty_to_winnings, user_id, gain, guild_id
                )
                wheel_bankruptcy_penalty = _pen_info["penalty_applied"]
                if wheel_bankruptcy_penalty > 0:
                    await asyncio.to_thread(
                        self._adjust_gamba_balance,
                        user_id,
                        user_id,
                        guild_id,
                        -wheel_bankruptcy_penalty,
                        "gamba bankruptcy penalty",
                        str(result_wedge[0]),
                        {"wheel_gain": gain},
                    )
                    new_balance = current_balance - wheel_bankruptcy_penalty

        if (
            isinstance(buff_service, BuffService)
            and player_repo is not None
            and balance_before_outcome is not None
        ):
            try:
                current_balance = await asyncio.to_thread(
                    self.player_service.get_balance, user_id, guild_id
                )
                gain = current_balance - balance_before_outcome
                if gain > 0:
                    skimmed = await asyncio.to_thread(
                        buff_service.apply_blood_pact_skim,
                        user_id,
                        guild_id,
                        gain,
                        player_repo,
                    )
                    if skimmed:
                        new_balance = current_balance - skimmed
            except Exception:
                logger.exception("Failed to apply Blood Pact skim to wheel payout")

        # Send final result embed
        await asyncio.sleep(0.5)  # Brief pause before result reveal
        result_embed = self._wheel_result_embed(
            result_wedge, new_balance, garnished_amount, next_spin_time,
            bankruptcy_penalty=wheel_bankruptcy_penalty,
            is_bankrupt=is_eligible_for_bad_gamba,
            is_golden=is_golden,
            **outcome_state.embed_kwargs(),
        )
        if bonus_spin:
            result_embed.add_field(
                name="Dig Bonus",
                value="Free spin — regular `/gamba` cooldown unchanged.",
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
            if result_value == "LIGHTNING_BOLT" and outcome_state.lightning_total > 0:
                _lt = outcome_state.lightning_total
                _lc = outcome_state.lightning_count
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
    @app_commands.command(name="nonprofit", description="View the Jopacoin Reserve")
    async def nonprofit(self, interaction: discord.Interaction):
        await _nonprofit_action(self, interaction)
    @app_commands.command(
        name="disburse", description="Propose or manage Jopacoin Reserve allocation"
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
        """Propose, view, or reset Jopacoin Reserve allocation voting."""
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
    # optional services: bankruptcy_service, gambling_stats_service, loan_service, disburse_service, flavor_text_service, tip_service

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
    )
    await bot.add_cog(cog)

    # Register persistent view for disbursement voting
    if disburse_service:
        bot.add_view(DisburseVoteView(disburse_service, cog))
