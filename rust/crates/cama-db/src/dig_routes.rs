//! Existing-schema persistence for persistent Dig routes.
//!
//! Python remains the production runtime and the sole schema/migration
//! authority. This repository only opens an already-migrated SQLite database
//! through [`crate::open_runtime_connection`]. Route choices, boss outcomes,
//! rewards, echoes, and audit rows use `BEGIN IMMEDIATE` so stale Discord
//! views cannot overwrite the first committed result.

use std::path::{Path, PathBuf};

use rusqlite::{
    Connection, OptionalExtension, Transaction, TransactionBehavior, params, params_from_iter,
    types::Value,
};
use thiserror::Error;

use crate::open_runtime_connection;

/// Route-facing tunnel columns read from Python's `tunnels` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteTunnelRecord {
    pub discord_id: i64,
    pub guild_id: i64,
    pub depth: i64,
    pub boss_progress: Option<String>,
    pub stinger_curse: Option<String>,
    pub route_state: Option<String>,
}

/// Nullable-text guard used where `None` must differ from "do not guard".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextGuard<'a> {
    #[default]
    Ignore,
    Null,
    Value(&'a str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteStateWriteOutcome {
    Updated,
    MissingTunnel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteChoiceClaim {
    Claimed,
    Conflict {
        persisted_route_state: Option<String>,
    },
    MissingTunnel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BossLockClaim {
    Locked { boss_progress: String },
    Existing { boss_progress: Option<String> },
    MissingTunnel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BossOutcomeClaim {
    Claimed,
    Conflict { persisted: RouteTunnelRecord },
    MissingTunnel,
    MissingPlayer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteChoiceRequest<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub expected_route_state: &'a str,
    pub selected_route_state: &'a str,
    pub route_id: &'a str,
    pub layer: &'a str,
    pub detail_json: &'a str,
    pub now: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BossLockRequest<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub expected_boss_progress: &'a str,
    pub locked_boss_progress: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BossVictoryRequest<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub expected_depth: i64,
    pub expected_boss_progress: &'a str,
    pub expected_stinger_curse: TextGuard<'a>,
    pub depth_after: i64,
    pub boss_progress_after: &'a str,
    pub route_state_after: &'a str,
    pub clear_stinger_curse: bool,
    pub jc_delta: i64,
    pub vanity_tax: i64,
    pub boss_id: &'a str,
    pub boss_depth: i64,
    pub echo_window_seconds: i64,
    pub detail_json: &'a str,
    pub now: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BossLossRequest<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub expected_depth: i64,
    pub expected_boss_progress: &'a str,
    pub expected_stinger_curse: TextGuard<'a>,
    pub depth_after: i64,
    pub boss_progress_after: &'a str,
    pub stinger_curse_after: Option<&'a str>,
    pub detail_json: &'a str,
    pub now: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LifecycleRouteClearRequest<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub expected_route_state: &'a str,
    pub detail_json: &'a str,
    pub now: i64,
}

#[derive(Debug, Error)]
pub enum DigRouteRepositoryError {
    #[error("boss echo window cannot be negative: {0}")]
    NegativeEchoWindow(i64),
    #[error("dig route arithmetic exceeded SQLite's signed 64-bit range")]
    AmountOverflow,
    #[error("dig route SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Python-compatible adapter over route-related tunnel state.
#[derive(Clone, Debug)]
pub struct DigRouteRepository {
    path: PathBuf,
}

impl DigRouteRepository {
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

    pub fn tunnel(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<RouteTunnelRecord>, DigRouteRepositoryError> {
        let connection = self.connection()?;
        query_tunnel(&connection, discord_id, Self::normalize_guild_id(guild_id))
            .map_err(Into::into)
    }

    /// Update the migrated nullable route column without creating schema.
    pub fn set_route_state(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        route_state: Option<&str>,
    ) -> Result<RouteStateWriteOutcome, DigRouteRepositoryError> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE tunnels SET route_state = ?1
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![route_state, discord_id, Self::normalize_guild_id(guild_id)],
        )?;
        Ok(if changed == 1 {
            RouteStateWriteOutcome::Updated
        } else {
            RouteStateWriteOutcome::MissingTunnel
        })
    }

    /// Claim a pending offer and write its audit row in one transaction.
    pub fn choose_route_atomic(
        &self,
        request: RouteChoiceRequest<'_>,
    ) -> Result<RouteChoiceClaim, DigRouteRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE tunnels SET route_state = ?1
             WHERE discord_id = ?2 AND guild_id = ?3 AND route_state = ?4",
            params![
                request.selected_route_state,
                request.discord_id,
                guild_id,
                request.expected_route_state
            ],
        )?;
        if changed == 0 {
            let persisted = query_route_state(&transaction, request.discord_id, guild_id)?;
            transaction.commit()?;
            return Ok(match persisted {
                Some(persisted_route_state) => RouteChoiceClaim::Conflict {
                    persisted_route_state,
                },
                None => RouteChoiceClaim::MissingTunnel,
            });
        }

        insert_action(
            &transaction,
            request.discord_id,
            guild_id,
            "route_choice",
            0,
            request.detail_json,
            request.now,
        )?;
        transaction.commit()?;
        Ok(RouteChoiceClaim::Claimed)
    }

    /// Persist only the first boss assignment made from a shared snapshot.
    pub fn lock_boss_atomic(
        &self,
        request: BossLockRequest<'_>,
    ) -> Result<BossLockClaim, DigRouteRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE tunnels SET boss_progress = ?1
             WHERE discord_id = ?2 AND guild_id = ?3 AND boss_progress = ?4",
            params![
                request.locked_boss_progress,
                request.discord_id,
                guild_id,
                request.expected_boss_progress
            ],
        )?;
        if changed == 1 {
            transaction.commit()?;
            return Ok(BossLockClaim::Locked {
                boss_progress: request.locked_boss_progress.to_owned(),
            });
        }
        let persisted = transaction
            .query_row(
                "SELECT boss_progress FROM tunnels
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![request.discord_id, guild_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        transaction.commit()?;
        Ok(match persisted {
            Some(boss_progress) => BossLockClaim::Existing { boss_progress },
            None => BossLockClaim::MissingTunnel,
        })
    }

    /// Claim a full boss victory, including route offer, payout, echo, and log.
    pub fn claim_boss_victory_atomic(
        &self,
        request: BossVictoryRequest<'_>,
    ) -> Result<BossOutcomeClaim, DigRouteRepositoryError> {
        if request.echo_window_seconds < 0 {
            return Err(DigRouteRepositoryError::NegativeEchoWindow(
                request.echo_window_seconds,
            ));
        }
        let weakened_until = request
            .now
            .checked_add(request.echo_window_seconds)
            .ok_or(DigRouteRepositoryError::AmountOverflow)?;
        let vanity_tax = request.vanity_tax.max(0);
        let gross_delta = request
            .jc_delta
            .checked_add(vanity_tax)
            .ok_or(DigRouteRepositoryError::AmountOverflow)?;
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let mut sql = String::from(
            "UPDATE tunnels
             SET depth = ?1, boss_progress = ?2, route_state = ?3,
                 stinger_curse = CASE WHEN ?4 THEN NULL ELSE stinger_curse END
             WHERE discord_id = ?5 AND guild_id = ?6
               AND depth = ?7 AND boss_progress = ?8",
        );
        let mut values = vec![
            Value::Integer(request.depth_after),
            Value::Text(request.boss_progress_after.to_owned()),
            Value::Text(request.route_state_after.to_owned()),
            Value::Integer(i64::from(request.clear_stinger_curse)),
            Value::Integer(request.discord_id),
            Value::Integer(guild_id),
            Value::Integer(request.expected_depth),
            Value::Text(request.expected_boss_progress.to_owned()),
        ];
        append_text_guard(
            &mut sql,
            &mut values,
            "stinger_curse",
            request.expected_stinger_curse,
        );
        let changed = transaction.execute(&sql, params_from_iter(values.iter()))?;
        if changed == 0 {
            return conflict_or_missing(transaction, request.discord_id, guild_id);
        }

        if !player_exists(&transaction, request.discord_id, guild_id)? {
            transaction.rollback()?;
            return Ok(BossOutcomeClaim::MissingPlayer);
        }
        if gross_delta != 0 {
            update_balance_with_ledger_context(
                &transaction,
                request.discord_id,
                guild_id,
                gross_delta,
                BalanceLedgerContext {
                    source: "dig",
                    related_id: request.boss_id,
                    metadata: request.detail_json,
                    reason: "dig boss fight credit",
                },
            )?;
        }
        if vanity_tax > 0 {
            update_balance_with_ledger_context(
                &transaction,
                request.discord_id,
                guild_id,
                -vanity_tax,
                BalanceLedgerContext {
                    source: "vanity_tax",
                    related_id: request.boss_id,
                    metadata: request.detail_json,
                    reason: "vanity tax on JC profit",
                },
            )?;
        }
        transaction.execute(
            "INSERT INTO dig_boss_echoes
                 (guild_id, boss_id, depth, killer_discord_id, weakened_until)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(guild_id, boss_id) DO UPDATE SET
                 killer_discord_id = excluded.killer_discord_id,
                 weakened_until = excluded.weakened_until,
                 depth = excluded.depth",
            params![
                guild_id,
                request.boss_id,
                request.boss_depth,
                request.discord_id,
                weakened_until
            ],
        )?;
        insert_action(
            &transaction,
            request.discord_id,
            guild_id,
            "boss_fight",
            request.jc_delta,
            request.detail_json,
            request.now,
        )?;
        transaction.commit()?;
        Ok(BossOutcomeClaim::Claimed)
    }

    /// Claim a boss loss with the same optimistic guard used by victory.
    pub fn claim_boss_loss_atomic(
        &self,
        request: BossLossRequest<'_>,
    ) -> Result<BossOutcomeClaim, DigRouteRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut sql = String::from(
            "UPDATE tunnels
             SET depth = ?1, boss_progress = ?2,
                 stinger_curse = CASE WHEN ?3 IS NULL THEN stinger_curse ELSE ?3 END
             WHERE discord_id = ?4 AND guild_id = ?5
               AND depth = ?6 AND boss_progress = ?7",
        );
        let mut values = vec![
            Value::Integer(request.depth_after),
            Value::Text(request.boss_progress_after.to_owned()),
            request
                .stinger_curse_after
                .map_or(Value::Null, |value| Value::Text(value.to_owned())),
            Value::Integer(request.discord_id),
            Value::Integer(guild_id),
            Value::Integer(request.expected_depth),
            Value::Text(request.expected_boss_progress.to_owned()),
        ];
        append_text_guard(
            &mut sql,
            &mut values,
            "stinger_curse",
            request.expected_stinger_curse,
        );
        let changed = transaction.execute(&sql, params_from_iter(values.iter()))?;
        if changed == 0 {
            return conflict_or_missing(transaction, request.discord_id, guild_id);
        }
        insert_action(
            &transaction,
            request.discord_id,
            guild_id,
            "boss_fight",
            0,
            request.detail_json,
            request.now,
        )?;
        transaction.commit()?;
        Ok(BossOutcomeClaim::Claimed)
    }

    pub fn clear_route_on_abandon(
        &self,
        request: LifecycleRouteClearRequest<'_>,
    ) -> Result<RouteChoiceClaim, DigRouteRepositoryError> {
        self.clear_route_for_lifecycle(request, "abandon")
    }

    pub fn clear_route_on_prestige(
        &self,
        request: LifecycleRouteClearRequest<'_>,
    ) -> Result<RouteChoiceClaim, DigRouteRepositoryError> {
        self.clear_route_for_lifecycle(request, "prestige")
    }

    fn clear_route_for_lifecycle(
        &self,
        request: LifecycleRouteClearRequest<'_>,
        action_type: &str,
    ) -> Result<RouteChoiceClaim, DigRouteRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE tunnels SET route_state = NULL
             WHERE discord_id = ?1 AND guild_id = ?2 AND route_state = ?3",
            params![request.discord_id, guild_id, request.expected_route_state],
        )?;
        if changed == 0 {
            let persisted = query_route_state(&transaction, request.discord_id, guild_id)?;
            transaction.commit()?;
            return Ok(match persisted {
                Some(persisted_route_state) => RouteChoiceClaim::Conflict {
                    persisted_route_state,
                },
                None => RouteChoiceClaim::MissingTunnel,
            });
        }
        insert_action(
            &transaction,
            request.discord_id,
            guild_id,
            action_type,
            0,
            request.detail_json,
            request.now,
        )?;
        transaction.commit()?;
        Ok(RouteChoiceClaim::Claimed)
    }
}

fn append_text_guard(
    sql: &mut String,
    values: &mut Vec<Value>,
    column: &str,
    guard: TextGuard<'_>,
) {
    match guard {
        TextGuard::Ignore => {}
        TextGuard::Null => {
            sql.push_str(" AND ");
            sql.push_str(column);
            sql.push_str(" IS NULL");
        }
        TextGuard::Value(value) => {
            sql.push_str(" AND ");
            sql.push_str(column);
            sql.push_str(" = ?");
            sql.push_str(&(values.len() + 1).to_string());
            values.push(Value::Text(value.to_owned()));
        }
    }
}

fn query_tunnel(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<RouteTunnelRecord>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT discord_id, guild_id, depth, boss_progress,
                    stinger_curse, route_state
             FROM tunnels WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |row| {
                Ok(RouteTunnelRecord {
                    discord_id: row.get(0)?,
                    guild_id: row.get(1)?,
                    depth: row.get(2)?,
                    boss_progress: row.get(3)?,
                    stinger_curse: row.get(4)?,
                    route_state: row.get(5)?,
                })
            },
        )
        .optional()
}

fn query_route_state(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<Option<String>>, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT route_state FROM tunnels
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()
}

fn conflict_or_missing(
    transaction: Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
) -> Result<BossOutcomeClaim, DigRouteRepositoryError> {
    let persisted = query_tunnel(&transaction, discord_id, guild_id)?;
    transaction.commit()?;
    Ok(match persisted {
        Some(persisted) => BossOutcomeClaim::Conflict { persisted },
        None => BossOutcomeClaim::MissingTunnel,
    })
}

fn player_exists(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
) -> Result<bool, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT 1 FROM players WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
}

#[derive(Clone, Copy)]
struct BalanceLedgerContext<'a> {
    source: &'a str,
    related_id: &'a str,
    metadata: &'a str,
    reason: &'a str,
}

fn update_balance_with_ledger_context(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
    delta: i64,
    context: BalanceLedgerContext<'_>,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context
             (id, source, actor_id, related_type, related_id, reason, metadata)
         VALUES (1, ?1, ?2, 'boss_fight', ?3, ?4, ?5)",
        params![
            context.source,
            discord_id,
            context.related_id,
            context.reason,
            context.metadata
        ],
    )?;
    let update = transaction.execute(
        "UPDATE players
         SET jopacoin_balance = jopacoin_balance + ?1,
             updated_at = CURRENT_TIMESTAMP
         WHERE discord_id = ?2 AND guild_id = ?3",
        params![delta, discord_id, guild_id],
    );
    let clear = transaction.execute("DELETE FROM economy_ledger_context", []);
    update?;
    clear?;
    Ok(())
}

fn insert_action(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
    action_type: &str,
    jc_delta: i64,
    detail_json: &str,
    now: i64,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO dig_actions
             (guild_id, actor_id, target_id, action_type, depth_before,
              depth_after, jc_delta, detail, created_at)
         VALUES (?1, ?2, NULL, ?3, 0, 0, ?4, ?5, ?6)",
        params![
            guild_id,
            discord_id,
            action_type,
            jc_delta,
            detail_json,
            now
        ],
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "dig_routes/tests.rs"]
mod tests;
