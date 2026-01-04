"""
Match enrichment commands: /setleague, /enrichmatch, /backfillsteamid, /profile, /matchhistory
"""

import logging

import discord
from discord import app_commands
from discord.ext import commands

from services.match_enrichment_service import MatchEnrichmentService
from services.opendota_player_service import OpenDotaPlayerService
from services.permissions import has_admin_permission
from utils.interaction_safety import safe_defer, safe_followup

logger = logging.getLogger("cama_bot.commands.enrichment")


class EnrichmentCommands(commands.Cog):
    """Commands for match enrichment and configuration."""

    def __init__(
        self,
        bot: commands.Bot,
        match_repo,
        player_repo,
        guild_config_repo,
        enrichment_service: MatchEnrichmentService,
        opendota_player_service: OpenDotaPlayerService,
    ):
        self.bot = bot
        self.match_repo = match_repo
        self.player_repo = player_repo
        self.guild_config_repo = guild_config_repo
        self.enrichment_service = enrichment_service
        self.opendota_player_service = opendota_player_service

    @app_commands.command(
        name="setleague", description="Set the Valve league ID for this server (Admin)"
    )
    @app_commands.describe(league_id="The Valve/Dota 2 league ID")
    async def setleague(self, interaction: discord.Interaction, league_id: int):
        """Set the league ID for match discovery."""
        logger.info(f"Setleague command: User {interaction.user.id} setting league to {league_id}")

        if not await safe_defer(interaction, ephemeral=True):
            return

        if not has_admin_permission(interaction):
            await safe_followup(
                interaction,
                content="This command is admin-only.",
                ephemeral=True,
            )
            return

        guild_id = interaction.guild_id
        if not guild_id:
            await safe_followup(
                interaction,
                content="This command must be used in a server.",
                ephemeral=True,
            )
            return

        self.guild_config_repo.set_league_id(guild_id, league_id)
        await safe_followup(
            interaction,
            content=f"League ID set to **{league_id}** for this server.",
            ephemeral=True,
        )
        logger.info(f"League ID set to {league_id} for guild {guild_id}")

    @app_commands.command(
        name="enrichmatch",
        description="Enrich a match with Valve API data (Admin)",
    )
    @app_commands.describe(
        valve_match_id="The Dota 2 match ID (optional - defaults to enriching most recent match)",
        internal_match_id="Our internal match ID (optional - defaults to most recent)",
    )
    async def enrichmatch(
        self,
        interaction: discord.Interaction,
        valve_match_id: int | None = None,
        internal_match_id: int | None = None,
    ):
        """
        Manually enrich a match with Valve API data.

        If valve_match_id is not provided, prompts user to provide it.
        If internal_match_id is not provided, uses the most recent match.
        """
        logger.info(
            f"Enrichmatch command: User {interaction.user.id}, "
            f"valve_match_id={valve_match_id}, internal_match_id={internal_match_id}"
        )

        if not await safe_defer(interaction, ephemeral=True):
            return

        if not has_admin_permission(interaction):
            await safe_followup(
                interaction,
                content="This command is admin-only.",
                ephemeral=True,
            )
            return

        # Determine which internal match to enrich
        if internal_match_id:
            match = self.match_repo.get_match(internal_match_id)
            if not match:
                await safe_followup(
                    interaction,
                    content=f"Match #{internal_match_id} not found.",
                    ephemeral=True,
                )
                return
        else:
            # Use most recent match
            match = self.match_repo.get_most_recent_match()
            if not match:
                await safe_followup(
                    interaction,
                    content="No matches found in the database.",
                    ephemeral=True,
                )
                return
            internal_match_id = match["match_id"]

        # Check if valve_match_id is required
        if not valve_match_id:
            # Check if match already has a valve_match_id
            if match.get("valve_match_id"):
                valve_match_id = match["valve_match_id"]
            else:
                await safe_followup(
                    interaction,
                    content=(
                        f"Match #{internal_match_id} needs a Dota 2 match ID to enrich.\n"
                        f"Use: `/enrichmatch valve_match_id:<DOTA_MATCH_ID>`\n\n"
                        f"You can find the match ID on Dotabuff or in the Dota 2 client."
                    ),
                    ephemeral=True,
                )
                return

        # Perform enrichment
        result = self.enrichment_service.enrich_match(internal_match_id, valve_match_id)

        if not result["success"]:
            await safe_followup(
                interaction,
                content=f"Enrichment failed: {result.get('error', 'Unknown error')}",
                ephemeral=True,
            )
            return

        # Format response
        duration_min = result.get("duration", 0) // 60
        duration_sec = result.get("duration", 0) % 60
        winner = "Radiant" if result.get("radiant_win") else "Dire"

        response = (
            f"Match #{internal_match_id} enriched successfully!\n\n"
            f"**Valve Match ID:** {valve_match_id}\n"
            f"**Duration:** {duration_min}:{duration_sec:02d}\n"
            f"**Score:** Radiant {result.get('radiant_score', 0)} - {result.get('dire_score', 0)} Dire\n"
            f"**Winner:** {winner}\n"
            f"**Players enriched:** {result['players_enriched']}/10"
        )

        if result["players_not_found"]:
            response += (
                f"\n\n*{len(result['players_not_found'])} players not matched (missing steam_id)*"
            )

        await safe_followup(interaction, content=response, ephemeral=True)
        logger.info(f"Match {internal_match_id} enriched: {result['players_enriched']} players")

    @app_commands.command(
        name="backfillsteamid",
        description="Backfill steam_id from dotabuff URLs for all players (Admin)",
    )
    async def backfillsteamid(self, interaction: discord.Interaction):
        """Backfill steam_id for all players who have dotabuff_url but no steam_id."""
        logger.info(f"Backfillsteamid command: User {interaction.user.id}")

        if not await safe_defer(interaction, ephemeral=True):
            return

        if not has_admin_permission(interaction):
            await safe_followup(
                interaction,
                content="This command is admin-only.",
                ephemeral=True,
            )
            return

        result = self.enrichment_service.backfill_steam_ids()

        response = (
            f"Steam ID backfill complete!\n\n**Players updated:** {result['players_updated']}"
        )

        if result["players_failed"]:
            response += (
                f"\n**Failed:** {len(result['players_failed'])} players (invalid dotabuff URLs)"
            )

        await safe_followup(interaction, content=response, ephemeral=True)
        logger.info(
            f"Backfill complete: {result['players_updated']} updated, "
            f"{len(result['players_failed'])} failed"
        )

    @app_commands.command(name="showconfig", description="Show current server configuration")
    async def showconfig(self, interaction: discord.Interaction):
        """Show the current configuration for this server."""
        logger.info(f"Showconfig command: User {interaction.user.id}")

        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild_id
        if not guild_id:
            await safe_followup(
                interaction,
                content="This command must be used in a server.",
                ephemeral=True,
            )
            return

        config = self.guild_config_repo.get_config(guild_id)

        if not config:
            await safe_followup(
                interaction,
                content="No configuration set for this server.\nUse `/setleague` to configure.",
                ephemeral=True,
            )
            return

        league_id = config.get("league_id", "Not set")
        auto_enrich = "Enabled" if config.get("auto_enrich_matches", True) else "Disabled"

        response = (
            f"**Server Configuration**\n\n"
            f"**League ID:** {league_id}\n"
            f"**Auto-enrich matches:** {auto_enrich}"
        )

        await safe_followup(interaction, content=response, ephemeral=True)

    @app_commands.command(
        name="matchhistory",
        description="View recent matches with detailed stats",
    )
    @app_commands.describe(
        user="Player to view history for (defaults to yourself)",
        limit="Number of matches to show (default: 5, max: 10)",
    )
    async def matchhistory(
        self,
        interaction: discord.Interaction,
        user: discord.Member | None = None,
        limit: int = 5,
    ):
        """View recent matches with enriched stats (heroes, KDA, etc)."""
        logger.info(
            f"Matchhistory command: User {interaction.user.id}, "
            f"target={user.id if user else 'self'}, limit={limit}"
        )

        if not await safe_defer(interaction, ephemeral=True):
            return

        target_id = user.id if user else interaction.user.id
        target_name = user.display_name if user else interaction.user.display_name

        # Validate player exists
        player = self.player_repo.get_by_id(target_id)
        if not player:
            await safe_followup(
                interaction,
                content=f"{'That user is' if user else 'You are'} not registered.",
                ephemeral=True,
            )
            return

        # Clamp limit
        limit = max(1, min(limit, 10))

        # Get matches
        matches = self.match_repo.get_player_matches(target_id, limit=limit)
        if not matches:
            await safe_followup(
                interaction,
                content=f"No matches found for {target_name}.",
                ephemeral=True,
            )
            return

        # Format matches as embed
        from utils.hero_lookup import get_hero_name

        # Count wins for color
        wins = sum(1 for m in matches if m["player_won"])
        losses = len(matches) - wins

        embed = discord.Embed(
            title=f"📜 Recent Matches for {target_name}",
            description=f"Last {len(matches)} matches • **{wins}W - {losses}L**",
            color=discord.Color.green()
            if wins > losses
            else discord.Color.red()
            if losses > wins
            else discord.Color.greyple(),
        )

        for match in matches:
            match_id = match["match_id"]
            won = match["player_won"]
            side = match.get("side", "?").capitalize()

            # Get participants for this match
            participants = self.match_repo.get_match_participants(match_id)

            # Find this player's stats
            player_stats = None
            for p in participants:
                if p["discord_id"] == target_id:
                    player_stats = p
                    break

            result_emoji = "✅" if won else "❌"

            valve_match_id = match.get("valve_match_id")
            dotabuff_link = (
                f"[Dotabuff](https://www.dotabuff.com/matches/{valve_match_id})"
                if valve_match_id
                else None
            )

            if player_stats and player_stats.get("hero_id"):
                hero = get_hero_name(player_stats["hero_id"])
                k = player_stats.get("kills", 0)
                d = player_stats.get("deaths", 0)
                a = player_stats.get("assists", 0)
                gpm = player_stats.get("gpm", 0)
                xpm = player_stats.get("xpm", 0)
                dmg = player_stats.get("hero_damage", 0)

                dmg_str = f"{dmg / 1000:.1f}k" if dmg >= 1000 else str(dmg)

                stats_block = (
                    f"```\n"
                    f"KDA:  {k}/{d}/{a}\n"
                    f"GPM:  {gpm}  XPM: {xpm}\n"
                    f"DMG:  {dmg_str}\n"
                    f"Side: {side}\n"
                    f"```"
                )
                if dotabuff_link:
                    stats_block += f"\n{dotabuff_link}"

                embed.add_field(
                    name=f"{result_emoji} #{match_id} • {hero}", value=stats_block, inline=True
                )
            else:
                stats_block = f"```\nKDA:  -/-/-\nGPM:  -    XPM: -\nDMG:  -\nSide: {side}\n```"
                embed.add_field(
                    name=f"{result_emoji} #{match_id} • ???", value=stats_block, inline=True
                )

        await safe_followup(interaction, embed=embed, ephemeral=True)

    @app_commands.command(
        name="profile",
        description="View OpenDota profile stats for a player",
    )
    @app_commands.describe(
        user="Player to view profile for (defaults to yourself)",
    )
    async def profile(
        self,
        interaction: discord.Interaction,
        user: discord.Member | None = None,
    ):
        """View OpenDota profile stats including W/L, avg KDA, top heroes."""
        logger.info(
            f"Profile command: User {interaction.user.id}, target={user.id if user else 'self'}"
        )

        if not await safe_defer(interaction, ephemeral=True):
            return

        target_id = user.id if user else interaction.user.id
        target_name = user.display_name if user else interaction.user.display_name

        # Validate player exists
        player = self.player_repo.get_by_id(target_id)
        if not player:
            await safe_followup(
                interaction,
                content=f"{'That user is' if user else 'You are'} not registered.",
                ephemeral=True,
            )
            return

        # Check for steam_id
        steam_id = self.player_repo.get_steam_id(target_id)
        if not steam_id:
            await safe_followup(
                interaction,
                content=(
                    f"{'That user has' if user else 'You have'} no Steam ID linked.\n"
                    "An admin can run `/backfillsteamid` to populate Steam IDs from Dotabuff URLs."
                ),
                ephemeral=True,
            )
            return

        # Fetch profile
        profile_data = self.opendota_player_service.format_profile_embed(target_id, target_name)

        if not profile_data:
            await safe_followup(
                interaction,
                content="Could not fetch OpenDota profile. The player's profile may be private or the API may be unavailable.",
                ephemeral=True,
            )
            return

        # Build embed
        embed = discord.Embed(
            title=profile_data["title"],
            color=discord.Color.blue(),
        )

        for field in profile_data["fields"]:
            embed.add_field(
                name=field["name"],
                value=field["value"],
                inline=field.get("inline", True),
            )

        # Add Dotabuff link
        if profile_data.get("last_match_id"):
            embed.add_field(
                name="Last Match",
                value=f"[View on Dotabuff](https://www.dotabuff.com/matches/{profile_data['last_match_id']})",
                inline=False,
            )

        embed.set_footer(text=profile_data.get("footer", "Data from OpenDota"))

        await safe_followup(interaction, embed=embed, ephemeral=True)

    # ==================== Admin Discovery Commands ====================

    @app_commands.command(
        name="autodiscover",
        description="[Admin] Auto-discover Dota match IDs for unenriched matches",
    )
    @app_commands.describe(
        dry_run="Preview only, don't apply enrichments (default: False)",
    )
    async def autodiscover(
        self,
        interaction: discord.Interaction,
        dry_run: bool = False,
    ):
        """Auto-discover Dota 2 match IDs by correlating player match histories."""
        if not has_admin_permission(interaction):
            await safe_followup(
                interaction,
                content="This command is admin-only.",
                ephemeral=True,
            )
            return

        logger.info(f"Autodiscover command: User {interaction.user.id}, dry_run={dry_run}")

        if not await safe_defer(interaction, ephemeral=True):
            return

        from services.match_discovery_service import MatchDiscoveryService

        discovery_service = MatchDiscoveryService(self.match_repo, self.player_repo)

        await safe_followup(
            interaction,
            content=f"Starting match discovery (dry_run={dry_run})... This may take a while.",
            ephemeral=True,
        )

        results = discovery_service.discover_all_matches(dry_run=dry_run)

        # Build summary
        lines = [
            f"**Match Discovery {'(DRY RUN)' if dry_run else 'Complete'}**",
            "",
            f"Total unenriched: {results['total_unenriched']}",
            f"Discovered: {results['discovered']}",
            f"Skipped (low confidence): {results['skipped_low_confidence']}",
            f"Skipped (no steam IDs): {results['skipped_no_steam_ids']}",
            f"Errors: {results['errors']}",
        ]

        # Add details for discovered matches
        discovered = [d for d in results["details"] if d["status"] == "discovered"]
        if discovered:
            lines.append("")
            lines.append("**Discovered Matches:**")
            for d in discovered[:10]:  # Limit to 10
                lines.append(
                    f"  #{d['match_id']} → {d['valve_match_id']} "
                    f"({d['confidence']:.0%} - {d['player_count']}/{d['total_players']} players)"
                )
            if len(discovered) > 10:
                lines.append(f"  ... and {len(discovered) - 10} more")

        await interaction.edit_original_response(content="\n".join(lines))

    @app_commands.command(
        name="wipediscovered",
        description="[Admin] Wipe all auto-discovered enrichments",
    )
    async def wipediscovered(self, interaction: discord.Interaction):
        """Clear all enrichments that were auto-discovered, keeping manual ones."""
        if not has_admin_permission(interaction):
            await safe_followup(
                interaction,
                content="This command is admin-only.",
                ephemeral=True,
            )
            return

        logger.info(f"Wipediscovered command: User {interaction.user.id}")

        if not await safe_defer(interaction, ephemeral=True):
            return

        # Get count first
        auto_count = self.match_repo.get_auto_discovered_count()

        if auto_count == 0:
            await safe_followup(
                interaction,
                content="No auto-discovered enrichments to wipe.",
                ephemeral=True,
            )
            return

        # Wipe them
        wiped = self.match_repo.wipe_auto_discovered_enrichments()

        await safe_followup(
            interaction,
            content=f"Wiped {wiped} auto-discovered enrichments. Manual enrichments preserved.",
            ephemeral=True,
        )

    @app_commands.command(
        name="wipematch",
        description="[Admin] Wipe enrichment data for a specific match",
    )
    @app_commands.describe(
        match_id="Internal match ID to wipe enrichment from",
    )
    async def wipematch(self, interaction: discord.Interaction, match_id: int):
        """Clear enrichment data for a specific match."""
        if not has_admin_permission(interaction):
            await safe_followup(
                interaction,
                content="This command is admin-only.",
                ephemeral=True,
            )
            return

        logger.info(f"Wipematch command: User {interaction.user.id}, match_id={match_id}")

        if not await safe_defer(interaction, ephemeral=True):
            return

        success = self.match_repo.wipe_match_enrichment(match_id)

        if success:
            await safe_followup(
                interaction,
                content=f"Wiped enrichment data for match #{match_id}.",
                ephemeral=True,
            )
        else:
            await safe_followup(
                interaction,
                content=f"Match #{match_id} not found.",
                ephemeral=True,
            )


async def setup(bot: commands.Bot):
    """Setup function called when loading the cog."""
    match_repo = getattr(bot, "match_repo", None)
    player_repo = getattr(bot, "player_repo", None)
    guild_config_repo = getattr(bot, "guild_config_repo", None)

    if not all([match_repo, player_repo, guild_config_repo]):
        logger.warning("enrichment cog: required repos not available, skipping")
        return

    enrichment_service = MatchEnrichmentService(match_repo, player_repo)
    opendota_player_service = OpenDotaPlayerService(player_repo)

    await bot.add_cog(
        EnrichmentCommands(
            bot,
            match_repo,
            player_repo,
            guild_config_repo,
            enrichment_service,
            opendota_player_service,
        )
    )
