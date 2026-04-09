"""
Service for Cama Wrapped yearly summary generation.

Aggregates stats and generates awards for a "Spotify Wrapped" style year-in-review.
"""

import json
import logging
import random
from dataclasses import dataclass, field
from datetime import UTC, datetime
from typing import TYPE_CHECKING

from config import WRAPPED_MIN_BETS, WRAPPED_MIN_GAMES
from utils.hero_lookup import get_hero_name

if TYPE_CHECKING:
    from repositories.bet_repository import BetRepository
    from repositories.match_repository import MatchRepository
    from repositories.pairings_repository import PairingsRepository
    from repositories.player_repository import PlayerRepository
    from repositories.wrapped_repository import WrappedRepository
    from services.gambling_stats_service import GamblingStatsService
    from services.package_deal_service import PackageDealService

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
class PersonalSummaryWrapped:
    """Personal summary stats for the wrapped story slides."""

    discord_id: int
    discord_username: str
    games_played: int
    wins: int
    losses: int
    win_rate: float
    rating_change: int
    total_kills: int
    total_deaths: int
    total_assists: int
    avg_game_duration: int  # seconds
    unique_heroes: int
    # Percentiles (0-100, compared against all registered server players)
    games_played_percentile: float
    win_rate_percentile: float
    kda_percentile: float
    unique_heroes_percentile: float
    total_kda_percentile: float  # total kills+assists vs population
    flavor_text: str = ""


@dataclass
class PairwiseEntry:
    """A single pairwise teammate/rival entry."""

    discord_id: int
    username: str
    games: int
    wins: int
    win_rate: float


@dataclass
class PairwiseWrapped:
    """Pairwise teammate and rival data for wrapped."""

    best_teammates: list[PairwiseEntry] = field(default_factory=list)
    most_played_with: list[PairwiseEntry] = field(default_factory=list)
    nemesis: PairwiseEntry | None = None  # worst matchup opponent
    punching_bag: PairwiseEntry | None = None  # best matchup opponent
    most_played_against: list[PairwiseEntry] = field(default_factory=list)


@dataclass
class PackageDealWrapped:
    """Anonymized package deal stats for wrapped."""

    times_bought: int = 0  # How many deals the player purchased
    times_bought_on_you: int = 0  # How many deals others purchased on the player
    unique_buyers: int = 0  # How many distinct people bought a deal on the player
    jc_spent: int = 0  # Total JC the player spent buying deals
    jc_spent_on_you: int = 0  # Total JC others spent on deals involving the player
    total_games_committed: int = 0  # Total games across all deals


@dataclass
class HeroSpotlightWrapped:
    """Hero spotlight data for wrapped."""

    top_hero_name: str = ""
    top_hero_picks: int = 0
    top_hero_wins: int = 0
    top_hero_win_rate: float = 0.0
    top_3_heroes: list[dict] = field(default_factory=list)  # [{name, picks, wins, win_rate}]
    unique_heroes: int = 0


@dataclass
class RoleBreakdownWrapped:
    """Lane breakdown data for wrapped (from OpenDota lane_role)."""

    lane_freq: dict[int, int] = field(default_factory=dict)  # lane_role -> count
    total_games: int = 0


@dataclass
class PersonalRecord:
    """A single personal record (best or worst)."""

    stat_key: str  # e.g. "kills_best"
    stat_label: str  # e.g. "Most Kills" or "Feeding Frenzy"
    value: float | int | None
    display_value: str  # e.g. "25 kills"
    hero_id: int | None
    match_id: int | None
    valve_match_id: int | None
    match_date: str | None
    is_worst: bool = False


# Slide definitions for personal records: (title, color_key, stat_keys)
RECORDS_SLIDE_DEFS: list[tuple[str, str, list[str]]] = [
    ("Combat", "combat", [
        "kills_best", "assists_best", "kda_best",
        "deaths_worst", "kda_worst", "kill_participation_best",
    ]),
    ("Farming", "farming", [
        "gpm_best", "xpm_best", "last_hits_best", "denies_best",
        "gpm_worst", "xpm_worst", "last_hits_worst",
    ]),
    ("Impact", "impact", [
        "hero_damage_best", "tower_damage_best", "towers_killed_best",
        "hero_healing_best", "comeback_best", "throw_worst",
    ]),
    ("Vision & Utility", "vision", [
        "obs_placed_best", "sen_placed_best", "stuns_best",
        "courier_kills_best", "pings_worst", "apm_best",
    ]),
    ("Endurance & Streaks", "endurance", [
        "longest_game", "shortest_game",
        "win_streak_best", "lose_streak_worst", "rapiers_best",
    ]),
]


@dataclass
class PersonalRecordsWrapped:
    """Personal records wrapped for a player."""

    discord_id: int
    discord_username: str
    year: int
    year_label: str  # e.g. "Cama Wrapped 2026"
    games_played: int
    records: list[PersonalRecord] = field(default_factory=list)

    def get_slides(self) -> list[tuple[str, str, list["PersonalRecord"]]]:
        """Returns [(slide_title, color_key, records), ...]"""
        record_map = {r.stat_key: r for r in self.records}
        slides = []
        for title, color_key, stat_keys in RECORDS_SLIDE_DEFS:
            slide_records = [record_map[k] for k in stat_keys if k in record_map]
            if slide_records:
                slides.append((title, color_key, slide_records))
        return slides


@dataclass
class ServerWrapped:
    """Server-wide wrapped summary."""

    guild_id: int
    year: int
    year_label: str
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


# ============ FLAVOR TEXT POOLS ============

FLAVOR_POOLS: dict[str, list[str]] = {
    # Story slides - games played
    "games_played_high": [
        "Your queue button has a restraining order against you",
        "Dota is your full-time job and it doesn't even pay",
        "You have played more Dota than some people have worked",
        "The client remembers you by name at this point",
        "We checked, you have no other hobbies",
        "Your matchmaking queue has its own reserved parking spot",
        "The ancient personally asked you to take a break",
        "Even Gaben thinks you should go outside",
        "Your Steam profile says 'Last Online: Never Offline'",
        "More games than a TI group stage",
        "Roshan respawns faster than you re-queue",
        "You've seen more creep waves than the ocean",
    ],
    "games_played_mid": [
        "A respectable grind",
        "Not a casual, not yet a degenerate",
        "The sweet spot between fun and concerning",
        "Balanced, as all things should be",
        "Your social life still has a pulse",
        "You haven't lost yourself. Yet.",
        "Enough games to have opinions, not enough to be insufferable",
        "The Goldilocks zone of Dota addiction",
        "Like a Midas — steady and reliable",
        "Your queue discipline rivals a pos 5 player",
    ],
    "games_played_low": [
        "Did you even play this year?",
        "The lobby barely knows you exist",
        "Touching grass is NOT a legitimate strategy",
        "Casual? Or just scared?",
        "Your absence was noted. And appreciated.",
        "You came, you saw, you queued... once",
        "Rarer than a Roshan Aghs drop",
        "You play Dota like it's a seasonal event",
        "Your hero pool is bigger than your game count",
        "Even couriers have more map time than you",
    ],
    # Win rate
    "win_rate_high": [
        "The team diff was always you",
        "Someone check if they're scripting",
        "Built different, apparently",
        "Winning is a personality trait at this point",
        "The lobby's main character",
        "You hit different, like a Daedalus crit",
        "Enemies see your name and pick defensive lineups",
        "Your win rate is higher than most heroes' pick rates",
        "Basically smurfing on a main account",
        "The Aegis keeps choosing you",
        "Even your losses feel like moral victories",
    ],
    "win_rate_mid": [
        "Perfectly balanced... painfully average",
        "Coin flip merchant",
        "The matchmaker's dream player",
        "You are exactly where you belong",
        "The definition of mid",
        "Balanced like a 50/50 Roshan fight",
        "Wins and losses cancel out like a Heart toggle",
        "The matchmaker nods approvingly",
        "Flatter than a creep equilibrium line",
        "50% win rate energy — the system working as intended",
    ],
    "win_rate_low": [
        "We appreciate your service to the other team",
        "They should name a charity after you",
        "At least you're consistent... at losing",
        "Have you considered a different game?",
        "Your W key must be broken",
        "You feed more than a Hand of Midas",
        "The enemy carry sends you thank-you cards",
        "Your throne is basically on a timer",
        "Even Techies has a better win rate",
        "Losing is a strategy. Yours, apparently.",
    ],
    # Teammate flavors
    "teammate_best": [
        "Your carry and your crutch",
        "The duo queue energy is unmistakable",
        "Win together, lose apart",
        "Package deal material right here",
        "When you're on the same team, it's just not fair",
        "Better synergy than Tiny-Io",
        "The Wisp to your Chaos Knight",
        "Lane partners in crime",
        "Two-hero wombo combo potential",
        "Chemistry so good it breaks the shuffler",
    ],
    "teammate_worst": [
        "Some bonds are forged in shared suffering",
        "Together you form a black hole of MMR",
        "The shuffler keeps pairing you two as a warning",
        "Your combined win rate violates the Geneva Convention",
        "Like a dual offlane — bold, but why?",
        "The anti-synergy is almost impressive",
        "Together you hit like a Quelling Blade on heroes",
    ],
    # Rival flavors
    "rival_nemesis": [
        "They live rent-free in your head",
        "Every time you see their name, you feel pain",
        "The rivalry that writes itself",
        "You owe them at least one keyboard replacement",
        "Matchmaking's favorite comedy duo",
        "They counter-pick you in real life",
        "Your personal Viper lane matchup",
        "Even Roshan respects this rivalry",
        "You two are like Radiant and Dire — eternally opposed",
        "BKB doesn't block their mental damage to you",
    ],
    "rival_punching_bag": [
        "Free real estate",
        "They see your name and alt-F4",
        "You own them and everyone knows it",
        "Not even close. Every single time.",
        "Your personal content creator",
        "They're your lane creep and don't even know it",
        "You farm them like a jungle stack",
        "Their name in the lobby is basically a free win",
        "You counter them harder than Meepo counters low MMR",
        "They need a BKB just to handle being on your screen",
    ],
    # Hero spotlight
    "hero_onetrick": [
        "Why learn other heroes when this one just works?",
        "Comfort zone? More like comfort CASTLE",
        "The one-trick pony gallops on",
        "Banning this hero is basically banning you",
        "You and this hero share a Steam account at this point",
        "First pick, every pick, only pick",
        "Your hero pool is a hero puddle",
        "Dotabuff has you listed as this hero's emergency contact",
        "If this hero gets nerfed, you get nerfed",
        "Even Invoker mains have more variety",
    ],
    "hero_diverse": [
        "Jack of all trades, master of... some?",
        "Your hero pool is an ocean",
        "The versatility is almost concerning",
        "Can you even remember all their abilities?",
        "Last-pick flexibility incarnate",
        "You've played more heroes than most people know exist",
        "Your hero grid is a rainbow",
        "The ultimate flex pick teammate",
        "Draft phase doesn't scare you — it excites you",
        "You've touched more heroes than a Rubick main",
    ],
    # Package deals
    "package_deal": [
        "Friendship has a price. You paid it.",
        "Contractually obligated to play together",
        "The ultimate duo queue energy",
        "Your bond has been legally notarized",
        "Bound by JC and shuffler RNG",
        "A sacred pact sealed in Jopacoins",
        "Friendship isn't free — it costs exactly this much",
        "The shuffler recognizes this arrangement",
        "Duo lane? More like duo LIFE.",
        "Even Io's tether isn't this committed",
    ],
    # Awards
    "award_gold_goblin": [
        "Farming simulator champion",
        "The creeps file taxes in your name",
        "Every gold coin trembles in your presence",
        "Your GPM makes accountants weep with joy",
        "Alchemist called — he wants his farming techniques back",
        "Last-hitting is a love language and you're fluent",
        "Your Midas is metaphorical AND literal",
        "The neutral camps filed a restraining order",
        "Anti-Mage looks at your farm and cries",
        "Gold doesn't grow on trees — it grows on your hero",
    ],
    "award_immortal_hands": [
        "Death is beneath them",
        "KDA so clean you could eat off it",
        "Guardian angel in mortal form",
        "Harder to kill than a Bristleback running away",
        "Even Necrophos can't find them in the death timer",
        "Wraith King wishes he had this survival rate",
        "Their buyback gold is basically a savings account",
        "The fountain is a foreign concept to them",
        "They dodge death like a Puck with Phase Shift",
        "Aegis? They don't need an Aegis.",
    ],
    "award_first_blood_enthusiast": [
        "At least they're consistent",
        "Dying is a form of space creation",
        "Their KDA exists as a cautionary tale",
        "Feeding is just aggressive map control",
        "The grey screen is their second monitor",
        "Bloodstone charges? Negative.",
        "They create space... in the respawn timer",
        "Their death count has its own highlight reel",
        "Even Aegis can't save this KDA",
        "The enemy's net worth graph thanks them personally",
    ],
    "award_ward_bot": [
        "Vision wins games",
        "The real MVP wears a support badge",
        "If wards gave frequent flyer miles...",
        "Their minimap is brighter than their future",
        "Observer wards cost 0 gold and they STILL outspend everyone",
        "Dewarding is their love language",
        "They see everything — like Zeus ult but permanent",
        "Nightstalker hates this one player",
        "Even Clockwerk's Rocket Flare provides less vision",
        "The fog of war fears them",
    ],
    "award_elo_inflation": [
        "The grind paid off",
        "Rating go brrrr",
        "They can't keep getting away with this",
        "Climbing faster than a Broodmother up a cliff",
        "Their MMR trajectory is a Pudge hook — straight up",
        "Calibration was just the beginning",
        "From Herald to legend... literally",
        "The climb was steeper than high ground, but they took it",
        "Even Icefrog couldn't nerf this progression",
    ],
    "award_the_cliff": [
        "It's just a number, right?",
        "The descent was almost artistic",
        "Terminal velocity achieved",
        "Fell harder than a cliff-jungling Nature's Prophet",
        "Their rating graph is a Pudge hook — straight down",
        "Even Buyback couldn't save this trajectory",
        "Recalibration waiting room",
        "The matchmaker sent a sympathy card",
        "Dropped faster than a Divine Rapier on death",
    ],
    "award_diamond_hands": [
        "The house hates them",
        "Even the bookie tips their hat",
        "Professional gambler (don't tell HR)",
        "Printing JC like it's Alchemist's passive",
        "Better market reads than a Chen with Aghs",
        "Their portfolio is more stacked than a Medusa at 50 min",
        "The Oracle of Jopacoins",
        "Gambler's fallacy? Never heard of it. They just win.",
    ],
    "award_house_favorite": [
        "Thank you for your donation",
        "The bookie's best customer",
        "Subsidizing everyone else's winnings since day 1",
        "Their JC keeps the economy alive",
        "Donating to the house like a Bounty Hunter donating Track gold",
        "The economy's unsung hero",
        "Without them, the house would go bankrupt",
        "They bet like they play — with reckless abandon",
    ],
    "award_degen_supreme": [
        "All in, every time",
        "Gambling is a personality trait at this point",
        "The casino sends you Christmas cards",
        "Bets on everything including the coin flip",
        "Their gambling history is longer than a Techies game",
        "The Degen Score was invented because of them",
        "Divine Rapier of gambling — all or nothing",
        "Even Ogre Magi's Multicast RNG is more responsible",
    ],
    "award_one_trick": [
        "Comfort zone champion",
        "Ban this hero and watch them crumble",
        "One hero to rule them all",
        "Their hero pool is a hero puddle",
        "First pick, last pick, only pick",
        "Dotabuff thinks they only own one hero",
        "This hero is their entire personality",
        "They've memorized every voice line. All of them.",
    ],
    "award_no_life": [
        "Touch grass? What's that?",
        "The queue button has PTSD from you",
        "Do you have a job? Genuine question",
        "More games than TI has prize pool dollars",
        "They queue like Axe spins — constantly",
        "Their Steam hours would make a speedrunner blush",
        "Even the Ancients ask for a break",
        "Dota IS the social life",
    ],
    "award_punching_bag": [
        "The market has spoken",
        "Betting against them is basically free money",
        "Professional underdog",
        "The community's most trusted fade target",
        "Their name on the bet slip means 'easy money'",
        "Even Bounty Hunter tracks them for the payout",
        "The bookies' favorite content",
        "Losing is an art form and they're Picasso",
    ],
    "award_steady_eddie": [
        "Predictably average",
        "The metronome of matchmaking",
        "Consistency is its own reward",
        "Stable as a Tidehunter in Anchor Smash range",
        "Their performance graph is a flat line — in a good way?",
        "Neither feast nor famine — the anti-Alchemist",
        "Reliable like a Treant heal",
        "The BKB of players — solid, dependable, not flashy",
    ],
    "award_coin_flip": [
        "Every game is an adventure",
        "Schrödinger's teammate",
        "You never know which version shows up",
        "Volatile like a Chaos Knight with max phantasms",
        "Today: Immortal. Tomorrow: Herald. No in-between.",
        "Their form chart looks like a Puck's orb path",
        "Ogre Magi Multicast energy — sometimes 4x, sometimes 0x",
        "Even Faceless Void can't predict their next game",
    ],
    "award_bankruptcy_speedrunner": [
        "It's a lifestyle",
        "Financial ruin any% world record",
        "The bank has you on speed dial",
        "They speedrun bankruptcy like others speedrun Meepo combos",
        "Their JC balance respawns as fast as they lose it",
        "Even Wraith King's skeletons have more economic stability",
        "The ultimate Rapier buyer — all in, all lost",
        "Bankruptcy is just a number to them",
    ],
    "award_jack_of_all_trades": [
        "Master of... some?",
        "Their hero pool needs its own zip code",
        "Variety is the spice of feeding",
        "Draft phase? Just random and pray.",
        "More hero variety than a TI grand final",
        "They've played heroes most people forgot exist",
        "The ultimate flex pick — for better or worse",
        "Even Invoker doesn't have this many options",
    ],
    "award_touched_grass": [
        "Has a life outside Dota",
        "The rare casual sighting",
        "Quality over quantity, apparently",
        "They play Dota like it's a seasonal event",
        "Even couriers log more game time",
        "Their queue cooldown is measured in weeks",
        "Casually casual",
        "The lobby misses them. Kind of.",
    ],
    "award_xp_vacuum": [
        "Levels ahead, brain behind",
        "Outleveling everyone including their own team",
        "Tome of Knowledge in human form",
        "They absorb XP like a Black Hole",
        "Even Meepo is jealous of this XP gain",
        "Level 25 while everyone else is level 18",
        "Their Talent Tree is always fully unlocked first",
        "XPM so high it should be nerfed",
    ],
    "award_serial_killer": [
        "Violence IS the answer",
        "Their kill count has a kill count",
        "The Aegis of the Immortal fears them",
        "More kills than a Phantom Assassin with Daedalus",
        "The kill feed is their personal blog",
        "Even Bloodseeker can't keep up with this body count",
        "Godlike streak? More like a lifestyle.",
        "Their kill participation is their entire personality",
    ],
    "award_assist_machine": [
        "Always there for the cleanup crew",
        "The real carry was the assists we made along the way",
        "Force Staff into every fight",
        "They touch every kill like a Bounty Hunter Track",
        "Assists? They call it 'team participation'",
        "The ultimate setup player — like a Rubick with stolen spells",
        "Their KDA ratio is carried entirely by the A",
        "Glimmer Cape, Force Staff, save, repeat",
    ],
    "award_win_merchant": [
        "They just win. That's it. That's the tweet.",
        "The matchmaker's nightmare",
        "Team diff personified",
        "Winning is their default setting",
        "They queue and the lobby trembles",
        "Built like a last-pick Broodmother — unstoppable",
        "Their win rate defies mathematical logic",
        "The lobby's final boss",
    ],
    "award_charity_case": [
        "They play so others may win",
        "Free MMR dispenser",
        "Morale support for the enemy team",
        "Their losses fund the enemy's win streaks",
        "The ultimate space creator — for the wrong team",
        "Even neutral creeps have a better record",
        "They try. That's what matters. Apparently.",
        "Participation trophy energy",
    ],
    "award_fantasy_king": [
        "TI fantasy draft energy",
        "If only fantasy points were real money",
        "The stats don't lie",
        "Their stat line reads like a Dotabuff highlight reel",
        "Fantasy points stacked higher than a triple-stacked camp",
        "If Dota had an All-Star game, they'd be captain",
        "Numbers don't lie — this player is built different",
        "The spreadsheet hero",
    ],
}


def get_random_flavor(key: str, **kwargs) -> str:
    """Get a random flavor text from a pool, with optional template formatting.

    Args:
        key: The flavor pool category key
        **kwargs: Template variables to format into the string

    Returns:
        A random flavor text string, formatted with kwargs if any
    """
    pool = FLAVOR_POOLS.get(key, [])
    if not pool:
        return ""
    text = random.choice(pool)
    if kwargs:
        try:
            text = text.format(**kwargs)
        except (KeyError, IndexError):
            pass
    return text


class WrappedService:
    """Service for generating Cama Wrapped yearly summaries."""

    def __init__(
        self,
        wrapped_repo: "WrappedRepository",
        player_repo: "PlayerRepository",
        match_repo: "MatchRepository",
        bet_repo: "BetRepository",
        gambling_stats_service: "GamblingStatsService | None" = None,
        pairings_repo: "PairingsRepository | None" = None,
        package_deal_service: "PackageDealService | None" = None,
    ):
        self.wrapped_repo = wrapped_repo
        self.player_repo = player_repo
        self.match_repo = match_repo
        self.bet_repo = bet_repo
        self.gambling_stats_service = gambling_stats_service
        self.pairings_repo = pairings_repo
        self.package_deal_service = package_deal_service

    def _get_year_timestamps(self, year: int) -> tuple[int, int]:
        """
        Get start and end timestamps for a full calendar year.

        Args:
            year: Calendar year (e.g. 2026)

        Returns:
            (start_timestamp, end_timestamp) as Unix timestamps
        """
        start = datetime(year, 1, 1, tzinfo=UTC)
        end = datetime(year, 12, 31, 23, 59, 59, tzinfo=UTC)
        return int(start.timestamp()), int(end.timestamp()) + 1

    def get_server_wrapped(
        self, guild_id: int | None, year: int
    ) -> ServerWrapped | None:
        """
        Generate server-wide wrapped summary for a calendar year.

        Args:
            guild_id: Discord guild ID
            year: Calendar year (e.g. 2026)

        Returns:
            ServerWrapped object or None if no data
        """
        start_ts, end_ts = self._get_year_timestamps(year)

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

        year_label = f"Cama Wrapped {year}"

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
            year=year,
            year_label=year_label,
            total_matches=summary.get("total_matches", 0),
            total_wagered=summary.get("total_wagered", 0),
            unique_players=summary.get("total_players", 0),
            unique_heroes=summary.get("unique_heroes", 0),
            awards=awards,
            top_players=top_players,
            most_played_heroes=most_played,
            best_hero=best_hero,
        )

        return wrapped

    def get_player_wrapped(
        self, discord_id: int, year: int, guild_id: int | None = None
    ) -> PlayerWrapped | None:
        """
        Generate personal wrapped summary for a player.

        Args:
            discord_id: Player's Discord ID
            year: Calendar year (e.g. 2026)
            guild_id: Guild ID for guild-specific stats

        Returns:
            PlayerWrapped object or None if no data
        """
        start_ts, end_ts = self._get_year_timestamps(year)

        # Get player info
        player = self.player_repo.get_by_id(discord_id, guild_id)
        if not player:
            return None

        # Query player's match stats for the period
        match_details = self.wrapped_repo.get_month_player_match_details(
            discord_id, guild_id, start_ts, end_ts
        )
        if not match_details:
            return None

        games_played = match_details["games_played"]
        wins = match_details["wins"]
        losses = match_details["losses"]

        # Get rating change
        rating_changes = self.wrapped_repo.get_month_rating_changes(guild_id, start_ts, end_ts)
        rating_change = 0
        for rc in rating_changes:
            if rc["discord_id"] == discord_id:
                rating_change = int(rc["rating_change"] or 0)
                break

        # Get top heroes
        player_heroes = self.wrapped_repo.get_month_player_heroes(guild_id, start_ts, end_ts)
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
        betting_stats = self.wrapped_repo.get_month_betting_stats(guild_id, start_ts, end_ts)
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

    def get_player_records_wrapped(
        self,
        discord_id: int,
        year: int,
        guild_id: int | None = None,
    ) -> PersonalRecordsWrapped | None:
        """
        Generate personal records wrapped for a player.

        Covers Jan 1 through Dec 31 of the given year.

        Returns:
            PersonalRecordsWrapped or None if insufficient data
        """
        _, end_ts = self._get_year_timestamps(year)

        rows = self.wrapped_repo.get_player_year_matches(
            discord_id, guild_id, year, end_ts
        )
        if len(rows) < WRAPPED_MIN_GAMES:
            return None

        player = self.player_repo.get_by_id(discord_id, guild_id)
        if not player:
            return None

        # Build steam_id set for enrichment_data player lookup
        steam_ids = set(self.player_repo.get_steam_ids(discord_id))

        records: list[PersonalRecord] = []

        # --- Column-based stats (best and worst) ---
        # (best_key, worst_key, column, best_label, worst_label, unit, best_is_worst)
        # best_is_worst: True when max value is semantically bad (e.g. most deaths)
        stat_defs = [
            ("kills_best", None, "kills", "Most Kills", None, "kills", False),
            ("assists_best", None, "assists", "Most Assists", None, "assists", False),
            ("deaths_worst", None, "deaths", "Feeding Frenzy", None, "deaths", True),
            ("gpm_best", "gpm_worst", "gpm", "Highest GPM", "Lowest GPM", "GPM", False),
            ("xpm_best", "xpm_worst", "xpm", "Highest XPM", "AFK Simulator", "XPM", False),
            ("last_hits_best", "last_hits_worst", "last_hits", "Most Last Hits", "Allergic to Creeps", "last hits", False),
            ("denies_best", None, "denies", "Most Denies", None, "denies", False),
            ("hero_damage_best", None, "hero_damage", "Most Hero Damage", None, "damage", False),
            ("tower_damage_best", None, "tower_damage", "Most Tower Damage", None, "damage", False),
            ("towers_killed_best", None, "towers_killed", "Most Tower Kills", None, "towers", False),
            ("hero_healing_best", None, "hero_healing", "Most Hero Healing", None, "healing", False),
            ("obs_placed_best", None, "obs_placed", "Most Obs Placed", None, "obs", False),
            ("sen_placed_best", None, "sen_placed", "Most Sentries Placed", None, "sentries", False),
            ("stuns_best", None, "stuns", "Most Stuns", None, "sec stuns", False),
        ]

        for best_key, worst_key, col, best_label, worst_label, unit, best_is_worst in stat_defs:
            valid_rows = [r for r in rows if r.get(col) is not None]
            if not valid_rows:
                continue

            # Best (max)
            best_row = max(valid_rows, key=lambda r: r[col])
            val = best_row[col]
            display = f"{val:,.1f} {unit}" if isinstance(val, float) else f"{val:,} {unit}"
            records.append(PersonalRecord(
                stat_key=best_key,
                stat_label=best_label,
                value=val,
                display_value=display,
                hero_id=best_row.get("hero_id"),
                match_id=best_row.get("match_id"),
                valve_match_id=best_row.get("valve_match_id"),
                match_date=str(best_row.get("match_date", ""))[:10] if best_row.get("match_date") else None,
                is_worst=best_is_worst,
            ))

            # Worst (min) if defined
            if worst_key and worst_label:
                worst_row = min(valid_rows, key=lambda r: r[col])
                wval = worst_row[col]
                wdisplay = f"{wval:,.1f} {unit}" if isinstance(wval, float) else f"{wval:,} {unit}"
                records.append(PersonalRecord(
                    stat_key=worst_key,
                    stat_label=worst_label,
                    value=wval,
                    display_value=wdisplay,
                    hero_id=worst_row.get("hero_id"),
                    match_id=worst_row.get("match_id"),
                    valve_match_id=worst_row.get("valve_match_id"),
                    match_date=str(worst_row.get("match_date", ""))[:10] if worst_row.get("match_date") else None,
                    is_worst=True,
                ))

        # --- KDA ratio (exclude 0/0/0 rows — likely incomplete data) ---
        kda_rows = [
            r for r in rows
            if r.get("kills") is not None and r.get("assists") is not None and r.get("deaths") is not None
            and (r["kills"] + r["assists"] + r["deaths"]) > 0
        ]
        if kda_rows:
            def _kda(r):
                return (r["kills"] + r["assists"]) / max(r["deaths"], 1)

            best_kda_row = max(kda_rows, key=_kda)
            best_kda_val = _kda(best_kda_row)
            records.append(PersonalRecord(
                stat_key="kda_best",
                stat_label="Best KDA",
                value=round(best_kda_val, 2),
                display_value=f"{best_kda_val:.2f} KDA",
                hero_id=best_kda_row.get("hero_id"),
                match_id=best_kda_row.get("match_id"),
                valve_match_id=best_kda_row.get("valve_match_id"),
                match_date=str(best_kda_row.get("match_date", ""))[:10] if best_kda_row.get("match_date") else None,
            ))

            worst_kda_row = min(kda_rows, key=_kda)
            worst_kda_val = _kda(worst_kda_row)
            records.append(PersonalRecord(
                stat_key="kda_worst",
                stat_label="Worst KDA",
                value=round(worst_kda_val, 2),
                display_value=f"{worst_kda_val:.2f} KDA",
                hero_id=worst_kda_row.get("hero_id"),
                match_id=worst_kda_row.get("match_id"),
                valve_match_id=worst_kda_row.get("valve_match_id"),
                match_date=str(worst_kda_row.get("match_date", ""))[:10] if worst_kda_row.get("match_date") else None,
                is_worst=True,
            ))

        # --- Duration records ---
        duration_rows = [r for r in rows if r.get("duration_seconds") and r["duration_seconds"] > 0]
        if duration_rows:
            longest = max(duration_rows, key=lambda r: r["duration_seconds"])
            dur_min = longest["duration_seconds"] // 60
            records.append(PersonalRecord(
                stat_key="longest_game",
                stat_label="Longest Game",
                value=longest["duration_seconds"],
                display_value=f"{dur_min}:{longest['duration_seconds'] % 60:02d} min",
                hero_id=longest.get("hero_id"),
                match_id=longest.get("match_id"),
                valve_match_id=longest.get("valve_match_id"),
                match_date=str(longest.get("match_date", ""))[:10] if longest.get("match_date") else None,
            ))

            shortest = min(duration_rows, key=lambda r: r["duration_seconds"])
            sdur_min = shortest["duration_seconds"] // 60
            records.append(PersonalRecord(
                stat_key="shortest_game",
                stat_label="Shortest Game",
                value=shortest["duration_seconds"],
                display_value=f"{sdur_min}:{shortest['duration_seconds'] % 60:02d} min",
                hero_id=shortest.get("hero_id"),
                match_id=shortest.get("match_id"),
                valve_match_id=shortest.get("valve_match_id"),
                match_date=str(shortest.get("match_date", ""))[:10] if shortest.get("match_date") else None,
            ))

        # --- Enrichment-data stats ---
        enrichment_stats = self._extract_enrichment_records(rows, steam_ids)
        records.extend(enrichment_stats)

        # --- Kill participation ---
        kp_records = []
        for r in rows:
            if r.get("kills") is None or r.get("assists") is None:
                continue
            team_score = 0
            if r.get("side") == "radiant":
                team_score = r.get("radiant_score") or 0
            elif r.get("side") == "dire":
                team_score = r.get("dire_score") or 0
            if team_score > 0:
                kp = (r["kills"] + r["assists"]) / team_score
                kp_records.append((kp, r))
        if kp_records:
            best_kp_val, best_kp_row = max(kp_records, key=lambda x: x[0])
            records.append(PersonalRecord(
                stat_key="kill_participation_best",
                stat_label="Highest Kill Participation",
                value=round(best_kp_val * 100, 1),
                display_value=f"{best_kp_val * 100:.1f}%",
                hero_id=best_kp_row.get("hero_id"),
                match_id=best_kp_row.get("match_id"),
                valve_match_id=best_kp_row.get("valve_match_id"),
                match_date=str(best_kp_row.get("match_date", ""))[:10] if best_kp_row.get("match_date") else None,
            ))

        # --- Streaks ---
        win_streak, lose_streak, win_breaker_hero, lose_breaker_hero = self._compute_streaks(rows)
        if win_streak > 0:
            records.append(PersonalRecord(
                stat_key="win_streak_best",
                stat_label="Longest Win Streak",
                value=win_streak,
                display_value=f"{win_streak} wins",
                hero_id=win_breaker_hero,
                match_id=None,
                valve_match_id=None,
                match_date=None,
            ))
        if lose_streak > 0:
            records.append(PersonalRecord(
                stat_key="lose_streak_worst",
                stat_label="Tilt Master",
                value=lose_streak,
                display_value=f"{lose_streak} losses",
                hero_id=lose_breaker_hero,
                match_id=None,
                valve_match_id=None,
                match_date=None,
                is_worst=True,
            ))

        return PersonalRecordsWrapped(
            discord_id=discord_id,
            discord_username=player.name,
            year=year,
            year_label=f"Cama Wrapped {year}",
            games_played=len(rows),
            records=records,
        )

    def _extract_enrichment_records(
        self, rows: list[dict], steam_ids: set[int]
    ) -> list[PersonalRecord]:
        """Extract records from enrichment_data JSON for each match."""
        records: list[PersonalRecord] = []

        # Track best per enrichment stat: stat_key -> (value, row)
        best_apm: tuple[float, dict] | None = None
        best_courier_kills: tuple[int, dict] | None = None
        worst_pings: tuple[int, dict] | None = None
        best_rapiers: tuple[int, dict] | None = None
        best_comeback: tuple[int, dict] | None = None
        worst_throw: tuple[int, dict] | None = None

        for row in rows:
            raw = row.get("enrichment_data")
            if not raw:
                continue
            try:
                data = json.loads(raw)
            except (json.JSONDecodeError, TypeError):
                continue

            # Find player in enrichment data by account_id (steam_id)
            player_data = None
            for p in data.get("players", []):
                if p.get("account_id") in steam_ids:
                    player_data = p
                    break

            if player_data:
                # APM
                apm = player_data.get("actions_per_min")
                if apm is not None and (best_apm is None or apm > best_apm[0]):
                    best_apm = (apm, row)

                # Courier kills
                ck = player_data.get("courier_kills")
                if ck is not None and ck > 0 and (best_courier_kills is None or ck > best_courier_kills[0]):
                    best_courier_kills = (ck, row)

                # Map pings
                pings = player_data.get("pings")
                if pings is not None and (worst_pings is None or pings > worst_pings[0]):
                    worst_pings = (pings, row)

                # Rapiers from purchase_log
                purchase_log = player_data.get("purchase_log")
                if purchase_log:
                    rapier_count = sum(1 for item in purchase_log if item.get("key") == "rapier")
                    if rapier_count > 0 and (best_rapiers is None or rapier_count > best_rapiers[0]):
                        best_rapiers = (rapier_count, row)

            # Match-level: comeback / throw
            comeback = data.get("comeback")
            if comeback is not None and comeback > 0 and (best_comeback is None or comeback > best_comeback[0]):
                best_comeback = (comeback, row)

            throw = data.get("throw")
            if throw is not None and throw > 0 and (worst_throw is None or throw > worst_throw[0]):
                worst_throw = (throw, row)

        def _make_record(key, label, val, display, row, is_worst=False):
            return PersonalRecord(
                stat_key=key,
                stat_label=label,
                value=val,
                display_value=display,
                hero_id=row.get("hero_id"),
                match_id=row.get("match_id"),
                valve_match_id=row.get("valve_match_id"),
                match_date=str(row.get("match_date", ""))[:10] if row.get("match_date") else None,
                is_worst=is_worst,
            )

        def _na_record(key, label, is_worst=False):
            return PersonalRecord(
                stat_key=key, stat_label=label, value=None,
                display_value="N/A", hero_id=None, match_id=None,
                valve_match_id=None, match_date=None, is_worst=is_worst,
            )

        # Enrichment stats: emit record or N/A placeholder
        enrichment_pairs = [
            ("apm_best", "Highest APM", best_apm, False),
            ("courier_kills_best", "Most Courier Kills", best_courier_kills, False),
            ("pings_worst", "Signal Spammer", worst_pings, True),
            ("rapiers_best", "Most Rapiers", best_rapiers, False),
            ("comeback_best", "Biggest Comeback", best_comeback, False),
            ("throw_worst", "Charity Case", worst_throw, True),
        ]
        format_fns = {
            "apm_best": lambda v: f"{v:,.0f} APM",
            "courier_kills_best": lambda v: f"{v} couriers",
            "pings_worst": lambda v: f"{v:,} pings",
            "rapiers_best": lambda v: f"{v} rapiers",
            "comeback_best": lambda v: f"{v:,} gold",
            "throw_worst": lambda v: f"{v:,} gold",
        }

        for key, label, best_val_row, is_worst in enrichment_pairs:
            if best_val_row:
                val, row = best_val_row
                records.append(_make_record(key, label, val, format_fns[key](val), row, is_worst=is_worst))
            else:
                records.append(_na_record(key, label, is_worst=is_worst))

        return records

    @staticmethod
    def _compute_streaks(matches: list[dict]) -> tuple[int, int, int | None, int | None]:
        """Compute longest win and lose streaks from sequential match results.

        Returns:
            (longest_win_streak, longest_lose_streak,
             win_streak_breaker_hero_id, lose_streak_breaker_hero_id)
        """
        best_win = 0
        best_lose = 0
        current_win = 0
        current_lose = 0
        # Track which streak instance is the best so far
        best_win_end_idx = -1
        best_lose_end_idx = -1

        for idx, m in enumerate(matches):
            if m.get("won") is None:
                current_win = 0
                current_lose = 0
                continue
            if m["won"]:
                current_win += 1
                current_lose = 0
                if current_win > best_win:
                    best_win = current_win
                    best_win_end_idx = idx
            else:
                current_lose += 1
                current_win = 0
                if current_lose > best_lose:
                    best_lose = current_lose
                    best_lose_end_idx = idx

        # Find the hero that broke each streak (the match right after the streak ended)
        win_breaker_hero: int | None = None
        if best_win > 0 and best_win_end_idx + 1 < len(matches):
            breaker = matches[best_win_end_idx + 1]
            if not breaker.get("won"):
                win_breaker_hero = breaker.get("hero_id")

        lose_breaker_hero: int | None = None
        if best_lose > 0 and best_lose_end_idx + 1 < len(matches):
            breaker = matches[best_lose_end_idx + 1]
            if breaker.get("won"):
                lose_breaker_hero = breaker.get("hero_id")

        return best_win, best_lose, win_breaker_hero, lose_breaker_hero

    # ============ NEW WRAPPED STORY METHODS ============

    def get_personal_summary_wrapped(
        self, discord_id: int, year: int, guild_id: int | None = None
    ) -> PersonalSummaryWrapped | None:
        """Get personal summary stats with percentile comparisons."""
        start_ts, end_ts = self._get_year_timestamps(year)

        player = self.player_repo.get_by_id(discord_id, guild_id)
        if not player:
            return None

        match_details = self.wrapped_repo.get_month_player_match_details(
            discord_id, guild_id, start_ts, end_ts
        )
        if not match_details:
            return None

        games_played = match_details["games_played"]
        wins = match_details["wins"]
        losses = match_details["losses"]
        win_rate = wins / games_played if games_played > 0 else 0

        # Get rating change
        rating_changes = self.wrapped_repo.get_month_rating_changes(guild_id, start_ts, end_ts)
        rating_change = 0
        for rc in rating_changes:
            if rc["discord_id"] == discord_id:
                rating_change = int(rc["rating_change"] or 0)
                break

        # Get aggregate stats from match_stats
        match_stats = self.wrapped_repo.get_month_match_stats(guild_id, start_ts, end_ts)
        total_kills = 0
        total_deaths = 0
        total_assists = 0
        for ms in match_stats:
            if ms["discord_id"] == discord_id:
                total_kills = ms.get("total_kills") or 0
                total_deaths = ms.get("total_deaths") or 0
                total_assists = ms.get("total_assists") or 0
                break

        # Get player heroes for unique count and avg duration
        player_heroes = self.wrapped_repo.get_month_player_heroes(guild_id, start_ts, end_ts)
        unique_heroes = len([ph for ph in player_heroes if ph["discord_id"] == discord_id])

        # Get avg game duration from match rows
        rows = self.wrapped_repo.get_player_year_matches(discord_id, guild_id, year, end_ts)
        durations = [r["duration_seconds"] for r in rows if r.get("duration_seconds")]
        avg_game_duration = int(sum(durations) / len(durations)) if durations else 0

        # Compute percentiles against all server players who played this year
        all_games = [ms["games_played"] for ms in match_stats]
        all_win_rates = [
            ms["wins"] / ms["games_played"]
            for ms in match_stats
            if ms["games_played"] > 0
        ]
        all_kdas = [
            ((ms.get("total_kills") or 0) + (ms.get("total_assists") or 0))
            / max(ms.get("total_deaths") or 1, 1)
            for ms in match_stats
            if ms["games_played"] > 0
        ]
        hero_count_by_player = {}
        for ph in player_heroes:
            hero_count_by_player[ph["discord_id"]] = (
                hero_count_by_player.get(ph["discord_id"], 0) + 1
            )
        all_unique_heroes = [
            hero_count_by_player.get(ms["discord_id"], 0)
            for ms in match_stats
        ]
        all_total_kda = [
            (ms.get("total_kills") or 0) + (ms.get("total_assists") or 0)
            for ms in match_stats
        ]

        kda = (total_kills + total_assists) / max(total_deaths, 1)
        games_played_percentile = self._compute_percentile(games_played, all_games)
        win_rate_percentile = self._compute_percentile(win_rate, all_win_rates)
        kda_percentile = self._compute_percentile(kda, all_kdas)
        unique_heroes_percentile = self._compute_percentile(unique_heroes, all_unique_heroes)
        total_kda_percentile = self._compute_percentile(
            total_kills + total_assists, all_total_kda
        )

        # Pick flavor text
        if games_played_percentile >= 75:
            flavor = get_random_flavor("games_played_high")
        elif games_played_percentile >= 30:
            flavor = get_random_flavor("games_played_mid")
        else:
            flavor = get_random_flavor("games_played_low")

        return PersonalSummaryWrapped(
            discord_id=discord_id,
            discord_username=player.name,
            games_played=games_played,
            wins=wins,
            losses=losses,
            win_rate=win_rate,
            rating_change=rating_change,
            total_kills=total_kills,
            total_deaths=total_deaths,
            total_assists=total_assists,
            avg_game_duration=avg_game_duration,
            unique_heroes=unique_heroes,
            games_played_percentile=games_played_percentile,
            win_rate_percentile=win_rate_percentile,
            kda_percentile=kda_percentile,
            unique_heroes_percentile=unique_heroes_percentile,
            total_kda_percentile=total_kda_percentile,
            flavor_text=flavor,
        )

    @staticmethod
    def _compute_percentile(value: float, all_values: list[float]) -> float:
        """Compute percentile rank of value within all_values."""
        if not all_values:
            return 50.0
        below = sum(1 for v in all_values if v < value)
        equal = sum(1 for v in all_values if v == value)
        return ((below + equal * 0.5) / len(all_values)) * 100

    def get_pairwise_wrapped(
        self, discord_id: int, guild_id: int | None = None
    ) -> PairwiseWrapped | None:
        """Get all-time pairwise teammate and rival data."""
        if not self.pairings_repo:
            return None

        best_teammates_raw = self.pairings_repo.get_best_teammates(discord_id, guild_id, min_games=3, limit=3)
        most_played_raw = self.pairings_repo.get_most_played_with(discord_id, guild_id, min_games=3, limit=3)
        worst_matchups_raw = self.pairings_repo.get_worst_matchups(discord_id, guild_id, min_games=3, limit=1)
        best_matchups_raw = self.pairings_repo.get_best_matchups(discord_id, guild_id, min_games=3, limit=1)
        most_against_raw = self.pairings_repo.get_most_played_against(discord_id, guild_id, min_games=3, limit=3)

        if not any([best_teammates_raw, most_played_raw, worst_matchups_raw, best_matchups_raw, most_against_raw]):
            return None

        def _resolve_entries(raw_list: list[dict], id_key: str) -> list[PairwiseEntry]:
            entries = []
            for r in raw_list:
                pid = r[id_key]
                p = self.player_repo.get_by_id(pid, guild_id)
                username = p.name if p else str(pid)
                games = r.get("games_together") or r.get("games_against") or 0
                wins = r.get("wins_together") or r.get("wins_against") or 0
                entries.append(PairwiseEntry(
                    discord_id=pid,
                    username=username,
                    games=games,
                    wins=wins,
                    win_rate=r.get("win_rate", 0),
                ))
            return entries

        best_teammates = _resolve_entries(best_teammates_raw, "teammate_id")
        most_played_with = _resolve_entries(most_played_raw, "teammate_id")
        most_played_against = _resolve_entries(most_against_raw, "opponent_id")

        nemesis = None
        if worst_matchups_raw:
            nemesis_entries = _resolve_entries(worst_matchups_raw, "opponent_id")
            nemesis = nemesis_entries[0] if nemesis_entries else None

        punching_bag = None
        if best_matchups_raw:
            bag_entries = _resolve_entries(best_matchups_raw, "opponent_id")
            punching_bag = bag_entries[0] if bag_entries else None

        return PairwiseWrapped(
            best_teammates=best_teammates,
            most_played_with=most_played_with,
            nemesis=nemesis,
            punching_bag=punching_bag,
            most_played_against=most_played_against,
        )

    def get_package_deal_wrapped(
        self, discord_id: int, guild_id: int | None = None
    ) -> PackageDealWrapped | None:
        """Get anonymized package deal stats for wrapped."""
        if not self.package_deal_service:
            return None

        repo = self.package_deal_service.package_deal_repo

        # Get all deals involving this player (as buyer or partner)
        deals = repo.get_deals_involving_player(guild_id, discord_id)
        if not deals:
            return None

        times_bought = 0
        times_bought_on_you = 0
        buyer_ids: set[int] = set()
        jc_spent = 0
        jc_spent_on_you = 0
        total_games = 0

        for deal in deals:
            total_games += deal.games_remaining
            if deal.buyer_discord_id == discord_id:
                times_bought += 1
                jc_spent += deal.cost_paid
            if deal.partner_discord_id == discord_id:
                times_bought_on_you += 1
                jc_spent_on_you += deal.cost_paid
                buyer_ids.add(deal.buyer_discord_id)

        return PackageDealWrapped(
            times_bought=times_bought,
            times_bought_on_you=times_bought_on_you,
            unique_buyers=len(buyer_ids),
            jc_spent=jc_spent,
            jc_spent_on_you=jc_spent_on_you,
            total_games_committed=total_games,
        )

    def get_hero_spotlight_wrapped(
        self, discord_id: int, year: int, guild_id: int | None = None
    ) -> HeroSpotlightWrapped | None:
        """Get hero spotlight data for wrapped."""
        start_ts, end_ts = self._get_year_timestamps(year)
        player_heroes = self.wrapped_repo.get_month_player_heroes(guild_id, start_ts, end_ts)

        # Filter to this player
        my_heroes = [ph for ph in player_heroes if ph["discord_id"] == discord_id]
        if not my_heroes:
            return None

        # Sort by picks descending
        my_heroes.sort(key=lambda x: x["picks"], reverse=True)

        top_3 = []
        for h in my_heroes[:3]:
            wr = h["wins"] / h["picks"] if h["picks"] > 0 else 0
            hero_name = get_hero_name(h["hero_id"]) or f"Hero #{h['hero_id']}"
            total_kills = h.get("total_kills") or 0
            total_deaths = h.get("total_deaths") or 0
            total_assists = h.get("total_assists") or 0
            kda = (total_kills + total_assists) / max(total_deaths, 1)
            top_3.append({
                "name": hero_name,
                "picks": h["picks"],
                "wins": h["wins"],
                "win_rate": wr,
                "kda": kda,
            })

        top = my_heroes[0]
        top_wr = top["wins"] / top["picks"] if top["picks"] > 0 else 0
        top_name = get_hero_name(top["hero_id"]) or f"Hero #{top['hero_id']}"

        return HeroSpotlightWrapped(
            top_hero_name=top_name,
            top_hero_picks=top["picks"],
            top_hero_wins=top["wins"],
            top_hero_win_rate=top_wr,
            top_3_heroes=top_3,
            unique_heroes=len(my_heroes),
        )

    def get_role_breakdown_wrapped(
        self, discord_id: int, year: int, guild_id: int | None = None
    ) -> RoleBreakdownWrapped | None:
        """Get lane frequency from match enrichment data."""
        _, end_ts = self._get_year_timestamps(year)

        rows = self.wrapped_repo.get_player_year_matches(discord_id, guild_id, year, end_ts)

        if not rows:
            return None

        # Hoist steam_ids lookup outside the loop
        steam_ids = set(self.player_repo.get_steam_ids(discord_id))

        pos_freq: dict[int, int] = {}
        for row in rows:
            raw = row.get("enrichment_data")
            if raw:
                try:
                    data = json.loads(raw)
                    for p in data.get("players", []):
                        if p.get("account_id") in steam_ids:
                            lane_role = p.get("lane_role", 0)
                            if lane_role in (1, 2, 3):  # safe, mid, off only
                                pos_freq[lane_role] = pos_freq.get(lane_role, 0) + 1
                            break
                except (json.JSONDecodeError, TypeError):
                    pass

        lane_games = sum(pos_freq.values())
        return RoleBreakdownWrapped(
            lane_freq=pos_freq,
            total_games=lane_games,
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
                    flavor_text=get_random_flavor("award_gold_goblin"),
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
                    flavor_text=get_random_flavor("award_immortal_hands"),
                )
            )

        # Worst KDA (fun award)
        worst_kda = min(eligible_players, key=lambda x: x.get("avg_kda") or float("inf"))
        if worst_kda.get("avg_kda") and worst_kda["avg_kda"] > 0 and worst_kda != best_kda:
            awards.append(
                Award(
                    category="performance",
                    title="First Blood Enthusiast",
                    stat_name="Worst KDA",
                    stat_value=f"{worst_kda['avg_kda']:.2f} KDA",
                    discord_id=worst_kda["discord_id"],
                    discord_username=worst_kda["discord_username"],
                    emoji="💀",
                    flavor_text=get_random_flavor("award_first_blood_enthusiast"),
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
                    flavor_text=get_random_flavor("award_ward_bot"),
                )
            )

        # Best XPM
        best_xpm = max(eligible_players, key=lambda x: x.get("avg_xpm") or 0)
        if best_xpm.get("avg_xpm"):
            awards.append(
                Award(
                    category="performance",
                    title="XP Vacuum",
                    stat_name="Best XPM",
                    stat_value=f"{int(best_xpm['avg_xpm'])} XPM",
                    discord_id=best_xpm["discord_id"],
                    discord_username=best_xpm["discord_username"],
                    emoji="📚",
                    flavor_text=get_random_flavor("award_xp_vacuum"),
                )
            )

        # Most Total Kills
        most_kills = max(eligible_players, key=lambda x: x.get("total_kills") or 0)
        if most_kills.get("total_kills"):
            awards.append(
                Award(
                    category="performance",
                    title="Serial Killer",
                    stat_name="Most Kills",
                    stat_value=f"{most_kills['total_kills']} kills",
                    discord_id=most_kills["discord_id"],
                    discord_username=most_kills["discord_username"],
                    emoji="🔪",
                    flavor_text=get_random_flavor("award_serial_killer"),
                )
            )

        # Most Assists
        most_assists = max(eligible_players, key=lambda x: x.get("total_assists") or 0)
        if most_assists.get("total_assists"):
            awards.append(
                Award(
                    category="performance",
                    title="Assist Machine",
                    stat_name="Most Assists",
                    stat_value=f"{most_assists['total_assists']} assists",
                    discord_id=most_assists["discord_id"],
                    discord_username=most_assists["discord_username"],
                    emoji="🤝",
                    flavor_text=get_random_flavor("award_assist_machine"),
                )
            )

        # Highest Win Rate
        best_wr = max(eligible_players, key=lambda x: x.get("wins", 0) / max(x.get("games_played", 1), 1))
        wr = best_wr["wins"] / max(best_wr["games_played"], 1)
        if wr > 0.5:
            awards.append(
                Award(
                    category="performance",
                    title="Win Merchant",
                    stat_name="Best Win Rate",
                    stat_value=f"{wr * 100:.0f}%",
                    discord_id=best_wr["discord_id"],
                    discord_username=best_wr["discord_username"],
                    emoji="👑",
                    flavor_text=get_random_flavor("award_win_merchant"),
                )
            )

        # Lowest Win Rate (fun)
        worst_wr = min(eligible_players, key=lambda x: x.get("wins", 0) / max(x.get("games_played", 1), 1))
        worst_wr_val = worst_wr["wins"] / max(worst_wr["games_played"], 1)
        if worst_wr_val < 0.5 and worst_wr != best_wr:
            awards.append(
                Award(
                    category="fun",
                    title="Charity Case",
                    stat_name="Lowest Win Rate",
                    stat_value=f"{worst_wr_val * 100:.0f}%",
                    discord_id=worst_wr["discord_id"],
                    discord_username=worst_wr["discord_username"],
                    emoji="🎗️",
                    flavor_text=get_random_flavor("award_charity_case"),
                )
            )

        # Best Fantasy Points
        best_fantasy = max(eligible_players, key=lambda x: x.get("total_fantasy") or 0)
        if best_fantasy.get("total_fantasy"):
            awards.append(
                Award(
                    category="performance",
                    title="Fantasy King",
                    stat_name="Most Fantasy Points",
                    stat_value=f"{best_fantasy['total_fantasy']:.0f} pts",
                    discord_id=best_fantasy["discord_id"],
                    discord_username=best_fantasy["discord_username"],
                    emoji="🏆",
                    flavor_text=get_random_flavor("award_fantasy_king"),
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
                        flavor_text=get_random_flavor("award_elo_inflation"),
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
                        flavor_text=get_random_flavor("award_the_cliff"),
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
                        flavor_text=get_random_flavor("award_steady_eddie"),
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
                            flavor_text=get_random_flavor("award_coin_flip"),
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
                        flavor_text=get_random_flavor("award_diamond_hands"),
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
                        flavor_text=get_random_flavor("award_house_favorite"),
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
                    flavor_text=get_random_flavor("award_degen_supreme"),
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
                        flavor_text=get_random_flavor("award_bankruptcy_speedrunner"),
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
                        flavor_text=get_random_flavor("award_one_trick"),
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
                        flavor_text=get_random_flavor("award_jack_of_all_trades"),
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
                    flavor_text=get_random_flavor("award_no_life"),
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
                        flavor_text=get_random_flavor("award_touched_grass"),
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
                        flavor_text=get_random_flavor("award_punching_bag"),
                    )
                )

        return awards

