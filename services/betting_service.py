"""
Handles betting-related business logic.
"""

import math
import time
from typing import TYPE_CHECKING, Any, Optional

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
        garnishment_service: Optional["GarnishmentService"] = None,
        leverage_tiers: list[int] | None = None,
        max_debt: int | None = None,
        bankruptcy_service: Optional["BankruptcyService"] = None,
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
        if leverage != 1 and leverage not in self.leverage_tiers:
            valid_tiers = ", ".join(str(t) for t in self.leverage_tiers)
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

        # Prefer atomic placement using DB pending match payload (also enforces lock + team restriction).
        if hasattr(self.bet_repo, "place_bet_against_pending_match_atomic"):
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
            return

        # Fallback: enforce timing and team restriction using in-memory pending_state.
        self._enforce_team_restriction(discord_id, team, pending_state)

        # Prefer atomic placement (balance debit + bet insert in one transaction).
        if hasattr(self.bet_repo, "place_bet_atomic"):
            self.bet_repo.place_bet_atomic(
                guild_id=guild_id,
                discord_id=discord_id,
                team=team,
                amount=amount,
                bet_time=now_ts,
                since_ts=int(since_ts),
                leverage=leverage,
                max_debt=self.max_debt,
                odds_at_placement=odds_at_placement,
                pending_match_id=pending_match_id,
            )
            return

        # No atomic method available - this should not happen in production
        raise NotImplementedError(
            "BetRepository must implement place_bet_atomic or "
            "place_bet_against_pending_match_atomic for safe bet placement."
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

        # If garnishment service is available, use it for individual processing
        if self.garnishment_service:
            for pid in player_ids:
                result = self.garnishment_service.add_income(pid, total_reward, guild_id=guild_id)
                result["bomb_pot_bonus"] = bomb_pot_bonus
                results[pid] = result
            return results

        # Otherwise, bulk add without garnishment tracking
        deltas = dict.fromkeys(player_ids, total_reward)
        if hasattr(self.player_repo, "add_balance_many"):
            self.player_repo.add_balance_many(deltas, guild_id=guild_id)  # type: ignore[attr-defined]
        else:
            for pid in player_ids:
                self.player_repo.add_balance(pid, guild_id, total_reward)

        for pid in player_ids:
            results[pid] = {"gross": total_reward, "garnished": 0, "net": total_reward, "bomb_pot_bonus": bomb_pot_bonus}
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

        # Prefer atomic settlement (payouts + bet tagging in one DB transaction)
        if hasattr(self.bet_repo, "settle_pending_bets_atomic"):
            return self.bet_repo.settle_pending_bets_atomic(
                match_id=match_id,
                guild_id=guild_id,
                since_ts=int(since_ts),
                winning_team=winning_team,
                house_payout_multiplier=HOUSE_PAYOUT_MULTIPLIER,
                betting_mode=betting_mode,
                pending_match_id=pending_match_id,
            )

        # Fallback (older behavior) - only supports house mode
        bets = self.bet_repo.get_bets_for_pending_match(guild_id, since_ts=since_ts, pending_match_id=pending_match_id)
        distributions: dict[str, list[dict]] = {"winners": [], "losers": []}
        if not bets:
            return distributions

        self.bet_repo.assign_match_id(guild_id, match_id, since_ts=since_ts, pending_match_id=pending_match_id)

        if betting_mode == "pool":
            return self._settle_pool_bets_fallback(bets, winning_team, guild_id)
        else:
            return self._settle_house_bets_fallback(bets, winning_team, guild_id)

    def _settle_house_bets_fallback(
        self, bets: list[dict], winning_team: str, guild_id: int | None = None
    ) -> dict[str, list[dict]]:
        """House mode fallback: 1:1 payouts."""
        distributions: dict[str, list[dict]] = {"winners": [], "losers": []}

        for bet in bets:
            outcome_entry = {
                "discord_id": bet["discord_id"],
                "amount": bet["amount"],
                "team": bet["team_bet_on"],
            }
            if bet["team_bet_on"] != winning_team:
                distributions["losers"].append(outcome_entry)
                continue

            payout = int(bet["amount"] * (1 + HOUSE_PAYOUT_MULTIPLIER))
            self.player_repo.add_balance(bet["discord_id"], guild_id, payout)
            outcome_entry["payout"] = payout
            distributions["winners"].append(outcome_entry)

        return distributions

    def _settle_pool_bets_fallback(
        self, bets: list[dict], winning_team: str, guild_id: int | None = None
    ) -> dict[str, list[dict]]:
        """Pool mode fallback: proportional payouts from total pool."""
        distributions: dict[str, list[dict]] = {"winners": [], "losers": []}

        # Calculate totals
        total_pool = sum(bet["amount"] for bet in bets)
        winner_pool = sum(bet["amount"] for bet in bets if bet["team_bet_on"] == winning_team)

        # Edge case: no bets on winning side - refund all bets
        if winner_pool == 0:
            for bet in bets:
                self.player_repo.add_balance(bet["discord_id"], guild_id, bet["amount"])
                distributions["losers"].append(
                    {
                        "discord_id": bet["discord_id"],
                        "amount": bet["amount"],
                        "team": bet["team_bet_on"],
                        "refunded": True,
                    }
                )
            return distributions

        for bet in bets:
            outcome_entry = {
                "discord_id": bet["discord_id"],
                "amount": bet["amount"],
                "team": bet["team_bet_on"],
            }
            if bet["team_bet_on"] != winning_team:
                distributions["losers"].append(outcome_entry)
                continue

            # Proportional payout: (bet_amount / winner_pool) * total_pool
            # Round up to ensure winners never lose fractional coins
            payout = math.ceil((bet["amount"] / winner_pool) * total_pool)
            multiplier = total_pool / winner_pool
            self.player_repo.add_balance(bet["discord_id"], guild_id, payout)
            outcome_entry["payout"] = payout
            outcome_entry["multiplier"] = multiplier
            distributions["winners"].append(outcome_entry)

        return distributions

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
        results: dict[int, dict[str, int]] = {}
        if not winning_ids:
            return results

        # Decrement bankruptcy penalty games for winners only (wins clear bankruptcy)
        if self.bankruptcy_service:
            for pid in winning_ids:
                self.bankruptcy_service.on_game_won(pid, guild_id)

        for pid in winning_ids:
            reward = JOPACOIN_WIN_REWARD
            bankruptcy_penalty = 0

            # Apply bankruptcy penalty if applicable
            if self.bankruptcy_service:
                penalty_result = self.bankruptcy_service.apply_penalty_to_winnings(
                    pid, reward, guild_id
                )
                reward = penalty_result["penalized"]
                bankruptcy_penalty = penalty_result["penalty_applied"]

            # Apply garnishment if player has debt
            if self.garnishment_service:
                result = self.garnishment_service.add_income(pid, reward, guild_id=guild_id)
                result["bankruptcy_penalty"] = bankruptcy_penalty
                result["gross"] = JOPACOIN_WIN_REWARD  # Original before penalty
                results[pid] = result
            else:
                self.player_repo.add_balance(pid, guild_id, reward)
                results[pid] = {
                    "gross": JOPACOIN_WIN_REWARD,
                    "garnished": 0,
                    "net": reward,
                    "bankruptcy_penalty": bankruptcy_penalty,
                }

        return results

    def award_exclusion_bonus(
        self, excluded_ids: list[int], guild_id: int | None = None
    ) -> dict[int, dict[str, int]]:
        """
        Reward excluded players with a small consolation bonus.

        Mirrors win bonus processing so bankruptcy and garnishment rules still apply.
        """
        results: dict[int, dict[str, int]] = {}
        if not excluded_ids:
            return results

        for pid in excluded_ids:
            reward = JOPACOIN_EXCLUSION_REWARD
            bankruptcy_penalty = 0

            if self.bankruptcy_service:
                penalty_result = self.bankruptcy_service.apply_penalty_to_winnings(
                    pid, reward, guild_id
                )
                reward = penalty_result["penalized"]
                bankruptcy_penalty = penalty_result["penalty_applied"]

            if self.garnishment_service:
                result = self.garnishment_service.add_income(pid, reward, guild_id=guild_id)
                result["bankruptcy_penalty"] = bankruptcy_penalty
                result["gross"] = JOPACOIN_EXCLUSION_REWARD
                results[pid] = result
            else:
                self.player_repo.add_balance(pid, guild_id, reward)
                results[pid] = {
                    "gross": JOPACOIN_EXCLUSION_REWARD,
                    "garnished": 0,
                    "net": reward,
                    "bankruptcy_penalty": bankruptcy_penalty,
                }

        return results

    def award_exclusion_bonus_half(
        self, excluded_ids: list[int], guild_id: int | None = None
    ) -> dict[int, dict[str, int]]:
        """
        Reward conditional players excluded from shuffle with half the normal bonus.

        Same processing as award_exclusion_bonus but with JOPACOIN_EXCLUSION_REWARD // 2.
        """
        results: dict[int, dict[str, int]] = {}
        if not excluded_ids:
            return results

        half_reward = JOPACOIN_EXCLUSION_REWARD // 2
        for pid in excluded_ids:
            reward = half_reward
            bankruptcy_penalty = 0

            if self.bankruptcy_service:
                penalty_result = self.bankruptcy_service.apply_penalty_to_winnings(
                    pid, reward, guild_id
                )
                reward = penalty_result["penalized"]
                bankruptcy_penalty = penalty_result["penalty_applied"]

            if self.garnishment_service:
                result = self.garnishment_service.add_income(pid, reward, guild_id=guild_id)
                result["bankruptcy_penalty"] = bankruptcy_penalty
                result["gross"] = half_reward
                results[pid] = result
            else:
                self.player_repo.add_balance(pid, guild_id, reward)
                results[pid] = {
                    "gross": half_reward,
                    "garnished": 0,
                    "net": reward,
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

        if hasattr(self.bet_repo, "refund_pending_bets_atomic"):
            return self.bet_repo.refund_pending_bets_atomic(
                guild_id=guild_id, since_ts=int(since_ts), pending_match_id=pending_match_id
            )

        bets = self.bet_repo.get_bets_for_pending_match(guild_id, since_ts=since_ts, pending_match_id=pending_match_id)
        if not bets:
            return 0

        for bet in bets:
            self.player_repo.add_balance(bet["discord_id"], guild_id, bet["amount"])

        return self.bet_repo.delete_pending_bets(guild_id, since_ts=since_ts, pending_match_id=pending_match_id)

    def _enforce_team_restriction(self, discord_id: int, team: str, state: dict[str, Any]) -> None:
        radiant = set(state.get("radiant_team_ids", []))
        dire = set(state.get("dire_team_ids", []))
        if discord_id in radiant and team != "radiant":
            raise ValueError("Participants on Radiant can only bet on Radiant.")
        if discord_id in dire and team != "dire":
            raise ValueError("Participants on Dire can only bet on Dire.")

    def create_auto_blind_bets(
        self,
        guild_id: int | None,
        radiant_ids: list[int],
        dire_ids: list[int],
        shuffle_timestamp: int,
        is_bomb_pot: bool = False,
        pending_match_id: int | None = None,
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

        # Process each team
        for team, player_ids in [("radiant", radiant_ids), ("dire", dire_ids)]:
            for discord_id in player_ids:
                try:
                    balance = self.player_repo.get_balance(discord_id, guild_id)

                    if is_bomb_pot:
                        # Bomb pot: mandatory ante for everyone, no threshold check
                        # Calculate: 10% of balance + flat ante
                        percentage_amount = round(balance * blind_percentage) if balance > 0 else 0
                        blind_amount = percentage_amount + BOMB_POT_ANTE

                        # Ensure minimum bet is at least the ante
                        if blind_amount < BOMB_POT_ANTE:
                            blind_amount = BOMB_POT_ANTE
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

                    # Calculate current odds for this team before placing bet
                    current_totals = self.bet_repo.get_total_bets_by_guild(
                        guild_id, since_ts=shuffle_timestamp, pending_match_id=pending_match_id
                    )
                    total_pool = current_totals["radiant"] + current_totals["dire"]
                    team_total = current_totals[team]

                    # Odds at placement: what multiplier you'd get if you win
                    # If no bets yet, odds are undefined (will be calculated when more bets come in)
                    if team_total > 0:
                        # After this bet, total_pool increases and team_total increases
                        # Show the odds that existed before this bet
                        odds_at_placement = total_pool / team_total if total_pool > 0 else None
                    else:
                        # First bet on this team - no meaningful odds yet
                        odds_at_placement = None

                    # Place the blind bet
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
