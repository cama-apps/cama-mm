"""
Service for gambling statistics and degen score calculation.
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from repositories.bet_repository import BetRepository
    from repositories.match_repository import MatchRepository
    from repositories.player_repository import PlayerRepository
    from services.bankruptcy_service import BankruptcyService
    from services.loan_service import LoanService


# Degen score weights (total = 100)
MAX_LEVERAGE_WEIGHT = 25  # % of bets at 5x leverage
BET_SIZE_WEIGHT = 25  # Avg bet size vs typical income
DEBT_DEPTH_WEIGHT = 20  # How deep into debt (lowest_balance_ever)
BANKRUPTCY_WEIGHT = 15  # Bankruptcy count
FREQUENCY_WEIGHT = 10  # % of matches bet on
LOSS_CHASE_WEIGHT = 5  # Rate of increasing bet after loss

# Bonus for peak degen behavior (can exceed 100)
NEGATIVE_LOAN_BONUS = 25  # Taking a loan while already in debt

# Typical income per match for bet size calculation (participation + avg win bonus)
TYPICAL_INCOME_PER_MATCH = 3

# Degen tiers
DEGEN_TIERS = [
    (0, 19, "Casual", "Do you even gamble?"),
    (20, 39, "Recreational", "Weekend warrior"),
    (40, 59, "Committed", "The house knows your name"),
    (60, 79, "Degenerate", "Your family is concerned"),
    (80, 89, "Menace", "Financial advisor on suicide watch"),
    (90, 100, "Legendary Degen", "They write songs about you"),
]


@dataclass
class DegenScoreBreakdown:
    """Breakdown of degen score components."""

    total: int
    title: str
    emoji: str
    tagline: str
    max_leverage_score: int  # % of bets at 5x
    bet_size_score: int  # Avg bet vs income
    debt_depth_score: int  # Lowest balance reached
    bankruptcy_score: int
    frequency_score: int
    loss_chase_score: int
    negative_loan_bonus: int  # +25 per loan taken while in debt
    flavor_texts: list[str]


@dataclass
class GambaStats:
    """Complete gambling statistics for a player."""

    discord_id: int
    total_bets: int
    wins: int
    losses: int
    win_rate: float
    net_pnl: int
    total_wagered: int
    roi: float
    avg_bet_size: float
    leverage_distribution: dict[int, int]
    current_streak: int
    best_streak: int
    worst_streak: int
    peak_pnl: int
    trough_pnl: int
    biggest_win: int
    biggest_loss: int
    degen_score: DegenScoreBreakdown
    paper_hands_count: int
    matches_played: int


@dataclass
class LeaderboardEntry:
    """Entry for gambling leaderboard."""

    discord_id: int
    total_bets: int
    wins: int
    losses: int
    win_rate: float
    net_pnl: int
    avg_leverage: float
    degen_score: int | None = None
    degen_title: str | None = None


@dataclass
class Leaderboard:
    """Complete leaderboard with multiple sections."""

    top_earners: list[LeaderboardEntry]
    down_bad: list[LeaderboardEntry]
    hall_of_degen: list[LeaderboardEntry]
    total_wagered: int
    total_bets: int
    avg_degen_score: float
    total_bankruptcies: int
    total_loans: int


@dataclass
class BettorProfile:
    """Profile of a bettor's activity on a specific player."""

    discord_id: int
    total_wagered_for: int  # Amount wagered on player's team
    total_wagered_against: int  # Amount wagered against player's team
    net_pnl_for: int  # P&L from bets FOR the player
    net_pnl_against: int  # P&L from bets AGAINST the player
    bets_for_count: int
    bets_against_count: int
    wins_for: int
    wins_against: int


@dataclass
class BettingImpactStats:
    """Stats about how others bet on/against a player."""

    discord_id: int
    matches_with_bets: int
    total_bets: int  # Total number of external bets

    # Aggregate totals
    total_wagered_for: int  # $ bet on player's team by others
    total_wagered_against: int  # $ bet against player's team by others
    supporters_net_pnl: int  # Total P&L of people betting FOR player
    haters_net_pnl: int  # Total P&L of people betting AGAINST player

    # Win rates
    supporter_win_rate: float  # % of supporter bets that won
    hater_win_rate: float  # % of hater bets that won
    supporter_bets_count: int  # Total supporter bets
    hater_bets_count: int  # Total hater bets

    # Derived metrics
    market_favorability: float  # % of bets on player's team
    supporter_roi: float  # ROI of supporters
    hater_roi: float  # ROI of haters

    # Notable bettors - by wagered amount
    biggest_fan: BettorProfile | None  # Most $ wagered FOR
    biggest_hater: BettorProfile | None  # Most $ wagered AGAINST

    # Notable bettors - by consistency (bet count)
    most_consistent_fan: BettorProfile | None  # Most bets placed FOR
    most_consistent_hater: BettorProfile | None  # Most bets placed AGAINST

    # Notable bettors - by P&L
    blessing: BettorProfile | None  # Profited most betting FOR (good luck charm)
    jinx: BettorProfile | None  # Lost most betting FOR (bad luck charm)
    luckiest_hater: BettorProfile | None  # Profited most betting AGAINST

    # Extremes - single bet records
    biggest_single_win: int  # Largest profit from a single bet on player's match
    biggest_single_loss: int  # Largest loss from a single bet on player's match

    # Counts
    unique_supporters: int
    unique_haters: int


class GamblingStatsService:
    """Service for calculating gambling statistics and degen scores."""

    def __init__(
        self,
        bet_repo: "BetRepository",
        player_repo: "PlayerRepository",
        match_repo: "MatchRepository",
        bankruptcy_service: "BankruptcyService | None" = None,
        loan_service: "LoanService | None" = None,
    ):
        self.bet_repo = bet_repo
        self.player_repo = player_repo
        self.match_repo = match_repo
        self.bankruptcy_service = bankruptcy_service
        self.loan_service = loan_service

    def get_player_stats(self, discord_id: int) -> GambaStats | None:
        """Get complete gambling statistics for a player."""
        history = self.bet_repo.get_player_bet_history(discord_id)

        if not history:
            return None

        # Basic stats
        total_bets = len(history)
        wins = sum(1 for b in history if b["outcome"] == "won")
        losses = total_bets - wins
        win_rate = wins / total_bets if total_bets > 0 else 0

        # P&L stats
        net_pnl = sum(b["profit"] for b in history)
        total_wagered = sum(b["effective_bet"] for b in history)
        roi = net_pnl / total_wagered if total_wagered > 0 else 0
        avg_bet_size = total_wagered / total_bets if total_bets > 0 else 0

        # Leverage distribution
        leverage_distribution = self.bet_repo.get_player_leverage_distribution(discord_id)

        # Streak analysis
        current_streak, best_streak, worst_streak = self._calculate_streaks(history)

        # P&L extremes
        peak_pnl, trough_pnl = self._calculate_pnl_extremes(history)
        biggest_win = max((b["profit"] for b in history if b["profit"] > 0), default=0)
        biggest_loss = min((b["profit"] for b in history if b["profit"] < 0), default=0)

        # Paper hands
        paper_hands_data = self.bet_repo.get_player_matches_without_self_bet(discord_id)
        paper_hands_count = paper_hands_data["paper_hands_count"]
        matches_played = paper_hands_data["matches_played"]

        # Degen score
        degen_score = self.calculate_degen_score(discord_id)

        return GambaStats(
            discord_id=discord_id,
            total_bets=total_bets,
            wins=wins,
            losses=losses,
            win_rate=win_rate,
            net_pnl=net_pnl,
            total_wagered=total_wagered,
            roi=roi,
            avg_bet_size=avg_bet_size,
            leverage_distribution=leverage_distribution,
            current_streak=current_streak,
            best_streak=best_streak,
            worst_streak=worst_streak,
            peak_pnl=peak_pnl,
            trough_pnl=trough_pnl,
            biggest_win=biggest_win,
            biggest_loss=biggest_loss,
            degen_score=degen_score,
            paper_hands_count=paper_hands_count,
            matches_played=matches_played,
        )

    def calculate_degen_score(self, discord_id: int) -> DegenScoreBreakdown:
        """Calculate the degen score with component breakdown."""
        history = self.bet_repo.get_player_bet_history(discord_id)
        leverage_dist = self.bet_repo.get_player_leverage_distribution(discord_id)
        loss_chase_data = self.bet_repo.count_player_loss_chasing(discord_id)

        flavor_texts = []
        total_bets = sum(leverage_dist.values())

        # 1. Max leverage addiction (0-25) - % of bets at 5x
        if total_bets > 0:
            pct_5x = leverage_dist.get(5, 0) / total_bets
            max_leverage_score = min(int(pct_5x * MAX_LEVERAGE_WEIGHT), MAX_LEVERAGE_WEIGHT)

            if pct_5x >= 0.5:
                flavor_texts.append("5x addict")
            elif pct_5x >= 0.2:
                flavor_texts.append("5x enthusiast")
        else:
            max_leverage_score = 0

        # 2. Bet size (0-25) - avg effective bet vs typical income
        if history:
            total_wagered = sum(b["effective_bet"] for b in history)
            avg_bet = total_wagered / len(history)
            # Ratio: 1:1 = 0pts, 10:1+ = 25pts (linear scale)
            bet_ratio = avg_bet / TYPICAL_INCOME_PER_MATCH
            bet_size_score = min(int((bet_ratio - 1) / 9 * BET_SIZE_WEIGHT), BET_SIZE_WEIGHT)
            bet_size_score = max(0, bet_size_score)  # Floor at 0

            if bet_ratio >= 10:
                flavor_texts.append("high roller")
            elif bet_ratio >= 5:
                flavor_texts.append("big bets")
        else:
            bet_size_score = 0

        # 3. Debt depth (0-20) - lowest balance ever reached
        lowest_balance = self.player_repo.get_lowest_balance(discord_id)
        if lowest_balance is not None and lowest_balance < 0:
            # Scale: 0 = 0pts, -500 = 20pts
            from config import MAX_DEBT
            debt_ratio = min(abs(lowest_balance) / MAX_DEBT, 1.0)
            debt_depth_score = int(debt_ratio * DEBT_DEPTH_WEIGHT)

            if lowest_balance <= -400:
                flavor_texts.append("debt lord")
            elif lowest_balance <= -200:
                flavor_texts.append("deep in debt")
        else:
            debt_depth_score = 0

        # 4. Bankruptcy count (0-15) - each = 5pts, max 15
        bankruptcy_count = self.bet_repo.get_player_bankruptcy_count(discord_id)
        bankruptcy_score = min(bankruptcy_count * 5, BANKRUPTCY_WEIGHT)
        if bankruptcy_count >= 3:
            flavor_texts.append(f"{bankruptcy_count} bankruptcies")
        elif bankruptcy_count > 0:
            flavor_texts.append(f"{bankruptcy_count} bankruptcy")

        # 5. Bet frequency (0-10) - % of matches bet on
        matches_bet_on = len(set(b["match_id"] for b in history))
        with self.match_repo.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT COUNT(*) as count FROM matches WHERE winning_team IS NOT NULL")
            total_matches = cursor.fetchone()["count"]

        if total_matches > 0:
            frequency_rate = matches_bet_on / total_matches
            frequency_score = min(int(frequency_rate * FREQUENCY_WEIGHT), FREQUENCY_WEIGHT)
            if frequency_rate >= 0.8:
                flavor_texts.append("never misses")
        else:
            frequency_score = 0

        # 6. Loss chasing (0-5)
        loss_chase_rate = loss_chase_data["loss_chase_rate"]
        loss_chase_score = min(int(loss_chase_rate * LOSS_CHASE_WEIGHT), LOSS_CHASE_WEIGHT)
        if loss_chase_rate >= 0.5:
            flavor_texts.append("loss chaser")

        # 7. Negative loan bonus (+25 each, can exceed 100)
        negative_loan_bonus = 0
        if self.loan_service:
            loan_state = self.loan_service.get_state(discord_id)
            negative_loans = loan_state.negative_loans_taken
            negative_loan_bonus = negative_loans * NEGATIVE_LOAN_BONUS
            if negative_loans >= 1:
                flavor_texts.insert(0, f"borrowed while broke x{negative_loans}")

        # Total score (can exceed 100 with negative loan bonus)
        total = (
            max_leverage_score
            + bet_size_score
            + debt_depth_score
            + bankruptcy_score
            + frequency_score
            + loss_chase_score
            + negative_loan_bonus
        )

        # Get tier (capped at 100 for tier lookup)
        title, tagline, emoji = self._get_degen_tier(min(total, 100))

        return DegenScoreBreakdown(
            total=total,
            title=title,
            emoji=emoji,
            tagline=tagline,
            max_leverage_score=max_leverage_score,
            bet_size_score=bet_size_score,
            debt_depth_score=debt_depth_score,
            bankruptcy_score=bankruptcy_score,
            frequency_score=frequency_score,
            loss_chase_score=loss_chase_score,
            negative_loan_bonus=negative_loan_bonus,
            flavor_texts=flavor_texts[:3],  # Limit to 3 flavor texts
        )

    def get_leaderboard(
        self, guild_id: int | None, limit: int = 5, min_bets: int = 3
    ) -> Leaderboard:
        """Get server gambling leaderboard with multiple sections."""
        summaries = self.bet_repo.get_guild_gambling_summary(guild_id, min_bets=min_bets)

        # Build entries with degen scores
        entries: list[LeaderboardEntry] = []
        for s in summaries:
            degen = self.calculate_degen_score(s["discord_id"])
            entries.append(
                LeaderboardEntry(
                    discord_id=s["discord_id"],
                    total_bets=s["total_bets"],
                    wins=s["wins"],
                    losses=s["losses"],
                    win_rate=s["win_rate"],
                    net_pnl=s["net_pnl"],
                    avg_leverage=s["avg_leverage"],
                    degen_score=degen.total,
                    degen_title=degen.title,
                )
            )

        # Top earners (sorted by net P&L descending)
        top_earners = sorted(entries, key=lambda e: e.net_pnl, reverse=True)[:limit]

        # Down bad (sorted by net P&L ascending, only negative)
        down_bad = sorted([e for e in entries if e.net_pnl < 0], key=lambda e: e.net_pnl)[:limit]

        # Hall of degen (sorted by degen score descending)
        hall_of_degen = sorted(
            entries, key=lambda e: e.degen_score or 0, reverse=True
        )[:limit]

        # Server totals
        total_wagered = sum(s["total_wagered"] for s in summaries)
        total_bets = sum(s["total_bets"] for s in summaries)
        avg_degen = (
            sum(e.degen_score or 0 for e in entries) / len(entries)
            if entries
            else 0
        )

        # Count bankruptcies
        total_bankruptcies = sum(
            self.bet_repo.get_player_bankruptcy_count(s["discord_id"])
            for s in summaries
        )

        # Count total loans taken (server-wide aggregate stat)
        total_loans = 0
        with self.bet_repo.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT COALESCE(SUM(total_loans_taken), 0) as total
                FROM loan_state
                """
            )
            row = cursor.fetchone()
            if row:
                total_loans = row["total"]

        return Leaderboard(
            top_earners=top_earners,
            down_bad=down_bad,
            hall_of_degen=hall_of_degen,
            total_wagered=total_wagered,
            total_bets=total_bets,
            avg_degen_score=avg_degen,
            total_bankruptcies=total_bankruptcies,
            total_loans=total_loans,
        )

    def get_betting_impact_stats(self, discord_id: int) -> BettingImpactStats | None:
        """
        Calculate how others' bets performed on matches this player participated in.

        Returns None if player has no match participation with external bets.
        """
        bets = self.bet_repo.get_bets_on_player_matches(discord_id)

        if not bets:
            return None

        # Aggregate by bettor
        bettor_data: dict[int, dict] = {}
        for bet in bets:
            bettor_id = bet["bettor_id"]
            if bettor_id not in bettor_data:
                bettor_data[bettor_id] = {
                    "wagered_for": 0,
                    "wagered_against": 0,
                    "pnl_for": 0,
                    "pnl_against": 0,
                    "bets_for": 0,
                    "bets_against": 0,
                    "wins_for": 0,
                    "wins_against": 0,
                }

            data = bettor_data[bettor_id]
            if bet["bet_direction"] == "for":
                data["wagered_for"] += bet["effective_bet"]
                data["pnl_for"] += bet["profit"]
                data["bets_for"] += 1
                if bet["won"]:
                    data["wins_for"] += 1
            else:
                data["wagered_against"] += bet["effective_bet"]
                data["pnl_against"] += bet["profit"]
                data["bets_against"] += 1
                if bet["won"]:
                    data["wins_against"] += 1

        # Calculate totals
        total_wagered_for = sum(d["wagered_for"] for d in bettor_data.values())
        total_wagered_against = sum(d["wagered_against"] for d in bettor_data.values())
        supporters_net_pnl = sum(d["pnl_for"] for d in bettor_data.values())
        haters_net_pnl = sum(d["pnl_against"] for d in bettor_data.values())

        total_wagered = total_wagered_for + total_wagered_against
        market_favorability = total_wagered_for / total_wagered if total_wagered > 0 else 0.5
        supporter_roi = supporters_net_pnl / total_wagered_for if total_wagered_for > 0 else 0
        hater_roi = haters_net_pnl / total_wagered_against if total_wagered_against > 0 else 0

        # Count bets and win rates
        supporter_bets_count = sum(d["bets_for"] for d in bettor_data.values())
        hater_bets_count = sum(d["bets_against"] for d in bettor_data.values())
        supporter_wins = sum(d["wins_for"] for d in bettor_data.values())
        hater_wins = sum(d["wins_against"] for d in bettor_data.values())
        supporter_win_rate = supporter_wins / supporter_bets_count if supporter_bets_count > 0 else 0
        hater_win_rate = hater_wins / hater_bets_count if hater_bets_count > 0 else 0

        # Count unique matches with bets
        matches_with_bets = len(set(bet["match_id"] for bet in bets))
        total_bets = len(bets)

        # Find single bet extremes
        biggest_single_win = max((bet["profit"] for bet in bets if bet["profit"] > 0), default=0)
        biggest_single_loss = min((bet["profit"] for bet in bets if bet["profit"] < 0), default=0)

        # Find supporters (anyone who bet FOR at least once)
        supporters = {
            bid: d for bid, d in bettor_data.items() if d["wagered_for"] > 0
        }
        # Find haters (anyone who bet AGAINST at least once)
        haters = {
            bid: d for bid, d in bettor_data.items() if d["wagered_against"] > 0
        }

        def make_profile(bettor_id: int, data: dict) -> BettorProfile:
            return BettorProfile(
                discord_id=bettor_id,
                total_wagered_for=data["wagered_for"],
                total_wagered_against=data["wagered_against"],
                net_pnl_for=data["pnl_for"],
                net_pnl_against=data["pnl_against"],
                bets_for_count=data["bets_for"],
                bets_against_count=data["bets_against"],
                wins_for=data["wins_for"],
                wins_against=data["wins_against"],
            )

        # Find notable bettors - by wagered amount
        biggest_fan = None
        if supporters:
            fan_id = max(supporters.keys(), key=lambda k: supporters[k]["wagered_for"])
            biggest_fan = make_profile(fan_id, bettor_data[fan_id])

        biggest_hater = None
        if haters:
            hater_id = max(haters.keys(), key=lambda k: haters[k]["wagered_against"])
            biggest_hater = make_profile(hater_id, bettor_data[hater_id])

        # Find notable bettors - by consistency (bet count)
        most_consistent_fan = None
        if supporters:
            consistent_id = max(supporters.keys(), key=lambda k: supporters[k]["bets_for"])
            most_consistent_fan = make_profile(consistent_id, bettor_data[consistent_id])

        most_consistent_hater = None
        if haters:
            consistent_id = max(haters.keys(), key=lambda k: haters[k]["bets_against"])
            most_consistent_hater = make_profile(consistent_id, bettor_data[consistent_id])

        # Find notable bettors - by P&L
        blessing = None  # Good luck charm - profited most betting FOR
        if supporters:
            best_id = max(supporters.keys(), key=lambda k: supporters[k]["pnl_for"])
            if supporters[best_id]["pnl_for"] > 0:
                blessing = make_profile(best_id, bettor_data[best_id])

        jinx = None  # Bad luck charm - lost most betting FOR
        if supporters:
            worst_id = min(supporters.keys(), key=lambda k: supporters[k]["pnl_for"])
            if supporters[worst_id]["pnl_for"] < 0:
                jinx = make_profile(worst_id, bettor_data[worst_id])

        luckiest_hater = None
        if haters:
            lucky_id = max(haters.keys(), key=lambda k: haters[k]["pnl_against"])
            if haters[lucky_id]["pnl_against"] > 0:
                luckiest_hater = make_profile(lucky_id, bettor_data[lucky_id])

        return BettingImpactStats(
            discord_id=discord_id,
            matches_with_bets=matches_with_bets,
            total_bets=total_bets,
            total_wagered_for=total_wagered_for,
            total_wagered_against=total_wagered_against,
            supporters_net_pnl=supporters_net_pnl,
            haters_net_pnl=haters_net_pnl,
            supporter_win_rate=supporter_win_rate,
            hater_win_rate=hater_win_rate,
            supporter_bets_count=supporter_bets_count,
            hater_bets_count=hater_bets_count,
            market_favorability=market_favorability,
            supporter_roi=supporter_roi,
            hater_roi=hater_roi,
            biggest_fan=biggest_fan,
            biggest_hater=biggest_hater,
            most_consistent_fan=most_consistent_fan,
            most_consistent_hater=most_consistent_hater,
            blessing=blessing,
            jinx=jinx,
            luckiest_hater=luckiest_hater,
            biggest_single_win=biggest_single_win,
            biggest_single_loss=biggest_single_loss,
            unique_supporters=len(supporters),
            unique_haters=len(haters),
        )

    def get_cumulative_pnl_series(self, discord_id: int) -> list[tuple[int, int, dict]]:
        """
        Get cumulative P&L series for charting.

        Returns list of (bet_number, cumulative_pnl, bet_info) tuples.
        bet_info contains: amount, leverage, outcome, profit
        """
        history = self.bet_repo.get_player_bet_history(discord_id)
        series = []
        cumulative = 0

        for i, bet in enumerate(history, 1):
            cumulative += bet["profit"]
            series.append(
                (
                    i,
                    cumulative,
                    {
                        "amount": bet["amount"],
                        "leverage": bet["leverage"],
                        "effective_bet": bet["effective_bet"],
                        "outcome": bet["outcome"],
                        "profit": bet["profit"],
                        "team": bet["team_bet_on"],
                    },
                )
            )

        return series

    def _calculate_streaks(self, history: list[dict]) -> tuple[int, int, int]:
        """Calculate current, best, and worst streaks from bet history."""
        if not history:
            return 0, 0, 0

        current_streak = 0
        best_streak = 0
        worst_streak = 0
        streak = 0

        for bet in history:
            if bet["outcome"] == "won":
                if streak >= 0:
                    streak += 1
                else:
                    streak = 1
                best_streak = max(best_streak, streak)
            else:
                if streak <= 0:
                    streak -= 1
                else:
                    streak = -1
                worst_streak = min(worst_streak, streak)

        current_streak = streak
        return current_streak, best_streak, worst_streak

    def _calculate_pnl_extremes(self, history: list[dict]) -> tuple[int, int]:
        """Calculate peak and trough cumulative P&L from history."""
        if not history:
            return 0, 0

        cumulative = 0
        peak = 0
        trough = 0

        for bet in history:
            cumulative += bet["profit"]
            peak = max(peak, cumulative)
            trough = min(trough, cumulative)

        return peak, trough

    def _get_degen_tier(self, score: int) -> tuple[str, str, str]:
        """Get the degen tier title and tagline for a score."""
        emojis = ["🥱", "🎰", "🔥", "💀", "🎪", "👑"]

        for i, (low, high, title, tagline) in enumerate(DEGEN_TIERS):
            if low <= score <= high:
                return title, tagline, emojis[i]

        # Fallback for edge cases
        return "Unknown", "???", "❓"
