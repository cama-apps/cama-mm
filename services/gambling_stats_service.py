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


# Degen score weights (total = 100)
MAX_LEVERAGE_WEIGHT = 25  # % of bets at 5x leverage
BET_SIZE_WEIGHT = 25  # Avg bet size vs typical income
DEBT_DEPTH_WEIGHT = 20  # How deep into debt (lowest_balance_ever)
BANKRUPTCY_WEIGHT = 15  # Bankruptcy count
FREQUENCY_WEIGHT = 10  # % of matches bet on
LOSS_CHASE_WEIGHT = 5  # Rate of increasing bet after loss

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


class GamblingStatsService:
    """Service for calculating gambling statistics and degen scores."""

    def __init__(
        self,
        bet_repo: "BetRepository",
        player_repo: "PlayerRepository",
        match_repo: "MatchRepository",
        bankruptcy_service: "BankruptcyService | None" = None,
    ):
        self.bet_repo = bet_repo
        self.player_repo = player_repo
        self.match_repo = match_repo
        self.bankruptcy_service = bankruptcy_service

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

        # Total score
        total = (
            max_leverage_score
            + bet_size_score
            + debt_depth_score
            + bankruptcy_score
            + frequency_score
            + loss_chase_score
        )

        # Get tier
        title, tagline, emoji = self._get_degen_tier(total)

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

        return Leaderboard(
            top_earners=top_earners,
            down_bad=down_bad,
            hall_of_degen=hall_of_degen,
            total_wagered=total_wagered,
            total_bets=total_bets,
            avg_degen_score=avg_degen,
            total_bankruptcies=total_bankruptcies,
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
