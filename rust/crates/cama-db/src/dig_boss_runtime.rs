//! Guarded existing-schema persistence for live Dig boss encounters.
//!
//! Boss policy belongs to `cama-app`. This repository owns only the Python
//! schema boundaries: an actor's tunnel, balance, boss echo, audit row, and
//! ledger context settle together; paused duels are claimed exactly once;
//! Lantern consumption and post-resolution gear wear remain separate atomic
//! operations, matching the production partial-failure ordering.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigBossRuntimeKey {
    pub discord_id: i64,
    pub guild_id: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DigBossRuntimeSnapshot {
    pub key: DigBossRuntimeKey,
    pub depth: i64,
    pub max_depth: i64,
    pub prestige_level: i64,
    pub balance: i64,
    pub boss_progress_json: Option<String>,
    pub stinger_curse_json: Option<String>,
    pub boss_attempts: i64,
    pub last_dig_at: Option<i64>,
    pub cheer_data_json: Option<String>,
    pub route_state_json: Option<String>,
    pub luminosity: i64,
    pub last_lum_update_at: Option<i64>,
    pub prestige_perks_json: Option<String>,
    pub mutations_json: Option<String>,
    pub stat_strength: i64,
    pub stat_smarts: i64,
    pub stat_stamina: i64,
    pub stat_points: i64,
    pub stat_boss_awards_json: Option<String>,
    pub pinnacle_boss_id: Option<String>,
    pub pinnacle_phase: i64,
    pub pinnacle_hp_remaining: Option<i64>,
    pub pinnacle_last_engaged_at: Option<i64>,
    pub retreat_cooldown_until: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DigBossRuntimeState {
    pub depth: i64,
    pub max_depth: i64,
    pub balance: i64,
    pub boss_progress_json: Option<String>,
    pub stinger_curse_json: Option<String>,
    pub boss_attempts: i64,
    pub last_dig_at: Option<i64>,
    pub cheer_data_json: Option<String>,
    pub route_state_json: Option<String>,
    pub luminosity: i64,
    pub last_lum_update_at: Option<i64>,
    pub stat_points: i64,
    pub stat_boss_awards_json: Option<String>,
    pub pinnacle_boss_id: Option<String>,
    pub pinnacle_phase: i64,
    pub pinnacle_hp_remaining: Option<i64>,
    pub pinnacle_last_engaged_at: Option<i64>,
    pub retreat_cooldown_until: Option<i64>,
}

impl From<&DigBossRuntimeSnapshot> for DigBossRuntimeState {
    fn from(snapshot: &DigBossRuntimeSnapshot) -> Self {
        Self {
            depth: snapshot.depth,
            max_depth: snapshot.max_depth,
            balance: snapshot.balance,
            boss_progress_json: snapshot.boss_progress_json.clone(),
            stinger_curse_json: snapshot.stinger_curse_json.clone(),
            boss_attempts: snapshot.boss_attempts,
            last_dig_at: snapshot.last_dig_at,
            cheer_data_json: snapshot.cheer_data_json.clone(),
            route_state_json: snapshot.route_state_json.clone(),
            luminosity: snapshot.luminosity,
            last_lum_update_at: snapshot.last_lum_update_at,
            stat_points: snapshot.stat_points,
            stat_boss_awards_json: snapshot.stat_boss_awards_json.clone(),
            pinnacle_boss_id: snapshot.pinnacle_boss_id.clone(),
            pinnacle_phase: snapshot.pinnacle_phase,
            pinnacle_hp_remaining: snapshot.pinnacle_hp_remaining,
            pinnacle_last_engaged_at: snapshot.pinnacle_last_engaged_at,
            retreat_cooldown_until: snapshot.retreat_cooldown_until,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DigBossRuntimeAudit<'a> {
    pub action_type: &'a str,
    pub target_id: Option<i64>,
    pub detail_json: &'a str,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct DigBossRuntimeEcho<'a> {
    pub boss_id: &'a str,
    pub depth: i64,
    pub killer_id: i64,
    pub weakened_until: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct DigBossRuntimeArtifactGrant<'a> {
    pub artifact_id: &'a str,
    pub found_at: i64,
    pub is_relic: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct DigBossRuntimeCommit<'a> {
    pub expected: &'a DigBossRuntimeSnapshot,
    pub proposed: &'a DigBossRuntimeState,
    pub audit: Option<DigBossRuntimeAudit<'a>>,
    pub echo: Option<DigBossRuntimeEcho<'a>>,
    /// Optional reward fused into the same guarded boss settlement. Pinnacle
    /// victory uses this so a crash cannot mint a relic without advancing the
    /// phase and paying the actor, or vice versa.
    pub artifact: Option<DigBossRuntimeArtifactGrant<'a>>,
    /// Profit-only vanity deduction. The repository credits gross Dig JC and
    /// records this as a separate `vanity_tax` debit in the same transaction.
    pub vanity_tax: i64,
    /// Profit-only low-priority deduction taken from the same gross Dig JC.
    /// It is accounted separately from the vanity deduction so each sink keeps
    /// its own `low_priority_tax` ledger row in the same transaction.
    pub low_priority_tax: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigBossRuntimeReceipt {
    pub action_id: Option<i64>,
    pub artifact_database_id: Option<i64>,
    pub balance_after: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct DigBossRegularGearGrant<'a> {
    pub key: DigBossRuntimeKey,
    pub slot: &'a str,
    pub tier: i64,
    pub durability: i64,
    pub acquired_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigBossGearPresentationRow {
    pub id: i64,
    pub slot: String,
    pub tier: i64,
    pub durability: i64,
    pub item_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigBossRuntimeCommitOutcome {
    Applied(DigBossRuntimeReceipt),
    Conflict,
    MissingPlayer,
    MissingTunnel,
}

#[derive(Clone, Copy, Debug)]
pub struct DigBossPreparationActivation<'a> {
    pub expected: &'a DigBossRuntimeSnapshot,
    pub inventory_item_id: i64,
    pub item_type: &'a str,
    pub proposed_boss_progress_json: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigBossPreparationActivationOutcome {
    Applied,
    Conflict,
    ItemUnavailable,
    MissingPlayer,
    MissingTunnel,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DigBossPausedDuelRow {
    pub key: DigBossRuntimeKey,
    pub boss_id: String,
    pub boundary: i64,
    pub mechanic_id: String,
    pub risk_tier: String,
    pub wager: i64,
    pub player_hp: i64,
    pub boss_hp: i64,
    pub round_num: i64,
    pub round_log_json: String,
    pub pending_prompt_json: Option<String>,
    pub rng_state: String,
    pub status_effects_json: String,
    pub echo_applied: bool,
    pub echo_killer_id: Option<i64>,
    pub player_hit: f64,
    pub player_damage: i64,
    pub boss_hit: f64,
    pub boss_damage: i64,
    pub critical_chance: f64,
    pub critical_bonus: i64,
    pub created_at: i64,
    pub last_interaction_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigBossGearSnapshot {
    pub gear_ids: Vec<i64>,
    pub armor_id: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigBossGearWearReceipt {
    pub broken_ids: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigBossCheerSnapshot {
    pub cheerer_id: i64,
    pub target_id: i64,
    pub guild_id: i64,
    pub target_depth: i64,
    pub target_boss_progress_json: Option<String>,
    pub target_cheer_data_json: Option<String>,
    pub cheerer_tunnel_exists: bool,
    pub cheerer_last_cheer_at: Option<i64>,
}

#[derive(Clone, Copy, Debug)]
pub struct DigBossCheerCommit<'a> {
    pub expected: &'a DigBossCheerSnapshot,
    pub proposed_target_cheer_data_json: &'a str,
    pub cheerer_last_cheer_at: i64,
    pub create_cheerer_tunnel_name: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigBossCheerCommitOutcome {
    Applied,
    Conflict,
    MissingCheerer,
    MissingTarget,
}

#[derive(Debug, Error)]
pub enum DigBossRuntimeRepositoryError {
    #[error("Dig boss SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Dig boss proposed state is invalid: {0}")]
    InvalidState(&'static str),
}

#[derive(Clone, Debug)]
pub struct DigBossRuntimeRepository {
    database_path: PathBuf,
}

impl DigBossRuntimeRepository {
    #[must_use]
    pub fn new(database_path: impl AsRef<Path>) -> Self {
        Self {
            database_path: database_path.as_ref().to_path_buf(),
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.database_path)
    }

    pub fn snapshot(
        &self,
        key: DigBossRuntimeKey,
    ) -> Result<Option<DigBossRuntimeSnapshot>, DigBossRuntimeRepositoryError> {
        query_snapshot(&self.connection()?, key)
    }

    pub fn commit(
        &self,
        request: DigBossRuntimeCommit<'_>,
    ) -> Result<DigBossRuntimeCommitOutcome, DigBossRuntimeRepositoryError> {
        if request
            .artifact
            .is_some_and(|artifact| artifact.artifact_id.trim().is_empty())
        {
            return Err(DigBossRuntimeRepositoryError::InvalidState(
                "boss artifact id cannot be empty",
            ));
        }
        validate_state(request.expected, request.proposed)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = query_snapshot(&transaction, request.expected.key)?;
        let Some(current) = current else {
            let player = player_exists(&transaction, request.expected.key)?;
            let tunnel = tunnel_exists(&transaction, request.expected.key)?;
            transaction.rollback()?;
            return Ok(if !player {
                DigBossRuntimeCommitOutcome::MissingPlayer
            } else if !tunnel {
                DigBossRuntimeCommitOutcome::MissingTunnel
            } else {
                DigBossRuntimeCommitOutcome::Conflict
            });
        };
        if current != *request.expected {
            transaction.rollback()?;
            return Ok(DigBossRuntimeCommitOutcome::Conflict);
        }

        apply_tunnel(&transaction, request.expected, request.proposed)?;
        apply_balance(&transaction, request)?;
        let action_id = request
            .audit
            .map(|audit| insert_audit(&transaction, request, audit))
            .transpose()?;
        let artifact_database_id = request
            .artifact
            .map(|artifact| insert_artifact(&transaction, request.expected.key, artifact))
            .transpose()?;
        if let Some(echo) = request.echo {
            upsert_echo(&transaction, request.expected.key.guild_id, echo)?;
        }
        transaction.commit()?;
        Ok(DigBossRuntimeCommitOutcome::Applied(
            DigBossRuntimeReceipt {
                action_id,
                artifact_database_id,
                balance_after: request.proposed.balance,
            },
        ))
    }

    /// Persist Python's post-victory regular gear drop as its own operation.
    /// Boss settlement and gear wear have already committed at this point.
    pub fn grant_regular_gear_drop(
        &self,
        grant: DigBossRegularGearGrant<'_>,
    ) -> Result<i64, DigBossRuntimeRepositoryError> {
        if !matches!(grant.slot, "weapon" | "armor" | "boots" | "amulet")
            || !(0..=7).contains(&grant.tier)
            || grant.durability <= 0
        {
            return Err(DigBossRuntimeRepositoryError::InvalidState(
                "regular boss gear drop is invalid",
            ));
        }
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO dig_gear
             (discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source)
             VALUES (?1,?2,?3,?4,?5,0,?6,'boss_drop')",
            params![
                grant.key.discord_id,
                grant.key.guild_id,
                grant.slot,
                grant.tier,
                grant.durability,
                grant.acquired_at,
            ],
        )?;
        Ok(connection.last_insert_rowid())
    }

    pub fn owned_artifact_ids(
        &self,
        key: DigBossRuntimeKey,
    ) -> Result<BTreeSet<String>, DigBossRuntimeRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT artifact_id FROM dig_artifacts
              WHERE discord_id=?1 AND guild_id=?2",
        )?;
        statement
            .query_map(params![key.discord_id, key.guild_id], |row| row.get(0))?
            .collect::<Result<BTreeSet<_>, _>>()
            .map_err(Into::into)
    }

    /// Insert the selected prestige relic only when it remains unowned. The
    /// eligibility roll happens in the application layer before this small
    /// post-settlement transaction, matching Python's ordering.
    pub fn grant_regular_relic_drop_if_unowned(
        &self,
        key: DigBossRuntimeKey,
        artifact_id: &str,
        found_at: i64,
    ) -> Result<Option<i64>, DigBossRuntimeRepositoryError> {
        if artifact_id.trim().is_empty() {
            return Err(DigBossRuntimeRepositoryError::InvalidState(
                "regular boss relic id cannot be empty",
            ));
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let owned = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM dig_artifacts
              WHERE discord_id=?1 AND guild_id=?2 AND artifact_id=?3)",
            params![key.discord_id, key.guild_id, artifact_id],
            |row| row.get::<_, bool>(0),
        )?;
        if owned {
            transaction.commit()?;
            return Ok(None);
        }
        transaction.execute(
            "INSERT INTO dig_artifacts
             (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,?3,?4,1,0)",
            params![key.discord_id, key.guild_id, artifact_id, found_at],
        )?;
        let row_id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(Some(row_id))
    }

    pub fn gear_presentation_rows(
        &self,
        key: DigBossRuntimeKey,
        gear_ids: &[i64],
    ) -> Result<Vec<DigBossGearPresentationRow>, DigBossRuntimeRepositoryError> {
        if gear_ids.is_empty() {
            return Ok(Vec::new());
        }
        let connection = self.connection()?;
        let mut rows = Vec::with_capacity(gear_ids.len());
        for gear_id in gear_ids {
            let row = connection
                .query_row(
                    "SELECT id,slot,tier,durability,item_id FROM dig_gear
                      WHERE id=?1 AND discord_id=?2 AND guild_id=?3",
                    params![gear_id, key.discord_id, key.guild_id],
                    |row| {
                        Ok(DigBossGearPresentationRow {
                            id: row.get(0)?,
                            slot: row.get(1)?,
                            tier: row.get(2)?,
                            durability: row.get(3)?,
                            item_id: row.get(4)?,
                        })
                    },
                )
                .optional()?;
            if let Some(row) = row {
                rows.push(row);
            }
        }
        Ok(rows)
    }

    /// Atomically promote one exact queued preparation item into boss progress.
    /// The application layer selects the first queued authored item and builds
    /// the JSON; this guarded repository operation owns only the live-row CAS,
    /// inventory deletion, and tunnel update transaction.
    pub fn activate_preparation(
        &self,
        request: DigBossPreparationActivation<'_>,
    ) -> Result<DigBossPreparationActivationOutcome, DigBossRuntimeRepositoryError> {
        if !matches!(
            request.item_type,
            "tempered_whetstone" | "warding_salts" | "rescue_line"
        ) {
            return Err(DigBossRuntimeRepositoryError::InvalidState(
                "unknown boss preparation item",
            ));
        }
        if !matches!(
            serde_json::from_str::<serde_json::Value>(request.proposed_boss_progress_json),
            Ok(serde_json::Value::Object(_))
        ) {
            return Err(DigBossRuntimeRepositoryError::InvalidState(
                "boss preparation progress must be a JSON object",
            ));
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = query_snapshot(&transaction, request.expected.key)?;
        let Some(current) = current else {
            let player = player_exists(&transaction, request.expected.key)?;
            let tunnel = tunnel_exists(&transaction, request.expected.key)?;
            transaction.rollback()?;
            return Ok(if !player {
                DigBossPreparationActivationOutcome::MissingPlayer
            } else if !tunnel {
                DigBossPreparationActivationOutcome::MissingTunnel
            } else {
                DigBossPreparationActivationOutcome::Conflict
            });
        };
        if current != *request.expected {
            transaction.rollback()?;
            return Ok(DigBossPreparationActivationOutcome::Conflict);
        }
        let deleted = transaction.execute(
            "DELETE FROM dig_inventory
              WHERE id=?1 AND discord_id=?2 AND guild_id=?3
                AND item_type=?4 AND queued=1",
            params![
                request.inventory_item_id,
                request.expected.key.discord_id,
                request.expected.key.guild_id,
                request.item_type,
            ],
        )?;
        if deleted != 1 {
            transaction.rollback()?;
            return Ok(DigBossPreparationActivationOutcome::ItemUnavailable);
        }
        transaction.execute(
            "UPDATE tunnels SET boss_progress=?1
              WHERE discord_id=?2 AND guild_id=?3",
            params![
                request.proposed_boss_progress_json,
                request.expected.key.discord_id,
                request.expected.key.guild_id,
            ],
        )?;
        transaction.commit()?;
        Ok(DigBossPreparationActivationOutcome::Applied)
    }

    pub fn active_echo(
        &self,
        guild_id: i64,
        boss_id: &str,
        now: i64,
    ) -> Result<Option<(i64, i64, i64)>, DigBossRuntimeRepositoryError> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT depth,killer_discord_id,weakened_until
                   FROM dig_boss_echoes
                  WHERE guild_id=?1 AND boss_id=?2 AND weakened_until>?3",
                params![guild_id, boss_id, now],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?)
    }

    pub fn save_active_duel(
        &self,
        duel: &DigBossPausedDuelRow,
    ) -> Result<(), DigBossRuntimeRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !tunnel_exists(&transaction, duel.key)? {
            transaction.rollback()?;
            return Err(DigBossRuntimeRepositoryError::InvalidState(
                "paused duel owner has no tunnel",
            ));
        }
        transaction.execute(
            "INSERT INTO dig_active_duels
             (discord_id,guild_id,boss_id,tier,mechanic_id,risk_tier,wager,
              player_hp,boss_hp,round_num,round_log,pending_prompt,rng_state,
              status_effects,echo_applied,echo_killer_id,player_hit,player_dmg,
              boss_hit,boss_dmg,crit_chance,crit_bonus,created_at,last_interaction_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,
                     ?15,?16,?17,?18,?19,?20,?21,?22,?23,?24)
             ON CONFLICT(discord_id,guild_id) DO UPDATE SET
              boss_id=excluded.boss_id,tier=excluded.tier,
              mechanic_id=excluded.mechanic_id,risk_tier=excluded.risk_tier,
              wager=excluded.wager,player_hp=excluded.player_hp,
              boss_hp=excluded.boss_hp,round_num=excluded.round_num,
              round_log=excluded.round_log,pending_prompt=excluded.pending_prompt,
              rng_state=excluded.rng_state,status_effects=excluded.status_effects,
              echo_applied=excluded.echo_applied,echo_killer_id=excluded.echo_killer_id,
              player_hit=excluded.player_hit,player_dmg=excluded.player_dmg,
              boss_hit=excluded.boss_hit,boss_dmg=excluded.boss_dmg,
              crit_chance=excluded.crit_chance,crit_bonus=excluded.crit_bonus,
              last_interaction_at=excluded.last_interaction_at",
            params![
                duel.key.discord_id,
                duel.key.guild_id,
                duel.boss_id,
                duel.boundary,
                duel.mechanic_id,
                duel.risk_tier,
                duel.wager,
                duel.player_hp,
                duel.boss_hp,
                duel.round_num,
                duel.round_log_json,
                duel.pending_prompt_json,
                duel.rng_state,
                duel.status_effects_json,
                i64::from(duel.echo_applied),
                duel.echo_killer_id,
                duel.player_hit,
                duel.player_damage,
                duel.boss_hit,
                duel.boss_damage,
                duel.critical_chance,
                duel.critical_bonus,
                duel.created_at,
                duel.last_interaction_at,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn active_duel(
        &self,
        key: DigBossRuntimeKey,
    ) -> Result<Option<DigBossPausedDuelRow>, DigBossRuntimeRepositoryError> {
        query_active_duel(&self.connection()?, key)
    }

    pub fn claim_active_duel(
        &self,
        key: DigBossRuntimeKey,
    ) -> Result<Option<DigBossPausedDuelRow>, DigBossRuntimeRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let duel = query_active_duel(&transaction, key)?;
        if duel.is_some() {
            transaction.execute(
                "DELETE FROM dig_active_duels WHERE discord_id=?1 AND guild_id=?2",
                params![key.discord_id, key.guild_id],
            )?;
        }
        transaction.commit()?;
        Ok(duel)
    }

    pub fn gear_snapshot(
        &self,
        key: DigBossRuntimeKey,
    ) -> Result<DigBossGearSnapshot, DigBossRuntimeRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id,slot FROM dig_gear
              WHERE discord_id=?1 AND guild_id=?2 AND equipped=1
              ORDER BY CASE slot WHEN 'weapon' THEN 0 WHEN 'armor' THEN 1
                                 WHEN 'boots' THEN 2 WHEN 'amulet' THEN 3
                                 WHEN 'ring' THEN 4 ELSE 5 END,id",
        )?;
        let rows = statement
            .query_map(params![key.discord_id, key.guild_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(DigBossGearSnapshot {
            gear_ids: rows.iter().map(|(id, _)| *id).collect(),
            armor_id: rows
                .iter()
                .find_map(|(id, slot)| (slot == "armor").then_some(*id)),
        })
    }

    /// Tick the pieces that actually fought, even if the player changed
    /// loadout while a mechanic prompt was open. A loss ticks snapshot armor
    /// one additional time, exactly like Python.
    pub fn tick_gear_after_resolution(
        &self,
        key: DigBossRuntimeKey,
        snapshot: &DigBossGearSnapshot,
        won: bool,
    ) -> Result<DigBossGearWearReceipt, DigBossRuntimeRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut broken_ids = Vec::new();
        for gear_id in &snapshot.gear_ids {
            tick_one_gear(&transaction, key, *gear_id, 1, &mut broken_ids)?;
        }
        if !won && let Some(armor_id) = snapshot.armor_id {
            tick_one_gear(&transaction, key, armor_id, 1, &mut broken_ids)?;
        }
        transaction.commit()?;
        broken_ids.sort_unstable();
        broken_ids.dedup();
        Ok(DigBossGearWearReceipt { broken_ids })
    }

    pub fn lantern_state(
        &self,
        key: DigBossRuntimeKey,
    ) -> Result<(Vec<i64>, bool), DigBossRuntimeRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id FROM dig_inventory
              WHERE discord_id=?1 AND guild_id=?2 AND item_type='lantern'
              ORDER BY id",
        )?;
        let lanterns = statement
            .query_map(params![key.discord_id, key.guild_id], |row| row.get(0))?
            .collect::<Result<Vec<i64>, _>>()?;
        let great = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM dig_inventory
              WHERE discord_id=?1 AND guild_id=?2 AND item_type='great_lantern')",
            params![key.discord_id, key.guild_id],
            |row| row.get::<_, bool>(0),
        )?;
        Ok((lanterns, great))
    }

    pub fn consume_lantern(
        &self,
        key: DigBossRuntimeKey,
        row_id: i64,
    ) -> Result<bool, DigBossRuntimeRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "DELETE FROM dig_inventory
              WHERE id=?1 AND discord_id=?2 AND guild_id=?3 AND item_type='lantern'",
            params![row_id, key.discord_id, key.guild_id],
        )?;
        transaction.commit()?;
        Ok(changed == 1)
    }

    pub fn cheer_snapshot(
        &self,
        cheerer_id: i64,
        target_id: i64,
        guild_id: i64,
    ) -> Result<Option<DigBossCheerSnapshot>, DigBossRuntimeRepositoryError> {
        let connection = self.connection()?;
        let cheerer_player = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM players WHERE discord_id=?1 AND guild_id=?2)",
            params![cheerer_id, guild_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !cheerer_player {
            return Ok(None);
        }
        let target = connection
            .query_row(
                "SELECT depth,boss_progress,cheer_data FROM tunnels
                  WHERE discord_id=?1 AND guild_id=?2",
                params![target_id, guild_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((target_depth, target_boss_progress_json, target_cheer_data_json)) = target else {
            return Ok(None);
        };
        let cheerer = connection
            .query_row(
                "SELECT last_cheer_at FROM tunnels
                  WHERE discord_id=?1 AND guild_id=?2",
                params![cheerer_id, guild_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?;
        Ok(Some(DigBossCheerSnapshot {
            cheerer_id,
            target_id,
            guild_id,
            target_depth,
            target_boss_progress_json,
            target_cheer_data_json,
            cheerer_tunnel_exists: cheerer.is_some(),
            cheerer_last_cheer_at: cheerer.flatten(),
        }))
    }

    pub fn commit_cheer(
        &self,
        request: DigBossCheerCommit<'_>,
    ) -> Result<DigBossCheerCommitOutcome, DigBossRuntimeRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let cheerer_exists = player_exists(
            &transaction,
            DigBossRuntimeKey {
                discord_id: request.expected.cheerer_id,
                guild_id: request.expected.guild_id,
            },
        )?;
        if !cheerer_exists {
            transaction.rollback()?;
            return Ok(DigBossCheerCommitOutcome::MissingCheerer);
        }
        let current_target = transaction
            .query_row(
                "SELECT depth,boss_progress,cheer_data FROM tunnels
                  WHERE discord_id=?1 AND guild_id=?2",
                params![request.expected.target_id, request.expected.guild_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some(current_target) = current_target else {
            transaction.rollback()?;
            return Ok(DigBossCheerCommitOutcome::MissingTarget);
        };
        let current_cheerer = transaction
            .query_row(
                "SELECT last_cheer_at FROM tunnels
                  WHERE discord_id=?1 AND guild_id=?2",
                params![request.expected.cheerer_id, request.expected.guild_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?;
        if current_target
            != (
                request.expected.target_depth,
                request.expected.target_boss_progress_json.clone(),
                request.expected.target_cheer_data_json.clone(),
            )
            || current_cheerer.is_some() != request.expected.cheerer_tunnel_exists
            || current_cheerer.flatten() != request.expected.cheerer_last_cheer_at
        {
            transaction.rollback()?;
            return Ok(DigBossCheerCommitOutcome::Conflict);
        }
        if !request.expected.cheerer_tunnel_exists {
            transaction.execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,depth,max_depth,total_digs,total_jc_earned,
                  streak_days,pickaxe_tier,prestige_level,tunnel_name,boss_progress,
                  boss_attempts,trap_active,trap_free_today,paid_digs_today,created_at)
                 VALUES (?1,?2,0,0,0,0,0,0,0,?3,NULL,NULL,0,1,0,?4)",
                params![
                    request.expected.cheerer_id,
                    request.expected.guild_id,
                    request.create_cheerer_tunnel_name,
                    request.cheerer_last_cheer_at,
                ],
            )?;
        }
        transaction.execute(
            "UPDATE tunnels SET last_cheer_at=?1
              WHERE discord_id=?2 AND guild_id=?3",
            params![
                request.cheerer_last_cheer_at,
                request.expected.cheerer_id,
                request.expected.guild_id,
            ],
        )?;
        transaction.execute(
            "UPDATE tunnels SET cheer_data=?1
              WHERE discord_id=?2 AND guild_id=?3",
            params![
                request.proposed_target_cheer_data_json,
                request.expected.target_id,
                request.expected.guild_id,
            ],
        )?;
        transaction.commit()?;
        Ok(DigBossCheerCommitOutcome::Applied)
    }
}

fn query_snapshot(
    connection: &Connection,
    key: DigBossRuntimeKey,
) -> Result<Option<DigBossRuntimeSnapshot>, DigBossRuntimeRepositoryError> {
    let balance = connection
        .query_row(
            "SELECT COALESCE(jopacoin_balance,0) FROM players
              WHERE discord_id=?1 AND guild_id=?2",
            params![key.discord_id, key.guild_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(balance) = balance else {
        return Ok(None);
    };
    Ok(connection
        .query_row(
            "SELECT depth,max_depth,prestige_level,boss_progress,stinger_curse,
                    CAST(COALESCE(boss_attempts,0) AS INTEGER),last_dig_at,
                    cheer_data,route_state,COALESCE(luminosity,100),last_lum_update_at,prestige_perks,
                    mutations,COALESCE(stat_strength,0),COALESCE(stat_smarts,0),
                    COALESCE(stat_stamina,0),COALESCE(stat_points,0),stat_boss_awards,
                    pinnacle_boss_id,COALESCE(pinnacle_phase,0),pinnacle_hp_remaining,
                    pinnacle_last_engaged_at,retreat_cooldown_until
               FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![key.discord_id, key.guild_id],
            |row| {
                Ok(DigBossRuntimeSnapshot {
                    key,
                    depth: row.get(0)?,
                    max_depth: row.get(1)?,
                    prestige_level: row.get(2)?,
                    balance,
                    boss_progress_json: row.get(3)?,
                    stinger_curse_json: row.get(4)?,
                    boss_attempts: row.get(5)?,
                    last_dig_at: row.get(6)?,
                    cheer_data_json: row.get(7)?,
                    route_state_json: row.get(8)?,
                    luminosity: row.get(9)?,
                    last_lum_update_at: row.get(10)?,
                    prestige_perks_json: row.get(11)?,
                    mutations_json: row.get(12)?,
                    stat_strength: row.get(13)?,
                    stat_smarts: row.get(14)?,
                    stat_stamina: row.get(15)?,
                    stat_points: row.get(16)?,
                    stat_boss_awards_json: row.get(17)?,
                    pinnacle_boss_id: row.get(18)?,
                    pinnacle_phase: row.get(19)?,
                    pinnacle_hp_remaining: row.get(20)?,
                    pinnacle_last_engaged_at: row.get(21)?,
                    retreat_cooldown_until: row.get(22)?,
                })
            },
        )
        .optional()?)
}

fn validate_state(
    expected: &DigBossRuntimeSnapshot,
    proposed: &DigBossRuntimeState,
) -> Result<(), DigBossRuntimeRepositoryError> {
    if proposed.depth < 0
        || proposed.max_depth < proposed.depth
        || (proposed.balance < 0 && proposed.balance != expected.balance)
        || proposed.boss_attempts < 0
        || !(0..=100).contains(&proposed.luminosity)
        || proposed.stat_points < 0
        || !(0..=3).contains(&proposed.pinnacle_phase)
    {
        return Err(DigBossRuntimeRepositoryError::InvalidState(
            "negative or out-of-range boss progression value",
        ));
    }
    if proposed.max_depth < expected.max_depth {
        return Err(DigBossRuntimeRepositoryError::InvalidState(
            "boss settlement cannot reduce historical max depth",
        ));
    }
    for raw in [
        proposed.boss_progress_json.as_deref(),
        proposed.stinger_curse_json.as_deref(),
        proposed.cheer_data_json.as_deref(),
        proposed.route_state_json.as_deref(),
        proposed.stat_boss_awards_json.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        serde_json::from_str::<serde_json::Value>(raw).map_err(|_| {
            DigBossRuntimeRepositoryError::InvalidState("proposed JSON is malformed")
        })?;
    }
    Ok(())
}

fn apply_tunnel(
    transaction: &Transaction<'_>,
    expected: &DigBossRuntimeSnapshot,
    proposed: &DigBossRuntimeState,
) -> Result<(), DigBossRuntimeRepositoryError> {
    let changed = transaction.execute(
        "UPDATE tunnels SET depth=?1,max_depth=?2,boss_progress=?3,
                stinger_curse=?4,boss_attempts=?5,last_dig_at=?6,
                cheer_data=?7,route_state=?8,luminosity=?9,last_lum_update_at=?10,
                stat_points=?11,stat_boss_awards=?12,pinnacle_boss_id=?13,
                pinnacle_phase=?14,pinnacle_hp_remaining=?15,
                pinnacle_last_engaged_at=?16,retreat_cooldown_until=?17
          WHERE discord_id=?18 AND guild_id=?19",
        params![
            proposed.depth,
            proposed.max_depth,
            proposed.boss_progress_json,
            proposed.stinger_curse_json,
            proposed.boss_attempts,
            proposed.last_dig_at,
            proposed.cheer_data_json,
            proposed.route_state_json,
            proposed.luminosity,
            proposed.last_lum_update_at,
            proposed.stat_points,
            proposed.stat_boss_awards_json,
            proposed.pinnacle_boss_id,
            proposed.pinnacle_phase,
            proposed.pinnacle_hp_remaining,
            proposed.pinnacle_last_engaged_at,
            proposed.retreat_cooldown_until,
            expected.key.discord_id,
            expected.key.guild_id,
        ],
    )?;
    if changed != 1 {
        return Err(DigBossRuntimeRepositoryError::InvalidState(
            "tunnel disappeared during boss settlement",
        ));
    }
    Ok(())
}

fn apply_balance(
    transaction: &Transaction<'_>,
    request: DigBossRuntimeCommit<'_>,
) -> Result<(), DigBossRuntimeRepositoryError> {
    let vanity_tax = request.vanity_tax.max(0);
    let low_priority_tax = request.low_priority_tax.max(0);
    let net_delta = request
        .proposed
        .balance
        .saturating_sub(request.expected.balance);
    let gross_delta = net_delta
        .saturating_add(vanity_tax)
        .saturating_add(low_priority_tax);
    if gross_delta == 0 && vanity_tax == 0 && low_priority_tax == 0 {
        return Ok(());
    }
    let audit = request
        .audit
        .ok_or(DigBossRuntimeRepositoryError::InvalidState(
            "balance-changing boss settlement requires an audit",
        ))?;
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context
         (id,source,actor_id,related_type,related_id,reason,metadata)
         VALUES (1,'dig',?1,'boss',?2,'dig boss balance change',?3)",
        params![
            request.expected.key.discord_id,
            audit.action_type,
            audit.detail_json,
        ],
    )?;
    let gross_balance = request.expected.balance.saturating_add(gross_delta);
    let changed = transaction.execute(
        "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
          WHERE discord_id=?2 AND guild_id=?3 AND COALESCE(jopacoin_balance,0)=?4",
        params![
            gross_balance,
            request.expected.key.discord_id,
            request.expected.key.guild_id,
            request.expected.balance,
        ],
    )?;
    if changed != 1 {
        return Err(DigBossRuntimeRepositoryError::InvalidState(
            "player balance changed during boss settlement",
        ));
    }
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    // Each profit sink is withheld from the same gross credit and keeps its
    // own ledger row, so the running balance walks the gross credit down to
    // the proposed net one sink at a time.
    let mut withheld_balance = gross_balance;
    if vanity_tax > 0 {
        transaction.execute(
            "INSERT INTO economy_ledger_context
             (id,source,actor_id,related_type,related_id,reason,metadata)
             VALUES (1,'vanity_tax',?1,'boss',?2,'vanity tax on JC profit',?3)",
            params![
                request.expected.key.discord_id,
                audit.action_type,
                audit.detail_json,
            ],
        )?;
        let next_balance = withheld_balance.saturating_sub(vanity_tax);
        let changed = transaction.execute(
            "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
              WHERE discord_id=?2 AND guild_id=?3 AND COALESCE(jopacoin_balance,0)=?4",
            params![
                next_balance,
                request.expected.key.discord_id,
                request.expected.key.guild_id,
                withheld_balance,
            ],
        )?;
        if changed != 1 {
            return Err(DigBossRuntimeRepositoryError::InvalidState(
                "player balance changed during vanity-tax settlement",
            ));
        }
        transaction.execute("DELETE FROM economy_ledger_context", [])?;
        withheld_balance = next_balance;
    }
    if low_priority_tax > 0 {
        transaction.execute(
            "INSERT INTO economy_ledger_context
             (id,source,actor_id,related_type,related_id,reason,metadata)
             VALUES (1,'low_priority_tax',?1,'boss',?2,'low priority tax on JC profit',?3)",
            params![
                request.expected.key.discord_id,
                audit.action_type,
                audit.detail_json,
            ],
        )?;
        let next_balance = withheld_balance.saturating_sub(low_priority_tax);
        let changed = transaction.execute(
            "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
              WHERE discord_id=?2 AND guild_id=?3 AND COALESCE(jopacoin_balance,0)=?4",
            params![
                next_balance,
                request.expected.key.discord_id,
                request.expected.key.guild_id,
                withheld_balance,
            ],
        )?;
        if changed != 1 {
            return Err(DigBossRuntimeRepositoryError::InvalidState(
                "player balance changed during low-priority-tax settlement",
            ));
        }
        transaction.execute("DELETE FROM economy_ledger_context", [])?;
    }
    Ok(())
}

fn insert_audit(
    transaction: &Transaction<'_>,
    request: DigBossRuntimeCommit<'_>,
    audit: DigBossRuntimeAudit<'_>,
) -> Result<i64, rusqlite::Error> {
    let delta = request
        .proposed
        .balance
        .saturating_sub(request.expected.balance);
    transaction.execute(
        "INSERT INTO dig_actions
         (guild_id,actor_id,target_id,action_type,depth_before,depth_after,
          jc_delta,detail,created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            request.expected.key.guild_id,
            request.expected.key.discord_id,
            audit.target_id,
            audit.action_type,
            request.expected.depth,
            request.proposed.depth,
            delta,
            audit.detail_json,
            audit.created_at,
        ],
    )?;
    Ok(transaction.last_insert_rowid())
}

fn insert_artifact(
    transaction: &Transaction<'_>,
    key: DigBossRuntimeKey,
    artifact: DigBossRuntimeArtifactGrant<'_>,
) -> Result<i64, rusqlite::Error> {
    transaction.execute(
        "INSERT INTO dig_artifacts
         (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
         VALUES (?1,?2,?3,?4,?5,0)",
        params![
            key.discord_id,
            key.guild_id,
            artifact.artifact_id,
            artifact.found_at,
            i64::from(artifact.is_relic),
        ],
    )?;
    Ok(transaction.last_insert_rowid())
}

fn upsert_echo(
    transaction: &Transaction<'_>,
    guild_id: i64,
    echo: DigBossRuntimeEcho<'_>,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO dig_boss_echoes
         (guild_id,boss_id,depth,killer_discord_id,weakened_until)
         VALUES (?1,?2,?3,?4,?5)
         ON CONFLICT(guild_id,boss_id) DO UPDATE SET
          depth=excluded.depth,killer_discord_id=excluded.killer_discord_id,
          weakened_until=excluded.weakened_until",
        params![
            guild_id,
            echo.boss_id,
            echo.depth,
            echo.killer_id,
            echo.weakened_until,
        ],
    )?;
    Ok(())
}

fn query_active_duel(
    connection: &Connection,
    key: DigBossRuntimeKey,
) -> Result<Option<DigBossPausedDuelRow>, DigBossRuntimeRepositoryError> {
    Ok(connection
        .query_row(
            "SELECT boss_id,tier,mechanic_id,risk_tier,wager,player_hp,boss_hp,
                    round_num,round_log,pending_prompt,rng_state,status_effects,
                    echo_applied,echo_killer_id,player_hit,player_dmg,boss_hit,
                    boss_dmg,crit_chance,crit_bonus,created_at,last_interaction_at
               FROM dig_active_duels WHERE discord_id=?1 AND guild_id=?2",
            params![key.discord_id, key.guild_id],
            |row| {
                Ok(DigBossPausedDuelRow {
                    key,
                    boss_id: row.get(0)?,
                    boundary: row.get(1)?,
                    mechanic_id: row.get(2)?,
                    risk_tier: row.get(3)?,
                    wager: row.get(4)?,
                    player_hp: row.get(5)?,
                    boss_hp: row.get(6)?,
                    round_num: row.get(7)?,
                    round_log_json: row.get(8)?,
                    pending_prompt_json: row.get(9)?,
                    rng_state: row.get(10)?,
                    status_effects_json: row.get(11)?,
                    echo_applied: row.get::<_, i64>(12)? != 0,
                    echo_killer_id: row.get(13)?,
                    player_hit: row.get(14)?,
                    player_damage: row.get(15)?,
                    boss_hit: row.get(16)?,
                    boss_damage: row.get(17)?,
                    critical_chance: row.get(18)?,
                    critical_bonus: row.get(19)?,
                    created_at: row.get(20)?,
                    last_interaction_at: row.get(21)?,
                })
            },
        )
        .optional()?)
}

fn tick_one_gear(
    transaction: &Transaction<'_>,
    key: DigBossRuntimeKey,
    gear_id: i64,
    amount: i64,
    broken_ids: &mut Vec<i64>,
) -> Result<(), rusqlite::Error> {
    let before = transaction
        .query_row(
            "SELECT durability FROM dig_gear
              WHERE id=?1 AND discord_id=?2 AND guild_id=?3",
            params![gear_id, key.discord_id, key.guild_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(before) = before else {
        return Ok(());
    };
    let after = before.saturating_sub(amount).max(0);
    transaction.execute(
        "UPDATE dig_gear SET durability=?1
          WHERE id=?2 AND discord_id=?3 AND guild_id=?4",
        params![after, gear_id, key.discord_id, key.guild_id],
    )?;
    if before > 0 && after == 0 {
        broken_ids.push(gear_id);
    }
    Ok(())
}

fn player_exists(connection: &Connection, key: DigBossRuntimeKey) -> Result<bool, rusqlite::Error> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM players WHERE discord_id=?1 AND guild_id=?2)",
        params![key.discord_id, key.guild_id],
        |row| row.get(0),
    )
}

fn tunnel_exists(connection: &Connection, key: DigBossRuntimeKey) -> Result<bool, rusqlite::Error> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM tunnels WHERE discord_id=?1 AND guild_id=?2)",
        params![key.discord_id, key.guild_id],
        |row| row.get(0),
    )
}

#[cfg(test)]
#[path = "dig_boss_runtime/tests.rs"]
mod tests;
