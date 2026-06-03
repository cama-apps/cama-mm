"""Daily Mafia subgame commands and background phase loop."""

from __future__ import annotations

import asyncio
import logging
import time
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands, tasks

from domain.models.mafia import (
    MafiaActionType,
    MafiaPhase,
    MafiaRole,
    MafiaTwist,
    MafiaWinner,
)
from services.mafia_service import (
    DAY_DURATION_S,
    NIGHT_DURATION_S,
    PHASE_REMINDER_LEAD_S,
)
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
}
GODFATHER_EMOJI = "👑"

TWIST_LABEL = {
    MafiaTwist.BLOOD_MOON: "🌑 Blood Moon",
    MafiaTwist.TOWN_HALL: "🏛️ Town Hall",
    MafiaTwist.MEMORY_FOG: "🌫️ Memory Fog",
    MafiaTwist.PLAGUE: "☠️ Plague",
}


def _gamba_channel(guild: discord.Guild) -> discord.TextChannel | None:
    """Return the first text channel containing 'gamba' in its name."""
    for ch in guild.text_channels:
        if "gamba" in ch.name.lower():
            return ch
    return None


def _role_label(role: MafiaRole) -> str:
    return f"{ROLE_EMOJI.get(role, '')} {role.value.title()}"


class MafiaCommands(commands.Cog):
    mafia = app_commands.Group(name="mafia", description="Daily Mafia subgame")

    def __init__(
        self,
        bot: commands.Bot,
        mafia_service: MafiaService,
        flavor_service: MafiaFlavorService,
    ):
        self.bot = bot
        self.mafia_service = mafia_service
        self.flavor_service = flavor_service
        # Per-guild memoization of post / reminder state by (game_date, phase).
        self._announced_phases: dict[int, set[tuple[str, str]]] = {}
        self._reminded_phases: dict[int, set[tuple[str, str]]] = {}

    async def cog_load(self) -> None:
        self._mafia_phase_loop.start()

    async def cog_unload(self) -> None:
        self._mafia_phase_loop.cancel()

    # ────────────────────────────────────────────────────────────────────
    # Subcommands
    # ────────────────────────────────────────────────────────────────────

    @mafia.command(name="role", description="Privately reveal your role in today's mafia game")
    async def role(self, interaction: discord.Interaction):
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
    @app_commands.describe(target="Player to act on")
    async def act(self, interaction: discord.Interaction, target: discord.User):
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
        else:
            msg = f"🔪 Kill vote registered for {target.mention}."
        await interaction.response.send_message(msg, ephemeral=True)

    @mafia.command(name="vote", description="Vote to lynch a player during the day phase")
    @app_commands.describe(target="Player to vote against")
    async def vote(self, interaction: discord.Interaction, target: discord.User):
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
        await interaction.response.send_message(
            f"🗳️ Vote locked in for {target.mention}. (Tallies hidden until resolution.)",
            ephemeral=True,
        )

    @mafia.command(name="status", description="Show today's mafia game status")
    async def status(self, interaction: discord.Interaction):
        guild_id = interaction.guild.id if interaction.guild else None
        s = await asyncio.to_thread(self.mafia_service.get_public_status, guild_id)
        if not s.get("active"):
            await interaction.response.send_message(
                "No mafia game is active. Next game starts at the next 4 AM PST rollover.",
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
        embed = discord.Embed(
            title="Cama Mafia — Rules",
            description=(
                "A new game starts daily at **4 AM PST**, auto-rostered from "
                "everyone who used `/gamba` or `/dig` in the last 24 hours.\n\n"
                "**Phases**\n"
                "• Night (6h): Mafia/Doctor/Detective/Vigilante submit `/mafia act`.\n"
                "• Day (13h): Living players vote with `/mafia vote`. Tallies hidden.\n"
                "• Resolution: Winners paid, MVP gets a bonus.\n\n"
                "**Roles**\n"
                f"{ROLE_EMOJI[MafiaRole.MAFIA]} Mafia — kill at night.\n"
                f"{ROLE_EMOJI[MafiaRole.DOCTOR]} Doctor — protect one player at night.\n"
                f"{ROLE_EMOJI[MafiaRole.DETECTIVE]} Detective — investigate one player.\n"
                f"{ROLE_EMOJI[MafiaRole.VIGILANTE]} Vigilante — one-shot kill (10+ rosters).\n"
                f"{ROLE_EMOJI[MafiaRole.TOWNIE]} Townie — vote during the day.\n"
                f"{ROLE_EMOJI[MafiaRole.JESTER]} Jester — wins solo if lynched (rare).\n"
                f"{GODFATHER_EMOJI} Godfather — a mafia who reads as Town to the detective.\n\n"
                "**Payouts**: 40 jc base, +8 per extra player above 5. MVP +20.\n\n"
                "Use `/mafia optout` to skip auto-roster."
            ),
            color=0x5865F2,
        )
        await interaction.response.send_message(embed=embed, ephemeral=True)

    @mafia.command(name="optout", description="Skip auto-roster for future mafia games")
    async def optout(self, interaction: discord.Interaction):
        guild_id = interaction.guild.id if interaction.guild else None
        await asyncio.to_thread(
            self.mafia_service.set_optout, guild_id, interaction.user.id, True
        )
        await interaction.response.send_message(
            "You're opted out of mafia auto-roster. Use `/mafia optin` to rejoin.",
            ephemeral=True,
        )

    @mafia.command(name="optin", description="Rejoin mafia auto-roster")
    async def optin(self, interaction: discord.Interaction):
        guild_id = interaction.guild.id if interaction.guild else None
        await asyncio.to_thread(
            self.mafia_service.set_optout, guild_id, interaction.user.id, False
        )
        await interaction.response.send_message(
            "Welcome back. You'll be eligible for the next mafia game.",
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

    async def _tick_guild(self, guild: discord.Guild) -> None:
        guild_id = guild.id
        game = await asyncio.to_thread(
            self.mafia_service.repo.get_active_game, guild_id
        )

        if game is None:
            # Try to start today's game.
            new_game = await asyncio.to_thread(
                self.mafia_service.start_daily_game, guild_id
            )
            if new_game is not None and new_game.phase != MafiaPhase.RESOLVED:
                await self._post_setup(guild, new_game)
            return

        now = int(time.time())
        elapsed = now - game.started_at

        if game.phase == MafiaPhase.NIGHT:
            if elapsed >= NIGHT_DURATION_S:
                summary = await asyncio.to_thread(
                    self.mafia_service.resolve_night, guild_id
                )
                if summary.get("resolved"):
                    refreshed = await asyncio.to_thread(
                        self.mafia_service.repo.get_game_by_id, game.game_id
                    )
                    await self._post_day_announcement(guild, refreshed, summary)
            elif elapsed >= NIGHT_DURATION_S - PHASE_REMINDER_LEAD_S:
                await self._maybe_post_reminder(guild, game, MafiaPhase.NIGHT)

        elif game.phase == MafiaPhase.DAY:
            if elapsed >= NIGHT_DURATION_S + DAY_DURATION_S:
                summary = await asyncio.to_thread(
                    self.mafia_service.resolve_day, guild_id
                )
                if summary.get("resolved"):
                    refreshed = await asyncio.to_thread(
                        self.mafia_service.repo.get_game_by_id, game.game_id
                    )
                    await self._post_resolution(guild, refreshed, summary)
            elif elapsed >= NIGHT_DURATION_S + DAY_DURATION_S - PHASE_REMINDER_LEAD_S:
                await self._maybe_post_reminder(guild, game, MafiaPhase.DAY)

    # ────────────────────────────────────────────────────────────────────
    # Public posting helpers
    # ────────────────────────────────────────────────────────────────────

    def _was_announced(self, guild_id: int, game_date: str, phase: MafiaPhase) -> bool:
        return (game_date, phase.value) in self._announced_phases.get(guild_id, set())

    def _mark_announced(self, guild_id: int, game_date: str, phase: MafiaPhase) -> None:
        self._announced_phases.setdefault(guild_id, set()).add((game_date, phase.value))

    def _was_reminded(self, guild_id: int, game_date: str, phase: MafiaPhase) -> bool:
        return (game_date, phase.value) in self._reminded_phases.get(guild_id, set())

    def _mark_reminded(self, guild_id: int, game_date: str, phase: MafiaPhase) -> None:
        self._reminded_phases.setdefault(guild_id, set()).add((game_date, phase.value))

    async def _post_setup(self, guild: discord.Guild, game) -> None:
        if self._was_announced(guild.id, game.game_date, MafiaPhase.SETUP):
            return
        channel = _gamba_channel(guild)
        if channel is None:
            return

        players = await asyncio.to_thread(
            self.mafia_service.repo.get_players, game.game_id
        )
        narration = await self.flavor_service.setup_narration(game)
        roster = " ".join(f"<@{p.discord_id}>" for p in players)
        ends_at = game.started_at + NIGHT_DURATION_S

        twist_line = ""
        if game.twist_event:
            twist_line = f"\n**Twist:** {TWIST_LABEL[game.twist_event]}"

        embed = discord.Embed(
            title=f"🌑 Cama Mafia #{game.game_id} — Night begins",
            description=(
                f"{narration}\n\n"
                f"**Roster ({game.roster_size}):** {roster}\n"
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

    async def _post_day_announcement(self, guild: discord.Guild, game, summary: dict) -> None:
        if self._was_announced(guild.id, game.game_date, MafiaPhase.DAY):
            return
        channel = _gamba_channel(guild)
        if channel is None:
            return

        ends_at = game.started_at + NIGHT_DURATION_S + DAY_DURATION_S
        killed = summary.get("killed", [])

        # Render deaths with role + hero.
        players_by_id = {
            p.discord_id: p
            for p in await asyncio.to_thread(
                self.mafia_service.repo.get_players, game.game_id
            )
        }

        death_lines: list[str] = []
        for k in killed:
            victim = players_by_id.get(k["discord_id"])
            if victim is None:
                continue
            line = await self.flavor_service.death_narration(
                victim, by_plague=(k["by"] == "plague")
            )
            death_lines.append(line)

        if not death_lines:
            death_lines.append("The night was quiet. No one died.")

        embed = discord.Embed(
            title=f"☀️ Cama Mafia #{game.game_id} — Day Cycle",
            description="\n".join(death_lines),
            color=0xF1C40F,
        )
        embed.add_field(
            name="Vote",
            value="Living players: cast your vote with `/mafia vote target:@x`.",
            inline=False,
        )
        embed.add_field(name="Day ends", value=f"<t:{ends_at}:R>", inline=False)

        try:
            msg = await channel.send(embed=embed)
            # Spawn discussion thread.
            try:
                thread = await msg.create_thread(
                    name=f"Mafia #{game.game_id} — Discussion",
                    auto_archive_duration=1440,
                )
                await asyncio.to_thread(
                    self.mafia_service.repo.set_thread_ids,
                    game.game_id,
                    discussion_thread_id=thread.id,
                )
            except discord.HTTPException:
                logger.warning("Could not create discussion thread")
        except discord.HTTPException:
            logger.exception("Failed to post mafia day announcement")

        # Archive mafia thread if we have one.
        if game.mafia_thread_id:
            try:
                mthread = guild.get_thread(game.mafia_thread_id) or await self.bot.fetch_channel(
                    game.mafia_thread_id
                )
                if isinstance(mthread, discord.Thread):
                    await mthread.edit(archived=True)
            except discord.HTTPException:
                pass

        self._mark_announced(guild.id, game.game_date, MafiaPhase.DAY)

    async def _post_resolution(self, guild: discord.Guild, game, summary: dict) -> None:
        if self._was_announced(guild.id, game.game_date, MafiaPhase.RESOLVED):
            return
        channel = _gamba_channel(guild)
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
        winning_ids = summary.get("winning_ids", [])
        mvp_id = summary.get("mvp_id")

        if winning_ids:
            mention_list = ", ".join(f"<@{wid}>" for wid in winning_ids[:15])
            extra = "" if len(winning_ids) <= 15 else f" (+{len(winning_ids) - 15} more)"
            body_lines.append(
                f"**Payout:** {payout} {JOPACOIN_EMOTE} each → {mention_list}{extra}"
            )
        if mvp_id is not None:
            body_lines.append(
                f"**MVP:** <@{mvp_id}> (+{20} {JOPACOIN_EMOTE})"
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

        self._mark_announced(guild.id, game.game_date, MafiaPhase.RESOLVED)

    async def _maybe_post_reminder(
        self, guild: discord.Guild, game, phase: MafiaPhase
    ) -> None:
        if self._was_reminded(guild.id, game.game_date, phase):
            return
        channel = _gamba_channel(guild)
        if channel is None:
            return

        if phase == MafiaPhase.NIGHT:
            missing = await asyncio.to_thread(
                self.mafia_service.players_needing_night_action, game
            )
            ends_at = game.started_at + NIGHT_DURATION_S
            label = "Night"
        else:
            missing = await asyncio.to_thread(
                self.mafia_service.players_needing_day_vote, game
            )
            ends_at = game.started_at + NIGHT_DURATION_S + DAY_DURATION_S
            label = "Day"

        if not missing:
            return

        msg = (
            f"⚠️ {len(missing)} {'player' if len(missing) == 1 else 'players'} "
            f"haven't acted yet. {label} ends <t:{ends_at}:R>."
        )
        try:
            await channel.send(msg)
        except discord.HTTPException:
            return
        self._mark_reminded(guild.id, game.game_date, phase)

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
        elif role == MafiaRole.JESTER:
            embed.add_field(
                name="Win condition",
                value="Get yourself lynched during the day phase.",
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
