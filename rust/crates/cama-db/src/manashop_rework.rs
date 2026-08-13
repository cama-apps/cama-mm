//! Existing-schema persistence for mana-shop purchases and timed buffs.
//!
//! Python remains the production runtime and the only schema-migration
//! authority. This adapter only opens an already-migrated SQLite database via
//! [`crate::open_runtime_connection`]. Multi-row state changes use
//! `BEGIN IMMEDIATE`, matching Python's claim/debit/refund serialization.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use rusqlite::{
    Connection, OptionalExtension, TransactionBehavior, params, params_from_iter, types::Value,
};
use thiserror::Error;

use crate::open_runtime_connection;

pub const PENDING_PURCHASE_STALE_SECONDS: i64 = 60;

#[derive(Debug, Error)]
pub enum ManashopRepositoryError {
    #[error("Manashop cost cannot be negative")]
    NegativeCost,
    #[error("manashop state changed during atomic operation: {0}")]
    StateChanged(&'static str),
    #[error("manashop SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PurchaseFailureReason {
    NotRegistered,
    PurchaseInProgress,
    AlreadyUsed,
    ManaTapped,
    InsufficientBalance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PurchaseResult {
    pub success: bool,
    pub reason: Option<PurchaseFailureReason>,
    pub balance: i64,
    pub purchase_id: Option<String>,
}

impl PurchaseResult {
    fn rejected(reason: PurchaseFailureReason, balance: i64) -> Self {
        Self {
            success: false,
            reason: Some(reason),
            balance,
            purchase_id: None,
        }
    }

    fn purchased(purchase_id: &str, balance: i64) -> Self {
        Self {
            success: true,
            reason: None,
            balance,
            purchase_id: Some(purchase_id.to_owned()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PurchaseRequest<'a> {
    pub purchase_id: &'a str,
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub item_id: &'a str,
    pub used_date: &'a str,
    pub cost: i64,
    pub tap_mana: bool,
    pub now: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PurchaseRecord {
    pub purchase_id: String,
    pub discord_id: i64,
    pub guild_id: i64,
    pub item_id: String,
    pub used_date: String,
    pub cost: i64,
    pub tap_mana: bool,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BuffData {
    pub capacity: Option<i64>,
    pub capacity_remaining: Option<i64>,
    pub rate: Option<f64>,
    pub shared: Option<bool>,
    pub rolling_retroactive: Option<bool>,
    pub caster_id: Option<i64>,
    pub ally_id: Option<i64>,
    pub protected_user_ids: Vec<i64>,
    pub charges_remaining: Option<i64>,
    pub skimmed_total: Option<i64>,
    pub cap: Option<i64>,
    pub skim_rate: Option<f64>,
    pub amount_due: Option<i64>,
    pub amount_paid: Option<i64>,
    pub default_penalty: Option<i64>,
    pub default_penalty_games: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BuffRecord {
    pub id: i64,
    pub discord_id: i64,
    pub guild_id: i64,
    pub buff_type: String,
    pub target_id: Option<i64>,
    pub granted_at: i64,
    pub expires_at: i64,
    pub triggered: bool,
    pub data: BuffData,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GrantBuffRequest<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub buff_type: &'a str,
    pub target_id: Option<i64>,
    pub granted_at: i64,
    pub expires_at: i64,
    pub data: Option<&'a BuffData>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BloodPactClaim {
    pub buff_id: i64,
    pub skimmer_id: i64,
    pub amount: i64,
    pub new_total: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DarkBargainStatus {
    Paid,
    Defaulted,
    MissingPlayer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DarkBargainSettlement {
    pub discord_id: i64,
    pub guild_id: i64,
    pub status: DarkBargainStatus,
    pub amount: i64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SlowDripState {
    pub claimed_today: i64,
    pub last_claim_at: i64,
}

#[derive(Clone, Debug)]
pub struct ManashopRepository {
    path: PathBuf,
}

impl ManashopRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    #[must_use]
    pub const fn normalize_guild_id(guild_id: Option<i64>) -> i64 {
        match guild_id {
            Some(guild_id) => guild_id,
            None => 0,
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    pub fn claim_mana_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        land: &str,
        assigned_date: &str,
    ) -> Result<bool, ManashopRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT assigned_date FROM player_mana
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        if existing.as_deref() == Some(assigned_date) {
            transaction.commit()?;
            return Ok(false);
        }
        let white_shield = i64::from(land == "Plains") * 25;
        transaction.execute(
            "INSERT INTO player_mana (
                 discord_id, guild_id, current_land, assigned_date,
                 bankrupt_insurance_used, bankrupt_reroll_used, consumed_today,
                 white_shield_remaining, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 0, 0, 0, ?5, CURRENT_TIMESTAMP)
             ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                 current_land = excluded.current_land,
                 assigned_date = excluded.assigned_date,
                 bankrupt_insurance_used = 0,
                 bankrupt_reroll_used = 0,
                 consumed_today = 0,
                 white_shield_remaining = excluded.white_shield_remaining,
                 updated_at = CURRENT_TIMESTAMP",
            params![discord_id, guild_id, land, assigned_date, white_shield],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn is_mana_consumed(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, ManashopRepositoryError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT consumed_today FROM player_mana
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some_and(|value| value != 0))
    }

    pub fn mark_mana_consumed_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, ManashopRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE player_mana SET consumed_today = 1, updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?1 AND guild_id = ?2 AND consumed_today = 0",
            params![discord_id, Self::normalize_guild_id(guild_id)],
        )?;
        transaction.commit()?;
        Ok(changed > 0)
    }

    pub fn was_item_used_today(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_id: &str,
        used_date: &str,
    ) -> Result<bool, ManashopRepositoryError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT 1 FROM manashop_daily_uses
                 WHERE discord_id = ?1 AND guild_id = ?2
                   AND item_id = ?3 AND used_date = ?4",
                params![
                    discord_id,
                    Self::normalize_guild_id(guild_id),
                    item_id,
                    used_date
                ],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn mark_item_used_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_id: &str,
        used_date: &str,
    ) -> Result<bool, ManashopRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "INSERT OR IGNORE INTO manashop_daily_uses
                 (discord_id, guild_id, item_id, used_date)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                discord_id,
                Self::normalize_guild_id(guild_id),
                item_id,
                used_date
            ],
        )?;
        transaction.commit()?;
        Ok(changed > 0)
    }

    pub fn try_purchase_item_atomic(
        &self,
        request: PurchaseRequest<'_>,
    ) -> Result<PurchaseResult, ManashopRepositoryError> {
        if request.cost < 0 {
            return Err(ManashopRepositoryError::NegativeCost);
        }
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(mut balance) = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance, 0) FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![request.discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        else {
            transaction.commit()?;
            return Ok(PurchaseResult::rejected(
                PurchaseFailureReason::NotRegistered,
                0,
            ));
        };

        let existing = transaction
            .query_row(
                "SELECT uses.purchase_id, purchases.status, purchases.cost,
                        purchases.tap_mana, purchases.updated_at
                 FROM manashop_daily_uses AS uses
                 LEFT JOIN manashop_purchases AS purchases
                   ON purchases.purchase_id = uses.purchase_id
                 WHERE uses.discord_id = ?1 AND uses.guild_id = ?2
                   AND uses.item_id = ?3 AND uses.used_date = ?4",
                params![
                    request.discord_id,
                    guild_id,
                    request.item_id,
                    request.used_date
                ],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                    ))
                },
            )
            .optional()?;
        if let Some((purchase_id, status, cost, tap_mana, updated_at)) = existing {
            let stale_pending = purchase_id.is_some()
                && status.as_deref() == Some("pending")
                && updated_at.unwrap_or(0)
                    <= request.now.saturating_sub(PENDING_PURCHASE_STALE_SECONDS);
            if !stale_pending {
                let reason = if status.as_deref() == Some("pending") {
                    PurchaseFailureReason::PurchaseInProgress
                } else {
                    PurchaseFailureReason::AlreadyUsed
                };
                transaction.commit()?;
                return Ok(PurchaseResult::rejected(reason, balance));
            }
            let stale_id = purchase_id.unwrap_or_default();
            let stale_cost = cost.unwrap_or(0);
            if transaction.execute(
                "UPDATE manashop_purchases
                 SET status = 'refunded', updated_at = ?1
                 WHERE purchase_id = ?2 AND status = 'pending'",
                params![request.now, stale_id],
            )? != 1
            {
                return Err(ManashopRepositoryError::StateChanged("stale purchase"));
            }
            transaction.execute(
                "DELETE FROM manashop_daily_uses WHERE purchase_id = ?1",
                params![stale_id],
            )?;
            transaction.execute(
                "UPDATE players
                 SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![stale_cost, request.discord_id, guild_id],
            )?;
            balance = balance.saturating_add(stale_cost);
            if tap_mana.unwrap_or(0) != 0 {
                transaction.execute(
                    "UPDATE player_mana
                     SET consumed_today = 0, updated_at = CURRENT_TIMESTAMP
                     WHERE discord_id = ?1 AND guild_id = ?2
                       AND NOT EXISTS (
                           SELECT 1 FROM manashop_purchases
                           WHERE discord_id = ?1 AND guild_id = ?2
                             AND used_date = ?3 AND tap_mana = 1
                             AND status IN ('applying', 'completed')
                       )",
                    params![request.discord_id, guild_id, request.used_date],
                )?;
            }
        }

        if request.tap_mana {
            let available = transaction
                .query_row(
                    "SELECT consumed_today FROM player_mana
                     WHERE discord_id = ?1 AND guild_id = ?2",
                    params![request.discord_id, guild_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .is_some_and(|consumed| consumed == 0);
            if !available {
                transaction.commit()?;
                return Ok(PurchaseResult::rejected(
                    PurchaseFailureReason::ManaTapped,
                    balance,
                ));
            }
        }
        if balance < request.cost {
            transaction.commit()?;
            return Ok(PurchaseResult::rejected(
                PurchaseFailureReason::InsufficientBalance,
                balance,
            ));
        }

        transaction.execute(
            "INSERT INTO manashop_purchases (
                 purchase_id, discord_id, guild_id, item_id, used_date,
                 cost, tap_mana, status, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?8)",
            params![
                request.purchase_id,
                request.discord_id,
                guild_id,
                request.item_id,
                request.used_date,
                request.cost,
                i64::from(request.tap_mana),
                request.now
            ],
        )?;
        transaction.execute(
            "INSERT INTO manashop_daily_uses
                 (discord_id, guild_id, item_id, used_date, purchase_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                request.discord_id,
                guild_id,
                request.item_id,
                request.used_date,
                request.purchase_id
            ],
        )?;
        if request.tap_mana
            && transaction.execute(
                "UPDATE player_mana
                 SET consumed_today = 1, updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?1 AND guild_id = ?2 AND consumed_today = 0",
                params![request.discord_id, guild_id],
            )? != 1
        {
            return Err(ManashopRepositoryError::StateChanged("mana tap"));
        }
        if transaction.execute(
            "UPDATE players
             SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3
               AND COALESCE(jopacoin_balance, 0) >= ?1",
            params![request.cost, request.discord_id, guild_id],
        )? != 1
        {
            return Err(ManashopRepositoryError::StateChanged("balance debit"));
        }
        transaction.execute(
            "UPDATE players SET lowest_balance_ever = jopacoin_balance
             WHERE discord_id = ?1 AND guild_id = ?2
               AND (lowest_balance_ever IS NULL OR jopacoin_balance < lowest_balance_ever)",
            params![request.discord_id, guild_id],
        )?;
        let balance_after = balance - request.cost;
        transaction.commit()?;
        Ok(PurchaseResult::purchased(
            request.purchase_id,
            balance_after,
        ))
    }

    pub fn get_item_purchase(
        &self,
        purchase_id: &str,
    ) -> Result<Option<PurchaseRecord>, ManashopRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT purchase_id, discord_id, guild_id, item_id, used_date,
                        cost, tap_mana, status, created_at, updated_at
                 FROM manashop_purchases WHERE purchase_id = ?1",
                params![purchase_id],
                map_purchase,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn mark_item_purchase_applying_atomic(
        &self,
        purchase_id: &str,
        now: i64,
    ) -> Result<bool, ManashopRepositoryError> {
        self.update_purchase_status(purchase_id, "pending", "applying", now)
    }

    pub fn complete_item_purchase_atomic(
        &self,
        purchase_id: &str,
        now: i64,
    ) -> Result<bool, ManashopRepositoryError> {
        self.update_purchase_status(purchase_id, "applying", "completed", now)
    }

    fn update_purchase_status(
        &self,
        purchase_id: &str,
        from: &str,
        to: &str,
        now: i64,
    ) -> Result<bool, ManashopRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE manashop_purchases SET status = ?1, updated_at = ?2
             WHERE purchase_id = ?3 AND status = ?4",
            params![to, now, purchase_id, from],
        )?;
        transaction.commit()?;
        Ok(changed == 1)
    }

    pub fn refund_item_purchase_atomic(
        &self,
        purchase_id: &str,
        now: i64,
    ) -> Result<bool, ManashopRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(purchase) = transaction
            .query_row(
                "SELECT purchase_id, discord_id, guild_id, item_id, used_date,
                        cost, tap_mana, status, created_at, updated_at
                 FROM manashop_purchases WHERE purchase_id = ?1",
                params![purchase_id],
                map_purchase,
            )
            .optional()?
        else {
            transaction.commit()?;
            return Ok(false);
        };
        if purchase.status != "pending" && purchase.status != "applying" {
            transaction.commit()?;
            return Ok(false);
        }
        if transaction.execute(
            "UPDATE manashop_purchases SET status = 'refunded', updated_at = ?1
             WHERE purchase_id = ?2 AND status IN ('pending', 'applying')",
            params![now, purchase_id],
        )? != 1
        {
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute(
            "DELETE FROM manashop_daily_uses WHERE purchase_id = ?1",
            params![purchase_id],
        )?;
        if transaction.execute(
            "UPDATE players
             SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![purchase.cost, purchase.discord_id, purchase.guild_id],
        )? != 1
        {
            return Err(ManashopRepositoryError::StateChanged("refund player"));
        }
        if purchase.tap_mana {
            transaction.execute(
                "UPDATE player_mana
                 SET consumed_today = 0, updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?1 AND guild_id = ?2
                   AND NOT EXISTS (
                       SELECT 1 FROM manashop_purchases
                       WHERE discord_id = ?1 AND guild_id = ?2
                         AND used_date = ?3 AND tap_mana = 1
                         AND status IN ('pending', 'applying', 'completed')
                   )",
                params![purchase.discord_id, purchase.guild_id, purchase.used_date],
            )?;
        }
        transaction.commit()?;
        Ok(true)
    }

    pub fn balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<i64>, ManashopRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT COALESCE(jopacoin_balance, 0) FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn grant_buff(
        &self,
        request: GrantBuffRequest<'_>,
    ) -> Result<i64, ManashopRepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO manashop_buffs (
                 discord_id, guild_id, buff_type, target_id, granted_at,
                 expires_at, triggered, data
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
            params![
                request.discord_id,
                Self::normalize_guild_id(request.guild_id),
                request.buff_type,
                request.target_id,
                request.granted_at,
                request.expires_at,
                request.data.map(buff_data_to_json)
            ],
        )?;
        Ok(connection.last_insert_rowid())
    }

    pub fn refresh_buff_atomic(
        &self,
        request: GrantBuffRequest<'_>,
    ) -> Result<i64, ManashopRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE manashop_buffs SET triggered = 1
             WHERE discord_id = ?1 AND guild_id = ?2 AND buff_type = ?3
               AND triggered = 0 AND expires_at > ?4",
            params![
                request.discord_id,
                guild_id,
                request.buff_type,
                request.granted_at
            ],
        )?;
        transaction.execute(
            "INSERT INTO manashop_buffs (
                 discord_id, guild_id, buff_type, target_id, granted_at,
                 expires_at, triggered, data
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
            params![
                request.discord_id,
                guild_id,
                request.buff_type,
                request.target_id,
                request.granted_at,
                request.expires_at,
                request.data.map(buff_data_to_json)
            ],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(id)
    }

    pub fn active_for(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        buff_type: &str,
        now: i64,
    ) -> Result<Vec<BuffRecord>, ManashopRepositoryError> {
        let connection = self.connection()?;
        query_buffs(
            &connection,
            "SELECT id, discord_id, guild_id, buff_type, target_id,
                    granted_at, expires_at, triggered, data
             FROM manashop_buffs
             WHERE discord_id = ?1 AND guild_id = ?2 AND buff_type = ?3
               AND triggered = 0 AND expires_at > ?4
             ORDER BY granted_at DESC, id DESC",
            params![
                discord_id,
                Self::normalize_guild_id(guild_id),
                buff_type,
                now
            ],
        )
    }

    pub fn buff_by_id(&self, buff_id: i64) -> Result<Option<BuffRecord>, ManashopRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, discord_id, guild_id, buff_type, target_id,
                        granted_at, expires_at, triggered, data
                 FROM manashop_buffs WHERE id = ?1",
                params![buff_id],
                map_buff,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn active_for_many(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
        buff_type: &str,
        now: i64,
    ) -> Result<Vec<(i64, Vec<BuffRecord>)>, ManashopRepositoryError> {
        let mut seen = BTreeSet::new();
        let unique = discord_ids
            .iter()
            .copied()
            .filter(|id| seen.insert(*id))
            .collect::<Vec<_>>();
        if unique.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", unique.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id, discord_id, guild_id, buff_type, target_id,
                    granted_at, expires_at, triggered, data
             FROM manashop_buffs
             WHERE discord_id IN ({placeholders})
               AND guild_id = ? AND buff_type = ?
               AND triggered = 0 AND expires_at > ?
             ORDER BY discord_id, granted_at DESC, id DESC"
        );
        let mut bindings = unique
            .iter()
            .copied()
            .map(Value::Integer)
            .collect::<Vec<_>>();
        bindings.extend([
            Value::Integer(Self::normalize_guild_id(guild_id)),
            Value::Text(buff_type.to_owned()),
            Value::Integer(now),
        ]);
        let connection = self.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(bindings), map_buff)?;
        let mut grouped: HashMap<i64, Vec<BuffRecord>> = HashMap::new();
        for row in rows {
            let record = row?;
            grouped.entry(record.discord_id).or_default().push(record);
        }
        Ok(unique
            .into_iter()
            .map(|id| (id, grouped.remove(&id).unwrap_or_default()))
            .collect())
    }

    pub fn active_targeted_at(
        &self,
        target_id: i64,
        guild_id: Option<i64>,
        buff_type: &str,
        now: i64,
    ) -> Result<Vec<BuffRecord>, ManashopRepositoryError> {
        let connection = self.connection()?;
        query_buffs(
            &connection,
            "SELECT id, discord_id, guild_id, buff_type, target_id,
                    granted_at, expires_at, triggered, data
             FROM manashop_buffs
             WHERE target_id = ?1 AND guild_id = ?2 AND buff_type = ?3
               AND triggered = 0 AND expires_at > ?4
             ORDER BY granted_at DESC, id DESC",
            params![
                target_id,
                Self::normalize_guild_id(guild_id),
                buff_type,
                now
            ],
        )
    }

    pub fn active_target_ids(
        &self,
        target_ids: &[i64],
        guild_id: Option<i64>,
        buff_type: &str,
        now: i64,
    ) -> Result<BTreeSet<i64>, ManashopRepositoryError> {
        let unique = target_ids.iter().copied().collect::<BTreeSet<_>>();
        if unique.is_empty() {
            return Ok(BTreeSet::new());
        }
        let placeholders = std::iter::repeat_n("?", unique.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT DISTINCT target_id FROM manashop_buffs
             WHERE target_id IN ({placeholders})
               AND guild_id = ? AND buff_type = ?
               AND triggered = 0 AND expires_at > ?"
        );
        let mut bindings = unique
            .iter()
            .copied()
            .map(Value::Integer)
            .collect::<Vec<_>>();
        bindings.extend([
            Value::Integer(Self::normalize_guild_id(guild_id)),
            Value::Text(buff_type.to_owned()),
            Value::Integer(now),
        ]);
        let connection = self.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(bindings), |row| row.get(0))?;
        rows.collect::<Result<BTreeSet<i64>, _>>()
            .map_err(Into::into)
    }

    pub fn consume_buff_atomic(&self, buff_id: i64) -> Result<bool, ManashopRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE manashop_buffs SET triggered = 1
             WHERE id = ?1 AND triggered = 0",
            params![buff_id],
        )?;
        transaction.commit()?;
        Ok(changed > 0)
    }

    pub fn update_buff_data(
        &self,
        buff_id: i64,
        data: &BuffData,
    ) -> Result<(), ManashopRepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE manashop_buffs SET data = ?1 WHERE id = ?2",
            params![buff_data_to_json(data), buff_id],
        )?;
        Ok(())
    }

    pub fn claim_blood_pact_skim_atomic(
        &self,
        target_id: i64,
        guild_id: Option<i64>,
        earning: i64,
        now: i64,
    ) -> Result<Option<BloodPactClaim>, ManashopRepositoryError> {
        if earning <= 0 {
            return Ok(None);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = transaction
            .query_row(
                "SELECT id, discord_id, data FROM manashop_buffs
                 WHERE target_id = ?1 AND guild_id = ?2 AND buff_type = 'blood_pact'
                   AND triggered = 0 AND expires_at > ?3
                 ORDER BY granted_at DESC, id DESC LIMIT 1",
                params![target_id, Self::normalize_guild_id(guild_id), now],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((buff_id, skimmer_id, raw_data)) = row else {
            transaction.commit()?;
            return Ok(None);
        };
        let mut data = buff_data_from_json(raw_data.as_deref().unwrap_or("{}"));
        let cap = data.cap.unwrap_or(0);
        let current = data.skimmed_total.unwrap_or(0);
        let remaining = cap - current;
        if remaining <= 0 {
            transaction.commit()?;
            return Ok(None);
        }
        let rate = data.skim_rate.unwrap_or(0.0);
        let amount = remaining.min(((earning as f64 * rate) as i64).max(1));
        if amount <= 0 {
            transaction.commit()?;
            return Ok(None);
        }
        let new_total = current + amount;
        data.skimmed_total = Some(new_total);
        transaction.execute(
            "UPDATE manashop_buffs SET data = ?1 WHERE id = ?2",
            params![buff_data_to_json(&data), buff_id],
        )?;
        transaction.commit()?;
        Ok(Some(BloodPactClaim {
            buff_id,
            skimmer_id,
            amount,
            new_total,
        }))
    }

    pub fn revert_blood_pact_skim(
        &self,
        buff_id: i64,
        amount: i64,
    ) -> Result<(), ManashopRepositoryError> {
        if amount <= 0 {
            return Ok(());
        }
        self.mutate_buff_data_atomic(buff_id, |data| {
            data.skimmed_total = Some(
                data.skimmed_total
                    .unwrap_or(0)
                    .saturating_sub(amount)
                    .max(0),
            );
            0
        })?;
        Ok(())
    }

    pub fn claim_blood_pact_extra_atomic(
        &self,
        buff_id: i64,
        amount: i64,
    ) -> Result<i64, ManashopRepositoryError> {
        if amount <= 0 {
            return Ok(0);
        }
        self.mutate_buff_data_atomic(buff_id, |data| {
            let current = data.skimmed_total.unwrap_or(0);
            let granted = amount.min(data.cap.unwrap_or(0) - current).max(0);
            data.skimmed_total = Some(current + granted);
            granted
        })
    }

    fn mutate_buff_data_atomic<F>(
        &self,
        buff_id: i64,
        mutation: F,
    ) -> Result<i64, ManashopRepositoryError>
    where
        F: FnOnce(&mut BuffData) -> i64,
    {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = transaction
            .query_row(
                "SELECT data FROM manashop_buffs WHERE id = ?1",
                params![buff_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            transaction.commit()?;
            return Ok(0);
        };
        let mut data = buff_data_from_json(raw.as_deref().unwrap_or("{}"));
        let result = mutation(&mut data);
        transaction.execute(
            "UPDATE manashop_buffs SET data = ?1 WHERE id = ?2",
            params![buff_data_to_json(&data), buff_id],
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn consume_data_charge_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        buff_type: &str,
        now: i64,
    ) -> Result<bool, ManashopRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = transaction
            .query_row(
                "SELECT id, data FROM manashop_buffs
                 WHERE discord_id = ?1 AND guild_id = ?2 AND buff_type = ?3
                   AND triggered = 0 AND expires_at > ?4
                 ORDER BY granted_at DESC, id DESC LIMIT 1",
                params![
                    discord_id,
                    Self::normalize_guild_id(guild_id),
                    buff_type,
                    now
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
        let Some((buff_id, raw)) = row else {
            transaction.commit()?;
            return Ok(false);
        };
        let mut data = buff_data_from_json(raw.as_deref().unwrap_or("{}"));
        let charges = data.charges_remaining.unwrap_or(0);
        if charges <= 0 {
            transaction.execute(
                "UPDATE manashop_buffs SET triggered = 1 WHERE id = ?1",
                params![buff_id],
            )?;
            transaction.commit()?;
            return Ok(false);
        }
        data.charges_remaining = Some(charges - 1);
        transaction.execute(
            "UPDATE manashop_buffs SET data = ?1, triggered = ?2 WHERE id = ?3",
            params![buff_data_to_json(&data), i64::from(charges == 1), buff_id],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn settle_due_dark_bargains(
        &self,
        now: i64,
    ) -> Result<Vec<DarkBargainSettlement>, ManashopRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let debts = {
            let mut statement = transaction.prepare(
                "SELECT id, discord_id, guild_id, data FROM manashop_buffs
                 WHERE buff_type = 'dark_bargain' AND triggered = 0 AND expires_at <= ?1
                 ORDER BY expires_at ASC, id ASC",
            )?;
            statement
                .query_map(params![now], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut settlements = Vec::new();
        for (buff_id, discord_id, guild_id, raw) in debts {
            let data = buff_data_from_json(raw.as_deref().unwrap_or("{}"));
            let amount_due = data.amount_due.unwrap_or(0);
            let default_penalty = data.default_penalty.unwrap_or(1_600);
            let penalty_games = data.default_penalty_games.unwrap_or(5);
            let balance = transaction
                .query_row(
                    "SELECT COALESCE(jopacoin_balance, 0) FROM players
                     WHERE discord_id = ?1 AND guild_id = ?2",
                    params![discord_id, guild_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            let Some(balance) = balance else {
                settlements.push(DarkBargainSettlement {
                    discord_id,
                    guild_id,
                    status: DarkBargainStatus::MissingPlayer,
                    amount: 0,
                });
                continue;
            };
            let (status, amount) = if balance >= amount_due {
                (DarkBargainStatus::Paid, amount_due)
            } else {
                (DarkBargainStatus::Defaulted, default_penalty)
            };
            set_ledger_context(&transaction, buff_id, status)?;
            let update = transaction.execute(
                "UPDATE players
                 SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?1,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![amount, discord_id, guild_id],
            );
            clear_ledger_context(&transaction)?;
            update?;
            transaction.execute(
                "UPDATE players SET lowest_balance_ever = jopacoin_balance
                 WHERE discord_id = ?1 AND guild_id = ?2
                   AND (lowest_balance_ever IS NULL OR jopacoin_balance < lowest_balance_ever)",
                params![discord_id, guild_id],
            )?;
            if status == DarkBargainStatus::Defaulted {
                transaction.execute(
                    "INSERT INTO bankruptcy_state (
                         discord_id, guild_id, last_bankruptcy_at,
                         penalty_games_remaining, bankruptcy_count, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, 1, CURRENT_TIMESTAMP)
                     ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                         last_bankruptcy_at = excluded.last_bankruptcy_at,
                         penalty_games_remaining = bankruptcy_state.penalty_games_remaining
                             + excluded.penalty_games_remaining,
                         bankruptcy_count = COALESCE(bankruptcy_state.bankruptcy_count, 0) + 1,
                         updated_at = CURRENT_TIMESTAMP",
                    params![discord_id, guild_id, now, penalty_games],
                )?;
            }
            transaction.execute(
                "UPDATE manashop_buffs SET triggered = 1 WHERE id = ?1",
                params![buff_id],
            )?;
            settlements.push(DarkBargainSettlement {
                discord_id,
                guild_id,
                status,
                amount,
            });
        }
        transaction.commit()?;
        Ok(settlements)
    }

    pub fn bankruptcy_penalty_games(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, ManashopRepositoryError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT penalty_games_remaining FROM bankruptcy_state
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    pub fn cleanup_expired_buffs(&self, before: i64) -> Result<usize, ManashopRepositoryError> {
        let connection = self.connection()?;
        connection
            .execute(
                "DELETE FROM manashop_buffs
                 WHERE triggered = 1 OR (expires_at <= ?1 AND buff_type != 'dark_bargain')",
                params![before],
            )
            .map_err(Into::into)
    }

    /// Explicit data compatibility repair matching Python's historical
    /// backfill. It never creates or alters schema and is never run implicitly.
    pub fn backfill_overgrowth_charges(&self, now: i64) -> Result<usize, ManashopRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows = {
            let mut statement = transaction.prepare(
                "SELECT id, data FROM manashop_buffs
                 WHERE buff_type = 'overgrowth' AND triggered = 0 AND expires_at > ?1",
            )?;
            statement
                .query_map(params![now], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut changed = 0;
        for (id, raw) in rows {
            let mut data = buff_data_from_json(raw.as_deref().unwrap_or("{}"));
            if data.charges_remaining.is_some() {
                continue;
            }
            data.charges_remaining = Some(10);
            transaction.execute(
                "UPDATE manashop_buffs SET data = ?1 WHERE id = ?2",
                params![buff_data_to_json(&data), id],
            )?;
            changed += 1;
        }
        transaction.commit()?;
        Ok(changed)
    }

    pub fn slow_drip_today(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        claim_date: &str,
    ) -> Result<SlowDripState, ManashopRepositoryError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT claimed_today, last_claim_at FROM slow_drip_claims
                 WHERE discord_id = ?1 AND guild_id = ?2 AND claim_date = ?3",
                params![discord_id, Self::normalize_guild_id(guild_id), claim_date],
                |row| {
                    Ok(SlowDripState {
                        claimed_today: row.get(0)?,
                        last_claim_at: row.get(1)?,
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }

    pub fn add_slow_drip_claim(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        claim_date: &str,
        amount: i64,
        now: i64,
    ) -> Result<(), ManashopRepositoryError> {
        if amount <= 0 {
            return Ok(());
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO slow_drip_claims (
                 discord_id, guild_id, claim_date, claimed_today, last_claim_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(discord_id, guild_id, claim_date) DO UPDATE SET
                 claimed_today = claimed_today + excluded.claimed_today,
                 last_claim_at = excluded.last_claim_at",
            params![
                discord_id,
                Self::normalize_guild_id(guild_id),
                claim_date,
                amount,
                now
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Start or refresh today's Slow Drip anchor without consuming any of the
    /// daily gross cap. Python uses this when there is no usable prior anchor
    /// and when the cap is already exhausted, so a later Dig cannot backfill
    /// time that was deliberately observed as ineligible.
    pub fn stamp_slow_drip_seen(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        claim_date: &str,
        now: i64,
    ) -> Result<(), ManashopRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO slow_drip_claims (
                 discord_id, guild_id, claim_date, claimed_today, last_claim_at
             ) VALUES (?1, ?2, ?3, 0, ?4)
             ON CONFLICT(discord_id, guild_id, claim_date) DO UPDATE SET
                 last_claim_at = excluded.last_claim_at",
            params![
                discord_id,
                Self::normalize_guild_id(guild_id),
                claim_date,
                now
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn transfer_balance_atomic(
        &self,
        from_id: i64,
        to_id: i64,
        guild_id: Option<i64>,
        amount: i64,
    ) -> Result<(), ManashopRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if transaction.execute(
            "UPDATE players SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?1
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![amount, from_id, guild_id],
        )? != 1
        {
            return Err(ManashopRepositoryError::StateChanged("transfer source"));
        }
        if transaction.execute(
            "UPDATE players SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![amount, to_id, guild_id],
        )? != 1
        {
            return Err(ManashopRepositoryError::StateChanged("transfer recipient"));
        }
        transaction.commit()?;
        Ok(())
    }
}

fn map_purchase(row: &rusqlite::Row<'_>) -> Result<PurchaseRecord, rusqlite::Error> {
    Ok(PurchaseRecord {
        purchase_id: row.get(0)?,
        discord_id: row.get(1)?,
        guild_id: row.get(2)?,
        item_id: row.get(3)?,
        used_date: row.get(4)?,
        cost: row.get(5)?,
        tap_mana: row.get::<_, i64>(6)? != 0,
        status: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn map_buff(row: &rusqlite::Row<'_>) -> Result<BuffRecord, rusqlite::Error> {
    let raw = row.get::<_, Option<String>>(8)?;
    Ok(BuffRecord {
        id: row.get(0)?,
        discord_id: row.get(1)?,
        guild_id: row.get(2)?,
        buff_type: row.get(3)?,
        target_id: row.get(4)?,
        granted_at: row.get(5)?,
        expires_at: row.get(6)?,
        triggered: row.get::<_, i64>(7)? != 0,
        data: buff_data_from_json(raw.as_deref().unwrap_or("{}")),
    })
}

fn query_buffs<P>(
    connection: &Connection,
    sql: &str,
    parameters: P,
) -> Result<Vec<BuffRecord>, ManashopRepositoryError>
where
    P: rusqlite::Params,
{
    let mut statement = connection.prepare(sql)?;
    statement
        .query_map(parameters, map_buff)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn set_ledger_context(
    connection: &Connection,
    buff_id: i64,
    status: DarkBargainStatus,
) -> Result<(), rusqlite::Error> {
    connection.execute("DELETE FROM economy_ledger_context", [])?;
    connection.execute(
        "INSERT INTO economy_ledger_context (
             id, source, related_type, related_id, reason, metadata
         ) VALUES (1, 'manashop_buff', 'dark_bargain', ?1, ?2, ?3)",
        params![
            buff_id.to_string(),
            match status {
                DarkBargainStatus::Paid => "dark bargain paid",
                DarkBargainStatus::Defaulted => "dark bargain defaulted",
                DarkBargainStatus::MissingPlayer => "dark bargain missing player",
            },
            format!("{{\"buff_id\":{buff_id}}}")
        ],
    )?;
    Ok(())
}

fn clear_ledger_context(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn buff_data_to_json(data: &BuffData) -> String {
    let mut fields = Vec::new();
    macro_rules! number {
        ($name:literal, $value:expr) => {
            if let Some(value) = $value {
                fields.push(format!(concat!("\"", $name, "\":{}"), value));
            }
        };
    }
    macro_rules! boolean {
        ($name:literal, $value:expr) => {
            if let Some(value) = $value {
                fields.push(format!(
                    concat!("\"", $name, "\":{}"),
                    if value { "true" } else { "false" }
                ));
            }
        };
    }
    number!("capacity", data.capacity);
    number!("capacity_remaining", data.capacity_remaining);
    number!("rate", data.rate);
    boolean!("shared", data.shared);
    boolean!("rolling_retroactive", data.rolling_retroactive);
    number!("caster_id", data.caster_id);
    number!("ally_id", data.ally_id);
    if !data.protected_user_ids.is_empty() {
        fields.push(format!(
            "\"protected_user_ids\":[{}]",
            data.protected_user_ids
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    number!("charges_remaining", data.charges_remaining);
    number!("skimmed_total", data.skimmed_total);
    number!("cap", data.cap);
    number!("skim_rate", data.skim_rate);
    number!("amount_due", data.amount_due);
    number!("amount_paid", data.amount_paid);
    number!("default_penalty", data.default_penalty);
    number!("default_penalty_games", data.default_penalty_games);
    format!("{{{}}}", fields.join(","))
}

fn buff_data_from_json(raw: &str) -> BuffData {
    BuffData {
        capacity: json_i64(raw, "capacity"),
        capacity_remaining: json_i64(raw, "capacity_remaining"),
        rate: json_f64(raw, "rate"),
        shared: json_bool(raw, "shared"),
        rolling_retroactive: json_bool(raw, "rolling_retroactive"),
        caster_id: json_i64(raw, "caster_id"),
        ally_id: json_i64(raw, "ally_id"),
        protected_user_ids: json_i64_array(raw, "protected_user_ids"),
        charges_remaining: json_i64(raw, "charges_remaining"),
        skimmed_total: json_i64(raw, "skimmed_total"),
        cap: json_i64(raw, "cap"),
        skim_rate: json_f64(raw, "skim_rate"),
        amount_due: json_i64(raw, "amount_due"),
        amount_paid: json_i64(raw, "amount_paid"),
        default_penalty: json_i64(raw, "default_penalty"),
        default_penalty_games: json_i64(raw, "default_penalty_games"),
    }
}

fn json_value<'a>(raw: &'a str, key: &str) -> Option<&'a str> {
    let marker = format!("\"{key}\"");
    let start = raw.find(&marker)? + marker.len();
    let remainder = raw[start..].trim_start();
    let remainder = remainder.strip_prefix(':')?.trim_start();
    if remainder.starts_with('[') {
        let end = remainder.find(']')?;
        return Some(&remainder[..=end]);
    }
    let end = remainder.find([',', '}']).unwrap_or(remainder.len());
    Some(remainder[..end].trim())
}

fn json_i64(raw: &str, key: &str) -> Option<i64> {
    json_value(raw, key)?.parse().ok()
}

fn json_f64(raw: &str, key: &str) -> Option<f64> {
    json_value(raw, key)?.parse().ok()
}

fn json_bool(raw: &str, key: &str) -> Option<bool> {
    match json_value(raw, key)? {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

fn json_i64_array(raw: &str, key: &str) -> Vec<i64> {
    json_value(raw, key)
        .and_then(|value| value.strip_prefix('[')?.strip_suffix(']'))
        .map(|body| {
            body.split(',')
                .filter_map(|value| value.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "manashop_rework/tests.rs"]
mod tests;
