"""
Lobby commands: /lobby, /kick, /resetlobby.

Uses Discord threads for lobby management similar to /prediction.
"""

import asyncio
import functools
import logging
import time
from collections.abc import Awaitable

import discord
from discord import app_commands
from discord.ext import commands, tasks

from commands.checks import require_guild
from config import LOBBY_CHANNEL_ID, LOWSKILL_LOBBY_CHANNEL_ID
from domain.models.lobby import LobbyKind
from services.lobby_service import LobbyService
from services.permissions import has_admin_permission
from utils.curfew import format_curfew_window, is_within_curfew
from utils.formatting import (
    JOPACOIN_EMOJI_ID,
    format_duration_short,
)
from utils.interaction_safety import safe_defer, safe_followup, update_lobby_message_closed
from utils.lobby_selection import format_lobby_labels, resolve_lobby_kind
from utils.neon_helpers import get_neon_service
from utils.pin_helpers import safe_unpin_message
from utils.rate_limiter import GLOBAL_RATE_LIMITER
from utils.suspension_format import format_suspension_scope, format_suspension_terms

logger = logging.getLogger("cama_bot.commands.lobby")

# Players who joined within this window are considered active and ready.
RECENT_JOIN_THRESHOLD = 10 * 60  # 10 minutes

# User-facing Whine & Cheese copy derives from the enforced cutoff so tuning
# the constant can never desync the messaging from the actual rule.
LOWSKILL_CUTOFF_TEXT = f"{LobbyService.LOWSKILL_RATING_CUTOFF:.0f}"

# A ready check older than this is stale: the next /readycheck deletes the
# buried original and posts a fresh one (resetting ✅ confirmations and pruning
# AFK no-shows) instead of editing it in place.
READYCHECK_STALE_THRESHOLD = 30 * 60  # 30 minutes


def _get_recent_signup_ids(player_data: dict[int, dict]) -> set[int]:
    """Return lobby signups that are recent enough to count as ready."""
    now = time.time()
    return {
        discord_id
        for discord_id, data in player_data.items()
        if (join_ts := data.get("join_ts")) is not None
        and (now - join_ts) < RECENT_JOIN_THRESHOLD
    }


def _private_suspension_message(suspension) -> str:
    """Describe a restriction only in ephemeral responses or target DMs."""
    if suspension is None:
        return "You are temporarily suspended from this matchmaking lobby."
    scope_text = format_suspension_scope(suspension)
    term_text = format_suspension_terms(suspension, empty="temporarily")
    return (
        f"You are suspended from {scope_text} {term_text}. "
        f"Reason: {getattr(suspension, 'reason', 'Not provided')}"
    )


class LobbyCommands(commands.Cog):
    """Slash commands for lobby management."""

    def __init__(
        self,
        bot: commands.Bot,
        lobby_service: LobbyService,
        player_service,
        curfew_service=None,
    ):
        self.bot = bot
        self.lobby_service = lobby_service
        self.player_service = player_service
        self.curfew_service = curfew_service
        self._readycheck_shuffle_notified: dict[
            tuple[int, LobbyKind], int
        ] = {}
        self._readycheck_shuffle_notification_locks: dict[
            tuple[int, LobbyKind], asyncio.Lock
        ] = {}
        self._readycheck_refresh_locks: dict[
            tuple[int, LobbyKind], asyncio.Lock
        ] = {}
        self._lobby_player_notification_tasks: set[asyncio.Task] = set()

    async def cog_load(self) -> None:
        if self.curfew_service is not None:
            self._curfew_sweep_loop.start()

    async def cog_unload(self) -> None:
        if self.curfew_service is not None:
            self._curfew_sweep_loop.cancel()

    # ────────────────────────────────────────────────────────────────────
    # Curfew enforcement
    # ────────────────────────────────────────────────────────────────────

    @tasks.loop(minutes=1)
    async def _curfew_sweep_loop(self):
        guild_ids = [guild.id for guild in self.bot.guilds]
        if not guild_ids:
            return
        try:
            kicks = await asyncio.to_thread(self.curfew_service.sweep, guild_ids)
        except Exception:
            logger.exception("Curfew sweep failed")
            return
        for kick in kicks:
            try:
                await self._deliver_curfew_kick(kick)
            except Exception:
                logger.exception(
                    "Curfew kick delivery failed for discord_id=%s guild_id=%s",
                    kick.discord_id,
                    kick.guild_id,
                )

    @_curfew_sweep_loop.before_loop
    async def _before_curfew_sweep(self):
        await self.bot.wait_until_ready()

    async def _deliver_curfew_kick(self, kick) -> None:
        """DM a player removed from a lobby by the curfew sweep, then refresh the display."""
        try:
            user = self.bot.get_user(kick.discord_id) or await self.bot.fetch_user(kick.discord_id)
            await user.send(
                f"🌙 Your bedtime hit, so you've been removed from {kick.lobby_kind.label}. "
                "Use `/player curfew off` if you'd rather queue through it."
            )
        except Exception as exc:
            logger.debug("Failed to DM user %d about curfew kick: %s", kick.discord_id, exc)

        active_lobby = await asyncio.to_thread(
            self.lobby_service.get_lobby,
            guild_id=kick.guild_id,
            lobby_kind=kick.lobby_kind,
        )
        if active_lobby is not None:
            await self._sync_lobby_displays(active_lobby, kick.guild_id)

    def rebuild_readycheck_embed(
        self,
        guild_id: int | None = None,
        expected_message_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> discord.Embed | None:
        """Rebuild the readycheck embed from stored data. Used by bot.py reaction handler."""
        if expected_message_id is None:
            expected_message_id = self.lobby_service.get_readycheck_message_id(
                guild_id=guild_id,
                lobby_kind=lobby_kind,
            )
        if expected_message_id is None:
            return None

        snapshot = self.lobby_service.get_readycheck_display_snapshot(
            expected_message_id,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )
        if snapshot is None:
            return None
        player_data, reacted = snapshot
        embed, _ = build_readycheck_embed(
            player_data,
            reacted,
            ready_threshold=self.lobby_service.ready_threshold,
            lobby_kind=lobby_kind,
        )
        return embed

    async def _classify_readycheck_players(
        self,
        guild: discord.Guild,
        lobby,
        guild_id: int | None,
    ) -> dict[int, dict]:
        """Build the live readycheck classification for the current lobby roster."""
        player_data: dict[int, dict] = {}
        now = time.time()

        for pid in list(lobby.players):
            member = guild.get_member(pid)
            if not member:
                try:
                    member = await guild.fetch_member(pid)
                except Exception:
                    player = await asyncio.to_thread(
                        self.player_service.get_player,
                        pid,
                        guild_id,
                    )
                    fallback_name = player.name if player else f"User {pid}"
                    player_data[pid] = {
                        "group": "afk",
                        "signals": "🔴",
                        "name": fallback_name,
                        "join_ts": lobby.player_join_times.get(pid),
                        "is_member": False,
                    }
                    continue

            join_ts = lobby.player_join_times.get(pid)
            time_in_lobby = (now - join_ts) if join_ts else float("inf")
            is_recent = time_in_lobby < RECENT_JOIN_THRESHOLD

            signals = []
            is_afk = False

            in_voice = member.voice is not None
            if in_voice:
                is_deafened = bool(
                    getattr(member.voice, "self_deaf", False)
                    or getattr(member.voice, "deaf", False)
                )
                signals.append("🔇" if is_deafened else "🔊")

            if _is_playing_dota(member):
                signals.append("🎮")

            status = member.status
            if status in (discord.Status.online, discord.Status.dnd):
                signals.append("🟢")
            elif status == discord.Status.idle:
                signals.append("🟡")
                if not is_recent and not in_voice:
                    is_afk = True
            else:
                signals.append("🔴")
                if not is_recent and not in_voice:
                    is_afk = True

            player_data[pid] = {
                "group": "afk" if is_afk else "active",
                "signals": "".join(signals),
                "name": member.display_name,
                "join_ts": join_ts,
                "is_member": True,
            }

        return player_data

    async def _apply_readycheck_lobby_update(
        self,
        message,
        player_data: dict[int, dict],
        guild_id: int | None,
        *,
        auto_confirm_ids: set[int] | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> tuple[bool, bool, list[int]]:
        """Return state/display success and mentions for one readycheck generation."""
        previous_snapshot = await asyncio.to_thread(
            self.lobby_service.get_readycheck_display_snapshot,
            message.id,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )
        if previous_snapshot is None:
            return False, False, []
        previous_player_data, previous_reacted = previous_snapshot
        effective_auto_confirm_ids = _get_recent_signup_ids(player_data)
        # Never auto re-confirm a player who explicitly withdrew their ✅ for
        # this readycheck; clicking ✅ again is the only way back to ready.
        # Only the background sweep is filtered: the explicitly passed
        # auto_confirm_ids (the /readycheck invoker) are a fresh readiness
        # signal, and add_readycheck_reaction below clears their declined
        # marker exactly like a manual re-click would.
        declined_ids = await asyncio.to_thread(
            self.lobby_service.get_readycheck_declined,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )
        effective_auto_confirm_ids.difference_update(declined_ids)
        effective_auto_confirm_ids.update(auto_confirm_ids or set())
        effective_auto_confirm_ids.intersection_update(player_data)
        departed_confirmations = set(previous_reacted) - set(player_data)
        new_unconfirmed_players = (
            set(player_data) - set(previous_player_data) - set(previous_reacted)
        ) - effective_auto_confirm_ids

        for discord_id in departed_confirmations | new_unconfirmed_players:
            try:
                await message.remove_reaction("✅", discord.Object(id=discord_id))
            except Exception as exc:
                # Best-effort display cleanup only: the bot may lack Manage
                # Messages (Forbidden) or the player may have no physical
                # reaction at all (auto-confirms are state-only, NotFound).
                # The authoritative state update below must still run so
                # departed players stop counting as confirmed.
                logger.warning(
                    "Failed to clear departed readycheck reaction for %s: %s",
                    discord_id,
                    exc,
                )

        removed_confirmations = await asyncio.to_thread(
            self.lobby_service.update_readycheck_data,
            set(player_data),
            player_data,
            guild_id=guild_id,
            expected_message_id=message.id,
            lobby_kind=lobby_kind,
        )
        if removed_confirmations is None:
            return False, False, []

        for auto_confirm_id in effective_auto_confirm_ids:
            await asyncio.to_thread(
                self.lobby_service.add_readycheck_reaction,
                auto_confirm_id,
                f"<@{auto_confirm_id}>",
                guild_id=guild_id,
                expected_message_id=message.id,
                lobby_kind=lobby_kind,
            )

        snapshot = await asyncio.to_thread(
            self.lobby_service.get_readycheck_display_snapshot,
            message.id,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )
        if snapshot is None:
            return True, False, []
        current_player_data, reacted = snapshot
        embed, mention_ids = build_readycheck_embed(
            current_player_data,
            reacted,
            ready_threshold=self.lobby_service.ready_threshold,
            lobby_kind=lobby_kind,
        )

        current_message_id = await asyncio.to_thread(
            self.lobby_service.get_readycheck_message_id,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )
        if current_message_id != message.id:
            return True, False, []

        try:
            await message.edit(embed=embed)
        except Exception as exc:
            logger.warning(
                "Failed to edit readycheck display for guild %s: %s",
                guild_id,
                exc,
            )
            return True, False, []

        return True, True, mention_ids

    async def sync_readycheck_with_lobby(
        self,
        guild_id: int | None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> bool:
        """Refresh the active readycheck from the live lobby after roster changes."""
        kind = LobbyKind.normalize(lobby_kind)
        normalized_guild_id = (guild_id or 0, kind)
        refresh_lock = self._readycheck_refresh_locks.setdefault(
            normalized_guild_id,
            asyncio.Lock(),
        )
        async with refresh_lock:
            message_id = await asyncio.to_thread(
                self.lobby_service.get_readycheck_message_id,
                guild_id=guild_id,
                lobby_kind=kind,
            )
            channel_id = await asyncio.to_thread(
                self.lobby_service.get_readycheck_channel_id,
                guild_id=guild_id,
                lobby_kind=kind,
            )
            if message_id is None or channel_id is None:
                return False

            guild = self.bot.get_guild(guild_id) if guild_id is not None else None
            lobby = await asyncio.to_thread(
                self.lobby_service.get_lobby,
                guild_id=guild_id,
                lobby_kind=kind,
            )
            if guild is None or lobby is None:
                return False

            player_data = await self._classify_readycheck_players(
                guild,
                lobby,
                guild_id,
            )

            try:
                channel = self.bot.get_channel(channel_id)
                if not channel:
                    channel = await self.bot.fetch_channel(channel_id)
                message = channel.get_partial_message(message_id)
                state_updated, display_updated, _ = (
                    await self._apply_readycheck_lobby_update(
                        message,
                        player_data,
                        guild_id,
                        lobby_kind=kind,
                    )
                )
                if state_updated:
                    await self.notify_readycheck_completion_if_ready(
                        guild_id or 0,
                        message_id,
                        fallback_channel=channel,
                        lobby_kind=kind,
                    )
                return display_updated
            except (discord.Forbidden, discord.NotFound, discord.HTTPException) as exc:
                logger.warning(
                    "Failed to sync readycheck display for guild %s: %s",
                    guild_id,
                    exc,
                )
                return False
            except Exception as exc:
                logger.warning(
                    "Unexpected readycheck display sync failure for guild %s: %s",
                    guild_id,
                    exc,
                )
                return False

    async def notify_readycheck_complete(
        self,
        guild_id: int,
        readycheck_message_id: int,
        *,
        fallback_channel=None,
        fallback_channel_id: int | None = None,
        _require_current_readycheck: bool = False,
        lobby_kind: LobbyKind | str | None = None,
    ) -> bool:
        """Recommend shuffling once per ready check after ten confirmations."""
        kind = LobbyKind.normalize(lobby_kind)
        key = (guild_id, kind)
        notification_lock = self._readycheck_shuffle_notification_locks.setdefault(
            key,
            asyncio.Lock(),
        )
        async with notification_lock:
            if (
                self._readycheck_shuffle_notified.get(key)
                == readycheck_message_id
            ):
                return False

            origin_channel_id = None
            try:
                origin_channel_id = await asyncio.to_thread(
                    self.lobby_service.get_origin_channel_id,
                    guild_id=guild_id,
                    lobby_kind=kind,
                )
            except Exception as exc:
                logger.warning(
                    "Could not load readycheck origin channel for guild %s: %s",
                    guild_id,
                    exc,
                )

            target_channel = None
            if origin_channel_id:
                try:
                    target_channel = self.bot.get_channel(origin_channel_id)
                    if not target_channel:
                        target_channel = await self.bot.fetch_channel(origin_channel_id)
                except Exception as exc:
                    logger.warning(
                        "Could not fetch readycheck origin channel %s: %s",
                        origin_channel_id,
                        exc,
                    )

            if target_channel is None:
                target_channel = fallback_channel
            if target_channel is None and fallback_channel_id:
                try:
                    target_channel = self.bot.get_channel(fallback_channel_id)
                    if not target_channel:
                        target_channel = await self.bot.fetch_channel(fallback_channel_id)
                except Exception as exc:
                    logger.warning(
                        "Could not fetch readycheck fallback channel %s: %s",
                        fallback_channel_id,
                        exc,
                    )

            if target_channel is None:
                logger.error(
                    "No channel available for readycheck completion in guild %s",
                    guild_id,
                )
                return False

            if _require_current_readycheck:
                reacted_count = await asyncio.to_thread(
                    self.lobby_service.get_readycheck_reaction_count_for_message,
                    readycheck_message_id,
                    guild_id=guild_id,
                    lobby_kind=kind,
                )
                if (
                    reacted_count is None
                    or reacted_count < self.lobby_service.ready_threshold
                ):
                    return False

            try:
                await target_channel.send(
                    f"✅ **{kind.label}: "
                    f"{self.lobby_service.ready_threshold} players confirmed ready.** "
                    "Anyone in this lobby can use `/shuffle` now. "
                    "Only players who confirmed ready will be included; "
                    "everyone else will sit out."
                )
            except Exception as exc:
                logger.error(
                    "Error notifying readycheck completion: %s",
                    exc,
                    exc_info=True,
                )
                return False

            self._readycheck_shuffle_notified[key] = readycheck_message_id
            return True

    async def notify_readycheck_completion_if_ready(
        self,
        guild_id: int,
        readycheck_message_id: int,
        *,
        fallback_channel=None,
        fallback_channel_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> bool:
        """Notify only while this ready-check generation is current and full."""
        reacted_count = await asyncio.to_thread(
            self.lobby_service.get_readycheck_reaction_count_for_message,
            readycheck_message_id,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )
        if reacted_count is None or reacted_count < self.lobby_service.ready_threshold:
            return False
        return await self.notify_readycheck_complete(
            guild_id,
            readycheck_message_id,
            fallback_channel=fallback_channel,
            fallback_channel_id=fallback_channel_id,
            _require_current_readycheck=True,
            lobby_kind=lobby_kind,
        )

    async def _get_lobby_target_channel(
        self,
        interaction: discord.Interaction,
        lobby_kind: LobbyKind | str | None = None,
    ) -> discord.abc.Messageable | None:
        """
        Get the target channel for lobby embeds.

        Whine & Cheese uses :data:`LOWSKILL_LOBBY_CHANNEL_ID` when configured,
        then falls back to the regular :data:`LOBBY_CHANNEL_ID`. Any lobby
        without a usable configured channel falls back to
        ``interaction.channel``. Returns ``None`` only if no usable channel
        could be resolved.
        """
        kind = LobbyKind.normalize(lobby_kind)
        configured_channel_id = (
            LOWSKILL_LOBBY_CHANNEL_ID
            if kind is LobbyKind.LOWSKILL and LOWSKILL_LOBBY_CHANNEL_ID
            else LOBBY_CHANNEL_ID
        )

        # If no dedicated channel configured, use interaction channel
        if not configured_channel_id:
            return interaction.channel

        try:
            channel = self.bot.get_channel(configured_channel_id)
            if not channel:
                channel = await self.bot.fetch_channel(configured_channel_id)

            # Verify we can send messages to this channel
            if isinstance(channel, discord.TextChannel):
                # Ensure dedicated channel is in the same guild
                if interaction.guild and channel.guild.id != interaction.guild.id:
                    logger.warning(
                        f"Dedicated lobby channel {configured_channel_id} is in different guild"
                    )
                    return interaction.channel

                perms = channel.permissions_for(channel.guild.me)
                if not perms.send_messages or not perms.create_public_threads:
                    logger.warning(
                        f"Bot lacks permissions in dedicated lobby channel {configured_channel_id}"
                    )
                    return interaction.channel

            return channel
        except (discord.NotFound, discord.Forbidden) as exc:
            logger.warning(
                f"Cannot access dedicated lobby channel {configured_channel_id}: {exc}"
            )
            return interaction.channel
        except Exception as exc:
            logger.warning(f"Error fetching dedicated lobby channel: {exc}")
            return interaction.channel

    async def _safe_pin(self, message: discord.Message) -> None:
        """Pin the lobby message, logging but not raising on failure (e.g., missing perms)."""
        try:
            await message.pin(reason="Cama lobby active")
        except discord.Forbidden:
            logger.warning("Cannot pin lobby message: missing Manage Messages permission.")
        except Exception as exc:
            logger.warning(f"Failed to pin lobby message: {exc}")

    async def _add_lobby_reactions(self, message: discord.Message) -> None:
        """Add lobby controls in their stable display order, best-effort."""
        try:
            await message.add_reaction("⚔️")
            jopacoin_emoji = discord.PartialEmoji(
                name="jopacoin",
                id=JOPACOIN_EMOJI_ID,
            )
            await message.add_reaction(jopacoin_emoji)
            await message.add_reaction("📋")
            await message.add_reaction("🔔")
        except Exception as exc:
            logger.debug("Failed to add lobby reactions: %s", exc)

    async def _create_lobby_thread_with_decorations(
        self,
        message: discord.Message,
        lobby_kind: LobbyKind | str | None = None,
    ):
        """Create the required thread while bounded optional decoration runs.

        Pinning and the ordered reaction sequence are independent once the
        starter message exists, so they run as two best-effort branches beside
        thread creation. ``gather`` awaits every branch even when thread
        creation fails; no publication work is detached from the command.
        """

        async def decorate_message() -> None:
            await asyncio.gather(
                self._safe_pin(message),
                self._add_lobby_reactions(message),
            )

        kind = LobbyKind.normalize(lobby_kind)
        thread_result, decoration_result = await asyncio.gather(
            message.create_thread(name=kind.label),
            decorate_message(),
            return_exceptions=True,
        )
        if isinstance(thread_result, BaseException):
            raise thread_result
        if isinstance(decoration_result, BaseException):
            raise decoration_result
        return thread_result

    async def _run_lobby_publication_wave(
        self,
        maintenance: list[tuple[str, Awaitable[object]]],
        *,
        followup: Awaitable[object] | None = None,
    ) -> None:
        """Await a publication wave without hiding the user response.

        Display, reaction, and thread upkeep is best-effort after the membership
        mutation has committed. The ephemeral confirmation is different: if it
        fails, let the command error path surface it after all sibling work has
        finished so no gather-created task is left running.
        """
        operations: list[Awaitable[object]] = []
        if followup is not None:
            operations.append(followup)
        operations.extend(operation for _, operation in maintenance)

        results = await asyncio.gather(*operations, return_exceptions=True)
        maintenance_results = results
        followup_failure: BaseException | None = None
        if followup is not None:
            followup_result = results[0]
            maintenance_results = results[1:]
            if isinstance(followup_result, BaseException):
                followup_failure = followup_result

        for (label, _), result in zip(
            maintenance, maintenance_results, strict=True
        ):
            if isinstance(result, asyncio.CancelledError):
                raise result
            if isinstance(result, BaseException):
                logger.warning("Lobby %s failed: %s", label, result)

        if followup_failure is not None:
            raise followup_failure


    async def _remove_user_lobby_reactions(
        self,
        user: discord.User | discord.Member,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> None:
        """Remove a user's sword reaction from the channel lobby message."""
        message_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_message_id,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )
        channel_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_channel_id,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )
        if not message_id or not channel_id:
            return

        try:
            channel = self.bot.get_channel(channel_id)
            if not channel:
                channel = await self.bot.fetch_channel(channel_id)
            message = channel.get_partial_message(message_id)
            try:
                await message.remove_reaction("⚔️", user)
            except Exception as e:
                logger.debug("Failed to remove sword reaction: %s", e)
        except discord.Forbidden:
            logger.warning("Cannot remove reaction: missing Manage Messages permission.")
        except Exception as exc:
            logger.warning(f"Failed to remove user lobby reactions: {exc}")

    async def _sync_lobby_displays(
        self,
        lobby,
        guild_id: int | None = None,
        *,
        sync_readycheck: bool = True,
    ) -> None:
        """Update channel message embed (which is also the thread starter)."""
        lobby_kind = lobby.kind

        # Update channel message - this also updates the thread starter view
        message_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_message_id,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )
        channel_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_channel_id,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )
        if message_id and channel_id:
            try:
                channel = self.bot.get_channel(channel_id)
                if not channel:
                    channel = await self.bot.fetch_channel(channel_id)
                message = channel.get_partial_message(message_id)
                from bot import update_lobby_message

                await update_lobby_message(
                    message,
                    lobby,
                    guild_id,
                    expected_message_id=message_id,
                    lobby_kind=lobby_kind,
                    clear_content=True,
                    lobby_service=self.lobby_service,
                )
            except Exception as exc:
                logger.warning(f"Failed to update channel message: {exc}")

        if sync_readycheck:
            await self.sync_readycheck_with_lobby(guild_id, lobby_kind)

    async def _update_thread_embed(self, lobby, guild_id: int | None = None) -> None:
        """Update the pinned embed in the lobby thread."""
        lobby_kind = lobby.kind
        thread_id, embed_message_id, lobby_message_id = await asyncio.gather(
            asyncio.to_thread(
                self.lobby_service.get_lobby_thread_id,
                guild_id=guild_id,
                lobby_kind=lobby_kind,
            ),
            asyncio.to_thread(
                self.lobby_service.get_lobby_embed_message_id,
                guild_id=guild_id,
                lobby_kind=lobby_kind,
            ),
            asyncio.to_thread(
                self.lobby_service.get_lobby_message_id,
                guild_id=guild_id,
                lobby_kind=lobby_kind,
            ),
        )

        if not thread_id or not embed_message_id:
            return

        try:
            thread = self.bot.get_channel(thread_id)
            if not thread:
                thread = await self.bot.fetch_channel(thread_id)

            message = thread.get_partial_message(embed_message_id)
            from bot import update_lobby_message

            await update_lobby_message(
                message,
                lobby,
                guild_id,
                expected_message_id=lobby_message_id,
                lobby_kind=lobby_kind,
                lobby_service=self.lobby_service,
            )
        except Exception as exc:
            logger.warning(f"Failed to update thread embed: {exc}")

    async def _post_join_activity(self, thread_id: int, user: discord.User) -> None:
        """Post a join message in thread and mention user to subscribe them."""
        try:
            thread = self.bot.get_channel(thread_id)
            if not thread:
                thread = await self.bot.fetch_channel(thread_id)

            # Mention user to subscribe them to the thread
            await thread.send(f"✅ {user.mention} joined the lobby!")
        except Exception as exc:
            logger.warning(f"Failed to post join activity: {exc}")

    async def _post_leave_activity(self, thread_id: int, user: discord.User) -> None:
        """Post a leave message in thread."""
        try:
            thread = self.bot.get_channel(thread_id)
            if not thread:
                thread = await self.bot.fetch_channel(thread_id)

            await thread.send(f"🚪 **{user.display_name}** left the lobby.")
        except Exception as exc:
            logger.warning(f"Failed to post leave activity: {exc}")

    async def _deliver_lobby_player_notifications(
        self,
        target_id: int,
        target_display_name: str,
        guild_id: int,
        lobby_kind: LobbyKind,
        join_cutoff_ns: int,
    ) -> None:
        """Deliver target DMs without delaying lobby display or public alerts."""
        reminder_service = getattr(self.bot, "reminder_service", None)
        if reminder_service is None:
            return
        try:
            await reminder_service.notify_lobby_player_subscribers(
                self.bot,
                target_id,
                target_display_name,
                guild_id,
                lobby_kind=lobby_kind,
                join_cutoff_ns=join_cutoff_ns,
            )
        except Exception as exc:
            logger.warning(
                "Failed to send one-shot lobby signup DMs for %s: %s",
                target_id,
                exc,
            )

    def _schedule_lobby_player_notifications(
        self,
        user: discord.User | discord.Member,
        guild_id: int | None,
        lobby_kind: LobbyKind | str | None,
        join_cutoff_ns: int,
    ) -> None:
        """Retain a one-shot notification task for a confirmed lobby join."""
        if getattr(self.bot, "reminder_service", None) is None:
            return
        task = asyncio.create_task(
            self._deliver_lobby_player_notifications(
                user.id,
                user.display_name,
                guild_id or 0,
                LobbyKind.normalize(lobby_kind),
                join_cutoff_ns,
            )
        )
        self._lobby_player_notification_tasks.add(task)
        task.add_done_callback(self._lobby_player_notification_tasks.discard)

    async def _publish_join_activity_and_notifications(
        self,
        lobby,
        user: discord.User | discord.Member,
        guild_id: int | None,
        *,
        auto_join: bool = False,
    ) -> None:
        """Publish ordered thread activity followed by rally/ready notices."""
        from bot import notify_lobby_rally, notify_lobby_ready

        lobby_kind = lobby.kind
        metadata = await asyncio.gather(
            asyncio.to_thread(
                self.lobby_service.get_lobby_thread_id,
                guild_id=guild_id,
                lobby_kind=lobby_kind,
            ),
            asyncio.to_thread(
                self.lobby_service.get_lobby_channel_id,
                guild_id=guild_id,
                lobby_kind=lobby_kind,
            ),
            return_exceptions=True,
        )
        for result in metadata:
            if isinstance(result, BaseException):
                raise result
        thread_id, channel_id = metadata

        # Both messages can target the lobby thread. Keep the subscription-
        # creating join activity ahead of the rally notification.
        if thread_id:
            try:
                await self._post_join_activity(thread_id, user)
            except Exception as exc:
                logger.warning("Failed to post join activity: %s", exc)

        if not channel_id or not thread_id:
            return

        try:
            channel = self.bot.get_channel(channel_id)
            if not channel:
                channel = await self.bot.fetch_channel(channel_id)
            thread = self.bot.get_channel(thread_id)
            if not thread:
                thread = await self.bot.fetch_channel(thread_id)

            is_ready = await asyncio.to_thread(self.lobby_service.is_ready, lobby)
            if not is_ready:
                await notify_lobby_rally(channel, thread, lobby, guild_id or 0)
            else:
                await notify_lobby_ready(
                    channel,
                    guild_id=guild_id or 0,
                    lobby_kind=lobby_kind,
                )
        except Exception as exc:
            context = " on auto-join" if auto_join else ""
            logger.warning(
                "Failed to send rally/ready notification%s: %s", context, exc
            )

    async def _publish_leave_activity(
        self,
        user: discord.User | discord.Member,
        guild_id: int | None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> None:
        """Load the lobby thread and publish a best-effort leave notice."""
        thread_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_thread_id,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )
        if thread_id:
            await self._post_leave_activity(thread_id, user)

    async def _update_channel_message_closed(
        self,
        reason: str = "Lobby Closed",
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> None:
        """Update the channel message embed to show lobby is closed."""
        await update_lobby_message_closed(
            self.bot,
            self.lobby_service,
            reason,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )

    async def _archive_lobby_thread(
        self,
        reason: str = "Lobby Reset",
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> None:
        """Lock and archive the lobby thread with a status message."""
        kind = LobbyKind.normalize(lobby_kind)
        thread_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_thread_id,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )
        if not thread_id:
            return

        try:
            thread = self.bot.get_channel(thread_id)
            if not thread:
                thread = await self.bot.fetch_channel(thread_id)

            # Skip if already archived
            if getattr(thread, "archived", False):
                return

            try:
                await thread.send(f"🚫 **{kind.label} — {reason}**")
            except Exception as e:
                logger.debug("Failed to send archive message to thread: %s", e)

            try:
                await thread.edit(
                    name=f"🚫 {kind.label} — {reason}",
                    locked=True,
                    archived=True,
                )
            except discord.Forbidden:
                try:
                    await thread.edit(archived=True)
                except Exception as e:
                    logger.debug("Failed to archive thread: %s", e)
        except Exception as exc:
            logger.warning(f"Failed to archive lobby thread: {exc}")

    async def _auto_join_lobby(
        self, interaction: discord.Interaction, lobby
    ) -> tuple[bool, str | None]:
        """
        Auto-join user to lobby if not already in it.

        Returns:
            (joined, message) tuple:
            - joined: True if user was joined, False if already in or couldn't join
            - message: Warning message if roles not set, None otherwise
        """
        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None

        if user_id in lobby.players:
            return False, None

        # Check if player has roles set
        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
        if not player or not player.preferred_roles:
            return False, "⚠️ Set your preferred roles with `/player roles` to auto-join."

        # Attempt to join (pending match check now inside LobbyService)
        success, reason, context = await asyncio.to_thread(
            self.lobby_service.join_lobby,
            user_id,
            guild_id,
            lobby.kind,
        )
        # Cut the one-shot subscription claim at the join's own watermark, so
        # a subscription committed while this join was in flight still fires.
        join_cutoff_ns = getattr(context, "joined_at_ns", None) or time.time_ns()
        if not success:
            logger.info(f"Auto-join failed for {user_id}: {reason}")
            if reason == "rating_too_high":
                return (
                    False,
                    f"ℹ️ You can view {lobby.kind.label}, but only players "
                    f"below {LOWSKILL_CUTOFF_TEXT} Glicko can join.",
                )
            if reason == "in_flight":
                return (
                    False,
                    "ℹ️ You can’t switch lobbies while your current shuffle or draft is in progress.",
                )
            if reason == "lobby_suspended":
                return False, f"❌ {_private_suspension_message(context)}"
            return False, None

        # Start the one-shot claim from the known successful mutation. It runs
        # independently so slow DMs cannot delay or suppress public 8/9 alerts.
        self._schedule_lobby_player_notifications(
            interaction.user,
            guild_id,
            lobby.kind,
            join_cutoff_ns,
        )

        # Refresh lobby state
        lobby = await asyncio.to_thread(
            self.lobby_service.get_lobby,
            guild_id=guild_id,
            lobby_kind=lobby.kind,
        )

        await self._run_lobby_publication_wave(
            [
                (
                    "display sync after auto-join",
                    self._sync_lobby_displays(lobby, guild_id),
                ),
                (
                    "thread publication after auto-join",
                    self._publish_join_activity_and_notifications(
                        lobby,
                        interaction.user,
                        guild_id,
                        auto_join=True,
                    ),
                ),
            ]
        )

        return True, None

    @app_commands.command(name="lobby", description="Create or view a matchmaking lobby")
    @app_commands.describe(lobby="Lobby to create or view")
    @app_commands.choices(
        lobby=[
            app_commands.Choice(name="🍽️ All You Can Feed", value="open"),
            app_commands.Choice(name="🧀 Whine & Cheese", value="lowskill"),
        ]
    )
    @require_guild
    async def lobby(
        self,
        interaction: discord.Interaction,
        lobby: app_commands.Choice[str] | None = None,
    ):
        logger.info(f"Lobby command: User {interaction.user.id} ({interaction.user})")
        if not await safe_defer(interaction, ephemeral=False):
            return

        guild_id = interaction.guild.id
        lobby_kind = LobbyKind.normalize(lobby.value if lobby else None)
        player = await asyncio.to_thread(self.player_service.get_player, interaction.user.id, guild_id)
        if not player:
            await safe_followup(
                interaction, content="❌ You're not registered! Use `/player register` first.", ephemeral=True
            )
            return

        # Acquire per-guild lock to prevent race condition when multiple users
        # call /lobby simultaneously in the same guild.
        async with self.lobby_service.get_creation_lock(
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        ):
            try:
                lobby = await asyncio.to_thread(
                    functools.partial(
                        self.lobby_service.get_or_create_lobby,
                        creator_id=interaction.user.id,
                        guild_id=guild_id,
                        lobby_kind=lobby_kind,
                    )
                )
            except ValueError as exc:
                if str(exc) == "rating_too_high":
                    await safe_followup(
                        interaction,
                        content=(
                            f"❌ Only players below {LOWSKILL_CUTOFF_TEXT} "
                            "Glicko can create Whine & Cheese."
                        ),
                        ephemeral=True,
                    )
                    return
                if str(exc) == "lobby_suspended":
                    await safe_followup(
                        interaction,
                        content=f"❌ {_private_suspension_message(getattr(exc, 'suspension', None))}",
                        ephemeral=True,
                    )
                    return
                raise
            embed = await asyncio.to_thread(self.lobby_service.build_lobby_embed, lobby, guild_id)

            # If message/thread already exists, refresh it; otherwise create new
            message_id = await asyncio.to_thread(
                self.lobby_service.get_lobby_message_id,
                guild_id=guild_id,
                lobby_kind=lobby_kind,
            )
            thread_id = await asyncio.to_thread(
                self.lobby_service.get_lobby_thread_id,
                guild_id=guild_id,
                lobby_kind=lobby_kind,
            )

            if message_id and thread_id:
                try:
                    # Fetch message from the dedicated/lobby channel (not necessarily interaction channel)
                    lobby_channel_id = await asyncio.to_thread(
                        self.lobby_service.get_lobby_channel_id,
                        guild_id=guild_id,
                        lobby_kind=lobby_kind,
                    )
                    if lobby_channel_id:
                        channel = self.bot.get_channel(lobby_channel_id)
                        if not channel:
                            channel = await self.bot.fetch_channel(lobby_channel_id)
                        message = await channel.fetch_message(message_id)
                    else:
                        message = await interaction.channel.fetch_message(message_id)

                    # Auto-join the user if not already in lobby
                    joined, warning = await self._auto_join_lobby(interaction, lobby)

                    # Auto-join already refreshed the channel message, which is
                    # also the thread starter. Keep the explicit refresh only
                    # for callers who were already in the lobby (or could not
                    # join), so /lobby still repairs a stale display without
                    # issuing a duplicate GET + PATCH after a successful join.
                    if not joined:
                        refreshed_lobby = await asyncio.to_thread(
                            self.lobby_service.get_lobby,
                            guild_id=guild_id,
                            lobby_kind=lobby_kind,
                        )
                        await self._update_thread_embed(
                            refreshed_lobby,
                            guild_id=guild_id,
                        )

                    # Build response based on join result
                    if joined:
                        response = (
                            f"✅ Joined {lobby_kind.label}! "
                            f"[View {lobby_kind.display_name}]({message.jump_url})"
                        )
                    elif warning:
                        response = (
                            f"{warning} "
                            f"[View {lobby_kind.label}]({message.jump_url})"
                        )
                    else:
                        response = f"[View {lobby_kind.label}]({message.jump_url})"

                    await safe_followup(interaction, content=response, ephemeral=True)
                    return
                except Exception as e:
                    # Log so a real failure (e.g. permissions, network) doesn't
                    # silently produce a duplicate lobby with orphan channel
                    # artifacts. We still fall through to create a new one.
                    logger.warning(
                        "Existing lobby refresh failed; creating a new lobby: %s", e
                    )

            # Get target channel (dedicated or fallback to interaction channel)
            target_channel = await self._get_lobby_target_channel(
                interaction,
                lobby_kind,
            )
            if not target_channel:
                await safe_followup(
                    interaction, content="❌ Could not find a valid channel to post the lobby.", ephemeral=True
                )
                return

            # Store the origin channel (where /lobby was run) for rally notifications
            origin_channel_id = interaction.channel.id

            # Send channel message with embed
            channel_msg = await target_channel.send(embed=embed)

            # Create thread from message (static name to avoid rate limits)
            try:
                thread = await self._create_lobby_thread_with_decorations(
                    channel_msg,
                    lobby_kind=lobby_kind,
                )

                # Store all IDs (embed is on channel_msg, which is also the thread starter)
                # Also store origin_channel_id for rally notifications
                await asyncio.to_thread(
                    functools.partial(
                        self.lobby_service.set_lobby_message_id,
                        message_id=channel_msg.id,
                        channel_id=target_channel.id,  # Where the embed lives (dedicated or interaction)
                        thread_id=thread.id,
                        embed_message_id=channel_msg.id,  # The channel msg IS the embed in thread
                        origin_channel_id=origin_channel_id,  # Where /lobby was run (for rally)
                        guild_id=guild_id,
                        lobby_kind=lobby_kind,
                    )
                )

                # Auto-join the user who created the lobby
                joined, warning = await self._auto_join_lobby(interaction, lobby)

                # Build response based on join result
                if joined:
                    response = (
                        f"✅ {lobby_kind.label} created and joined! "
                        f"[View]({channel_msg.jump_url})"
                    )
                elif warning:
                    response = (
                        f"✅ {lobby_kind.label} created! {warning} "
                        f"[View]({channel_msg.jump_url})"
                    )
                else:
                    response = (
                        f"✅ {lobby_kind.label} created! "
                        f"[View]({channel_msg.jump_url})"
                    )

                await safe_followup(interaction, content=response, ephemeral=True)
                return

            except discord.Forbidden:
                # Thread permissions required
                logger.warning("Cannot create lobby thread: missing Create Public Threads permission.")
                await channel_msg.delete()
                await safe_followup(
                    interaction, content="❌ Bot needs 'Create Public Threads' permission to create lobbies.",
                    ephemeral=True,
                )
            except Exception as exc:
                logger.exception(f"Error creating lobby thread: {exc}")
                await channel_msg.delete()
                await safe_followup(
                    interaction, content="❌ Failed to create lobby thread. Please try again or contact an admin.",
                    ephemeral=True,
                )

    @app_commands.command(
        name="kick",
        description="Kick a player from every lobby they're in (Admin or lobby creator only)",
    )
    @app_commands.describe(
        player="The player to kick from the lobby",
        reason="Optional reason shown privately to the player and admins",
    )
    @require_guild
    async def kick(
        self,
        interaction: discord.Interaction,
        player: discord.Member,
        reason: app_commands.Range[str, 3, 300] | None = None,
    ):
        logger.info(f"Kick command: User {interaction.user.id} kicking {player.id}")
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        target_kinds = await asyncio.to_thread(
            self.lobby_service.get_lobby_kinds_for_player,
            player.id,
            guild_id,
        )
        lobbies: dict[LobbyKind, object] = {}
        for kind in target_kinds:
            lobby = await asyncio.to_thread(
                self.lobby_service.get_lobby,
                guild_id=guild_id,
                lobby_kind=kind,
            )
            if lobby is not None:
                lobbies[kind] = lobby
        if not lobbies:
            await safe_followup(
                interaction,
                content=f"⚠️ {player.mention} is not in a lobby.",
                ephemeral=True,
            )
            return

        is_admin = has_admin_permission(interaction)
        # Lobby ownership is not durable moderation authority, and it does not
        # reach across queues: a creator can only kick out of the lobby they
        # opened and are still sitting in. Admins reach every lobby.
        if is_admin:
            kickable = list(lobbies)
        else:
            kickable = [
                kind
                for kind, lobby in lobbies.items()
                if lobby.created_by == interaction.user.id
                and interaction.user.id in lobby.players
            ]
        if not kickable:
            await safe_followup(
                interaction, content="❌ Permission denied. Admin or lobby creator only.",
                ephemeral=True,
            )
            return

        if player.id == interaction.user.id:
            await safe_followup(
                interaction, content="❌ You can't kick yourself. Use the Leave button in the lobby thread.",
                ephemeral=True,
            )
            return

        target_name = getattr(player, "display_name", player.mention)
        actor_name = getattr(
            interaction.user,
            "display_name",
            getattr(interaction.user, "mention", str(interaction.user.id)),
        )
        public_message = f"👢 **{target_name}** was kicked by {actor_name}."

        kicked_kinds: list[LobbyKind] = []
        for kind in kickable:
            if await self.eject_lobby_player(
                player,
                guild_id=guild_id,
                lobby_kind=kind,
                public_message=public_message,
                # One DM covers the whole kick, sent below once the set of
                # lobbies they actually lost is known.
                dm_message=None,
            ):
                kicked_kinds.append(kind)

        if kicked_kinds:
            dm_message = (
                f"You were kicked from {format_lobby_labels(kicked_kinds)} "
                f"by {interaction.user.mention}."
            )
            if reason:
                dm_message += f"\nReason: {reason}"
            try:
                await player.send(dm_message)
            except Exception as exc:
                logger.debug("Failed to DM kicked player: %s", exc)

            await safe_followup(
                interaction,
                content=(
                    f"✅ Kicked {player.mention} from "
                    f"{format_lobby_labels(kicked_kinds)}."
                ),
                ephemeral=True,
            )

            moderation_service = getattr(self.bot, "moderation_service", None)
            if moderation_service is not None:
                # The audit trail is per lobby kind — record_kick rejects an
                # all-lobbies scope — so a two-lobby kick logs two entries.
                for kind in kicked_kinds:
                    try:
                        await asyncio.to_thread(
                            moderation_service.record_kick,
                            player.id,
                            guild_id,
                            actor_id=interaction.user.id,
                            lobby_kind=kind.value,
                            reason=reason,
                            source="admin" if is_admin else "creator",
                        )
                    except Exception as exc:
                        logger.warning("Failed to audit lobby kick: %s", exc)
        else:
            await safe_followup(interaction, content=f"❌ Failed to kick {player.mention}.", ephemeral=True)

    async def eject_lobby_player(
        self,
        player: discord.User | discord.Member,
        *,
        guild_id: int,
        lobby_kind: LobbyKind | str,
        public_message: str,
        dm_message: str | None,
    ) -> bool:
        """Remove one non-reserved player and run shared Discord cleanup.

        Persistent moderation state must be written before calling this helper.
        A False result means the player was no longer in the lobby or had
        already been reserved by a shuffle/draft; in-flight matches are never
        disrupted by a newly issued suspension.
        """
        kind = LobbyKind.normalize(lobby_kind)
        removed = await asyncio.to_thread(
            self.lobby_service.leave_lobby,
            player.id,
            guild_id,
            kind,
        )
        if not removed:
            return False

        lobby = await asyncio.to_thread(
            self.lobby_service.get_lobby,
            guild_id=guild_id,
            lobby_kind=kind,
        )
        if lobby is not None:
            await self._sync_lobby_displays(lobby, guild_id)
        await self._remove_user_lobby_reactions(
            player,
            guild_id=guild_id,
            lobby_kind=kind,
        )

        thread_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_thread_id,
            guild_id=guild_id,
            lobby_kind=kind,
        )
        if thread_id:
            try:
                thread = self.bot.get_channel(thread_id)
                if not thread:
                    thread = await self.bot.fetch_channel(thread_id)
                await thread.send(public_message)
            except Exception as exc:
                logger.warning("Failed to post lobby ejection activity: %s", exc)

        if dm_message:
            try:
                await player.send(dm_message)
            except Exception as exc:
                logger.debug("Failed to DM ejected player: %s", exc)
        return True

    async def evict_matched_players_from_other_lobbies(
        self,
        player_ids: list[int] | set[int],
        *,
        guild_id: int | None,
        popped_kind: LobbyKind | str,
    ) -> dict[LobbyKind, set[int]]:
        """Pull a freshly matched roster out of every lobby it did not play in.

        Queueing in both lobbies at once is allowed, so a shuffle or draft that
        commits players to a match has to release the seats they are still
        holding elsewhere. Returns the ids actually removed, per lobby kind.

        Call this while the popped lobby still holds its reservation on these
        players: that is what makes the eviction race-free. Until the other
        lobby loses them from its roster, ``_can_reserve_locked`` refuses to
        start a second shuffle over the same people, so nobody can be popped
        into two matches at once.

        Membership is mutated first and the Discord upkeep is best-effort — a
        failed embed edit must never unwind a match that has already started.
        """
        kind = LobbyKind.normalize(popped_kind)
        ids = set(player_ids)
        evicted: dict[LobbyKind, set[int]] = {}
        if not ids:
            return evicted

        for other in LobbyKind:
            if other is kind:
                continue
            removed = await asyncio.to_thread(
                self.lobby_service.remove_players_from_lobby,
                ids,
                guild_id,
                other,
            )
            if not removed:
                continue
            evicted[other] = removed
            try:
                await self._publish_cross_lobby_eviction(
                    removed,
                    guild_id=guild_id,
                    other_kind=other,
                    popped_kind=kind,
                )
            except Exception as exc:
                logger.warning(
                    "Failed to publish %s eviction after the %s match started: %s",
                    other.value,
                    kind.value,
                    exc,
                )
        return evicted

    async def _rearm_readycheck_notice(
        self,
        guild_id: int | None,
        lobby_kind: LobbyKind,
    ) -> None:
        """Let the completion notice fire again if the drain broke the quorum.

        The notice is memoised once per ready-check generation. Losing players
        to the other lobby's match can drop a lobby back under the threshold,
        and it should be able to announce again when it refills — but only
        then. Clearing the memo unconditionally would let the very next resync
        re-send the notice for a lobby that never stopped being ready.
        """
        confirmed = await asyncio.to_thread(
            self.lobby_service.get_readycheck_reacted,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )
        if len(confirmed) >= self.lobby_service.ready_threshold:
            return
        key = (guild_id, lobby_kind)
        # Scoped to the pop alone: notify_readycheck_complete writes this dict
        # under the same lock, and it is not reentrant, so holding it across
        # the display resync would deadlock.
        async with self._readycheck_shuffle_notification_locks.setdefault(
            key,
            asyncio.Lock(),
        ):
            self._readycheck_shuffle_notified.pop(key, None)

    async def _publish_cross_lobby_eviction(
        self,
        removed_ids: set[int],
        *,
        guild_id: int | None,
        other_kind: LobbyKind,
        popped_kind: LobbyKind,
    ) -> None:
        """Repaint the drained lobby and tell its thread who left."""
        lobby = await asyncio.to_thread(
            self.lobby_service.get_lobby,
            guild_id=guild_id,
            lobby_kind=other_kind,
        )
        if lobby is not None:
            # Before the memo below, so this resync cannot itself re-announce.
            await self._sync_lobby_displays(lobby, guild_id)
        await self._rearm_readycheck_notice(guild_id, other_kind)

        for player_id in sorted(removed_ids):
            await self._remove_user_lobby_reactions(
                discord.Object(id=player_id),
                guild_id=guild_id,
                lobby_kind=other_kind,
            )

        thread_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_thread_id,
            guild_id=guild_id,
            lobby_kind=other_kind,
        )
        if not thread_id:
            return
        mentions = ", ".join(f"<@{player_id}>" for player_id in sorted(removed_ids))
        try:
            thread = self.bot.get_channel(thread_id)
            if not thread:
                thread = await self.bot.fetch_channel(thread_id)
            await thread.send(
                f"🔀 {mentions} left for the {popped_kind.label} match.",
                allowed_mentions=discord.AllowedMentions.none(),
            )
        except Exception as exc:
            logger.warning("Failed to post cross-lobby eviction activity: %s", exc)

    @app_commands.command(name="join", description="Join a matchmaking lobby")
    @app_commands.describe(lobby="Lobby to join")
    @app_commands.choices(
        lobby=[
            app_commands.Choice(name="🍽️ All You Can Feed", value="open"),
            app_commands.Choice(name="🧀 Whine & Cheese", value="lowskill"),
        ]
    )
    @require_guild
    async def join(
        self,
        interaction: discord.Interaction,
        lobby: app_commands.Choice[str] | None = None,
    ):
        """Join the matchmaking lobby from any channel."""
        logger.info(f"Join command: User {interaction.user.id} ({interaction.user})")
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        lobby_kind = LobbyKind.normalize(lobby.value if lobby else None)

        # Check registration
        player = await asyncio.to_thread(self.player_service.get_player, interaction.user.id, guild_id)
        if not player:
            await safe_followup(
                interaction, content="❌ You're not registered! Use `/player register` first.", ephemeral=True
            )
            return

        # Check roles set
        if not player.preferred_roles:
            await safe_followup(
                interaction, content="❌ Set your preferred roles first! Use `/player roles`.", ephemeral=True
            )
            return

        # Check curfew
        if is_within_curfew(player):
            await safe_followup(
                interaction,
                content=(
                    f"❌ It's past your bedtime ({format_curfew_window(player)}). "
                    "Queueing is locked until your curfew ends. Use `/player curfew off` to disable it."
                ),
                ephemeral=True,
            )
            return

        # Check lobby exists
        active_lobby = await asyncio.to_thread(
            self.lobby_service.get_lobby,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )
        if not active_lobby:
            await safe_followup(
                interaction,
                content=(
                    f"⚠️ No active {lobby_kind.label} lobby. "
                    "Use `/lobby` to create one."
                ),
                ephemeral=True,
            )
            return

        # Attempt to join (pending match check now inside LobbyService)
        success, reason, pending_info = await asyncio.to_thread(
            self.lobby_service.join_lobby,
            interaction.user.id,
            guild_id,
            lobby_kind,
        )
        if not success:
            if reason == "in_pending_match" and pending_info:
                pending_match_id = pending_info.pending_match_id
                jump_url = pending_info.shuffle_message_jump_url
                message_text = f"❌ You're already in a pending match (Match #{pending_match_id})!"
                if jump_url:
                    message_text += f" [View your match]({jump_url}) and use `/record` to complete it first."
                else:
                    message_text += " Use `/record` to complete it first."
                await safe_followup(interaction, content=message_text, ephemeral=True)
            elif reason == "lobby_full":
                await safe_followup(
                    interaction,
                    content=f"❌ {lobby_kind.label} is full.",
                    ephemeral=True,
                )
            elif reason == "rating_too_high":
                await safe_followup(
                    interaction,
                    content=(
                        f"❌ {LobbyKind.LOWSKILL.label} is limited to players "
                        f"below {LOWSKILL_CUTOFF_TEXT} Glicko."
                    ),
                    ephemeral=True,
                )
            elif reason == "in_flight":
                await safe_followup(
                    interaction,
                    content=(
                        "❌ You can’t switch lobbies while your current shuffle "
                        "or draft is in progress."
                    ),
                    ephemeral=True,
                )
            elif reason == "lobby_suspended":
                await safe_followup(
                    interaction,
                    content=f"❌ {_private_suspension_message(pending_info)}",
                    ephemeral=True,
                )
            else:
                await safe_followup(
                    interaction,
                    content=(
                        f"❌ Already in {lobby_kind.label}, or that lobby is closed."
                    ),
                    ephemeral=True,
                )
            return

        # Cut the one-shot subscription claim at the join's own watermark, so
        # a subscription committed while this join was in flight still fires.
        join_cutoff_ns = getattr(pending_info, "joined_at_ns", None) or time.time_ns()
        # Tie the one-shot notification to the successful join result rather
        # than the best-effort lobby refresh/publication that follows.
        self._schedule_lobby_player_notifications(
            interaction.user,
            guild_id,
            lobby_kind,
            join_cutoff_ns,
        )

        # Refresh lobby state after join
        active_lobby = await asyncio.to_thread(
            self.lobby_service.get_lobby,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )

        await self._run_lobby_publication_wave(
            [
                (
                    "display sync after join",
                    self._sync_lobby_displays(active_lobby, guild_id),
                ),
                (
                    "thread publication after join",
                    self._publish_join_activity_and_notifications(
                        active_lobby, interaction.user, guild_id
                    ),
                ),
            ],
            followup=safe_followup(
                interaction,
                content=f"✅ Joined {lobby_kind.label}!",
                ephemeral=True,
            ),
        )

        # Neon Degen Terminal hook for lobby join
        try:
            neon = get_neon_service(self.bot)
            if neon and active_lobby:
                queue_position = len(active_lobby.players)
                neon_result = await neon.on_lobby_join(
                    interaction.user.id, guild_id, queue_position
                )
                if neon_result and (neon_result.text_block or neon_result.footer_text):
                    channel_id = await asyncio.to_thread(
                        self.lobby_service.get_lobby_channel_id,
                        guild_id=guild_id,
                        lobby_kind=lobby_kind,
                    )
                    if channel_id:
                        channel = self.bot.get_channel(channel_id)
                        if channel:
                            text = neon_result.text_block or neon_result.footer_text
                            await channel.send(text)
        except Exception as e:
            logger.debug(f"Neon lobby join hook error: {e}")

    @app_commands.command(
        name="leave",
        description="Leave every matchmaking lobby you're queued in",
    )
    @require_guild
    async def leave(self, interaction: discord.Interaction):
        """Leave every lobby the caller is queued in, from any channel.

        Deliberately takes no lobby argument: leaving is never ambiguous
        because leaving everything is always what the caller meant.
        """
        logger.info(f"Leave command: User {interaction.user.id} ({interaction.user})")
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        member_kinds = await asyncio.to_thread(
            self.lobby_service.get_lobby_kinds_for_player,
            interaction.user.id,
            guild_id,
        )
        if not member_kinds:
            await safe_followup(
                interaction,
                content="⚠️ You're not in a lobby.",
                ephemeral=True,
            )
            return

        left_kinds: list[LobbyKind] = []
        pinned_kinds: list[LobbyKind] = []
        for kind in member_kinds:
            if await asyncio.to_thread(
                self.lobby_service.leave_lobby,
                interaction.user.id,
                guild_id,
                kind,
            ):
                left_kinds.append(kind)
            elif (
                await asyncio.to_thread(
                    self.lobby_service.get_in_flight_lobby_kind_for_player,
                    interaction.user.id,
                    guild_id,
                )
                is kind
            ):
                pinned_kinds.append(kind)
            # Otherwise the lobby closed under us and there is nothing to leave.

        if not left_kinds:
            await safe_followup(
                interaction,
                content=(
                    f"❌ You can’t leave {format_lobby_labels(pinned_kinds)} "
                    "while its shuffle or draft is in progress."
                    if pinned_kinds
                    else "⚠️ You’re no longer in the lobby."
                ),
                ephemeral=True,
            )
            return

        maintenance: list[tuple[str, Awaitable[object]]] = []
        for kind in left_kinds:
            # Re-fetch each lobby after removal so its embed reflects the
            # current roster.
            lobby = await asyncio.to_thread(
                self.lobby_service.get_lobby,
                guild_id=guild_id,
                lobby_kind=kind,
            )
            if lobby is not None:
                maintenance.append(
                    (
                        f"display sync after leave ({kind.value})",
                        self._sync_lobby_displays(lobby, guild_id),
                    )
                )
            maintenance.extend(
                [
                    (
                        f"reaction cleanup after leave ({kind.value})",
                        self._remove_user_lobby_reactions(
                            interaction.user,
                            guild_id=guild_id,
                            lobby_kind=kind,
                        ),
                    ),
                    (
                        f"thread publication after leave ({kind.value})",
                        self._publish_leave_activity(
                            interaction.user,
                            guild_id,
                            kind,
                        ),
                    ),
                ]
            )

        content = f"✅ Left {format_lobby_labels(left_kinds)}."
        if pinned_kinds:
            content += (
                f"\n⚠️ Still in {format_lobby_labels(pinned_kinds)} — its "
                "shuffle or draft is in progress."
            )
        await self._run_lobby_publication_wave(
            maintenance,
            followup=safe_followup(
                interaction,
                content=content,
                ephemeral=True,
            ),
        )

    @app_commands.command(
        name="resetlobby",
        description="Reset the current lobby (Admin or lobby creator only)",
    )
    @app_commands.describe(lobby="Lobby to reset (defaults to the lobby you're in)")
    @app_commands.choices(
        lobby=[
            app_commands.Choice(name="🍽️ All You Can Feed", value="open"),
            app_commands.Choice(name="🧀 Whine & Cheese", value="lowskill"),
        ]
    )
    @require_guild
    async def resetlobby(
        self,
        interaction: discord.Interaction,
        lobby: app_commands.Choice[str] | None = None,
    ):
        """Allow admins or lobby creators to reset/abort an unfilled lobby."""
        logger.info(f"Reset lobby command: User {interaction.user.id} ({interaction.user})")
        can_respond = await safe_defer(interaction, ephemeral=True)

        guild_id = interaction.guild.id
        if lobby is not None:
            # An explicit choice lets admins and departed creators reset a
            # lobby they are not currently a member of; the permission check
            # below runs against the resolved lobby either way.
            lobby_kind = LobbyKind.normalize(lobby.value)
        else:
            member_kinds = await asyncio.to_thread(
                self.lobby_service.get_lobby_kinds_for_player,
                interaction.user.id,
                guild_id,
            )
            lobby_kind, selection_error = resolve_lobby_kind(
                member_kinds,
                None,
                no_lobby_message=(
                    "❌ Join a lobby, or pass the `lobby` option to "
                    "pick which lobby to reset."
                ),
            )
            if selection_error is not None:
                if can_respond:
                    await safe_followup(
                        interaction,
                        content=selection_error,
                        ephemeral=True,
                    )
                return

        match_service = getattr(self.bot, "match_service", None)
        if match_service:
            # get_last_shuffle returns None when *multiple* pending matches
            # exist, not just when there are none, so asking it here let the
            # guard fall open in exactly the concurrent-match case it protects.
            pending_matches = await asyncio.to_thread(
                match_service.state_service.get_all_pending_matches, guild_id
            )
            relevant_pending_matches = [
                pending
                for pending in pending_matches
                if pending.lobby_kind is None
                or LobbyKind.normalize(pending.lobby_kind) is lobby_kind
            ]
            if relevant_pending_matches:
                pending_match = relevant_pending_matches[0]
                if can_respond:
                    jump_url = pending_match.shuffle_message_jump_url
                    message_text = "❌ There's a pending match that needs to be recorded!"
                    if jump_url:
                        message_text += (
                            f" [View pending match]({jump_url}) then use `/record` first."
                        )
                    else:
                        message_text += " Use `/record` first."
                    await safe_followup(interaction, content=message_text, ephemeral=True)
                return

        lobby = await asyncio.to_thread(
            self.lobby_service.get_lobby,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )
        if not lobby:
            if can_respond:
                await safe_followup(
                    interaction,
                    content=f"⚠️ No active {lobby_kind.label} lobby.",
                    ephemeral=True,
                )
            return

        is_admin = has_admin_permission(interaction)
        is_creator = lobby.created_by == interaction.user.id
        if not (is_admin or is_creator):
            if can_respond:
                await safe_followup(
                    interaction, content="❌ Permission denied. Admin or lobby creator only.",
                    ephemeral=True,
                )
            return

        # Block if there's an active draft
        draft_state_manager = getattr(self.bot, "draft_state_manager", None)
        active_draft = (
            await asyncio.to_thread(draft_state_manager.get_state, guild_id)
            if draft_state_manager
            else None
        )
        if active_draft and LobbyKind.normalize(active_draft.lobby_kind) is lobby_kind:
            if can_respond:
                await safe_followup(
                    interaction, content="❌ There's an active draft in progress. "
                    "Use `/draft restart` first to clear the draft.",
                    ephemeral=True,
                )
            return

        has_in_flight_players = await asyncio.to_thread(
            self.lobby_service.has_in_flight_lobby_players,
            guild_id,
            lobby_kind,
        )
        if has_in_flight_players:
            if can_respond:
                await safe_followup(
                    interaction,
                    content=(
                        "❌ This lobby can’t be reset while its shuffle or draft "
                        "is starting. Please try again in a moment."
                    ),
                    ephemeral=True,
                )
            return

        await self._reset_lobby_serialized(
            interaction,
            can_respond=can_respond,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )

    async def _reset_lobby_serialized(
        self,
        interaction: discord.Interaction,
        *,
        can_respond: bool,
        guild_id: int,
        lobby_kind: LobbyKind,
    ) -> None:
        """Finish a reset while excluding shuffle/draft startup for this lobby."""
        lobby_manager = self.lobby_service.lobby_manager
        await asyncio.to_thread(
            lobby_manager._check_stale_lock,
            guild_id,
            lobby_kind,
        )
        shuffle_lock = await asyncio.to_thread(
            lobby_manager.get_shuffle_lock,
            guild_id,
            lobby_kind,
        )
        try:
            await asyncio.wait_for(shuffle_lock.acquire(), timeout=0.5)
        except TimeoutError:
            if can_respond:
                await safe_followup(
                    interaction,
                    content=(
                        f"❌ {lobby_kind.label} is currently starting a shuffle or "
                        "draft. Please try again in a moment."
                    ),
                    ephemeral=True,
                )
            return

        await asyncio.to_thread(
            lobby_manager.record_lock_acquired,
            guild_id,
            lobby_kind,
        )
        try:
            # A match or draft may have started between the command's first
            # validation and acquiring the shared operation lock.
            match_service = getattr(self.bot, "match_service", None)
            if match_service:
                pending_matches = await asyncio.to_thread(
                    match_service.state_service.get_all_pending_matches,
                    guild_id,
                )
                relevant_pending = [
                    pending
                    for pending in pending_matches
                    if pending.lobby_kind is None
                    or LobbyKind.normalize(pending.lobby_kind) is lobby_kind
                ]
                if relevant_pending:
                    if can_respond:
                        pending_match = relevant_pending[0]
                        message_text = "❌ There's a pending match that needs to be recorded!"
                        if pending_match.shuffle_message_jump_url:
                            message_text += (
                                f" [View pending match]({pending_match.shuffle_message_jump_url}) "
                                "then use `/record` first."
                            )
                        else:
                            message_text += " Use `/record` first."
                        await safe_followup(
                            interaction,
                            content=message_text,
                            ephemeral=True,
                        )
                    return

            draft_state_manager = getattr(self.bot, "draft_state_manager", None)
            active_draft = (
                await asyncio.to_thread(draft_state_manager.get_state, guild_id)
                if draft_state_manager
                else None
            )
            if (
                active_draft
                and LobbyKind.normalize(active_draft.lobby_kind) is lobby_kind
            ):
                if can_respond:
                    await safe_followup(
                        interaction,
                        content=(
                            "❌ There's an active draft in progress. "
                            "Use `/draft restart` first to clear the draft."
                        ),
                        ephemeral=True,
                    )
                return

            if await asyncio.to_thread(
                self.lobby_service.has_in_flight_lobby_players,
                guild_id,
                lobby_kind,
            ):
                if can_respond:
                    await safe_followup(
                        interaction,
                        content=(
                            "❌ This lobby can’t be reset while its shuffle or draft "
                            "is starting. Please try again in a moment."
                        ),
                        ephemeral=True,
                    )
                return

            await self._finish_lobby_reset(
                interaction,
                can_respond=can_respond,
                guild_id=guild_id,
                lobby_kind=lobby_kind,
            )
        finally:
            await asyncio.to_thread(
                lobby_manager.clear_lock_time,
                guild_id,
                lobby_kind,
            )
            shuffle_lock.release()

    async def _finish_lobby_reset(
        self,
        interaction: discord.Interaction,
        *,
        can_respond: bool,
        guild_id: int,
        lobby_kind: LobbyKind,
    ) -> None:
        """Update Discord surfaces and clear a validated lobby."""
        # Update channel message to show closed and archive thread
        await self._update_channel_message_closed(
            "Lobby Reset",
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )
        await self._archive_lobby_thread(
            "Lobby Reset",
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )

        # Unpin from the lobby channel (may be dedicated channel, not interaction channel)
        lobby_channel_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_channel_id,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )
        lobby_message_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_message_id,
            guild_id=guild_id,
            lobby_kind=lobby_kind,
        )
        lobby_channel = None
        if lobby_channel_id:
            try:
                lobby_channel = self.bot.get_channel(lobby_channel_id)
                if not lobby_channel:
                    lobby_channel = await self.bot.fetch_channel(lobby_channel_id)
            except Exception:
                lobby_channel = interaction.channel
        else:
            lobby_channel = interaction.channel
        await safe_unpin_message(lobby_channel, lobby_message_id)
        await asyncio.to_thread(
            self.lobby_service.reset_lobby,
            guild_id,
            lobby_kind,
        )

        # Clear lobby rally cooldowns
        from bot import clear_lobby_rally_cooldowns
        clear_lobby_rally_cooldowns(guild_id, lobby_kind=lobby_kind)

        logger.info(f"Lobby reset by user {interaction.user.id}")
        if can_respond:
            await safe_followup(
                interaction,
                content=(
                    f"✅ {lobby_kind.label} reset. "
                    "You can create a new lobby with `/lobby`."
                ),
                ephemeral=True,
            )


    @app_commands.command(
        name="readycheck",
        description="Check lobby players' online status and ping those who are away",
    )
    @app_commands.describe(lobby="Lobby to check (defaults to the lobby you're in)")
    @app_commands.choices(
        lobby=[
            app_commands.Choice(name="🍽️ All You Can Feed", value="open"),
            app_commands.Choice(name="🧀 Whine & Cheese", value="lowskill"),
        ]
    )
    @require_guild
    async def readycheck(
        self,
        interaction: discord.Interaction,
        lobby: app_commands.Choice[str] | None = None,
    ):
        logger.info(f"Readycheck command: User {interaction.user.id} ({interaction.user})")
        if not await safe_defer(interaction, ephemeral=False):
            return

        guild = interaction.guild
        guild_id = guild.id
        member_kinds = await asyncio.to_thread(
            self.lobby_service.get_lobby_kinds_for_player,
            interaction.user.id,
            guild_id,
        )
        lobby_kind, selection_error = resolve_lobby_kind(
            member_kinds,
            lobby,
            no_lobby_message="❌ Join a lobby before running a ready check.",
        )
        if selection_error is not None:
            await safe_followup(
                interaction,
                content=selection_error,
                ephemeral=True,
            )
            return
        status, info = await self._execute_readycheck(
            guild,
            guild_id,
            interaction.user.id,
            lobby_kind=lobby_kind,
        )

        if status == "no_lobby":
            await safe_followup(
                interaction,
                content=f"⚠️ No active {lobby_kind.label} lobby.",
                ephemeral=True,
            )
        elif status == "no_guild":
            await safe_followup(interaction, content="❌ This command must be used in a server.", ephemeral=True)
        elif status == "not_member":
            await safe_followup(
                interaction,
                content=f"❌ Join {lobby_kind.label} before running its ready check.",
                ephemeral=True,
            )
        elif status == "cooldown":
            await safe_followup(
                interaction,
                content=(
                    f"⏳ {lobby_kind.label} ready check is on cooldown. "
                    f"Try again in {info['retry_after_seconds']}s."
                ),
                ephemeral=True,
            )
        elif status == "no_thread":
            await safe_followup(
                interaction,
                content=(
                    f"❌ No {lobby_kind.label} thread found. "
                    "Create a lobby with `/lobby` first."
                ),
                ephemeral=True,
            )
        elif status == "ok":
            verb = "refreshed" if info.get("is_refresh") else "posted"
            mention_ids = info.get("mention_ids", [])
            invocation_channel_id = getattr(interaction.channel, "id", None)
            if (
                mention_ids
                and invocation_channel_id != info.get("readycheck_channel_id")
            ):
                tags = " ".join(f"<@{user_id}>" for user_id in mention_ids)
                await safe_followup(
                    interaction,
                    content=(
                        f"⚔️ **{lobby_kind.label} ready check!** {tags}\n"
                        f"[React in the lobby thread]({info['message_jump_url']})"
                    ),
                    allowed_mentions=discord.AllowedMentions(
                        users=[discord.Object(id=user_id) for user_id in mention_ids]
                    ),
                )
            else:
                await safe_followup(
                    interaction,
                    content=(
                        f"✅ {lobby_kind.label} ready check {verb}! "
                        f"[View]({info['message_jump_url']})"
                    ),
                    ephemeral=True,
                )
        else:  # "error"
            await safe_followup(
                interaction,
                content="❌ Ready check failed — make sure a lobby exists, then try again.",
                ephemeral=True,
            )

    async def _execute_readycheck(
        self,
        guild: discord.Guild | None,
        guild_id: int | None,
        invoker_id: int,
        lobby_kind: LobbyKind | str | None = None,
        confirm_invoker: bool = True,
    ) -> tuple[str, dict]:
        """Run the readycheck flow. Returns (status, info).

        Shared by /readycheck and the 🔔 lobby-embed reaction shortcut so the
        cooldown is genuinely a single per-guild bucket. Does not touch any
        Discord interaction object — callers translate the status into the
        appropriate user-facing feedback (ephemeral followup or reaction
        removal). ``invoker_id`` is the user who triggered the check; they are
        never pruned and, when ``confirm_invoker`` is True (the explicit
        /readycheck command — a fresh readiness signal), auto-counted as
        ready even over their own earlier withdrawal. The 🔔 shortcut passes
        False: its intent is to re-ping others, not to declare readiness.

        status: one of "ok" | "no_lobby" | "not_member" | "no_thread"
                | "cooldown" | "no_guild" | "error"
        info contents:
            ok        -> {"message_jump_url": str, "is_refresh": bool,
                          "pruned_count": int, "mention_ids": list[int],
                          "readycheck_channel_id": int,
                          "readycheck_message_id": int}
            cooldown  -> {"retry_after_seconds": int}
            (others)  -> {}
        """
        kind = LobbyKind.normalize(lobby_kind)
        lobby = await asyncio.to_thread(
            self.lobby_service.get_lobby,
            guild_id=guild_id,
            lobby_kind=kind,
        )
        if not lobby:
            return "no_lobby", {}

        if invoker_id not in lobby.players:
            return "not_member", {}

        if not guild:
            return "no_guild", {}

        # Global shared rate limit (1 per 120s per guild) — checked after
        # preconditions so failed attempts don't consume the cooldown
        rl = GLOBAL_RATE_LIMITER.check(
            scope=f"readycheck:{kind.value}",
            guild_id=guild_id or 0,
            user_id=0,
            limit=1,
            per_seconds=120,
        )
        if not rl.allowed:
            return "cooldown", {"retry_after_seconds": rl.retry_after_seconds}

        all_player_ids = list(lobby.players)
        current_lobby_set = set(all_player_ids)

        # Classify every player — store structured data for later rebuilds
        now = time.time()
        player_data = await self._classify_readycheck_players(
            guild,
            lobby,
            guild_id,
        )

        # Check if refreshing an existing readycheck
        existing_msg_id = await asyncio.to_thread(
            self.lobby_service.get_readycheck_message_id,
            guild_id=guild_id,
            lobby_kind=kind,
        )
        existing_channel_id = await asyncio.to_thread(
            self.lobby_service.get_readycheck_channel_id,
            guild_id=guild_id,
            lobby_kind=kind,
        )
        is_refresh = False
        msg = None

        if existing_msg_id and existing_channel_id:
            try:
                ch = self.bot.get_channel(existing_channel_id)
                if not ch:
                    ch = await self.bot.fetch_channel(existing_channel_id)
                msg = await ch.fetch_message(existing_msg_id)
                is_refresh = True
            except (discord.NotFound, discord.HTTPException):
                msg = None

        # If the existing check is stale (30+ min old), delete the buried message
        # and start fresh: prune currently AFK players who did not confirm the
        # stale check, then post a new message with confirmations reset. Keep
        # the trigger-er and anyone who reacted to the check being swept.
        pruned_ids: list[int] = []
        if is_refresh and msg is not None:
            created_at = await asyncio.to_thread(
                self.lobby_service.get_readycheck_created_at,
                guild_id=guild_id,
                lobby_kind=kind,
            )
            if created_at is not None and (now - created_at) > READYCHECK_STALE_THRESHOLD:
                old_reacted = await asyncio.to_thread(
                    self.lobby_service.get_readycheck_reacted,
                    guild_id=guild_id,
                    lobby_kind=kind,
                )
                pruned_ids = [
                    pid
                    for pid in list(current_lobby_set)
                    if player_data.get(pid, {}).get("group") == "afk"
                    # Classification can say AFK when an invisible member is
                    # reported offline or when Discord member lookup fails.
                    # Enforce the recent-join grace period again at the actual
                    # destructive boundary so those signals cannot eject a
                    # player who only just joined.
                    and (
                        lobby.player_join_times.get(pid) is None
                        or (now - lobby.player_join_times[pid])
                        >= RECENT_JOIN_THRESHOLD
                    )
                    and pid not in old_reacted
                    and pid != invoker_id
                ]
                removed_ids: list[int] = []
                for pid in pruned_ids:
                    removed = await asyncio.to_thread(
                        self.lobby_service.leave_lobby,
                        pid,
                        guild_id,
                        kind,
                    )
                    if not removed:
                        # Reserved by an in-flight shuffle/draft (or already
                        # gone): keep their state instead of announcing a
                        # removal that did not happen.
                        continue
                    removed_ids.append(pid)
                    player_data.pop(pid, None)
                    current_lobby_set.discard(pid)
                    await self._remove_user_lobby_reactions(
                        discord.Object(id=pid),
                        guild_id=guild_id,
                        lobby_kind=kind,
                    )
                pruned_ids = removed_ids
                if pruned_ids:
                    logger.info(
                        "Stale readycheck prune: removed %d AFK player(s) %s from guild %s",
                        len(pruned_ids),
                        pruned_ids,
                        guild_id,
                    )
                if pruned_ids:
                    lobby = await asyncio.to_thread(
                        self.lobby_service.get_lobby,
                        guild_id=guild_id,
                        lobby_kind=kind,
                    )
                    await self._sync_lobby_displays(
                        lobby,
                        guild_id,
                        sync_readycheck=False,
                    )
                try:
                    await msg.delete()
                except (discord.NotFound, discord.HTTPException) as e:
                    logger.debug("Failed to delete stale readycheck message: %s", e)
                msg = None
                is_refresh = False

        if is_refresh and msg:
            refresh_lock = self._readycheck_refresh_locks.setdefault(
                (guild_id or 0, kind),
                asyncio.Lock(),
            )
            async with refresh_lock:
                state_updated, display_updated, mention_ids = (
                    await self._apply_readycheck_lobby_update(
                        msg,
                        player_data,
                        guild_id,
                        auto_confirm_ids=(
                            {invoker_id} if confirm_invoker else set()
                        ),
                        lobby_kind=kind,
                    )
                )
            if state_updated:
                await self.notify_readycheck_completion_if_ready(
                    guild_id or 0,
                    msg.id,
                    fallback_channel=msg.channel,
                    lobby_kind=kind,
                )
            if not display_updated:
                return "error", {}
            embed = None
        else:
            auto_confirm_ids = _get_recent_signup_ids(player_data)
            # Running the check counts the trigger-er as ready (if they're in
            # the lobby). Reflected in the new embed now and persisted after
            # the message is saved below.
            if invoker_id in current_lobby_set:
                auto_confirm_ids.add(invoker_id)
            reacted = {
                discord_id: f"<@{discord_id}>"
                for discord_id in auto_confirm_ids & current_lobby_set
            }

            embed, mention_ids = build_readycheck_embed(
                player_data,
                reacted,
                ready_threshold=self.lobby_service.ready_threshold,
                lobby_kind=kind,
            )

        # Resolve target channel - lobby thread only
        target_channel = None
        lobby_thread_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_thread_id,
            guild_id=guild_id,
            lobby_kind=kind,
        )
        if lobby_thread_id:
            try:
                target_channel = self.bot.get_channel(lobby_thread_id)
                if not target_channel:
                    target_channel = await self.bot.fetch_channel(lobby_thread_id)
            except Exception as e:
                logger.debug("Failed to fetch lobby thread channel: %s", e)

        if not target_channel:
            return "no_thread", {}

        # Ping all lobby members (exclude those who already reacted)
        allowed_mentions = discord.AllowedMentions(
            users=[discord.Object(id=uid) for uid in mention_ids]
        )
        ping_content = None
        if mention_ids:
            tags = " ".join(f"<@{uid}>" for uid in mention_ids)
            ping_content = f"⚔️ **{kind.label} ready check!** {tags}"

        if is_refresh and msg:
            if ping_content:
                await msg.channel.send(ping_content, allowed_mentions=allowed_mentions)
            return "ok", {
                "message_jump_url": msg.jump_url,
                "is_refresh": True,
                "pruned_count": 0,
                "mention_ids": mention_ids,
                "readycheck_channel_id": msg.channel.id,
                "readycheck_message_id": msg.id,
            }

        # Post to lobby thread (target_channel is guaranteed to exist here)
        msg = await target_channel.send(embed=embed)
        # Register the generation without yielding so a fast user reaction
        # cannot arrive before this message becomes authoritative. This setter
        # only takes the lobby manager's in-memory lock; it performs no I/O.
        self.lobby_service.set_readycheck_state(
            msg.id,
            msg.channel.id,
            current_lobby_set,
            player_data,
            guild_id=guild_id,
            lobby_kind=kind,
            initial_reacted=reacted,
        )
        try:
            await msg.add_reaction("✅")
        except Exception as e:
            logger.debug("Failed to add checkmark reaction: %s", e)
        if ping_content:
            await target_channel.send(ping_content, allowed_mentions=allowed_mentions)

        await self.notify_readycheck_completion_if_ready(
            guild_id or 0,
            msg.id,
            fallback_channel=msg.channel,
            lobby_kind=kind,
        )
        if pruned_ids:
            note = (
                "🧹 Removed (away during ready check): "
                + " ".join(f"<@{pid}>" for pid in pruned_ids)
                + f" — re-join {kind.label} with `/join` if you're back."
            )
            await target_channel.send(
                note,
                allowed_mentions=discord.AllowedMentions(
                    users=[discord.Object(id=pid) for pid in pruned_ids]
                ),
            )
        return "ok", {
            "message_jump_url": msg.jump_url,
            "is_refresh": False,
            "pruned_count": len(pruned_ids),
            "mention_ids": mention_ids,
            "readycheck_channel_id": msg.channel.id,
            "readycheck_message_id": msg.id,
        }


def build_readycheck_embed(
    player_data: dict[int, dict],
    reacted: dict[int, str],
    ready_threshold: int = 10,
    lobby_kind: LobbyKind | str | None = None,
) -> tuple[discord.Embed, list[int]]:
    """Build the readycheck embed from stored classification data.

    Returns (embed, mention_ids) where mention_ids are all lobby members to ping.
    """
    now = time.time()
    count = len(player_data)
    description = f"**{count}** players in lobby"
    if count < ready_threshold:
        description += f" · need {ready_threshold - count} more for a full game"
    kind = LobbyKind.normalize(lobby_kind)
    embed = discord.Embed(
        title=f"{kind.label} Ready Check",
        description=description,
        color=discord.Color.blue(),
    )

    active_lines: list[str] = []
    afk_lines: list[str] = []
    mention_ids: list[int] = []

    for pid, d in player_data.items():
        if pid in reacted:
            continue
        if d["is_member"]:
            mention_ids.append(pid)
        join_ts = d.get("join_ts")
        time_str = f" ({format_duration_short(now - join_ts)})" if join_ts else ""
        if d["group"] == "active":
            active_lines.append(f"{d['name']} {d['signals']}{time_str}")
        else:
            if d["is_member"]:
                afk_lines.append(f"<@{pid}> {d['signals']}{time_str}")
            else:
                afk_lines.append(f"{d['name']} {d['signals']}{time_str}")

    if active_lines:
        embed.add_field(
            name=f"✅ Likely Active ({len(active_lines)})",
            value="\n".join(active_lines),
            inline=False,
        )
    if afk_lines:
        embed.add_field(
            name=f"⚠️ Possibly AFK ({len(afk_lines)})",
            value="\n".join(afk_lines),
            inline=False,
        )
    if reacted:
        embed.add_field(
            name=f"✅ Reacted to Ready Check ({len(reacted)})",
            value="\n".join(reacted.values()),
            inline=False,
        )

    embed.set_footer(text="React with ✅ to confirm you are ready")
    return embed, mention_ids


def _is_playing_dota(member: discord.Member) -> bool:
    """Check if a member is currently playing Dota 2."""
    for activity in member.activities:
        if isinstance(activity, discord.Game) and activity.name and "dota" in activity.name.lower():
            return True
        if isinstance(activity, discord.Activity) and activity.name and "dota" in activity.name.lower():
            return True
    return False


async def setup(bot: commands.Bot):
    lobby_service = getattr(bot, "lobby_service", None)
    player_service = getattr(bot, "player_service", None)
    curfew_service = getattr(bot, "curfew_service", None)
    cog = LobbyCommands(bot, lobby_service, player_service, curfew_service)
    await bot.add_cog(cog)
