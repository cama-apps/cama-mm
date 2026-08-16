//! Existing-schema persistence used by the live Rust `/shop` provider.
//!
//! The provider intentionally composes the focused package-deal, soft-avoid,
//! mana, and timed-buff repositories already present in this crate.  This
//! module owns the remaining Python-compatible atomic boundaries: ordinary
//! purchases, paid pings, Double or Nothing, hostile-loss protection, recent
//! loss aggregates, and the few dig mutations purchased by mana items.  It
//! never creates or alters schema.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde_json::{Value, json};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Debug, Error)]
pub enum ShopRuntimeRepositoryError {
    #[error("shop runtime cost cannot be negative")]
    NegativeCost,
    #[error("shop runtime cooldown cannot be negative")]
    NegativeCooldown,
    #[error("shop runtime amount must be positive")]
    NonPositiveAmount,
    #[error("shop runtime event key is required")]
    MissingEventKey,
    #[error("shop runtime player {discord_id} is not registered in guild {guild_id}")]
    MissingPlayer { discord_id: i64, guild_id: i64 },
    #[error("shop runtime recipient is required for a player destination")]
    MissingRecipient,
    #[error("shop runtime recipient cannot be the victim")]
    RecipientIsVictim,
    #[error("shop runtime event key already has a different payload")]
    EventPayloadConflict,
    #[error("shop runtime protection pool is not eligible for reconciliation")]
    InvalidProtectionPool,
    #[error("shop runtime JSON is invalid: {0}")]
    InvalidJson(String),
    #[error("shop runtime SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShopPlayer {
    pub discord_id: i64,
    pub name: String,
    pub wins: i64,
    pub losses: i64,
    pub glicko_rating: Option<f64>,
    pub glicko_rd: Option<f64>,
    pub glicko_volatility: Option<f64>,
    pub os_mu: Option<f64>,
    pub os_sigma: Option<f64>,
    pub balance: i64,
    pub lowest_balance: Option<i64>,
    pub last_double_or_nothing: Option<i64>,
}

impl ShopPlayer {
    #[must_use]
    pub const fn games_played(&self) -> i64 {
        self.wins.saturating_add(self.losses)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaidPingKind {
    Dash,
    Kevin,
}

impl PaidPingKind {
    const fn column(self) -> &'static str {
        match self {
            Self::Dash => "last_pingedash",
            Self::Kevin => "last_pingedkevin",
        }
    }

    #[must_use]
    pub const fn source(self) -> &'static str {
        match self {
            Self::Dash => "pingedash",
            Self::Kevin => "pingedkevin",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PurchaseFailureReason {
    NotRegistered,
    OnCooldown,
    InsufficientBalance,
    InDebt,
    NothingToDouble,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaidPingPurchase {
    pub success: bool,
    pub reason: Option<PurchaseFailureReason>,
    pub balance: i64,
    pub cooldown_ends_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DoubleOrNothingRequest {
    pub discord_id: i64,
    pub guild_id: i64,
    pub cost: i64,
    pub won: bool,
    pub now: i64,
    pub cooldown_seconds: i64,
    pub bypass_cooldown: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoubleOrNothingSettlement {
    pub success: bool,
    pub reason: Option<PurchaseFailureReason>,
    pub balance_before: i64,
    pub balance_at_risk: i64,
    pub balance_after: i64,
    pub won: bool,
    pub cooldown_ends_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RecentLossAggregates {
    pub largest: i64,
    pub total: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigTunnelInfo {
    pub depth: i64,
    pub luminosity: i64,
    pub prestige_level: i64,
    pub relic_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostileDestination {
    Burn,
    Player,
    Reserve,
}

impl HostileDestination {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Burn => "burn",
            Self::Player => "player",
            Self::Reserve => "reserve",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostileLossRequest {
    pub victim_id: i64,
    pub guild_id: i64,
    pub requested: i64,
    pub kind: String,
    pub actor_id: Option<i64>,
    pub event_key: String,
    pub destination: HostileDestination,
    pub recipient_id: Option<i64>,
    pub clamp_to_balance: bool,
    pub min_balance: Option<i64>,
    pub protect_self: bool,
    pub metadata: Value,
    pub occurred_at: i64,
    pub mana_date: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtectionDetail {
    pub source: String,
    pub absorbed: i64,
    pub rate_millionths: i64,
    pub capacity_before: Option<i64>,
    pub capacity_after: Option<i64>,
    pub buff_id: Option<i64>,
    pub retroactive: bool,
}

impl ProtectionDetail {
    #[must_use]
    pub fn rate(&self) -> f64 {
        self.rate_millionths as f64 / 1_000_000.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostileLossSettlement {
    pub event_id: i64,
    pub event_key: String,
    pub requested: i64,
    pub attempted: i64,
    pub absorbed: i64,
    pub applied: i64,
    pub victim_balance_before: i64,
    pub victim_balance_after: i64,
    pub destination_balance_before: Option<i64>,
    pub destination_balance_after: Option<i64>,
    pub duplicate: bool,
    pub details: Vec<ProtectionDetail>,
}

/// Durable result for a non-JC attack such as sabotage.
///
/// These attacks do not move JopaCoin, but they still need an idempotent
/// hostile-loss event so a retry cannot consume a second Aegis charge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NonJcProtectionResult {
    pub blocked: bool,
    pub source: Option<String>,
    pub buff_id: Option<i64>,
    pub event_id: i64,
    pub duplicate: bool,
}

#[derive(Clone, Debug)]
pub struct ShopRuntimeRepository {
    path: PathBuf,
}

impl ShopRuntimeRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    /// Fail closed before command registration when the migrated shop schema
    /// is not available. This check is strictly read-only.
    pub fn verify_schema(&self) -> Result<(), ShopRuntimeRepositoryError> {
        let connection = self.connection()?;
        for table in [
            "players",
            "economy_ledger_context",
            "double_or_nothing_spins",
            "bets",
            "matches",
            "wheel_spins",
            "hostile_loss_events",
            "mana_protection_events",
            "manashop_buffs",
            "manashop_daily_uses",
            "manashop_purchases",
            "player_mana",
            "nonprofit_fund",
            "tunnels",
            "dig_artifacts",
            "soft_avoids",
            "player_pairings",
            "package_deals",
            "package_deal_purchases",
            "recalibration_state",
            "openskill_rating_events",
            "openskill_rating_revisions",
        ] {
            connection.query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |_| Ok(()),
            )?;
        }
        Ok(())
    }

    pub fn player(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<ShopPlayer>, ShopRuntimeRepositoryError> {
        self.connection()?
            .query_row(
                "SELECT discord_id, COALESCE(discord_username, ''),
                        COALESCE(wins, 0), COALESCE(losses, 0),
                        glicko_rating, glicko_rd, glicko_volatility,
                        os_mu, os_sigma, COALESCE(jopacoin_balance, 0),
                        lowest_balance_ever, last_double_or_nothing
                 FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                map_player,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn players(&self, guild_id: i64) -> Result<Vec<ShopPlayer>, ShopRuntimeRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id, COALESCE(discord_username, ''),
                    COALESCE(wins, 0), COALESCE(losses, 0),
                    glicko_rating, glicko_rd, glicko_volatility,
                    os_mu, os_sigma, COALESCE(jopacoin_balance, 0),
                    lowest_balance_ever, last_double_or_nothing
             FROM players WHERE guild_id=?1",
        )?;
        statement
            .query_map([guild_id], map_player)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn try_spend(
        &self,
        discord_id: i64,
        guild_id: i64,
        amount: i64,
        source: &str,
        reason: &str,
    ) -> Result<bool, ShopRuntimeRepositoryError> {
        if amount < 0 {
            return Err(ShopRuntimeRepositoryError::NegativeCost);
        }
        if amount == 0 {
            return Ok(true);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        set_ledger_context(
            &transaction,
            Some(source),
            Some(discord_id),
            None,
            None,
            Some(reason),
            None,
        )?;
        let changed = transaction.execute(
            "UPDATE players
             SET jopacoin_balance=COALESCE(jopacoin_balance,0)-?1,
                 updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3
               AND COALESCE(jopacoin_balance,0)>=?1",
            params![amount, discord_id, guild_id],
        );
        clear_ledger_context(&transaction)?;
        let changed = changed?;
        if changed == 1 {
            update_lowest_balance(&transaction, discord_id, guild_id)?;
        }
        transaction.commit()?;
        Ok(changed == 1)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn adjust_balance(
        &self,
        discord_id: i64,
        guild_id: i64,
        delta: i64,
        source: Option<&str>,
        actor_id: Option<i64>,
        related_type: Option<&str>,
        related_id: Option<&str>,
        reason: Option<&str>,
        metadata: Option<&Value>,
    ) -> Result<i64, ShopRuntimeRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let has_context = source.is_some()
            || actor_id.is_some()
            || related_type.is_some()
            || related_id.is_some()
            || reason.is_some()
            || metadata.is_some();
        if has_context {
            set_ledger_context(
                &transaction,
                source,
                actor_id,
                related_type,
                related_id,
                reason,
                metadata,
            )?;
        }
        let changed = transaction.execute(
            "UPDATE players
             SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                 updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3",
            params![delta, discord_id, guild_id],
        );
        if has_context {
            clear_ledger_context(&transaction)?;
        }
        if changed? != 1 {
            return Err(ShopRuntimeRepositoryError::MissingPlayer {
                discord_id,
                guild_id,
            });
        }
        if delta < 0 {
            update_lowest_balance(&transaction, discord_id, guild_id)?;
        }
        let balance = player_balance(&transaction, discord_id, guild_id)?;
        transaction.commit()?;
        Ok(balance)
    }

    pub fn purchase_paid_ping(
        &self,
        kind: PaidPingKind,
        discord_id: i64,
        guild_id: i64,
        cost: i64,
        now: i64,
        cooldown_seconds: i64,
    ) -> Result<PaidPingPurchase, ShopRuntimeRepositoryError> {
        if cost < 0 {
            return Err(ShopRuntimeRepositoryError::NegativeCost);
        }
        if cooldown_seconds < 0 {
            return Err(ShopRuntimeRepositoryError::NegativeCooldown);
        }
        let column = kind.column();
        let cutoff = now.saturating_sub(cooldown_seconds);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        set_ledger_context(
            &transaction,
            Some(kind.source()),
            Some(discord_id),
            Some("command"),
            Some(kind.source()),
            Some(&format!("{} purchase", kind.source())),
            None,
        )?;
        let sql = format!(
            "UPDATE players
             SET jopacoin_balance=COALESCE(jopacoin_balance,0)-?1,
                 {column}=?2, updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?3 AND guild_id=?4
               AND COALESCE(jopacoin_balance,0)>=?1
               AND ({column} IS NULL OR {column}<=?5)"
        );
        let changed = transaction.execute(&sql, params![cost, now, discord_id, guild_id, cutoff]);
        clear_ledger_context(&transaction)?;
        if changed? == 1 {
            update_lowest_balance(&transaction, discord_id, guild_id)?;
            let balance = player_balance(&transaction, discord_id, guild_id)?;
            transaction.commit()?;
            return Ok(PaidPingPurchase {
                success: true,
                reason: None,
                balance,
                cooldown_ends_at: Some(now.saturating_add(cooldown_seconds)),
            });
        }
        let query = format!(
            "SELECT COALESCE(jopacoin_balance,0), {column}
             FROM players WHERE discord_id=?1 AND guild_id=?2"
        );
        let row = transaction
            .query_row(&query, params![discord_id, guild_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?))
            })
            .optional()?;
        let result = match row {
            None => PaidPingPurchase {
                success: false,
                reason: Some(PurchaseFailureReason::NotRegistered),
                balance: 0,
                cooldown_ends_at: None,
            },
            Some((balance, Some(last))) if last > cutoff => PaidPingPurchase {
                success: false,
                reason: Some(PurchaseFailureReason::OnCooldown),
                balance,
                cooldown_ends_at: Some(last.saturating_add(cooldown_seconds)),
            },
            Some((balance, _)) => PaidPingPurchase {
                success: false,
                reason: Some(PurchaseFailureReason::InsufficientBalance),
                balance,
                cooldown_ends_at: None,
            },
        };
        transaction.commit()?;
        Ok(result)
    }

    pub fn settle_double_or_nothing(
        &self,
        request: DoubleOrNothingRequest,
    ) -> Result<DoubleOrNothingSettlement, ShopRuntimeRepositoryError> {
        if request.cost < 0 {
            return Err(ShopRuntimeRepositoryError::NegativeCost);
        }
        if request.cooldown_seconds < 0 {
            return Err(ShopRuntimeRepositoryError::NegativeCooldown);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance,0), last_double_or_nothing
                 FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![request.discord_id, request.guild_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()?;
        let Some((balance, last_spin)) = row else {
            transaction.commit()?;
            return Ok(rejected_double(
                PurchaseFailureReason::NotRegistered,
                0,
                request.won,
            ));
        };
        if !request.bypass_cooldown
            && last_spin
                .is_some_and(|last| last >= request.now.saturating_sub(request.cooldown_seconds))
        {
            let mut rejected =
                rejected_double(PurchaseFailureReason::OnCooldown, balance, request.won);
            rejected.cooldown_ends_at =
                last_spin.map(|last| last.saturating_add(request.cooldown_seconds));
            transaction.commit()?;
            return Ok(rejected);
        }
        let reason = if balance < 0 {
            Some(PurchaseFailureReason::InDebt)
        } else if balance < request.cost {
            Some(PurchaseFailureReason::InsufficientBalance)
        } else if balance == request.cost {
            Some(PurchaseFailureReason::NothingToDouble)
        } else {
            None
        };
        if let Some(reason) = reason {
            transaction.commit()?;
            return Ok(rejected_double(reason, balance, request.won));
        }
        let at_risk = balance - request.cost;
        transaction.execute(
            "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3",
            params![at_risk, request.discord_id, request.guild_id],
        )?;
        update_lowest_balance(&transaction, request.discord_id, request.guild_id)?;
        let after = if request.won {
            at_risk.saturating_mul(2)
        } else {
            0
        };
        transaction.execute(
            "UPDATE players SET jopacoin_balance=?1,last_double_or_nothing=?2,
                    updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?3 AND guild_id=?4",
            params![after, request.now, request.discord_id, request.guild_id],
        )?;
        update_lowest_balance(&transaction, request.discord_id, request.guild_id)?;
        transaction.execute(
            "INSERT INTO double_or_nothing_spins
                 (guild_id,discord_id,cost,balance_before,balance_after,won,spin_time)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                request.guild_id,
                request.discord_id,
                request.cost,
                at_risk,
                after,
                i64::from(request.won),
                request.now
            ],
        )?;
        transaction.commit()?;
        Ok(DoubleOrNothingSettlement {
            success: true,
            reason: None,
            balance_before: balance,
            balance_at_risk: at_risk,
            balance_after: after,
            won: request.won,
            cooldown_ends_at: Some(request.now.saturating_add(request.cooldown_seconds)),
        })
    }

    /// Preserve Python's legacy atomic cooldown-claim repository contract.
    /// The live command uses [`Self::settle_double_or_nothing`], but callers
    /// and cross-language parity tests still rely on this exact check-and-set
    /// operation being available independently.
    pub fn try_claim_double_or_nothing(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
        cooldown_seconds: i64,
    ) -> Result<bool, ShopRuntimeRepositoryError> {
        if cooldown_seconds < 0 {
            return Err(ShopRuntimeRepositoryError::NegativeCooldown);
        }
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE players
             SET last_double_or_nothing=?1,updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3
               AND (last_double_or_nothing IS NULL OR last_double_or_nothing<?4)",
            params![
                now,
                discord_id,
                guild_id,
                now.saturating_sub(cooldown_seconds)
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn recent_loss_aggregates(
        &self,
        discord_id: i64,
        guild_id: i64,
        cutoff_ts: i64,
    ) -> Result<RecentLossAggregates, ShopRuntimeRepositoryError> {
        self.connection()?
            .query_row(
                "WITH losses(loss) AS (
                     SELECT b.amount*COALESCE(b.leverage,1)
                     FROM bets b JOIN matches m ON m.match_id=b.match_id
                     WHERE b.guild_id=?1 AND b.discord_id=?2
                       AND b.match_id IS NOT NULL AND b.bet_time>=?3
                       AND CASE
                         WHEN m.winning_team=1 AND b.team_bet_on='radiant' THEN 0
                         WHEN m.winning_team=2 AND b.team_bet_on='dire' THEN 0
                         ELSE 1 END=1
                     UNION ALL
                     SELECT -result FROM wheel_spins
                     WHERE guild_id=?1 AND discord_id=?2 AND spin_time>=?3 AND result<0
                 )
                 SELECT COALESCE(MAX(loss),0),COALESCE(SUM(loss),0) FROM losses",
                params![guild_id, discord_id, cutoff_ts],
                |row| {
                    Ok(RecentLossAggregates {
                        largest: row.get(0)?,
                        total: row.get(1)?,
                    })
                },
            )
            .map_err(Into::into)
    }

    pub fn shave_dig_cooldown(
        &self,
        discord_id: i64,
        guild_id: i64,
        seconds: i64,
    ) -> Result<i64, ShopRuntimeRepositoryError> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE tunnels SET last_dig_at=MAX(0,last_dig_at-?1)
             WHERE discord_id=?2 AND guild_id=?3 AND last_dig_at IS NOT NULL",
            params![seconds.max(0), discord_id, guild_id],
        )?;
        Ok(if changed == 1 { seconds.max(0) } else { 0 })
    }

    pub fn set_dig_temp_buff(
        &self,
        discord_id: i64,
        guild_id: i64,
        payload: &Value,
    ) -> Result<(), ShopRuntimeRepositoryError> {
        self.connection()?.execute(
            "UPDATE tunnels SET temp_buffs=?1 WHERE discord_id=?2 AND guild_id=?3",
            params![payload.to_string(), discord_id, guild_id],
        )?;
        Ok(())
    }

    pub fn has_equipped_relic(
        &self,
        discord_id: i64,
        guild_id: i64,
        relic_id: &str,
    ) -> Result<bool, ShopRuntimeRepositoryError> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT 1 FROM dig_artifacts
                 WHERE discord_id=?1 AND guild_id=?2 AND artifact_id=?3
                   AND is_relic=1 AND equipped=1 LIMIT 1",
                params![discord_id, guild_id, relic_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn dig_tunnel_info(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<DigTunnelInfo>, ShopRuntimeRepositoryError> {
        let connection = self.connection()?;
        let tunnel = connection
            .query_row(
                "SELECT depth,luminosity,prestige_level FROM tunnels
                 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((depth, luminosity, prestige_level)) = tunnel else {
            return Ok(None);
        };
        let mut statement = connection.prepare(
            "SELECT artifact_id FROM dig_artifacts
             WHERE discord_id=?1 AND guild_id=?2 AND is_relic=1 AND equipped=1
             ORDER BY id",
        )?;
        let relic_ids = statement
            .query_map(params![discord_id, guild_id], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(Some(DigTunnelInfo {
            depth,
            luminosity,
            prestige_level,
            relic_ids,
        }))
    }

    pub fn apply_hostile_loss(
        &self,
        request: &HostileLossRequest,
    ) -> Result<HostileLossSettlement, ShopRuntimeRepositoryError> {
        validate_hostile_request(request)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let settlement = apply_hostile_loss_in(&transaction, request)?;
        transaction.commit()?;
        Ok(settlement)
    }

    /// Read one durable hostile-loss result by its exact idempotency key.
    /// Recovery callers use this before reserving a consumable Blood-Pact
    /// allowance, avoiding a second reservation when the protected transfer
    /// committed before the process stopped.
    pub fn hostile_loss_event(
        &self,
        guild_id: i64,
        victim_id: i64,
        event_key: &str,
    ) -> Result<Option<HostileLossSettlement>, ShopRuntimeRepositoryError> {
        let connection = self.connection()?;
        find_hostile_event(&connection, guild_id, victim_id, event_key).map_err(Into::into)
    }

    /// Persist one non-JC hostile attack and consume at most one whole-use
    /// ward. Counterspell and Sanctuary block without consumption; Aegis and
    /// First Aegis Today are consumed exactly once. Reusing the event key
    /// returns the stored decision without touching the ward again.
    pub fn block_non_jc_attack(
        &self,
        victim_id: i64,
        guild_id: i64,
        actor_id: Option<i64>,
        event_key: &str,
        occurred_at: i64,
    ) -> Result<NonJcProtectionResult, ShopRuntimeRepositoryError> {
        if event_key.trim().is_empty() {
            return Err(ShopRuntimeRepositoryError::MissingEventKey);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = find_hostile_event(&transaction, guild_id, victim_id, event_key)? {
            let detail = existing.details.first();
            transaction.commit()?;
            return Ok(NonJcProtectionResult {
                blocked: detail.is_some(),
                source: detail.map(|detail| detail.source.clone()),
                buff_id: detail.and_then(|detail| detail.buff_id),
                event_id: existing.event_id,
                duplicate: true,
            });
        }

        let balance = player_balance(&transaction, victim_id, guild_id)?;
        let shieldable = actor_id != Some(victim_id);
        let mut selected: Option<(String, i64)> = None;
        if shieldable {
            for (buff_type, shared, consume) in [
                ("counterspell", false, false),
                ("sanctuary", true, false),
                ("aegis", false, true),
                ("first_aegis_today", false, true),
            ] {
                let Some(buff) = active_buffs(
                    &transaction,
                    victim_id,
                    guild_id,
                    buff_type,
                    occurred_at,
                    shared,
                )?
                .into_iter()
                .next() else {
                    continue;
                };
                if consume {
                    transaction.execute(
                        "UPDATE manashop_buffs SET triggered=1 WHERE id=?1",
                        [buff.id],
                    )?;
                }
                selected = Some((buff_type.to_owned(), buff.id));
                break;
            }
        }

        let details = selected
            .as_ref()
            .map(|(source, buff_id)| ProtectionDetail {
                source: source.clone(),
                absorbed: 0,
                rate_millionths: 1_000_000,
                capacity_before: None,
                capacity_after: None,
                buff_id: Some(*buff_id),
                retroactive: false,
            })
            .into_iter()
            .collect::<Vec<_>>();
        transaction.execute(
            "INSERT INTO hostile_loss_events (
                 guild_id,victim_id,actor_id,event_key,kind,destination,recipient_id,
                 requested,attempted,absorbed,applied,victim_balance_before,
                 victim_balance_after,destination_balance_before,destination_balance_after,
                 shieldable,retro_covered,protection_details,metadata,occurred_at,created_at
             ) VALUES (?1,?2,?3,?4,'sabotage','burn',NULL,0,0,0,0,?5,?5,NULL,NULL,?6,0,?7,NULL,?8,?8)",
            params![
                guild_id,
                victim_id,
                actor_id,
                event_key,
                balance,
                i64::from(shieldable),
                details_to_json(&details).to_string(),
                occurred_at,
            ],
        )?;
        let event_id = transaction.last_insert_rowid();
        if let Some(detail) = details.first() {
            let buff_id = detail.buff_id.expect("selected protection buff id");
            insert_protection_event(
                &transaction,
                event_id,
                guild_id,
                victim_id,
                detail,
                &format!("buff:{buff_id}"),
                occurred_at,
                Some(&json!({"non_jc": true})),
            )?;
        }
        transaction.commit()?;
        Ok(NonJcProtectionResult {
            blocked: selected.is_some(),
            source: selected.as_ref().map(|(source, _)| source.clone()),
            buff_id: selected.map(|(_, buff_id)| buff_id),
            event_id,
            duplicate: false,
        })
    }

    /// Match Python's ordered batch/savepoint semantics. Invalid victims are
    /// returned in place while valid settlements remain committed.
    pub fn apply_hostile_losses(
        &self,
        requests: &[HostileLossRequest],
    ) -> Result<Vec<Result<HostileLossSettlement, String>>, ShopRuntimeRepositoryError> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut results = Vec::with_capacity(requests.len());
        for (index, request) in requests.iter().enumerate() {
            let savepoint = format!("shop_hostile_{index}");
            transaction.execute_batch(&format!("SAVEPOINT {savepoint}"))?;
            match validate_hostile_request(request)
                .and_then(|()| apply_hostile_loss_in(&transaction, request))
            {
                Ok(settlement) => {
                    transaction.execute_batch(&format!("RELEASE SAVEPOINT {savepoint}"))?;
                    results.push(Ok(settlement));
                }
                Err(error) => {
                    transaction.execute_batch(&format!(
                        "ROLLBACK TO SAVEPOINT {savepoint}; RELEASE SAVEPOINT {savepoint}"
                    ))?;
                    results.push(Err(error.to_string()));
                }
            }
        }
        transaction.commit()?;
        Ok(results)
    }

    pub fn reconcile_purchased_pool(
        &self,
        discord_id: i64,
        guild_id: i64,
        buff_id: i64,
        since_ts: i64,
        now: i64,
    ) -> Result<i64, ShopRuntimeRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = transaction
            .query_row(
                "SELECT id,buff_type,data FROM manashop_buffs
                 WHERE id=?1 AND guild_id=?2 AND discord_id=?3
                   AND triggered=0 AND expires_at>?4",
                params![buff_id, guild_id, discord_id, now],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((buff_id, buff_type, raw_data)) = row else {
            transaction.commit()?;
            return Ok(0);
        };
        let (mut data, mut capacity, rate) = decode_pool(&buff_type, raw_data.as_deref())?;
        let retroactive = buff_type == "reprieve"
            || data
                .get("rolling_retroactive")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        if !retroactive {
            return Err(ShopRuntimeRepositoryError::InvalidProtectionPool);
        }
        let pool_key = format!("buff:{buff_id}");
        let mut refunded = 0_i64;
        for event in
            eligible_retro_events(&transaction, discord_id, guild_id, since_ts, now, &pool_key)?
        {
            let uncovered = event.applied.saturating_sub(event.retro_covered);
            let amount = capacity.min(rate_absorption(uncovered, rate));
            if amount <= 0 {
                continue;
            }
            let after = capacity - amount;
            let detail = ProtectionDetail {
                source: buff_type.clone(),
                absorbed: amount,
                rate_millionths: rate_to_millionths(rate),
                capacity_before: Some(capacity),
                capacity_after: Some(after),
                buff_id: Some(buff_id),
                retroactive: true,
            };
            credit_retro_event(
                &transaction,
                &event,
                discord_id,
                guild_id,
                amount,
                &detail,
                &pool_key,
                now,
            )?;
            capacity = after;
            refunded = refunded.saturating_add(amount);
            if capacity <= 0 {
                break;
            }
        }
        data["capacity_remaining"] = Value::from(capacity);
        transaction.execute(
            "UPDATE manashop_buffs SET data=?1,triggered=?2 WHERE id=?3",
            params![data.to_string(), i64::from(capacity <= 0), buff_id],
        )?;
        transaction.commit()?;
        Ok(refunded)
    }
}

fn map_player(row: &Row<'_>) -> Result<ShopPlayer, rusqlite::Error> {
    Ok(ShopPlayer {
        discord_id: row.get(0)?,
        name: row.get(1)?,
        wins: row.get(2)?,
        losses: row.get(3)?,
        glicko_rating: row.get(4)?,
        glicko_rd: row.get(5)?,
        glicko_volatility: row.get(6)?,
        os_mu: row.get(7)?,
        os_sigma: row.get(8)?,
        balance: row.get(9)?,
        lowest_balance: row.get(10)?,
        last_double_or_nothing: row.get(11)?,
    })
}

fn rejected_double(
    reason: PurchaseFailureReason,
    balance: i64,
    won: bool,
) -> DoubleOrNothingSettlement {
    DoubleOrNothingSettlement {
        success: false,
        reason: Some(reason),
        balance_before: balance,
        balance_at_risk: balance,
        balance_after: balance,
        won,
        cooldown_ends_at: None,
    }
}

fn set_ledger_context(
    connection: &Connection,
    source: Option<&str>,
    actor_id: Option<i64>,
    related_type: Option<&str>,
    related_id: Option<&str>,
    reason: Option<&str>,
    metadata: Option<&Value>,
) -> Result<(), rusqlite::Error> {
    connection.execute("DELETE FROM economy_ledger_context", [])?;
    connection.execute(
        "INSERT INTO economy_ledger_context
             (id,source,actor_id,related_type,related_id,reason,metadata)
         VALUES (1,?1,?2,?3,?4,?5,?6)",
        params![
            source,
            actor_id,
            related_type,
            related_id,
            reason,
            metadata.map(Value::to_string)
        ],
    )?;
    Ok(())
}

fn clear_ledger_context(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn update_lowest_balance(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<(), rusqlite::Error> {
    connection.execute(
        "UPDATE players SET lowest_balance_ever=jopacoin_balance
         WHERE discord_id=?1 AND guild_id=?2
           AND (lowest_balance_ever IS NULL OR jopacoin_balance<lowest_balance_ever)",
        params![discord_id, guild_id],
    )?;
    Ok(())
}

fn player_balance(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<i64, ShopRuntimeRepositoryError> {
    connection
        .query_row(
            "SELECT COALESCE(jopacoin_balance,0) FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(ShopRuntimeRepositoryError::MissingPlayer {
            discord_id,
            guild_id,
        })
}

pub(crate) fn validate_hostile_request(
    request: &HostileLossRequest,
) -> Result<(), ShopRuntimeRepositoryError> {
    if request.requested <= 0 {
        return Err(ShopRuntimeRepositoryError::NonPositiveAmount);
    }
    if request.event_key.trim().is_empty() {
        return Err(ShopRuntimeRepositoryError::MissingEventKey);
    }
    if request.destination == HostileDestination::Player && request.recipient_id.is_none() {
        return Err(ShopRuntimeRepositoryError::MissingRecipient);
    }
    if request.recipient_id == Some(request.victim_id) {
        return Err(ShopRuntimeRepositoryError::RecipientIsVictim);
    }
    Ok(())
}

pub(crate) fn apply_hostile_loss_in(
    connection: &Connection,
    request: &HostileLossRequest,
) -> Result<HostileLossSettlement, ShopRuntimeRepositoryError> {
    if let Some(existing) = find_hostile_event(
        connection,
        request.guild_id,
        request.victim_id,
        &request.event_key,
    )? {
        let row = connection.query_row(
            "SELECT requested,kind,destination,actor_id,recipient_id,shieldable
             FROM hostile_loss_events WHERE event_id=?1",
            [existing.event_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )?;
        if row
            != (
                request.requested,
                request.kind.clone(),
                request.destination.as_str().to_owned(),
                request.actor_id,
                request.recipient_id,
                i64::from(request.protect_self || request.actor_id != Some(request.victim_id)),
            )
        {
            return Err(ShopRuntimeRepositoryError::EventPayloadConflict);
        }
        return Ok(HostileLossSettlement {
            duplicate: true,
            ..existing
        });
    }

    let victim_before = player_balance(connection, request.victim_id, request.guild_id)?;
    let mut attempted = request.requested;
    if request
        .min_balance
        .is_some_and(|minimum| victim_before < minimum)
    {
        attempted = 0;
    }
    if request.clamp_to_balance {
        attempted = attempted.min(victim_before.max(0));
    }
    let destination_before = match request.destination {
        HostileDestination::Burn => None,
        HostileDestination::Player => Some(player_balance(
            connection,
            request
                .recipient_id
                .ok_or(ShopRuntimeRepositoryError::MissingRecipient)?,
            request.guild_id,
        )?),
        HostileDestination::Reserve => Some(
            connection
                .query_row(
                    "SELECT COALESCE(total_collected,0) FROM nonprofit_fund WHERE guild_id=?1",
                    [request.guild_id],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or_default(),
        ),
    };
    let shieldable = request.protect_self || request.actor_id != Some(request.victim_id);
    let mut remaining = attempted;
    let mut details = Vec::new();
    let mut pool_keys = Vec::new();

    if shieldable
        && remaining > 0
        && let Some(buff) = active_buffs(
            connection,
            request.victim_id,
            request.guild_id,
            "counterspell",
            request.occurred_at,
            false,
        )?
        .into_iter()
        .next()
    {
        details.push(ProtectionDetail {
            source: "counterspell".to_owned(),
            absorbed: remaining,
            rate_millionths: 1_000_000,
            capacity_before: None,
            capacity_after: None,
            buff_id: Some(buff.id),
            retroactive: false,
        });
        pool_keys.push(format!("counterspell:{}", buff.id));
        remaining = 0;
    }

    for (buff_type, shared) in [("aegis", false), ("sanctuary", true), ("reprieve", false)] {
        if !shieldable || remaining <= 0 {
            break;
        }
        for buff in active_buffs(
            connection,
            request.victim_id,
            request.guild_id,
            buff_type,
            request.occurred_at,
            shared,
        )? {
            let (mut data, capacity, rate) = decode_pool(buff_type, buff.data.as_deref())?;
            if capacity <= 0 || rate <= 0.0 {
                persist_pool(connection, buff.id, &mut data, 0)?;
                continue;
            }
            let absorbed = remaining
                .min(capacity)
                .min(rate_absorption(remaining, rate));
            if absorbed <= 0 {
                continue;
            }
            let after = capacity - absorbed;
            persist_pool(connection, buff.id, &mut data, after)?;
            details.push(ProtectionDetail {
                source: buff_type.to_owned(),
                absorbed,
                rate_millionths: rate_to_millionths(rate),
                capacity_before: Some(capacity),
                capacity_after: Some(after),
                buff_id: Some(buff.id),
                retroactive: false,
            });
            pool_keys.push(format!("buff:{}", buff.id));
            remaining -= absorbed;
            if remaining <= 0 {
                break;
            }
        }
    }

    if shieldable && remaining > 0 {
        let capacity = connection
            .query_row(
                "SELECT white_shield_remaining FROM player_mana
                 WHERE discord_id=?1 AND guild_id=?2 AND current_land='Plains'
                   AND assigned_date=?3 AND consumed_today=0",
                params![request.victim_id, request.guild_id, request.mana_date],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten()
            .unwrap_or_default();
        if capacity > 0 {
            let absorbed = capacity.min(rate_absorption(remaining, 0.5));
            let after = capacity - absorbed;
            connection.execute(
                "UPDATE player_mana SET white_shield_remaining=?1,updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2 AND guild_id=?3",
                params![after, request.victim_id, request.guild_id],
            )?;
            details.push(ProtectionDetail {
                source: "guardian".to_owned(),
                absorbed,
                rate_millionths: 500_000,
                capacity_before: Some(capacity),
                capacity_after: Some(after),
                buff_id: None,
                retroactive: false,
            });
            pool_keys.push(format!("guardian:{}", request.mana_date));
            remaining -= absorbed;
        }
    }

    let applied = remaining;
    let absorbed = attempted - applied;
    let victim_after = victim_before - applied;
    let destination_after = destination_before.map(|balance| balance.saturating_add(applied));
    let details_json = details_to_json(&details).to_string();
    let metadata = (!request.metadata.is_null()).then(|| request.metadata.to_string());
    connection.execute(
        "INSERT INTO hostile_loss_events (
             guild_id,victim_id,actor_id,event_key,kind,destination,recipient_id,
             requested,attempted,absorbed,applied,victim_balance_before,
             victim_balance_after,destination_balance_before,destination_balance_after,
             shieldable,retro_covered,protection_details,metadata,occurred_at,created_at
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,0,?17,?18,?19,?19)",
        params![
            request.guild_id,
            request.victim_id,
            request.actor_id,
            request.event_key,
            request.kind,
            request.destination.as_str(),
            request.recipient_id,
            request.requested,
            attempted,
            absorbed,
            applied,
            victim_before,
            victim_after,
            destination_before,
            destination_after,
            i64::from(shieldable),
            details_json,
            metadata,
            request.occurred_at
        ],
    )?;
    let event_id = connection.last_insert_rowid();
    for (detail, pool_key) in details.iter().zip(pool_keys.iter()) {
        insert_protection_event(
            connection,
            event_id,
            request.guild_id,
            request.victim_id,
            detail,
            pool_key,
            request.occurred_at,
            None,
        )?;
    }
    if applied > 0 {
        let ledger_metadata = json!({
            "kind": request.kind,
            "event_key": request.event_key,
            "requested": request.requested,
            "attempted": attempted,
            "absorbed": absorbed,
            "applied": applied,
            "destination": request.destination.as_str(),
            "recipient_id": request.recipient_id,
        });
        let ledger_source = if request.kind == "wheel_loss" {
            "gamba"
        } else {
            "hostile_loss"
        };
        set_ledger_context(
            connection,
            Some(ledger_source),
            request.actor_id,
            Some("hostile_loss_event"),
            Some(&event_id.to_string()),
            Some(&format!("{} hostile loss", request.kind)),
            Some(&ledger_metadata),
        )?;
        let victim_changed = connection.execute(
            "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3",
            params![victim_after, request.victim_id, request.guild_id],
        );
        if victim_changed? != 1 {
            return Err(ShopRuntimeRepositoryError::MissingPlayer {
                discord_id: request.victim_id,
                guild_id: request.guild_id,
            });
        }
        update_lowest_balance(connection, request.victim_id, request.guild_id)?;
        match request.destination {
            HostileDestination::Burn => {}
            HostileDestination::Player => {
                let recipient = request
                    .recipient_id
                    .ok_or(ShopRuntimeRepositoryError::MissingRecipient)?;
                if connection.execute(
                    "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
                     WHERE discord_id=?2 AND guild_id=?3",
                    params![destination_after, recipient, request.guild_id],
                )? != 1
                {
                    return Err(ShopRuntimeRepositoryError::MissingPlayer {
                        discord_id: recipient,
                        guild_id: request.guild_id,
                    });
                }
            }
            HostileDestination::Reserve => {
                connection.execute(
                    "INSERT INTO nonprofit_fund(guild_id,total_collected,updated_at)
                     VALUES (?1,?2,CURRENT_TIMESTAMP)
                     ON CONFLICT(guild_id) DO UPDATE SET
                       total_collected=excluded.total_collected,
                       updated_at=CURRENT_TIMESTAMP",
                    params![request.guild_id, destination_after],
                )?;
            }
        }
        clear_ledger_context(connection)?;
    }
    Ok(HostileLossSettlement {
        event_id,
        event_key: request.event_key.clone(),
        requested: request.requested,
        attempted,
        absorbed,
        applied,
        victim_balance_before: victim_before,
        victim_balance_after: victim_after,
        destination_balance_before: destination_before,
        destination_balance_after: destination_after,
        duplicate: false,
        details,
    })
}

#[derive(Clone, Debug)]
struct ActiveBuff {
    id: i64,
    data: Option<String>,
}

fn active_buffs(
    connection: &Connection,
    victim_id: i64,
    guild_id: i64,
    buff_type: &str,
    now: i64,
    shared: bool,
) -> Result<Vec<ActiveBuff>, rusqlite::Error> {
    let (sql, values) = if shared {
        (
            "SELECT id,data FROM manashop_buffs
             WHERE (discord_id=?1 OR target_id=?1) AND guild_id=?2
               AND buff_type=?3 AND triggered=0 AND expires_at>?4
             ORDER BY expires_at,granted_at,id",
            params![victim_id, guild_id, buff_type, now],
        )
    } else {
        (
            "SELECT id,data FROM manashop_buffs
             WHERE discord_id=?1 AND guild_id=?2 AND buff_type=?3
               AND triggered=0 AND expires_at>?4
             ORDER BY expires_at,granted_at,id",
            params![victim_id, guild_id, buff_type, now],
        )
    };
    let mut statement = connection.prepare(sql)?;
    statement
        .query_map(values, |row| {
            Ok(ActiveBuff {
                id: row.get(0)?,
                data: row.get(1)?,
            })
        })?
        .collect()
}

fn decode_pool(
    buff_type: &str,
    raw: Option<&str>,
) -> Result<(Value, i64, f64), ShopRuntimeRepositoryError> {
    let (default_capacity, default_rate) = match buff_type {
        "reprieve" => (25, 0.5),
        "aegis" => (75, 1.0),
        "sanctuary" => (150, 1.0),
        _ => return Err(ShopRuntimeRepositoryError::InvalidProtectionPool),
    };
    let mut data = match raw {
        Some(value) => serde_json::from_str::<Value>(value)
            .map_err(|error| ShopRuntimeRepositoryError::InvalidJson(error.to_string()))?,
        None => json!({}),
    };
    if !data.is_object() {
        data = json!({});
    }
    let capacity = data
        .get("capacity_remaining")
        .and_then(Value::as_i64)
        .unwrap_or(default_capacity)
        .max(0);
    let rate = data
        .get("rate")
        .and_then(Value::as_f64)
        .unwrap_or(default_rate)
        .clamp(0.0, 1.0);
    Ok((data, capacity, rate))
}

fn persist_pool(
    connection: &Connection,
    buff_id: i64,
    data: &mut Value,
    remaining: i64,
) -> Result<(), rusqlite::Error> {
    data["capacity_remaining"] = Value::from(remaining);
    connection.execute(
        "UPDATE manashop_buffs SET data=?1,triggered=?2 WHERE id=?3",
        params![data.to_string(), i64::from(remaining <= 0), buff_id],
    )?;
    Ok(())
}

fn rate_absorption(amount: i64, rate: f64) -> i64 {
    if amount <= 0 || rate <= 0.0 {
        0
    } else if rate >= 1.0 {
        amount
    } else {
        ((amount as f64 * rate).floor() as i64).max(1)
    }
}

fn rate_to_millionths(rate: f64) -> i64 {
    (rate * 1_000_000.0).round() as i64
}

fn details_to_json(details: &[ProtectionDetail]) -> Value {
    Value::Array(
        details
            .iter()
            .map(|detail| {
                json!({
                    "source": detail.source,
                    "absorbed": detail.absorbed,
                    "rate": detail.rate(),
                    "capacity_before": detail.capacity_before,
                    "capacity_after": detail.capacity_after,
                    "buff_id": detail.buff_id,
                    "retroactive": detail.retroactive,
                })
            })
            .collect(),
    )
}

fn details_from_json(raw: &str) -> Vec<ProtectionDetail> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| {
            Some(ProtectionDetail {
                source: value.get("source")?.as_str()?.to_owned(),
                absorbed: value.get("absorbed").and_then(Value::as_i64).unwrap_or(0),
                rate_millionths: rate_to_millionths(
                    value.get("rate").and_then(Value::as_f64).unwrap_or(0.0),
                ),
                capacity_before: value.get("capacity_before").and_then(Value::as_i64),
                capacity_after: value.get("capacity_after").and_then(Value::as_i64),
                buff_id: value.get("buff_id").and_then(Value::as_i64),
                retroactive: value
                    .get("retroactive")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

fn find_hostile_event(
    connection: &Connection,
    guild_id: i64,
    victim_id: i64,
    event_key: &str,
) -> Result<Option<HostileLossSettlement>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT event_id,event_key,requested,attempted,absorbed,applied,
                    victim_balance_before,victim_balance_after,
                    destination_balance_before,destination_balance_after,
                    protection_details
             FROM hostile_loss_events
             WHERE guild_id=?1 AND victim_id=?2 AND event_key=?3",
            params![guild_id, victim_id, event_key],
            |row| {
                let details: String = row.get(10)?;
                Ok(HostileLossSettlement {
                    event_id: row.get(0)?,
                    event_key: row.get(1)?,
                    requested: row.get(2)?,
                    attempted: row.get(3)?,
                    absorbed: row.get(4)?,
                    applied: row.get(5)?,
                    victim_balance_before: row.get(6)?,
                    victim_balance_after: row.get(7)?,
                    destination_balance_before: row.get(8)?,
                    destination_balance_after: row.get(9)?,
                    duplicate: false,
                    details: details_from_json(&details),
                })
            },
        )
        .optional()
}

#[allow(clippy::too_many_arguments)]
fn insert_protection_event(
    connection: &Connection,
    hostile_event_id: i64,
    guild_id: i64,
    victim_id: i64,
    detail: &ProtectionDetail,
    pool_key: &str,
    created_at: i64,
    extra: Option<&Value>,
) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT INTO mana_protection_events (
             hostile_loss_event_id,guild_id,victim_id,protection_type,pool_key,
             buff_id,amount,rate,capacity_before,capacity_after,retroactive,
             created_at,details
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![
            hostile_event_id,
            guild_id,
            victim_id,
            detail.source,
            pool_key,
            detail.buff_id,
            detail.absorbed,
            detail.rate(),
            detail.capacity_before,
            detail.capacity_after,
            i64::from(detail.retroactive),
            created_at,
            extra.map(Value::to_string)
        ],
    )?;
    Ok(())
}

#[derive(Clone, Debug)]
struct RetroEvent {
    event_id: i64,
    event_key: String,
    kind: String,
    applied: i64,
    retro_covered: i64,
}

fn eligible_retro_events(
    connection: &Connection,
    victim_id: i64,
    guild_id: i64,
    since_ts: i64,
    through_ts: i64,
    pool_key: &str,
) -> Result<Vec<RetroEvent>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT h.event_id,h.event_key,h.kind,h.applied,h.retro_covered
         FROM hostile_loss_events h
         WHERE h.guild_id=?1 AND h.victim_id=?2 AND h.shieldable=1
           AND h.occurred_at>=?3 AND h.occurred_at<=?4
           AND h.applied>h.retro_covered
           AND NOT EXISTS (
             SELECT 1 FROM mana_protection_events p
             WHERE p.hostile_loss_event_id=h.event_id AND p.pool_key=?5)
         ORDER BY h.occurred_at,h.event_id",
    )?;
    statement
        .query_map(
            params![guild_id, victim_id, since_ts, through_ts, pool_key],
            |row| {
                Ok(RetroEvent {
                    event_id: row.get(0)?,
                    event_key: row.get(1)?,
                    kind: row.get(2)?,
                    applied: row.get(3)?,
                    retro_covered: row.get(4)?,
                })
            },
        )?
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn credit_retro_event(
    connection: &Connection,
    event: &RetroEvent,
    victim_id: i64,
    guild_id: i64,
    amount: i64,
    detail: &ProtectionDetail,
    pool_key: &str,
    now: i64,
) -> Result<(), ShopRuntimeRepositoryError> {
    let metadata = json!({
        "event_key": event.event_key,
        "kind": event.kind,
        "amount": amount,
        "protection_type": detail.source,
        "pool_key": pool_key,
    });
    set_ledger_context(
        connection,
        Some("mana_protection"),
        None,
        Some("hostile_loss_event"),
        Some(&event.event_id.to_string()),
        Some(&format!("retroactive {} reimbursement", detail.source)),
        Some(&metadata),
    )?;
    let changed = connection.execute(
        "UPDATE players SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                updated_at=CURRENT_TIMESTAMP
         WHERE discord_id=?2 AND guild_id=?3",
        params![amount, victim_id, guild_id],
    );
    clear_ledger_context(connection)?;
    if changed? != 1 {
        return Err(ShopRuntimeRepositoryError::MissingPlayer {
            discord_id: victim_id,
            guild_id,
        });
    }
    connection.execute(
        "UPDATE hostile_loss_events SET retro_covered=retro_covered+?1 WHERE event_id=?2",
        params![amount, event.event_id],
    )?;
    insert_protection_event(
        connection,
        event.event_id,
        guild_id,
        victim_id,
        detail,
        pool_key,
        now,
        Some(&json!({"event_key": event.event_key})),
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "shop_runtime/tests.rs"]
mod tests;
