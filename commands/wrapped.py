"""
Cama Wrapped commands - Year in review feature.

Provides /wrapped command with a unified Spotify Wrapped-style story experience.
"""

import asyncio
import io
import logging
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Callable

import discord
from discord import app_commands
from discord.ext import commands

from services.wrapped_service import get_random_flavor
from utils.hero_lookup import get_hero_name
from utils.interaction_safety import safe_defer, safe_followup
from utils.wrapped_drawing import (
    SLIDE_COLORS,
    draw_awards_grid,
    draw_hero_spotlight_slide,
    draw_package_deal_slide,
    draw_pairwise_slide,
    draw_records_slide,
    draw_lane_breakdown_slide,
    draw_story_slide,
    draw_summary_stats_slide,
    draw_wrapped_summary,
    wrap_chart_in_slide,
)

logger = logging.getLogger("cama_bot.commands.wrapped")


def select_awards_for_viewer(
    all_awards: list, viewer_id: int, max_awards: int = 6
) -> list:
    """Select awards to display, guaranteeing viewer's wins are included."""
    import random

    viewer_awards = [a for a in all_awards if a.discord_id == viewer_id]
    other_awards = [a for a in all_awards if a.discord_id != viewer_id]
    random.shuffle(other_awards)
    slots_remaining = max_awards - len(viewer_awards)
    selected = viewer_awards + other_awards[: max(slots_remaining, 0)]
    return selected[:max_awards]


def _get_hero_names_dict() -> dict[int, str]:
    """Build a dict of hero_id -> hero_name for image generation."""
    hero_names = {}
    for hero_id in range(1, 150):
        name = get_hero_name(hero_id)
        if name and name != "Unknown":
            hero_names[hero_id] = name
    return hero_names


@dataclass
class WrappedSlide:
    """A single slide in the wrapped story."""

    slide_type: str
    title: str
    render_fn: Callable[[], io.BytesIO]


class WrappedStoryView(discord.ui.View):
    """Unified view for the wrapped story experience with Prev/Next navigation."""

    def __init__(self, slides: list[WrappedSlide], owner_id: int, timeout: int = 300):
        super().__init__(timeout=timeout)
        self.slides = slides
        self.owner_id = owner_id
        self.current_slide = 0
        self._slide_cache: dict[int, bytes] = {}
        self.message: discord.Message | None = None
        self._update_buttons()

    async def interaction_check(self, interaction: discord.Interaction) -> bool:
        if interaction.user.id != self.owner_id:
            await interaction.response.send_message(
                "Only the command invoker can navigate this wrapped.", ephemeral=True
            )
            return False
        return True

    def _update_buttons(self):
        self.prev_button.disabled = self.current_slide == 0
        self.next_button.disabled = self.current_slide >= len(self.slides) - 1

    async def render_slide(self, index: int) -> discord.File:
        """Render a slide, using cache if available."""
        if index not in self._slide_cache:
            buf = await asyncio.to_thread(self.slides[index].render_fn)
            self._slide_cache[index] = buf.read()
        return discord.File(io.BytesIO(self._slide_cache[index]), filename="wrapped_slide.png")

    @discord.ui.button(label="< Prev", style=discord.ButtonStyle.secondary)
    async def prev_button(self, interaction: discord.Interaction, button: discord.ui.Button):
        if self.current_slide > 0:
            self.current_slide -= 1
            self._update_buttons()
            file = await self.render_slide(self.current_slide)
            await interaction.response.edit_message(attachments=[file], view=self)

    @discord.ui.button(label="Next >", style=discord.ButtonStyle.primary)
    async def next_button(self, interaction: discord.Interaction, button: discord.ui.Button):
        if self.current_slide < len(self.slides) - 1:
            self.current_slide += 1
            self._update_buttons()
            file = await self.render_slide(self.current_slide)
            await interaction.response.edit_message(attachments=[file], view=self)

    async def on_timeout(self):
        if self.message:
            try:
                self.prev_button.disabled = True
                self.next_button.disabled = True
                await self.message.edit(view=self)
            except discord.HTTPException:
                pass


async def _prefetch_avatars(
    guild: discord.Guild | None,
    discord_ids: set[int],
) -> dict[int, bytes]:
    """Pre-fetch Discord avatars for pairwise slides."""
    avatar_cache: dict[int, bytes] = {}
    if not guild:
        return avatar_cache
    for did in discord_ids:
        try:
            member = guild.get_member(did) or await guild.fetch_member(did)
            if member and member.avatar:
                avatar_bytes = await member.avatar.read()
                avatar_cache[did] = avatar_bytes
        except Exception:
            logger.debug("Failed to fetch avatar for user %d", did)
    return avatar_cache


def _build_slides(
    server_wrapped,
    personal_summary,
    records_wrapped,
    pairwise_data,
    package_deal_data,
    hero_spotlight,
    role_breakdown,
    gamba_data,
    rating_history,
    hero_names: dict[int, str],
    target_username: str,
    target_user_id: int,
    year_label: str,
    avatar_cache: dict[int, bytes],
) -> list[WrappedSlide]:
    """Build the complete ordered slide list for the wrapped story."""
    slides: list[WrappedSlide] = []

    # --- Slide 1: Server Summary ---
    if server_wrapped:
        # Use default-argument binding to avoid closure bugs
        def _render_server_summary(sw=server_wrapped, hn=hero_names):
            return draw_wrapped_summary(sw, hn)
        slides.append(WrappedSlide("server_summary", "Server Summary", _render_server_summary))

    # --- Slide 2: Awards ---
    if server_wrapped and server_wrapped.awards:
        def _render_awards(all_awards=server_wrapped.awards, uid=target_user_id):
            selected = select_awards_for_viewer(all_awards, uid)
            return draw_awards_grid(selected, viewer_discord_id=uid)
        slides.append(WrappedSlide("awards", "Awards", _render_awards))

    # --- Slide 3: Your Year In Review (Big Reveal) ---
    if personal_summary:
        def _render_games_reveal(ps=personal_summary, yl=year_label):
            comparisons = []
            if ps.games_played_percentile > 0:
                comparisons.append(f"More than {ps.games_played_percentile:.0f}% of players")
            return draw_story_slide(
                headline="YOUR YEAR IN REVIEW",
                stat_value=str(ps.games_played),
                stat_label="GAMES PLAYED",
                flavor_text=ps.flavor_text,
                accent_color=SLIDE_COLORS["story_games"],
                username=ps.discord_username,
                year_label=yl,
                comparisons=comparisons,
            )
        slides.append(WrappedSlide("story_games", "Your Year", _render_games_reveal))

    # --- Slide 4: Summary Stats Grid ---
    if personal_summary:
        def _render_stats_grid(ps=personal_summary, yl=year_label):
            def _pct_text(pct: float) -> str:
                if pct >= 50:
                    return f"Top {max(100 - pct, 1):.0f}%"
                return f"Bottom {max(pct, 1):.0f}%"

            kda = (ps.total_kills + ps.total_assists) / max(ps.total_deaths, 1)
            dur_min = ps.avg_game_duration // 60

            stats = [
                (f"{ps.win_rate*100:.0f}%", "WIN RATE", _pct_text(ps.win_rate_percentile), (241, 196, 15)),
                (f"{kda:.1f}", "AVG KDA", _pct_text(ps.kda_percentile), (88, 101, 242)),
                (f"{dur_min}m", "AVG GAME", "", (155, 89, 182)),
                (f"{ps.total_kills}/{ps.total_deaths}/{ps.total_assists}", "TOTAL K/D/A", _pct_text(ps.total_kda_percentile), (237, 66, 69)),
                (str(ps.unique_heroes), "UNIQUE HEROES", _pct_text(ps.unique_heroes_percentile), (46, 204, 113)),
            ]
            return draw_summary_stats_slide(ps.discord_username, yl, stats)
        slides.append(WrappedSlide("story_summary", "Stats Grid", _render_stats_grid))

    # --- Slides 5-9: Personal Records ---
    if records_wrapped and records_wrapped.records:
        record_slides = records_wrapped.get_slides()
        for idx, (title, color_key, records) in enumerate(record_slides):
            accent = SLIDE_COLORS.get(color_key, (241, 196, 15))
            # Bind loop variables explicitly
            def _render_records(
                t=title, a=accent, r=records,
                u=records_wrapped.discord_username,
                m=records_wrapped.year_label,
                si=idx+1, ts=len(record_slides), hn=hero_names,
            ):
                return draw_records_slide(t, a, r, u, m, si, ts, hn)
            slides.append(WrappedSlide(f"records_{color_key}", title, _render_records))

    # --- Slide 10: Hero Spotlight ---
    if hero_spotlight:
        def _render_hero(hs=hero_spotlight, yl=year_label, u=target_username):
            return draw_hero_spotlight_slide(
                u, yl,
                {"name": hs.top_hero_name, "picks": hs.top_hero_picks,
                 "wins": hs.top_hero_wins, "win_rate": hs.top_hero_win_rate},
                hs.top_3_heroes, hs.unique_heroes,
            )
        slides.append(WrappedSlide("story_hero", "Hero Spotlight", _render_hero))

    # --- Slide 11: Lane Breakdown ---
    if role_breakdown and role_breakdown.lane_freq:
        def _render_lanes(rb=role_breakdown, yl=year_label, u=target_username):
            return draw_lane_breakdown_slide(u, yl, rb.lane_freq, rb.total_games)
        slides.append(WrappedSlide("story_lanes", "Lane Breakdown", _render_lanes))

    # --- Slide 12: Teammates (all-time) ---
    if pairwise_data and (pairwise_data.best_teammates or pairwise_data.most_played_with):
        def _render_teammates(pw=pairwise_data, u=target_username, ac=avatar_cache):
            entries = []
            for tm in pw.best_teammates[:3]:
                entries.append({
                    "discord_id": tm.discord_id, "username": tm.username,
                    "games": tm.games, "wins": tm.wins, "win_rate": tm.win_rate,
                    "label": "Best Teammate", "flavor": get_random_flavor("teammate_best"),
                })
            for tm in pw.most_played_with[:3]:
                if not any(e["discord_id"] == tm.discord_id for e in entries):
                    entries.append({
                        "discord_id": tm.discord_id, "username": tm.username,
                        "games": tm.games, "wins": tm.wins, "win_rate": tm.win_rate,
                        "label": "Most Played With", "flavor": None,
                    })
            return draw_pairwise_slide(u, "All-Time", entries[:6], "teammates", ac)
        slides.append(WrappedSlide("story_teammates", "Teammates", _render_teammates))

    # --- Slide 13: Rivals (all-time) ---
    if pairwise_data and (pairwise_data.nemesis or pairwise_data.punching_bag or pairwise_data.most_played_against):
        def _render_rivals(pw=pairwise_data, u=target_username, ac=avatar_cache):
            entries = []
            if pw.nemesis:
                entries.append({
                    "discord_id": pw.nemesis.discord_id, "username": pw.nemesis.username,
                    "games": pw.nemesis.games, "wins": pw.nemesis.wins,
                    "win_rate": pw.nemesis.win_rate,
                    "label": "Nemesis", "flavor": get_random_flavor("rival_nemesis"),
                })
            if pw.punching_bag:
                entries.append({
                    "discord_id": pw.punching_bag.discord_id, "username": pw.punching_bag.username,
                    "games": pw.punching_bag.games, "wins": pw.punching_bag.wins,
                    "win_rate": pw.punching_bag.win_rate,
                    "label": "Punching Bag", "flavor": get_random_flavor("rival_punching_bag"),
                })
            for opp in pw.most_played_against[:3]:
                if not any(e["discord_id"] == opp.discord_id for e in entries):
                    entries.append({
                        "discord_id": opp.discord_id, "username": opp.username,
                        "games": opp.games, "wins": opp.wins, "win_rate": opp.win_rate,
                        "label": "Most Faced", "flavor": None,
                    })
            return draw_pairwise_slide(u, "All-Time", entries[:6], "rivals", ac)
        slides.append(WrappedSlide("story_rivals", "Rivals", _render_rivals))

    # --- Slide 14: Package Deals (all-time, conditional) ---
    if package_deal_data:
        def _render_deals(pd=package_deal_data, u=target_username):
            return draw_package_deal_slide(
                u, "All-Time",
                times_bought=pd.times_bought,
                times_bought_on_you=pd.times_bought_on_you,
                unique_buyers=pd.unique_buyers,
                jc_spent=pd.jc_spent,
                jc_spent_on_you=pd.jc_spent_on_you,
                total_games=pd.total_games_committed,
            )
        slides.append(WrappedSlide("story_packages", "Package Deals", _render_deals))

    # --- Slide 15: Rating Chart (all-time, conditional) ---
    if rating_history and len(rating_history) >= 2:
        def _render_rating_chart(rh=rating_history, u=target_username):
            from utils.drawing import draw_rating_history_chart
            chart_buf = draw_rating_history_chart(u, rh)
            chart_bytes = chart_buf.read()
            return wrap_chart_in_slide(chart_bytes, "Rating History (All-Time)", "")
        slides.append(WrappedSlide("chart_rating", "Rating Chart", _render_rating_chart))

    # --- Slides 16-17: Gamba Story + Chart (all-time, conditional) ---
    if gamba_data:
        pnl_series, gamba_stats = gamba_data
        if pnl_series:
            def _render_gamba_story(gs=gamba_stats, u=target_username):
                degen_score = gs.get("degen_score", 0)
                net_pnl = gs.get("net_pnl", 0)
                total_bets = gs.get("total_bets", 0)
                pnl_str = f"+{net_pnl}" if net_pnl >= 0 else str(net_pnl)

                if degen_score >= 60:
                    flavor = get_random_flavor("gamba_degen")
                elif net_pnl > 0:
                    flavor = get_random_flavor("gamba_winner")
                elif net_pnl < 0:
                    flavor = get_random_flavor("gamba_loser")
                else:
                    flavor = get_random_flavor("gamba_casual")

                comparisons = [
                    f"{total_bets} total bets",
                    f"Degen Score: {degen_score}",
                ]
                return draw_story_slide(
                    headline="YOUR GAMBA JOURNEY",
                    stat_value=f"{pnl_str} JC",
                    stat_label="NET P&L (ALL-TIME)",
                    flavor_text=flavor,
                    accent_color=(87, 242, 135) if net_pnl >= 0 else (237, 66, 69),
                    username=u,
                    year_label="All-Time",
                    comparisons=comparisons,
                )
            slides.append(WrappedSlide("story_gamba", "Gamba Story", _render_gamba_story))

            def _render_gamba_chart(
                ps=pnl_series, gs=gamba_stats, u=target_username,
            ):
                from utils.drawing import draw_gamba_chart
                chart_buf = draw_gamba_chart(
                    u,
                    gs.get("degen_score", 0),
                    gs.get("degen_title", ""),
                    gs.get("degen_emoji", ""),
                    ps,
                    gs,
                )
                chart_bytes = chart_buf.read()
                return wrap_chart_in_slide(chart_bytes, "Gamba Chart (All-Time)", "")
            slides.append(WrappedSlide("chart_gamba", "Gamba Chart", _render_gamba_chart))

    return slides


class WrappedCog(commands.Cog):
    """Cog for Cama Wrapped year-in-review summaries."""

    def __init__(self, bot: commands.Bot):
        self.bot = bot
        self._hero_names: dict[int, str] | None = None

    @property
    def hero_names(self) -> dict[int, str]:
        if self._hero_names is None:
            self._hero_names = _get_hero_names_dict()
        return self._hero_names

    @property
    def wrapped_service(self):
        return getattr(self.bot, "wrapped_service", None)

    def _fetch_all_wrapped_data(
        self, guild_id: int | None, year: int, target_user_id: int,
    ) -> tuple:
        """Fetch all data needed for the wrapped story slides (synchronous)."""
        ws = self.wrapped_service

        server_wrapped = ws.get_server_wrapped(guild_id, year)
        personal_summary = ws.get_personal_summary_wrapped(target_user_id, year, guild_id)
        records_wrapped = ws.get_player_records_wrapped(target_user_id, year, guild_id)
        pairwise_data = ws.get_pairwise_wrapped(target_user_id, guild_id)
        package_deal_data = ws.get_package_deal_wrapped(target_user_id, guild_id)
        hero_spotlight = ws.get_hero_spotlight_wrapped(target_user_id, year, guild_id)
        role_breakdown = ws.get_role_breakdown_wrapped(target_user_id, year, guild_id)

        # Gamba data
        gamba_data = None
        gambling_stats = getattr(self.bot, "gambling_stats_service", None)
        if gambling_stats:
            pnl_series = gambling_stats.get_cumulative_pnl_series(target_user_id, guild_id)
            if pnl_series:
                player_stats = gambling_stats.get_player_stats(target_user_id, guild_id)
                if player_stats:
                    degen = gambling_stats.calculate_degen_score(target_user_id, guild_id)
                    gamba_stats = {
                        "total_bets": player_stats.total_bets,
                        "win_rate": player_stats.win_rate,
                        "net_pnl": player_stats.net_pnl,
                        "roi": player_stats.roi,
                        "degen_score": degen.total if degen else 0,
                        "degen_title": degen.title if degen else "",
                        "degen_emoji": degen.emoji if degen else "",
                    }
                    gamba_data = (pnl_series, gamba_stats)

        # Rating history
        rating_history = None
        match_repo = getattr(self.bot, "match_repo", None)
        if match_repo:
            try:
                rating_history = match_repo.get_player_rating_history_detailed(
                    target_user_id, guild_id
                )
            except Exception as e:
                logger.debug("Failed to fetch rating history: %s", e)

        return (
            server_wrapped, personal_summary, records_wrapped,
            pairwise_data, package_deal_data, hero_spotlight,
            role_breakdown, gamba_data, rating_history,
        )

    @app_commands.command(name="wrapped", description="View your Cama Wrapped year in review")
    @app_commands.describe(
        user="View another user's wrapped",
    )
    async def wrapped(
        self,
        interaction: discord.Interaction,
        user: discord.User | None = None,
    ):
        """View wrapped story for the current year."""
        if not self.wrapped_service:
            await interaction.response.send_message(
                "Wrapped feature is not available.", ephemeral=True
            )
            return

        if not await safe_defer(interaction):
            return

        year = datetime.now(timezone.utc).year
        guild_id = interaction.guild.id if interaction.guild else None
        target_user = user or interaction.user

        try:
            # Fetch all data
            (
                server_wrapped, personal_summary, records_wrapped,
                pairwise_data, package_deal_data, hero_spotlight,
                role_breakdown, gamba_data, rating_history,
            ) = await asyncio.to_thread(
                self._fetch_all_wrapped_data,
                guild_id, year, target_user.id,
            )

            if not server_wrapped:
                await safe_followup(
                    interaction,
                    content=f"No match data found for {year}.",
                    ephemeral=True,
                )
                return

            # Collect all referenced discord IDs for avatar pre-fetching
            avatar_ids: set[int] = set()
            if pairwise_data:
                for tm in (pairwise_data.best_teammates + pairwise_data.most_played_with):
                    avatar_ids.add(tm.discord_id)
                for opp in pairwise_data.most_played_against:
                    avatar_ids.add(opp.discord_id)
                if pairwise_data.nemesis:
                    avatar_ids.add(pairwise_data.nemesis.discord_id)
                if pairwise_data.punching_bag:
                    avatar_ids.add(pairwise_data.punching_bag.discord_id)

            avatar_cache = await _prefetch_avatars(interaction.guild, avatar_ids)

            # Build unified slide list
            year_label = server_wrapped.year_label
            slides = _build_slides(
                server_wrapped=server_wrapped,
                personal_summary=personal_summary,
                records_wrapped=records_wrapped,
                pairwise_data=pairwise_data,
                package_deal_data=package_deal_data,
                hero_spotlight=hero_spotlight,
                role_breakdown=role_breakdown,
                gamba_data=gamba_data,
                rating_history=rating_history,
                hero_names=self.hero_names,
                target_username=target_user.display_name,
                target_user_id=target_user.id,
                year_label=year_label,
                avatar_cache=avatar_cache,
            )

            if not slides:
                await safe_followup(
                    interaction,
                    content=f"No wrapped data available for {year}.",
                    ephemeral=True,
                )
                return

            # Send unified story view
            view = WrappedStoryView(slides, owner_id=interaction.user.id)
            first_file = await view.render_slide(0)
            msg = await safe_followup(
                interaction,
                content=f"**{year_label}** for {target_user.display_name}",
                file=first_file,
                view=view,
            )
            if msg:
                view.message = msg

        except Exception as e:
            logger.error(f"Error generating wrapped: {e}", exc_info=True)
            await safe_followup(
                interaction,
                content=f"Error generating wrapped: {str(e)}",
                ephemeral=True,
            )


async def setup(bot: commands.Bot):
    """Set up the Wrapped cog."""
    await bot.add_cog(WrappedCog(bot))
