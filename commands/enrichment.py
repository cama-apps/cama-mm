"""
Match enrichment commands: /setleague, /enrichmatch, /backfillsteamid, /matchhistory,
/dotastats, /viewmatch, /recent, /rolesgraph, /lanegraph
"""

import asyncio
import functools
import logging

import discord
from discord import app_commands
from discord.ext import commands

from services.match_enrichment_service import MatchEnrichmentService
from services.opendota_player_service import OpenDotaPlayerService
from services.permissions import has_admin_permission
from utils.drawing import draw_matches_table
from utils.embeds import _determine_lane_outcomes, create_enriched_match_embed
from utils.interaction_safety import safe_defer, safe_followup

logger = logging.getLogger("cama_bot.commands.enrichment")


class EnrichmentCommands(commands.Cog):
    """Commands for match enrichment and configuration."""

    def __init__(
        self,
        bot: commands.Bot,
        match_service,
        player_service,
        guild_config_service,
        enrichment_service: MatchEnrichmentService,
        opendota_player_service: OpenDotaPlayerService,
        discovery_service=None,
        bankruptcy_repo=None,
    ):
        self.bot = bot
        self.match_service = match_service
        self.player_service = player_service
        self.guild_config_service = guild_config_service
        self.enrichment_service = enrichment_service
        self.opendota_player_service = opendota_player_service
        self.discovery_service = discovery_service
        self.bankruptcy_repo = bankruptcy_repo

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

        self.guild_config_service.set_league_id(guild_id, league_id)
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

        guild_id = interaction.guild.id if interaction.guild else None

        # Determine which internal match to enrich
        if internal_match_id:
            match = self.match_service.get_match_by_id(internal_match_id, guild_id)
            if not match:
                await safe_followup(
                    interaction,
                    content=f"Match #{internal_match_id} not found.",
                    ephemeral=True,
                )
                return
        else:
            # Use most recent match
            match = self.match_service.get_most_recent_match(guild_id)
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

        # Perform enrichment (skip_validation=True for manual admin enrichment)
        result = await asyncio.to_thread(
            functools.partial(self.enrichment_service.enrich_match,
                internal_match_id, valve_match_id, skip_validation=True, guild_id=guild_id)
        )

        if not result["success"]:
            await safe_followup(
                interaction,
                content=f"Enrichment failed: {result.get('error', 'Unknown error')}",
                ephemeral=True,
            )
            return

        # Fetch enriched data for embed
        match_data = self.match_service.get_match_by_id(internal_match_id, guild_id)
        participants = self.match_service.get_match_participants(internal_match_id, guild_id)

        if match_data and participants:
            radiant = [p for p in participants if p.get("side") == "radiant"]
            dire = [p for p in participants if p.get("side") == "dire"]

            embed = create_enriched_match_embed(
                match_id=internal_match_id,
                valve_match_id=valve_match_id,
                duration_seconds=match_data.get("duration_seconds"),
                radiant_score=match_data.get("radiant_score"),
                dire_score=match_data.get("dire_score"),
                winning_team=match_data.get("winning_team", 1),
                radiant_participants=radiant,
                dire_participants=dire,
                bankruptcy_repo=self.bankruptcy_repo,
                lobby_type=match_data.get("lobby_type", "shuffle"),
            )

            # Add summary note
            note = f"Enriched {result['players_enriched']}/10 players"
            if result["players_not_found"]:
                note += f" ({len(result['players_not_found'])} missing steam_id)"

            await safe_followup(interaction, content=note, embed=embed, ephemeral=True)
        else:
            # Fallback to text response
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
                response += f"\n\n*{len(result['players_not_found'])} players not matched (missing steam_id)*"

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

        config = self.guild_config_service.get_config(guild_id)

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
        guild_id = interaction.guild.id if interaction.guild else None

        # Validate player exists
        player = self.player_service.get_player(target_id, guild_id)
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
        matches = self.match_service.get_player_matches(target_id, guild_id, limit=limit)
        if not matches:
            await safe_followup(
                interaction,
                content=f"No matches found for {target_name}.",
                ephemeral=True,
            )
            return

        # Format matches as embed
        from utils.hero_lookup import get_hero_image_url, get_hero_name

        # Lane role names (0 = Roaming is valid but falsy)
        lane_names = {0: "Roam", 1: "Safe", 2: "Mid", 3: "Off", 4: "Jgl"}

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

        # Set thumbnail to most recent hero played
        first_hero_id = None

        for match in matches:
            match_id = match["match_id"]
            won = match["player_won"]
            side = match.get("side", "?").capitalize()

            # Get participants for this match
            participants = self.match_service.get_match_participants(match_id, guild_id)

            # Find this player's stats
            player_stats = None
            for p in participants:
                if p["discord_id"] == target_id:
                    player_stats = p
                    break

            result_emoji = "✅" if won else "❌"
            lobby_type = match.get("lobby_type", "shuffle")
            lobby_emoji = "👑" if lobby_type == "draft" else "🎲"

            valve_match_id = match.get("valve_match_id")
            dotabuff_link = (
                f"[DB](https://www.dotabuff.com/matches/{valve_match_id})"
                if valve_match_id
                else None
            )

            if player_stats and player_stats.get("hero_id"):
                hero_id = player_stats["hero_id"]
                hero = get_hero_name(hero_id)

                # Set thumbnail to first match's hero
                if first_hero_id is None:
                    first_hero_id = hero_id

                k = player_stats.get("kills", 0)
                d = player_stats.get("deaths", 0)
                a = player_stats.get("assists", 0)
                gpm = player_stats.get("gpm", 0)
                dmg = player_stats.get("hero_damage", 0)
                nw = player_stats.get("net_worth", 0)
                tower_dmg = player_stats.get("tower_damage", 0)
                heal = player_stats.get("hero_healing", 0)
                lane_role = player_stats.get("lane_role")

                # Format large numbers compactly
                def fmt(n):
                    return f"{n / 1000:.1f}k" if n and n >= 1000 else str(n or 0)

                dmg_str = fmt(dmg)
                nw_str = fmt(nw)
                td_str = fmt(tower_dmg)
                heal_str = fmt(heal)

                # Determine lane outcome (W/L/D) by comparing with opponents
                lane_str = lane_names.get(lane_role, "")
                if lane_str and player_stats.get("lane_efficiency") is not None:
                    # Split participants by team to calculate lane outcomes
                    radiant = [p for p in participants if p.get("side") == "radiant"]
                    dire = [p for p in participants if p.get("side") == "dire"]

                    lane_outcomes = _determine_lane_outcomes(radiant, dire)

                    # Find player's position in their team's list
                    player_team = "radiant" if side.lower() == "radiant" else "dire"
                    team_list = radiant if player_team == "radiant" else dire
                    team_idx = next(
                        (i for i, p in enumerate(team_list) if p["discord_id"] == target_id),
                        None,
                    )

                    if team_idx is not None:
                        outcome = lane_outcomes.get((player_team, team_idx))
                        if outcome:
                            lane_str = f"{lane_str} {outcome}"

                stats_block = (
                    f"```\n"
                    f"KDA: {k}/{d}/{a}  GPM: {gpm}\n"
                    f"DMG: {dmg_str}  TD: {td_str}\n"
                    f"NW:  {nw_str}  Heal: {heal_str}\n"
                    f"Side: {side}"
                )
                if lane_str:
                    stats_block += f"  Lane: {lane_str}"
                stats_block += "\n```"

                if dotabuff_link:
                    stats_block += f"{dotabuff_link}"

                embed.add_field(
                    name=f"{result_emoji}{lobby_emoji} #{match_id} • {hero}", value=stats_block, inline=True
                )
            else:
                stats_block = f"```\nKDA: -/-/-  GPM: -\nDMG: -  TD: -\nNW: -  Side: {side}\n```"
                embed.add_field(
                    name=f"{result_emoji}{lobby_emoji} #{match_id} • ???", value=stats_block, inline=True
                )

        # Set thumbnail to most recently played hero
        if first_hero_id:
            hero_img = get_hero_image_url(first_hero_id)
            if hero_img:
                embed.set_thumbnail(url=hero_img)

        embed.set_footer(text="Use /viewmatch <id> for detailed match stats")

        await safe_followup(interaction, embed=embed, ephemeral=True)

    @app_commands.command(
        name="viewmatch",
        description="View detailed stats for a specific match",
    )
    @app_commands.describe(
        match_id="Internal match ID to view (defaults to most recent)",
        user="Player whose most recent match to view (if no match_id)",
    )
    async def viewmatch(
        self,
        interaction: discord.Interaction,
        match_id: int | None = None,
        user: discord.Member | None = None,
    ):
        """View detailed stats for a specific match with the enriched embed."""
        target_id = user.id if user else interaction.user.id
        target_name = user.display_name if user else interaction.user.display_name
        guild_id = interaction.guild.id if interaction.guild else None

        logger.info(
            f"Viewmatch command: User {interaction.user.id}, match_id={match_id}, target={target_id}"
        )

        # Not ephemeral so others can see
        if not await safe_defer(interaction, ephemeral=False):
            return

        # Get match - default to target's most recent match
        if match_id:
            match_data = self.match_service.get_match_by_id(match_id, guild_id)
            if not match_data:
                await safe_followup(
                    interaction,
                    content=f"Match #{match_id} not found.",
                )
                return
        else:
            # Try to get target's most recent match first
            user_matches = self.match_service.get_player_matches(target_id, guild_id, limit=1)
            if user_matches:
                match_id = user_matches[0]["match_id"]
                match_data = self.match_service.get_match_by_id(match_id, guild_id)
            else:
                # Fall back to globally most recent
                match_data = self.match_service.get_most_recent_match(guild_id)

            if not match_data:
                await safe_followup(
                    interaction,
                    content=f"No matches found for {target_name}." if user else "No matches found.",
                )
                return
            match_id = match_data["match_id"]

        # Get participants
        participants = self.match_service.get_match_participants(match_id, guild_id)

        if not participants:
            await safe_followup(
                interaction,
                content=f"No participant data for match #{match_id}.",
            )
            return

        radiant = [p for p in participants if p.get("side") == "radiant"]
        dire = [p for p in participants if p.get("side") == "dire"]

        # Check if enriched
        is_enriched = any(p.get("hero_id") for p in participants)

        if is_enriched:
            embed = create_enriched_match_embed(
                match_id=match_id,
                valve_match_id=match_data.get("valve_match_id"),
                duration_seconds=match_data.get("duration_seconds"),
                radiant_score=match_data.get("radiant_score"),
                dire_score=match_data.get("dire_score"),
                winning_team=match_data.get("winning_team", 1),
                radiant_participants=radiant,
                dire_participants=dire,
                bankruptcy_repo=self.bankruptcy_repo,
                lobby_type=match_data.get("lobby_type", "shuffle"),
            )

            # Generate and attach stats image
            from utils.hero_lookup import get_hero_image_url

            # If user was in this match, show their hero as thumbnail
            user_participant = None
            for p in participants:
                if p.get("discord_id") == interaction.user.id:
                    user_participant = p
                    break

            if user_participant and user_participant.get("hero_id"):
                hero_img = get_hero_image_url(user_participant["hero_id"])
                if hero_img:
                    # Override MVP thumbnail with user's hero
                    embed.set_thumbnail(url=hero_img)

            await safe_followup(interaction, embed=embed)
        else:
            # Not enriched - show basic info
            from utils.embeds import create_match_summary_embed

            embed = create_match_summary_embed(
                match_id=match_id,
                winning_team=match_data.get("winning_team", 1),
                radiant_participants=radiant,
                dire_participants=dire,
                valve_match_id=match_data.get("valve_match_id"),
                bankruptcy_repo=self.bankruptcy_repo,
                lobby_type=match_data.get("lobby_type", "shuffle"),
            )
            note = (
                "ℹ️ This match has not been enriched yet. Use `/enrichmatch` to add detailed stats."
            )
            await safe_followup(interaction, content=note, embed=embed)

    # ==================== Admin Discovery Commands ====================

    @app_commands.command(
        name="autodiscover",
        description="[Admin] Auto-discover Dota match IDs for unenriched matches",
    )
    @app_commands.describe(
        dry_run="Preview only, don't apply enrichments (default: False)",
        refill_fantasy="Re-enrich matches that have enrichment but no fantasy data",
    )
    async def autodiscover(
        self,
        interaction: discord.Interaction,
        dry_run: bool = False,
        refill_fantasy: bool = False,
    ):
        """Auto-discover Dota 2 match IDs by correlating player match histories."""
        if not has_admin_permission(interaction):
            await safe_followup(
                interaction,
                content="This command is admin-only.",
                ephemeral=True,
            )
            return

        logger.info(
            f"Autodiscover command: User {interaction.user.id}, "
            f"dry_run={dry_run}, refill_fantasy={refill_fantasy}"
        )

        if not await safe_defer(interaction, ephemeral=True):
            return

        # Handle refill_fantasy mode - re-enrich matches that have valve_match_id but no fantasy data
        if refill_fantasy:
            guild_id = interaction.guild_id
            await self._refill_fantasy_data(interaction, dry_run, guild_id)
            return

        if not self.discovery_service:
            await safe_followup(
                interaction,
                content="Match discovery service not available.",
                ephemeral=True,
            )
            return

        discovery_service = self.discovery_service

        await safe_followup(
            interaction,
            content=f"Starting match discovery (dry_run={dry_run})... This may take a while.",
            ephemeral=True,
        )

        guild_id = interaction.guild_id
        results = discovery_service.discover_all_matches(guild_id=guild_id, dry_run=dry_run)

        # Build summary
        lines = [
            f"**Match Discovery {'(DRY RUN)' if dry_run else 'Complete'}**",
            "",
            f"Total unenriched: {results['total_unenriched']}",
            f"Discovered: {results['discovered']}",
            f"Skipped (low confidence): {results['skipped_low_confidence']}",
            f"Skipped (validation failed): {results.get('skipped_validation_failed', 0)}",
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

        # Show low confidence matches that need manual attention
        low_confidence = [
            d for d in results["details"]
            if d["status"] == "low_confidence" and d.get("best_valve_match_id")
        ]
        if low_confidence:
            lines.append("")
            lines.append("**⚠️ Low Confidence (needs manual review):**")
            for d in low_confidence[:5]:
                lines.append(
                    f"  #{d['match_id']} → {d['best_valve_match_id']} "
                    f"({d['confidence']:.0%} - {d['player_count']}/{d['total_players']} players)"
                )
            if len(low_confidence) > 5:
                lines.append(f"  ... and {len(low_confidence) - 5} more")
            lines.append("*Use `/enrichmatch` to manually enrich these matches*")

        # Show validation failures
        validation_failed = [
            d for d in results["details"]
            if d["status"] == "validation_failed"
        ]
        if validation_failed:
            lines.append("")
            lines.append("**❌ Validation Failed:**")
            for d in validation_failed[:5]:
                lines.append(
                    f"  #{d['match_id']} → {d.get('best_valve_match_id', '?')}: {d.get('validation_error', 'Unknown')}"
                )
            if len(validation_failed) > 5:
                lines.append(f"  ... and {len(validation_failed) - 5} more")

        await interaction.edit_original_response(content="\n".join(lines))

    async def _refill_fantasy_data(
        self, interaction: discord.Interaction, dry_run: bool, guild_id: int | None = None
    ):
        """Re-enrich matches that have enrichment but no fantasy data."""
        matches = self.match_service.get_matches_without_fantasy_data(limit=100)

        if not matches:
            await safe_followup(
                interaction,
                content="No matches need fantasy data refill. All enriched matches have fantasy points.",
                ephemeral=True,
            )
            return

        await safe_followup(
            interaction,
            content=f"Found {len(matches)} matches without fantasy data. {'Previewing...' if dry_run else 'Re-enriching...'}",
            ephemeral=True,
        )

        refilled = []
        errors = []

        for match in matches:
            match_id = match["match_id"]
            valve_match_id = match["valve_match_id"]

            if dry_run:
                refilled.append({"match_id": match_id, "valve_match_id": valve_match_id})
            else:
                try:
                    # Re-enrich with skip_validation since we already have the correct valve_match_id
                    result = await asyncio.to_thread(
                        functools.partial(self.enrichment_service.enrich_match,
                            match_id, valve_match_id, source="manual", skip_validation=True,
                            guild_id=guild_id)
                    )
                    if result["success"]:
                        refilled.append({
                            "match_id": match_id,
                            "valve_match_id": valve_match_id,
                            "fantasy_calculated": result.get("fantasy_points_calculated", False),
                        })
                    else:
                        errors.append({"match_id": match_id, "error": result.get("error", "Unknown")})
                except Exception as e:
                    errors.append({"match_id": match_id, "error": str(e)})

        # Build response
        lines = [
            f"**Fantasy Data Refill {'(DRY RUN)' if dry_run else 'Complete'}**",
            "",
            f"Matches processed: {len(matches)}",
            f"Successfully refilled: {len(refilled)}",
            f"Errors: {len(errors)}",
        ]

        if refilled and not dry_run:
            lines.append("")
            lines.append("**Refilled:**")
            for r in refilled[:10]:
                lines.append(f"  #{r['match_id']} → {r['valve_match_id']}")
            if len(refilled) > 10:
                lines.append(f"  ... and {len(refilled) - 10} more")

        if errors:
            lines.append("")
            lines.append("**Errors:**")
            for e in errors[:5]:
                lines.append(f"  #{e['match_id']}: {e['error']}")

        await interaction.edit_original_response(content="\n".join(lines))

    @app_commands.command(
        name="wipediscovered",
        description="[Admin] Wipe all match enrichments",
    )
    async def wipediscovered(self, interaction: discord.Interaction):
        """Clear all match enrichments so they can be re-discovered."""
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

        guild_id = interaction.guild_id
        enriched_count = self.match_service.get_enriched_count(guild_id)

        if enriched_count == 0:
            await safe_followup(
                interaction,
                content="No enrichments to wipe.",
                ephemeral=True,
            )
            return

        wiped = self.match_service.wipe_all_enrichments(guild_id)

        await safe_followup(
            interaction,
            content=f"Wiped {wiped} enrichments. Run `/autodiscover` to re-enrich.",
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

        success = self.match_service.wipe_match_enrichment(match_id)

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

    @app_commands.command(
        name="recent",
        description="View your recent Dota 2 matches as an image",
    )
    @app_commands.describe(
        user="User to show matches for (defaults to yourself)",
        limit="Number of matches to show (default 10, max 20)",
    )
    async def recent(
        self,
        interaction: discord.Interaction,
        user: discord.Member | None = None,
        limit: int = 10,
    ):
        """Display recent matches as an image table."""
        if not await safe_defer(interaction):
            return

        target = user or interaction.user
        target_id = target.id
        target_name = target.display_name

        # Validate limit
        limit = max(1, min(20, limit))

        logger.info(
            f"Recent command: User {interaction.user.id} viewing {target_id}, limit={limit}"
        )

        matches = await asyncio.to_thread(self.opendota_player_service.get_recent_matches_detailed, target_id, limit=limit)

        if not matches:
            await safe_followup(
                interaction,
                content=f"{target_name} hasn't linked their Steam account. Use `/linksteam` first.",
            )
            return

        # Generate image
        try:
            image_bytes = await asyncio.to_thread(draw_matches_table, matches)
            file = discord.File(image_bytes, filename="recent_matches.png")

            embed = discord.Embed(
                title=f"Recent Matches: {target_name}",
                color=discord.Color.blue(),
            )
            embed.set_image(url="attachment://recent_matches.png")
            embed.set_footer(text=f"Showing {len(matches)} most recent matches")

            await safe_followup(interaction, embed=embed, file=file)
        except Exception as e:
            logger.error(f"Error generating recent matches image: {e}")
            await safe_followup(
                interaction,
                content="Failed to generate match image. Please try again.",
            )


async def setup(bot: commands.Bot):
    """Setup function called when loading the cog."""
    match_service = getattr(bot, "match_service", None)
    player_service = getattr(bot, "player_service", None)
    guild_config_service = getattr(bot, "guild_config_service", None)
    enrichment_service = getattr(bot, "match_enrichment_service", None)
    opendota_player_service = getattr(bot, "opendota_player_service", None)
    discovery_service = getattr(bot, "match_discovery_service", None)
    bankruptcy_repo = getattr(bot, "bankruptcy_repo", None)

    if not all([match_service, player_service, guild_config_service]):
        logger.warning("enrichment cog: required services not available, skipping")
        return

    await bot.add_cog(
        EnrichmentCommands(
            bot,
            match_service,
            player_service,
            guild_config_service,
            enrichment_service,
            opendota_player_service,
            discovery_service,
            bankruptcy_repo,
        )
    )
