"""
Flavor Text Service for AI-generated commentary.

Provides two types of AI-generated text:
1. Event roasts - Snarky comments for gambling events (loans, bankruptcy, bets)
2. Data insights - Natural language interpretation of stats
"""

from __future__ import annotations

import json
import logging
import random
from dataclasses import dataclass
from enum import Enum
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from repositories.interfaces import IGuildConfigRepository, IPlayerRepository
    from services.ai_service import AIService
    from services.bankruptcy_service import BankruptcyService
    from services.gambling_stats_service import GamblingStatsService
    from services.loan_service import LoanService

logger = logging.getLogger("cama_bot.services.flavor_text")


class FlavorEvent(Enum):
    """Events that trigger AI-generated flavor text."""

    LOAN_TAKEN = "loan_taken"
    NEGATIVE_LOAN = "negative_loan"  # Loan while in debt
    LOAN_COOLDOWN = "loan_cooldown"  # Tried to take loan during cooldown
    LOAN_DENIED_DEBT = "loan_denied_debt"  # Loan denied due to max debt
    BANKRUPTCY_DECLARED = "bankruptcy_declared"
    BANKRUPTCY_COOLDOWN = "bankruptcy_cooldown"  # Tried bankruptcy during cooldown
    DEBT_PAID = "debt_paid"
    BET_WON = "bet_won"
    BET_LOST = "bet_lost"
    LEVERAGE_LOSS = "leverage_loss"  # Lost leveraged bet
    MATCH_WIN = "match_win"
    MATCH_LOSS = "match_loss"
    SHOP_ANNOUNCE = "shop_announce"  # Balance announcement flex
    SHOP_ANNOUNCE_TARGET = "shop_announce_target"  # Targeted flex on another player
    DOUBLE_OR_NOTHING_WIN = "double_or_nothing_win"  # Won the 50/50 gamble
    DOUBLE_OR_NOTHING_LOSE = "double_or_nothing_lose"  # Lost everything
    DOUBLE_OR_NOTHING_ZERO = "double_or_nothing_zero"  # Had exactly 25 JC, doubled nothing


# Example messages for each event type (used as few-shot examples and fallbacks)
# These should be SPICY - the AI will use them as inspiration
EVENT_EXAMPLES: dict[FlavorEvent, list[str]] = {
    FlavorEvent.LOAN_TAKEN: [
        "Your financial advisor just felt a disturbance in the force.",
        "Congratulations on funding the nonprofit with your poor decisions.",
        "The bank approved your loan. They also started a betting pool on when you'll be back.",
        "Fresh jopacoin to lose! The circle of life continues.",
    ],
    FlavorEvent.NEGATIVE_LOAN: [
        "You borrowed money while already broke. This isn't gambling addiction, this is PERFORMANCE ART.",
        "Taking a loan in debt? Your credit score just became a war crime.",
        "Galaxy brain move: borrow money you can't use because you're too deep in the hole. Peak degen.",
        "The money went straight to your creditors. You gained nothing but our eternal respect, you absolute legend.",
        "This is the financial equivalent of lighting money on fire while drowning.",
    ],
    FlavorEvent.LOAN_COOLDOWN: [
        "Back already? The ink on your last loan isn't even dry yet.",
        "Slow down there, crackhead. One financial disaster at a time.",
        "The bank said no. Probably because you still owe them from 5 minutes ago.",
        "Loan machine broke. Come back when you've reflected on your life choices (3 days).",
    ],
    FlavorEvent.LOAN_DENIED_DEBT: [
        "Even loan sharks have standards. You've exceeded them.",
        "We'd give you money but it would just disappear into the void of your debt.",
        "The bank took one look at your balance and laughed. Out loud. In the meeting.",
        "Your credit score is so low it's measuring depth, not height.",
    ],
    FlavorEvent.BANKRUPTCY_COOLDOWN: [
        "You JUST declared bankruptcy. The judge is still laughing.",
        "One reset per week. We're not running a charity here. Oh wait, yes we are.",
        "The bankruptcy court has a restraining order against you at this point.",
        "Try winning a game or two before crawling back here.",
    ],
    FlavorEvent.BANKRUPTCY_DECLARED: [
        "Financial reset complete. Shame meter: still maxed.",
        "From zero to zero. The jopacoin cycle of poverty continues.",
        "The house always wins, and today the house took EVERYTHING.",
        "Bankruptcy #??? - at this point we've lost count.",
    ],
    FlavorEvent.DEBT_PAID: [
        "Wait, you're actually being responsible? This feels wrong.",
        "Debt-free! Quick, take out another loan before this feeling fades.",
        "The nonprofit fund just shed a single tear. They'll miss your donations.",
        "Look at you, adulting. It won't last.",
    ],
    FlavorEvent.BET_WON: [
        "Even a broken clock is right twice a day. This was one of those times.",
        "Don't get used to it. The house ALWAYS wins eventually.",
        "Temporary profit detected. Deploying gambling addiction to correct this.",
    ],
    FlavorEvent.BET_LOST: [
        "The house sends its regards. And by regards, we mean your money.",
        "Your jopacoin are in a better place now. Someone else's wallet.",
        "Maybe next time? (Narrator: It would not be next time.)",
        "L + ratio + you're broke now",
    ],
    FlavorEvent.LEVERAGE_LOSS: [
        "5x leverage? That's 5x the pain, baby. Math checks out.",
        "You leveraged yourself into a casket. Financially speaking.",
        "High risk, no reward. Classic.",
        "Leveraged losses hit different. And by different, we mean worse.",
    ],
    FlavorEvent.MATCH_WIN: [
        # Underdog examples
        "Wasn't supposed to win that one. Did anyway.",
        "Doubters in shambles.",
        # Big gainer examples
        "The grind is paying off.",
        "Rating stonks going up.",
        # General examples
        "Clean game.",
        "Another one for the books.",
    ],
    FlavorEvent.MATCH_LOSS: [
        "It's not about winning or losing, it's about— actually no, it's about winning. You lost.",
        "The rating system sends its condolences.",
        "Diff. Just diff.",
        "Go next (and probably lose that one too).",
    ],
    FlavorEvent.SHOP_ANNOUNCE: [
        "ATTENTION: Someone with poor financial priorities has entered the chat.",
        "I could have saved this money. But being obnoxious was more important.",
        "This announcement was brought to you by: questionable life decisions.",
        "POV: You're about to witness someone flex money they'll lose gambling.",
    ],
    FlavorEvent.SHOP_ANNOUNCE_TARGET: [
        "Hey, I paid good money for you to feel poor right now. You're welcome.",
        "Imagine having less jopacoin than me. Oh wait, you don't have to imagine. Just look at yours.",
        "I spent money specifically to remind you I'm better. That's how petty I am.",
        "The numbers don't lie, and right now they're saying 'get rekt'.",
    ],
    FlavorEvent.DOUBLE_OR_NOTHING_WIN: [
        "The coin gods smile upon you today. Enjoy it while it lasts.",
        "DOUBLED! The house weeps. (Don't worry, they'll get it back.)",
        "Fortune favors the bold. And apparently, the financially reckless.",
        "You absolute madman. It actually worked.",
        "Against all odds... just kidding, it was literally 50/50.",
        "Lady Luck is on your side! Quick, go lose it on something else.",
        "The spirits of the coin have blessed you. They're known for taking it back later.",
        "Congratulations on your temporary victory over probability.",
    ],
    FlavorEvent.DOUBLE_OR_NOTHING_LOSE: [
        "And it's gone. All of it. Every last jopacoin.",
        "The coin has spoken. You have nothing. Absolutely nothing.",
        "Fortune is a fickle mistress, and she just took EVERYTHING.",
        "Poof. Should have walked away. But you're not that person, are you?",
        "Your wealth has been redistributed to the void. The void thanks you.",
        "Gone. Reduced to atoms. Your jopacoin are in a better place now.",
        "The house always... wait, you did this to yourself.",
        "From hero to zero in one coin flip. Speedrun any%.",
        "Remember when you had money? That was nice.",
    ],
    FlavorEvent.DOUBLE_OR_NOTHING_ZERO: [
        "You paid 25 to double... nothing. Math is cruel.",
        "Congratulations! 0 x 2 = 0. The house thanks you for the donation.",
        "The coin landed in your favor! Unfortunately, 2 times nothing is still nothing.",
        "Victory! Your vast fortune of 0 has been doubled to... 0.",
        "You won the gamble but lost the war. Classic.",
        "Technically a win. Spiritually, a disaster.",
    ],
}


@dataclass
class PlayerContext:
    """Context about a player for personalized commentary."""

    username: str
    balance: int
    debt_amount: int | None
    win_rate: float | None
    degen_score: int | None
    bankruptcy_count: int
    total_bets: int
    total_loans: int
    # Extended context for better burns
    negative_loans: int  # Loans taken while in debt
    total_fees_paid: int  # Total loan fees paid
    biggest_win: int | None  # Biggest single bet payout
    biggest_loss: int | None  # Biggest single bet loss
    bet_win_rate: float | None  # Betting win rate
    times_in_debt: int  # How many times they've been in debt
    lowest_balance: int | None  # Lowest balance ever

    @classmethod
    def from_services(
        cls,
        discord_id: int,
        player_repo: IPlayerRepository,
        bankruptcy_service: BankruptcyService | None = None,
        loan_service: LoanService | None = None,
        gambling_stats_service: GamblingStatsService | None = None,
        guild_id: int | None = None,
    ) -> PlayerContext | None:
        """
        Build PlayerContext from various services.

        Returns None if player not found.
        """
        player = player_repo.get_by_id(discord_id, guild_id)
        if not player:
            return None

        balance = player.jopacoin_balance or 0
        debt_amount = abs(balance) if balance < 0 else None

        # Calculate win rate
        total_games = (player.wins or 0) + (player.losses or 0)
        win_rate = (player.wins or 0) / total_games * 100 if total_games > 0 else None

        # Get loan info
        total_loans = 0
        negative_loans = 0
        total_fees_paid = 0
        if loan_service:
            loan_state = loan_service.get_state(discord_id, guild_id)
            total_loans = loan_state.total_loans_taken
            negative_loans = getattr(loan_state, "negative_loans_taken", 0) or 0
            total_fees_paid = getattr(loan_state, "total_fees_paid", 0) or 0

        # Get degen score, gambling stats, and bankruptcy count
        degen_score = None
        biggest_win = None
        biggest_loss = None
        bet_win_rate = None
        total_bets = 0
        bankruptcy_count = 0
        if gambling_stats_service:
            try:
                stats = gambling_stats_service.get_player_stats(discord_id, guild_id)
                if stats:
                    # GambaStats is a dataclass, access attributes directly
                    degen_score = stats.degen_score.total if stats.degen_score else None
                    biggest_win = stats.biggest_win
                    biggest_loss = abs(stats.biggest_loss) if stats.biggest_loss else None
                    total_bets = stats.total_bets
                    if total_bets > 0:
                        bet_win_rate = stats.win_rate * 100
                # Get bankruptcy count from bet_repo
                bankruptcy_count = gambling_stats_service.bet_repo.get_player_bankruptcy_count(
                    discord_id, guild_id
                )
            except Exception:
                pass

        # Get lowest balance ever from player record
        lowest_balance = getattr(player, "lowest_balance_ever", None)

        return cls(
            username=player.name,
            balance=balance,
            debt_amount=debt_amount,
            win_rate=win_rate,
            degen_score=degen_score,
            bankruptcy_count=bankruptcy_count,
            total_bets=total_bets,
            total_loans=total_loans,
            negative_loans=negative_loans,
            total_fees_paid=total_fees_paid,
            biggest_win=biggest_win,
            biggest_loss=biggest_loss,
            bet_win_rate=bet_win_rate,
            times_in_debt=bankruptcy_count,  # Approximation
            lowest_balance=lowest_balance,
        )


class FlavorTextService:
    """
    Service for generating AI-powered flavor text.

    Supports:
    - Event-based roasts (loans, bankruptcy, betting)
    - Data interpretation (stats summaries in natural language)
    """

    def __init__(
        self,
        ai_service: AIService,
        player_repo: IPlayerRepository,
        bankruptcy_service: BankruptcyService | None = None,
        loan_service: LoanService | None = None,
        gambling_stats_service: GamblingStatsService | None = None,
        guild_config_repo: IGuildConfigRepository | None = None,
    ):
        self.ai_service = ai_service
        self.player_repo = player_repo
        self.bankruptcy_service = bankruptcy_service
        self.loan_service = loan_service
        self.gambling_stats_service = gambling_stats_service
        self.guild_config_repo = guild_config_repo

    def _get_fallback_flavor(self, event: FlavorEvent) -> str | None:
        """Get a random fallback flavor text from examples."""
        examples = EVENT_EXAMPLES.get(event, [])
        if examples:
            return random.choice(examples)
        return None

    async def generate_event_flavor(
        self,
        guild_id: int | None,
        event: FlavorEvent,
        discord_id: int,
        event_details: dict[str, Any],
    ) -> str | None:
        """
        Generate flavor text for an event.

        Falls back to random example text if AI is disabled or fails.

        Args:
            guild_id: Guild ID to check AI enabled
            event: Type of event
            discord_id: Player's Discord ID
            event_details: Event-specific details dict

        Returns:
            Generated comment string, fallback example, or None if no examples
        """
        # Get examples for this event type (used for AI context and fallback)
        examples = EVENT_EXAMPLES.get(event, [])
        logger.info(f"generate_event_flavor called: event={event.value}, guild={guild_id}, user={discord_id}")

        # Check if AI is enabled - fall back to examples if disabled
        ai_enabled = True
        if guild_id is not None and self.guild_config_repo:
            ai_enabled = self.guild_config_repo.get_ai_enabled(guild_id)

        if not ai_enabled:
            logger.info(f"AI disabled for guild {guild_id}, using fallback flavor")
            return self._get_fallback_flavor(event)

        # Build player context
        context = PlayerContext.from_services(
            discord_id,
            self.player_repo,
            self.bankruptcy_service,
            self.loan_service,
            self.gambling_stats_service,
            guild_id=guild_id,
        )

        if not context:
            # Can't build context, use fallback
            logger.warning(f"Could not build player context for {discord_id}, using fallback")
            return self._get_fallback_flavor(event)

        # Generate flavor text with full context
        try:
            player_context_dict = {
                "username": context.username,
                "balance": context.balance,
                "debt_amount": context.debt_amount,
                "win_rate": f"{context.win_rate:.0f}%" if context.win_rate else "Unknown",
                "degen_score": context.degen_score,
                "bankruptcy_count": context.bankruptcy_count,
                "total_loans": context.total_loans,
                "negative_loans": context.negative_loans,
                "total_fees_paid": context.total_fees_paid,
                "biggest_win": context.biggest_win,
                "biggest_loss": context.biggest_loss,
                "bet_win_rate": f"{context.bet_win_rate:.0f}%" if context.bet_win_rate else None,
                "total_bets": context.total_bets,
                "lowest_balance": context.lowest_balance,
            }

            logger.info(f"Calling AI service for {event.value}")
            result = await self.ai_service.generate_flavor(
                event_type=event.value,
                player_context=player_context_dict,
                event_details=event_details,
                examples=examples,
            )

            # If AI returns None, use fallback
            if result is None:
                logger.warning("AI returned None for flavor, using fallback")
                return self._get_fallback_flavor(event)

            logger.info(f"AI generated flavor: {result[:50]}..." if len(result) > 50 else f"AI generated flavor: {result}")
            return result
        except Exception as e:
            logger.error(f"Failed to generate flavor text: {e}, using fallback")
            return self._get_fallback_flavor(event)

    async def generate_data_insight(
        self,
        guild_id: int | None,
        data_type: str,
        data: dict[str, Any],
        context: str | None = None,
    ) -> str | None:
        """
        Generate natural language interpretation of stats data.

        Args:
            guild_id: Guild ID to check AI enabled
            data_type: Type of data (e.g., "pairwise", "matchup", "leaderboard")
            data: The stats data to interpret
            context: Optional additional context

        Returns:
            Generated insight string or None on error/disabled
        """
        # Check if AI is enabled
        if guild_id is not None and self.guild_config_repo:
            if not self.guild_config_repo.get_ai_enabled(guild_id):
                return None

        # Build prompt based on data type
        prompts = {
            "pairwise": self._build_pairwise_prompt,
            "matchup": self._build_matchup_prompt,
            "leaderboard": self._build_leaderboard_prompt,
        }

        prompt_builder = prompts.get(data_type)
        if not prompt_builder:
            logger.warning(f"Unknown data type for insight: {data_type}")
            return None

        prompt = prompt_builder(data, context)

        try:
            result = await self.ai_service.complete(
                prompt=prompt,
                system_prompt="""You are a helpful analyst for a Dota 2 inhouse league.
Generate a brief (2-3 sentences) natural language summary explaining the stats.
Focus on key takeaways: who should team with whom, who to avoid, notable patterns.
Be clear and informative. Keep it conversational but not too casual.""",
                temperature=0.7,
                max_tokens=2000,  # Reasoning models need more tokens to complete
            )
            return result
        except Exception as e:
            logger.error(f"Failed to generate data insight: {e}")
            return None

    def _build_pairwise_prompt(self, data: dict[str, Any], context: str | None) -> str:
        """Build prompt for pairwise stats interpretation."""
        player_name = data.get("player_name", "Player")
        best_teammates = data.get("best_teammates", [])
        worst_teammates = data.get("worst_teammates", [])
        best_matchups = data.get("best_matchups", [])
        worst_matchups = data.get("worst_matchups", [])

        parts = [f"Pairwise stats for {player_name}:"]

        if best_teammates:
            parts.append(f"Best teammates: {json.dumps(best_teammates[:3])}")
        if worst_teammates:
            parts.append(f"Worst teammates: {json.dumps(worst_teammates[:3])}")
        if best_matchups:
            parts.append(f"Dominates: {json.dumps(best_matchups[:3])}")
        if worst_matchups:
            parts.append(f"Struggles against: {json.dumps(worst_matchups[:3])}")

        if context:
            parts.append(f"Additional context: {context}")

        parts.append(
            "\nExplain what these stats mean for this player. "
            "Who should they queue with? Who should they avoid? Any notable patterns?"
        )
        return "\n".join(parts)

    def _build_matchup_prompt(self, data: dict[str, Any], context: str | None) -> str:
        """Build prompt for head-to-head matchup interpretation."""
        player1 = data.get("player1_name", "Player 1")
        player2 = data.get("player2_name", "Player 2")
        games_together = data.get("games_together", 0)
        wins_together = data.get("wins_together", 0)
        games_against = data.get("games_against", 0)
        player1_wins = data.get("player1_wins_against", 0)

        parts = [f"Head-to-head: {player1} vs {player2}"]

        if games_together > 0:
            together_rate = wins_together / games_together * 100
            parts.append(
                f"As teammates: {games_together} games, {wins_together} wins ({together_rate:.0f}%)"
            )
        else:
            parts.append("Never played together")

        if games_against > 0:
            parts.append(
                f"As opponents: {games_against} games, {player1} won {player1_wins}"
            )
        else:
            parts.append("Never played against each other")

        if context:
            parts.append(f"Additional context: {context}")

        parts.append(
            "\nExplain this head-to-head matchup. "
            "Should they team up? Who has the advantage when they face off? "
            "What does this say about their playstyles?"
        )
        return "\n".join(parts)

    def _build_leaderboard_prompt(self, data: dict[str, Any], context: str | None) -> str:
        """Build prompt for leaderboard interpretation."""
        top_players = data.get("top_players", [])
        bottom_players = data.get("bottom_players", [])
        total_players = data.get("total_players", 0)

        parts = [f"Leaderboard with {total_players} players:"]

        if top_players:
            parts.append(f"Top players: {json.dumps(top_players[:5])}")
        if bottom_players:
            parts.append(f"Wall of shame: {json.dumps(bottom_players[:3])}")

        if context:
            parts.append(f"Additional context: {context}")

        parts.append(
            "\nSummarize the leaderboard standings in a funny, engaging way. "
            "Highlight the leaders and playfully roast anyone in debt."
        )
        return "\n".join(parts)
