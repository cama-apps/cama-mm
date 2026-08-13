//! Existing-schema persistence for `/dig` event threats.
//!
//! Python remains the sole schema, migration, catalog, and live-runtime
//! authority. This adapter opens an already-migrated database through
//! [`crate::open_runtime_connection`]. Direct curse replacement mirrors the
//! Python helper boundary, while event and cursed-dig mutations use an exact
//! tunnel/wallet snapshot in one `BEGIN IMMEDIATE` transaction.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

/// A Discord user and guild pair. A missing guild normalizes to Python's
/// historical global-guild sentinel (`0`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreatTunnelKey {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
}

/// The five numeric effects accepted by Python's `TempCurse` contract.
/// `None` means the key was absent, which lets replacement preserve exact
/// no-merge semantics instead of serializing unrelated zero-valued keys.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ThreatCurseEffects {
    pub advance_bonus: Option<i64>,
    pub cave_in_bonus: Option<f64>,
    pub cooldown_penalty: Option<f64>,
    pub jc_bonus: Option<i64>,
    pub luminosity_drain: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThreatCurse {
    pub id: String,
    pub name: String,
    pub digs_remaining: i64,
    pub effects: ThreatCurseEffects,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PersistedCurse {
    Absent,
    Malformed,
    Stored(ThreatCurse),
}

impl PersistedCurse {
    #[must_use]
    pub fn active(&self) -> Option<&ThreatCurse> {
        match self {
            Self::Stored(curse) if curse.digs_remaining > 0 => Some(curse),
            Self::Absent | Self::Malformed | Self::Stored(_) => None,
        }
    }
}

/// Every field guarded by atomic threat settlement.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreatSnapshot {
    pub key: ThreatTunnelKey,
    pub depth: i64,
    pub streak_days: i64,
    pub luminosity: i64,
    pub temp_curse_json: Option<String>,
    pub temp_curse: PersistedCurse,
    pub temp_buff_json: Option<String>,
    pub balance: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplaceCurseOutcome {
    Applied,
    MissingTunnel,
}

#[derive(Clone, Copy, Debug)]
pub enum ThreatCurseMutation<'a> {
    Preserve,
    Clear,
    Replace(&'a ThreatCurse),
}

/// Final state for either an event resolution or a subsequent cursed dig.
/// The event service calculates policy; this request only persists it.
#[derive(Clone, Copy, Debug)]
pub struct AtomicThreatSettlement<'a> {
    pub expected: &'a ThreatSnapshot,
    pub depth_after: i64,
    pub streak_days_after: i64,
    pub luminosity_after: i64,
    pub curse_mutation: ThreatCurseMutation<'a>,
    pub balance_delta: i64,
    pub action_type: &'a str,
    pub related_id: &'a str,
    pub reason: &'a str,
    /// Already-serialized JSON, matching Python's repository boundary.
    pub detail_json: &'a str,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreatSettlementOutcome {
    Applied {
        balance_before: i64,
        balance_after: i64,
    },
    Conflict,
    MissingTunnel,
    MissingPlayer,
}

#[derive(Debug, Error)]
pub enum DigEventThreatRepositoryError {
    #[error("curse contains a non-finite fractional effect")]
    NonFiniteCurseEffect,
    #[error("Dig event threat arithmetic exceeded SQLite's signed 64-bit range")]
    ArithmeticOverflow,
    #[error("Dig event threat SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct DigEventThreatRepository {
    path: PathBuf,
}

impl DigEventThreatRepository {
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

    /// Read a detached, guild-scoped snapshot. No schema is created here.
    pub fn snapshot(
        &self,
        key: ThreatTunnelKey,
    ) -> Result<Option<ThreatSnapshot>, DigEventThreatRepositoryError> {
        let connection = self.connection()?;
        query_snapshot(&connection, key).map_err(Into::into)
    }

    /// Mirror `DigService.set_temp_curse`: replace only `temp_curses`, with
    /// no event audit or wallet mutation and no strengthening.
    pub fn replace_temp_curse(
        &self,
        key: ThreatTunnelKey,
        curse: Option<&ThreatCurse>,
    ) -> Result<ReplaceCurseOutcome, DigEventThreatRepositoryError> {
        let serialized = curse.map(serialize_curse).transpose()?;
        let guild_id = Self::normalize_guild_id(key.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE tunnels SET temp_curses = ?1
              WHERE discord_id = ?2 AND guild_id = ?3",
            params![serialized, key.discord_id, guild_id],
        )?;
        transaction.commit()?;
        Ok(if changed == 1 {
            ReplaceCurseOutcome::Applied
        } else {
            ReplaceCurseOutcome::MissingTunnel
        })
    }

    /// Commit tunnel threat state, an intentionally signed/no-floor wallet
    /// delta, ledger context, and the Dig audit row as one guarded operation.
    pub fn settle_atomic(
        &self,
        request: AtomicThreatSettlement<'_>,
    ) -> Result<ThreatSettlementOutcome, DigEventThreatRepositoryError> {
        let (mutate_curse, curse_after) = match request.curse_mutation {
            ThreatCurseMutation::Preserve => (false, None),
            ThreatCurseMutation::Clear => (true, None),
            ThreatCurseMutation::Replace(curse) => (true, Some(serialize_curse(curse)?)),
        };
        let expected = request.expected;
        let guild_id = Self::normalize_guild_id(expected.key.guild_id);
        let balance_after = expected
            .balance
            .checked_add(request.balance_delta)
            .ok_or(DigEventThreatRepositoryError::ArithmeticOverflow)?;

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let tunnel_changed = transaction.execute(
            "UPDATE tunnels
                SET depth = ?1,
                    streak_days = ?2,
                    luminosity = ?3,
                    temp_curses = CASE WHEN ?4 THEN ?5 ELSE temp_curses END
              WHERE discord_id = ?6 AND guild_id = ?7
                AND COALESCE(depth, 0) = ?8
                AND COALESCE(streak_days, 0) = ?9
                AND COALESCE(luminosity, 100) = ?10
                AND temp_curses IS ?11",
            params![
                request.depth_after,
                request.streak_days_after,
                request.luminosity_after,
                mutate_curse,
                curse_after,
                expected.key.discord_id,
                guild_id,
                expected.depth,
                expected.streak_days,
                expected.luminosity,
                expected.temp_curse_json,
            ],
        )?;
        if tunnel_changed != 1 {
            let exists = row_exists(&transaction, "tunnels", expected.key.discord_id, guild_id)?;
            transaction.rollback()?;
            return Ok(if exists {
                ThreatSettlementOutcome::Conflict
            } else {
                ThreatSettlementOutcome::MissingTunnel
            });
        }

        let player_balance = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance, 0)
                   FROM players
                  WHERE discord_id = ?1 AND guild_id = ?2",
                params![expected.key.discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(player_balance) = player_balance else {
            transaction.rollback()?;
            return Ok(ThreatSettlementOutcome::MissingPlayer);
        };
        if player_balance != expected.balance {
            transaction.rollback()?;
            return Ok(ThreatSettlementOutcome::Conflict);
        }

        if request.balance_delta != 0 {
            set_ledger_context(&transaction, request)?;
            let player_changed = transaction.execute(
                "UPDATE players
                    SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
                  WHERE discord_id = ?2 AND guild_id = ?3
                    AND COALESCE(jopacoin_balance, 0) = ?4",
                params![
                    balance_after,
                    expected.key.discord_id,
                    guild_id,
                    expected.balance,
                ],
            )?;
            if player_changed != 1 {
                transaction.rollback()?;
                return Ok(ThreatSettlementOutcome::Conflict);
            }
            clear_ledger_context(&transaction)?;
        }

        transaction.execute(
            "INSERT INTO dig_actions (
                 guild_id, actor_id, target_id, action_type, depth_before,
                 depth_after, jc_delta, detail, created_at
             ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                guild_id,
                expected.key.discord_id,
                request.action_type,
                expected.depth,
                request.depth_after,
                request.balance_delta,
                request.detail_json,
                request.created_at,
            ],
        )?;
        transaction.commit()?;
        Ok(ThreatSettlementOutcome::Applied {
            balance_before: expected.balance,
            balance_after,
        })
    }
}

fn query_snapshot(
    connection: &Connection,
    key: ThreatTunnelKey,
) -> Result<Option<ThreatSnapshot>, rusqlite::Error> {
    let guild_id = DigEventThreatRepository::normalize_guild_id(key.guild_id);
    let raw = connection
        .query_row(
            "SELECT COALESCE(t.depth, 0),
                    COALESCE(t.streak_days, 0),
                    COALESCE(t.luminosity, 100),
                    t.temp_curses,
                    t.temp_buffs,
                    COALESCE(p.jopacoin_balance, 0)
               FROM tunnels AS t
               LEFT JOIN players AS p
                 ON p.discord_id = t.discord_id AND p.guild_id = t.guild_id
              WHERE t.discord_id = ?1 AND t.guild_id = ?2",
            params![key.discord_id, guild_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((depth, streak_days, luminosity, temp_curse_json, temp_buff_json, balance)) = raw
    else {
        return Ok(None);
    };
    let temp_curse = parse_curse(connection, temp_curse_json.as_deref())?;
    Ok(Some(ThreatSnapshot {
        key,
        depth,
        streak_days,
        luminosity,
        temp_curse_json,
        temp_curse,
        temp_buff_json,
        balance,
    }))
}

fn parse_curse(
    connection: &Connection,
    raw: Option<&str>,
) -> Result<PersistedCurse, rusqlite::Error> {
    let Some(raw) = raw.filter(|value| !value.is_empty()) else {
        return Ok(PersistedCurse::Absent);
    };
    let valid = connection.query_row("SELECT json_valid(?1)", [raw], |row| row.get::<_, i64>(0))?;
    if valid == 0 {
        return Ok(PersistedCurse::Malformed);
    }
    type ParsedCurseRow = (
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<i64>,
        Option<f64>,
        Option<f64>,
        Option<i64>,
        Option<i64>,
    );
    let parsed: ParsedCurseRow = connection.query_row(
        "SELECT json_extract(?1, '$.id'),
                json_extract(?1, '$.name'),
                json_extract(?1, '$.digs_remaining'),
                json_extract(?1, '$.effect.advance_bonus'),
                json_extract(?1, '$.effect.cave_in_bonus'),
                json_extract(?1, '$.effect.cooldown_penalty'),
                json_extract(?1, '$.effect.jc_bonus'),
                json_extract(?1, '$.effect.luminosity_drain')",
        [raw],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            ))
        },
    )?;
    let (Some(id), Some(name), Some(digs_remaining), advance, cave_in, cooldown, jc, luminosity) =
        parsed
    else {
        return Ok(PersistedCurse::Malformed);
    };
    Ok(PersistedCurse::Stored(ThreatCurse {
        id,
        name,
        digs_remaining,
        effects: ThreatCurseEffects {
            advance_bonus: advance,
            cave_in_bonus: cave_in,
            cooldown_penalty: cooldown,
            jc_bonus: jc,
            luminosity_drain: luminosity,
        },
    }))
}

fn serialize_curse(curse: &ThreatCurse) -> Result<String, DigEventThreatRepositoryError> {
    for value in [curse.effects.cave_in_bonus, curse.effects.cooldown_penalty]
        .into_iter()
        .flatten()
    {
        if !value.is_finite() {
            return Err(DigEventThreatRepositoryError::NonFiniteCurseEffect);
        }
    }
    let mut json = String::from("{\"id\":");
    push_json_string(&mut json, &curse.id);
    json.push_str(",\"name\":");
    push_json_string(&mut json, &curse.name);
    let _ = write!(
        json,
        ",\"digs_remaining\":{},\"effect\":{{",
        curse.digs_remaining
    );
    let mut first = true;
    macro_rules! effect {
        ($name:literal, $value:expr) => {
            if let Some(value) = $value {
                if !first {
                    json.push(',');
                }
                first = false;
                let _ = write!(json, concat!("\"", $name, "\":{}"), value);
            }
        };
    }
    effect!("advance_bonus", curse.effects.advance_bonus);
    effect!("cave_in_bonus", curse.effects.cave_in_bonus);
    effect!("cooldown_penalty", curse.effects.cooldown_penalty);
    effect!("jc_bonus", curse.effects.jc_bonus);
    effect!("luminosity_drain", curse.effects.luminosity_drain);
    let _ = first;
    json.push_str("}}");
    Ok(json)
}

fn push_json_string(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character <= '\u{1f}' => {
                let _ = write!(output, "\\u{:04x}", u32::from(character));
            }
            character => output.push(character),
        }
    }
    output.push('"');
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    request: AtomicThreatSettlement<'_>,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context (
             id, source, actor_id, related_type, related_id, reason, metadata
         ) VALUES (1, 'dig', ?1, ?2, ?3, ?4, ?5)",
        params![
            request.expected.key.discord_id,
            request.action_type,
            request.related_id,
            request.reason,
            request.detail_json,
        ],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn row_exists(
    transaction: &Transaction<'_>,
    table: &str,
    discord_id: i64,
    guild_id: i64,
) -> Result<bool, rusqlite::Error> {
    debug_assert_eq!(table, "tunnels");
    transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM tunnels WHERE discord_id = ?1 AND guild_id = ?2
         )",
        params![discord_id, guild_id],
        |row| row.get::<_, bool>(0),
    )
}

#[cfg(test)]
#[path = "dig_event_threats/tests.rs"]
mod tests;
