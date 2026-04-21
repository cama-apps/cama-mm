"""
Repository for wheel war (rebellion) data access.

Handles CRUD for wheel_wars and war_bets tables.
"""

import json

from repositories.base_repository import BaseRepository, safe_json_loads
from repositories.interfaces import IRebellionRepository


class RebellionRepository(BaseRepository, IRebellionRepository):
    """Data access for wheel war feature."""

    def get_active_war(self, guild_id: int) -> dict | None:
        """Get the active (voting/betting/war) wheel war for a guild."""
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM wheel_wars
                WHERE guild_id = ? AND status IN ('voting', 'betting', 'war')
                ORDER BY war_id DESC LIMIT 1
                """,
                (normalized,),
            )
            row = cursor.fetchone()
            return dict(row) if row else None

    def get_war(self, war_id: int) -> dict | None:
        """Get a specific wheel war by ID."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT * FROM wheel_wars WHERE war_id = ?", (war_id,))
            row = cursor.fetchone()
            return dict(row) if row else None

    def create_war(
        self,
        guild_id: int,
        inciter_id: int,
        vote_closes_at: int,
        created_at: int,
    ) -> int:
        """Create a new wheel war. Returns war_id."""
        normalized = self.normalize_guild_id(guild_id)
        # Add inciter as first attacker automatically (bankruptcy_count fetched separately)
        initial_voters = json.dumps([{"discord_id": inciter_id, "bankruptcy_count": 0}])
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO wheel_wars
                    (guild_id, inciter_id, status, attack_voter_ids, defend_voter_ids,
                     effective_attack_count, effective_defend_count,
                     vote_closes_at, created_at)
                VALUES (?, ?, 'voting', ?, '[]', 1.0, 0.0, ?, ?)
                """,
                (normalized, inciter_id, initial_voters, vote_closes_at, created_at),
            )
            conn.commit()
            return cursor.lastrowid

    def add_attack_vote(self, war_id: int, discord_id: int, bankruptcy_count: int) -> dict:
        """Add an attack vote. Returns updated effective counts."""
        from config import (
            REBELLION_VETERAN_REBEL_MIN_BANKRUPTCIES,
            REBELLION_VETERAN_REBEL_VOTE_WEIGHT,
        )
        weight = (
            REBELLION_VETERAN_REBEL_VOTE_WEIGHT
            if bankruptcy_count >= REBELLION_VETERAN_REBEL_MIN_BANKRUPTCIES
            else 1.0
        )
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT attack_voter_ids, effective_attack_count FROM wheel_wars WHERE war_id = ?",
                (war_id,),
            )
            row = cursor.fetchone()
            if not row:
                raise ValueError(f"War {war_id} not found")
            voters = safe_json_loads(
                row["attack_voter_ids"],
                default=[],
                context=f"wheel_wars.attack_voter_ids war_id={war_id}",
            )
            # Prevent duplicate votes
            if any(v["discord_id"] == discord_id for v in voters):
                return {
                    "effective_attack_count": row["effective_attack_count"],
                    "duplicate": True,
                }
            voters.append({"discord_id": discord_id, "bankruptcy_count": bankruptcy_count})
            new_count = row["effective_attack_count"] + weight
            cursor.execute(
                """
                UPDATE wheel_wars
                SET attack_voter_ids = ?, effective_attack_count = ?
                WHERE war_id = ?
                """,
                (json.dumps(voters), new_count, war_id),
            )
            return {"effective_attack_count": new_count, "duplicate": False}

    def defend_vote_with_stake_atomic(
        self,
        war_id: int,
        discord_id: int,
        guild_id: int,
        stake: int,
    ) -> dict:
        """Atomically debit the defender stake and record the defend vote.

        Validates war status, non-inciter, non-duplicate, and sufficient
        balance inside one ``BEGIN IMMEDIATE`` so a crash can't burn the
        stake without recording a vote (or double-debit on retry).

        Returns a dict with at least ``effective_defend_count`` and one of:
          - ``stake_deducted: int`` on success
          - ``duplicate: True`` if the user already voted defend
        Raises ``ValueError`` with a user-facing message on any other failure.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            cursor.execute(
                """
                SELECT status, inciter_id, defend_voter_ids, effective_defend_count
                FROM wheel_wars WHERE war_id = ?
                """,
                (war_id,),
            )
            row = cursor.fetchone()
            if not row:
                raise ValueError("War not found.")
            if row["status"] != "voting":
                raise ValueError("Voting is no longer open.")
            if discord_id == row["inciter_id"]:
                raise ValueError("The inciter cannot defend the Wheel.")

            voters = safe_json_loads(
                row["defend_voter_ids"],
                default=[],
                context=f"wheel_wars.defend_voter_ids war_id={war_id}",
            )
            if discord_id in voters:
                return {
                    "effective_defend_count": row["effective_defend_count"],
                    "duplicate": True,
                }

            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) AS balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, normalized_guild),
            )
            bal_row = cursor.fetchone()
            if not bal_row:
                raise ValueError("Player not found.")
            balance = int(bal_row["balance"])
            if balance < stake:
                raise ValueError(
                    f"You need {stake} JC to vote DEFEND (you have {balance})."
                )

            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = jopacoin_balance - ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (stake, discord_id, normalized_guild),
            )

            voters.append(discord_id)
            new_count = row["effective_defend_count"] + 1.0
            cursor.execute(
                """
                UPDATE wheel_wars
                SET defend_voter_ids = ?, effective_defend_count = ?
                WHERE war_id = ?
                """,
                (json.dumps(voters), new_count, war_id),
            )
            return {
                "effective_defend_count": new_count,
                "duplicate": False,
                "stake_deducted": stake,
            }

    def add_defend_vote(self, war_id: int, discord_id: int) -> dict:
        """Add a defend vote. Returns updated defend count. Stake deducted separately by service."""
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT defend_voter_ids, effective_defend_count FROM wheel_wars WHERE war_id = ?",
                (war_id,),
            )
            row = cursor.fetchone()
            if not row:
                raise ValueError(f"War {war_id} not found")
            voters = safe_json_loads(
                row["defend_voter_ids"],
                default=[],
                context=f"wheel_wars.defend_voter_ids war_id={war_id}",
            )
            # Prevent duplicate votes
            if discord_id in voters:
                return {"effective_defend_count": row["effective_defend_count"], "duplicate": True}
            voters.append(discord_id)
            new_count = row["effective_defend_count"] + 1.0
            cursor.execute(
                """
                UPDATE wheel_wars
                SET defend_voter_ids = ?, effective_defend_count = ?
                WHERE war_id = ?
                """,
                (json.dumps(voters), new_count, war_id),
            )
            return {"effective_defend_count": new_count, "duplicate": False}

    def update_war_status(self, war_id: int, status: str) -> None:
        """Update war status."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE wheel_wars SET status = ? WHERE war_id = ?",
                (status, war_id),
            )

    def atomic_resolve_fizzle(
        self,
        *,
        war_id: int,
        guild_id: int,
        defender_ids: list[int],
        defender_stake: int,
        inciter_cooldown_until: int,
        resolved_at: int,
    ) -> None:
        """Refund defender stakes and mark the war fizzled in one txn.

        Without this, a crash between refunding stake N and stake N+1 would
        leave some stakes burned and the war stuck in 'voting'; a retry would
        double-refund the stakes that already committed.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            if defender_ids:
                cursor.executemany(
                    """
                    UPDATE players
                    SET jopacoin_balance = jopacoin_balance + ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    [(defender_stake, did, normalized_guild) for did in defender_ids],
                )
            cursor.execute(
                """
                UPDATE wheel_wars
                SET status = 'fizzled', outcome = 'fizzled',
                    resolved_at = ?, inciter_cooldown_until = ?
                WHERE war_id = ?
                """,
                (resolved_at, inciter_cooldown_until, war_id),
            )

    def atomic_resolve_defenders_win(
        self,
        *,
        war_id: int,
        guild_id: int,
        inciter_id: int,
        defender_ids: list[int],
        first_defender_id: int | None,
        per_defender_credit: int,
        first_defender_bonus: int,
        battle_roll: int,
        victory_threshold: int,
        wheel_effect_spins: int,
        inciter_cooldown_until: int,
        resolved_at: int,
    ) -> None:
        """Apply the full defenders-win outcome atomically.

        Credits every defender by ``per_defender_credit``, adds the extra
        ``first_defender_bonus`` to ``first_defender_id``, bumps the inciter's
        penalty_games_remaining by +1 (preserving the upsert_state increment
        of bankruptcy_count for bug-compat), and flips wheel_wars to
        resolved/defenders_win with the wheel-effect spins, resolved_at, and
        inciter cooldown — all inside one BEGIN IMMEDIATE.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            if defender_ids:
                cursor.executemany(
                    """
                    UPDATE players
                    SET jopacoin_balance = jopacoin_balance + ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    [(per_defender_credit, did, normalized_guild) for did in defender_ids],
                )
            if first_defender_id is not None and first_defender_bonus > 0:
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = jopacoin_balance + ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (first_defender_bonus, first_defender_id, normalized_guild),
                )

            cursor.execute(
                """
                SELECT last_bankruptcy_at,
                       COALESCE(penalty_games_remaining, 0) AS penalty_games_remaining
                FROM bankruptcy_state
                WHERE discord_id = ? AND guild_id = ?
                """,
                (inciter_id, normalized_guild),
            )
            row = cursor.fetchone()
            current_last_at = row["last_bankruptcy_at"] if row else None
            current_penalty = row["penalty_games_remaining"] if row else 0

            cursor.execute(
                """
                INSERT INTO bankruptcy_state (
                    discord_id, guild_id, last_bankruptcy_at,
                    penalty_games_remaining, bankruptcy_count, updated_at
                )
                VALUES (?, ?, ?, ?, 1, CURRENT_TIMESTAMP)
                ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                    last_bankruptcy_at = excluded.last_bankruptcy_at,
                    penalty_games_remaining = excluded.penalty_games_remaining,
                    bankruptcy_count = COALESCE(bankruptcy_state.bankruptcy_count, 0) + 1,
                    updated_at = CURRENT_TIMESTAMP
                """,
                (inciter_id, normalized_guild, current_last_at, current_penalty + 1),
            )

            cursor.execute(
                """
                UPDATE wheel_wars SET
                    status = 'resolved',
                    outcome = 'defenders_win',
                    battle_roll = ?,
                    victory_threshold = ?,
                    wheel_effect_spins_remaining = ?,
                    war_scar_wedge_label = NULL,
                    celebration_spin_expires_at = NULL,
                    resolved_at = ?,
                    inciter_cooldown_until = ?
                WHERE war_id = ?
                """,
                (
                    battle_roll, victory_threshold, wheel_effect_spins,
                    resolved_at, inciter_cooldown_until, war_id,
                ),
            )

    def atomic_resolve_attackers_win(
        self,
        *,
        war_id: int,
        guild_id: int,
        inciter_id: int,
        attacker_ids: list[int],
        per_attacker_credit: int,
        inciter_flat_reward: int,
        battle_roll: int,
        victory_threshold: int,
        wheel_effect_spins: int,
        war_scar_label: str,
        celebration_expires_at: int,
        inciter_cooldown_until: int,
        resolved_at: int,
    ) -> dict:
        """Apply the full attackers-win outcome atomically.

        Credits the inciter (+ ``inciter_flat_reward``), credits each attacker
        by ``per_attacker_credit``, halves the inciter's
        penalty_games_remaining if > 0 (preserving upsert_state's
        bankruptcy_count bump for bug-compat), and flips wheel_wars to
        resolved/attackers_win with war_scar, celebration_spin, and inciter
        cooldown — all inside one BEGIN IMMEDIATE.

        Returns ``{"inciter_penalty_before": int, "inciter_penalty_after": int}``
        so the service can build its response payload.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = jopacoin_balance + ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (inciter_flat_reward, inciter_id, normalized_guild),
            )

            if attacker_ids:
                cursor.executemany(
                    """
                    UPDATE players
                    SET jopacoin_balance = jopacoin_balance + ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    [(per_attacker_credit, did, normalized_guild) for did in attacker_ids],
                )

            cursor.execute(
                """
                SELECT last_bankruptcy_at,
                       COALESCE(penalty_games_remaining, 0) AS penalty_games_remaining
                FROM bankruptcy_state
                WHERE discord_id = ? AND guild_id = ?
                """,
                (inciter_id, normalized_guild),
            )
            row = cursor.fetchone()
            inciter_penalty_before = row["penalty_games_remaining"] if row else 0
            new_penalty = inciter_penalty_before // 2

            if inciter_penalty_before > 0:
                last_at = row["last_bankruptcy_at"]
                cursor.execute(
                    """
                    INSERT INTO bankruptcy_state (
                        discord_id, guild_id, last_bankruptcy_at,
                        penalty_games_remaining, bankruptcy_count, updated_at
                    )
                    VALUES (?, ?, ?, ?, 1, CURRENT_TIMESTAMP)
                    ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                        last_bankruptcy_at = excluded.last_bankruptcy_at,
                        penalty_games_remaining = excluded.penalty_games_remaining,
                        bankruptcy_count = COALESCE(bankruptcy_state.bankruptcy_count, 0) + 1,
                        updated_at = CURRENT_TIMESTAMP
                    """,
                    (inciter_id, normalized_guild, last_at, new_penalty),
                )

            cursor.execute(
                """
                UPDATE wheel_wars SET
                    status = 'resolved',
                    outcome = 'attackers_win',
                    battle_roll = ?,
                    victory_threshold = ?,
                    wheel_effect_spins_remaining = ?,
                    war_scar_wedge_label = ?,
                    celebration_spin_expires_at = ?,
                    resolved_at = ?,
                    inciter_cooldown_until = ?
                WHERE war_id = ?
                """,
                (
                    battle_roll, victory_threshold, wheel_effect_spins,
                    war_scar_label, celebration_expires_at,
                    resolved_at, inciter_cooldown_until, war_id,
                ),
            )

            return {
                "inciter_penalty_before": inciter_penalty_before,
                "inciter_penalty_after": new_penalty,
            }

    def set_inciter_veteran_weight(
        self,
        war_id: int,
        bankruptcy_count: int,
        effective_attack_count: float,
    ) -> None:
        """Overwrite the inciter's attack-voter row with a veteran weight.

        The inciter is always the first entry in ``attack_voter_ids`` (it's
        created as the sole voter when the war is created). This helper rewrites
        that entry's bankruptcy_count and bumps ``effective_attack_count`` so
        the repository remains the only layer that touches the column.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT attack_voter_ids FROM wheel_wars WHERE war_id = ?",
                (war_id,),
            )
            row = cursor.fetchone()
            if not row:
                raise ValueError(f"War {war_id} not found")
            voters = safe_json_loads(
                row["attack_voter_ids"],
                default=[],
                context=f"wheel_wars.attack_voter_ids war_id={war_id}",
            )
            if not voters:
                return
            voters[0]["bankruptcy_count"] = bankruptcy_count
            cursor.execute(
                "UPDATE wheel_wars SET attack_voter_ids = ?, effective_attack_count = ? WHERE war_id = ?",
                (json.dumps(voters), effective_attack_count, war_id),
            )

    def set_war_outcome(
        self,
        war_id: int,
        outcome: str,
        battle_roll: int,
        victory_threshold: int,
        wheel_effect_spins_remaining: int,
        war_scar_wedge_label: str | None,
        celebration_spin_expires_at: int | None,
        resolved_at: int,
    ) -> None:
        """Record the final outcome of a war."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE wheel_wars SET
                    status = 'resolved',
                    outcome = ?,
                    battle_roll = ?,
                    victory_threshold = ?,
                    wheel_effect_spins_remaining = ?,
                    war_scar_wedge_label = ?,
                    celebration_spin_expires_at = ?,
                    resolved_at = ?
                WHERE war_id = ?
                """,
                (
                    outcome,
                    battle_roll,
                    victory_threshold,
                    wheel_effect_spins_remaining,
                    war_scar_wedge_label,
                    celebration_spin_expires_at,
                    resolved_at,
                    war_id,
                ),
            )

    def set_fizzled(self, war_id: int, resolved_at: int) -> None:
        """Mark a war as fizzled."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE wheel_wars SET status = 'fizzled', outcome = 'fizzled', resolved_at = ?
                WHERE war_id = ?
                """,
                (resolved_at, war_id),
            )

    def set_meta_bet_window(self, war_id: int, meta_bet_closes_at: int) -> None:
        """Set the meta-bet window close time when war is declared."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE wheel_wars SET status = 'betting', meta_bet_closes_at = ? WHERE war_id = ?",
                (meta_bet_closes_at, war_id),
            )

    def place_meta_bet_atomic(
        self,
        war_id: int,
        guild_id: int,
        discord_id: int,
        side: str,
        amount: int,
        created_at: int,
        max_debt: int,
    ) -> int:
        """Atomically place a meta-bet (debit balance + insert). Returns bet_id."""
        normalized = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            # Check balance
            cursor.execute(
                "SELECT jopacoin_balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, normalized),
            )
            row = cursor.fetchone()
            if not row:
                raise ValueError("Player not found")
            balance = row["jopacoin_balance"]
            if balance < amount:
                raise ValueError(f"Insufficient balance: have {balance}, need {amount}")
            # Debit balance
            cursor.execute(
                "UPDATE players SET jopacoin_balance = jopacoin_balance - ? WHERE discord_id = ? AND guild_id = ?",
                (amount, discord_id, normalized),
            )
            # Insert bet
            cursor.execute(
                """
                INSERT INTO war_bets (war_id, guild_id, discord_id, side, amount, created_at)
                VALUES (?, ?, ?, ?, ?, ?)
                """,
                (war_id, normalized, discord_id, side, amount, created_at),
            )
            return cursor.lastrowid

    def get_meta_bets(self, war_id: int) -> list[dict]:
        """Get all meta-bets for a war."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT * FROM war_bets WHERE war_id = ? ORDER BY created_at",
                (war_id,),
            )
            return [dict(row) for row in cursor.fetchall()]

    def settle_meta_bets(self, war_id: int, winning_side: str) -> dict:
        """Settle meta-bets parimutuel. Returns payout summary."""
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT * FROM war_bets WHERE war_id = ?", (war_id,))
            bets = [dict(row) for row in cursor.fetchall()]

            if not bets:
                return {"total_pool": 0, "winning_side": winning_side, "payouts": []}

            total_pool = sum(b["amount"] for b in bets)
            winning_bets = [b for b in bets if b["side"] == winning_side]
            winning_pool = sum(b["amount"] for b in winning_bets)

            payouts = []
            for bet in winning_bets:
                if winning_pool > 0:
                    payout = int(bet["amount"] / winning_pool * total_pool)
                else:
                    payout = bet["amount"]
                cursor.execute(
                    "UPDATE war_bets SET payout = ? WHERE bet_id = ?",
                    (payout, bet["bet_id"]),
                )
                cursor.execute(
                    """
                    UPDATE players SET jopacoin_balance = jopacoin_balance + ?
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (payout, bet["discord_id"], bet["guild_id"]),
                )
                payouts.append({"discord_id": bet["discord_id"], "payout": payout})

            # Mark losing bets with payout=0
            for bet in bets:
                if bet["side"] != winning_side:
                    cursor.execute(
                        "UPDATE war_bets SET payout = 0 WHERE bet_id = ?",
                        (bet["bet_id"],),
                    )

            return {
                "total_pool": total_pool,
                "winning_side": winning_side,
                "winning_pool": winning_pool,
                "payouts": payouts,
            }

    def consume_war_spin(self, war_id: int, discord_id: int) -> int:
        """Decrement wheel_effect_spins_remaining. Returns new count."""
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE wheel_wars
                SET wheel_effect_spins_remaining = MAX(0, wheel_effect_spins_remaining - 1)
                WHERE war_id = ?
                """,
                (war_id,),
            )
            cursor.execute(
                "SELECT wheel_effect_spins_remaining FROM wheel_wars WHERE war_id = ?",
                (war_id,),
            )
            row = cursor.fetchone()
            return row["wheel_effect_spins_remaining"] if row else 0

    def use_celebration_spin(self, war_id: int, discord_id: int) -> bool:
        """Mark a player as having used their free celebration spin. Returns True if newly used."""
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT celebration_spins_used FROM wheel_wars WHERE war_id = ?",
                (war_id,),
            )
            row = cursor.fetchone()
            if not row:
                return False
            used = safe_json_loads(
                row["celebration_spins_used"],
                default=[],
                context=f"wheel_wars.celebration_spins_used war_id={war_id}",
            )
            if discord_id in used:
                return False
            used.append(discord_id)
            cursor.execute(
                "UPDATE wheel_wars SET celebration_spins_used = ? WHERE war_id = ?",
                (json.dumps(used), war_id),
            )
            return True

    def get_recent_wars(self, guild_id: int, limit: int = 5) -> list[dict]:
        """Get the most recent wars for a guild."""
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM wheel_wars
                WHERE guild_id = ?
                ORDER BY war_id DESC LIMIT ?
                """,
                (normalized, limit),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_player_war_stats(self, discord_id: int, guild_id: int) -> dict:
        """Get rebellion stats for a player."""
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            # Wars incited
            cursor.execute(
                """
                SELECT outcome, COUNT(*) as cnt
                FROM wheel_wars
                WHERE guild_id = ? AND inciter_id = ?
                GROUP BY outcome
                """,
                (normalized, discord_id),
            )
            incited_rows = cursor.fetchall()
            incited = {row["outcome"]: row["cnt"] for row in incited_rows}

            # Wars participated (attack votes) - search JSON
            cursor.execute(
                """
                SELECT outcome, COUNT(*) as cnt
                FROM wheel_wars
                WHERE guild_id = ?
                  AND attack_voter_ids LIKE ?
                  AND inciter_id != ?
                GROUP BY outcome
                """,
                (normalized, f"%\"discord_id\": {discord_id}%", discord_id),
            )
            attack_rows = cursor.fetchall()
            attacked = {row["outcome"]: row["cnt"] for row in attack_rows}

            # Wars defended
            cursor.execute(
                """
                SELECT outcome, COUNT(*) as cnt
                FROM wheel_wars
                WHERE guild_id = ?
                  AND defend_voter_ids LIKE ?
                GROUP BY outcome
                """,
                (normalized, f"%{discord_id}%"),
            )
            defend_rows = cursor.fetchall()
            defended = {row["outcome"]: row["cnt"] for row in defend_rows}

            return {
                "incited": incited,
                "attacked": attacked,
                "defended": defended,
            }

    def get_inciter_cooldown(self, discord_id: int, guild_id: int) -> int | None:
        """Get the unix timestamp when the inciter's cooldown expires (or None)."""
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT inciter_cooldown_until FROM wheel_wars
                WHERE guild_id = ? AND inciter_id = ?
                  AND inciter_cooldown_until IS NOT NULL
                ORDER BY war_id DESC LIMIT 1
                """,
                (normalized, discord_id),
            )
            row = cursor.fetchone()
            return row["inciter_cooldown_until"] if row else None

    def set_inciter_cooldown(self, war_id: int, discord_id: int, guild_id: int, cooldown_until: int) -> None:
        """Set inciter cooldown expiry timestamp."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE wheel_wars SET inciter_cooldown_until = ? WHERE war_id = ?",
                (cooldown_until, war_id),
            )

    def get_active_war_effect(self, guild_id: int) -> dict | None:
        """Get the most recent resolved war with active wheel effects."""
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM wheel_wars
                WHERE guild_id = ?
                  AND status = 'resolved'
                  AND wheel_effect_spins_remaining > 0
                ORDER BY war_id DESC LIMIT 1
                """,
                (normalized,),
            )
            row = cursor.fetchone()
            return dict(row) if row else None

    def get_war_leaderboard(self, guild_id: int) -> list[dict]:
        """Get rebellion leaderboard stats for all players in a guild."""
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            # Inciter wins
            cursor.execute(
                """
                SELECT inciter_id as discord_id,
                       COUNT(*) as wars_incited,
                       SUM(CASE WHEN outcome = 'attackers_win' THEN 1 ELSE 0 END) as wars_won_as_inciter
                FROM wheel_wars
                WHERE guild_id = ?
                GROUP BY inciter_id
                """,
                (normalized,),
            )
            inciter_rows = {row["discord_id"]: dict(row) for row in cursor.fetchall()}

            # Meta-bet JC won
            cursor.execute(
                """
                SELECT wb.discord_id,
                       SUM(COALESCE(wb.payout, 0) - wb.amount) as net_meta_bet_jc
                FROM war_bets wb
                WHERE wb.guild_id = ?
                GROUP BY wb.discord_id
                """,
                (normalized,),
            )
            meta_bet_rows = {row["discord_id"]: dict(row) for row in cursor.fetchall()}

            # Combine
            all_ids = set(inciter_rows.keys()) | set(meta_bet_rows.keys())
            result = []
            for did in all_ids:
                ir = inciter_rows.get(did, {})
                mbr = meta_bet_rows.get(did, {})
                result.append({
                    "discord_id": did,
                    "wars_incited": ir.get("wars_incited", 0),
                    "wars_won_as_inciter": ir.get("wars_won_as_inciter", 0),
                    "net_meta_bet_jc": mbr.get("net_meta_bet_jc", 0),
                })
            result.sort(key=lambda x: (-x["wars_won_as_inciter"], -x["net_meta_bet_jc"]))
            return result
