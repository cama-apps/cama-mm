"""
Handles betting-related business logic.
"""

import logging
import time
from typing import TYPE_CHECKING, Any

logger = logging.getLogger("cama_bot.services.betting")

from config import (
    AUTO_BLIND_ENABLED,
    AUTO_BLIND_PERCENTAGE,
    AUTO_BLIND_THRESHOLD,
    BOMB_POT_ANTE,
    BOMB_POT_BLIND_PERCENTAGE,
    BOMB_POT_PARTICIPATION_BONUS,
    HOUSE_PAYOUT_MULTIPLIER,
    JOPACOIN_EXCLUSION_REWARD,
    JOPACOIN_PER_GAME,
    JOPACOIN_WIN_REWARD,
    LEVERAGE_TIERS,
    MAX_DEBT,
)
from repositories.bet_repository import BetRepository
from repositories.player_repository import PlayerRepository

if TYPE_CHECKING:
    from services.bankruptcy_service import BankruptcyService
    from services.garnishment_service import GarnishmentService


class BettingService:
    """Encapsulates jopacoin wagering, timing, and house payouts."""

    def __init__(
        self,
        bet_repo: BetRepository,
        player_repo: PlayerRepository,
        garnishment_service: "GarnishmentService | None" = None,
        leverage_tiers: list[int] | None = None,
        max_debt: int | None = None,
        bankruptcy_service: "BankruptcyService | None" = None,
    ):
        self.bet_repo = bet_repo
        self.player_repo = player_repo
        self.garnishment_service = garnishment_service
        self.leverage_tiers = leverage_tiers if leverage_tiers is not None else LEVERAGE_TIERS
        self.max_debt = max_debt if max_debt is not None else MAX_DEBT
        self.bankruptcy_service = bankruptcy_service

    def _since_ts(self, pending_state: dict[str, Any] | None) -> int | None:
        """Derive the start timestamp for the current pending match window."""
        if not pending_state:
            return None
        return pending_state.get("shuffle_timestamp")

    def place_bet(
        self,
        guild_id: int | None,
        discord_id: int,
        team: str,
        amount: int,
        pending_state: dict[str, Any],
        leverage: int = 1,
    ) -> None:
        """Place a bet after verifying timing and participant/team rules."""
        if pending_state is None:
            raise ValueError("No pending match to bet on.")

        now_ts = int(time.time())
        # Fast/strict check first (also preserves test behavior where pending_state is mutated).
        lock_until = pending_state.get("bet_lock_until")
        if lock_until is None or now_ts >= lock_until:
            raise ValueError("Betting is closed for the current match.")

        since_ts = self._since_ts(pending_state)
        if since_ts is None:
            raise ValueError("No pending match to bet on.")
        if team not in self.bet_repo.VALID_TEAMS:
            raise ValueError("Invalid team selection.")

        if amount <= 0:
            raise ValueError("Bet amount must be positive.")

        # Validate leverage tier
        # 10x is a valid tier (gated by Red mana in the command layer)
        valid_leverages = list(self.leverage_tiers) + [10]
        if leverage != 1 and leverage not in valid_leverages:
            valid_tiers = ", ".join(str(t) for t in valid_leverages)
            raise ValueError(f"Invalid leverage. Valid tiers: 1 (none), {valid_tiers}")

        # Get pending_match_id for concurrent match support
        pending_match_id = pending_state.get("pending_match_id")

        # Calculate odds at placement
        current_totals = self.bet_repo.get_total_bets_by_guild(
            guild_id, since_ts=int(since_ts), pending_match_id=pending_match_id
        )
        total_pool = current_totals["radiant"] + current_totals["dire"]
        team_total = current_totals[team]
        odds_at_placement = total_pool / team_total if team_total > 0 and total_pool > 0 else None

        # Atomic placement using DB pending match payload (enforces lock + team restriction).
        self.bet_repo.place_bet_against_pending_match_atomic(
            guild_id=guild_id,
            discord_id=discord_id,
            team=team,
            amount=amount,
            bet_time=now_ts,
            leverage=leverage,
            max_debt=self.max_debt,
            odds_at_placement=odds_at_placement,
            pending_match_id=pending_match_id,
        )

    def award_participation(
        self,
        player_ids: list[int],
        guild_id: int | None = None,
        is_bomb_pot: bool = False,
        bomb_pot_bonus_only: bool = False,
    ) -> dict[int, dict[str, int]]:
        """
        Give each participant jopacoin for playing.

        Base reward is JOPACOIN_PER_GAME (1). In bomb pot matches, all players
        receive an additional BOMB_POT_PARTICIPATION_BONUS (+1 JC).

        Args:
            player_ids: List of player Discord IDs to reward
            guild_id: Guild ID for multi-guild support
            is_bomb_pot: Whether this is a bomb pot match (adds bomb pot bonus)
            bomb_pot_bonus_only: If True, only give the bomb pot bonus (for winners
                who already get their reward through award_win_bonus)

        Note: Bankruptcy penalty games are NOT decremented here - only wins count
        toward clearing bankruptcy (like Dota 2 low priority). See award_win_bonus().

        Returns dict of {discord_id: {gross, garnished, net, bomb_pot_bonus}} for each player.
        """
        results: dict[int, dict[str, int]] = {}
        if not player_ids:
            return results

        # Calculate reward amount
        if bomb_pot_bonus_only:
            # Only give the bomb pot bonus (for winners in bomb pot mode)
            base_reward = 0
            bomb_pot_bonus = BOMB_POT_PARTICIPATION_BONUS if is_bomb_pot else 0
        else:
            # Normal participation (base + bomb pot bonus if applicable)
            base_reward = JOPACOIN_PER_GAME
            bomb_pot_bonus = BOMB_POT_PARTICIPATION_BONUS if is_bomb_pot else 0

        total_reward = base_reward + bomb_pot_bonus

        # Skip if nothing to award
        if total_reward <= 0:
            for pid in player_ids:
                results[pid] = {"gross": 0, "garnished": 0, "net": 0, "bomb_pot_bonus": 0}
            return results

        # Always credit each player atomically. When a garnishment service is
        # injected we delegate to it (which uses the atomic add_balance_with_garnishment
        # path under the hood). When no service is injected we still use the
        # atomic player_repo path with a zero garnishment rate, so the reported
        # result dict cannot diverge from the actual DB mutation even under
        # concurrent balance writes.
        for pid in player_ids:
            if self.garnishment_service:
                result = self.garnishment_service.add_income(pid, total_reward, guild_id=guild_id)
            else:
                result = self.player_repo.add_balance_with_garnishment(
                    pid, guild_id, total_reward, 0.0
                )
            result["bomb_pot_bonus"] = bomb_pot_bonus
            results[pid] = result
        return results

    def settle_bets(
        self, match_id: int, guild_id: int | None, winning_team: str, pending_state: dict[str, Any]
    ) -> dict[str, list[dict]]:
        """
        Settle bets based on betting mode.

        House mode: Pay winners 1:1 against the house.
        Pool mode: Winners split the total pool proportionally.
        """
        since_ts = self._since_ts(pending_state)
        if since_ts is None:
            # If no pending state, treat as no bets to avoid pulling stale wagers.
            return {"winners": [], "losers": []}

        betting_mode = pending_state.get("betting_mode", "pool")
        pending_match_id = pending_state.get("pending_match_id")

        # Atomic settlement (payouts + bet tagging in one DB transaction)
        return self.bet_repo.settle_pending_bets_atomic(
            match_id=match_id,
            guild_id=guild_id,
            since_ts=int(since_ts),
            winning_team=winning_team,
            house_payout_multiplier=HOUSE_PAYOUT_MULTIPLIER,
            betting_mode=betting_mode,
            pending_match_id=pending_match_id,
        )

    def award_win_bonus(
        self, winning_ids: list[int], guild_id: int | None = None
    ) -> dict[int, dict[str, int]]:
        """
        Reward winners with additional jopacoins.

        Applies bankruptcy penalty if applicable (reduced reward for players
        who declared bankruptcy). Also decrements bankruptcy penalty games
        for winners - only wins count toward clearing the penalty (like Dota 2 low prio).

        Returns dict of {discord_id: {gross, garnished, net, bankruptcy_penalty}} for each player.
        """
        # Decrement bankruptcy penalty games for winners only (wins clear bankruptcy)
        if self.bankruptcy_service and winning_ids:
            for pid in winning_ids:
                self.bankruptcy_service.on_game_won(pid, guild_id)

        return self._award_with_penalties(winning_ids, JOPACOIN_WIN_REWARD, guild_id)

    def award_exclusion_bonus(
        self, excluded_ids: list[int], guild_id: int | None = None
    ) -> dict[int, dict[str, int]]:
        """
        Reward excluded players with a small consolation bonus.

        Mirrors win bonus processing so bankruptcy and garnishment rules still apply.
        """
        return self._award_with_penalties(excluded_ids, JOPACOIN_EXCLUSION_REWARD, guild_id)

    def award_exclusion_bonus_half(
        self, excluded_ids: list[int], guild_id: int | None = None
    ) -> dict[int, dict[str, int]]:
        """
        Reward conditional players excluded from shuffle with half the normal bonus.

        Same processing as award_exclusion_bonus but with JOPACOIN_EXCLUSION_REWARD // 2.
        """
        return self._award_with_penalties(excluded_ids, JOPACOIN_EXCLUSION_REWARD // 2, guild_id)

    def award_streaming_bonus(
        self, player_ids: list[int], guild_id: int | None = None
    ) -> dict[int, dict[str, int]]:
        """
        Reward streaming players (Go Live + Dota 2) with a jopacoin bonus.

        Same processing as other awards so bankruptcy and garnishment rules still apply.
        """
        from config import STREAMING_BONUS
        return self._award_with_penalties(player_ids, STREAMING_BONUS, guild_id)

    def award_first_game_bonus(
        self, player_ids: list[int], guild_id: int | None = None
    ) -> dict[int, dict[str, int]]:
        """
        Reward all players in the first game of the night with a jopacoin bonus.

        Same processing as other awards so bankruptcy and garnishment rules still apply.
        """
        from config import FIRST_GAME_BONUS
        return self._award_with_penalties(player_ids, FIRST_GAME_BONUS, guild_id)

    def _award_with_penalties(
        self, player_ids: list[int], reward_amount: int, guild_id: int | None = None
    ) -> dict[int, dict[str, int]]:
        """
        Award jopacoins to players, applying garnishment then bankruptcy penalty.

        Ordering matters: garnishment runs first so it operates on the full gross
        reward (and the full gross gets credited to the balance, paying down debt
        as intended). The bankruptcy penalty then applies to whatever the player
        would have "felt" as net income after garnishment. Applying the penalty
        first would shrink the pool garnishment sees and effectively over-take
        from debt repayment.

        Shared logic for win bonus, exclusion bonus, and half-exclusion bonus.

        Returns dict of {discord_id: {gross, garnished, net, bankruptcy_penalty}}.
        """
        results: dict[int, dict[str, int]] = {}
        if not player_ids:
            return results

        for pid in player_ids:
            bankruptcy_penalty = 0

            # Apply garnishment FIRST on the full gross reward. The gross goes to
            # the balance; the service returns the split into garnished vs net.
            if self.garnishment_service:
                garn = self.garnishment_service.add_income(pid, reward_amount, guild_id=guild_id)
                gross = garn["gross"]
                garnished = garn["garnished"]
                net = garn["net"]
            else:
                # No garnishment service: entire reward is net.
                self.player_repo.add_balance(pid, guild_id, reward_amount)
                gross = reward_amount
                garnished = 0
                net = reward_amount

            # Apply bankruptcy penalty SECOND, on the net income the player
            # would have received. The penalty reduces the balance after the
            # fact; garnishment has already claimed its share of the gross.
            if self.bankruptcy_service:
                penalty_result = self.bankruptcy_service.apply_penalty_to_winnings(
                    pid, net, guild_id
                )
                penalized_net = penalty_result["penalized"]
                bankruptcy_penalty = penalty_result["penalty_applied"]
                if bankruptcy_penalty > 0:
                    # Remove the penalized portion from the balance (it stays
                    # "un-received"). The player keeps only the penalized_net.
                    self.player_repo.add_balance(pid, guild_id, -bankruptcy_penalty)
                    net = penalized_net

            results[pid] = {
                "gross": gross,
                "garnished": garnished,
                "net": net,
                "bankruptcy_penalty": bankruptcy_penalty,
            }

        return results

    def get_pot_odds(
        self, guild_id: int | None, pending_state: dict[str, Any] | None = None
    ) -> dict[str, int]:
        """Return current bet totals by team for odds calculation."""
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return dict.fromkeys(self.bet_repo.VALID_TEAMS, 0)
        pending_match_id = pending_state.get("pending_match_id")
        return self.bet_repo.get_total_bets_by_guild(
            guild_id, since_ts=since_ts, pending_match_id=pending_match_id
        )

    def get_pending_bet(
        self, guild_id: int | None, discord_id: int, pending_state: dict[str, Any] | None = None
    ) -> dict | None:
        """Get the pending bet for a player."""
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return None
        pending_match_id = pending_state.get("pending_match_id")
        return self.bet_repo.get_player_pending_bet(
            guild_id, discord_id, since_ts=since_ts, pending_match_id=pending_match_id
        )

    def get_pending_bets(
        self, guild_id: int | None, discord_id: int, pending_state: dict[str, Any] | None = None
    ) -> list[dict]:
        """Get all pending bets for a player, ordered by bet_time."""
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return []
        pending_match_id = pending_state.get("pending_match_id")
        return self.bet_repo.get_player_pending_bets(
            guild_id, discord_id, since_ts=since_ts, pending_match_id=pending_match_id
        )

    def refund_pending_bets(
        self, guild_id: int | None, pending_state: dict[str, Any] | None,
        pending_match_id: int | None = None
    ) -> int:
        """
        Refund all pending bets for the current match window.

        Args:
            guild_id: Guild ID
            pending_state: The pending match state dict
            pending_match_id: Optional specific match ID for concurrent match support

        Returns the number of bets refunded.
        """
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return 0
        # Get pending_match_id from state if not provided
        if pending_match_id is None:
            pending_match_id = pending_state.get("pending_match_id")

        return self.bet_repo.refund_pending_bets_atomic(
            guild_id=guild_id, since_ts=int(since_ts), pending_match_id=pending_match_id
        )

    def create_auto_blind_bets(
        self,
        guild_id: int | None,
        radiant_ids: list[int],
        dire_ids: list[int],
        shuffle_timestamp: int,
        is_bomb_pot: bool = False,
        pending_match_id: int | None = None,
        ante_overrides: dict[int, int] | None = None,
    ) -> dict[str, Any]:
        """
        Create auto-liquidity blind bets for all eligible players after shuffle.

        Normal mode:
        - Eligible players are those with balance >= AUTO_BLIND_THRESHOLD
        - Each eligible player bets 5% of their balance (rounded to nearest int)

        Bomb pot mode (is_bomb_pot=True):
        - ALL players participate (mandatory, no threshold check)
        - Each player bets 10% of their balance + flat 10 JC ante
        - Players can go negative (up to max_debt) to meet the ante

        Args:
            guild_id: The guild ID (or None for DMs)
            radiant_ids: List of Discord IDs on Radiant team
            dire_ids: List of Discord IDs on Dire team
            shuffle_timestamp: The shuffle timestamp for bet timing
            is_bomb_pot: Whether this is a bomb pot match (higher stakes, mandatory)
            pending_match_id: Optional specific match ID for concurrent match support
            ante_overrides: Optional per-player ante override (e.g. Red mana tripled ante)

        Returns:
            {
                "created": int,
                "total_radiant": int,
                "total_dire": int,
                "bets": [{"discord_id": int, "team": str, "amount": int}, ...],
                "skipped": [{"discord_id": int, "reason": str}, ...],
                "is_bomb_pot": bool
            }
        """
        logger.debug(
            f"create_auto_blind_bets called: guild={guild_id}, "
            f"pending_match_id={pending_match_id}, radiant={len(radiant_ids)}, dire={len(dire_ids)}"
        )
        if not AUTO_BLIND_ENABLED:
            return {
                "created": 0,
                "total_radiant": 0,
                "total_dire": 0,
                "bets": [],
                "skipped": [],
                "is_bomb_pot": is_bomb_pot,
            }

        result: dict[str, Any] = {
            "created": 0,
            "total_radiant": 0,
            "total_dire": 0,
            "bets": [],
            "skipped": [],
            "is_bomb_pot": is_bomb_pot,
        }

        # Choose percentage based on mode
        blind_percentage = BOMB_POT_BLIND_PERCENTAGE if is_bomb_pot else AUTO_BLIND_PERCENTAGE

        # Fetch bet totals once before the loop (avoid N+1 queries)
        cached_totals = self.bet_repo.get_total_bets_by_guild(
            guild_id, since_ts=shuffle_timestamp, pending_match_id=pending_match_id
        )

        # Process each team
        for team, player_ids in [("radiant", radiant_ids), ("dire", dire_ids)]:
            for discord_id in player_ids:
                try:
                    # Read balance immediately before computing amount + placing
                    # the bet. ``place_bet_atomic`` re-reads the balance under
                    # BEGIN IMMEDIATE and will reject if the player has flipped
                    # negative past ``max_debt`` between the read here and the
                    # atomic placement — so the atomic op is the source of truth
                    # and this read is only used to size the bet.
                    balance = self.player_repo.get_balance(discord_id, guild_id)

                    if is_bomb_pot:
                        # Bomb pot: mandatory ante for everyone, no threshold check
                        # Per-player ante (e.g. Red mana tripled)
                        player_ante = (ante_overrides or {}).get(discord_id, BOMB_POT_ANTE)
                        # Calculate: 10% of balance + flat ante
                        percentage_amount = round(balance * blind_percentage) if balance > 0 else 0
                        blind_amount = percentage_amount + player_ante

                        # Ensure minimum bet is at least the ante
                        if blind_amount < player_ante:
                            blind_amount = player_ante
                    else:
                        # Normal mode: skip players below threshold
                        if balance < AUTO_BLIND_THRESHOLD:
                            result["skipped"].append({
                                "discord_id": discord_id,
                                "reason": f"balance {balance} < threshold {AUTO_BLIND_THRESHOLD}",
                            })
                            continue

                        # Calculate blind amount (round to nearest integer)
                        blind_amount = round(balance * blind_percentage)

                        # Skip if rounded amount is less than 1
                        if blind_amount < 1:
                            result["skipped"].append({
                                "discord_id": discord_id,
                                "reason": f"blind amount {blind_amount} < 1",
                            })
                            continue

                    # Calculate current odds using cached totals (updated after each bet)
                    total_pool = cached_totals["radiant"] + cached_totals["dire"]
                    team_total = cached_totals[team]

                    # Odds at placement: what multiplier you'd get if you win
                    # If no bets yet, odds are undefined (will be calculated when more bets come in)
                    if team_total > 0:
                        # After this bet, total_pool increases and team_total increases
                        # Show the odds that existed before this bet
                        odds_at_placement = total_pool / team_total if total_pool > 0 else None
                    else:
                        # First bet on this team - no meaningful odds yet
                        odds_at_placement = None

                    # Place the blind bet. ``place_bet_atomic`` re-reads balance
                    # inside a BEGIN IMMEDIATE transaction and enforces:
                    #   - bomb pot: new_balance >= -max_debt
                    #   - normal:   balance >= 0 AND balance >= amount
                    # So if the balance has flipped past max_debt between our
                    # read above and this call, the atomic op raises and the
                    # bet lands in ``skipped`` — no over-leveraged blind is
                    # ever placed.
                    self.bet_repo.place_bet_atomic(
                        guild_id=guild_id,
                        discord_id=discord_id,
                        team=team,
                        amount=blind_amount,
                        bet_time=shuffle_timestamp,
                        since_ts=shuffle_timestamp,
                        leverage=1,
                        max_debt=self.max_debt,
                        is_blind=True,
                        odds_at_placement=odds_at_placement,
                        allow_negative=is_bomb_pot,  # Bomb pot antes can go into debt
                        pending_match_id=pending_match_id,
                    )

                    result["created"] += 1
                    result["bets"].append({
                        "discord_id": discord_id,
                        "team": team,
                        "amount": blind_amount,
                    })
                    # Update cached totals so next iteration has accurate odds
                    cached_totals[team] += blind_amount
                    if team == "radiant":
                        result["total_radiant"] += blind_amount
                    else:
                        result["total_dire"] += blind_amount

                except ValueError as e:
                    result["skipped"].append({
                        "discord_id": discord_id,
                        "reason": str(e),
                    })

        return result

    def get_all_pending_bets(
        self, guild_id: int | None, pending_state: dict[str, Any] | None = None
    ) -> list[dict]:
        """Get all pending bets for a guild (for /bets command)."""
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return []
        pending_match_id = pending_state.get("pending_match_id")
        return self.bet_repo.get_bets_for_pending_match(
            guild_id, since_ts=since_ts, pending_match_id=pending_match_id
        )
