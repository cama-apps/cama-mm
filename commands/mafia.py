"""Daily Mafia subgame commands and background phase loop."""

from __future__ import annotations

import asyncio
import logging
import time
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands, tasks

from commands.checks import require_mafia_channel
from config import MAFIA_CHANNEL_ID
from domain.models.mafia import (
    MafiaActionType,
    MafiaPhase,
    MafiaRole,
    MafiaTwist,
    MafiaWinner,
)
from services.mafia_service import (
    DAY_DURATION_S,
    ENTRY_FEE,
    MAX_WINNER_PAYOUT,
    MVP_BONUS,
    NIGHT_DURATION_S,
    PHASE_REMINDER_AFTER_S,
    phase_duration,
)
from services.permissions import has_admin_permission
from utils.formatting import JOPACOIN_EMOTE

if TYPE_CHECKING:
    from services.mafia_flavor_service import MafiaFlavorService
    from services.mafia_service import MafiaService

logger = logging.getLogger("cama_bot.commands.mafia")


ROLE_EMOJI = {
    MafiaRole.MAFIA: "🔪",
    MafiaRole.DOCTOR: "⚕️",
    MafiaRole.DETECTIVE: "🕵️",
    MafiaRole.VIGILANTE: "🔫",
    MafiaRole.TOWNIE: "👥",
    MafiaRole.JESTER: "🃏",
    MafiaRole.BOOKIE: "🎰",
}
GODFATHER_EMOJI = "👑"

TWIST_LABEL = {
    MafiaTwist.BLOOD_MOON: "🌑 Blood Moon",
    MafiaTwist.TOWN_HALL: "🏛️ Town Hall",
    MafiaTwist.MEMORY_FOG: "🌫️ Memory Fog",
    MafiaTwist.PLAGUE: "☠️ Plague",
    MafiaTwist.RESURRECTION: "✨ Resurrection",
}


def _mafia_post_channel(guild: discord.Guild) -> discord.TextChannel | None:
    """Channel for public mafia embeds.

    Prefers the dedicated MAFIA_CHANNEL_ID; falls back to the first text
    channel containing 'gamba' in its name so other guilds keep working.
    """
    dedicated = guild.get_channel(MAFIA_CHANNEL_ID)
    if isinstance(dedicated, discord.TextChannel):
        return dedicated
    for ch in guild.text_channels:
        if "gamba" in ch.name.lower():
            return ch
    return None


def _role_label(role: MafiaRole) -> str:
    return f"{ROLE_EMOJI.get(role, '')} {role.value.title()}"


class MafiaCommands(commands.Cog):
    mafia = app_commands.Group(name="mafia", description="Mafia subgame")
    admin = app_commands.Group(
        name="admin", description="Mafia admin controls", parent=mafia
    )

    def __init__(
        self,
        bot: commands.Bot,
        mafia_service: MafiaService,
        flavor_service: MafiaFlavorService,
    ):
        self.bot = bot
        self.mafia_service = mafia_service
        self.flavor_service = flavor_service
        # Per-guild memoization of public phase posts by (game_date, phase).
        self._announced_phases: dict[int, set[tuple[str, str]]] = {}

    async def cog_load(self) -> None:
        self._mafia_phase_loop.start()

    async def cog_unload(self) -> None:
        self._mafia_phase_loop.cancel()

    # ────────────────────────────────────────────────────────────────────
    # Subcommands
    # ────────────────────────────────────────────────────────────────────

    @mafia.command(name="role", description="Privately reveal your role in today's mafia game")
    async def role(self, interaction: discord.Interaction):
        if not await require_mafia_channel(interaction):
            return
        guild_id = interaction.guild.id if interaction.guild else None
        info = await asyncio.to_thread(
            self.mafia_service.get_player_role, guild_id, interaction.user.id
        )
        if info is None:
            await interaction.response.send_message(
                "You're not in today's mafia game (or no game is active).",
                ephemeral=True,
            )
            return

        titles = await asyncio.to_thread(
            self.mafia_service.get_titles, guild_id, interaction.user.id
        )
        embed = self._build_role_embed(info, titles)
        await interaction.response.send_message(embed=embed, ephemeral=True)

    @mafia.command(name="act", description="Submit your nightly mafia action")
    @app_commands.describe(target="Player to act on (Bookie: who you predict the town will lynch)")
    async def act(self, interaction: discord.Interaction, target: discord.User):
        if not await require_mafia_channel(interaction):
            return
        guild_id = interaction.guild.id if interaction.guild else None
        # Determine action type from caller's role.
        info = await asyncio.to_thread(
            self.mafia_service.get_player_role, guild_id, interaction.user.id
        )
        if info is None:
            await interaction.response.send_message(
                "You're not in today's game.", ephemeral=True
            )
            return
        role = MafiaRole(info["role"])
        action_map = {
            MafiaRole.MAFIA: MafiaActionType.KILL,
            MafiaRole.DOCTOR: MafiaActionType.SAVE,
            MafiaRole.DETECTIVE: MafiaActionType.INVESTIGATE,
            MafiaRole.VIGILANTE: MafiaActionType.VIG_KILL,
            MafiaRole.BOOKIE: MafiaActionType.WAGER,
        }
        if role not in action_map:
            await interaction.response.send_message(
                "Your role has no night action.", ephemeral=True
            )
            return

        result = await asyncio.to_thread(
            self.mafia_service.submit_night_action,
            guild_id,
            interaction.user.id,
            target.id,
            action_map[role],
        )
        if not result.get("ok"):
            await interaction.response.send_message(
                result.get("error", "Action rejected."), ephemeral=True
            )
            return

        if action_map[role] == MafiaActionType.INVESTIGATE:
            verdict = result.get("result", "?")
            msg = f"🕵️ Investigation result for {target.mention}: **{verdict}**."
        elif action_map[role] == MafiaActionType.SAVE:
            msg = f"⚕️ You will protect {target.mention} tonight."
        elif action_map[role] == MafiaActionType.VIG_KILL:
            msg = f"🔫 Your one shot is locked on {target.mention}."
        elif action_map[role] == MafiaActionType.WAGER:
            msg = (
                f"🎰 Ticket placed: you're calling {target.mention} to swing from "
                f"the rax today. Call it right and the house pays out."
            )
        else:
            msg = f"🔪 Kill vote registered for {target.mention}."
        await interaction.response.send_message(msg, ephemeral=True)

    @mafia.command(name="vote", description="Vote to lynch a player during the day phase")
    @app_commands.describe(target="Player to vote against")
    async def vote(self, interaction: discord.Interaction, target: discord.User):
        if not await require_mafia_channel(interaction):
            return
        guild_id = interaction.guild.id if interaction.guild else None
        result = await asyncio.to_thread(
            self.mafia_service.submit_day_vote,
            guild_id,
            interaction.user.id,
            target.id,
        )
        if not result.get("ok"):
            await interaction.response.send_message(
                result.get("error", "Vote rejected."), ephemeral=True
            )
            return
        verb = "changed to" if result.get("changed") else "locked in for"
        await interaction.response.send_message(
            f"🗳️ Vote {verb} {target.mention}. You can change it until dusk; "
            "tallies stay hidden until then.",
            ephemeral=True,
        )

    @mafia.command(
        name="bounty",
        description="Stake 1 jopacoin on a suspect — pays out if they're lynched and were mafia",
    )
    @app_commands.describe(target="The player you suspect is mafia")
    async def bounty(self, interaction: discord.Interaction, target: discord.User):
        if not await require_mafia_channel(interaction):
            return
        guild_id = interaction.guild.id if interaction.guild else None
        result = await asyncio.to_thread(
            self.mafia_service.submit_bounty, guild_id, interaction.user.id, target.id
        )
        if not result.get("ok"):
            await interaction.response.send_message(
                result.get("error", "Bounty rejected."), ephemeral=True
            )
            return
        await interaction.response.send_message(
            f"🎯 Bounty placed: 1 {JOPACOIN_EMOTE} on {target.mention}. If the town "
            "lynches them today and they bleed mafia red, the contributors split "
            "the bounty (up to the number still alive).",
            ephemeral=True,
        )

    @mafia.command(
        name="join",
        description="Reserve a seat in the next mafia game",
    )
    async def join(self, interaction: discord.Interaction):
        if not await require_mafia_channel(interaction):
            return
        guild_id = interaction.guild.id if interaction.guild else None
        result = await asyncio.to_thread(
            self.mafia_service.join, guild_id, interaction.user.id
        )
        if not result.get("ok"):
            await interaction.response.send_message(
                "You need to be registered before joining Mafia.",
                ephemeral=True,
            )
            return
        await interaction.response.send_message(
            "✅ You're queued for the next available Mafia roster. Seats go to "
            "registered, entry-fee-eligible players in signup order.",
            ephemeral=True,
        )

    @mafia.command(name="remind", description="Ping players who still need to act this phase")
    @app_commands.checks.cooldown(1, 60)
    async def remind(self, interaction: discord.Interaction):
        if not await require_mafia_channel(interaction):
            return
        guild_id = interaction.guild.id if interaction.guild else None
        game = await asyncio.to_thread(
            self.mafia_service.repo.get_active_game, guild_id
        )
        if game is None or game.phase not in (MafiaPhase.NIGHT, MafiaPhase.DAY):
            await interaction.response.send_message(
                "No game is waiting on anyone right now.", ephemeral=True
            )
            return
        if game.phase == MafiaPhase.NIGHT:
            missing = await asyncio.to_thread(
                self.mafia_service.players_needing_night_action, game
            )
            verb = "submit your night action with `/mafia act`"
        else:
            missing = await asyncio.to_thread(
                self.mafia_service.players_needing_day_vote, game
            )
            verb = "cast your vote with `/mafia vote`"
        if not missing:
            await interaction.response.send_message(
                "Everyone's already acted — the phase will resolve shortly.",
                ephemeral=True,
            )
            return
        pings = " ".join(f"<@{pid}>" for pid in missing[:25])
        await interaction.response.send_message(
            f"⏰ {pings} — the game is waiting on you. Please {verb}."
        )

    # ── Admin controls ──────────────────────────────────────────────────────

    @admin.command(name="start", description="Force-start a new mafia game")
    async def admin_start(self, interaction: discord.Interaction):
        if not await require_mafia_channel(interaction):
            return
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "❌ Admin only (Administrator or Manage Server).", ephemeral=True
            )
            return
        guild_id = interaction.guild.id if interaction.guild else None
        await interaction.response.defer(ephemeral=True)
        game = await asyncio.to_thread(
            self.mafia_service.start_game, guild_id, force=True
        )
        if game is None:
            await interaction.followup.send(
                "Couldn't start — too few eligible players, or a game is already running.",
                ephemeral=True,
            )
            return
        await self._post_setup(interaction.guild, game)
        await self._update_standings_board(interaction.guild, game)
        await interaction.followup.send(
            f"✅ Started Cama Mafia #{game.game_id} with {game.roster_size} players.",
            ephemeral=True,
        )

    @admin.command(name="stop", description="End the running game now with a standing tally")
    async def admin_stop(self, interaction: discord.Interaction):
        if not await require_mafia_channel(interaction):
            return
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "❌ Admin only (Administrator or Manage Server).", ephemeral=True
            )
            return
        guild_id = interaction.guild.id if interaction.guild else None
        await interaction.response.defer(ephemeral=True)
        summary = await asyncio.to_thread(self.mafia_service.force_finalize, guild_id)
        if not summary.get("resolved"):
            await interaction.followup.send("No active game to stop.", ephemeral=True)
            return
        refreshed = await asyncio.to_thread(
            self.mafia_service.repo.get_game_by_id, summary["game_id"]
        )
        await self._post_resolution(interaction.guild, refreshed, summary)
        await interaction.followup.send(
            f"🛑 Stopped & resolved — **{summary['winner']}** wins.", ephemeral=True
        )

    @admin.command(name="abort", description="Cancel the running game and refund entry fees")
    async def admin_abort(self, interaction: discord.Interaction):
        if not await require_mafia_channel(interaction):
            return
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "❌ Admin only (Administrator or Manage Server).", ephemeral=True
            )
            return
        guild_id = interaction.guild.id if interaction.guild else None
        await interaction.response.defer(ephemeral=True)
        result = await asyncio.to_thread(
            self.mafia_service.abort_game, guild_id, refund=True
        )
        if not result.get("ok"):
            await interaction.followup.send("No active game to abort.", ephemeral=True)
            return
        channel = _mafia_post_channel(interaction.guild)
        if channel is not None:
            if result.get("standings_message_id"):
                try:
                    board = await channel.fetch_message(result["standings_message_id"])
                    await board.unpin()
                except discord.HTTPException:
                    pass
            try:
                await channel.send(
                    "🚫 The current Mafia game was aborted by an admin. "
                    f"Entry fees were refunded to {len(result.get('refunded', {}))} players."
                )
            except discord.HTTPException:
                pass
        await interaction.followup.send("🚫 Game aborted and fees refunded.", ephemeral=True)

    @mafia.command(name="status", description="Show today's mafia game status")
    async def status(self, interaction: discord.Interaction):
        if not await require_mafia_channel(interaction):
            return
        guild_id = interaction.guild.id if interaction.guild else None
        s = await asyncio.to_thread(self.mafia_service.get_public_status, guild_id)
        if not s.get("active"):
            await interaction.response.send_message(
                "No Mafia game is active. Use `/mafia join` to queue for the next "
                "roster; it starts once at least 5 eligible players are queued.",
                ephemeral=True,
            )
            return

        embed = discord.Embed(
            title=f"Cama Mafia #{s['game_id']} — {s['phase']}",
            color=0x5865F2,
        )
        embed.add_field(
            name="Alive",
            value=f"{s['alive_count']} / {s['roster_size']}",
            inline=True,
        )
        if s.get("twist"):
            twist = MafiaTwist(s["twist"])
            embed.add_field(name="Twist", value=TWIST_LABEL[twist], inline=True)

        if s["phase"] == MafiaPhase.DAY.value:
            embed.add_field(
                name="Voted",
                value=f"{s.get('voted_count', 0)} / {s.get('alive_voters', 0)}",
                inline=True,
            )

        if s.get("phase_ends_at"):
            embed.add_field(
                name="Phase ends",
                value=f"<t:{s['phase_ends_at']}:R>",
                inline=False,
            )

        deaths = s.get("deaths") or []
        if deaths:
            embed.add_field(
                name="Deaths so far",
                value="\n".join(
                    f"<@{d['discord_id']}> — {_role_label(MafiaRole(d['role']))}"
                    f" ({d['phase']})"
                    for d in deaths
                ),
                inline=False,
            )
        await interaction.response.send_message(embed=embed, ephemeral=True)

    @mafia.command(name="history", description="Your mafia game history")
    async def history(self, interaction: discord.Interaction):
        if not await require_mafia_channel(interaction):
            return
        guild_id = interaction.guild.id if interaction.guild else None
        rows = await asyncio.to_thread(
            self.mafia_service.get_history, guild_id, interaction.user.id, 10
        )
        if not rows:
            await interaction.response.send_message(
                "No mafia history yet.", ephemeral=True
            )
            return
        lines = []
        for r in rows:
            outcome = "🏆 Win" if _is_winning_role(r["winner"], r["role"]) else "💀 Loss"
            gf = " 👑" if r["is_godfather"] else ""
            lines.append(
                f"`#{r['game_id']}` {r['game_date']} — "
                f"{_role_label(MafiaRole(r['role']))}{gf} ({r['hero_name']}) "
                f"→ {r['winner']} • {outcome}"
            )
        embed = discord.Embed(
            title="Your Mafia History",
            description="\n".join(lines),
            color=0x5865F2,
        )
        await interaction.response.send_message(embed=embed, ephemeral=True)

    @mafia.command(name="leaderboard", description="Mafia leaderboard for this guild")
    async def leaderboard(self, interaction: discord.Interaction):
        if not await require_mafia_channel(interaction):
            return
        guild_id = interaction.guild.id if interaction.guild else None
        rows = await asyncio.to_thread(self.mafia_service.get_leaderboard, guild_id, 20)
        if not rows:
            await interaction.response.send_message(
                "No resolved mafia games yet.", ephemeral=True
            )
            return
        lines = []
        for i, r in enumerate(rows, 1):
            wr = (r["wins"] / r["games_played"] * 100.0) if r["games_played"] else 0
            lines.append(
                f"`{i:>2}.` <@{r['discord_id']}> — "
                f"{r['wins']}W / {r['games_played']}G ({wr:.0f}%) • "
                f"🔪{r['mafia_wins']} 👥{r['town_wins']} 🃏{r['jester_wins']} ⭐{r['mvp_count']}"
            )
        embed = discord.Embed(
            title="Mafia Leaderboard",
            description="\n".join(lines),
            color=0xF1C40F,
        )
        await interaction.response.send_message(embed=embed, ephemeral=True)

    @mafia.command(name="info", description="How to play Daily Mafia")
    async def info(self, interaction: discord.Interaction):
        if not await require_mafia_channel(interaction):
            return
        embed = discord.Embed(
            title="Cama Mafia — Rules",
            description=(
                "A new game starts after the current one finishes once at least "
                "5 registered, entry-fee-eligible players are queued with "
                "`/mafia join`.\n\n"
                "**Phases**\n"
                f"• Night ({NIGHT_DURATION_S // 3600}h): "
                "Mafia/Doctor/Detective/Vigilante submit `/mafia act`.\n"
                f"• Day ({DAY_DURATION_S // 3600}h): "
                "Living players vote with `/mafia vote`. Tallies hidden.\n"
                "• Resolution: Winners split the pot, MVP gets a bonus from it.\n\n"
                "**Roles**\n"
                f"{ROLE_EMOJI[MafiaRole.MAFIA]} Mafia — kill at night.\n"
                f"{ROLE_EMOJI[MafiaRole.DOCTOR]} Doctor — protect one player at night.\n"
                f"{ROLE_EMOJI[MafiaRole.DETECTIVE]} Detective — investigate **one** player per night.\n"
                f"{ROLE_EMOJI[MafiaRole.VIGILANTE]} Vigilante — one-shot kill (10+ rosters).\n"
                f"{ROLE_EMOJI[MafiaRole.TOWNIE]} Townie — vote during the day.\n"
                f"{ROLE_EMOJI[MafiaRole.JESTER]} Jester — wins solo if lynched (rare).\n"
                f"{ROLE_EMOJI[MafiaRole.BOOKIE]} Bookie — neutral; wager at night on the day's "
                "lynch and cash out if you call it (rare).\n"
                f"{GODFATHER_EMOJI} Godfather — a mafia who reads as Town to the detective.\n\n"
                f"**Stakes**: every rostered player pays {ENTRY_FEE} {JOPACOIN_EMOTE} entry. "
                f"The pot (`roster × {ENTRY_FEE}`) is split among the winning faction, "
                f"MVP gets +{MVP_BONUS} from the pot, and each winner is capped at "
                f"+{MAX_WINNER_PAYOUT} {JOPACOIN_EMOTE} — anything over the cap funds the "
                "Jopacoin Reserve. Long-run EV is 0 — play to win.\n\n"
                "Use `/mafia optout` to stop required actions and cancel a pending signup."
            ),
            color=0x5865F2,
        )
        await interaction.response.send_message(embed=embed, ephemeral=True)

    @mafia.command(
        name="optout",
        description="Stop required actions and cancel your next-game signup",
    )
    async def optout(self, interaction: discord.Interaction):
        if not await require_mafia_channel(interaction):
            return
        guild_id = interaction.guild.id if interaction.guild else None
        await asyncio.to_thread(
            self.mafia_service.set_optout, guild_id, interaction.user.id, True
        )
        await interaction.response.send_message(
            "You're opted out. Mafia won't wait for or remind you, and any "
            "next-game signup was cancelled. Use `/mafia optin` to resume.",
            ephemeral=True,
        )

    @mafia.command(name="optin", description="Resume required actions in Mafia")
    async def optin(self, interaction: discord.Interaction):
        if not await require_mafia_channel(interaction):
            return
        guild_id = interaction.guild.id if interaction.guild else None
        await asyncio.to_thread(
            self.mafia_service.set_optout, guild_id, interaction.user.id, False
        )
        await interaction.response.send_message(
            "Welcome back. Use `/mafia join` if you want a seat in the next game.",
            ephemeral=True,
        )

    # ────────────────────────────────────────────────────────────────────
    # Background phase loop
    # ────────────────────────────────────────────────────────────────────

    @tasks.loop(minutes=5)
    async def _mafia_phase_loop(self):
        for guild in list(self.bot.guilds):
            try:
                await self._tick_guild(guild)
            except Exception:
                logger.exception("Mafia phase loop failed for guild %s", guild.id)

    @_mafia_phase_loop.before_loop
    async def _before_loop(self):
        await self.bot.wait_until_ready()

    @staticmethod
    def _phase_ends_at(game) -> int:
        start = game.phase_started_at or game.started_at
        return start + phase_duration(game.phase, start)

    async def _start_and_announce(self, guild: discord.Guild) -> None:
        """Start a fresh game (if none active) and post setup + standings."""
        new_game = await asyncio.to_thread(
            self.mafia_service.start_game, guild.id
        )
        if new_game is not None and new_game.phase != MafiaPhase.RESOLVED:
            await self._post_setup(guild, new_game)
            await self._update_standings_board(guild, new_game)

    async def _tick_guild(self, guild: discord.Guild) -> None:
        guild_id = guild.id
        game = await asyncio.to_thread(
            self.mafia_service.repo.get_active_game, guild_id
        )

        if game is None:
            # No game running → start one immediately (no calendar gating).
            await self._start_and_announce(guild)
            return

        if getattr(game, "status", "ACTIVE") != "ACTIVE":
            return

        now = int(time.time())
        phase_start = game.phase_started_at or game.started_at
        fallback = phase_duration(game.phase, phase_start)
        elapsed = now - phase_start

        if game.phase == MafiaPhase.NIGHT:
            ready = await asyncio.to_thread(self.mafia_service.night_ready, game)
            if ready or elapsed >= fallback:
                summary = await asyncio.to_thread(
                    self.mafia_service.resolve_night, guild_id
                )
                if summary.get("resolved"):
                    refreshed = await asyncio.to_thread(
                        self.mafia_service.repo.get_game_by_id, game.game_id
                    )
                    await self._post_day_recap(guild, refreshed, summary)
                    await self._update_standings_board(guild, refreshed)
            elif elapsed >= PHASE_REMINDER_AFTER_S:
                await self._maybe_post_reminder(guild, game, MafiaPhase.NIGHT)

        elif game.phase == MafiaPhase.DAY:
            ready = await asyncio.to_thread(self.mafia_service.day_ready, game)
            if ready or elapsed >= fallback:
                summary = await asyncio.to_thread(
                    self.mafia_service.resolve_day, guild_id
                )
                if summary.get("resolved"):
                    refreshed = await asyncio.to_thread(
                        self.mafia_service.repo.get_game_by_id, game.game_id
                    )
                    if summary.get("continued"):
                        await self._post_day_recap(guild, refreshed, summary)
                        await self._update_standings_board(guild, refreshed)
                    else:
                        await self._post_resolution(guild, refreshed, summary)
                        # The week is dead, long live the week — start the next
                        # game immediately.
                        await self._start_and_announce(guild)
            elif elapsed >= PHASE_REMINDER_AFTER_S:
                await self._maybe_post_reminder(guild, game, MafiaPhase.DAY)

    # ────────────────────────────────────────────────────────────────────
    # Public posting helpers
    # ────────────────────────────────────────────────────────────────────

    def _was_announced(self, guild_id: int, game_date: str, phase: MafiaPhase) -> bool:
        return (game_date, phase.value) in self._announced_phases.get(guild_id, set())

    def _mark_announced(self, guild_id: int, game_date: str, phase: MafiaPhase) -> None:
        self._announced_phases.setdefault(guild_id, set()).add((game_date, phase.value))

    async def _post_setup(self, guild: discord.Guild, game) -> None:
        if self._was_announced(guild.id, game.game_date, MafiaPhase.SETUP):
            return
        channel = _mafia_post_channel(guild)
        if channel is None:
            return

        players = await asyncio.to_thread(
            self.mafia_service.repo.get_players, game.game_id
        )
        narration = await self.flavor_service.setup_narration(game)
        roster = " ".join(f"<@{p.discord_id}>" for p in players)
        ends_at = self._phase_ends_at(game)

        twist_line = ""
        if game.twist_event:
            twist_line = f"\n**Twist:** {TWIST_LABEL[game.twist_event]}"

        pot_total = game.roster_size * ENTRY_FEE
        embed = discord.Embed(
            title=f"🌑 Cama Mafia #{game.game_id} — Night begins",
            description=(
                f"{narration}\n\n"
                f"**Roster ({game.roster_size}):** {roster}\n"
                f"**Stakes:** −{ENTRY_FEE} {JOPACOIN_EMOTE} each → "
                f"pot **{pot_total}** {JOPACOIN_EMOTE} for the winning faction.\n"
                f"Use `/mafia role` to learn yours.\n"
                f"Mafia/Doctor/Detective/Vigilante: `/mafia act target:@x`.\n"
                f"Night ends <t:{ends_at}:R>."
                f"{twist_line}"
            ),
            color=0x2C2F33,
        )
        try:
            msg = await channel.send(embed=embed)
            await asyncio.to_thread(
                self.mafia_service.repo.set_thread_ids,
                game.game_id,
                setup_message_id=msg.id,
            )
        except discord.HTTPException:
            logger.exception("Failed to post mafia setup announcement")

        # Try to create a private mafia coordination thread.
        try:
            mafia_players = [p for p in players if p.role == MafiaRole.MAFIA]
            if mafia_players:
                thread = await channel.create_thread(
                    name=f"Mafia #{game.game_id} — Mafia",
                    type=discord.ChannelType.private_thread,
                    auto_archive_duration=1440,
                    invitable=False,
                )
                for mp in mafia_players:
                    member = guild.get_member(mp.discord_id)
                    if member is not None:
                        try:
                            await thread.add_user(member)
                        except discord.HTTPException:
                            logger.warning(
                                "Could not add mafia member %s to thread", mp.discord_id
                            )
                gf_line = ""
                gf = next((p for p in mafia_players if p.is_godfather), None)
                if gf:
                    gf_line = f"\nGodfather: <@{gf.discord_id}> {GODFATHER_EMOJI}"
                allies = ", ".join(f"<@{p.discord_id}>" for p in mafia_players)
                await thread.send(
                    f"🔪 The mafia: {allies}.{gf_line}\n"
                    f"Coordinate kills here. Submit your final pick via `/mafia act`."
                )
                await asyncio.to_thread(
                    self.mafia_service.repo.set_thread_ids,
                    game.game_id,
                    mafia_thread_id=thread.id,
                )
        except discord.HTTPException:
            logger.warning(
                "Could not create private mafia thread for guild %s; continuing without it",
                guild.id,
            )

        self._mark_announced(guild.id, game.game_date, MafiaPhase.SETUP)

    def _once(self, guild_id: int, key: str) -> bool:
        """Return True if ``key`` was already posted (and mark it). Per-cycle keys
        (with day_number) let recaps/pings fire once per cycle across the week."""
        seen = self._announced_phases.setdefault(guild_id, set())
        if key in seen:
            return True
        seen.add(key)
        return False

    async def _post_day_recap(self, guild: discord.Guild, game, summary: dict) -> None:
        """One public recap per cycle: dawn reveal (after night) or dusk
        transition (after an undecided day). Each opens a fresh Town Square."""
        channel = _mafia_post_channel(guild)
        if channel is None:
            return

        is_dawn = "killed" in summary
        kind = "dawn" if is_dawn else "dusk"
        key = f"recap:{game.game_date}:{game.day_number}:{kind}"
        if self._once(guild.id, key):
            return

        players_by_id = {
            p.discord_id: p
            for p in await asyncio.to_thread(
                self.mafia_service.repo.get_players, game.game_id
            )
        }
        ends_at = self._phase_ends_at(game)
        body: list[str] = []

        if is_dawn:
            # Dawn: overnight deaths (the centerpiece reveal).
            for k in summary.get("killed", []):
                victim = players_by_id.get(k["discord_id"])
                if victim is not None:
                    body.append(
                        await self.flavor_service.death_narration(
                            victim, by_plague=(k["by"] == "plague")
                        )
                    )
            if not body:
                body.append("The night was quiet. No one died.")
            revived = summary.get("revived_id")
            if revived is not None:
                body.append(f"✨ The earth stirs — <@{revived}> claws back from the grave!")
            title = f"☀️ Day {game.day_number} dawns — Cama Mafia #{game.game_id}"
            footer = "Living players: debate, then `/mafia vote target:@x` (you can change it until dusk). `/mafia bounty target:@x` to stake a read."
        else:
            # Dusk: the lynch result, then night falls again.
            lynched = summary.get("lynched_id")
            if lynched is not None and lynched in players_by_id:
                body.append(await self.flavor_service.lynch_narration(players_by_id[lynched]))
            else:
                body.append(await self.flavor_service.no_lynch_narration())
            body.append(self._format_vote_reveal(summary))
            bounty = summary.get("bounty") or {}
            if bounty.get("reward"):
                body.append(f"🎯 The town bounty paid out **{bounty['reward']}** {JOPACOIN_EMOTE}.")
            title = f"🌙 Night {game.day_number} falls — Cama Mafia #{game.game_id}"
            footer = "Roles with night actions: `/mafia act target:@x`."

        embed = discord.Embed(title=title, description="\n".join(body), color=0xF1C40F)
        embed.add_field(name="Phase ends", value=f"<t:{ends_at}:R>", inline=False)
        embed.set_footer(text=footer)

        ping = await self._living_ping(game)
        try:
            msg = await channel.send(content=ping or None, embed=embed)
            if is_dawn:
                await self._open_town_square(guild, game, msg)
        except discord.HTTPException:
            logger.exception("Failed to post mafia day recap")

        await self._sync_graveyard(guild, game)

    def _format_vote_reveal(self, summary: dict) -> str:
        """Anonymous-until-dusk: reveal who voted whom at resolution."""
        detail = summary.get("vote_detail") or []
        if not detail:
            return "_No votes were cast._"
        lines = [
            f"• <@{v['actor_id']}> → <@{v['target_id']}>" for v in detail
        ]
        return "**🗳️ The votes are revealed:**\n" + "\n".join(lines)

    async def _living_ping(self, game) -> str:
        """@-mention living players for the phase-change ping."""
        players = await asyncio.to_thread(
            self.mafia_service.repo.get_players, game.game_id
        )
        mentions = " ".join(f"<@{p.discord_id}>" for p in players if p.is_alive)
        return mentions

    async def _open_town_square(self, guild: discord.Guild, game, msg) -> None:
        """Fresh public discussion thread for the day; archive the prior one."""
        if game.discussion_thread_id:
            try:
                old = guild.get_thread(game.discussion_thread_id)
                if isinstance(old, discord.Thread):
                    await old.edit(archived=True)
            except discord.HTTPException:
                pass
        try:
            thread = await msg.create_thread(
                name=f"Day {game.day_number} — Town Square",
                auto_archive_duration=1440,
            )
            await asyncio.to_thread(
                self.mafia_service.repo.set_thread_ids,
                game.game_id,
                discussion_thread_id=thread.id,
            )
        except discord.HTTPException:
            logger.warning("Could not create Town Square thread")

    async def _build_standings_embed(self, game) -> discord.Embed:
        players = await asyncio.to_thread(
            self.mafia_service.repo.get_players, game.game_id
        )
        alive = [p for p in players if p.is_alive]
        dead = [p for p in players if not p.is_alive]
        ends_at = self._phase_ends_at(game)
        phase_word = "🌙 Night" if game.phase == MafiaPhase.NIGHT else "☀️ Day"
        embed = discord.Embed(
            title=f"📊 Cama Mafia #{game.game_id} — Standings",
            color=0x5865F2,
        )
        embed.add_field(
            name=f"{phase_word} {game.day_number}",
            value=f"Phase ends <t:{ends_at}:R>",
            inline=False,
        )
        embed.add_field(
            name=f"🫀 Alive ({len(alive)})",
            value=" ".join(f"<@{p.discord_id}>" for p in alive) or "—",
            inline=False,
        )
        if dead:
            embed.add_field(
                name=f"💀 Fallen ({len(dead)})",
                value="\n".join(
                    f"<@{p.discord_id}> — {_role_label(p.role)}" for p in dead
                ),
                inline=False,
            )
        return embed

    async def _update_standings_board(self, guild: discord.Guild, game) -> None:
        channel = _mafia_post_channel(guild)
        if channel is None:
            return
        embed = await self._build_standings_embed(game)
        if game.standings_message_id:
            try:
                msg = await channel.fetch_message(game.standings_message_id)
                await msg.edit(embed=embed)
                return
            except discord.HTTPException:
                pass  # message gone — repost below
        try:
            msg = await channel.send(embed=embed)
            try:
                await msg.pin()
            except discord.HTTPException:
                pass
            await asyncio.to_thread(
                self.mafia_service.repo.set_thread_ids,
                game.game_id,
                standings_message_id=msg.id,
            )
        except discord.HTTPException:
            logger.warning("Could not post standings board")

    async def _sync_graveyard(self, guild: discord.Guild, game) -> None:
        """Add the fallen to a private graveyard thread so they can spectate."""
        channel = _mafia_post_channel(guild)
        if channel is None:
            return
        players = await asyncio.to_thread(
            self.mafia_service.repo.get_players, game.game_id
        )
        dead = [p for p in players if not p.is_alive]
        if not dead:
            return
        thread = None
        if game.graveyard_thread_id:
            thread = guild.get_thread(game.graveyard_thread_id)
        if thread is None:
            try:
                thread = await channel.create_thread(
                    name=f"⚰️ Mafia #{game.game_id} — Graveyard",
                    type=discord.ChannelType.private_thread,
                    auto_archive_duration=10080,
                    invitable=False,
                )
                await thread.send(
                    "Welcome to the graveyard. The dead see all and tell nothing "
                    "(to the living). Spectate, gossip, and heckle in peace. 🪦"
                )
                await asyncio.to_thread(
                    self.mafia_service.repo.set_thread_ids,
                    game.game_id,
                    graveyard_thread_id=thread.id,
                )
            except discord.HTTPException:
                logger.warning("Could not create graveyard thread")
                return
        for p in dead:
            member = guild.get_member(p.discord_id)
            if member is not None:
                try:
                    await thread.add_user(member)
                except discord.HTTPException:
                    pass

    async def _post_resolution(self, guild: discord.Guild, game, summary: dict) -> None:
        if self._was_announced(guild.id, game.game_date, MafiaPhase.RESOLVED):
            return
        channel = _mafia_post_channel(guild)
        if channel is None:
            return

        winner = MafiaWinner(summary["winner"])
        narration = await self.flavor_service.resolution_narration(winner)

        players_by_id = {
            p.discord_id: p
            for p in await asyncio.to_thread(
                self.mafia_service.repo.get_players, game.game_id
            )
        }

        body_lines: list[str] = [narration, ""]

        if summary.get("twist") == MafiaTwist.TOWN_HALL.value:
            body_lines.append("Town Hall was in session — no lynch today.")
        elif summary.get("lynched_id") is not None:
            victim = players_by_id.get(summary["lynched_id"])
            if victim is not None:
                lynch_line = await self.flavor_service.lynch_narration(victim)
                body_lines.append(lynch_line)
        else:
            body_lines.append(await self.flavor_service.no_lynch_narration())

        body_lines.append("")
        payout = summary.get("payout_per_winner", 0)
        pot_total = summary.get("pot_total", 0)
        winning_ids = summary.get("winning_ids", [])
        mvp_id = summary.get("mvp_id")

        if winning_ids:
            mention_list = ", ".join(f"<@{wid}>" for wid in winning_ids[:15])
            extra = "" if len(winning_ids) <= 15 else f" (+{len(winning_ids) - 15} more)"
            body_lines.append(
                f"**Pot:** {pot_total} {JOPACOIN_EMOTE} → "
                f"{payout} each to {mention_list}{extra}"
            )
        if mvp_id is not None:
            # Show the bonus actually awarded (clamped to the faction pot), not a
            # flat MVP_BONUS the small-pot case may never have paid.
            mvp_bonus = summary.get("mvp_bonus", MVP_BONUS)
            if mvp_bonus > 0:
                body_lines.append(
                    f"**MVP:** <@{mvp_id}> (+{mvp_bonus} {JOPACOIN_EMOTE})"
                )
            else:
                body_lines.append(f"**MVP:** <@{mvp_id}>")
        bookie_id = summary.get("bookie_id")
        if bookie_id is not None:
            body_lines.append(
                f"🎰 **The Bookie** <@{bookie_id}> called the lynch and cashed out "
                f"(+{summary.get('bookie_payout', 0)} {JOPACOIN_EMOTE}). The house wins."
            )
        overflow = summary.get("nonprofit_overflow", 0)
        if overflow > 0:
            body_lines.append(
                f"_{overflow} {JOPACOIN_EMOTE} over the +{MAX_WINNER_PAYOUT}/winner "
                f"cap skimmed into the Jopacoin Reserve._"
            )

        breakdown = summary.get("vote_breakdown") or {}
        if breakdown:
            sorted_votes = sorted(breakdown.items(), key=lambda kv: kv[1], reverse=True)
            body_lines.append("")
            body_lines.append("**Vote breakdown:**")
            body_lines.extend(
                f"• <@{tid}>: {count}" for tid, count in sorted_votes
            )

        embed = discord.Embed(
            title=f"🏁 Cama Mafia #{game.game_id} — {winner.value} wins",
            description="\n".join(body_lines),
            color=0x57F287 if winner == MafiaWinner.TOWN else 0xED4245,
        )

        try:
            await channel.send(embed=embed)
        except discord.HTTPException:
            logger.exception("Failed to post mafia resolution")

        # Archive discussion thread.
        if game.discussion_thread_id:
            try:
                thread = guild.get_thread(game.discussion_thread_id) or await self.bot.fetch_channel(
                    game.discussion_thread_id
                )
                if isinstance(thread, discord.Thread):
                    await thread.edit(archived=True)
            except discord.HTTPException:
                pass

        # Unpin the standings board now that the game is over.
        if game.standings_message_id:
            try:
                board = await channel.fetch_message(game.standings_message_id)
                await board.unpin()
            except discord.HTTPException:
                pass

        self._mark_announced(guild.id, game.game_date, MafiaPhase.RESOLVED)

    async def _maybe_post_reminder(
        self, guild: discord.Guild, game, phase: MafiaPhase
    ) -> None:
        channel = _mafia_post_channel(guild)
        if channel is None:
            return

        if phase == MafiaPhase.NIGHT:
            missing = await asyncio.to_thread(
                self.mafia_service.players_needing_night_action, game
            )
            label = "Night"
        else:
            missing = await asyncio.to_thread(
                self.mafia_service.players_needing_day_vote, game
            )
            label = "Day"
        ends_at = self._phase_ends_at(game)

        if not missing:
            return

        claimed = await asyncio.to_thread(
            self.mafia_service.repo.claim_phase_reminder,
            guild.id,
            game.game_id,
            game.day_number,
            phase,
        )
        if not claimed:
            return

        current = await asyncio.to_thread(
            self.mafia_service.repo.get_active_game, guild.id
        )
        if (
            current is None
            or current.game_id != game.game_id
            or current.day_number != game.day_number
            or current.phase != phase
        ):
            await asyncio.to_thread(
                self.mafia_service.repo.release_phase_reminder,
                guild.id,
                game.game_id,
                game.day_number,
                phase,
            )
            return

        if phase == MafiaPhase.NIGHT:
            missing = await asyncio.to_thread(
                self.mafia_service.players_needing_night_action, current
            )
        else:
            missing = await asyncio.to_thread(
                self.mafia_service.players_needing_day_vote, current
            )
        if not missing:
            await asyncio.to_thread(
                self.mafia_service.repo.release_phase_reminder,
                guild.id,
                game.game_id,
                game.day_number,
                phase,
            )
            return

        game = current
        ends_at = self._phase_ends_at(game)
        pings = " ".join(f"<@{pid}>" for pid in missing[:25])
        msg = (
            f"⚠️ {pings}\n{len(missing)} "
            f"{'player' if len(missing) == 1 else 'players'} "
            f"haven't acted yet — {label} {game.day_number} ends <t:{ends_at}:R>."
        )
        try:
            await channel.send(msg)
        except discord.HTTPException:
            await asyncio.to_thread(
                self.mafia_service.repo.release_phase_reminder,
                guild.id,
                game.game_id,
                game.day_number,
                phase,
            )
            return

    # ────────────────────────────────────────────────────────────────────
    # Embed builders
    # ────────────────────────────────────────────────────────────────────

    def _build_role_embed(self, info: dict, titles: list[str]) -> discord.Embed:
        role = MafiaRole(info["role"])
        gf = " " + GODFATHER_EMOJI if info.get("is_godfather") else ""
        title_line = f" — _{', '.join(titles)}_" if titles else ""
        embed = discord.Embed(
            title=f"{ROLE_EMOJI.get(role, '')} You are a {role.value.title()}{gf}",
            description=(
                f"Hero: **{info.get('hero_name') or 'Unknown'}**{title_line}\n"
                f"Phase: {info['phase']}\n"
                f"{'Alive' if info['is_alive'] else '💀 Dead'}"
            ),
            color=0x5865F2,
        )

        if "allies" in info and info["allies"]:
            embed.add_field(
                name="Mafia allies",
                value="\n".join(
                    f"<@{a['discord_id']}> ({a['hero_name']})"
                    f"{' ' + GODFATHER_EMOJI if a['is_godfather'] else ''}"
                    for a in info["allies"]
                ),
                inline=False,
            )

        if "investigations" in info:
            if info["investigations"]:
                embed.add_field(
                    name="Investigations",
                    value="\n".join(
                        f"<@{i['target_id']}>: **{i['result']}**"
                        for i in info["investigations"]
                    ),
                    inline=False,
                )
            else:
                embed.add_field(
                    name="Investigations",
                    value="No investigations yet.",
                    inline=False,
                )

        if role == MafiaRole.VIGILANTE:
            embed.add_field(
                name="Ability",
                value="One-shot kill. Use `/mafia act target:@x` once during any night.",
                inline=False,
            )
        elif role == MafiaRole.DETECTIVE:
            embed.add_field(
                name="Ability",
                value="Investigate **one** player per night with `/mafia act target:@x` — "
                "choose carefully, your read is locked once it's in.",
                inline=False,
            )
        elif role == MafiaRole.JESTER:
            embed.add_field(
                name="Win condition",
                value="Get yourself lynched during the day phase.",
                inline=False,
            )
        elif role == MafiaRole.BOOKIE:
            embed.add_field(
                name="Win condition",
                value="🎰 Neutral. At night, wager on who the town will lynch with "
                "`/mafia act target:@x`. Survive the day and call it right and you "
                "cash the ticket — a payout off the top, no matter who wins.",
                inline=False,
            )

        return embed


def _is_winning_role(winner: str | None, role: str) -> bool:
    if winner is None:
        return False
    if winner == MafiaWinner.TOWN.value:
        return role in {r.value for r in (MafiaRole.TOWNIE, MafiaRole.DOCTOR, MafiaRole.DETECTIVE, MafiaRole.VIGILANTE)}
    if winner == MafiaWinner.MAFIA.value:
        return role == MafiaRole.MAFIA.value
    if winner == MafiaWinner.JESTER.value:
        return role == MafiaRole.JESTER.value
    return False


async def setup(bot: commands.Bot):
    mafia_service = getattr(bot, "mafia_service", None)
    flavor_service = getattr(bot, "mafia_flavor_service", None)
    if mafia_service is None or flavor_service is None:
        raise RuntimeError("Mafia services not registered on bot.")
    await bot.add_cog(MafiaCommands(bot, mafia_service, flavor_service))
