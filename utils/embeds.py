"""
Reusable Discord embed builders.
"""

import discord

from rating_system import CamaRatingSystem
from utils.formatting import ROLE_EMOJIS, ROLE_NAMES, get_player_display_name


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

