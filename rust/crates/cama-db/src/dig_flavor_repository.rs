//! Existing-schema SQLite adapter for the production Dig flavor layer.
//!
//! This module deliberately owns no migration and creates no tables.  It
//! reads the already-migrated `players`, `tunnels`, `dig_actions`,
//! `dig_personality`, and `dig_dm_memory` tables, and uses the existing
//! balance-ledger trigger context for the one optional flavor JC nudge.
//!
//! The nudge is claimed by the committed `dig_actions.id` that produced the
//! narration.  A marker row in the same existing `dig_actions` table is
//! inserted under `BEGIN IMMEDIATE`; checking and inserting the marker and
//! updating the wallet therefore form one serialized, crash-safe operation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use thiserror::Error;

use crate::open_runtime_connection;

pub const FLAVOR_BONUS_ACTION_TYPE: &str = "dig_flavor_bonus";
pub const FLAVOR_BONUS_SOURCE: &str = "dig_flavor";

/// Action rows that can own a post-commit Dig flavor result.  Social and
/// inventory audit rows are deliberately excluded: accepting an arbitrary
/// actor-owned row would let a caller accidentally (or maliciously) attach a
/// second balance mutation to an unrelated action.
const FLAVOR_SOURCE_ACTION_TYPES: &[&str] = &["dig", "paid_dig", "boss_fight"];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DigFlavorBonusClaim {
    pub applied: bool,
    pub delta: i64,
}

/// Durable state for the post-commit flavor phase.  The marker lives in the
/// existing `dig_actions.detail` JSON, so this enum does not require a schema
/// migration or a second outbox table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigFlavorReceiptState {
    Applied,
    Skipped,
}

impl DigFlavorReceiptState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Skipped => "skipped",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "applied" => Some(Self::Applied),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigFlavorNpcAppearance {
    pub id_or_name: String,
    pub line: String,
}

/// A validated, replayable flavor result.  `bonus_delta` is the amount that
/// actually reached the wallet; on a duplicate this is recovered from the
/// marker rather than copied from a newly sampled LLM response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigFlavorReceipt {
    pub source_action_id: i64,
    pub state: DigFlavorReceiptState,
    pub narrative: Option<String>,
    pub tone: Option<String>,
    pub callback_reference: Option<String>,
    pub npc_appearance: Option<DigFlavorNpcAppearance>,
    pub picked_event_id: Option<String>,
    pub memory_update: Option<String>,
    pub bonus_delta: i64,
}

#[derive(Debug, Error)]
pub enum DigFlavorRepositoryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid persisted JSON in {field}: {message}")]
    InvalidJson {
        field: &'static str,
        message: String,
    },
    #[error("player {discord_id} was not found in guild {guild_id}")]
    PlayerNotFound { discord_id: i64, guild_id: i64 },
    #[error("dig action {action_id} was not found for actor {actor_id} in guild {guild_id}")]
    DigActionNotFound {
        action_id: i64,
        actor_id: i64,
        guild_id: i64,
    },
    #[error("flavor bonus action identity is required for a production nudge")]
    MissingActionIdentity,
    #[error("invalid durable Dig flavor receipt: {0}")]
    InvalidReceipt(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigFlavorTunnel {
    pub tunnel_name: String,
    pub depth: i64,
    pub max_depth: i64,
    pub pickaxe_tier: i64,
    pub prestige_level: i64,
    pub luminosity: i64,
    pub streak_days: i64,
    pub total_digs: i64,
    pub total_jc_earned: i64,
    pub miner_about: String,
    pub stat_strength: i64,
    pub stat_smarts: i64,
    pub stat_stamina: i64,
    pub stat_points: i64,
    pub equipped_relics: Vec<String>,
    pub temp_buffs: Vec<DigFlavorBuff>,
    pub mutations: Vec<String>,
    pub weather_name: Option<String>,
    pub weather_description: Option<String>,
    pub cheer_data: Option<String>,
    pub boss_progress: BTreeMap<String, String>,
    pub boss_attempts: i64,
    pub best_run_score: i64,
    pub revenge_until: i64,
    pub revenge_type: String,
    pub trap_active: bool,
    pub insured_until: i64,
    pub reinforced_until: i64,
    pub injury_state: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigFlavorBuff {
    pub name: String,
    pub digs_remaining: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigFlavorPersonality {
    pub play_style: String,
    pub choice_histogram: BTreeMap<String, i64>,
    pub notable_moments: Vec<String>,
    pub social_summary: BTreeMap<String, i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigFlavorAction {
    pub guild_id: i64,
    pub actor_id: i64,
    pub target_id: Option<i64>,
    pub action_type: String,
    pub depth_before: i64,
    pub depth_after: i64,
    pub jc_delta: i64,
    pub damage: i64,
    pub advance: i64,
    pub detail_target_id: Option<i64>,
    pub created_at: i64,
    pub detail: Option<String>,
    pub cave_in: bool,
    pub block_loss: i64,
    pub event_id: Option<String>,
    pub won: Option<bool>,
    pub risk: Option<String>,
    pub boundary: Option<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigFlavorGathered {
    pub tunnel: Option<DigFlavorTunnel>,
    pub balance: i64,
    pub personality: Option<DigFlavorPersonality>,
    pub social_actions: Vec<DigFlavorAction>,
    pub recent_actions: Vec<DigFlavorAction>,
    pub player_rank: i64,
    pub names: BTreeMap<i64, String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigSplashGathered {
    pub digger_name: Option<String>,
    pub digger_depth: i64,
    pub names: BTreeMap<i64, String>,
}

#[derive(Clone, Debug)]
pub struct DigFlavorRepository {
    path: PathBuf,
}

impl DigFlavorRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    fn connection(&self) -> Result<Connection, DigFlavorRepositoryError> {
        Ok(open_runtime_connection(&self.path)?)
    }

    pub fn gather(
        &self,
        discord_id: i64,
        guild_id: i64,
        now_unix: i64,
    ) -> Result<DigFlavorGathered, DigFlavorRepositoryError> {
        let connection = self.connection()?;
        let tunnel = load_tunnel(&connection, discord_id, guild_id)?;
        let balance = connection
            .query_row(
                "SELECT COALESCE(jopacoin_balance, 0) FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let personality = load_personality(&connection, discord_id, guild_id)?;
        let social_rows = query_actions(
            &connection,
            discord_id,
            guild_id,
            20,
            Some(now_unix.saturating_sub(48 * 3_600)),
            true,
        )?;
        let recent_rows = query_actions(&connection, discord_id, guild_id, 15, None, false)?;
        let mut ids = BTreeSet::new();
        for action in social_rows.iter().chain(recent_rows.iter()) {
            ids.insert(action.actor_id);
            if let Some(target_id) = action.target_id {
                ids.insert(target_id);
            }
            if let Some(target_id) = action.detail_target_id {
                ids.insert(target_id);
            }
        }
        if let Some(tunnel) = tunnel.as_ref() {
            for id in cheer_ids(tunnel.cheer_data.as_deref()) {
                ids.insert(id);
            }
        }
        let names = load_names(&connection, guild_id, &ids.into_iter().collect::<Vec<_>>())?;
        let player_rank = if tunnel.is_some() {
            connection
                .query_row(
                    "SELECT COUNT(*) + 1 FROM tunnels
                     WHERE guild_id=?1 AND (prestige_level >
                        COALESCE((SELECT prestige_level FROM tunnels WHERE discord_id=?2 AND guild_id=?1), 0)
                        OR (prestige_level = COALESCE((SELECT prestige_level FROM tunnels WHERE discord_id=?2 AND guild_id=?1), 0)
                            AND depth > COALESCE((SELECT depth FROM tunnels WHERE discord_id=?2 AND guild_id=?1), 0))
                        OR (prestige_level = COALESCE((SELECT prestige_level FROM tunnels WHERE discord_id=?2 AND guild_id=?1), 0)
                            AND depth = COALESCE((SELECT depth FROM tunnels WHERE discord_id=?2 AND guild_id=?1), 0)
                            AND discord_id < ?2))",
                    params![guild_id, discord_id],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(0)
        } else {
            0
        };
        Ok(DigFlavorGathered {
            tunnel,
            balance,
            personality,
            social_actions: social_rows,
            recent_actions: recent_rows,
            player_rank,
            names,
        })
    }

    /// Minimal splash projection: one tunnel depth query and one guild-scoped
    /// display-name query. It deliberately does not load personality,
    /// balances, action history, rank, or DM context.
    pub fn gather_splash(
        &self,
        digger_id: i64,
        guild_id: i64,
        victim_ids: &[i64],
    ) -> Result<DigSplashGathered, DigFlavorRepositoryError> {
        let connection = self.connection()?;
        let depth = connection
            .query_row(
                "SELECT COALESCE(depth, 0) FROM tunnels
                 WHERE discord_id=?1 AND guild_id=?2",
                params![digger_id, guild_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let mut ids = Vec::with_capacity(victim_ids.len() + 1);
        ids.push(digger_id);
        ids.extend(victim_ids.iter().copied());
        let names = load_names(&connection, guild_id, &ids)?;
        Ok(DigSplashGathered {
            digger_name: names.get(&digger_id).cloned(),
            digger_depth: depth,
            names,
        })
    }

    pub fn get_dm_memory(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<String, DigFlavorRepositoryError> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT summary_text FROM dig_dm_memory
                 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_default())
    }

    pub fn recent_diggers(
        &self,
        guild_id: i64,
        since_unix: i64,
        exclude_id: Option<i64>,
    ) -> Result<Vec<i64>, DigFlavorRepositoryError> {
        let mut sql = String::from(
            "SELECT DISTINCT actor_id FROM dig_actions
             WHERE guild_id=?1 AND action_type='dig' AND created_at>=?2",
        );
        if exclude_id.is_some() {
            sql.push_str(" AND actor_id<>?3");
        }
        let connection = self.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let rows = if let Some(exclude_id) = exclude_id {
            statement
                .query_map(params![guild_id, since_unix, exclude_id], |row| {
                    row.get::<_, i64>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            statement
                .query_map(params![guild_id, since_unix], |row| row.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows)
    }

    pub fn recent_boss_kills(
        &self,
        guild_id: i64,
        since_unix: i64,
    ) -> Result<i64, DigFlavorRepositoryError> {
        Ok(self.connection()?.query_row(
            "SELECT COUNT(*) FROM dig_actions
                 WHERE guild_id=?1 AND action_type='boss_fight'
                   AND created_at>=?2
                   AND json_extract(
                         CASE WHEN json_valid(detail) THEN detail ELSE '{}' END,
                         '$.won'
                       ) = 1",
            params![guild_id, since_unix],
            |row| row.get(0),
        )?)
    }

    /// Render the one-line summary used by the Dig DM context bundle.
    ///
    /// The Python `get_player_matches` projection calls this column
    /// `player_won`, while `_summarize_last_match` currently looks up `won`;
    /// preserving that existing key mismatch is intentional for cutover
    /// parity, so the summary is loss-biased and only includes the side.
    pub fn recent_match_summary(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<String, DigFlavorRepositoryError> {
        let row = self
            .connection()?
            .query_row(
                "SELECT mp.side
                 FROM matches AS m
                 JOIN match_participants AS mp ON mp.match_id=m.match_id
                 WHERE mp.discord_id=?1 AND mp.guild_id=?2 AND m.guild_id=?2
                 ORDER BY m.match_date DESC, m.match_id DESC
                 LIMIT 1",
                params![discord_id, guild_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        let Some(side) = row else {
            return Ok("No recent inhouse matches.".to_owned());
        };
        let mut summary = "Most recent inhouse: lost".to_owned();
        if let Some(side) = side.filter(|side| !side.is_empty()) {
            summary.push_str(&format!(", on {side}"));
        }
        summary.push('.');
        Ok(summary)
    }

    /// Recover a post-commit flavor receipt before any new AI request is
    /// issued.  The receipt is keyed by the committed Dig action identity and
    /// is stored in the existing `dig_flavor_bonus` marker detail.
    pub fn get_flavor_receipt(
        &self,
        source_action_id: i64,
        actor_id: i64,
        guild_id: i64,
    ) -> Result<Option<DigFlavorReceipt>, DigFlavorRepositoryError> {
        load_flavor_receipt(&self.connection()?, source_action_id, actor_id, guild_id)
    }

    /// Persist a validated flavor result, its optional memory rewrite, and
    /// its actual wallet/ledger delta in one serialized transaction.  A
    /// previously persisted marker wins over a replay, so a crash after this
    /// transaction cannot cause another AI-sampled delta or prose payload to
    /// be applied.
    pub fn apply_flavor_receipt(
        &self,
        receipt: DigFlavorReceipt,
        actor_id: i64,
        guild_id: i64,
        now_unix: i64,
    ) -> Result<DigFlavorReceipt, DigFlavorRepositoryError> {
        if receipt.source_action_id <= 0 {
            return Err(DigFlavorRepositoryError::InvalidReceipt(
                "source action identity must be positive".to_owned(),
            ));
        }
        if receipt.state == DigFlavorReceiptState::Skipped && receipt.bonus_delta != 0 {
            return Err(DigFlavorRepositoryError::InvalidReceipt(
                "a skipped flavor receipt must have a zero bonus delta".to_owned(),
            ));
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_flavor_source_action(&transaction, receipt.source_action_id, actor_id, guild_id)?;
        if let Some(existing) =
            load_flavor_receipt(&transaction, receipt.source_action_id, actor_id, guild_id)?
        {
            transaction.commit()?;
            return Ok(existing);
        }

        let balance_before = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance, 0) FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![actor_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or(DigFlavorRepositoryError::PlayerNotFound {
                discord_id: actor_id,
                guild_id,
            })?;
        let delta = receipt.bonus_delta;
        if receipt.state == DigFlavorReceiptState::Applied && delta != 0 {
            install_ledger_context(&transaction, actor_id, receipt.source_action_id, delta)?;
            let update_result = transaction.execute(
                "UPDATE players
                 SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                     lowest_balance_ever = CASE WHEN ?1 < 0 THEN MIN(
                         COALESCE(lowest_balance_ever,
                                  COALESCE(jopacoin_balance,0)+?1),
                         COALESCE(jopacoin_balance,0)+?1)
                         ELSE lowest_balance_ever END,
                     updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2 AND guild_id=?3",
                params![delta, actor_id, guild_id],
            );
            clear_ledger_context(&transaction)?;
            update_result?;
        }

        if let Some(memory_update) = receipt.memory_update.as_deref() {
            let memory_update = truncate_utf8_bytes(memory_update, 2_048);
            transaction.execute(
                "INSERT INTO dig_dm_memory(discord_id,guild_id,summary_text,updated_at)
                 VALUES(?1,?2,?3,?4)
                 ON CONFLICT(discord_id,guild_id) DO UPDATE SET
                   summary_text=excluded.summary_text,
                   updated_at=excluded.updated_at",
                params![actor_id, guild_id, memory_update, now_unix],
            )?;
        }

        let balance_after = balance_before.saturating_add(delta);
        let detail = serde_json::to_string(&json!({
            "source_action_id": receipt.source_action_id,
            "state": receipt.state.as_str(),
            // Keep the legacy key so existing claim_flavor_bonus readers and
            // ledger audits continue to recover the actual delta.
            "delta": delta,
            "balance_before": balance_before,
            "balance_after": balance_after,
            "narrative": receipt.narrative,
            "tone": receipt.tone,
            "callback_reference": receipt.callback_reference,
            "npc_appearance": receipt.npc_appearance.as_ref().map(|npc| json!({
                "id_or_name": npc.id_or_name,
                "line": npc.line,
            })),
            "picked_event_id": receipt.picked_event_id,
            "memory_update": receipt.memory_update,
        }))
        .map_err(|error| DigFlavorRepositoryError::InvalidJson {
            field: "flavor_receipt_detail",
            message: error.to_string(),
        })?;
        transaction.execute(
            "INSERT INTO dig_actions(
               guild_id,actor_id,target_id,action_type,depth_before,depth_after,
               jc_delta,detail,created_at
             ) VALUES(?1,?2,NULL,?3,0,0,?4,?5,?6)",
            params![
                guild_id,
                actor_id,
                FLAVOR_BONUS_ACTION_TYPE,
                delta,
                detail,
                now_unix,
            ],
        )?;
        transaction.commit()?;
        Ok(receipt)
    }

    pub fn set_dm_memory(
        &self,
        discord_id: i64,
        guild_id: i64,
        value: &str,
        now_unix: i64,
    ) -> Result<(), DigFlavorRepositoryError> {
        let value = truncate_utf8_bytes(value, 2_048);
        self.connection()?.execute(
            "INSERT INTO dig_dm_memory(discord_id,guild_id,summary_text,updated_at)
             VALUES(?1,?2,?3,?4)
             ON CONFLICT(discord_id,guild_id) DO UPDATE SET
               summary_text=excluded.summary_text, updated_at=excluded.updated_at",
            params![discord_id, guild_id, value, now_unix],
        )?;
        Ok(())
    }

    pub fn get_personality(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<DigFlavorPersonality>, DigFlavorRepositoryError> {
        let connection = self.connection()?;
        load_personality(&connection, discord_id, guild_id)
    }

    pub fn upsert_personality(
        &self,
        discord_id: i64,
        guild_id: i64,
        personality: &DigFlavorPersonality,
        now_unix: i64,
    ) -> Result<(), DigFlavorRepositoryError> {
        let histogram = serde_json::to_string(&personality.choice_histogram).map_err(|error| {
            DigFlavorRepositoryError::InvalidJson {
                field: "choice_histogram",
                message: error.to_string(),
            }
        })?;
        let notable = serde_json::to_string(&personality.notable_moments).map_err(|error| {
            DigFlavorRepositoryError::InvalidJson {
                field: "notable_moments",
                message: error.to_string(),
            }
        })?;
        let social = serde_json::to_string(&personality.social_summary).map_err(|error| {
            DigFlavorRepositoryError::InvalidJson {
                field: "social_summary",
                message: error.to_string(),
            }
        })?;
        self.connection()?.execute(
            "INSERT INTO dig_personality(
               discord_id,guild_id,play_style,choice_histogram,notable_moments,
               summary,social_summary,updated_at
             ) VALUES(?1,?2,?3,?4,?5,'',?6,?7)
             ON CONFLICT(discord_id,guild_id) DO UPDATE SET
               play_style=excluded.play_style,
               choice_histogram=excluded.choice_histogram,
               notable_moments=excluded.notable_moments,
               social_summary=excluded.social_summary,
               updated_at=excluded.updated_at",
            params![
                discord_id,
                guild_id,
                personality.play_style,
                histogram,
                notable,
                social,
                now_unix,
            ],
        )?;
        Ok(())
    }

    /// Atomically claim and apply the optional flavor nudge for one committed
    /// Dig action. Returns `true` only when this call changed the wallet.
    pub fn claim_flavor_bonus(
        &self,
        dig_action_id: Option<i64>,
        discord_id: i64,
        guild_id: i64,
        delta: i64,
        now_unix: i64,
    ) -> Result<DigFlavorBonusClaim, DigFlavorRepositoryError> {
        let action_id = dig_action_id.ok_or(DigFlavorRepositoryError::MissingActionIdentity)?;
        if delta == 0 {
            return Ok(DigFlavorBonusClaim::default());
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source_action_type = transaction
            .query_row(
                "SELECT action_type FROM dig_actions
                 WHERE id=?1 AND guild_id=?2 AND actor_id=?3",
                params![action_id, guild_id, discord_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if !source_action_type
            .as_deref()
            .is_some_and(|action_type| FLAVOR_SOURCE_ACTION_TYPES.contains(&action_type))
        {
            return Err(DigFlavorRepositoryError::DigActionNotFound {
                action_id,
                actor_id: discord_id,
                guild_id,
            });
        }
        if let Some(existing_delta) =
            flavor_bonus_marker_delta(&transaction, action_id, discord_id, guild_id)?
        {
            transaction.commit()?;
            return Ok(DigFlavorBonusClaim {
                applied: false,
                delta: existing_delta,
            });
        }
        let balance = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance, 0) FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or(DigFlavorRepositoryError::PlayerNotFound {
                discord_id,
                guild_id,
            })?;
        install_ledger_context(&transaction, discord_id, action_id, delta)?;
        let update_result = transaction.execute(
            "UPDATE players
             SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                 lowest_balance_ever = CASE WHEN ?1 < 0 THEN MIN(
                     COALESCE(lowest_balance_ever,
                              COALESCE(jopacoin_balance,0)+?1),
                     COALESCE(jopacoin_balance,0)+?1)
                     ELSE lowest_balance_ever END,
                 updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3",
            params![delta, discord_id, guild_id],
        );
        clear_ledger_context(&transaction)?;
        update_result?;
        let detail = serde_json::to_string(&json!({
            "source_action_id": action_id,
            "delta": delta,
            "balance_before": balance,
            "balance_after": balance.saturating_add(delta),
        }))
        .map_err(|error| DigFlavorRepositoryError::InvalidJson {
            field: "flavor_bonus_detail",
            message: error.to_string(),
        })?;
        transaction.execute(
            "INSERT INTO dig_actions(
               guild_id,actor_id,target_id,action_type,depth_before,depth_after,
               jc_delta,detail,created_at
             ) VALUES(?1,?2,NULL,?3,0,0,?4,?5,?6)",
            params![
                guild_id,
                discord_id,
                FLAVOR_BONUS_ACTION_TYPE,
                delta,
                detail,
                now_unix,
            ],
        )?;
        transaction.commit()?;
        Ok(DigFlavorBonusClaim {
            applied: true,
            delta,
        })
    }
}

fn load_tunnel(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<DigFlavorTunnel>, DigFlavorRepositoryError> {
    let raw = connection
        .query_row(
            "SELECT tunnel_name, depth, max_depth, pickaxe_tier, prestige_level,
                    COALESCE(luminosity,100), streak_days, total_digs,
                    total_jc_earned, COALESCE(miner_about,''),
                    COALESCE(stat_strength,0), COALESCE(stat_smarts,0),
                    COALESCE(stat_stamina,0),
                    CASE WHEN COALESCE(stat_points,0)=0 THEN 5 ELSE stat_points END,
                    temp_buffs, mutations, cheer_data,
                    boss_progress, boss_attempts, COALESCE(best_run_score,0),
                    COALESCE(revenge_until,0), revenge_type,
                    COALESCE(trap_active,0), COALESCE(insured_until,0),
                    COALESCE(reinforced_until,0), injury_state
             FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, Option<String>>(15)?,
                    row.get::<_, Option<String>>(16)?,
                    row.get::<_, Option<String>>(17)?,
                    row.get::<_, Option<String>>(18)?,
                    row.get::<_, i64>(19)?,
                    row.get::<_, i64>(20)?,
                    row.get::<_, Option<String>>(21)?,
                    row.get::<_, i64>(22)?,
                    row.get::<_, i64>(23)?,
                    row.get::<_, i64>(24)?,
                    row.get::<_, Option<String>>(25)?,
                ))
            },
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    Ok(Some(DigFlavorTunnel {
        tunnel_name: raw.0.unwrap_or_else(|| "Unknown Tunnel".to_owned()),
        depth: raw.1,
        max_depth: raw.2,
        pickaxe_tier: raw.3,
        prestige_level: raw.4,
        luminosity: raw.5,
        streak_days: raw.6,
        total_digs: raw.7,
        total_jc_earned: raw.8,
        miner_about: raw.9,
        stat_strength: raw.10,
        stat_smarts: raw.11,
        stat_stamina: raw.12,
        stat_points: raw.13,
        // Python passes the raw `tunnels` row to the prompt builder.  That
        // row has no `equipped_relics` key, so the Rust cutover intentionally
        // leaves this prompt field empty until Python changes in lockstep.
        equipped_relics: Vec::new(),
        temp_buffs: parse_buffs(raw.14)?,
        mutations: parse_string_list(raw.15, "mutations")?,
        weather_name: None,
        weather_description: None,
        cheer_data: raw.16,
        boss_progress: parse_string_map(raw.17, "boss_progress")?,
        boss_attempts: parse_i64_scalar(raw.18),
        best_run_score: raw.19,
        revenge_until: raw.20,
        revenge_type: raw.21.unwrap_or_default(),
        trap_active: raw.22 != 0,
        insured_until: raw.23,
        reinforced_until: raw.24,
        injury_state: raw.25,
    }))
}

fn load_personality(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<DigFlavorPersonality>, DigFlavorRepositoryError> {
    let raw = connection
        .query_row(
            "SELECT play_style, choice_histogram, notable_moments, social_summary
             FROM dig_personality WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    Ok(Some(DigFlavorPersonality {
        play_style: raw.0.unwrap_or_else(|| "unknown".to_owned()),
        choice_histogram: parse_i64_map(raw.1, "choice_histogram")?,
        notable_moments: parse_string_list(raw.2, "notable_moments")?,
        social_summary: parse_i64_map(raw.3, "social_summary")?,
    }))
}

fn query_actions(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
    limit: i64,
    since_unix: Option<i64>,
    social_only: bool,
) -> Result<Vec<DigFlavorAction>, DigFlavorRepositoryError> {
    let type_filter = social_only.then_some(" AND action_type IN ('sabotage','help','cheer')");
    // Keep one stable placeholder layout for the actor and target branches;
    // `NULL` means no lookback (the general history window), while a value
    // applies Python's 48-hour social cutoff.
    let since_filter = " AND (?3 IS NULL OR created_at >= ?3)";
    let sql = format!(
        "SELECT guild_id,actor_id,target_id,action_type,depth_before,depth_after,
                COALESCE(jc_delta,0),detail,created_at
         FROM (
           SELECT guild_id,actor_id,target_id,action_type,depth_before,depth_after,jc_delta,detail,created_at
           FROM dig_actions
           WHERE guild_id=?1 AND actor_id=?2
             AND action_type <> 'dig_flavor_bonus'{type_filter}{since_filter}
           UNION ALL
           SELECT guild_id,actor_id,target_id,action_type,depth_before,depth_after,jc_delta,detail,created_at
           FROM dig_actions
           WHERE guild_id=?1 AND target_id=?2 AND actor_id<>?2
             AND action_type <> 'dig_flavor_bonus'{type_filter}{since_filter}
         ) AS player_actions ORDER BY created_at DESC LIMIT ?4",
        type_filter = type_filter.unwrap_or(""),
        since_filter = since_filter,
    );
    let mut statement = connection.prepare(&sql)?;
    let since = since_unix;
    statement
        .query_map(params![guild_id, discord_id, since, limit], map_action)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn map_action(row: &rusqlite::Row<'_>) -> rusqlite::Result<DigFlavorAction> {
    let detail = row.get::<_, Option<String>>(7)?;
    let parsed = detail
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    let object = parsed.as_ref().and_then(Value::as_object);
    Ok(DigFlavorAction {
        guild_id: row.get(0)?,
        actor_id: row.get(1)?,
        target_id: row.get(2)?,
        action_type: row.get(3)?,
        depth_before: row.get(4)?,
        depth_after: row.get(5)?,
        jc_delta: row.get(6)?,
        damage: object
            .and_then(|object| object.get("damage"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        advance: object
            .and_then(|object| object.get("advance"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        // Python's prompt builder reads `target_id` from the detail object,
        // not the normalized dig_actions column; preserve its odd but
        // observable actor/target classification.
        detail_target_id: object
            .and_then(|object| object.get("target_id"))
            .and_then(Value::as_i64),
        created_at: row.get(8)?,
        detail: detail.clone(),
        cave_in: object
            .and_then(|object| object.get("cave_in"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        block_loss: object
            .and_then(|object| object.get("block_loss"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        event_id: object
            .and_then(|object| object.get("event_id"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        won: object
            .and_then(|object| object.get("won"))
            .and_then(Value::as_bool),
        risk: object
            .and_then(|object| object.get("risk"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        boundary: object
            .and_then(|object| object.get("boundary"))
            .and_then(Value::as_i64),
    })
}

fn load_names(
    connection: &Connection,
    guild_id: i64,
    ids: &[i64],
) -> Result<BTreeMap<i64, String>, DigFlavorRepositoryError> {
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let ids = ids.iter().copied().collect::<BTreeSet<_>>();
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT discord_id,discord_username FROM players
         WHERE guild_id=?1 AND discord_id IN ({placeholders})"
    );
    let mut statement = connection.prepare(&sql)?;
    let mut values: Vec<rusqlite::types::Value> = Vec::with_capacity(ids.len() + 1);
    values.push(guild_id.into());
    values.extend(ids.iter().copied().map(Into::into));
    let mut names = BTreeMap::new();
    for row in statement.query_map(rusqlite::params_from_iter(values), |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })? {
        let (id, name) = row?;
        names.insert(id, name);
    }
    Ok(names)
}

fn cheer_ids(raw: Option<&str>) -> Vec<i64> {
    raw.and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| entry.get("cheerer_id").and_then(Value::as_i64))
        .collect()
}

fn parse_string_list(
    raw: Option<String>,
    field: &'static str,
) -> Result<Vec<String>, DigFlavorRepositoryError> {
    let Some(raw) = raw.filter(|value| !value.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    let value = serde_json::from_str::<Value>(&raw).map_err(|error| {
        DigFlavorRepositoryError::InvalidJson {
            field,
            message: error.to_string(),
        }
    })?;
    Ok(value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            entry
                .as_str()
                .map(str::to_owned)
                .or_else(|| entry.get("name").and_then(Value::as_str).map(str::to_owned))
                .or_else(|| {
                    entry
                        .get("relic_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .or_else(|| entry.get("id").and_then(Value::as_str).map(str::to_owned))
                .or_else(|| {
                    entry
                        .get("summary")
                        .or_else(|| entry.get("description"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
        })
        .collect())
}

fn parse_buffs(raw: Option<String>) -> Result<Vec<DigFlavorBuff>, DigFlavorRepositoryError> {
    let Some(raw) = raw.filter(|value| !value.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    let value = serde_json::from_str::<Value>(&raw).map_err(|error| {
        DigFlavorRepositoryError::InvalidJson {
            field: "temp_buffs",
            message: error.to_string(),
        }
    })?;
    let entries = value.as_array().cloned().unwrap_or_else(|| vec![value]);
    Ok(entries
        .into_iter()
        .filter_map(|entry| {
            let object = entry.as_object()?;
            let remaining = object
                .get("digs_remaining")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            (remaining > 0).then(|| DigFlavorBuff {
                name: object
                    .get("name")
                    .or_else(|| object.get("source"))
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown Buff")
                    .to_owned(),
                digs_remaining: remaining,
            })
        })
        .collect())
}

fn parse_i64_map(
    raw: Option<String>,
    field: &'static str,
) -> Result<BTreeMap<String, i64>, DigFlavorRepositoryError> {
    let Some(raw) = raw.filter(|value| !value.trim().is_empty()) else {
        return Ok(BTreeMap::new());
    };
    let value = serde_json::from_str::<Value>(&raw).map_err(|error| {
        DigFlavorRepositoryError::InvalidJson {
            field,
            message: error.to_string(),
        }
    })?;
    Ok(value
        .as_object()
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| value.as_i64().map(|value| (key.clone(), value)))
                .collect()
        })
        .unwrap_or_default())
}

fn parse_string_map(
    raw: Option<String>,
    field: &'static str,
) -> Result<BTreeMap<String, String>, DigFlavorRepositoryError> {
    let Some(raw) = raw.filter(|value| !value.trim().is_empty()) else {
        return Ok(BTreeMap::new());
    };
    let value = serde_json::from_str::<Value>(&raw).map_err(|error| {
        DigFlavorRepositoryError::InvalidJson {
            field,
            message: error.to_string(),
        }
    })?;
    Ok(value
        .as_object()
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| {
                    value
                        .as_str()
                        .map(|value| (key.clone(), value.to_owned()))
                        .or_else(|| {
                            value
                                .get("status")
                                .and_then(Value::as_str)
                                .map(|value| (key.clone(), value.to_owned()))
                        })
                })
                .collect()
        })
        .unwrap_or_default())
}

fn parse_i64_scalar(raw: Option<String>) -> i64 {
    raw.and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(0)
}

fn validate_flavor_source_action(
    transaction: &Transaction<'_>,
    action_id: i64,
    actor_id: i64,
    guild_id: i64,
) -> Result<(), DigFlavorRepositoryError> {
    let source_action_type = transaction
        .query_row(
            "SELECT action_type FROM dig_actions
             WHERE id=?1 AND guild_id=?2 AND actor_id=?3",
            params![action_id, guild_id, actor_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if !source_action_type
        .as_deref()
        .is_some_and(|action_type| FLAVOR_SOURCE_ACTION_TYPES.contains(&action_type))
    {
        return Err(DigFlavorRepositoryError::DigActionNotFound {
            action_id,
            actor_id,
            guild_id,
        });
    }
    Ok(())
}

fn parse_flavor_receipt(detail: &str) -> Option<DigFlavorReceipt> {
    let value = serde_json::from_str::<Value>(detail).ok()?;
    let source_action_id = value.get("source_action_id")?.as_i64()?;
    let state = value
        .get("state")
        .and_then(Value::as_str)
        .and_then(DigFlavorReceiptState::from_str)
        // Rows written by the first adapter revision predate the typed state;
        // those rows are durable applied bonus markers with no prose payload.
        .unwrap_or(DigFlavorReceiptState::Applied);
    let npc_appearance = value.get("npc_appearance").and_then(|raw| {
        let object = raw.as_object()?;
        Some(DigFlavorNpcAppearance {
            id_or_name: object.get("id_or_name")?.as_str()?.to_owned(),
            line: object.get("line")?.as_str()?.to_owned(),
        })
    });
    Some(DigFlavorReceipt {
        source_action_id,
        state,
        narrative: value
            .get("narrative")
            .and_then(Value::as_str)
            .map(str::to_owned),
        tone: value.get("tone").and_then(Value::as_str).map(str::to_owned),
        callback_reference: value
            .get("callback_reference")
            .and_then(Value::as_str)
            .map(str::to_owned),
        npc_appearance,
        picked_event_id: value
            .get("picked_event_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        memory_update: value
            .get("memory_update")
            .and_then(Value::as_str)
            .map(str::to_owned),
        bonus_delta: value
            .get("delta")
            .or_else(|| value.get("bonus_delta"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
    })
}

fn load_flavor_receipt(
    connection: &Connection,
    source_action_id: i64,
    actor_id: i64,
    guild_id: i64,
) -> Result<Option<DigFlavorReceipt>, DigFlavorRepositoryError> {
    let mut statement = connection.prepare(
        "SELECT detail FROM dig_actions
         WHERE guild_id=?1 AND actor_id=?2 AND action_type=?3",
    )?;
    for row in statement.query_map(
        params![guild_id, actor_id, FLAVOR_BONUS_ACTION_TYPE],
        |row| row.get::<_, Option<String>>(0),
    )? {
        let Some(detail) = row? else { continue };
        if let Some(receipt) = parse_flavor_receipt(&detail)
            && receipt.source_action_id == source_action_id
        {
            return Ok(Some(receipt));
        }
    }
    Ok(None)
}

fn flavor_bonus_marker_delta(
    transaction: &Transaction<'_>,
    source_action_id: i64,
    actor_id: i64,
    guild_id: i64,
) -> Result<Option<i64>, rusqlite::Error> {
    let receipt = load_flavor_receipt(transaction, source_action_id, actor_id, guild_id).map_err(
        |error| match error {
            DigFlavorRepositoryError::Sqlite(error) => error,
            _ => rusqlite::Error::InvalidQuery,
        },
    )?;
    Ok(receipt.map(|receipt| receipt.bonus_delta))
}

fn install_ledger_context(
    transaction: &Transaction<'_>,
    actor_id: i64,
    source_action_id: i64,
    delta: i64,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context(
           id,source,actor_id,related_type,related_id,reason,metadata
         ) VALUES(1,?1,?2,?3,?4,?5,?6)",
        params![
            FLAVOR_BONUS_SOURCE,
            actor_id,
            "dig_flavor_bonus",
            source_action_id.to_string(),
            "Dig flavor bonus",
            json!({"source_action_id": source_action_id, "delta": delta}).to_string(),
        ],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn truncate_utf8_bytes(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_owned();
    }
    let mut end = maximum.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    const ACTOR: i64 = -91_001;
    const OTHER: i64 = -91_002;
    const GUILD: i64 = 91_000;
    const NOW: i64 = 2_000_000_000;

    struct Fixture {
        database: NamedTempFile,
        repository: DigFlavorRepository,
    }

    impl Fixture {
        fn new() -> Self {
            let database = NamedTempFile::new().expect("temporary flavor database");
            Connection::open(database.path())
                .expect("open flavor database")
                .execute_batch(
                    "PRAGMA journal_mode=WAL;
                     CREATE TABLE players (
                         discord_id INTEGER NOT NULL,
                         guild_id INTEGER NOT NULL,
                         discord_username TEXT NOT NULL,
                         jopacoin_balance INTEGER,
                         lowest_balance_ever INTEGER,
                         updated_at TEXT,
                         PRIMARY KEY (discord_id,guild_id)
                     );
                     CREATE TABLE tunnels (
                         discord_id INTEGER NOT NULL,
                         guild_id INTEGER NOT NULL,
                         depth INTEGER NOT NULL DEFAULT 0,
                         max_depth INTEGER NOT NULL DEFAULT 0,
                         total_digs INTEGER NOT NULL DEFAULT 0,
                         total_jc_earned INTEGER NOT NULL DEFAULT 0,
                         streak_days INTEGER NOT NULL DEFAULT 0,
                         pickaxe_tier INTEGER NOT NULL DEFAULT 0,
                         prestige_level INTEGER NOT NULL DEFAULT 0,
                         tunnel_name TEXT,
                         luminosity INTEGER,
                         temp_buffs TEXT,
                         mutations TEXT,
                         cheer_data TEXT,
                         miner_about TEXT,
                         stat_strength INTEGER,
                         stat_smarts INTEGER,
                         stat_stamina INTEGER,
                         stat_points INTEGER,
                         boss_progress TEXT,
                         boss_attempts TEXT,
                         best_run_score INTEGER,
                         revenge_until INTEGER,
                         revenge_type TEXT,
                         trap_active INTEGER,
                         insured_until INTEGER,
                         reinforced_until INTEGER,
                         injury_state TEXT,
                         PRIMARY KEY (discord_id,guild_id)
                     );
                     CREATE TABLE dig_artifacts (
                         id INTEGER PRIMARY KEY AUTOINCREMENT,
                         discord_id INTEGER NOT NULL,
                         guild_id INTEGER NOT NULL,
                         artifact_id TEXT NOT NULL,
                         found_at INTEGER NOT NULL,
                         is_relic INTEGER NOT NULL DEFAULT 0,
                         equipped INTEGER NOT NULL DEFAULT 0
                     );
                     CREATE TABLE dig_actions (
                         id INTEGER PRIMARY KEY AUTOINCREMENT,
                         guild_id INTEGER NOT NULL,
                         actor_id INTEGER NOT NULL,
                         target_id INTEGER,
                         action_type TEXT NOT NULL,
                         depth_before INTEGER NOT NULL DEFAULT 0,
                         depth_after INTEGER NOT NULL DEFAULT 0,
                         jc_delta INTEGER NOT NULL DEFAULT 0,
                         detail TEXT,
                         created_at INTEGER NOT NULL
                     );
                     CREATE TABLE matches (
                         match_id INTEGER PRIMARY KEY,
                         guild_id INTEGER NOT NULL,
                         team1_players TEXT NOT NULL DEFAULT '[]',
                         team2_players TEXT NOT NULL DEFAULT '[]',
                         winning_team INTEGER,
                         match_date TEXT NOT NULL
                     );
                     CREATE TABLE match_participants (
                         match_id INTEGER NOT NULL,
                         discord_id INTEGER NOT NULL,
                         guild_id INTEGER NOT NULL,
                         team_number INTEGER,
                         won INTEGER,
                         side TEXT,
                         PRIMARY KEY (match_id,discord_id)
                     );
                     CREATE TABLE dig_personality (
                         discord_id INTEGER NOT NULL,
                         guild_id INTEGER NOT NULL,
                         play_style TEXT,
                         choice_histogram TEXT,
                         notable_moments TEXT,
                         summary TEXT,
                         social_summary TEXT,
                         updated_at INTEGER,
                         PRIMARY KEY (discord_id,guild_id)
                     );
                     CREATE TABLE dig_dm_memory (
                         discord_id INTEGER NOT NULL,
                         guild_id INTEGER NOT NULL,
                         summary_text TEXT NOT NULL DEFAULT '',
                         updated_at INTEGER NOT NULL DEFAULT 0,
                         PRIMARY KEY (discord_id,guild_id)
                     );
                     CREATE TABLE economy_ledger_context (
                         id INTEGER PRIMARY KEY CHECK(id=1),
                         source TEXT,
                         actor_id INTEGER,
                         related_type TEXT,
                         related_id TEXT,
                         reason TEXT,
                         metadata TEXT
                     );
                     CREATE TABLE economy_ledger_entries (
                         ledger_id INTEGER PRIMARY KEY AUTOINCREMENT,
                         guild_id INTEGER NOT NULL,
                         account_id INTEGER,
                         delta INTEGER NOT NULL,
                         balance_before INTEGER,
                         balance_after INTEGER,
                         source TEXT NOT NULL,
                         related_type TEXT,
                         related_id TEXT,
                         reason TEXT,
                         metadata TEXT
                     );
                     CREATE TRIGGER ledger_player_update
                     AFTER UPDATE OF jopacoin_balance ON players
                     WHEN OLD.jopacoin_balance IS NOT NEW.jopacoin_balance
                     BEGIN
                         INSERT INTO economy_ledger_entries(
                             guild_id,account_id,delta,balance_before,balance_after,
                             source,related_type,related_id,reason,metadata
                         ) VALUES(
                             NEW.guild_id,NEW.discord_id,
                             NEW.jopacoin_balance-OLD.jopacoin_balance,
                             OLD.jopacoin_balance,NEW.jopacoin_balance,
                             COALESCE((SELECT source FROM economy_ledger_context WHERE id=1),'balance_update'),
                             (SELECT related_type FROM economy_ledger_context WHERE id=1),
                             (SELECT related_id FROM economy_ledger_context WHERE id=1),
                             (SELECT reason FROM economy_ledger_context WHERE id=1),
                             (SELECT metadata FROM economy_ledger_context WHERE id=1)
                         );
                     END;",
                )
                .expect("create flavor fixture schema");
            let repository = DigFlavorRepository::new(database.path());
            let fixture = Self {
                database,
                repository,
            };
            fixture.seed_player(ACTOR, "actor", 100, Some(100));
            fixture.seed_player(OTHER, "other", 20, None);
            fixture
        }

        fn connection(&self) -> Connection {
            Connection::open(self.database.path()).expect("fixture connection")
        }

        fn seed_player(&self, discord_id: i64, name: &str, balance: i64, lowest: Option<i64>) {
            self.connection()
                .execute(
                    "INSERT INTO players(
                         discord_id,guild_id,discord_username,jopacoin_balance,
                         lowest_balance_ever
                     ) VALUES(?1,?2,?3,?4,?5)",
                    params![discord_id, GUILD, name, balance, lowest],
                )
                .expect("seed player");
        }

        fn seed_tunnel(&self, discord_id: i64, depth: i64) {
            self.connection()
                .execute(
                    "INSERT INTO tunnels(
                         discord_id,guild_id,depth,max_depth,total_digs,
                         total_jc_earned,streak_days,pickaxe_tier,prestige_level,
                         tunnel_name,luminosity,temp_buffs,mutations,cheer_data,
                         miner_about,stat_strength,stat_smarts,stat_stamina,stat_points,
                         boss_progress,boss_attempts,best_run_score,revenge_until,
                         revenge_type,trap_active,insured_until,reinforced_until,injury_state
                     ) VALUES(
                         ?1,?2,?3,100,8,90,2,1,0,'test tunnel',88,
                         '[{\"name\":\"lamp\",\"digs_remaining\":2}]',
                         '[\"moss\"]','[]','about',1,2,3,5,'{}','0',4,0,'',0,0,0,NULL
                     )",
                    params![discord_id, GUILD, depth],
                )
                .expect("seed tunnel");
        }

        fn seed_action(
            &self,
            actor_id: i64,
            action_type: &str,
            detail: Option<&str>,
            created_at: i64,
        ) -> i64 {
            let connection = self.connection();
            connection
                .execute(
                    "INSERT INTO dig_actions(
                         guild_id,actor_id,action_type,depth_before,depth_after,
                         jc_delta,detail,created_at
                     ) VALUES(?1,?2,?3,1,2,0,?4,?5)",
                    params![GUILD, actor_id, action_type, detail, created_at],
                )
                .expect("seed action");
            connection.last_insert_rowid()
        }

        fn seed_match(
            &self,
            match_id: i64,
            guild_id: i64,
            discord_id: i64,
            won: i64,
            side: Option<&str>,
            match_date: &str,
        ) {
            let connection = self.connection();
            connection
                .execute(
                    "INSERT INTO matches(
                         match_id,guild_id,team1_players,team2_players,
                         winning_team,match_date
                     ) VALUES(?1,?2,'[]','[]',?3,?4)",
                    params![match_id, guild_id, 1, match_date],
                )
                .expect("seed match");
            connection
                .execute(
                    "INSERT INTO match_participants(
                         match_id,discord_id,guild_id,team_number,won,side
                     ) VALUES(?1,?2,?3,1,?4,?5)",
                    params![match_id, discord_id, guild_id, won, side],
                )
                .expect("seed match participant");
        }
    }

    #[test]
    fn recent_diggers_bind_optional_exclusion_shape() {
        let fixture = Fixture::new();
        fixture.seed_action(ACTOR, "dig", None, NOW);
        fixture.seed_action(OTHER, "dig", None, NOW - 1);
        assert_eq!(
            fixture
                .repository
                .recent_diggers(GUILD, NOW - 10, None)
                .unwrap(),
            vec![ACTOR, OTHER]
        );
        assert_eq!(
            fixture
                .repository
                .recent_diggers(GUILD, NOW - 10, Some(ACTOR))
                .unwrap(),
            vec![OTHER]
        );
    }

    #[test]
    fn recent_boss_kills_accept_compact_and_spaced_json_but_ignore_bad_rows() {
        let fixture = Fixture::new();
        for (detail, created_at) in [
            (Some(r#"{"won":true}"#), NOW),
            (Some(r#"{"won": true}"#), NOW - 1),
            (Some(r#"{"won":false}"#), NOW - 2),
            (Some("not json"), NOW - 3),
            (None, NOW - 4),
        ] {
            fixture.seed_action(ACTOR, "boss_fight", detail, created_at);
        }
        assert_eq!(
            fixture
                .repository
                .recent_boss_kills(GUILD, NOW - 10)
                .unwrap(),
            2
        );
    }

    #[test]
    fn recent_match_summary_preserves_python_projection_and_side() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture
                .repository
                .recent_match_summary(ACTOR, GUILD)
                .unwrap(),
            "No recent inhouse matches."
        );

        // Python's MatchRepository calls this field `player_won`, while the
        // summary currently reads `won`; even a persisted win therefore
        // intentionally renders as a loss until that upstream projection is
        // changed in both implementations.
        fixture.seed_match(1, GUILD, ACTOR, 1, Some("radiant"), "2026-01-01 12:00:00");
        assert_eq!(
            fixture
                .repository
                .recent_match_summary(ACTOR, GUILD)
                .unwrap(),
            "Most recent inhouse: lost, on radiant."
        );

        fixture.seed_match(2, GUILD, ACTOR, 0, Some("dire"), "2026-01-02 12:00:00");
        assert_eq!(
            fixture
                .repository
                .recent_match_summary(ACTOR, GUILD)
                .unwrap(),
            "Most recent inhouse: lost, on dire."
        );
    }

    #[test]
    fn recent_match_summary_is_guild_scoped_and_orders_date_then_id() {
        let fixture = Fixture::new();
        let other_guild = GUILD + 1;
        // Date wins over id: the newer row has the smaller id.
        fixture.seed_match(100, GUILD, ACTOR, 1, Some("old"), "2026-01-03 12:00:00");
        fixture.seed_match(3, GUILD, ACTOR, 1, Some("new"), "2026-01-04 12:00:00");
        // For equal dates, the larger match id wins.
        fixture.seed_match(
            4,
            GUILD,
            ACTOR,
            1,
            Some("same-date-high-id"),
            "2026-01-04 12:00:00",
        );
        fixture.seed_match(
            1000,
            other_guild,
            ACTOR,
            1,
            Some("foreign-guild"),
            "2027-01-01 12:00:00",
        );

        assert_eq!(
            fixture
                .repository
                .recent_match_summary(ACTOR, GUILD)
                .unwrap(),
            "Most recent inhouse: lost, on same-date-high-id."
        );
        assert_eq!(
            fixture
                .repository
                .recent_match_summary(ACTOR, other_guild)
                .unwrap(),
            "Most recent inhouse: lost, on foreign-guild."
        );
    }

    #[test]
    fn claim_bonus_is_action_scoped_idempotent_and_tracks_negative_low() {
        let fixture = Fixture::new();
        let source = fixture.seed_action(ACTOR, "dig", None, NOW);
        let first = fixture
            .repository
            .claim_flavor_bonus(Some(source), ACTOR, GUILD, -15, NOW)
            .unwrap();
        assert_eq!(
            first,
            DigFlavorBonusClaim {
                applied: true,
                delta: -15
            }
        );
        let replay = fixture
            .repository
            .claim_flavor_bonus(Some(source), ACTOR, GUILD, 50, NOW + 1)
            .unwrap();
        assert_eq!(
            replay,
            DigFlavorBonusClaim {
                applied: false,
                delta: -15
            }
        );
        let connection = fixture.connection();
        let wallet: i64 = connection
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![ACTOR, GUILD],
                |row| row.get(0),
            )
            .unwrap();
        let lowest: i64 = connection
            .query_row(
                "SELECT lowest_balance_ever FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![ACTOR, GUILD],
                |row| row.get(0),
            )
            .unwrap();
        let ledger_rows: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries WHERE source=?1",
                [FLAVOR_BONUS_SOURCE],
                |row| row.get(0),
            )
            .unwrap();
        let marker_rows: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM dig_actions WHERE action_type=?1",
                [FLAVOR_BONUS_ACTION_TYPE],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(wallet, 85);
        assert_eq!(lowest, 85);
        assert_eq!(ledger_rows, 1);
        assert_eq!(marker_rows, 1);
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
    }

    #[test]
    fn typed_flavor_receipt_persists_payload_memory_and_replays_original() {
        let fixture = Fixture::new();
        let source = fixture.seed_action(ACTOR, "dig", None, NOW);
        let receipt = DigFlavorReceipt {
            source_action_id: source,
            state: DigFlavorReceiptState::Applied,
            narrative: Some("Dust remembers the shape of your hands.".to_owned()),
            tone: Some("industrial_grim".to_owned()),
            callback_reference: Some("the old seam".to_owned()),
            npc_appearance: Some(DigFlavorNpcAppearance {
                id_or_name: "the_old_hand".to_owned(),
                line: "Keep the lamp low.".to_owned(),
            }),
            picked_event_id: Some("old_seam".to_owned()),
            memory_update: Some("The old seam answered once.".to_owned()),
            bonus_delta: 4,
        };
        assert_eq!(
            fixture
                .repository
                .apply_flavor_receipt(receipt.clone(), ACTOR, GUILD, NOW)
                .unwrap(),
            receipt
        );

        let recovered = fixture
            .repository
            .get_flavor_receipt(source, ACTOR, GUILD)
            .unwrap()
            .expect("durable flavor receipt");
        assert_eq!(recovered, receipt);
        assert_eq!(
            fixture.repository.get_dm_memory(ACTOR, GUILD).unwrap(),
            "The old seam answered once."
        );
        assert_eq!(
            fixture
                .connection()
                .query_row(
                    "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                    params![ACTOR, GUILD],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            104
        );

        let replay = DigFlavorReceipt {
            narrative: Some("A different sampled line.".to_owned()),
            bonus_delta: 99,
            ..receipt.clone()
        };
        assert_eq!(
            fixture
                .repository
                .apply_flavor_receipt(replay, ACTOR, GUILD, NOW + 1)
                .unwrap(),
            receipt
        );
        assert_eq!(
            fixture
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM economy_ledger_entries WHERE source=?1",
                    [FLAVOR_BONUS_SOURCE],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            fixture
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM dig_actions WHERE action_type=?1",
                    [FLAVOR_BONUS_ACTION_TYPE],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn typed_flavor_receipt_can_persist_skipped_without_wallet_mutation() {
        let fixture = Fixture::new();
        let source = fixture.seed_action(ACTOR, "paid_dig", None, NOW);
        let receipt = DigFlavorReceipt {
            source_action_id: source,
            state: DigFlavorReceiptState::Skipped,
            narrative: None,
            tone: None,
            callback_reference: None,
            npc_appearance: None,
            picked_event_id: None,
            memory_update: None,
            bonus_delta: 0,
        };
        assert_eq!(
            fixture
                .repository
                .apply_flavor_receipt(receipt.clone(), ACTOR, GUILD, NOW)
                .unwrap(),
            receipt
        );
        assert_eq!(
            fixture
                .repository
                .get_flavor_receipt(source, ACTOR, GUILD)
                .unwrap(),
            Some(receipt)
        );
        assert_eq!(
            fixture
                .connection()
                .query_row(
                    "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                    params![ACTOR, GUILD],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            100
        );
    }

    #[test]
    fn positive_bonus_does_not_initialize_or_raise_lowest_balance() {
        let fixture = Fixture::new();
        let source = fixture.seed_action(OTHER, "dig", None, NOW);
        fixture
            .repository
            .claim_flavor_bonus(Some(source), OTHER, GUILD, 7, NOW)
            .unwrap();
        let connection = fixture.connection();
        let (balance, lowest): (i64, Option<i64>) = connection
            .query_row(
                "SELECT jopacoin_balance,lowest_balance_ever
                 FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![OTHER, GUILD],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(balance, 27);
        assert_eq!(lowest, None);
    }

    #[test]
    fn non_dig_source_action_is_rejected() {
        let fixture = Fixture::new();
        let source = fixture.seed_action(ACTOR, "event", None, NOW);
        assert!(matches!(
            fixture
                .repository
                .claim_flavor_bonus(Some(source), ACTOR, GUILD, 3, NOW),
            Err(DigFlavorRepositoryError::DigActionNotFound { .. })
        ));
    }

    #[test]
    fn gather_uses_actual_tunnel_columns_and_missing_player_rank_is_zero() {
        let fixture = Fixture::new();
        fixture.seed_tunnel(ACTOR, 42);
        let gathered = fixture.repository.gather(ACTOR, GUILD, NOW).unwrap();
        let tunnel = gathered.tunnel.expect("tunnel projection");
        assert_eq!(tunnel.depth, 42);
        assert_eq!(tunnel.equipped_relics, Vec::<String>::new());
        assert_eq!(gathered.player_rank, 1);
        let missing = fixture.repository.gather(-99, GUILD, NOW).unwrap();
        assert_eq!(missing.tunnel, None);
        assert_eq!(missing.player_rank, 0);
    }
}
