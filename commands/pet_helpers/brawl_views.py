"""Interactive views for pet brawls.

Two stages share one public message: the challenge (accept / decline /
withdraw) and the battle (three move buttons, both owners picking secretly).
Views are stateless renderers over a cog-held ``PetBrawlSession`` — the
persisted row only tracks lifecycle, so a bot restart voids the fight
harmlessly (stakes are hunger-only and settle at the very end).

Message updates go through ``message.edit`` with a channel.send fallback
(``utils.interaction_safety.edit_message_with_fallback``) — never the
15-minute interaction token.
"""

from __future__ import annotations

import asyncio
import logging
import random
from dataclasses import dataclass, field
from typing import TYPE_CHECKING

import discord

from commands.pet_helpers import brawl_embeds
from domain.models.pet_brawl import PetBrawl
from domain.pet_brawl import (
    MOVE_EMOJI,
    SAFE_MOVE,
    PetBrawlMove,
    PetBrawlState,
    move_name,
    resolve_round,
)
from domain.pet_constants import (
    PET_BRAWL_ACCEPT_SECONDS,
    PET_BRAWL_TURN_SECONDS,
)
from utils.interaction_safety import edit_message_with_fallback, safe_defer

if TYPE_CHECKING:
    from commands.pet import PetCommands

logger = logging.getLogger("cama_bot.commands.pet_brawl")

_MOVE_BUTTONS = (
    (PetBrawlMove.SPIT, "Spit"),
    (PetBrawlMove.STAMPEDE, "Stampede"),
    (PetBrawlMove.HUNKER, "Hunker Down"),
)


@dataclass
class PetBrawlSession:
    """In-memory battle state for one active brawl, keyed by brawl_id."""

    brawl_id: int
    guild_id: int | None
    state: PetBrawlState
    rng: random.Random
    picks: dict[str, PetBrawlMove] = field(default_factory=dict)
    last_log: tuple[str, ...] = ()
    message: discord.Message | None = None
    consecutive_full_timeouts: int = 0
    lock: asyncio.Lock = field(default_factory=asyncio.Lock)


async def _edit_or_send(message: discord.Message | None, **kwargs) -> None:
    """Edit the brawl message; fall back to a fresh channel send."""
    await edit_message_with_fallback(
        message, log_label="Pet brawl message", **kwargs
    )


class PetBrawlChallengeView(discord.ui.View):
    """Accept / Decline (recipient) and Withdraw (challenger) buttons."""

    def __init__(self, cog: PetCommands, brawl: PetBrawl):
        super().__init__(timeout=PET_BRAWL_ACCEPT_SECONDS)
        self.cog = cog
        self.brawl = brawl
        self.message: discord.Message | None = None
        self._responded = False

    @discord.ui.button(label="Accept", style=discord.ButtonStyle.success, emoji="⚔️")
    async def accept(self, interaction: discord.Interaction, _):
        if interaction.user.id != self.brawl.recipient_id:
            await interaction.response.send_message(
                "This challenge isn't yours to answer.", ephemeral=True
            )
            return
        if self._responded:
            await interaction.response.send_message(
                "Already answered.", ephemeral=True
            )
            return
        self._responded = True
        await interaction.response.defer()
        result = await asyncio.to_thread(
            self.cog.pet_brawl_service.accept,
            self.brawl.brawl_id,
            self.brawl.guild_id,
            interaction.user.id,
        )
        if not result.success:
            self._responded = False
            await interaction.followup.send(f"❌ {result.error}", ephemeral=True)
            if result.error_code == "pet_dead":  # challenge was voided
                self.stop()
                await _edit_or_send(
                    self.message,
                    embed=brawl_embeds.build_void_embed(result.error),
                    view=None,
                    attachments=[],
                )
            return
        session = PetBrawlSession(
            brawl_id=self.brawl.brawl_id,
            guild_id=self.brawl.guild_id,
            state=result.value["state"],
            rng=result.value["rng"],
            message=self.message,
        )
        self.cog._brawl_sessions[self.brawl.brawl_id] = session
        self.stop()
        battle_view = PetBrawlBattleView(self.cog, session)
        embed = brawl_embeds.build_battle_embed(session.state, (), {})
        await _edit_or_send(
            self.message, embed=embed, view=battle_view, attachments=[]
        )
        battle_view.message = self.message

    @discord.ui.button(label="Decline", style=discord.ButtonStyle.secondary)
    async def decline(self, interaction: discord.Interaction, _):
        if interaction.user.id != self.brawl.recipient_id:
            await interaction.response.send_message(
                "This challenge isn't yours to answer.", ephemeral=True
            )
            return
        if self._responded:
            await interaction.response.send_message(
                "Already answered.", ephemeral=True
            )
            return
        self._responded = True
        await interaction.response.defer()
        result = await asyncio.to_thread(
            self.cog.pet_brawl_service.decline,
            self.brawl.brawl_id,
            self.brawl.guild_id,
            interaction.user.id,
        )
        if not result.success:
            self._responded = False
            await interaction.followup.send(f"❌ {result.error}", ephemeral=True)
            return
        self.stop()
        await _edit_or_send(
            self.message,
            embed=brawl_embeds.build_declined_embed(interaction.user.id),
            view=None,
            attachments=[],
        )

    @discord.ui.button(label="Withdraw", style=discord.ButtonStyle.secondary)
    async def withdraw(self, interaction: discord.Interaction, _):
        if interaction.user.id != self.brawl.challenger_id:
            await interaction.response.send_message(
                "Only the challenger can withdraw.", ephemeral=True
            )
            return
        if self._responded:
            await interaction.response.send_message(
                "Already answered.", ephemeral=True
            )
            return
        self._responded = True
        await interaction.response.defer()
        result = await asyncio.to_thread(
            self.cog.pet_brawl_service.withdraw,
            self.brawl.brawl_id,
            self.brawl.guild_id,
            interaction.user.id,
        )
        if not result.success:
            self._responded = False
            await interaction.followup.send(f"❌ {result.error}", ephemeral=True)
            return
        self.stop()
        await _edit_or_send(
            self.message,
            embed=brawl_embeds.build_void_embed("The challenger thought better of it."),
            view=None,
            attachments=[],
        )

    async def on_timeout(self):
        if self._responded:
            return
        self._responded = True
        await asyncio.to_thread(
            self.cog.pet_brawl_service.void, self.brawl.brawl_id, self.brawl.guild_id
        )
        await _edit_or_send(
            self.message,
            embed=brawl_embeds.build_expired_embed(),
            view=None,
            attachments=[],
        )


class PetBrawlBattleView(discord.ui.View):
    """One round of secret simultaneous picks on the shared battle message."""

    def __init__(self, cog: PetCommands, session: PetBrawlSession):
        super().__init__(timeout=PET_BRAWL_TURN_SECONDS)
        self.cog = cog
        self.session = session
        self.message: discord.Message | None = session.message
        self._resolved = False
        # The round number MUST be part of the custom_id: successive rounds
        # share one message, and ViewStore.remove_view (run by the previous
        # round's stop()) deregisters by (type, custom_id) with no identity
        # check — identical ids let the old view's stop() kill the new
        # round's buttons, leaving every click silently discarded.
        for move, label in _MOVE_BUTTONS:
            btn = discord.ui.Button(
                label=label,
                emoji=MOVE_EMOJI[move],
                style=discord.ButtonStyle.primary,
                custom_id=(
                    f"pet_brawl:{session.brawl_id}:"
                    f"{session.state.round_no}:{move.value}"
                ),
            )
            btn.callback = self._make_callback(move)
            self.add_item(btn)

    def _side_of(self, user_id: int) -> str | None:
        if user_id == self.session.state.a.owner_id:
            return "a"
        if user_id == self.session.state.b.owner_id:
            return "b"
        return None

    def _make_callback(self, move: PetBrawlMove):
        async def callback(interaction: discord.Interaction):
            session = self.session
            side = self._side_of(interaction.user.id)
            if side is None:
                await interaction.response.send_message(
                    "Only the two duelists can fight this battle.", ephemeral=True
                )
                return
            # Ack before touching the lock: a round resolving on the other
            # side (DB settle + art render + edits) can hold it well past
            # Discord's 3-second response window, and a click that waits
            # that long dies as "This interaction failed".
            if not await safe_defer(interaction):
                return
            async with session.lock:
                if self._resolved:
                    await interaction.followup.send(
                        "That round already resolved.", ephemeral=True
                    )
                    return
                if side in session.picks:
                    await interaction.followup.send(
                        "You're already locked in for this round.", ephemeral=True
                    )
                    return
                session.picks[side] = move
                duelist = session.state.a if side == "a" else session.state.b
                flavored = move_name(duelist.species_id, move)
                try:
                    await interaction.followup.send(
                        f"{MOVE_EMOJI[move]} You picked **{flavored}** ✅ — "
                        "waiting for your opponent.",
                        ephemeral=True,
                    )
                except discord.HTTPException as exc:
                    # The pick is recorded; a failed ephemeral ack must not
                    # abort resolution and stall the round until timeout.
                    logger.warning("Pet brawl pick ack failed: %s", exc)
                if len(session.picks) == 2:
                    session.consecutive_full_timeouts = 0
                    self._resolved = True
                    await self._resolve()
                else:
                    # Reveal WHO is locked in, never what they picked.
                    await _edit_or_send(
                        self.message,
                        embed=brawl_embeds.build_battle_embed(
                            session.state, session.last_log, session.picks
                        ),
                    )

        return callback

    async def on_timeout(self):
        session = self.session
        if self._resolved:
            return
        async with session.lock:
            if self._resolved:
                return
            missing = [side for side in ("a", "b") if side not in session.picks]
            if len(missing) == 2:
                session.consecutive_full_timeouts += 1
                if session.consecutive_full_timeouts >= 2:
                    self._resolved = True
                    self.cog._brawl_sessions.pop(session.brawl_id, None)
                    await asyncio.to_thread(
                        self.cog.pet_brawl_service.void,
                        session.brawl_id,
                        session.guild_id,
                    )
                    await _edit_or_send(
                        self.message,
                        embed=brawl_embeds.build_void_embed(
                            "Both camas wandered off mid-fight. The brawl is called."
                        ),
                        view=None,
                    )
                    return
            else:
                session.consecutive_full_timeouts = 0
            auto_notes = []
            for side in missing:
                session.picks[side] = SAFE_MOVE
                duelist = session.state.a if side == "a" else session.state.b
                auto_notes.append(
                    f"⏰ **{duelist.name}** never picked — "
                    f"{move_name(duelist.species_id, SAFE_MOVE)} thrown "
                    "automatically."
                )
            self._resolved = True
            await self._resolve(prefix_log=tuple(auto_notes))

    async def _resolve(self, prefix_log: tuple[str, ...] = ()) -> None:
        """Resolve the round. Caller holds session.lock and set _resolved."""
        session = self.session
        move_a, move_b = session.picks["a"], session.picks["b"]
        session.picks = {}
        state, log = resolve_round(session.state, move_a, move_b, session.rng)
        log = prefix_log + log
        rounds = state.round_no
        session.state = state
        session.last_log = log
        if state.winner is None:
            next_view = PetBrawlBattleView(self.cog, session)
            await _edit_or_send(
                self.message,
                embed=brawl_embeds.build_battle_embed(state, log, {}),
                view=next_view,
            )
            next_view.message = self.message
            self.stop()
            return
        self.cog._brawl_sessions.pop(session.brawl_id, None)
        result = await asyncio.to_thread(
            self.cog.pet_brawl_service.settle,
            session.brawl_id,
            session.guild_id,
            state,
            rounds,
        )
        if not result.success:
            await asyncio.to_thread(
                self.cog.pet_brawl_service.void,
                session.brawl_id,
                session.guild_id,
            )
            await _edit_or_send(
                self.message,
                embed=brawl_embeds.build_void_embed(
                    f"The fight ended but settling it failed: {result.error}"
                ),
                view=None,
            )
            self.stop()
            return
        embed, file = await asyncio.to_thread(
            brawl_embeds.build_result_embed, result.value, rounds, log
        )
        await _edit_or_send(
            self.message,
            embed=embed,
            view=None,
            attachments=[file] if file else [],
        )
        self.stop()
