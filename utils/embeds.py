"""
Reusable Discord embed builders.
"""

import discord

from rating_system import CamaRatingSystem
from utils.formatting import ROLE_EMOJIS
from utils.hero_lookup import get_hero_image_url, get_hero_name


def format_player_list(players, player_ids):
    """
    Build a formatted lobby player list with ratings and role emojis.

    Deduplicates by Discord ID to avoid double-counting the same user.
    """
    if not players:
        return "No players yet", 0

    rating_system = CamaRatingSystem()

    seen_ids = set()
    unique_players = []
    unique_ids = []

    for player, pid in zip(players, player_ids):
        if pid in seen_ids:
            continue
        seen_ids.add(pid)
        unique_players.append(player)
        unique_ids.append(pid)

    players_with_ratings = []
    for player, pid in zip(unique_players, unique_ids):
        if player.glicko_rating is not None:
            rating = player.glicko_rating
        elif player.mmr is not None:
            rating = rating_system.mmr_to_rating(player.mmr)
        else:
            rating = rating_system.mmr_to_rating(4000)
        players_with_ratings.append((rating, player, pid))

    players_with_ratings.sort(key=lambda x: x[0], reverse=True)

    items = []
    for idx, (rating, player, pid) in enumerate(players_with_ratings, 1):
        # Use Discord mention when we have a real Discord ID; fall back to name for fakes/unknown
        is_real_user = pid is not None and pid >= 0
        display = f"<@{pid}>" if is_real_user else player.name
        name = f"{idx}. {display}"
        if player.glicko_rating is not None:
            cama_rating = rating_system.rating_to_display(player.glicko_rating)
            name += f" [{cama_rating}]"
        if player.preferred_roles:
            role_display = " ".join(ROLE_EMOJIS.get(r, "") for r in player.preferred_roles)
            if role_display:
                name += f" {role_display}"
        items.append(name)

    return "\n".join(items), len(unique_players)


def create_lobby_embed(lobby, players, player_ids, ready_threshold: int = 10):
    """Create the lobby embed with player list and status."""
    player_count = lobby.get_player_count()

    if lobby.created_at:
        timestamp_text = f"Opened at <t:{int(lobby.created_at.timestamp())}:t>"
    else:
        timestamp_text = "Opened just now"

    embed = discord.Embed(
        title="🎮 Matchmaking Lobby",
        description="Join to play!",
        color=discord.Color.green() if player_count >= ready_threshold else discord.Color.blue(),
    )
    embed.set_footer(text=timestamp_text)

    player_list, unique_count = format_player_list(players, player_ids)

    embed.add_field(
        name=f"Players ({player_count}/12)",
        value=player_list if players else "No players yet",
        inline=False,
    )

    if player_count >= ready_threshold:
        embed.add_field(
            name="✅ Ready!",
            value="Anyone can use `/shuffle` to create teams!",
            inline=False,
        )
    else:
        embed.add_field(
            name="Status",
            value="🟢 Open - React with ⚔️ to join!",
            inline=False,
        )

    return embed


def _format_number(n: int | None) -> str:
    """Format large numbers compactly (e.g., 45123 -> '45.1k')."""
    if n is None:
        return "—"
    if n >= 1000:
        return f"{n / 1000:.1f}k"
    return str(n)


def _format_duration(seconds: int | None) -> str:
    """Format duration as MM:SS."""
    if seconds is None or seconds <= 0:
        return "—"
    minutes = seconds // 60
    secs = seconds % 60
    return f"{minutes}:{secs:02d}"


# Lane role mapping from OpenDota (parsed matches only)
LANE_ROLE_NAMES = {
    1: "Safe",
    2: "Mid",
    3: "Off",
    4: "Jungle",
}

# Lane role emojis
LANE_EMOJIS = {
    1: "🛡️",  # Safe Lane
    2: "⚔️",  # Mid
    3: "🗡️",  # Off Lane
    4: "🌲",  # Jungle
}


def _determine_lane_outcomes(
    radiant_participants: list[dict],
    dire_participants: list[dict],
) -> dict[tuple[str, int], str]:
    """
    Determine lane outcomes by comparing average efficiencies between opposing lanes.

    Side lanes have 2 players each (carry+support vs offlaner+support).
    We compare the average efficiency of all players in each lane.

    Lane matchups:
    - Radiant Safe (lane_role=1) vs Dire Off (lane_role=3)
    - Radiant Mid (lane_role=2) vs Dire Mid (lane_role=2)
    - Radiant Off (lane_role=3) vs Dire Safe (lane_role=1)

    Returns:
        Dict mapping (team, participant_idx) to outcome "W"/"L"/"D"
    """
    outcomes: dict[tuple[str, int], str] = {}

    # Group players by lane for each team (multiple players per lane possible)
    def group_by_lane(participants: list[dict]) -> dict[int, list[tuple[int, int]]]:
        """Returns {lane_role: [(idx, efficiency), ...]}"""
        lanes: dict[int, list[tuple[int, int]]] = {}
        for i, p in enumerate(participants):
            lane = p.get("lane_role")
            eff = p.get("lane_efficiency")
            if lane and eff is not None:
                if lane not in lanes:
                    lanes[lane] = []
                lanes[lane].append((i, eff))
        return lanes

    radiant_lanes = group_by_lane(radiant_participants)
    dire_lanes = group_by_lane(dire_participants)

    def avg_efficiency(players: list[tuple[int, int]]) -> float:
        if not players:
            return 0
        return sum(eff for _, eff in players) / len(players)

    # Compare lanes (Radiant lane vs opposing Dire lane)
    matchups = [
        (1, 3),  # Radiant Safe vs Dire Off
        (2, 2),  # Mid vs Mid
        (3, 1),  # Radiant Off vs Dire Safe
    ]

    for rad_lane, dire_lane in matchups:
        rad_players = radiant_lanes.get(rad_lane, [])
        dire_players = dire_lanes.get(dire_lane, [])

        if rad_players and dire_players:
            rad_avg = avg_efficiency(rad_players)
            dire_avg = avg_efficiency(dire_players)

            if rad_avg > dire_avg + 5:  # 5% threshold for "win"
                for idx, _ in rad_players:
                    outcomes[("radiant", idx)] = "W"
                for idx, _ in dire_players:
                    outcomes[("dire", idx)] = "L"
            elif dire_avg > rad_avg + 5:
                for idx, _ in rad_players:
                    outcomes[("radiant", idx)] = "L"
                for idx, _ in dire_players:
                    outcomes[("dire", idx)] = "W"
            else:
                for idx, _ in rad_players:
                    outcomes[("radiant", idx)] = "D"
                for idx, _ in dire_players:
                    outcomes[("dire", idx)] = "D"

    return outcomes


def create_enriched_match_embed(
    match_id: int,
    valve_match_id: int | None,
    duration_seconds: int | None,
    radiant_score: int | None,
    dire_score: int | None,
    winning_team: int,
    radiant_participants: list[dict],
    dire_participants: list[dict],
    show_mvp: bool = True,
) -> discord.Embed:
    """
    Create a rich embed displaying enriched match statistics.

    Args:
        match_id: Internal match ID
        valve_match_id: Dota 2 match ID (for links)
        duration_seconds: Match duration in seconds
        radiant_score: Radiant team kills
        dire_score: Dire team kills
        winning_team: 1 = Radiant won, 2 = Dire won
        radiant_participants: List of participant dicts (with hero_id, kills, etc.)
        dire_participants: List of participant dicts
        show_mvp: Whether to calculate and show MVP

    Returns:
        Discord embed with match statistics
    """
    winner = "Radiant" if winning_team == 1 else "Dire"
    duration_str = _format_duration(duration_seconds)

    # Build title and description
    title = f"Match #{match_id} - {winner} Victory"
    if duration_str != "—":
        title += f" ({duration_str})"

    description_parts = []
    if radiant_score is not None and dire_score is not None:
        description_parts.append(f"**Score:** {radiant_score} - {dire_score}")

    # Add external links
    links = []
    if valve_match_id:
        links.append(f"[OpenDota](https://www.opendota.com/matches/{valve_match_id})")
        links.append(f"[DotaBuff](https://www.dotabuff.com/matches/{valve_match_id})")
        links.append(f"[STRATZ](https://stratz.com/matches/{valve_match_id})")
    if links:
        description_parts.append(" | ".join(links))

    embed = discord.Embed(
        title=title,
        description="\n".join(description_parts) if description_parts else None,
        color=discord.Color.green() if winning_team == 1 else discord.Color.red(),
    )

    # Find MVP (highest damage on winning team)
    mvp = None
    if show_mvp:
        winning_team_players = radiant_participants if winning_team == 1 else dire_participants
        if winning_team_players:
            mvp = max(winning_team_players, key=lambda p: p.get("hero_damage") or 0)

    # Set thumbnail to MVP hero image
    if mvp and mvp.get("hero_id"):
        hero_img = get_hero_image_url(mvp["hero_id"])
        if hero_img:
            embed.set_thumbnail(url=hero_img)

    # Calculate lane outcomes for both teams
    lane_outcomes = _determine_lane_outcomes(radiant_participants, dire_participants)

    def format_team_field(participants: list[dict], team: str, is_winner: bool) -> str:
        """Format a team's stats as embed field value."""
        if not participants:
            return "No data"

        lines = []
        # Header with columns - add Lane column
        lines.append("```")
        lines.append(f"{'Hero':<11} {'KDA':<8} {'DMG':>5} {'NW':>5} {'Lane':>6}")
        lines.append("-" * 41)

        for i, p in enumerate(participants):
            hero = get_hero_name(p.get("hero_id") or 0)
            # Truncate hero name if too long
            if len(hero) > 10:
                hero = hero[:9] + "."

            kills = p.get("kills") or 0
            deaths = p.get("deaths") or 0
            assists = p.get("assists") or 0
            kda = f"{kills}/{deaths}/{assists}"

            dmg = _format_number(p.get("hero_damage"))
            nw = _format_number(p.get("net_worth"))

            # Lane with W/L/D outcome (e.g., "Mid W" or "Safe L")
            lane_role = p.get("lane_role")
            outcome = lane_outcomes.get((team, i))
            if lane_role:
                lane_abbrev = {1: "Safe", 2: "Mid", 3: "Off", 4: "Jgl"}.get(lane_role, "")
                if outcome:
                    lane_str = f"{lane_abbrev} {outcome}"
                else:
                    lane_str = lane_abbrev
            else:
                lane_str = "—"

            line = f"{hero:<11} {kda:<8} {dmg:>5} {nw:>5} {lane_str:>6}"
            lines.append(line)

        lines.append("```")
        return "\n".join(lines)

    # Radiant team field
    radiant_label = "🟢 RADIANT" + (" (Winner)" if winning_team == 1 else "")
    embed.add_field(
        name=radiant_label,
        value=format_team_field(radiant_participants, "radiant", winning_team == 1),
        inline=False,
    )

    # Dire team field
    dire_label = "🔴 DIRE" + (" (Winner)" if winning_team == 2 else "")
    embed.add_field(
        name=dire_label,
        value=format_team_field(dire_participants, "dire", winning_team == 2),
        inline=False,
    )

    # Add MVP footer
    if mvp and mvp.get("hero_id"):
        mvp_hero = get_hero_name(mvp["hero_id"])
        mvp_dmg = _format_number(mvp.get("hero_damage"))
        embed.set_footer(text=f"MVP: {mvp_hero} ({mvp_dmg} damage)")

    return embed


def create_match_summary_embed(
    match_id: int,
    winning_team: int,
    radiant_participants: list[dict],
    dire_participants: list[dict],
    valve_match_id: int | None = None,
) -> discord.Embed:
    """
    Create a simpler match summary embed for non-enriched matches.

    Shows hero and KDA only (no damage/net worth without enrichment).
    """
    winner = "Radiant" if winning_team == 1 else "Dire"

    embed = discord.Embed(
        title=f"Match #{match_id} - {winner} Victory",
        color=discord.Color.green() if winning_team == 1 else discord.Color.red(),
    )

    def format_simple_team(participants: list[dict]) -> str:
        if not participants:
            return "No data"

        lines = []
        for p in participants:
            hero_id = p.get("hero_id")
            if hero_id:
                hero = get_hero_name(hero_id)
                kda = f"{p.get('kills', 0)}/{p.get('deaths', 0)}/{p.get('assists', 0)}"
                lines.append(f"**{hero}** ({kda})")
            else:
                lines.append(f"<@{p.get('discord_id', 0)}>")

        return "\n".join(lines) if lines else "No data"

    embed.add_field(
        name="🟢 Radiant" + (" (Winner)" if winning_team == 1 else ""),
        value=format_simple_team(radiant_participants),
        inline=True,
    )

    embed.add_field(
        name="🔴 Dire" + (" (Winner)" if winning_team == 2 else ""),
        value=format_simple_team(dire_participants),
        inline=True,
    )

    if valve_match_id:
        embed.add_field(
            name="Links",
            value=f"[OpenDota](https://www.opendota.com/matches/{valve_match_id}) | "
            f"[DotaBuff](https://www.dotabuff.com/matches/{valve_match_id})",
            inline=False,
        )

    return embed
