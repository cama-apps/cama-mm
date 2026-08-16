//! Exact-once post-Dig Blood Pact settlement.
//!
//! The Dig wallet/tunnel transaction deliberately commits before this effect,
//! matching Python's live ordering. This repository owns the *second* durable
//! boundary: the Blood Pact cap reservation and the protection-aware hostile
//! transfer are committed together so a process stop cannot consume cap
//! without also recording and applying the transfer.

use std::path::{Path, PathBuf};

use cama_domain::economy_scaling::{DEFAULT_MINIGAME_JC_DELTA_SCALE, scale_minigame_jc_delta};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use thiserror::Error;

use crate::manashop_rework_repository::ManashopRepository;
use crate::open_runtime_connection;
use crate::shop_runtime::{
    HostileDestination, HostileLossRequest, HostileLossSettlement, ShopRuntimeRepositoryError,
    apply_hostile_loss_in, validate_hostile_request,
};

#[derive(Debug, Error)]
pub enum DigBloodPactRepositoryError {
    #[error("Dig Blood Pact event key is required")]
    MissingEventKey,
    #[error("Dig Blood Pact scale must be finite and positive")]
    InvalidScale,
    #[error("Dig Blood Pact event key already has a different payload")]
    EventPayloadConflict,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Shop(#[from] ShopRuntimeRepositoryError),
}

/// The durable input for one post-commit Dig Blood Pact effect.
#[derive(Clone, Debug)]
pub struct DigBloodPactSettlementRequest {
    pub target_id: i64,
    pub guild_id: i64,
    pub earning: i64,
    pub event_key: String,
    pub minigame_jc_delta_scale: f64,
    pub occurred_at: i64,
    pub mana_date: String,
}

impl DigBloodPactSettlementRequest {
    #[must_use]
    pub fn with_default_scale(
        target_id: i64,
        guild_id: i64,
        earning: i64,
        event_key: impl Into<String>,
        occurred_at: i64,
        mana_date: impl Into<String>,
    ) -> Self {
        Self {
            target_id,
            guild_id,
            earning,
            event_key: event_key.into(),
            minigame_jc_delta_scale: DEFAULT_MINIGAME_JC_DELTA_SCALE,
            occurred_at,
            mana_date: mana_date.into(),
        }
    }
}

/// Receipt returned by the atomic Blood Pact boundary.
///
/// `event` is absent when the earning is nonpositive or no active pact targets
/// the Dig player. A retry returns the original hostile-loss receipt with
/// `duplicate=true`; it never reserves more cap or moves another balance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigBloodPactSettlement {
    pub event_key: String,
    pub event: Option<HostileLossSettlement>,
    pub buff_id: Option<i64>,
    pub skimmer_id: Option<i64>,
    /// Capacity reserved while the protection-aware transfer was attempted.
    pub reserved_amount: i64,
    /// Amount presented to the hostile-loss policy after minigame scaling.
    pub attempted_amount: i64,
    /// Amount actually transferred after protection absorption.
    pub applied_amount: i64,
    pub duplicate: bool,
}

impl DigBloodPactSettlement {
    fn noop(event_key: String) -> Self {
        Self {
            event_key,
            event: None,
            buff_id: None,
            skimmer_id: None,
            reserved_amount: 0,
            attempted_amount: 0,
            applied_amount: 0,
            duplicate: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DigBloodPactRepository {
    path: PathBuf,
}

impl DigBloodPactRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Reserve, protect, transfer, and account one Dig Blood Pact effect in a
    /// single `BEGIN IMMEDIATE` transaction.
    pub fn settle(
        &self,
        request: &DigBloodPactSettlementRequest,
    ) -> Result<DigBloodPactSettlement, DigBloodPactRepositoryError> {
        if request.earning <= 0 {
            return Ok(DigBloodPactSettlement::noop(request.event_key.clone()));
        }
        if request.event_key.trim().is_empty() {
            return Err(DigBloodPactRepositoryError::MissingEventKey);
        }
        if !request.minigame_jc_delta_scale.is_finite() || request.minigame_jc_delta_scale <= 0.0 {
            return Err(DigBloodPactRepositoryError::InvalidScale);
        }

        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(existing) = existing_blood_pact_event(
            &transaction,
            request.guild_id,
            request.target_id,
            &request.event_key,
        )? {
            if existing.earning != Some(request.earning) {
                return Err(DigBloodPactRepositoryError::EventPayloadConflict);
            }
            let hostile_request = HostileLossRequest {
                victim_id: request.target_id,
                guild_id: request.guild_id,
                requested: existing.requested,
                kind: "blood_pact".to_owned(),
                actor_id: existing.actor_id,
                event_key: request.event_key.clone(),
                destination: HostileDestination::Player,
                recipient_id: existing.recipient_id,
                clamp_to_balance: false,
                min_balance: None,
                protect_self: false,
                metadata: Value::Null,
                occurred_at: request.occurred_at,
                mana_date: request.mana_date.clone(),
            };
            validate_hostile_request(&hostile_request)?;
            let event = apply_hostile_loss_in(&transaction, &hostile_request)?;
            transaction.commit()?;
            return Ok(DigBloodPactSettlement {
                event_key: request.event_key.clone(),
                buff_id: existing.buff_id,
                skimmer_id: existing.actor_id,
                reserved_amount: existing.reserved_amount,
                attempted_amount: event.requested,
                applied_amount: event.applied,
                duplicate: true,
                event: Some(event),
            });
        }

        let Some(claim) = ManashopRepository::claim_blood_pact_skim_in(
            &transaction,
            request.target_id,
            request.guild_id,
            request.earning,
            request.occurred_at,
        )?
        else {
            transaction.commit()?;
            return Ok(DigBloodPactSettlement::noop(request.event_key.clone()));
        };

        let scaled = scale_minigame_jc_delta(claim.amount as f64, request.minigame_jc_delta_scale);
        let (reserved_amount, attempted_amount) = if scaled > claim.amount {
            let extra = ManashopRepository::claim_blood_pact_extra_in(
                &transaction,
                claim.buff_id,
                scaled.saturating_sub(claim.amount),
            )?;
            let accounted = claim.amount.saturating_add(extra);
            (accounted, accounted)
        } else {
            (claim.amount, scaled)
        };

        let hostile_request = HostileLossRequest {
            victim_id: request.target_id,
            guild_id: request.guild_id,
            requested: attempted_amount,
            kind: "blood_pact".to_owned(),
            actor_id: Some(claim.skimmer_id),
            event_key: request.event_key.clone(),
            destination: HostileDestination::Player,
            recipient_id: Some(claim.skimmer_id),
            clamp_to_balance: false,
            min_balance: None,
            protect_self: false,
            metadata: json!({
                "origin": "dig",
                "earning": request.earning,
                "buff_id": claim.buff_id,
                "reserved_amount": reserved_amount,
                "skimmer_id": claim.skimmer_id,
            }),
            occurred_at: request.occurred_at,
            mana_date: request.mana_date.clone(),
        };
        validate_hostile_request(&hostile_request)?;
        let event = apply_hostile_loss_in(&transaction, &hostile_request)?;
        ManashopRepository::settle_blood_pact_skim_in(
            &transaction,
            claim.buff_id,
            reserved_amount,
            event.applied,
        )?;
        transaction.commit()?;

        Ok(DigBloodPactSettlement {
            event_key: request.event_key.clone(),
            event: Some(event.clone()),
            buff_id: Some(claim.buff_id),
            skimmer_id: Some(claim.skimmer_id),
            reserved_amount,
            attempted_amount: event.requested,
            applied_amount: event.applied,
            duplicate: false,
        })
    }
}

#[derive(Clone, Debug)]
struct ExistingBloodPactEvent {
    requested: i64,
    earning: Option<i64>,
    actor_id: Option<i64>,
    recipient_id: Option<i64>,
    buff_id: Option<i64>,
    reserved_amount: i64,
}

fn existing_blood_pact_event(
    connection: &Connection,
    guild_id: i64,
    target_id: i64,
    event_key: &str,
) -> Result<Option<ExistingBloodPactEvent>, DigBloodPactRepositoryError> {
    let row = connection
        .query_row(
            "SELECT requested,kind,destination,actor_id,recipient_id,metadata
             FROM hostile_loss_events
             WHERE guild_id=?1 AND victim_id=?2 AND event_key=?3",
            params![guild_id, target_id, event_key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((requested, kind, destination, actor_id, recipient_id, metadata)) = row else {
        return Ok(None);
    };
    if kind != "blood_pact" || destination != "player" {
        return Err(DigBloodPactRepositoryError::EventPayloadConflict);
    }
    let metadata = metadata
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .unwrap_or_else(|| json!({}));
    Ok(Some(ExistingBloodPactEvent {
        requested,
        earning: metadata.get("earning").and_then(Value::as_i64),
        actor_id,
        recipient_id,
        buff_id: metadata.get("buff_id").and_then(Value::as_i64),
        reserved_amount: metadata
            .get("reserved_amount")
            .and_then(Value::as_i64)
            .unwrap_or(requested),
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;

    use rusqlite::{Connection, params};
    use tempfile::NamedTempFile;

    use crate::core_repositories::{NewPlayer, PlayerRepository};
    use crate::schema_manager::initialize_or_migrate;

    use super::*;

    const GUILD: i64 = 81_001;
    const TARGET: i64 = 81_002;
    const SKIMMER: i64 = 81_003;
    const MISSING_SKIMMER: i64 = 81_004;
    const NOW: i64 = 2_000_000_000;
    const MANA_DATE: &str = "2033-05-18";

    struct Fixture {
        database: NamedTempFile,
        repository: DigBloodPactRepository,
    }

    impl Fixture {
        fn new(target_balance: i64, skimmer_balance: i64) -> Self {
            let database = NamedTempFile::new().expect("temporary Dig Blood Pact database");
            initialize_or_migrate(database.path()).expect("migrate canonical schema");
            let players = PlayerRepository::new(database.path());
            players
                .add(&NewPlayer::new(TARGET, "target", Some(GUILD)))
                .expect("target player");
            players
                .add(&NewPlayer::new(SKIMMER, "skimmer", Some(GUILD)))
                .expect("skimmer player");
            let connection = Connection::open(database.path()).expect("open migrated database");
            connection
                .execute(
                    "UPDATE players SET jopacoin_balance=?1
                     WHERE discord_id=?2 AND guild_id=?3",
                    params![target_balance, TARGET, GUILD],
                )
                .expect("target balance");
            connection
                .execute(
                    "UPDATE players SET jopacoin_balance=?1
                     WHERE discord_id=?2 AND guild_id=?3",
                    params![skimmer_balance, SKIMMER, GUILD],
                )
                .expect("skimmer balance");
            Self {
                repository: DigBloodPactRepository::new(database.path()),
                database,
            }
        }

        fn connection(&self) -> Connection {
            Connection::open(self.database.path()).expect("open fixture connection")
        }

        fn grant_pact(&self, cap: i64, rate: f64) {
            self.connection()
                .execute(
                    "INSERT INTO manashop_buffs(
                         discord_id,guild_id,buff_type,target_id,granted_at,expires_at,
                         triggered,data
                     ) VALUES(?1,?2,'blood_pact',?3,?4,?5,0,?6)",
                    params![
                        SKIMMER,
                        GUILD,
                        TARGET,
                        NOW - 1,
                        NOW + 86_400,
                        json!({
                            "skimmed_total": 0,
                            "cap": cap,
                            "skim_rate": rate,
                        })
                        .to_string(),
                    ],
                )
                .expect("grant Blood Pact");
        }

        fn request(&self, key: &str, earning: i64) -> DigBloodPactSettlementRequest {
            DigBloodPactSettlementRequest::with_default_scale(
                TARGET, GUILD, earning, key, NOW, MANA_DATE,
            )
        }

        fn balances(&self) -> (i64, i64) {
            let connection = self.connection();
            let target = connection
                .query_row(
                    "SELECT jopacoin_balance FROM players
                     WHERE discord_id=?1 AND guild_id=?2",
                    params![TARGET, GUILD],
                    |row| row.get(0),
                )
                .expect("target balance");
            let skimmer = connection
                .query_row(
                    "SELECT jopacoin_balance FROM players
                     WHERE discord_id=?1 AND guild_id=?2",
                    params![SKIMMER, GUILD],
                    |row| row.get(0),
                )
                .expect("skimmer balance");
            (target, skimmer)
        }

        fn skimmed_total(&self) -> i64 {
            self.connection()
                .query_row(
                    "SELECT json_extract(data,'$.skimmed_total')
                     FROM manashop_buffs
                     WHERE guild_id=?1 AND target_id=?2 AND buff_type='blood_pact'",
                    params![GUILD, TARGET],
                    |row| row.get(0),
                )
                .expect("Blood Pact cap")
        }
    }

    #[test]
    fn ordinary_settlement_moves_balances_and_writes_ledger_audit() {
        let fixture = Fixture::new(100, 0);
        fixture.grant_pact(150, 0.25);

        let result = fixture
            .repository
            .settle(&fixture.request("dig:81_002:blood-pact:1", 100))
            .expect("settle Blood Pact");

        assert!(!result.duplicate);
        assert_eq!(result.reserved_amount, 25);
        assert_eq!(result.attempted_amount, 25);
        assert_eq!(result.applied_amount, 25);
        assert_eq!(fixture.balances(), (75, 25));
        assert_eq!(fixture.skimmed_total(), 25);
        assert_eq!(
            fixture
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM hostile_loss_events
                     WHERE guild_id=?1 AND victim_id=?2 AND event_key=?3",
                    params![GUILD, TARGET, "dig:81_002:blood-pact:1"],
                    |row| row.get::<_, i64>(0),
                )
                .expect("hostile event count"),
            1
        );
        assert_eq!(
            fixture
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE guild_id=?1 AND source='hostile_loss'",
                    params![GUILD],
                    |row| row.get::<_, i64>(0),
                )
                .expect("ledger count"),
            2
        );
    }

    #[test]
    fn retry_returns_first_receipt_without_second_cap_or_transfer() {
        let fixture = Fixture::new(100, 0);
        fixture.grant_pact(150, 0.25);
        let request = fixture.request("dig:81_002:blood-pact:retry", 100);

        let first = fixture.repository.settle(&request).expect("first settle");
        let second = fixture.repository.settle(&request).expect("retry settle");

        assert!(!first.duplicate);
        assert!(second.duplicate);
        assert_eq!(
            second.event.as_ref().map(|event| event.event_id),
            first.event.as_ref().map(|event| event.event_id)
        );
        assert_eq!(
            second.event.as_ref().map(|event| event.event_key.as_str()),
            first.event.as_ref().map(|event| event.event_key.as_str())
        );
        assert_eq!(
            second.event.as_ref().map(|event| event.applied),
            first.event.as_ref().map(|event| event.applied)
        );
        assert_eq!(second.applied_amount, first.applied_amount);
        assert_eq!(fixture.balances(), (75, 25));
        assert_eq!(fixture.skimmed_total(), 25);
        assert_eq!(
            fixture
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM hostile_loss_events
                     WHERE guild_id=?1 AND victim_id=?2",
                    params![GUILD, TARGET],
                    |row| row.get::<_, i64>(0),
                )
                .expect("hostile event count"),
            1
        );
    }

    #[test]
    fn reused_event_key_rejects_a_different_earning_payload() {
        let fixture = Fixture::new(100, 0);
        fixture.grant_pact(150, 0.25);
        let event_key = "dig:81_002:blood-pact:payload";
        fixture
            .repository
            .settle(&fixture.request(event_key, 100))
            .expect("first settlement");

        let error = fixture
            .repository
            .settle(&fixture.request(event_key, 80))
            .expect_err("changed earning must not reuse the receipt");
        assert!(matches!(
            error,
            DigBloodPactRepositoryError::EventPayloadConflict
        ));
        assert_eq!(fixture.balances(), (75, 25));
        assert_eq!(fixture.skimmed_total(), 25);
    }

    #[test]
    fn aegis_absorbs_transfer_and_only_applied_amount_consumes_cap() {
        let fixture = Fixture::new(100, 0);
        fixture.grant_pact(150, 0.25);
        fixture
            .connection()
            .execute(
                "INSERT INTO manashop_buffs(
                     discord_id,guild_id,buff_type,granted_at,expires_at,triggered,data
                 ) VALUES(?1,?2,'aegis',?3,?4,0,?5)",
                params![
                    TARGET,
                    GUILD,
                    NOW - 1,
                    NOW + 86_400,
                    r#"{"capacity":75,"capacity_remaining":75,"rate":1.0}"#,
                ],
            )
            .expect("grant Aegis");

        let result = fixture
            .repository
            .settle(&fixture.request("dig:81_002:blood-pact:aegis", 400))
            .expect("settle protected Blood Pact");

        assert_eq!(result.attempted_amount, 100);
        assert_eq!(result.applied_amount, 25);
        assert_eq!(fixture.balances(), (75, 25));
        assert_eq!(fixture.skimmed_total(), 25);
        assert_eq!(
            fixture
                .connection()
                .query_row(
                    "SELECT json_extract(data,'$.capacity_remaining')
                     FROM manashop_buffs WHERE guild_id=?1 AND discord_id=?2
                       AND buff_type='aegis'",
                    params![GUILD, TARGET],
                    |row| row.get::<_, i64>(0),
                )
                .expect("Aegis capacity"),
            0
        );
        assert_eq!(
            fixture
                .connection()
                .query_row("SELECT COUNT(*) FROM mana_protection_events", [], |row| row
                    .get::<_, i64>(0),)
                .expect("protection audit count"),
            1
        );
    }

    #[test]
    fn scaled_and_partial_cap_accounting_matches_python_policy() {
        let fixture = Fixture::new(500, 0);
        fixture.grant_pact(150, 0.25);
        let mut request = fixture.request("dig:81_002:blood-pact:scale", 100);
        request.minigame_jc_delta_scale = 2.0;

        let first = fixture.repository.settle(&request).expect("scaled settle");
        assert_eq!(first.reserved_amount, 50);
        assert_eq!(first.attempted_amount, 50);
        assert_eq!(first.applied_amount, 50);
        assert_eq!(fixture.skimmed_total(), 50);
        assert_eq!(fixture.balances(), (450, 50));

        let second = fixture
            .repository
            .settle(&DigBloodPactSettlementRequest {
                event_key: "dig:81_002:blood-pact:scale-2".to_owned(),
                earning: 400,
                ..request.clone()
            })
            .expect("second scaled settle");
        assert_eq!(second.reserved_amount, 100);
        assert_eq!(second.attempted_amount, 100);
        assert_eq!(second.applied_amount, 100);
        assert_eq!(fixture.skimmed_total(), 150);
        assert_eq!(fixture.balances(), (350, 150));

        let exhausted = fixture
            .repository
            .settle(&DigBloodPactSettlementRequest {
                event_key: "dig:81_002:blood-pact:scale-3".to_owned(),
                ..request
            })
            .expect("exhausted cap is a no-op");
        assert!(exhausted.event.is_none());
        assert_eq!(fixture.skimmed_total(), 150);
    }

    #[test]
    fn no_pact_and_nonpositive_earning_are_no_ops() {
        let fixture = Fixture::new(100, 0);
        let no_target = fixture
            .repository
            .settle(&fixture.request("dig:81_002:blood-pact:none", 100))
            .expect("missing pact is a no-op");
        let no_earning = fixture
            .repository
            .settle(&fixture.request("dig:81_002:blood-pact:zero", 0))
            .expect("nonpositive earning is a no-op");

        assert!(no_target.event.is_none());
        assert!(no_earning.event.is_none());
        assert_eq!(fixture.balances(), (100, 0));
        assert_eq!(
            fixture
                .connection()
                .query_row("SELECT COUNT(*) FROM hostile_loss_events", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("hostile event count"),
            0
        );
    }

    #[test]
    fn no_funds_still_uses_python_nonclamped_transfer_semantics() {
        let fixture = Fixture::new(0, 0);
        fixture.grant_pact(150, 0.25);

        let result = fixture
            .repository
            .settle(&fixture.request("dig:81_002:blood-pact:no-funds", 100))
            .expect("nonclamped settlement");

        assert_eq!(result.applied_amount, 25);
        assert_eq!(fixture.balances(), (-25, 25));
        assert_eq!(fixture.skimmed_total(), 25);
    }

    #[test]
    fn failed_recipient_rolls_back_cap_and_hostile_audit() {
        let fixture = Fixture::new(100, 0);
        fixture
            .connection()
            .execute(
                "INSERT INTO manashop_buffs(
                     discord_id,guild_id,buff_type,target_id,granted_at,expires_at,
                     triggered,data
                 ) VALUES(?1,?2,'blood_pact',?3,?4,?5,0,?6)",
                params![
                    MISSING_SKIMMER,
                    GUILD,
                    TARGET,
                    NOW - 1,
                    NOW + 86_400,
                    r#"{"skimmed_total":0,"cap":150,"skim_rate":0.25}"#,
                ],
            )
            .expect("grant pact to missing recipient");

        let error = fixture
            .repository
            .settle(&fixture.request("dig:81_002:blood-pact:rollback", 100))
            .expect_err("missing recipient must roll back");
        assert!(matches!(
            error,
            DigBloodPactRepositoryError::Shop(ShopRuntimeRepositoryError::MissingPlayer { .. })
        ));
        assert_eq!(
            fixture
                .connection()
                .query_row(
                    "SELECT json_extract(data,'$.skimmed_total')
                     FROM manashop_buffs WHERE guild_id=?1 AND target_id=?2",
                    params![GUILD, TARGET],
                    |row| row.get::<_, i64>(0),
                )
                .expect("rolled-back cap"),
            0
        );
        assert_eq!(fixture.balances().0, 100);
        assert_eq!(
            fixture
                .connection()
                .query_row("SELECT COUNT(*) FROM hostile_loss_events", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("rolled-back hostile audit"),
            0
        );
    }

    #[test]
    fn injected_hostile_audit_failure_rolls_back_cap_and_balances() {
        let fixture = Fixture::new(100, 0);
        fixture.grant_pact(150, 0.25);
        fixture
            .connection()
            .execute_batch(
                "CREATE TRIGGER dig_blood_pact_test_abort
                 BEFORE INSERT ON hostile_loss_events
                 WHEN NEW.event_key='dig:81_002:blood-pact:injected-rollback'
                 BEGIN
                   SELECT RAISE(ABORT, 'injected Blood Pact audit failure');
                 END;",
            )
            .expect("install injected rollback trigger");

        let error = fixture
            .repository
            .settle(&fixture.request("dig:81_002:blood-pact:injected-rollback", 100))
            .expect_err("injected audit failure must roll back");
        assert!(matches!(
            error,
            DigBloodPactRepositoryError::Shop(ShopRuntimeRepositoryError::Sqlite(_))
        ));
        assert_eq!(fixture.balances(), (100, 0));
        assert_eq!(fixture.skimmed_total(), 0);
        assert_eq!(
            fixture
                .connection()
                .query_row("SELECT COUNT(*) FROM hostile_loss_events", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("hostile event count"),
            0
        );
    }

    #[test]
    fn concurrent_same_key_settles_once() {
        let fixture = Fixture::new(100, 0);
        fixture.grant_pact(150, 0.25);
        let path = fixture.database.path().to_path_buf();
        let request = Arc::new(fixture.request("dig:81_002:blood-pact:concurrent", 100));
        let barrier = Arc::new(Barrier::new(2));
        let handles = (0..2)
            .map(|_| {
                let path = path.clone();
                let request = Arc::clone(&request);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    DigBloodPactRepository::new(path).settle(&request)
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("settlement thread"))
            .collect::<Result<Vec<_>, _>>()
            .expect("concurrent settlements");

        assert_eq!(results.iter().filter(|result| !result.duplicate).count(), 1);
        assert_eq!(results.iter().filter(|result| result.duplicate).count(), 1);
        assert_eq!(fixture.balances(), (75, 25));
        assert_eq!(fixture.skimmed_total(), 25);
        assert_eq!(
            fixture
                .connection()
                .query_row("SELECT COUNT(*) FROM hostile_loss_events", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("hostile event count"),
            1
        );
    }
}
