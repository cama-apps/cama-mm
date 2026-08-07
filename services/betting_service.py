"""
Handles betting-related business logic.
"""

import hashlib
import logging
import math
import time
from typing import TYPE_CHECKING, Any

logger = logging.getLogger("cama_bot.services.betting")

from config import (
    AUTO_BLIND_ENABLED,
    AUTO_BLIND_PERCENTAGE,
    AUTO_BLIND_THRESHOLD,
    AUTO_SPECTATOR_BET_COUNT,
    AUTO_SPECTATOR_BET_ENABLED,
    AUTO_SPECTATOR_BET_PERCENTAGE,
    AUTO_SPECTATOR_BET_TOP_COUNT,
    AUTO_SPECTATOR_BET_TOP_PERCENTAGE,
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
from domain.models.pending_match_state import PendingMatchState
from domain.pet_evolution import PetActivity
from repositories.bet_repository import BetRepository
from repositories.player_repository import PlayerRepository
from utils.economy_scaling import adjust_generated_jc_reward

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
        buff_service=None,
        economy_event_service=None,
        vanity_tax_service=None,
        pet_evolution_service=None,
    ):
        self.bet_repo = bet_repo
        self.player_repo = player_repo
        self.garnishment_service = garnishment_service
        self.leverage_tiers = leverage_tiers if leverage_tiers is not None else LEVERAGE_TIERS
        self.max_debt = max_debt if max_debt is not None else MAX_DEBT
        self.bankruptcy_service = bankruptcy_service
        self.buff_service = buff_service
        self.economy_event_service = economy_event_service
        self.vanity_tax_service = vanity_tax_service
        self.pet_evolution_service = pet_evolution_service

    def _economy_event_multiplier(self, guild_id: int | None, field: str) -> float:
        """Return one bounded daily-event multiplier, defaulting safely to 1x."""
        if self.economy_event_service is None:
            return 1.0
        try:
            effects = self.economy_event_service.get_effects(guild_id)
            value = float(getattr(effects, field, 1.0))
        except (AttributeError, TypeError, ValueError):
            logger.warning("Invalid economy event %s; using 1x", field, exc_info=True)
            return 1.0
        except Exception:
            logger.exception("Failed to load economy event %s; using 1x", field)
            return 1.0
        if not math.isfinite(value):
            logger.warning("Non-finite economy event %s; using 1x", field)
            return 1.0
        # Event definitions are trusted policy inputs, but bounding here prevents
        # a malformed row from creating negative payouts or an unbounded mint.
        return min(10.0, max(0.0, value))

    def _apply_blood_pact_skim(
        self,
        earner_id: int,
        guild_id: int | None,
        earning: int,
        *,
        match_id: int | None = None,
    ) -> int:
        if self.buff_service is None or earning <= 0:
            return 0
        if match_id is not None:
            return self.buff_service.apply_blood_pact_skim(
                earner_id,
                guild_id,
                earning,
                self.player_repo,
                source="dota_bet_blood_pact",
                related_type="match",
                related_id=match_id,
                event_key=f"dota-bet-blood-pact:{match_id}:{earner_id}",
                metadata={"match_id": match_id, "origin": "dota_bet"},
            )
        return self.buff_service.apply_blood_pact_skim(
            earner_id, guild_id, earning, self.player_repo
        )

    def _get_blood_pact_targets(
        self, player_ids: list[int], guild_id: int | None
    ) -> set[int]:
        """Snapshot active pact targets before processing an award batch."""
        if self.buff_service is None or not player_ids:
            return set()
        try:
            return set(
                self.buff_service.get_blood_pact_targets(player_ids, guild_id)
            )
        except Exception:
            logger.exception("Failed to bulk-load Blood Pact targets")
            return set()

    def _skim_blood_pact_from_awards(
        self,
        results: dict[int, dict[str, int]],
        guild_id: int | None,
        *,
        earning_key: str = "gross",
    ) -> None:
        """Apply Blood Pact skims to a batch of award results."""
        pact_targets = self._get_blood_pact_targets(list(results), guild_id)
        for pid, result in results.items():
            if pid not in pact_targets:
                continue
            earning = int(result.get(earning_key, 0))
            skimmed = self._apply_blood_pact_skim(pid, guild_id, earning)
            if skimmed:
                result["blood_pact_skimmed"] = skimmed
                result["net"] = int(result.get("net", 0)) - skimmed

    def _since_ts(self, pending_state: PendingMatchState | None) -> int | None:
        """Derive the start timestamp for the current pending match window."""
        if not pending_state:
            return None
        return pending_state.shuffle_timestamp

    def place_bet(
        self,
        guild_id: int | None,
        discord_id: int,
        team: str,
        amount: int,
        pending_state: PendingMatchState,
        leverage: int = 1,
    ) -> None:
        """Place a bet after verifying timing and participant/team rules."""
        if pending_state is None:
            raise ValueError("No pending match to bet on.")

        now_ts = int(time.time())
        # Fast/strict check first (also preserves test behavior where pending_state is mutated).
        lock_until = pending_state.bet_lock_until
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
        pending_match_id = pending_state.pending_match_id

        # Calculate odds at placement
        current_totals = self.bet_repo.get_total_bets_by_guild(
            guild_id, since_ts=int(since_ts), pending_match_id=pending_match_id
        )
        total_pool = current_totals["radiant"] + current_totals["dire"]
        team_total = current_totals[team]
        odds_at_placement = total_pool / team_total if team_total > 0 and total_pool > 0 else None

        # Atomic placement using DB pending match payload (enforces lock + team restriction).
        bet_id = self.bet_repo.place_bet_against_pending_match_atomic(
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
        if self.pet_evolution_service is not None:
            self.pet_evolution_service.record_activity(
                discord_id,
                guild_id,
                PetActivity.BET_PLACED,
                f"bet:{bet_id}",
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

        Base reward is JOPACOIN_PER_GAME. In bomb pot matches, all players
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

        pact_targets = self._get_blood_pact_targets(player_ids, guild_id)

        # With no pact transfers to interleave, all balance mutations can share
        # one transaction. Active-pact batches retain the point path below so a
        # skim that credits another participant can still affect that player's
        # subsequent live debt/garnishment check exactly as before.
        if not pact_targets:
            if self.garnishment_service:
                award_results = self.garnishment_service.add_income_many(
                    player_ids, total_reward, guild_id=guild_id
                )
            else:
                award_results = self.player_repo.add_balances_with_garnishment(
                    [(pid, total_reward, 0.0) for pid in player_ids],
                    guild_id,
                    garnishment_rate=0.0,
                )
            for pid, result in zip(player_ids, award_results, strict=True):
                result["bomb_pot_bonus"] = bomb_pot_bonus
                results[pid] = result
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
            skimmed = 0
            if pid in pact_targets:
                skimmed = self._apply_blood_pact_skim(
                    pid, guild_id, total_reward
                )
            if skimmed:
                result["blood_pact_skimmed"] = skimmed
                result["net"] = int(result.get("net", 0)) - skimmed
            results[pid] = result
        return results

    def settle_bets(
        self,
        match_id: int,
        guild_id: int | None,
        winning_team: str,
        pending_state: PendingMatchState,
        *,
        persist_match_jc_changes: bool = False,
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

        betting_mode = pending_state.betting_mode
        pending_match_id = pending_state.pending_match_id
        event_payout_multiplier = self._economy_event_multiplier(
            guild_id, "bet_payout_multiplier"
        )

        # Atomic settlement (payouts + bet tagging in one DB transaction). The
        # bankruptcy debuff is folded in here too: a penalized winner keeps only
        # the configured fraction of their profit (payout above their at-risk
        # stake). The debuff itself cannot turn profit negative; a disclosed
        # daily-event gross payout multiplier may. The withheld share is netted
        # out inside the same txn instead of through a follow-up debit with a
        # crash window. Stake basis is effective_bet (amount * leverage).
        distributions = self.bet_repo.settle_pending_bets_atomic(
            match_id=match_id,
            guild_id=guild_id,
            since_ts=int(since_ts),
            winning_team=winning_team,
            house_payout_multiplier=HOUSE_PAYOUT_MULTIPLIER,
            betting_mode=betting_mode,
            pending_match_id=pending_match_id,
            bankruptcy_penalty_rate=(
                self.bankruptcy_service.penalty_rate if self.bankruptcy_service else None
            ),
            vanity_tax_rate=(
                self.vanity_tax_service.TAX_RATE
                if self.vanity_tax_service
                else 0.0
            ),
            vanity_taxable_ids=(
                self.vanity_tax_service.taxable_ids(guild_id)
                if self.vanity_tax_service
                else frozenset()
            ),
            bet_seed_radiant=pending_state.bet_seed_radiant,
            bet_seed_dire=pending_state.bet_seed_dire,
            bet_seed_bonus=pending_state.bet_seed_bonus,
            payout_multiplier=event_payout_multiplier,
            persist_match_jc_changes=persist_match_jc_changes,
        )
        pending_state.bet_seed_reserved = 0
        pending_state.bet_seed_radiant = 0
        pending_state.bet_seed_dire = 0
        pending_state.bet_seed_bonus = 0

        if self.buff_service:
            # Skim what each winner actually received: the payout column stays
            # gross, but a bankruptcy-penalized winner only had payout - penalty
            # credited (the penalty is netted inside the settlement txn), so
            # aggregate profit per user and subtract their penalty first.
            pact_skims: dict[int, int] = {}
            penalties = distributions.get("bankruptcy_penalties", {})
            vanity_taxes = distributions.get("vanity_taxes", {})
            profits: dict[int, int] = {}
            for w in distributions.get("winners", []):
                pid = w["discord_id"]
                profits[pid] = (
                    profits.get(pid, 0)
                    + int(w.get("payout", 0))
                    - int(w.get("effective_bet", w.get("amount", 0)))
                )
            for loser in distributions.get("losers", []):
                pid = loser["discord_id"]
                if pid in profits:
                    profits[pid] -= int(
                        loser.get("effective_bet", loser.get("amount", 0))
                    )
            pact_targets = self._get_blood_pact_targets(list(profits), guild_id)
            for pid, profit in profits.items():
                net_profit = (
                    profit
                    - int(penalties.get(pid, 0))
                    - int(vanity_taxes.get(pid, 0))
                )
                if net_profit <= 0:
                    continue
                skimmed = 0
                if pid in pact_targets:
                    skimmed = self._apply_blood_pact_skim(
                        pid,
                        guild_id,
                        net_profit,
                        match_id=match_id if persist_match_jc_changes else None,
                    )
                if skimmed:
                    pact_skims[pid] = skimmed
            if pact_skims:
                distributions["blood_pact_skims"] = pact_skims

        return distributions

    def award_win_bonus(
        self, winning_ids: list[int], guild_id: int | None = None,
        *, match_id: int | None = None, reward_amount: int | None = None,
    ) -> dict[int, dict[str, int]]:
        """
        Reward winners with additional jopacoins.

        Applies bankruptcy penalty if applicable (reduced reward for players
        who declared bankruptcy). Also decrements bankruptcy penalty games
        for winners - only wins count toward clearing the penalty (like Dota 2 low prio).

        Returns dict of {discord_id: {gross, garnished, net, bankruptcy_penalty}} for each player.
        """
        # Attribute the credit to its match. The ledger row lands in the same
        # transaction as the balance change, so it is an atomic record that
        # this player has already been paid for this match — which a retry of
        # a partially-failed correction can check before paying again.
        ledger = (
            {
                "source": "match_win_bonus",
                "related_type": "match_win_bonus",
                "related_id": match_id,
                "reason": "match win bonus",
            }
            if match_id is not None
            else None
        )
        base_reward = JOPACOIN_WIN_REWARD if reward_amount is None else int(reward_amount)
        results = self._award_with_penalties(
            winning_ids, base_reward, guild_id, ledger=ledger,
        )

        # Decrement after awarding so the final required win is still reduced.
        if self.bankruptcy_service and winning_ids:
            self.bankruptcy_service.on_games_won(winning_ids, guild_id)

        # Manashop buffs that boost win bonus: Sanctuary (+15% for 24h) and
        # Communion blessing (+10% on the next match-win, single-charge).
        # The blessing bonus is gated on the atomic consume so two concurrent
        # match finalizations can't double-pay it.
        if self.buff_service and winning_ids:
            from services.buff_service import BUFF_COMMUNION_BLESSING

            try:
                communion_blessings = self.buff_service.buff_repo.active_for_many(
                    winning_ids, guild_id, BUFF_COMMUNION_BLESSING
                )
            except Exception:
                communion_blessings = {}
            for pid in winning_ids:
                credited = 0
                sanctuary_bonus = 0
                try:
                    if self.buff_service.has_sanctuary_match_bonus(pid, guild_id):
                        sanctuary_bonus = adjust_generated_jc_reward(
                            max(1, int(base_reward * 0.15)),
                            guild_id=guild_id,
                            economy_event_service=self.economy_event_service,
                        )
                except Exception:
                    sanctuary_bonus = 0
                if sanctuary_bonus > 0:
                    try:
                        self.player_repo.add_balance(
                            pid,
                            guild_id,
                            sanctuary_bonus,
                            source="manashop_buff",
                            related_type="sanctuary_match_bonus",
                            reason="sanctuary match-win bonus",
                            metadata={"bonus": sanctuary_bonus},
                        )
                        credited += sanctuary_bonus
                    except Exception:
                        logger.exception(
                            "Failed to credit manashop win bonus %d to player %d",
                            sanctuary_bonus, pid,
                        )
                # The blessing's one-shot charge is consumed and credited in a
                # single repository transaction, so a consumed charge can never
                # burn without its payout. Only the caller that wins the
                # conditional UPDATE gets True; a second concurrent match
                # finalization observes False and skips.
                blessing = communion_blessings.get(pid)
                if blessing:
                    blessing_bonus = adjust_generated_jc_reward(
                        max(1, int(base_reward * 0.10)),
                        guild_id=guild_id,
                        economy_event_service=self.economy_event_service,
                    )
                    try:
                        consumed = self.buff_service.buff_repo.consume_and_credit_atomic(
                            blessing[0]["id"], pid, guild_id, blessing_bonus
                        )
                    except Exception:
                        logger.exception(
                            "Failed to consume+credit blessing bonus %d for player %d",
                            blessing_bonus, pid,
                        )
                        consumed = False
                    if consumed:
                        credited += blessing_bonus
                if credited > 0:
                    results.setdefault(pid, {"gross": 0, "garnished": 0, "net": 0, "bankruptcy_penalty": 0})
                    results[pid]["net"] = int(results[pid].get("net", 0)) + credited
                    results[pid]["manashop_bonus"] = credited

        if self.buff_service:
            # Net already reflects garnishment, bankruptcy, and any manashop
            # bonuses, mirroring prediction settlement's skim base.
            self._skim_blood_pact_from_awards(
                results, guild_id, earning_key="net"
            )

        return results

    def award_exclusion_bonus(
        self, excluded_ids: list[int], guild_id: int | None = None
    ) -> dict[int, dict[str, int]]:
        """
        Reward excluded players with a small consolation bonus.

        Mirrors win bonus processing so bankruptcy and garnishment rules still apply.
        """
        results = self._award_with_penalties(excluded_ids, JOPACOIN_EXCLUSION_REWARD, guild_id)
        self._skim_blood_pact_from_awards(results, guild_id)
        return results

    def award_exclusion_bonus_half(
        self, excluded_ids: list[int], guild_id: int | None = None
    ) -> dict[int, dict[str, int]]:
        """
        Preserve the retired conditional-exclusion path's historical fallback.

        No live lobby creates these awards, but persisted legacy state may still
        reach this method. Keep its previous +1 JC behavior independent of the
        full exclusion reward.
        """
        results = self._award_with_penalties(excluded_ids, 1, guild_id)
        self._skim_blood_pact_from_awards(results, guild_id)
        return results

    def award_streaming_bonus(
        self, player_ids: list[int], guild_id: int | None = None
    ) -> dict[int, dict[str, int]]:
        """
        Reward streaming players (Go Live + Dota 2) with a jopacoin bonus.

        Same processing as other awards so bankruptcy and garnishment rules still apply.
        """
        from config import STREAMING_BONUS
        results = self._award_with_penalties(player_ids, STREAMING_BONUS, guild_id)
        self._skim_blood_pact_from_awards(results, guild_id)
        return results

    def _award_with_penalties(
        self, player_ids: list[int], reward_amount: int, guild_id: int | None = None,
        *, ledger: dict | None = None,
    ) -> dict[int, dict[str, int]]:
        """
        Award jopacoins to players, applying garnishment then bankruptcy penalty.

        Ordering matters: garnishment runs first so it operates on the full gross
        reward (and the full gross gets credited to the balance, paying down debt
        as intended). The bankruptcy penalty then applies to whatever the player
        would have "felt" as net income after garnishment. Applying the penalty
        first would shrink the pool garnishment sees and effectively over-take
        from debt repayment.

        Both the garnishment-conditional balance read and the bankruptcy-penalty
        debit now happen inside a single ``BEGIN IMMEDIATE`` in the repo, so a
        concurrent balance flip between steps can't drift the penalty base.

        Shared logic for win bonus, exclusion bonus, and half-exclusion bonus.

        Returns dict of {discord_id: {gross, garnished, net, bankruptcy_penalty}}.
        """
        results: dict[int, dict[str, int]] = {}
        if not player_ids:
            return results

        penalized_ids: set[int] = set()
        if self.bankruptcy_service and reward_amount > 0:
            bankruptcy_states = self.bankruptcy_service.get_bulk_states(
                player_ids, guild_id
            )
            penalized_ids = {
                pid
                for pid, state in bankruptcy_states.items()
                if state.penalty_games_remaining > 0
            }

        # Decide (in the service layer, where bankruptcy policy lives) which
        # players receive a nonzero penalty coefficient. The repository still
        # computes garnishment and the penalty from each player's live balance,
        # but all awards now share one BEGIN IMMEDIATE transaction.
        bankruptcy_penalty_rates: dict[int, float] = {}
        if self.bankruptcy_service and penalized_ids:
            penalty_rate = max(
                0.0, min(1.0, self.bankruptcy_service.penalty_rate)
            )
            bankruptcy_penalty_rates = dict.fromkeys(penalized_ids, penalty_rate)

        vanity_tax_rates: dict[int, float] = {}
        if self.vanity_tax_service and reward_amount > 0:
            taxable_ids = self.vanity_tax_service.taxable_ids(guild_id)
            vanity_tax_rates = {
                pid: self.vanity_tax_service.TAX_RATE
                for pid in player_ids
                if pid in taxable_ids
            }

        if self.garnishment_service:
            award_results = self.garnishment_service.add_income_many(
                player_ids,
                reward_amount,
                guild_id=guild_id,
                bankruptcy_penalty_rates=bankruptcy_penalty_rates,
                vanity_tax_rates=vanity_tax_rates,
                **(ledger or {}),
            )
        else:
            award_results = self.player_repo.add_balances_with_garnishment(
                [
                    (
                        pid,
                        reward_amount,
                        bankruptcy_penalty_rates.get(pid, 0.0),
                    )
                    for pid in player_ids
                ],
                guild_id,
                garnishment_rate=0.0,
                vanity_tax_rates=vanity_tax_rates,
                **(ledger or {}),
            )

        for pid, garn in zip(player_ids, award_results, strict=True):
            results[pid] = {
                "gross": garn["gross"],
                "garnished": garn["garnished"],
                "net": garn["net"],
                "bankruptcy_penalty": garn["bankruptcy_penalty"],
                "vanity_tax": garn["vanity_tax"],
            }

        return results

    def get_pot_odds(
        self, guild_id: int | None, pending_state: PendingMatchState | None = None
    ) -> dict[str, int]:
        """Return current bet totals by team for odds calculation."""
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return dict.fromkeys(self.bet_repo.VALID_TEAMS, 0)
        pending_match_id = pending_state.pending_match_id
        return self.bet_repo.get_total_bets_by_guild(
            guild_id, since_ts=since_ts, pending_match_id=pending_match_id
        )

    def get_pending_bet(
        self, guild_id: int | None, discord_id: int, pending_state: PendingMatchState | None = None
    ) -> dict | None:
        """Get the pending bet for a player."""
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return None
        pending_match_id = pending_state.pending_match_id
        return self.bet_repo.get_player_pending_bet(
            guild_id, discord_id, since_ts=since_ts, pending_match_id=pending_match_id
        )

    def get_pending_bets(
        self, guild_id: int | None, discord_id: int, pending_state: PendingMatchState | None = None
    ) -> list[dict]:
        """Get all pending bets for a player, ordered by bet_time."""
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return []
        pending_match_id = pending_state.pending_match_id
        return self.bet_repo.get_player_pending_bets(
            guild_id, discord_id, since_ts=since_ts, pending_match_id=pending_match_id
        )

    def get_top_voluntary_bettor(
        self, guild_id: int | None, pending_state: PendingMatchState | None = None
    ) -> dict | None:
        """Return the single largest voluntary (non-blind) bet for the pending match.

        Used to pick the "biggest bettor" called out in the last-call reminder.
        Returns None when there are no voluntary bets (e.g. only auto-liquidity).
        """
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return None
        bets = self.bet_repo.get_bets_for_pending_match(
            guild_id, since_ts=since_ts, pending_match_id=pending_state.pending_match_id
        )
        voluntary = [b for b in bets if not b.get("is_blind")]
        if not voluntary:
            return None
        return max(voluntary, key=lambda b: b.get("amount", 0))

    def refund_pending_bets(
        self, guild_id: int | None, pending_state: PendingMatchState | None,
        pending_match_id: int | None = None
    ) -> int:
        """
        Refund all pending bets for the current match window.

        Args:
            guild_id: Guild ID
            pending_state: The pending match state
            pending_match_id: Optional specific match ID for concurrent match support

        Returns the number of bets refunded.
        """
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return 0
        # Get pending_match_id from state if not provided
        if pending_match_id is None:
            pending_match_id = pending_state.pending_match_id

        refunded = self.bet_repo.refund_pending_bets_atomic(
            guild_id=guild_id,
            since_ts=int(since_ts),
            pending_match_id=pending_match_id,
            bet_seed_reserved=pending_state.bet_seed_reserved,
        )
        pending_state.bet_seed_reserved = 0
        pending_state.bet_seed_radiant = 0
        pending_state.bet_seed_dire = 0
        pending_state.bet_seed_bonus = 0
        pending_state.first_game_pool_reserved = 0
        return refunded

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
        - Each eligible player bets 10% of their balance (rounded to nearest int)

        Bomb pot mode (is_bomb_pot=True):
        - ALL players participate (mandatory, no threshold check)
        - Each player bets 15% of their balance + flat 10 JC ante
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
        blind_percentage = BOMB_POT_BLIND_PERCENTAGE if is_bomb_pot else AUTO_BLIND_PERCENTAGE
        if not AUTO_BLIND_ENABLED:
            return {
                "created": 0,
                "total_radiant": 0,
                "total_dire": 0,
                "percentage": blind_percentage,
                "bets": [],
                "skipped": [],
                "is_bomb_pot": is_bomb_pot,
            }

        result: dict[str, Any] = {
            "created": 0,
            "total_radiant": 0,
            "total_dire": 0,
            "percentage": blind_percentage,
            "bets": [],
            "skipped": [],
            "is_bomb_pot": is_bomb_pot,
        }

        # Fetch bet totals once before the loop (avoid N+1 queries)
        cached_totals = self.bet_repo.get_total_bets_by_guild(
            guild_id, since_ts=shuffle_timestamp, pending_match_id=pending_match_id
        )
        balances = self.player_repo.get_balances_bulk(
            radiant_ids + dire_ids, guild_id
        )

        automatic_bets: list[tuple[int, str, int]] = []
        # Size each bet from the one-query balance snapshot. Live balance and
        # debt validation still happen inside the shared write transaction.
        for team, player_ids in [("radiant", radiant_ids), ("dire", dire_ids)]:
            for discord_id in player_ids:
                balance = balances.get(discord_id, 0)

                if is_bomb_pot:
                    # Bomb pot: mandatory ante for everyone, no threshold check
                    # Per-player ante (e.g. Red mana tripled)
                    player_ante = (ante_overrides or {}).get(
                        discord_id, BOMB_POT_ANTE
                    )
                    # Calculate: configured percentage of balance + flat ante
                    percentage_amount = (
                        round(balance * blind_percentage) if balance > 0 else 0
                    )
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

                automatic_bets.append((discord_id, team, blind_amount))

        if not automatic_bets:
            return result

        with self.bet_repo.automatic_bet_batch() as place_bet:
            for discord_id, team, blind_amount in automatic_bets:
                # Odds are calculated in original placement order and totals
                # advance only after a successful savepoint.
                total_pool = cached_totals["radiant"] + cached_totals["dire"]
                team_total = cached_totals[team]
                odds_at_placement = (
                    total_pool / team_total
                    if team_total > 0 and total_pool > 0
                    else None
                )

                try:
                    place_bet(
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
                except ValueError as e:
                    result["skipped"].append({
                        "discord_id": discord_id,
                        "reason": str(e),
                    })
                    continue

                result["created"] += 1
                result["bets"].append({
                    "discord_id": discord_id,
                    "team": team,
                    "amount": blind_amount,
                })
                cached_totals[team] += blind_amount
                if team == "radiant":
                    result["total_radiant"] += blind_amount
                else:
                    result["total_dire"] += blind_amount

        return result

    def create_auto_spectator_bets(
        self,
        guild_id: int | None,
        radiant_ids: list[int],
        dire_ids: list[int],
        shuffle_timestamp: int,
        pending_match_id: int | None = None,
    ) -> dict[str, Any]:
        """
        Auto-wager for the richest spectators after shuffle.

        Spectators are registered players who are not on either team. The
        richest configured tier wagers the top percentage of current balance,
        followed by an equally sized tier at the base percentage. Team
        selection is deterministic pseudorandom on ties, otherwise it chooses
        the side that leaves spectator auto-wager totals closest to even.
        """
        top_count = (
            min(
                max(AUTO_SPECTATOR_BET_TOP_COUNT, 0),
                max(AUTO_SPECTATOR_BET_COUNT, 0),
            )
            if AUTO_SPECTATOR_BET_TOP_PERCENTAGE > 0
            else 0
        )
        result: dict[str, Any] = {
            "created": 0,
            "total_radiant": 0,
            "total_dire": 0,
            "percentage": AUTO_SPECTATOR_BET_PERCENTAGE,
            "top_percentage": AUTO_SPECTATOR_BET_TOP_PERCENTAGE,
            "total_count": AUTO_SPECTATOR_BET_COUNT,
            "top_count": top_count,
            "bets": [],
            "skipped": [],
        }
        if (
            not AUTO_SPECTATOR_BET_ENABLED
            or AUTO_SPECTATOR_BET_COUNT <= 0
            or max(
                AUTO_SPECTATOR_BET_TOP_PERCENTAGE,
                AUTO_SPECTATOR_BET_PERCENTAGE,
            ) <= 0
        ):
            return result

        participant_ids = set(radiant_ids) | set(dire_ids)
        candidates = self.player_repo.get_richest_players(
            guild_id,
            limit=(AUTO_SPECTATOR_BET_COUNT * 2) + len(participant_ids),
            min_balance=1,
        )
        spectators = [
            row for row in candidates
            if int(row["discord_id"]) not in participant_ids
        ]
        if not spectators:
            return result

        cached_totals = self.bet_repo.get_total_bets_by_guild(
            guild_id, since_ts=shuffle_timestamp, pending_match_id=pending_match_id
        )
        spectator_totals = {"radiant": 0, "dire": 0}
        with self.bet_repo.automatic_bet_batch() as place_bet:
            for spectator in spectators:
                if result["created"] >= AUTO_SPECTATOR_BET_COUNT:
                    break

                successful_rank = result["created"]
                discord_id = int(spectator["discord_id"])
                balance = int(spectator.get("jopacoin_balance") or 0)
                percentage = (
                    AUTO_SPECTATOR_BET_TOP_PERCENTAGE
                    if successful_rank < top_count
                    else AUTO_SPECTATOR_BET_PERCENTAGE
                )
                amount = round(balance * percentage)
                if amount < 1:
                    result["skipped"].append({
                        "discord_id": discord_id,
                        "reason": f"auto-wager amount {amount} < 1",
                    })
                    continue

                team = self._choose_auto_spectator_team(
                    amount=amount,
                    spectator_totals=spectator_totals,
                    guild_id=guild_id,
                    discord_id=discord_id,
                    shuffle_timestamp=shuffle_timestamp,
                    pending_match_id=pending_match_id,
                    index=successful_rank,
                )

                total_pool = cached_totals["radiant"] + cached_totals["dire"]
                team_total = cached_totals[team]
                odds_at_placement = (
                    total_pool / team_total
                    if team_total > 0 and total_pool > 0
                    else None
                )

                try:
                    place_bet(
                        guild_id=guild_id,
                        discord_id=discord_id,
                        team=team,
                        amount=amount,
                        bet_time=shuffle_timestamp,
                        since_ts=shuffle_timestamp,
                        leverage=1,
                        max_debt=self.max_debt,
                        is_blind=True,
                        odds_at_placement=odds_at_placement,
                        pending_match_id=pending_match_id,
                    )
                except ValueError as e:
                    result["skipped"].append({
                        "discord_id": discord_id,
                        "reason": str(e),
                    })
                    continue

                balance = int(spectator.get("jopacoin_balance") or 0)
                result["created"] += 1
                result["bets"].append({
                    "discord_id": discord_id,
                    "team": team,
                    "amount": amount,
                    "networth": balance,
                    "percentage": percentage,
                })
                cached_totals[team] += amount
                spectator_totals[team] += amount
                if team == "radiant":
                    result["total_radiant"] += amount
                else:
                    result["total_dire"] += amount

        return result

    def _choose_auto_spectator_team(
        self,
        *,
        amount: int,
        spectator_totals: dict[str, int],
        guild_id: int | None,
        discord_id: int,
        shuffle_timestamp: int,
        pending_match_id: int | None,
        index: int,
    ) -> str:
        radiant_after_diff = abs((spectator_totals["radiant"] + amount) - spectator_totals["dire"])
        dire_after_diff = abs(spectator_totals["radiant"] - (spectator_totals["dire"] + amount))
        if radiant_after_diff < dire_after_diff:
            return "radiant"
        if dire_after_diff < radiant_after_diff:
            return "dire"

        seed = f"{guild_id or 0}:{pending_match_id or shuffle_timestamp}:{discord_id}:{index}:auto-spectator"
        digest = hashlib.sha256(seed.encode("ascii")).digest()
        return "radiant" if digest[0] % 2 == 0 else "dire"

    def get_all_pending_bets(
        self, guild_id: int | None, pending_state: PendingMatchState | None = None
    ) -> list[dict]:
        """Get all pending bets for a guild (for /bets command)."""
        since_ts = self._since_ts(pending_state)
        if pending_state is None or since_ts is None:
            return []
        pending_match_id = pending_state.pending_match_id
        return self.bet_repo.get_bets_for_pending_match(
            guild_id, since_ts=since_ts, pending_match_id=pending_match_id
        )
