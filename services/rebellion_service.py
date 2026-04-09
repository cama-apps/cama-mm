"""
Service for the Wheel War (Rebellion) feature.

Bankrupt players can incite the community to declare war on the Wheel of Fortune.
A public vote determines whether quorum is reached. If war is declared, a dramatic
battle roll resolves the conflict, leaving temporary effects on the wheel.
"""

import random
import time

from config import (
    BANKRUPTCY_COOLDOWN_SECONDS,
    REBELLION_ATTACK_QUORUM,
    REBELLION_ATTACKER_FLAT_REWARD,
    REBELLION_BASE_THRESHOLD,
    REBELLION_CELEBRATION_SPIN_WINDOW,
    REBELLION_DEFENDER_STAKE,
    REBELLION_DEFENDER_WIN_REWARD,
    REBELLION_FIRST_DEFENDER_BONUS,
    REBELLION_INCITER_COOLDOWN_SECONDS,
    REBELLION_INCITER_FLAT_REWARD,
    REBELLION_MAX_THRESHOLD,
    REBELLION_MIN_THRESHOLD,
    REBELLION_THRESHOLD_STEP,
    REBELLION_VETERAN_REBEL_MIN_BANKRUPTCIES,
    REBELLION_VETERAN_REBEL_VOTE_WEIGHT,
    REBELLION_VOTE_WINDOW_SECONDS,
    REBELLION_WHEEL_EFFECT_SPINS,
)
from repositories.bankruptcy_repository import BankruptcyRepository
from repositories.player_repository import PlayerRepository
from repositories.rebellion_repository import RebellionRepository


class RebellionService:
    """
    Orchestrates the full wheel war lifecycle:
      1. Eligibility check for /incite
      2. Vote processing (ATTACK / DEFEND)
      3. War declaration + meta-bet window
      4. Battle roll + outcome resolution
      5. Wheel effect tracking for /gamba
    """

    def __init__(
        self,
        rebellion_repo: RebellionRepository,
        bankruptcy_repo: BankruptcyRepository,
        player_repo: PlayerRepository,
    ):
        self.rebellion_repo = rebellion_repo
        self.bankruptcy_repo = bankruptcy_repo
        self.player_repo = player_repo

    # ------------------------------------------------------------------
    # Eligibility
    # ------------------------------------------------------------------

    def check_incite_eligibility(self, discord_id: int, guild_id: int) -> dict:
        """
        Check if a player can use /incite.

        Returns dict with:
            eligible: bool
            reason: str (if not eligible)
            bankruptcy_state: dict
        """
        now = int(time.time())

        # Check for active war in guild
        active = self.rebellion_repo.get_active_war(guild_id)
        if active:
            return {
                "eligible": False,
                "reason": "A rebellion is already in progress in this server.",
            }

        # Check inciter cooldown
        cooldown_until = self.rebellion_repo.get_inciter_cooldown(discord_id, guild_id)
        if cooldown_until and now < cooldown_until:
            remaining = cooldown_until - now
            days = remaining // 86400
            hours = (remaining % 86400) // 3600
            return {
                "eligible": False,
                "reason": f"You must wait {days}d {hours}h before inciting again.",
            }

        # Check bankruptcy eligibility
        bstate = self.bankruptcy_repo.get_state(discord_id, guild_id)
        if not bstate:
            return {
                "eligible": False,
                "reason": (
                    "Only players who have recently declared bankruptcy or are serving "
                    "penalty games may incite a rebellion."
                ),
            }

        last_bankruptcy_at = bstate.get("last_bankruptcy_at")
        penalty_games = bstate.get("penalty_games_remaining", 0)
        recently_bankrupt = (
            last_bankruptcy_at is not None
            and last_bankruptcy_at >= now - BANKRUPTCY_COOLDOWN_SECONDS
        )
        eligible = recently_bankrupt or penalty_games > 0

        if not eligible:
            return {
                "eligible": False,
                "reason": (
                    "Only players who declared bankruptcy within the past 7 days "
                    "or are still serving penalty games may incite a rebellion."
                ),
            }

        return {
            "eligible": True,
            "bankruptcy_count": bstate.get("bankruptcy_count", 0),
            "penalty_games_remaining": penalty_games,
        }

    # ------------------------------------------------------------------
    # War creation
    # ------------------------------------------------------------------

    def create_rebellion(self, discord_id: int, guild_id: int) -> dict:
        """
        Start a new rebellion. Inciter auto-votes ATTACK.

        Returns dict with war_id, vote_closes_at.
        """
        now = int(time.time())
        vote_closes_at = now + REBELLION_VOTE_WINDOW_SECONDS

        # Get inciter's bankruptcy count for veteran check
        bstate = self.bankruptcy_repo.get_state(discord_id, guild_id)
        bankruptcy_count = bstate.get("bankruptcy_count", 0) if bstate else 0

        war_id = self.rebellion_repo.create_war(
            guild_id=guild_id,
            inciter_id=discord_id,
            vote_closes_at=vote_closes_at,
            created_at=now,
        )

        # Update inciter's vote weight if veteran
        if bankruptcy_count >= REBELLION_VETERAN_REBEL_MIN_BANKRUPTCIES:
            # Update stored voter metadata and effective count to veteran weight.
            import json
            war = self.rebellion_repo.get_war(war_id)
            voters = json.loads(war["attack_voter_ids"])
            voters[0]["bankruptcy_count"] = bankruptcy_count
            # Update effective count to veteran weight
            with self.rebellion_repo.connection() as conn:
                cursor = conn.cursor()
                cursor.execute(
                    "UPDATE wheel_wars SET attack_voter_ids = ?, effective_attack_count = ? WHERE war_id = ?",
                    (json.dumps(voters), REBELLION_VETERAN_REBEL_VOTE_WEIGHT, war_id),
                )

        return {
            "war_id": war_id,
            "vote_closes_at": vote_closes_at,
            "bankruptcy_count": bankruptcy_count,
        }

    # ------------------------------------------------------------------
    # Voting
    # ------------------------------------------------------------------

    def process_attack_vote(self, war_id: int, discord_id: int, guild_id: int) -> dict:
        """
        Process an ATTACK vote. Free to cast.

        Returns dict with success, duplicate, effective_attack_count, message.
        """
        import json

        war = self.rebellion_repo.get_war(war_id)
        if not war:
            return {"success": False, "message": "War not found."}
        if war["status"] != "voting":
            return {"success": False, "message": "Voting is no longer open."}

        # Cannot vote DEFEND (they may vote ATTACK if they changed their mind - we prevent doubles)
        defend_voters = json.loads(war["defend_voter_ids"])
        if discord_id in defend_voters:
            return {"success": False, "message": "You already voted DEFEND for this rebellion."}

        # Check if already in attack voters
        attack_voters = json.loads(war["attack_voter_ids"])
        if any(v["discord_id"] == discord_id for v in attack_voters):
            return {"success": False, "duplicate": True, "message": "You already voted ATTACK."}

        # Get bankruptcy count for veteran check
        bstate = self.bankruptcy_repo.get_state(discord_id, guild_id)
        bankruptcy_count = bstate.get("bankruptcy_count", 0) if bstate else 0

        result = self.rebellion_repo.add_attack_vote(war_id, discord_id, bankruptcy_count)
        is_veteran = bankruptcy_count >= REBELLION_VETERAN_REBEL_MIN_BANKRUPTCIES

        return {
            "success": True,
            "duplicate": result.get("duplicate", False),
            "effective_attack_count": result["effective_attack_count"],
            "is_veteran": is_veteran,
            "bankruptcy_count": bankruptcy_count,
        }

    def process_defend_vote(self, war_id: int, discord_id: int, guild_id: int) -> dict:
        """
        Process a DEFEND vote. Costs REBELLION_DEFENDER_STAKE JC.

        Returns dict with success, duplicate, effective_defend_count, message.
        """
        import json

        war = self.rebellion_repo.get_war(war_id)
        if not war:
            return {"success": False, "message": "War not found."}
        if war["status"] != "voting":
            return {"success": False, "message": "Voting is no longer open."}

        # Inciter cannot vote DEFEND
        if discord_id == war["inciter_id"]:
            return {"success": False, "message": "The inciter cannot defend the Wheel."}

        defend_voters = json.loads(war["defend_voter_ids"])
        if discord_id in defend_voters:
            return {"success": False, "duplicate": True, "message": "You already voted DEFEND."}

        # Check balance
        balance = self.player_repo.get_balance(discord_id, guild_id)
        if balance < REBELLION_DEFENDER_STAKE:
            return {
                "success": False,
                "message": f"You need {REBELLION_DEFENDER_STAKE} JC to vote DEFEND (you have {balance}).",
            }

        # Deduct stake
        self.player_repo.add_balance(discord_id, guild_id, -REBELLION_DEFENDER_STAKE)

        result = self.rebellion_repo.add_defend_vote(war_id, discord_id)

        return {
            "success": True,
            "duplicate": result.get("duplicate", False),
            "effective_defend_count": result["effective_defend_count"],
            "stake_deducted": REBELLION_DEFENDER_STAKE,
        }

    # ------------------------------------------------------------------
    # Vote resolution
    # ------------------------------------------------------------------

    def resolve_vote(self, war_id: int) -> dict:
        """
        Evaluate vote results at timeout.

        Returns dict with outcome: 'war_declared' or 'fizzled', and counts.
        """
        import json

        war = self.rebellion_repo.get_war(war_id)
        if not war:
            return {"outcome": "fizzled", "reason": "War not found."}

        eff_attack = war["effective_attack_count"]
        eff_defend = war["effective_defend_count"]

        quorum_met = eff_attack >= REBELLION_ATTACK_QUORUM
        attack_wins = eff_attack > eff_defend

        if quorum_met and attack_wins:
            return {
                "outcome": "war_declared",
                "effective_attack_count": eff_attack,
                "effective_defend_count": eff_defend,
                "attack_voter_ids": json.loads(war["attack_voter_ids"]),
                "defend_voter_ids": json.loads(war["defend_voter_ids"]),
            }
        else:
            reason = "Quorum not reached." if not quorum_met else "Defenders held the line."
            return {
                "outcome": "fizzled",
                "reason": reason,
                "effective_attack_count": eff_attack,
                "effective_defend_count": eff_defend,
                "attack_voter_ids": json.loads(war["attack_voter_ids"]),
                "defend_voter_ids": json.loads(war["defend_voter_ids"]),
            }

    # ------------------------------------------------------------------
    # Fizzle resolution
    # ------------------------------------------------------------------

    def resolve_fizzle(self, war_id: int, guild_id: int) -> dict:
        """
        Handle a fizzled rebellion.
          - Refund defender stakes
          - Apply inciter cooldown
        """
        import json

        war = self.rebellion_repo.get_war(war_id)
        if not war:
            return {}

        now = int(time.time())
        defend_voters = json.loads(war["defend_voter_ids"])

        # Refund defender stakes
        for did in defend_voters:
            self.player_repo.add_balance(did, guild_id, REBELLION_DEFENDER_STAKE)

        # Apply inciter cooldown
        cooldown_until = now + REBELLION_INCITER_COOLDOWN_SECONDS
        self.rebellion_repo.set_inciter_cooldown(
            war_id, war["inciter_id"], guild_id, cooldown_until
        )
        self.rebellion_repo.set_fizzled(war_id, resolved_at=now)

        return {
            "inciter_id": war["inciter_id"],
            "defenders_refunded": defend_voters,
            "cooldown_until": cooldown_until,
        }

    # ------------------------------------------------------------------
    # Battle
    # ------------------------------------------------------------------

    def calculate_threshold(self, attack_count: float, defend_count: float) -> int:
        """Calculate the wheel victory threshold."""
        net_defenders = int(defend_count) - int(attack_count)
        raw = REBELLION_BASE_THRESHOLD + REBELLION_THRESHOLD_STEP * net_defenders
        return max(REBELLION_MIN_THRESHOLD, min(REBELLION_MAX_THRESHOLD, raw))

    def roll_battle(self) -> int:
        """Roll the wheel battle result (1–100)."""
        return random.randint(1, 100)

    def resolve_battle(
        self,
        war_id: int,
        guild_id: int,
        battle_roll: int,
        victory_threshold: int,
    ) -> dict:
        """
        Resolve the battle after the roll.

        Wheel wins if roll >= threshold. Attackers win if roll < threshold.
        Applies all rewards, penalties, and wheel effects.
        """
        import json

        war = self.rebellion_repo.get_war(war_id)
        if not war:
            return {}

        now = int(time.time())
        attack_voters = json.loads(war["attack_voter_ids"])
        defend_voters = json.loads(war["defend_voter_ids"])
        inciter_id = war["inciter_id"]
        attack_discord_ids = [v["discord_id"] for v in attack_voters]
        defend_discord_ids = list(defend_voters)

        defenders_win = battle_roll >= victory_threshold

        if defenders_win:
            outcome = "defenders_win"
            result = self._resolve_defenders_win(
                war_id=war_id,
                guild_id=guild_id,
                inciter_id=inciter_id,
                attack_discord_ids=attack_discord_ids,
                defend_discord_ids=defend_discord_ids,
                battle_roll=battle_roll,
                victory_threshold=victory_threshold,
                now=now,
            )
        else:
            outcome = "attackers_win"
            result = self._resolve_attackers_win(
                war_id=war_id,
                guild_id=guild_id,
                inciter_id=inciter_id,
                attack_discord_ids=attack_discord_ids,
                defend_discord_ids=defend_discord_ids,
                battle_roll=battle_roll,
                victory_threshold=victory_threshold,
                now=now,
            )

        result["outcome"] = outcome
        result["battle_roll"] = battle_roll
        result["victory_threshold"] = victory_threshold

        # Set inciter cooldown
        cooldown_until = now + REBELLION_INCITER_COOLDOWN_SECONDS
        self.rebellion_repo.set_inciter_cooldown(war_id, inciter_id, guild_id, cooldown_until)

        return result

    def _resolve_defenders_win(
        self,
        war_id: int,
        guild_id: int,
        inciter_id: int,
        attack_discord_ids: list[int],
        defend_discord_ids: list[int],
        battle_roll: int,
        victory_threshold: int,
        now: int,
    ) -> dict:
        """Handle defender victory outcome."""
        # Inciter: +1 penalty game
        self.bankruptcy_repo.upsert_state(
            discord_id=inciter_id,
            guild_id=guild_id,
            last_bankruptcy_at=self._get_last_bankruptcy_at(inciter_id, guild_id),
            penalty_games_remaining=self._get_penalty_games(inciter_id, guild_id) + 1,
        )

        # Attackers: +48h gamba cooldown (done in command layer via player_service)
        # Defenders: stake back + REBELLION_DEFENDER_WIN_REWARD each
        for did in defend_discord_ids:
            self.player_repo.add_balance(did, guild_id, REBELLION_DEFENDER_STAKE + REBELLION_DEFENDER_WIN_REWARD)

        # First defender bonus
        first_defender_id = defend_discord_ids[0] if defend_discord_ids else None
        if first_defender_id:
            self.player_repo.add_balance(first_defender_id, guild_id, REBELLION_FIRST_DEFENDER_BONUS)

        # Wheel effects: WAR TROPHY, RETRIBUTION, BANKRUPT +50%
        self.rebellion_repo.set_war_outcome(
            war_id=war_id,
            outcome="defenders_win",
            battle_roll=battle_roll,
            victory_threshold=victory_threshold,
            wheel_effect_spins_remaining=REBELLION_WHEEL_EFFECT_SPINS,
            war_scar_wedge_label=None,  # No scar on defender win
            celebration_spin_expires_at=None,
            resolved_at=now,
        )

        return {
            "inciter_penalty_added": 1,
            "defenders_rewarded": defend_discord_ids,
            "first_defender_id": first_defender_id,
            "attackers_penalized": attack_discord_ids,
            "wheel_effects": ["WAR_TROPHY", "RETRIBUTION", "BANKRUPT_STRENGTHEN"],
        }

    def _resolve_attackers_win(
        self,
        war_id: int,
        guild_id: int,
        inciter_id: int,
        attack_discord_ids: list[int],
        defend_discord_ids: list[int],
        battle_roll: int,
        victory_threshold: int,
        now: int,
    ) -> dict:
        """Handle attacker victory outcome."""
        # Inciter: +30 JC + cut penalty in half
        self.player_repo.add_balance(inciter_id, guild_id, REBELLION_INCITER_FLAT_REWARD)
        current_penalty = self._get_penalty_games(inciter_id, guild_id)
        new_penalty = current_penalty // 2
        if current_penalty > 0:
            self.bankruptcy_repo.upsert_state(
                discord_id=inciter_id,
                guild_id=guild_id,
                last_bankruptcy_at=self._get_last_bankruptcy_at(inciter_id, guild_id),
                penalty_games_remaining=new_penalty,
            )

        # All attackers: flat 15 JC + equal share of defender stakes
        defender_stake_pool = len(defend_discord_ids) * REBELLION_DEFENDER_STAKE
        stake_share = defender_stake_pool // len(attack_discord_ids) if attack_discord_ids else 0
        for did in attack_discord_ids:
            self.player_repo.add_balance(did, guild_id, REBELLION_ATTACKER_FLAT_REWARD + stake_share)

        # Pick a positive wedge to scar
        war_scar_label = self._pick_war_scar_wedge()

        # Wheel effects: WAR SCAR, BANKRUPT -25%, free celebration spins
        celebration_expires = now + REBELLION_CELEBRATION_SPIN_WINDOW
        self.rebellion_repo.set_war_outcome(
            war_id=war_id,
            outcome="attackers_win",
            battle_roll=battle_roll,
            victory_threshold=victory_threshold,
            wheel_effect_spins_remaining=REBELLION_WHEEL_EFFECT_SPINS,
            war_scar_wedge_label=war_scar_label,
            celebration_spin_expires_at=celebration_expires,
            resolved_at=now,
        )

        return {
            "inciter_reward": REBELLION_INCITER_FLAT_REWARD,
            "inciter_penalty_before": current_penalty,
            "inciter_penalty_after": new_penalty,
            "attacker_flat_reward": REBELLION_ATTACKER_FLAT_REWARD,
            "attacker_stake_share": stake_share,
            "defender_stake_pool": defender_stake_pool,
            "war_scar_label": war_scar_label,
            "celebration_spin_expires_at": celebration_expires,
            "wheel_effects": ["WAR_SCAR", "BANKRUPT_WEAKEN", "CELEBRATION_SPINS"],
        }

    def _pick_war_scar_wedge(self) -> str:
        """Pick a random positive wedge to be scarred."""
        # These are representative positive labels from the normal wheel
        positive_labels = [
            "5", "10", "15", "20", "25", "30", "35", "40", "45", "50",
            "55", "60", "65", "70", "75", "80", "85", "90", "95", "100",
        ]
        return random.choice(positive_labels)

    def _get_penalty_games(self, discord_id: int, guild_id: int) -> int:
        state = self.bankruptcy_repo.get_state(discord_id, guild_id)
        return state.get("penalty_games_remaining", 0) if state else 0

    def _get_last_bankruptcy_at(self, discord_id: int, guild_id: int) -> int:
        state = self.bankruptcy_repo.get_state(discord_id, guild_id)
        return state.get("last_bankruptcy_at", 0) if state else 0

    # ------------------------------------------------------------------
    # Wheel effect integration
    # ------------------------------------------------------------------

    def get_active_war_effect(self, guild_id: int) -> dict | None:
        """Get the active war effect state for a guild."""
        return self.rebellion_repo.get_active_war_effect(guild_id)

    def consume_war_spin(self, war_id: int, guild_id: int, spinner_id: int) -> int:
        """Consume one spin from the active war effect. Returns spins remaining."""
        return self.rebellion_repo.consume_war_spin(war_id, spinner_id)

    def check_and_use_celebration_spin(self, war_id: int, discord_id: int, guild_id: int) -> bool:
        """
        Check if this player has a free celebration spin available and mark it used.
        Returns True if the celebration spin was granted.
        """
        war = self.rebellion_repo.get_war(war_id)
        if not war:
            return False
        if war.get("outcome") != "attackers_win":
            return False
        expires = war.get("celebration_spin_expires_at")
        if not expires:
            return False
        now = int(time.time())
        if now > expires:
            return False
        return self.rebellion_repo.use_celebration_spin(war_id, discord_id)

    def is_attacker(self, war_id: int, discord_id: int) -> bool:
        """Check if a player was an attacker in the given war."""
        import json
        war = self.rebellion_repo.get_war(war_id)
        if not war:
            return False
        voters = json.loads(war.get("attack_voter_ids", "[]"))
        return any(v["discord_id"] == discord_id for v in voters)

    # ------------------------------------------------------------------
    # History / Stats
    # ------------------------------------------------------------------

    def get_war_history(self, guild_id: int, limit: int = 5) -> list[dict]:
        """Get recent war history for a guild."""
        return self.rebellion_repo.get_recent_wars(guild_id, limit)

    def get_player_war_stats(self, discord_id: int, guild_id: int) -> dict:
        """Get rebellion stats for a player."""
        return self.rebellion_repo.get_player_war_stats(discord_id, guild_id)

    def get_war_leaderboard(self, guild_id: int) -> list[dict]:
        """Get rebellion leaderboard."""
        return self.rebellion_repo.get_war_leaderboard(guild_id)
