"""
Service for Cama Wrapped monthly summary generation.

Aggregates stats and generates awards for a "Spotify Wrapped" style summary.
"""

import calendar
import json
import logging
import time
from dataclasses import dataclass, field
from datetime import datetime
from typing import TYPE_CHECKING, Any

from config import WRAPPED_MIN_BETS, WRAPPED_MIN_GAMES
from utils.hero_lookup import get_hero_name

if TYPE_CHECKING:
    from repositories.player_repository import PlayerRepository
    from repositories.match_repository import MatchRepository
    from repositories.bet_repository import BetRepository
    from repositories.wrapped_repository import WrappedRepository
    from services.gambling_stats_service import GamblingStatsService

logger = logging.getLogger("cama_bot.services.wrapped")


@dataclass
class Award:
    """A wrapped award/factoid."""

    category: str  # "performance", "rating", "economy", "hero", "fun"
    title: str  # Fun title like "Gold Goblin"
    stat_name: str  # What stat this is for
    stat_value: str  # Formatted value
    discord_id: int
    discord_username: str
    emoji: str = ""
    flavor_text: str = ""


@dataclass
class PlayerWrapped:
    """Personal wrapped summary for a player."""

    discord_id: int
    discord_username: str
    games_played: int
    wins: int
    losses: int
    win_rate: float
    rating_change: int
    # Top heroes
    top_heroes: list[dict] = field(default_factory=list)
    # Awards won
    awards: list[Award] = field(default_factory=list)
    # Betting stats
    total_bets: int = 0
    betting_pnl: int = 0
    degen_score: int | None = None


@dataclass
class ServerWrapped:
    """Server-wide wrapped summary."""

    guild_id: int
    year_month: str
    month_name: str
    # Summary stats
    total_matches: int
    total_wagered: int
    unique_players: int
    unique_heroes: int
    # Awards
    awards: list[Award] = field(default_factory=list)
    # Top performers
    top_players: list[dict] = field(default_factory=list)
    # Most played heroes
    most_played_heroes: list[dict] = field(default_factory=list)
    # Best hero (win rate)
    best_hero: dict | None = None


class WrappedService:
    """Service for generating Cama Wrapped monthly summaries."""

    def __init__(
        self,
        wrapped_repo: "WrappedRepository",
        player_repo: "PlayerRepository",
        match_repo: "MatchRepository",
        bet_repo: "BetRepository",
        gambling_stats_service: "GamblingStatsService | None" = None,
    ):
        self.wrapped_repo = wrapped_repo
        self.player_repo = player_repo
        self.match_repo = match_repo
        self.bet_repo = bet_repo
        self.gambling_stats_service = gambling_stats_service

    def _get_month_timestamps(self, year_month: str) -> tuple[int, int]:
        """
        Get start and end timestamps for a YYYY-MM string.

        Args:
            year_month: Month in "YYYY-MM" format

        Returns:
            (start_timestamp, end_timestamp) as Unix timestamps
        """
        year, month = map(int, year_month.split("-"))
        start = datetime(year, month, 1)
        # Get last day of month
        _, last_day = calendar.monthrange(year, month)
        end = datetime(year, month, last_day, 23, 59, 59)
        return int(start.timestamp()), int(end.timestamp()) + 1

    def was_wrapped_generated(self, guild_id: int, year_month: str) -> bool:
        """Check if wrapped was already generated for a guild/month."""
        record = self.wrapped_repo.get_wrapped(guild_id, year_month)
        return record is not None

    def can_generate_wrapped(self, guild_id: int, year_month: str) -> tuple[bool, str]:
        """
        Check if wrapped can be generated for a guild/month.

        Validates:
        1. The month must be complete (current date is in a later month)
        2. At least 25 days since last wrapped generation for this guild

        Returns:
            (can_generate, reason) tuple
        """
        year, month = map(int, year_month.split("-"))
        now = datetime.now()

        # Check if the month is complete (we must be in a later month)
        if now.year < year or (now.year == year and now.month <= month):
            return False, f"Cannot generate wrapped for {year_month} - month not yet complete"

        # Check if already generated for this specific month
        if self.was_wrapped_generated(guild_id, year_month):
            return False, f"Wrapped already generated for {year_month}"

        # Check cooldown: at least 25 days since last generation for ANY month
        last_gen = self.wrapped_repo.get_last_generation(guild_id)
        if last_gen:
            days_since = (int(time.time()) - last_gen["generated_at"]) / 86400
            if days_since < 25:
                return False, f"Only {days_since:.0f} days since last wrapped generation (need 25+)"

        return True, "OK"

    def mark_wrapped_generated(
        self,
        guild_id: int,
        year_month: str,
        stats: dict,
        channel_id: int | None = None,
        message_id: int | None = None,
        generated_by: int | None = None,
        generation_type: str = "auto",
    ) -> None:
        """Mark wrapped as generated and cache stats."""
        self.wrapped_repo.save_wrapped(
            guild_id=guild_id,
            year_month=year_month,
            stats=stats,
            channel_id=channel_id,
            message_id=message_id,
            generated_by=generated_by,
            generation_type=generation_type,
        )

    def get_cached_wrapped(self, guild_id: int, year_month: str) -> dict | None:
        """Get cached wrapped stats if available."""
        record = self.wrapped_repo.get_wrapped(guild_id, year_month)
        if record and record.get("stats_json"):
            try:
                return json.loads(record["stats_json"])
            except json.JSONDecodeError:
                return None
        return None

    def get_server_wrapped(
        self, guild_id: int, year_month: str, force_regenerate: bool = False
    ) -> ServerWrapped | None:
        """
        Generate server-wide wrapped summary.

        Args:
            guild_id: Discord guild ID
            year_month: Month in "YYYY-MM" format
            force_regenerate: If True, regenerate even if cached

        Returns:
            ServerWrapped object or None if no data
        """
        # Check cache first
        if not force_regenerate:
            cached = self.get_cached_wrapped(guild_id, year_month)
            if cached:
                return self._dict_to_server_wrapped(cached)

        start_ts, end_ts = self._get_month_timestamps(year_month)

        # Get summary stats
        summary = self.wrapped_repo.get_month_summary(guild_id, start_ts, end_ts)
        if not summary or summary.get("total_matches", 0) == 0:
            return None

        # Get detailed stats
        match_stats = self.wrapped_repo.get_month_match_stats(guild_id, start_ts, end_ts)
        hero_stats = self.wrapped_repo.get_month_hero_stats(guild_id, start_ts, end_ts)
        player_heroes = self.wrapped_repo.get_month_player_heroes(guild_id, start_ts, end_ts)
        rating_changes = self.wrapped_repo.get_month_rating_changes(guild_id, start_ts, end_ts)
        betting_stats = self.wrapped_repo.get_month_betting_stats(guild_id, start_ts, end_ts)
        bets_against = self.wrapped_repo.get_month_bets_against_player(guild_id, start_ts, end_ts)
        bankruptcies = self.wrapped_repo.get_month_bankruptcy_count(guild_id, start_ts, end_ts)

        # Generate awards
        awards = self._generate_awards(
            match_stats=match_stats,
            hero_stats=hero_stats,
            player_heroes=player_heroes,
            rating_changes=rating_changes,
            betting_stats=betting_stats,
            bets_against=bets_against,
            bankruptcies=bankruptcies,
        )

        # Get month name
        year, month = map(int, year_month.split("-"))
        month_name = calendar.month_name[month]

        # Build top players (by games played + win rate)
        top_players = []
        for p in match_stats[:10]:
            if p["games_played"] >= WRAPPED_MIN_GAMES:
                wr = p["wins"] / p["games_played"] if p["games_played"] > 0 else 0
                top_players.append(
                    {
                        "discord_id": p["discord_id"],
                        "discord_username": p["discord_username"],
                        "games_played": p["games_played"],
                        "wins": p["wins"],
                        "win_rate": wr,
                    }
                )

        # Build most played heroes
        most_played = []
        for h in hero_stats[:5]:
            wr = h["wins"] / h["picks"] if h["picks"] > 0 else 0
            most_played.append(
                {
                    "hero_id": h["hero_id"],
                    "picks": h["picks"],
                    "win_rate": wr,
                }
            )

        # Find best hero (min 5 games, best win rate)
        best_hero = None
        for h in hero_stats:
            if h["picks"] >= 5:
                wr = h["wins"] / h["picks"]
                if best_hero is None or wr > best_hero.get("win_rate", 0):
                    best_hero = {
                        "hero_id": h["hero_id"],
                        "picks": h["picks"],
                        "wins": h["wins"],
                        "win_rate": wr,
                    }

        wrapped = ServerWrapped(
            guild_id=guild_id,
            year_month=year_month,
            month_name=f"{month_name} {year}",
            total_matches=summary.get("total_matches", 0),
            total_wagered=summary.get("total_wagered", 0),
            unique_players=summary.get("total_players", 0),
            unique_heroes=summary.get("unique_heroes", 0),
            awards=awards,
            top_players=top_players,
            most_played_heroes=most_played,
            best_hero=best_hero,
        )

        # Cache the result
        stats_dict = self._server_wrapped_to_dict(wrapped)
        self.mark_wrapped_generated(
            guild_id=guild_id,
            year_month=year_month,
            stats=stats_dict,
            generation_type="auto",
        )

        return wrapped

    def get_player_wrapped(
        self, discord_id: int, year_month: str, guild_id: int | None = None
    ) -> PlayerWrapped | None:
        """
        Generate personal wrapped summary for a player.

        Args:
            discord_id: Player's Discord ID
            year_month: Month in "YYYY-MM" format
            guild_id: Guild ID for guild-specific stats

        Returns:
            PlayerWrapped object or None if no data
        """
        start_ts, end_ts = self._get_month_timestamps(year_month)

        # Get player info
        player = self.player_repo.get_by_id(discord_id, guild_id)
        if not player:
            return None

        # Query player's match stats for the period
        with self.wrapped_repo.cursor() as cursor:
            cursor.execute(
                """
                SELECT
                    COUNT(DISTINCT m.match_id) as games_played,
                    SUM(CASE WHEN mp.won = 1 THEN 1 ELSE 0 END) as wins,
                    SUM(CASE WHEN mp.won = 0 THEN 1 ELSE 0 END) as losses
                FROM match_participants mp
                JOIN matches m ON mp.match_id = m.match_id
                WHERE mp.discord_id = ?
                  AND m.winning_team IS NOT NULL
                  AND m.match_date >= datetime(?, 'unixepoch')
                  AND m.match_date < datetime(?, 'unixepoch')
                """,
                (discord_id, start_ts, end_ts),
            )
            row = cursor.fetchone()
            if not row or row["games_played"] == 0:
                return None

            games_played = row["games_played"]
            wins = row["wins"] or 0
            losses = row["losses"] or 0

        # Get rating change
        rating_changes = self.wrapped_repo.get_month_rating_changes(0, start_ts, end_ts)
        rating_change = 0
        for rc in rating_changes:
            if rc["discord_id"] == discord_id:
                rating_change = int(rc["rating_change"] or 0)
                break

        # Get top heroes
        player_heroes = self.wrapped_repo.get_month_player_heroes(0, start_ts, end_ts)
        top_heroes = []
        for ph in player_heroes:
            if ph["discord_id"] == discord_id:
                wr = ph["wins"] / ph["picks"] if ph["picks"] > 0 else 0
                top_heroes.append(
                    {
                        "hero_id": ph["hero_id"],
                        "picks": ph["picks"],
                        "wins": ph["wins"],
                        "win_rate": wr,
                    }
                )
        top_heroes = sorted(top_heroes, key=lambda x: x["picks"], reverse=True)[:5]

        # Get betting stats
        betting_stats = self.wrapped_repo.get_month_betting_stats(0, start_ts, end_ts)
        total_bets = 0
        betting_pnl = 0
        for bs in betting_stats:
            if bs["discord_id"] == discord_id:
                total_bets = bs["total_bets"]
                betting_pnl = bs["net_pnl"] or 0
                break

        # Get degen score if available
        degen_score = None
        if self.gambling_stats_service:
            degen = self.gambling_stats_service.calculate_degen_score(discord_id, guild_id)
            if degen:
                degen_score = degen.total

        return PlayerWrapped(
            discord_id=discord_id,
            discord_username=player.name,
            games_played=games_played,
            wins=wins,
            losses=losses,
            win_rate=wins / games_played if games_played > 0 else 0,
            rating_change=rating_change,
            top_heroes=top_heroes,
            awards=[],  # Awards populated by server wrapped
            total_bets=total_bets,
            betting_pnl=betting_pnl,
            degen_score=degen_score,
        )

    def _generate_awards(
        self,
        match_stats: list[dict],
        hero_stats: list[dict],
        player_heroes: list[dict],
        rating_changes: list[dict],
        betting_stats: list[dict],
        bets_against: list[dict],
        bankruptcies: list[dict],
    ) -> list[Award]:
        """Generate all awards from stats data."""
        awards = []

        # Filter to players with minimum games
        eligible_players = [p for p in match_stats if p["games_played"] >= WRAPPED_MIN_GAMES]

        if not eligible_players:
            return awards

        # ============ PERFORMANCE AWARDS (Server-Wide) ============

        # Best GPM
        best_gpm = max(eligible_players, key=lambda x: x.get("avg_gpm") or 0)
        if best_gpm.get("avg_gpm"):
            awards.append(
                Award(
                    category="performance",
                    title="Gold Goblin",
                    stat_name="Best GPM",
                    stat_value=f"{int(best_gpm['avg_gpm'])} GPM",
                    discord_id=best_gpm["discord_id"],
                    discord_username=best_gpm["discord_username"],
                    emoji="💰",
                    flavor_text="Farming simulator champion",
                )
            )

        # Best KDA
        best_kda = max(eligible_players, key=lambda x: x.get("avg_kda") or 0)
        if best_kda.get("avg_kda"):
            awards.append(
                Award(
                    category="performance",
                    title="Immortal Hands",
                    stat_name="Best KDA",
                    stat_value=f"{best_kda['avg_kda']:.2f} KDA",
                    discord_id=best_kda["discord_id"],
                    discord_username=best_kda["discord_username"],
                    emoji="⚔️",
                    flavor_text="Death is beneath them",
                )
            )

        # Worst KDA (fun award)
        worst_kda = min(eligible_players, key=lambda x: x.get("avg_kda") or float("inf"))
        if worst_kda.get("avg_kda") is not None and worst_kda != best_kda:
            awards.append(
                Award(
                    category="performance",
                    title="First Blood Enthusiast",
                    stat_name="Worst KDA",
                    stat_value=f"{worst_kda['avg_kda']:.2f} KDA",
                    discord_id=worst_kda["discord_id"],
                    discord_username=worst_kda["discord_username"],
                    emoji="💀",
                    flavor_text="At least they're consistent",
                )
            )

        # Most wards (supports)
        best_wards = max(eligible_players, key=lambda x: x.get("total_wards") or 0)
        if best_wards.get("total_wards") and best_wards["total_wards"] > 0:
            awards.append(
                Award(
                    category="performance",
                    title="Ward Bot 9000",
                    stat_name="Most Wards",
                    stat_value=f"{best_wards['total_wards']} placed",
                    discord_id=best_wards["discord_id"],
                    discord_username=best_wards["discord_username"],
                    emoji="👁️",
                    flavor_text="Vision wins games",
                )
            )

        # ============ RATING AWARDS (Server-Wide) ============

        if rating_changes:
            # Biggest climb
            biggest_climb = max(rating_changes, key=lambda x: x.get("rating_change") or 0)
            if biggest_climb.get("rating_change") and biggest_climb["rating_change"] > 0:
                awards.append(
                    Award(
                        category="rating",
                        title="Elo Inflation",
                        stat_name="Biggest Climb",
                        stat_value=f"+{int(biggest_climb['rating_change'])} rating",
                        discord_id=biggest_climb["discord_id"],
                        discord_username=biggest_climb["discord_username"],
                        emoji="📈",
                        flavor_text="The grind paid off",
                    )
                )

            # Biggest fall
            biggest_fall = min(rating_changes, key=lambda x: x.get("rating_change") or 0)
            if biggest_fall.get("rating_change") and biggest_fall["rating_change"] < 0:
                awards.append(
                    Award(
                        category="rating",
                        title="The Cliff",
                        stat_name="Biggest Fall",
                        stat_value=f"{int(biggest_fall['rating_change'])} rating",
                        discord_id=biggest_fall["discord_id"],
                        discord_username=biggest_fall["discord_username"],
                        emoji="📉",
                        flavor_text="It's just a number, right?",
                    )
                )

            # Most consistent (lowest variance)
            with_variance = [r for r in rating_changes if r.get("rating_variance") is not None]
            if with_variance:
                most_consistent = min(with_variance, key=lambda x: x["rating_variance"])
                std_dev = int(most_consistent['rating_variance'] ** 0.5) if most_consistent['rating_variance'] else 0
                awards.append(
                    Award(
                        category="rating",
                        title="Steady Eddie",
                        stat_name="Most Consistent",
                        stat_value=f"±{std_dev} rating std dev",
                        discord_id=most_consistent["discord_id"],
                        discord_username=most_consistent["discord_username"],
                        emoji="⚖️",
                        flavor_text="Predictably average",
                    )
                )

                # Most volatile
                most_volatile = max(with_variance, key=lambda x: x["rating_variance"])
                if most_volatile != most_consistent:
                    std_dev = int(most_volatile['rating_variance'] ** 0.5) if most_volatile['rating_variance'] else 0
                    awards.append(
                        Award(
                            category="rating",
                            title="Coin Flip Player",
                            stat_name="Most Volatile",
                            stat_value=f"±{std_dev} rating std dev",
                            discord_id=most_volatile["discord_id"],
                            discord_username=most_volatile["discord_username"],
                            emoji="🎲",
                            flavor_text="Every game is an adventure",
                        )
                    )

        # ============ ECONOMY AWARDS (Server-Wide) ============

        eligible_bettors = [b for b in betting_stats if b["total_bets"] >= WRAPPED_MIN_BETS]

        if eligible_bettors:
            # Best ROI
            for b in eligible_bettors:
                b["roi"] = (b["net_pnl"] / b["total_wagered"]) if b["total_wagered"] > 0 else 0

            best_roi = max(eligible_bettors, key=lambda x: x["roi"])
            if best_roi["roi"] > 0:
                awards.append(
                    Award(
                        category="economy",
                        title="Diamond Hands",
                        stat_name="Best ROI",
                        stat_value=f"+{best_roi['roi'] * 100:.1f}%",
                        discord_id=best_roi["discord_id"],
                        discord_username=best_roi["discord_username"],
                        emoji="💎",
                        flavor_text="The house hates them",
                    )
                )

            # Worst ROI
            worst_roi = min(eligible_bettors, key=lambda x: x["roi"])
            if worst_roi["roi"] < 0 and worst_roi != best_roi:
                awards.append(
                    Award(
                        category="economy",
                        title="House's Favorite",
                        stat_name="Worst ROI",
                        stat_value=f"{worst_roi['roi'] * 100:.1f}%",
                        discord_id=worst_roi["discord_id"],
                        discord_username=worst_roi["discord_username"],
                        emoji="🏠",
                        flavor_text="Thank you for your donation",
                    )
                )

            # High roller (most wagered)
            high_roller = max(eligible_bettors, key=lambda x: x["total_wagered"])
            awards.append(
                Award(
                    category="economy",
                    title="Degen Supreme",
                    stat_name="Most Wagered",
                    stat_value=f"{high_roller['total_wagered']} JC",
                    discord_id=high_roller["discord_id"],
                    discord_username=high_roller["discord_username"],
                    emoji="🎰",
                    flavor_text="All in, every time",
                )
            )

        # Most bankruptcies
        if bankruptcies:
            most_bankrupt = max(bankruptcies, key=lambda x: x["bankruptcy_count"])
            if most_bankrupt["bankruptcy_count"] > 0:
                awards.append(
                    Award(
                        category="economy",
                        title="Bankruptcy Speedrunner",
                        stat_name="Most Bankruptcies",
                        stat_value=f"{most_bankrupt['bankruptcy_count']}x",
                        discord_id=most_bankrupt["discord_id"],
                        discord_username=most_bankrupt["discord_username"],
                        emoji="💸",
                        flavor_text="It's a lifestyle",
                    )
                )

        # ============ HERO AWARDS ============

        # Group player_heroes by player
        player_hero_map: dict[int, list[dict]] = {}
        for ph in player_heroes:
            pid = ph["discord_id"]
            if pid not in player_hero_map:
                player_hero_map[pid] = []
            player_hero_map[pid].append(ph)

        # One-trick (most games on single hero)
        one_tricks = []
        for pid, heroes in player_hero_map.items():
            if heroes:
                top_hero = max(heroes, key=lambda x: x["picks"])
                total_games = sum(h["picks"] for h in heroes)
                one_trick_pct = top_hero["picks"] / total_games if total_games > 0 else 0
                # Find player name
                player_name = None
                for p in match_stats:
                    if p["discord_id"] == pid:
                        player_name = p["discord_username"]
                        break
                if player_name and total_games >= WRAPPED_MIN_GAMES:
                    one_tricks.append(
                        {
                            "discord_id": pid,
                            "discord_username": player_name,
                            "hero_id": top_hero["hero_id"],
                            "picks": top_hero["picks"],
                            "total_games": total_games,
                            "one_trick_pct": one_trick_pct,
                        }
                    )

        if one_tricks:
            biggest_one_trick = max(one_tricks, key=lambda x: x["one_trick_pct"])
            if biggest_one_trick["one_trick_pct"] >= 0.3:  # At least 30% on one hero
                hero_name = get_hero_name(biggest_one_trick["hero_id"]) or f"Hero #{biggest_one_trick['hero_id']}"
                awards.append(
                    Award(
                        category="hero",
                        title="One-Trick Pony",
                        stat_name="Most Dedicated",
                        stat_value=f"{biggest_one_trick['picks']}g on {hero_name}",
                        discord_id=biggest_one_trick["discord_id"],
                        discord_username=biggest_one_trick["discord_username"],
                        emoji="🎠",
                        flavor_text="Comfort zone champion",
                    )
                )

            # Hero pool (most unique heroes)
            hero_pools = [
                {
                    "discord_id": ot["discord_id"],
                    "discord_username": ot["discord_username"],
                    "unique_heroes": len(player_hero_map.get(ot["discord_id"], [])),
                }
                for ot in one_tricks
            ]
            if hero_pools:
                biggest_pool = max(hero_pools, key=lambda x: x["unique_heroes"])
                awards.append(
                    Award(
                        category="hero",
                        title="Jack of All Trades",
                        stat_name="Hero Pool",
                        stat_value=f"{biggest_pool['unique_heroes']} heroes",
                        discord_id=biggest_pool["discord_id"],
                        discord_username=biggest_pool["discord_username"],
                        emoji="🃏",
                        flavor_text="Master of... some?",
                    )
                )

        # ============ FUN/MEME AWARDS ============

        if eligible_players:
            # Iron Man (most games)
            iron_man = max(eligible_players, key=lambda x: x["games_played"])
            awards.append(
                Award(
                    category="fun",
                    title="No Life",
                    stat_name="Most Games",
                    stat_value=f"{iron_man['games_played']} games",
                    discord_id=iron_man["discord_id"],
                    discord_username=iron_man["discord_username"],
                    emoji="🦾",
                    flavor_text="Touch grass? What's that?",
                )
            )

            # Casual (fewest games among eligible)
            casual = min(eligible_players, key=lambda x: x["games_played"])
            if casual != iron_man:
                awards.append(
                    Award(
                        category="fun",
                        title="Touched Grass",
                        stat_name="Fewest Games",
                        stat_value=f"{casual['games_played']} games",
                        discord_id=casual["discord_id"],
                        discord_username=casual["discord_username"],
                        emoji="🌱",
                        flavor_text="Has a life outside Dota",
                    )
                )

        # Punching bag (most bets against)
        if bets_against:
            punching_bag = max(bets_against, key=lambda x: x["bets_against"])
            if punching_bag["bets_against"] >= 3:
                awards.append(
                    Award(
                        category="fun",
                        title="Public Enemy #1",
                        stat_name="Most Bet Against",
                        stat_value=f"{punching_bag['bets_against']} bets",
                        discord_id=punching_bag["discord_id"],
                        discord_username=punching_bag["discord_username"],
                        emoji="🎯",
                        flavor_text="The market has spoken",
                    )
                )

        return awards

    def _server_wrapped_to_dict(self, wrapped: ServerWrapped) -> dict:
        """Convert ServerWrapped to dict for JSON storage."""
        return {
            "guild_id": wrapped.guild_id,
            "year_month": wrapped.year_month,
            "month_name": wrapped.month_name,
            "total_matches": wrapped.total_matches,
            "total_wagered": wrapped.total_wagered,
            "unique_players": wrapped.unique_players,
            "unique_heroes": wrapped.unique_heroes,
            "awards": [
                {
                    "category": a.category,
                    "title": a.title,
                    "stat_name": a.stat_name,
                    "stat_value": a.stat_value,
                    "discord_id": a.discord_id,
                    "discord_username": a.discord_username,
                    "emoji": a.emoji,
                    "flavor_text": a.flavor_text,
                }
                for a in wrapped.awards
            ],
            "top_players": wrapped.top_players,
            "most_played_heroes": wrapped.most_played_heroes,
            "best_hero": wrapped.best_hero,
        }

    def _dict_to_server_wrapped(self, data: dict) -> ServerWrapped:
        """Convert dict from JSON back to ServerWrapped."""
        awards = [
            Award(
                category=a["category"],
                title=a["title"],
                stat_name=a["stat_name"],
                stat_value=a["stat_value"],
                discord_id=a["discord_id"],
                discord_username=a["discord_username"],
                emoji=a.get("emoji", ""),
                flavor_text=a.get("flavor_text", ""),
            )
            for a in data.get("awards", [])
        ]

        return ServerWrapped(
            guild_id=data["guild_id"],
            year_month=data["year_month"],
            month_name=data["month_name"],
            total_matches=data["total_matches"],
            total_wagered=data.get("total_wagered", 0),
            unique_players=data["unique_players"],
            unique_heroes=data["unique_heroes"],
            awards=awards,
            top_players=data.get("top_players", []),
            most_played_heroes=data.get("most_played_heroes", []),
            best_hero=data.get("best_hero"),
        )
