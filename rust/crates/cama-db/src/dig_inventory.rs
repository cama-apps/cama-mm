//! Existing-schema persistence for dig consumables and tunnel defenses.
//!
//! Python remains the production runtime and the only schema-migration
//! authority. This adapter opens an already-migrated database through
//! [`crate::open_runtime_connection`]; it never creates, alters, or migrates
//! production schema. Money-moving and queue-changing operations acquire a
//! `BEGIN IMMEDIATE` transaction so balance, inventory, tunnel state, and the
//! Python-owned economy-ledger triggers commit or roll back together.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

const BOSS_PREPARATION_ITEM_TYPES: [&str; 3] =
    ["tempered_whetstone", "warding_salts", "rescue_line"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryItemRecord {
    pub id: i64,
    pub discord_id: i64,
    pub guild_id: i64,
    pub item_type: String,
    pub queued: bool,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TunnelDefenseRecord {
    pub discord_id: i64,
    pub guild_id: i64,
    pub depth: i64,
    pub trap_active: bool,
    pub trap_free_today: i64,
    pub trap_date: Option<String>,
    pub insured_until: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuyItemRequest<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub item_type: &'a str,
    pub price: i64,
    pub inventory_limit: usize,
    pub auto_queue_if_available: bool,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuyItemOutcome {
    Purchased {
        item_id: i64,
        queued: bool,
        cost: i64,
        balance_after: i64,
    },
    MissingTunnel,
    InventoryFull {
        limit: usize,
    },
    InsufficientBalance {
        balance: i64,
        cost: i64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueueItemOutcome {
    Queued { item_id: i64, item_type: String },
    MissingTunnel,
    NotOwned,
    AlreadyQueued { item_type: String },
    BossPreparationAlreadyQueued,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoBuySelection<'a> {
    pub item_type: &'a str,
    pub price: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoBuyRequest<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub selections: &'a [AutoBuySelection<'a>],
    pub reserved_balance: i64,
    pub inventory_limit: usize,
    pub created_at: i64,
    /// Optional caller snapshot used to reproduce Python's stale-read seam.
    /// The transaction still checks every debit against the live balance.
    pub observed_balance: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoBuyStatus {
    AlreadyQueued,
    QueuedFromInventory,
    Purchased,
    SkippedInventoryFull,
    SkippedInsufficientBalance,
    SkippedMissingTunnel,
}

impl AutoBuyStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyQueued => "already_queued",
            Self::QueuedFromInventory => "queued_from_inventory",
            Self::Purchased => "purchased",
            Self::SkippedInventoryFull => "skipped_inventory_full",
            Self::SkippedInsufficientBalance => "skipped_insufficient_balance",
            Self::SkippedMissingTunnel => "skipped_error",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoBuyItemOutcome {
    pub item_type: String,
    pub status: AutoBuyStatus,
    pub cost: i64,
    pub item_id: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetTrapOutcome {
    Set { cost: i64, balance_after: i64 },
    MissingTunnel,
    AlreadyActive,
    InsufficientBalance { balance: i64, cost: i64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuyInsuranceOutcome {
    Purchased {
        cost: i64,
        expires_at: i64,
        balance_after: i64,
    },
    MissingTunnel,
    InsufficientBalance {
        balance: i64,
        cost: i64,
    },
}

#[derive(Debug, Error)]
pub enum DigInventoryRepositoryError {
    #[error("dig inventory cost cannot be negative: {0}")]
    NegativeCost(i64),
    #[error("dig inventory limit exceeds SQLite's signed integer range")]
    InventoryLimitOverflow,
    #[error("dig inventory arithmetic exceeded SQLite's signed 64-bit range")]
    AmountOverflow,
    #[error("dig inventory SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Python-compatible adapter over `players`, `tunnels`, and `dig_inventory`.
#[derive(Clone, Debug)]
pub struct DigInventoryRepository {
    path: PathBuf,
}

impl DigInventoryRepository {
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

    pub fn balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, DigInventoryRepositoryError> {
        let connection = self.connection()?;
        query_balance(&connection, discord_id, Self::normalize_guild_id(guild_id))
            .map_err(Into::into)
    }

    pub fn tunnel(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<TunnelDefenseRecord>, DigInventoryRepositoryError> {
        let connection = self.connection()?;
        query_tunnel(&connection, discord_id, Self::normalize_guild_id(guild_id))
            .map_err(Into::into)
    }

    pub fn inventory(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<InventoryItemRecord>, DigInventoryRepositoryError> {
        let connection = self.connection()?;
        query_inventory(&connection, discord_id, Self::normalize_guild_id(guild_id))
            .map_err(Into::into)
    }

    pub fn queued_items(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<InventoryItemRecord>, DigInventoryRepositoryError> {
        let connection = self.connection()?;
        query_queued_items(&connection, discord_id, Self::normalize_guild_id(guild_id))
            .map_err(Into::into)
    }

    /// Return `None` when the player has no tunnel; otherwise return the rows
    /// used by the inventory presentation layer from one connection snapshot.
    pub fn inventory_for_tunnel(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Vec<InventoryItemRecord>>, DigInventoryRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        if !tunnel_exists(&connection, discord_id, guild_id)? {
            return Ok(None);
        }
        Ok(Some(query_inventory(&connection, discord_id, guild_id)?))
    }

    /// Insert an existing-schema inventory row. This is also used by reward
    /// settlement adapters; it is not schema initialization.
    pub fn add_item(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_type: &str,
        created_at: i64,
    ) -> Result<i64, DigInventoryRepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO dig_inventory
                 (discord_id, guild_id, item_type, queued, created_at)
             VALUES (?1, ?2, ?3, 0, ?4)",
            params![
                discord_id,
                Self::normalize_guild_id(guild_id),
                item_type,
                created_at
            ],
        )?;
        Ok(connection.last_insert_rowid())
    }

    pub fn buy_item_atomic(
        &self,
        request: BuyItemRequest<'_>,
    ) -> Result<BuyItemOutcome, DigInventoryRepositoryError> {
        validate_cost(request.price)?;
        let inventory_limit = sqlite_inventory_limit(request.inventory_limit)?;
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if !tunnel_exists(&transaction, request.discord_id, guild_id)? {
            transaction.commit()?;
            return Ok(BuyItemOutcome::MissingTunnel);
        }

        let inventory_size = inventory_count(&transaction, request.discord_id, guild_id)?;
        if inventory_size >= inventory_limit {
            transaction.commit()?;
            return Ok(BuyItemOutcome::InventoryFull {
                limit: request.inventory_limit,
            });
        }

        let queued = request.auto_queue_if_available
            && !queued_type_exists(
                &transaction,
                request.discord_id,
                guild_id,
                request.item_type,
            )?;
        let balance = query_balance(&transaction, request.discord_id, guild_id)?;
        if balance < request.price {
            transaction.commit()?;
            return Ok(BuyItemOutcome::InsufficientBalance {
                balance,
                cost: request.price,
            });
        }

        if !debit_player(&transaction, request.discord_id, guild_id, request.price, 0)? {
            let live_balance = query_balance(&transaction, request.discord_id, guild_id)?;
            transaction.commit()?;
            return Ok(BuyItemOutcome::InsufficientBalance {
                balance: live_balance,
                cost: request.price,
            });
        }

        transaction.execute(
            "INSERT INTO dig_inventory
                 (discord_id, guild_id, item_type, queued, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                request.discord_id,
                guild_id,
                request.item_type,
                i64::from(queued),
                request.created_at
            ],
        )?;
        let item_id = transaction.last_insert_rowid();
        let balance_after = balance
            .checked_sub(request.price)
            .ok_or(DigInventoryRepositoryError::AmountOverflow)?;
        transaction.commit()?;
        Ok(BuyItemOutcome::Purchased {
            item_id,
            queued,
            cost: request.price,
            balance_after,
        })
    }

    /// Queue reserve copies and conditionally purchase the missing selections
    /// in one `BEGIN IMMEDIATE`. The optional observed balance deliberately
    /// preserves Python's stale-snapshot behavior while every actual debit is
    /// checked against the live balance and the paid-dig reserve.
    pub fn ensure_auto_buy_items_atomic(
        &self,
        request: AutoBuyRequest<'_>,
    ) -> Result<Vec<AutoBuyItemOutcome>, DigInventoryRepositoryError> {
        if request.selections.is_empty() {
            return Ok(Vec::new());
        }
        for selection in request.selections {
            validate_cost(selection.price)?;
        }
        let inventory_limit = sqlite_inventory_limit(request.inventory_limit)?;
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let reserved_balance = request.reserved_balance.max(0);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        // Each state family is read once. Subsequent decisions update these
        // local snapshots sequentially, matching Python's orchestration.
        let mut inventory = query_inventory(&transaction, request.discord_id, guild_id)?;
        let mut queued_types = inventory
            .iter()
            .filter(|item| item.queued)
            .map(|item| item.item_type.clone())
            .collect::<HashSet<_>>();
        let live_balance = query_balance(&transaction, request.discord_id, guild_id)?;
        let mut planning_balance = request
            .observed_balance
            .unwrap_or(live_balance)
            .saturating_sub(reserved_balance)
            .max(0);
        let mut tunnel_snapshot: Option<bool> = None;
        let mut results = Vec::with_capacity(request.selections.len());
        let mut planned_purchases = Vec::new();

        for selection in request.selections {
            if queued_types.contains(selection.item_type) {
                results.push(AutoBuyItemOutcome {
                    item_type: selection.item_type.to_owned(),
                    status: AutoBuyStatus::AlreadyQueued,
                    cost: 0,
                    item_id: None,
                });
                continue;
            }

            if let Some(reserve) = inventory
                .iter_mut()
                .find(|item| item.item_type == selection.item_type && !item.queued)
            {
                transaction.execute(
                    "UPDATE dig_inventory SET queued = 1
                     WHERE id = ?1 AND discord_id = ?2 AND guild_id = ?3 AND queued = 0",
                    params![reserve.id, request.discord_id, guild_id],
                )?;
                reserve.queued = true;
                queued_types.insert(selection.item_type.to_owned());
                results.push(AutoBuyItemOutcome {
                    item_type: selection.item_type.to_owned(),
                    status: AutoBuyStatus::QueuedFromInventory,
                    cost: 0,
                    // Python reports the database id only for a newly
                    // purchased copy, not for a reserve that was queued.
                    item_id: None,
                });
                continue;
            }

            if i64::try_from(inventory.len())
                .map_err(|_| DigInventoryRepositoryError::InventoryLimitOverflow)?
                >= inventory_limit
            {
                results.push(AutoBuyItemOutcome {
                    item_type: selection.item_type.to_owned(),
                    status: AutoBuyStatus::SkippedInventoryFull,
                    cost: 0,
                    item_id: None,
                });
                continue;
            }

            if planning_balance < selection.price {
                results.push(AutoBuyItemOutcome {
                    item_type: selection.item_type.to_owned(),
                    status: AutoBuyStatus::SkippedInsufficientBalance,
                    cost: selection.price,
                    item_id: None,
                });
                continue;
            }

            let has_tunnel = match tunnel_snapshot {
                Some(has_tunnel) => has_tunnel,
                None => {
                    let has_tunnel = tunnel_exists(&transaction, request.discord_id, guild_id)?;
                    tunnel_snapshot = Some(has_tunnel);
                    has_tunnel
                }
            };
            if !has_tunnel {
                results.push(AutoBuyItemOutcome {
                    item_type: selection.item_type.to_owned(),
                    status: AutoBuyStatus::SkippedMissingTunnel,
                    cost: selection.price,
                    item_id: None,
                });
                continue;
            }

            let result_index = results.len();
            results.push(AutoBuyItemOutcome {
                item_type: selection.item_type.to_owned(),
                status: AutoBuyStatus::Purchased,
                cost: selection.price,
                item_id: None,
            });
            planned_purchases.push((result_index, *selection));
            planning_balance = planning_balance
                .checked_sub(selection.price)
                .ok_or(DigInventoryRepositoryError::AmountOverflow)?;
            queued_types.insert(selection.item_type.to_owned());
            inventory.push(InventoryItemRecord {
                id: i64::MIN,
                discord_id: request.discord_id,
                guild_id,
                item_type: selection.item_type.to_owned(),
                queued: true,
                created_at: request.created_at,
            });
        }

        for (result_index, selection) in planned_purchases {
            if !debit_player(
                &transaction,
                request.discord_id,
                guild_id,
                selection.price,
                reserved_balance,
            )? {
                results[result_index].status = AutoBuyStatus::SkippedInsufficientBalance;
                continue;
            }
            transaction.execute(
                "INSERT INTO dig_inventory
                     (discord_id, guild_id, item_type, queued, created_at)
                 VALUES (?1, ?2, ?3, 1, ?4)",
                params![
                    request.discord_id,
                    guild_id,
                    selection.item_type,
                    request.created_at
                ],
            )?;
            results[result_index].item_id = Some(transaction.last_insert_rowid());
        }

        transaction.commit()?;
        Ok(results)
    }

    pub fn queue_item_type_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_type: &str,
    ) -> Result<QueueItemOutcome, DigInventoryRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if !tunnel_exists(&transaction, discord_id, guild_id)? {
            transaction.commit()?;
            return Ok(QueueItemOutcome::MissingTunnel);
        }

        let inventory = query_inventory(&transaction, discord_id, guild_id)?;
        if !inventory.iter().any(|item| item.item_type == item_type) {
            transaction.commit()?;
            return Ok(QueueItemOutcome::NotOwned);
        }
        let queued_types = inventory
            .iter()
            .filter(|item| item.queued)
            .map(|item| item.item_type.as_str())
            .collect::<HashSet<_>>();
        if is_boss_preparation(item_type)
            && queued_types
                .iter()
                .any(|queued| is_boss_preparation(queued))
        {
            transaction.commit()?;
            return Ok(QueueItemOutcome::BossPreparationAlreadyQueued);
        }
        if queued_types.contains(item_type) {
            transaction.commit()?;
            return Ok(QueueItemOutcome::AlreadyQueued {
                item_type: item_type.to_owned(),
            });
        }
        let item_id = inventory
            .iter()
            .find(|item| item.item_type == item_type && !item.queued)
            .map(|item| item.id)
            .ok_or(DigInventoryRepositoryError::AmountOverflow)?;
        transaction.execute(
            "UPDATE dig_inventory SET queued = 1
             WHERE id = ?1 AND discord_id = ?2 AND guild_id = ?3 AND queued = 0",
            params![item_id, discord_id, guild_id],
        )?;
        transaction.commit()?;
        Ok(QueueItemOutcome::Queued {
            item_id,
            item_type: item_type.to_owned(),
        })
    }

    pub fn queue_item_id_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_id: i64,
    ) -> Result<QueueItemOutcome, DigInventoryRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let target = transaction
            .query_row(
                "SELECT id, discord_id, guild_id, item_type, queued, created_at
                 FROM dig_inventory
                 WHERE id = ?1 AND discord_id = ?2 AND guild_id = ?3",
                params![item_id, discord_id, guild_id],
                inventory_item_from_row,
            )
            .optional()?;
        let Some(target) = target else {
            transaction.commit()?;
            return Ok(QueueItemOutcome::NotOwned);
        };
        let queued_types = query_queued_items(&transaction, discord_id, guild_id)?
            .into_iter()
            .map(|item| item.item_type)
            .collect::<HashSet<_>>();
        if is_boss_preparation(&target.item_type)
            && queued_types
                .iter()
                .any(|queued| is_boss_preparation(queued))
        {
            transaction.commit()?;
            return Ok(QueueItemOutcome::BossPreparationAlreadyQueued);
        }
        if queued_types.contains(&target.item_type) {
            transaction.commit()?;
            return Ok(QueueItemOutcome::AlreadyQueued {
                item_type: target.item_type,
            });
        }
        transaction.execute(
            "UPDATE dig_inventory SET queued = 1
             WHERE id = ?1 AND discord_id = ?2 AND guild_id = ?3 AND queued = 0",
            params![item_id, discord_id, guild_id],
        )?;
        transaction.commit()?;
        Ok(QueueItemOutcome::Queued {
            item_id,
            item_type: target.item_type,
        })
    }

    pub fn set_trap_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        game_date: &str,
    ) -> Result<SetTrapOutcome, DigInventoryRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(tunnel) = query_tunnel(&transaction, discord_id, guild_id)? else {
            transaction.commit()?;
            return Ok(SetTrapOutcome::MissingTunnel);
        };
        if tunnel.trap_active {
            transaction.commit()?;
            return Ok(SetTrapOutcome::AlreadyActive);
        }

        let free_traps_used = if tunnel.trap_date.as_deref() == Some(game_date) {
            tunnel.trap_free_today
        } else {
            0
        };
        let cost = if free_traps_used > 0 {
            defense_cost(tunnel.depth)?
        } else {
            0
        };
        let balance = query_balance(&transaction, discord_id, guild_id)?;
        if cost > 0 && balance < cost {
            transaction.commit()?;
            return Ok(SetTrapOutcome::InsufficientBalance { balance, cost });
        }
        if cost > 0 && !debit_player(&transaction, discord_id, guild_id, cost, 0)? {
            let live_balance = query_balance(&transaction, discord_id, guild_id)?;
            transaction.commit()?;
            return Ok(SetTrapOutcome::InsufficientBalance {
                balance: live_balance,
                cost,
            });
        }
        let next_free_traps_used = free_traps_used
            .checked_add(1)
            .ok_or(DigInventoryRepositoryError::AmountOverflow)?;
        transaction.execute(
            "UPDATE tunnels
             SET trap_active = 1, trap_free_today = ?1, trap_date = ?2
             WHERE discord_id = ?3 AND guild_id = ?4 AND trap_active = 0",
            params![next_free_traps_used, game_date, discord_id, guild_id],
        )?;
        let balance_after = balance
            .checked_sub(cost)
            .ok_or(DigInventoryRepositoryError::AmountOverflow)?;
        transaction.commit()?;
        Ok(SetTrapOutcome::Set {
            cost,
            balance_after,
        })
    }

    pub fn buy_insurance_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<BuyInsuranceOutcome, DigInventoryRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(tunnel) = query_tunnel(&transaction, discord_id, guild_id)? else {
            transaction.commit()?;
            return Ok(BuyInsuranceOutcome::MissingTunnel);
        };
        let cost = defense_cost(tunnel.depth)?;
        let expires_at = now
            .checked_add(86_400)
            .ok_or(DigInventoryRepositoryError::AmountOverflow)?;
        let balance = query_balance(&transaction, discord_id, guild_id)?;
        if balance < cost {
            transaction.commit()?;
            return Ok(BuyInsuranceOutcome::InsufficientBalance { balance, cost });
        }
        if !debit_player(&transaction, discord_id, guild_id, cost, 0)? {
            let live_balance = query_balance(&transaction, discord_id, guild_id)?;
            transaction.commit()?;
            return Ok(BuyInsuranceOutcome::InsufficientBalance {
                balance: live_balance,
                cost,
            });
        }
        transaction.execute(
            "UPDATE tunnels SET insured_until = ?1
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![expires_at, discord_id, guild_id],
        )?;
        let balance_after = balance
            .checked_sub(cost)
            .ok_or(DigInventoryRepositoryError::AmountOverflow)?;
        transaction.commit()?;
        Ok(BuyInsuranceOutcome::Purchased {
            cost,
            expires_at,
            balance_after,
        })
    }
}

fn validate_cost(cost: i64) -> Result<(), DigInventoryRepositoryError> {
    if cost < 0 {
        Err(DigInventoryRepositoryError::NegativeCost(cost))
    } else {
        Ok(())
    }
}

fn sqlite_inventory_limit(limit: usize) -> Result<i64, DigInventoryRepositoryError> {
    i64::try_from(limit).map_err(|_| DigInventoryRepositoryError::InventoryLimitOverflow)
}

fn defense_cost(depth: i64) -> Result<i64, DigInventoryRepositoryError> {
    5_i64
        .checked_add(depth.div_euclid(25))
        .ok_or(DigInventoryRepositoryError::AmountOverflow)
}

fn is_boss_preparation(item_type: &str) -> bool {
    BOSS_PREPARATION_ITEM_TYPES.contains(&item_type)
}

fn inventory_item_from_row(
    row: &rusqlite::Row<'_>,
) -> Result<InventoryItemRecord, rusqlite::Error> {
    Ok(InventoryItemRecord {
        id: row.get(0)?,
        discord_id: row.get(1)?,
        guild_id: row.get(2)?,
        item_type: row.get(3)?,
        queued: row.get::<_, i64>(4)? != 0,
        created_at: row.get(5)?,
    })
}

fn query_inventory(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Vec<InventoryItemRecord>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT id, discord_id, guild_id, item_type, queued, created_at
         FROM dig_inventory
         WHERE discord_id = ?1 AND guild_id = ?2
         ORDER BY created_at DESC",
    )?;
    statement
        .query_map(params![discord_id, guild_id], inventory_item_from_row)?
        .collect()
}

fn query_queued_items(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Vec<InventoryItemRecord>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT id, discord_id, guild_id, item_type, queued, created_at
         FROM dig_inventory
         WHERE discord_id = ?1 AND guild_id = ?2 AND queued = 1
         ORDER BY created_at DESC",
    )?;
    statement
        .query_map(params![discord_id, guild_id], inventory_item_from_row)?
        .collect()
}

fn query_tunnel(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<TunnelDefenseRecord>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT discord_id, guild_id, COALESCE(depth, 0),
                    COALESCE(trap_active, 0), COALESCE(trap_free_today, 0),
                    trap_date, insured_until
             FROM tunnels
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |row| {
                Ok(TunnelDefenseRecord {
                    discord_id: row.get(0)?,
                    guild_id: row.get(1)?,
                    depth: row.get(2)?,
                    trap_active: row.get::<_, i64>(3)? != 0,
                    trap_free_today: row.get(4)?,
                    trap_date: row.get(5)?,
                    insured_until: row.get(6)?,
                })
            },
        )
        .optional()
}

fn query_balance(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<i64, rusqlite::Error> {
    connection
        .query_row(
            "SELECT COALESCE(jopacoin_balance, 0)
             FROM players WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()
        .map(Option::unwrap_or_default)
}

fn tunnel_exists(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<bool, rusqlite::Error> {
    connection
        .query_row(
            "SELECT 1 FROM tunnels WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
}

fn queued_type_exists(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
    item_type: &str,
) -> Result<bool, rusqlite::Error> {
    connection
        .query_row(
            "SELECT 1 FROM dig_inventory
             WHERE discord_id = ?1 AND guild_id = ?2
               AND item_type = ?3 AND queued = 1
             LIMIT 1",
            params![discord_id, guild_id, item_type],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
}

fn inventory_count(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<i64, rusqlite::Error> {
    connection.query_row(
        "SELECT COUNT(*) FROM dig_inventory
         WHERE discord_id = ?1 AND guild_id = ?2",
        params![discord_id, guild_id],
        |row| row.get(0),
    )
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    discord_id: i64,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context
             (id, source, actor_id, related_type, related_id, reason, metadata)
         VALUES (1, 'dig', ?1, 'dig_action', NULL,
                 'dig cost or penalty', NULL)",
        params![discord_id],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn debit_player(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
    cost: i64,
    reserved_balance: i64,
) -> Result<bool, DigInventoryRepositoryError> {
    if cost == 0 {
        return Ok(true);
    }
    let required_balance = cost
        .checked_add(reserved_balance)
        .ok_or(DigInventoryRepositoryError::AmountOverflow)?;
    set_ledger_context(transaction, discord_id)?;
    let debit = transaction.execute(
        "UPDATE players
         SET jopacoin_balance = jopacoin_balance - ?1,
             updated_at = CURRENT_TIMESTAMP
         WHERE discord_id = ?2 AND guild_id = ?3
           AND jopacoin_balance >= ?4",
        params![cost, discord_id, guild_id, required_balance],
    );
    let clear = clear_ledger_context(transaction);
    match debit {
        Ok(row_count) => {
            clear?;
            Ok(row_count == 1)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
#[path = "dig_inventory/tests.rs"]
mod tests;
