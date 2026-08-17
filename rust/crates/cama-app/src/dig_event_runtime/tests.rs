//! Migrated-SQLite application tests for the canonical event runtime.

use crate::economy_event_service::EconomyEventConfig;
use crate::test_support::copy_migrated_database as initialize_or_migrate;
use cama_db::dig_event_runtime::{DigEventActorKey, DigEventRuntimeRepository};
use cama_db::economy_event_repository::{
    EconomyEventRepository, EventDirection, EventDraft, EventEffects,
};
use cama_domain::dig_gear::{GearService, GearSlot};
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const ACTOR: i64 = 91_001;
const GUILD: i64 = 42;
const NOW: i64 = 1_700_000_000;

struct Fixture {
    database: NamedTempFile,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary SQLite database");
        initialize_or_migrate(database.path()).expect("canonical migrated schema");
        Connection::open(database.path())
            .expect("open disposable event database")
            .pragma_update(None, "journal_mode", "MEMORY")
            .expect("use in-memory rollback journal");
        Self { database }
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(self.database.path()).expect("fixture connection");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match Python foreign-key behavior");
        connection
    }

    fn seed_actor(&self, discord_id: i64, balance: i64, depth: i64, prestige_level: i64) {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username, jopacoin_balance
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    discord_id,
                    GUILD,
                    format!("event-test-{discord_id}"),
                    balance
                ],
            )
            .expect("seed player");
        connection
            .execute(
                "INSERT INTO tunnels (
                     discord_id, guild_id, depth, max_depth, luminosity,
                     prestige_level, prestige_perks, boss_progress, streak_days
                 ) VALUES (?1, ?2, ?3, ?3, 100, ?4, '[]', '{}', 4)",
                params![discord_id, GUILD, depth, prestige_level],
            )
            .expect("seed tunnel");
    }

    fn mark_active_digger(&self, discord_id: i64) {
        self.connection()
            .execute(
                "INSERT INTO dig_actions (
                     guild_id, actor_id, target_id, action_type, depth_before,
                     depth_after, jc_delta, detail, created_at
                 ) VALUES (?1, ?2, NULL, 'dig', 1, 2, 0, '{}', ?3)",
                params![GUILD, discord_id, NOW - 30],
            )
            .expect("seed active dig action");
    }

    fn seed_event_dig_action(&self, event_id: &str) -> i64 {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO dig_actions (
                     guild_id, actor_id, target_id, action_type, depth_before,
                     depth_after, jc_delta, detail, created_at
                 ) VALUES (?1, ?2, NULL, 'dig', 49, 50, 1, ?3, ?4)",
                params![
                    GUILD,
                    ACTOR,
                    serde_json::json!({"event": event_id}).to_string(),
                    NOW - 1
                ],
            )
            .expect("seed event Dig action");
        connection.last_insert_rowid()
    }

    fn seed_legacy_event_dig_action(&self, event_id: &str) -> i64 {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO dig_actions (
                     guild_id, actor_id, target_id, action_type, depth_before,
                     depth_after, jc_delta, detail, created_at
                 ) VALUES (?1, ?2, NULL, 'dig', 24, 24, 0, ?3, ?4)",
                params![
                    GUILD,
                    ACTOR,
                    serde_json::json!({"event": event_id}).to_string(),
                    NOW - 1
                ],
            )
            .expect("seed legacy event Dig action");
        connection.last_insert_rowid()
    }

    fn balance(&self, discord_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                  WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, GUILD],
                |row| row.get(0),
            )
            .expect("player balance")
    }

    fn count_actions(&self, actor_id: i64, action_type: &str) -> i64 {
        self.connection()
            .query_row(
                "SELECT COUNT(*) FROM dig_actions
                  WHERE guild_id=?1 AND actor_id=?2 AND action_type=?3",
                params![GUILD, actor_id, action_type],
                |row| row.get(0),
            )
            .expect("action count")
    }

    fn snapshot(&self) -> DigEventActorSnapshot {
        DigEventRuntimeRepository::new(self.database.path())
            .actor_snapshot(DigEventActorKey {
                discord_id: ACTOR,
                guild_id: Some(GUILD),
            })
            .expect("actor snapshot")
            .expect("seeded actor")
    }

    fn service(&self) -> DigEventRuntimeService {
        let mut config = DigRuntimeConfig::default();
        config.economy_event.enabled = false;
        DigEventRuntimeService::sqlite_with_config(self.database.path(), config)
    }
}

fn request<'a>(
    event_id: &'a str,
    choice: &'a str,
    event_key: &'a str,
) -> DigRuntimeEventRequest<'a> {
    DigRuntimeEventRequest {
        discord_id: ACTOR,
        guild_id: GUILD,
        event_id,
        choice,
        event_key,
        now: NOW,
        chained: false,
    }
}

#[test]
fn event_service_keeps_live_runtime_config_without_an_extra_finale_tax_hook() {
    let fixture = Fixture::new();
    let config = DigRuntimeConfig::default().with_runtime_policy(
        0.5,
        EconomyEventConfig {
            enabled: true,
            normal_annual_rate: 0.12,
            inflation_ceiling: 0.4,
            lookback_days: 9,
            max_reserve_burn_pct: 0.2,
            max_wallet_burn_pct: 0.1,
            trigger_hour_local: 4,
        },
    );
    let service = DigEventRuntimeService::sqlite_with_config_and_finale_tax_hook(
        fixture.database.path(),
        config.clone(),
        None,
    );
    assert_eq!(service.config().minigame_jc_delta_scale, 0.5);
    assert_eq!(
        service.config().economy_event.normal_annual_rate,
        config.economy_event.normal_annual_rate
    );
    assert_eq!(
        service.config().economy_event.trigger_hour_local,
        config.economy_event.trigger_hour_local
    );
    // Quest finales apply the built-in ManaRepository policy. A provider must
    // pass this configured service through rather than constructing a second
    // default event service (and must not bind an unrelated second tax_fn).
    assert!(service.finale_tax_hook.is_none());
}

fn key_for_outcome(
    snapshot: &DigEventActorSnapshot,
    event_id: &str,
    choice: &str,
    succeeded: bool,
) -> String {
    for index in 0..10_000 {
        let key = format!("event-test:{event_id}:{choice}:{index}");
        let event_request = request(event_id, choice, &key);
        let policy = event_policy(snapshot, false, 1.0);
        let mut entropy = SeededLootEntropy::new(event_seed(event_request));
        let mut rolls = CanonicalEventRolls::default();
        if canonical_event_needs_cruel_echo_roll(event_id, choice, policy) {
            rolls.cruel_echo_roll = Some(entropy.unit());
        }
        rolls.success_roll = entropy.unit();
        let resolution = resolve_canonical_event_with_policy(event_id, choice, rolls, policy)
            .expect("canonical outcome while finding deterministic key");
        if resolution.succeeded == succeeded {
            return key;
        }
    }
    panic!("no deterministic key found for {event_id}:{choice}:{succeeded}");
}

#[test]
fn migrated_safe_event_commits_full_actor_result_once() {
    let fixture = Fixture::new();
    fixture.seed_actor(ACTOR, 100, 50, 0);
    let service = fixture.service();
    let event_request = request("underground_stream", "safe", "event:safe:1");

    let first = service.resolve_event(event_request).expect("first event");
    assert!(first.success);
    assert!(first.applied_now);
    assert_eq!(first.depth_before, 50);
    assert_eq!(first.depth_after, 50);
    let resolution = first.resolution.as_ref().expect("typed resolution");
    assert!(resolution.succeeded);
    assert_eq!(resolution.event_id, "underground_stream");
    assert_eq!(fixture.balance(ACTOR), 100 + resolution.jc);
    assert_eq!(fixture.count_actions(ACTOR, "event"), 1);

    let retry = service.resolve_event(event_request).expect("event retry");
    assert!(retry.success);
    assert!(!retry.applied_now);
    assert_eq!(retry.action_id, first.action_id);
    assert_eq!(retry.balance_after, first.balance_after);
    assert_eq!(fixture.count_actions(ACTOR, "event"), 1);
}

#[test]
fn test_positive_event_reward_is_scaled_once_and_audited() {
    let fixture = Fixture::new();
    fixture.seed_actor(ACTOR, 100, 50, 0);
    let service = fixture.service();
    let outcome = service
        .resolve_event(request(
            "underground_stream",
            "safe",
            "reward-policy:positive",
        ))
        .expect("positive event reward");
    let resolution = outcome.resolution.as_ref().expect("typed resolution");
    assert!(outcome.success && outcome.applied_now);
    assert_eq!(resolution.economy_gross_jc, 4);
    assert_eq!(
        resolution.jc,
        cama_domain::dig_economy::scale_positive_dig_jc(4)
    );
    assert_eq!(fixture.balance(ACTOR), 100 + resolution.jc);
    let detail: String = fixture
        .connection()
        .query_row(
            "SELECT detail FROM dig_actions WHERE id=?1",
            params![outcome.action_id.expect("event action")],
            |row| row.get(0),
        )
        .expect("event audit detail");
    let value: serde_json::Value = serde_json::from_str(&detail).expect("event detail JSON");
    assert_eq!(value["gross_jc"], 4);
    assert_eq!(value["reward_multiplier"], 0.65);
    assert_eq!(value["jc"], resolution.jc);
    let retry = service
        .resolve_event(request(
            "underground_stream",
            "safe",
            "reward-policy:positive",
        ))
        .expect("positive event retry");
    assert!(!retry.applied_now);
    assert_eq!(fixture.count_actions(ACTOR, "event"), 1);
}

#[test]
fn test_economy_event_reward_is_scaled_once_and_audited() {
    let fixture = Fixture::new();
    fixture.seed_actor(ACTOR, 100, 50, 0);
    let event_date =
        cama_domain::game_date::game_date_for_timestamp(NOW as f64).expect("event date");
    EconomyEventRepository::new(fixture.database.path())
        .activate_event_atomic(
            Some(GUILD),
            &EventDraft {
                event_date,
                name: "Double legacy reward".to_owned(),
                hero: "Earthshaker".to_owned(),
                direction: EventDirection::Neutral,
                severity: 1,
                target_effect_jc: 0,
                forecast_flow_jc: 0,
                expected_effect_jc: 0,
                monetary_stock_before: 0,
                effects: EventEffects {
                    reward_multiplier: 2.0,
                    ..EventEffects::default()
                },
                announcement: "Double legacy reward".to_owned(),
                starts_at: NOW - 60,
                ends_at: NOW + 60,
                created_at: NOW - 60,
            },
        )
        .expect("persist doubling economy event");
    let mut config = DigRuntimeConfig::default();
    config.economy_event.enabled = true;
    let service = DigEventRuntimeService::sqlite_with_config(fixture.database.path(), config);
    let event_key = key_for_outcome(&fixture.snapshot(), "techies_cache", "safe", true);
    let outcome = service
        .resolve_event(request("techies_cache", "safe", &event_key))
        .expect("legacy event reward");
    let resolution = outcome
        .resolution
        .as_ref()
        .expect("typed legacy resolution");
    assert!(outcome.success && outcome.applied_now);
    assert_eq!(resolution.economy_gross_jc, 4);
    assert_eq!(
        resolution.jc,
        cama_domain::dig_economy::scale_positive_dig_jc(8)
    );
    assert_eq!(fixture.balance(ACTOR), 100 + resolution.jc);
    let detail: String = fixture
        .connection()
        .query_row(
            "SELECT detail FROM dig_actions WHERE id=?1",
            params![outcome.action_id.expect("legacy event action")],
            |row| row.get(0),
        )
        .expect("legacy event audit detail");
    let value: serde_json::Value = serde_json::from_str(&detail).expect("legacy detail JSON");
    assert_eq!(value["gross_jc"], 4);
    assert_eq!(value["reward_multiplier"], 0.65);
    assert_eq!(value["jc"], resolution.jc);
    assert_eq!(fixture.count_actions(ACTOR, "event"), 1);
}

// tests/test_dig_reward_policy.py::test_legacy_event_reward_is_scaled_once_and_audited
#[test]
fn test_legacy_event_reward_is_scaled_once_and_audited() {
    let fixture = Fixture::new();
    fixture.seed_actor(ACTOR, 100, 24, 0);
    let dig_action_id = fixture.seed_legacy_event_dig_action("legacy_reward_policy_event");
    let event_date =
        cama_domain::game_date::game_date_for_timestamp(NOW as f64).expect("event date");
    EconomyEventRepository::new(fixture.database.path())
        .activate_event_atomic(
            Some(GUILD),
            &EventDraft {
                event_date,
                name: "Double legacy reward".to_owned(),
                hero: "Earthshaker".to_owned(),
                direction: EventDirection::Neutral,
                severity: 1,
                target_effect_jc: 0,
                forecast_flow_jc: 0,
                expected_effect_jc: 0,
                monetary_stock_before: 0,
                effects: EventEffects {
                    reward_multiplier: 2.0,
                    ..EventEffects::default()
                },
                announcement: "Double legacy reward".to_owned(),
                starts_at: NOW - 60,
                ends_at: NOW + 60,
                created_at: NOW - 60,
            },
        )
        .expect("persist doubling economy event");
    let mut config = DigRuntimeConfig::default();
    config.economy_event.enabled = true;
    let service = DigEventRuntimeService::sqlite_with_config(fixture.database.path(), config);
    let catalog = DigLegacyEventCatalog::from_json_str(
        &serde_json::json!([{
            "id": "legacy_reward_policy_event",
            "name": "Legacy Reward Policy Event",
            "outcomes": {
                "inspect": {
                    "message": "A compact reward.",
                    "jc": 15,
                    "advance": 0
                }
            }
        }])
        .to_string(),
    )
    .expect("parse legacy EVENT_POOL shape");
    let request = DigEventActionRequest {
        discord_id: ACTOR,
        guild_id: GUILD,
        dig_action_id,
        choice: "inspect",
        now: NOW,
    };
    let outcome = service
        .resolve_action_event_with_legacy_catalog(request, &catalog)
        .expect("legacy event reward");
    let resolution = outcome.resolution.as_ref().expect("legacy resolution");
    assert!(outcome.success && outcome.applied_now);
    assert_eq!(resolution.economy_gross_jc, 15);
    assert_eq!(
        resolution.jc,
        cama_domain::dig_economy::scale_positive_dig_jc(30)
    );
    assert_eq!(fixture.balance(ACTOR), 100 + resolution.jc);
    let detail: String = fixture
        .connection()
        .query_row(
            "SELECT detail FROM dig_actions WHERE id=?1",
            params![outcome.action_id.expect("legacy event action")],
            |row| row.get(0),
        )
        .expect("legacy event audit detail");
    let value: serde_json::Value = serde_json::from_str(&detail).expect("legacy detail JSON");
    assert_eq!(value["gross_jc"], 15);
    assert_eq!(value["reward_multiplier"], 0.65);
    assert_eq!(value["jc"], resolution.jc);
    let retry = service
        .resolve_action_event_with_legacy_catalog(request, &catalog)
        .expect("legacy event retry");
    assert!(!retry.applied_now);
    assert_eq!(retry.action_id, outcome.action_id);
    assert_eq!(fixture.count_actions(ACTOR, "event"), 1);
}

#[test]
fn legacy_catalog_dispatches_ranges_clamps_at_boss_and_retries_idempotently() {
    let fixture = Fixture::new();
    fixture.seed_actor(ACTOR, 100, 24, 0);
    let action_id = fixture.seed_legacy_event_dig_action("legacy_range_event");
    let catalog = DigLegacyEventCatalog::from_json_str(
        &serde_json::json!([{
            "id": "legacy_range_event",
            "name": "Legacy Range Event",
            "outcomes": {
                "inspect": {
                    "message": "The old catalog still speaks.",
                    "jc": [10, 12],
                    "depth": [3, 9]
                }
            }
        }])
        .to_string(),
    )
    .expect("legacy catalog parse");
    assert_eq!(catalog.len(), 1);
    assert_eq!(
        catalog.event("legacy_range_event").unwrap().outcomes["inspect"].jc,
        DigLegacyIntegerRange {
            minimum: 10,
            maximum: 12
        }
    );

    let service = fixture.service();
    let request = DigEventActionRequest {
        discord_id: ACTOR,
        guild_id: GUILD,
        dig_action_id: action_id,
        choice: "inspect",
        now: NOW,
    };
    let first = service
        .resolve_action_event_with_legacy_catalog(request, &catalog)
        .expect("legacy catalog dispatch");
    let resolution = first.resolution.as_ref().expect("legacy resolution");
    assert!(first.success && first.applied_now);
    assert!(resolution.economy_gross_jc >= 10 && resolution.economy_gross_jc <= 12);
    assert_eq!(resolution.advance, 0);
    assert!(resolution.boss_encounter);
    assert_eq!(first.depth_before, 24);
    assert_eq!(first.depth_after, 24);
    assert_eq!(
        fixture.balance(ACTOR),
        100 + first.resolution.as_ref().unwrap().jc
    );

    let retry = service
        .resolve_action_event_with_legacy_catalog(request, &catalog)
        .expect("legacy catalog retry");
    assert!(retry.success && !retry.applied_now);
    assert_eq!(retry.action_id, first.action_id);
    assert_eq!(retry.balance_after, first.balance_after);
    assert_eq!(fixture.count_actions(ACTOR, "event"), 1);
}

#[test]
fn legacy_catalog_rejects_malformed_range_and_missing_outcomes() {
    let malformed = serde_json::json!([{
        "id": "bad_range",
        "name": "Bad Range",
        "outcomes": {"inspect": {"jc": [4], "message": "nope"}}
    }]);
    let error = DigLegacyEventCatalog::from_json(&malformed).expect_err("malformed range");
    assert!(error.contains("range must contain two integers"));

    let missing = serde_json::json!([{"id": "missing_outcomes", "name": "Missing"}]);
    let error = DigLegacyEventCatalog::from_json(&missing).expect_err("missing outcomes");
    assert!(error.contains("outcomes object"));
}

#[test]
fn action_identity_drives_presentation_and_idempotent_component_settlement() {
    let fixture = Fixture::new();
    fixture.seed_actor(ACTOR, 100, 50, 0);
    let action_id = fixture.seed_event_dig_action("underground_stream");
    let service = fixture.service();

    let presentation = service
        .presentation_for_action(ACTOR, GUILD, action_id)
        .expect("presentation lookup")
        .expect("pending event");
    assert_eq!(presentation.event_id, "underground_stream");
    assert!(
        service
            .presentation_for_action(ACTOR + 1, GUILD, action_id)
            .expect("wrong actor lookup")
            .is_none()
    );

    let request = DigEventActionRequest {
        discord_id: ACTOR,
        guild_id: GUILD,
        dig_action_id: action_id,
        choice: "safe",
        now: NOW,
    };
    let first = service
        .resolve_action_event(request)
        .expect("component settlement");
    assert!(first.success);
    assert!(first.applied_now);
    let retry = service
        .resolve_action_event(request)
        .expect("component retry");
    assert!(retry.success);
    assert!(!retry.applied_now);
    assert_eq!(retry.action_id, first.action_id);

    let copied = service
        .resolve_action_event(DigEventActionRequest {
            discord_id: ACTOR + 1,
            ..request
        })
        .expect("copied component is blocked");
    assert!(!copied.success);

    let expired_action_id = fixture.seed_event_dig_action("gas_pocket");
    let expired = service
        .resolve_action_event(DigEventActionRequest {
            dig_action_id: expired_action_id,
            now: NOW + 59,
            ..request
        })
        .expect("expired component is blocked");
    assert!(!expired.success);
    assert!(
        expired
            .error
            .as_deref()
            .is_some_and(|error| error.contains("expired"))
    );
    assert_eq!(fixture.count_actions(ACTOR, "event"), 1);
}

#[test]
fn event_result_delivery_is_atomic_recoverable_and_exactly_once() {
    let fixture = Fixture::new();
    fixture.seed_actor(ACTOR, 100, 50, 0);
    let action_id = fixture.seed_event_dig_action("underground_stream");
    let service = fixture.service();
    let context = DigEventDeliveryContext::new(ACTOR, GUILD, 0x1234_5678, 777);
    let outcome = service
        .resolve_action_event_with_delivery(
            DigEventActionRequest {
                discord_id: ACTOR,
                guild_id: GUILD,
                dig_action_id: action_id,
                choice: "safe",
                now: NOW,
            },
            context,
        )
        .expect("resolve event with durable delivery");
    assert!(outcome.success && outcome.applied_now);
    let resolved_action_id = outcome.action_id.expect("resolved event action");
    let connection = fixture.connection();
    let delivery_json: String = connection
        .query_row(
            "SELECT json_extract(detail, '$.event_delivery.outcome')
               FROM dig_actions WHERE id=?1",
            params![resolved_action_id],
            |row| row.get(0),
        )
        .expect("atomic event delivery projection");
    let persisted: DigEventRuntimeOutcome =
        serde_json::from_str(&delivery_json).expect("typed persisted event result");
    assert_eq!(persisted.resolution, outcome.resolution);
    assert_eq!(persisted.action_id, Some(resolved_action_id));
    let state: String = connection
        .query_row(
            "SELECT json_extract(detail, '$.event_delivery.state')
               FROM dig_actions WHERE id=?1",
            params![resolved_action_id],
            |row| row.get(0),
        )
        .expect("terminal ready state");
    assert_eq!(state, "ready");

    let pending = service
        .pending_event_deliveries(DigEventPendingDeliveryQuery {
            guild_id: Some(GUILD),
            discord_id: Some(ACTOR),
            limit: 10,
        })
        .expect("pending event delivery");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].action_id, resolved_action_id);
    assert_eq!(pending[0].nonce(), "cama-e:12345678");

    let drifted_json = serde_json::json!({
        "success": true,
        "balance_after": outcome.balance_after.saturating_add(999),
        "drift": "recomputed-after-ready"
    })
    .to_string();
    let drifted = DigEventRuntimeRepository::new(fixture.database.path())
        .update_event_delivery_outcome(
            resolved_action_id,
            ACTOR,
            GUILD,
            &pending[0].source_key,
            &drifted_json,
        )
        .expect_err("ready delivery rejects a recomputed drifted outcome");
    assert!(matches!(
        drifted,
        DigEventRuntimeRepositoryError::IdempotencyConflict
    ));
    let persisted_after_drift: String = connection
        .query_row(
            "SELECT json_extract(detail, '$.event_delivery.outcome')
               FROM dig_actions WHERE id=?1",
            params![resolved_action_id],
            |row| row.get(0),
        )
        .expect("frozen delivery after drift rejection");
    assert_eq!(persisted_after_drift, delivery_json);

    let mark = DigEventDeliveryMarkRequest {
        action_id: resolved_action_id,
        discord_id: ACTOR,
        guild_id: GUILD,
        source_key: pending[0].source_key.clone(),
        delivered_at: NOW + 1,
    };
    assert!(
        service
            .mark_event_delivery_delivered(mark.clone())
            .expect("delivery CAS")
    );
    assert!(
        service
            .mark_event_delivery_delivered(mark)
            .expect("idempotent delivery CAS")
    );
    assert!(
        service
            .pending_event_deliveries(DigEventPendingDeliveryQuery {
                guild_id: Some(GUILD),
                discord_id: Some(ACTOR),
                limit: 10,
            })
            .expect("pending delivery after CAS")
            .is_empty()
    );
}

#[test]
fn pending_event_delivery_recovery_replays_and_freezes_full_result() {
    let fixture = Fixture::new();
    fixture.seed_actor(ACTOR, 100, 50, 0);
    let action_id = fixture.seed_event_dig_action("underground_stream");
    let context = DigEventDeliveryContext::new(ACTOR, GUILD, 0x1234_5679, 778);
    let service = fixture.service();
    let request = DigEventActionRequest {
        discord_id: ACTOR,
        guild_id: GUILD,
        dig_action_id: action_id,
        choice: "safe",
        now: NOW,
    };
    let first = service
        .resolve_action_event_with_delivery(request, context)
        .expect("initial event settlement");
    let resolved_action_id = first.action_id.expect("resolved action");

    // Simulate the crash point after actor COMMIT by moving the durable row
    // back to its atomic draft. Recovery must make this state explicit rather
    // than allowing the incomplete payload through the READY query.
    fixture
        .connection()
        .execute(
            "UPDATE dig_actions
                SET detail=json_set(detail,
                    '$.event_delivery.state', 'pending',
                    '$.event_delivery.outcome.action_id', NULL,
                    '$.event_delivery.outcome.quest_finale', NULL)
              WHERE id=?1",
            params![resolved_action_id],
        )
        .expect("simulate process stop before finalization");
    assert!(
        service
            .pending_event_deliveries(DigEventPendingDeliveryQuery {
                guild_id: Some(GUILD),
                discord_id: Some(ACTOR),
                limit: 10,
            })
            .expect("ready query excludes pending")
            .is_empty()
    );
    assert_eq!(
        service
            .pending_event_delivery_recoveries(DigEventPendingDeliveryQuery {
                guild_id: Some(GUILD),
                discord_id: Some(ACTOR),
                limit: 10,
            })
            .expect("pending recovery query")
            .len(),
        1
    );

    let recovered = service
        .recover_event_delivery(resolved_action_id)
        .expect("recover pending event")
        .expect("recovery outcome");
    assert!(recovered.success);
    assert_eq!(recovered.action_id, Some(resolved_action_id));
    let ready = service
        .pending_event_deliveries(DigEventPendingDeliveryQuery {
            guild_id: Some(GUILD),
            discord_id: Some(ACTOR),
            limit: 10,
        })
        .expect("ready delivery after recovery");
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].state, DigEventDeliveryState::Ready);
    assert_eq!(ready[0].outcome.action_id, Some(resolved_action_id));
}

#[test]
fn action_presentation_enforces_owner_and_carries_darkness_and_reading_perk() {
    let fixture = Fixture::new();
    fixture.seed_actor(ACTOR, 100, 50, 0);
    fixture
        .connection()
        .execute(
            "UPDATE tunnels SET luminosity=0, prestige_perks=?1
              WHERE discord_id=?2 AND guild_id=?3",
            params![
                serde_json::json!(["reading_the_stone"]).to_string(),
                ACTOR,
                GUILD
            ],
        )
        .expect("seed live prompt policy");
    let action_id = fixture.seed_event_dig_action("underground_stream");
    let service = fixture.service();

    let prompt = service
        .action_presentation(ACTOR, GUILD, action_id, NOW)
        .expect("prompt lookup")
        .expect("owned prompt");
    assert_eq!(prompt.event.event_id, "underground_stream");
    assert_eq!(prompt.luminosity, 0);
    assert!(prompt.safe_disabled);
    let hint = prompt.reading_the_stone_hint.expect("perk hint");
    assert!(!hint.chars().any(|character| character.is_ascii_digit()));
    assert!(
        READING_THE_STONE_SAFE_HINTS.contains(&hint.as_str())
            || READING_THE_STONE_RISKY_HINTS.contains(&hint.as_str())
            || READING_THE_STONE_DESPERATE_HINTS.contains(&hint.as_str())
    );
    assert!(
        service
            .action_presentation(ACTOR + 1, GUILD, action_id, NOW)
            .expect("copied prompt lookup")
            .is_none()
    );
    assert!(
        service
            .action_presentation(ACTOR, GUILD, action_id, NOW + 59)
            .expect("expired prompt lookup")
            .is_none(),
        "the Python view expires exactly 60 seconds after its source action"
    );
}

#[test]
fn reading_the_stone_uses_highest_jc_ev_stable_ties_and_number_free_copy() {
    let mut event = canonical_event("underground_stream")
        .expect("fixture event")
        .clone();
    let safe = event.safe_option.as_mut().expect("safe option");
    safe.success_chance = 1.0;
    safe.success.jc = 10;
    let risky = event.risky_option.as_mut().expect("risky option");
    risky.success_chance = 0.5;
    risky.success.jc = 5;
    risky.failure.as_mut().expect("risky failure").jc = -10;
    assert!(
        READING_THE_STONE_SAFE_HINTS
            .contains(&reading_the_stone_hint(&event, 1).expect("safe hint"))
    );

    event.safe_option.as_mut().expect("safe option").success.jc = 1;
    let risky = event.risky_option.as_mut().expect("risky option");
    risky.success_chance = 0.9;
    risky.success.jc = 20;
    risky.failure.as_mut().expect("risky failure").jc = 0;
    assert!(
        READING_THE_STONE_RISKY_HINTS
            .contains(&reading_the_stone_hint(&event, 2).expect("risky hint"))
    );

    let desperate = event
        .risky_option
        .clone()
        .expect("fixture for desperate option");
    event.safe_option = None;
    event.risky_option = None;
    event.desperate_option = Some(desperate);
    for (variant, expected) in READING_THE_STONE_DESPERATE_HINTS.iter().enumerate() {
        let hint = reading_the_stone_hint(&event, variant).expect("desperate hint");
        assert_eq!(hint, *expected);
        assert!(!hint.chars().any(|character| character.is_ascii_digit()));
    }

    event.desperate_option = None;
    assert_eq!(reading_the_stone_hint(&event, 0), None);
}

#[test]
fn all_203_authored_events_and_every_visible_choice_settle_on_migrated_sqlite() {
    let fixture = Fixture::new();
    fixture.seed_actor(ACTOR, 10_000, 50, 10);
    let service = fixture.service();
    let catalog = crate::dig_loot::canonical_event_catalog();
    assert_eq!(catalog.len(), 203);
    let mut settled = 0_usize;

    for event in catalog {
        let choices = [
            event.safe_option.as_ref().map(|_| "safe".to_owned()),
            event.risky_option.as_ref().map(|_| "risky".to_owned()),
            event
                .desperate_option
                .as_ref()
                .map(|_| "desperate".to_owned()),
        ]
        .into_iter()
        .flatten()
        .chain(
            event
                .boon_options
                .iter()
                .enumerate()
                .map(|(index, _)| format!("boon_{index}")),
        )
        .collect::<Vec<_>>();
        assert!(!choices.is_empty(), "{} has no live choice", event.id);

        for choice in choices {
            let mut connection = fixture.connection();
            let transaction = connection
                .transaction()
                .expect("reset authored event transaction");
            transaction
                .execute(
                    "UPDATE players SET jopacoin_balance=10000
                      WHERE discord_id=?1 AND guild_id=?2",
                    params![ACTOR, GUILD],
                )
                .expect("reset event balance");
            transaction
                .execute(
                    "UPDATE tunnels SET depth=50, max_depth=50, luminosity=100,
                         prestige_level=10, prestige_perks='[]', boss_progress='{}',
                         streak_days=40, temp_buffs=NULL, temp_curses=NULL
                      WHERE discord_id=?1 AND guild_id=?2",
                    params![ACTOR, GUILD],
                )
                .expect("reset event tunnel");
            for table in ["dig_inventory", "dig_gear", "dig_artifacts"] {
                transaction
                    .execute(
                        &format!("DELETE FROM {table} WHERE discord_id=?1 AND guild_id=?2"),
                        params![ACTOR, GUILD],
                    )
                    .expect("clear prior event reward");
            }
            transaction.commit().expect("commit authored event reset");
            drop(connection);

            let event_key = format!("catalog:{}:{choice}", event.id);
            let outcome = service
                .resolve_event(request(&event.id, &choice, &event_key))
                .unwrap_or_else(|error| panic!("{}:{choice} failed: {error}", event.id));
            assert!(
                outcome.success,
                "{}:{choice} blocked: {:?}",
                event.id, outcome.error
            );
            let resolution = outcome.resolution.expect("typed catalog resolution");
            assert_eq!(resolution.event_id, event.id);
            assert_eq!(resolution.choice, choice);
            settled += 1;
        }
    }

    assert_eq!(settled, 475, "exact authored option surface changed");
}

#[test]
fn steal_recovery_does_not_double_credit_after_actor_commit_failure() {
    let fixture = Fixture::new();
    let victim = ACTOR + 1;
    fixture.seed_actor(ACTOR, 100, 50, 0);
    fixture.seed_actor(victim, 200, 50, 0);
    let key = key_for_outcome(&fixture.snapshot(), "crow_snipe", "risky", true);
    let event_request = request("crow_snipe", "risky", &key);
    let service = fixture.service();
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER reject_event_actor
             BEFORE INSERT ON dig_actions
             WHEN NEW.action_type = 'event'
             BEGIN SELECT RAISE(ABORT, 'forced actor audit failure'); END;",
        )
        .expect("install actor failure");

    assert!(matches!(
        service.resolve_event(event_request),
        Err(DigEventRuntimeError::EventRepository(_))
    ));
    let actor_after_splash = fixture.balance(ACTOR);
    let victim_after_splash = fixture.balance(victim);
    let stolen = actor_after_splash - 100;
    assert!(stolen > 0);
    assert_eq!(200 - victim_after_splash, stolen);
    fixture
        .connection()
        .execute("DROP TRIGGER reject_event_actor", [])
        .expect("remove actor failure");

    let recovered = service
        .resolve_event(event_request)
        .expect("restart recovery");
    assert!(recovered.success);
    let resolution = recovered.resolution.as_ref().expect("typed resolution");
    assert_eq!(fixture.balance(victim), victim_after_splash);
    assert_eq!(fixture.balance(ACTOR), 100 + stolen + resolution.jc);
    assert_eq!(
        recovered.splash.as_ref().expect("steal splash").total_moved,
        stolen
    );
    assert_eq!(fixture.count_actions(ACTOR, "splash_thief"), 1);
    assert_eq!(fixture.count_actions(ACTOR, "event"), 1);
}

#[test]
fn cooperative_splash_grant_is_durable_and_retry_safe() {
    let fixture = Fixture::new();
    let partner = ACTOR + 1;
    fixture.seed_actor(ACTOR, 100, 60, 0);
    fixture.seed_actor(partner, 80, 40, 0);
    fixture.mark_active_digger(partner);
    let key = key_for_outcome(&fixture.snapshot(), "io_tether_pact", "risky", true);
    let event_request = request("io_tether_pact", "risky", &key);
    let service = fixture.service();

    let first = service.resolve_event(event_request).expect("grant event");
    let splash = first.splash.as_ref().expect("grant splash");
    assert_eq!(splash.mode, "grant");
    assert_eq!(splash.victims.len(), 1);
    assert_eq!(splash.victims[0].0, partner);
    assert_eq!(fixture.balance(partner), 80 + splash.victims[0].1);
    let partner_after = fixture.balance(partner);

    let retry = service.resolve_event(event_request).expect("grant retry");
    assert!(!retry.applied_now);
    assert_eq!(fixture.balance(partner), partner_after);
    assert_eq!(fixture.count_actions(partner, "splash_victim"), 1);
}

#[test]
fn resolve_helltide_event_sets_modifier() {
    let fixture = Fixture::new();
    fixture.seed_actor(ACTOR, 100, 50, 0);
    let key = key_for_outcome(&fixture.snapshot(), "helltide_bell", "risky", true);
    let event_request = request("helltide_bell", "risky", &key);
    let service = fixture.service();
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER reject_event_actor
             BEFORE INSERT ON dig_actions
             WHEN NEW.action_type = 'event'
             BEGIN SELECT RAISE(ABORT, 'forced actor audit failure'); END;",
        )
        .expect("install actor failure");

    assert!(matches!(
        service.resolve_event(event_request),
        Err(DigEventRuntimeError::EventRepository(_))
    ));
    let expires_at = fixture
        .connection()
        .query_row(
            "SELECT expires_at FROM dig_guild_modifiers
              WHERE guild_id=?1 AND modifier_id='helltide_active'",
            params![GUILD],
            |row| row.get::<_, i64>(0),
        )
        .expect("modifier committed before actor");
    assert_eq!(expires_at, NOW + 1_800);
    fixture
        .connection()
        .execute("DROP TRIGGER reject_event_actor", [])
        .expect("remove actor failure");

    let recovered = service
        .resolve_event(event_request)
        .expect("modifier retry");
    let modifier = recovered.guild_modifier.expect("typed modifier outcome");
    assert!(!modifier.applied_now);
    assert_eq!(modifier.expires_at, expires_at);
    assert_eq!(fixture.count_actions(ACTOR, "event_modifier"), 1);
}

#[test]
fn burn_splash_scales_actor_payout_from_actual_destroyed_jc() {
    let fixture = Fixture::new();
    fixture.seed_actor(ACTOR, 100, 70, 0);
    fixture.seed_actor(ACTOR + 1, 200, 20, 0);
    fixture.seed_actor(ACTOR + 2, 40, 20, 0);
    fixture.seed_actor(ACTOR + 3, 1, 20, 0);
    let key = key_for_outcome(&fixture.snapshot(), "wealth_siphon", "risky", true);
    let event_request = request("wealth_siphon", "risky", &key);
    let service = fixture.service();

    let outcome = service.resolve_event(event_request).expect("burn event");
    let splash = outcome.splash.as_ref().expect("burn splash");
    let resolution = outcome.resolution.as_ref().expect("typed resolution");
    assert_eq!(splash.mode, "burn");
    assert_eq!(splash.victims.len(), 1);
    assert!(splash.total_moved > 0);
    let ratio = resolution
        .splash_payout_ratio
        .expect("actual burn payout ratio");
    assert!((ratio - (1.0 / 3.0)).abs() < 1e-9);
    assert!(resolution.economy_gross_jc < 59);
    let victim_balance = fixture.balance(ACTOR + 1);

    let retry = service.resolve_event(event_request).expect("burn retry");
    assert!(!retry.applied_now);
    assert_eq!(fixture.balance(ACTOR + 1), victim_balance);
}

#[test]
fn protected_burn_reports_absorption_and_uses_post_shield_loss() {
    let fixture = Fixture::new();
    let victim = ACTOR + 1;
    fixture.seed_actor(ACTOR, 100, 70, 0);
    fixture.seed_actor(victim, 200, 20, 0);
    fixture
        .connection()
        .execute(
            "INSERT INTO manashop_buffs (
                 discord_id, guild_id, buff_type, granted_at, expires_at,
                 triggered, data
             ) VALUES (?1, ?2, 'aegis', ?3, ?4, 0, ?5)",
            params![
                victim,
                GUILD,
                NOW - 1,
                NOW + 100,
                r#"{"capacity":10,"capacity_remaining":10,"rate":1.0}"#
            ],
        )
        .expect("seed Aegis protection");
    let key = key_for_outcome(&fixture.snapshot(), "wealth_siphon", "risky", true);
    let outcome = fixture
        .service()
        .resolve_event(request("wealth_siphon", "risky", &key))
        .expect("protected burn event");

    let splash = outcome.splash.as_ref().expect("protected splash");
    let nominal_penalty =
        scale_deflationary_minigame_jc_delta(strengthen_dig_event_penalty(10) as f64, 1.0);
    assert_eq!(splash.absorbed_total, 10);
    assert_eq!(splash.shielded_count, 1);
    assert_eq!(splash.total_moved, nominal_penalty - 10);
    assert_eq!(fixture.balance(victim), 200 - splash.total_moved);
    let ratio = outcome
        .resolution
        .as_ref()
        .and_then(|resolution| resolution.splash_payout_ratio)
        .expect("protected payout ratio");
    let expected_ratio = splash.total_moved as f64 / (nominal_penalty * 3) as f64;
    assert!((ratio - expected_ratio).abs() < 1e-9);
    assert_eq!(
        fixture
            .connection()
            .query_row("SELECT COUNT(*) FROM mana_protection_events", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("protection audit count"),
        1
    );
}

#[test]
fn resolve_event_grants_and_returns_unique_gear_atomically() {
    let fixture = Fixture::new();
    fixture.seed_actor(ACTOR, 100, 100, 0);
    let key = key_for_outcome(&fixture.snapshot(), "collapsed_armory", "risky", true);
    let event_request = request("collapsed_armory", "risky", &key);
    let service = fixture.service();

    let first = service.resolve_event(event_request).expect("gear event");
    let reward_id = match &first.resolution.as_ref().expect("resolution").reward {
        Some(CanonicalReward::Gear(item_id)) => item_id,
        reward => panic!("expected gear reward, got {reward:?}"),
    };
    let row = fixture
        .connection()
        .query_row(
            "SELECT item_id, source FROM dig_gear WHERE id=?1",
            params![first.reward_row_id.expect("reward row")],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("persisted gear reward");
    assert_eq!(row.0, *reward_id);
    assert_eq!(row.1, "event:collapsed_armory");

    let retry = service
        .resolve_event(event_request)
        .expect("gear event retry");
    assert!(!retry.applied_now);
    assert_eq!(retry.reward_row_id, first.reward_row_id);
    assert_eq!(
        fixture
            .connection()
            .query_row("SELECT COUNT(*) FROM dig_gear", [], |row| row
                .get::<_, i64>(0))
            .expect("gear count"),
        1
    );
}

#[test]
fn resolve_event_gear_grant_rolls_back_with_event_log_failure() {
    let fixture = Fixture::new();
    fixture.seed_actor(ACTOR, 100, 100, 0);
    let key = key_for_outcome(&fixture.snapshot(), "collapsed_armory", "risky", true);
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER reject_event_log
             BEFORE INSERT ON dig_actions
             WHEN NEW.action_type = 'event'
             BEGIN SELECT RAISE(ABORT, 'forced event log failure'); END;",
        )
        .expect("install event-log failure");

    let result = fixture
        .service()
        .resolve_event(request("collapsed_armory", "risky", &key));
    assert!(
        result.is_err(),
        "event log failure must abort gear settlement"
    );
    assert_eq!(
        fixture
            .connection()
            .query_row("SELECT COUNT(*) FROM dig_gear", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("gear count"),
        0
    );
    assert_eq!(fixture.balance(ACTOR), 100);
}

#[test]
fn unique_gear_repair_preview_matches_prorated_debit() {
    let mut service = GearService::fixture();
    let gear_id = service.store.add_gear(
        111,
        0,
        GearSlot::Weapon,
        3,
        Some(4),
        "event:collapsed_armory",
        Some("glassbreaker_pick"),
    );
    let preview =
        service.compute_repair_cost(GearSlot::Weapon, 3, Some("glassbreaker_pick"), 4, Some(8));
    assert_eq!(preview, 10);
    let result = service.repair_gear(111, 0, gear_id);
    assert!(result.success);
    assert_eq!(result.cost, preview);
    assert_eq!(service.store.balance(111, 0), 5_000 - preview);
    assert_eq!(
        service
            .store
            .gear_by_id(gear_id)
            .expect("repaired gear")
            .durability,
        8
    );
}

#[test]
fn event_result_embed_surfaces_gear_drop_details() {
    let presentation = gear_drop_presentation(
        "Glassbreaker Pick",
        "weapon",
        8,
        "Diamond dig bonuses; +2 boss damage; -8% hit chance.",
    );
    assert_eq!(presentation.field_name, "Gear Drop");
    assert!(presentation.field_value.contains("**Glassbreaker Pick**"));
    assert!(presentation.field_value.contains("Weapon"));
    assert!(presentation.field_value.contains("Durability: 8"));
    assert!(
        presentation
            .field_value
            .contains("Diamond dig bonuses; +2 boss damage; -8% hit chance.")
    );
    assert!(
        presentation
            .field_value
            .contains("Stored in your gear inventory. View with `/dig gear`.")
    );
}

#[test]
fn migrated_artifact_reward_pool_loads_owned_set_once_before_settlement() {
    let fixture = Fixture::new();
    fixture.seed_actor(ACTOR, 100, 100, 0);
    for index in 0..8 {
        fixture
            .connection()
            .execute(
                "INSERT INTO dig_inventory (
                     discord_id, guild_id, item_type, queued, created_at
                 ) VALUES (?1, ?2, 'lantern', 0, ?3)",
                params![ACTOR, GUILD, index],
            )
            .expect("fill inventory before artifact reward");
    }
    let key = key_for_outcome(&fixture.snapshot(), "song_below_stone", "risky", true);
    let outcome = fixture
        .service()
        .resolve_event(request("song_below_stone", "risky", &key))
        .expect("artifact reward event");

    assert!(outcome.success);
    assert!(matches!(
        outcome.resolution.as_ref().expect("resolution").reward,
        Some(CanonicalReward::Artifact(_))
    ));
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM dig_artifacts
                  WHERE discord_id=?1 AND guild_id=?2",
                params![ACTOR, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("artifact reward row count"),
        1,
    );
}

#[test]
fn deterministic_authored_chain_is_preserved_in_typed_outcome() {
    let fixture = Fixture::new();
    fixture.seed_actor(ACTOR, 100, 200, 6);
    let outcome = fixture
        .service()
        .resolve_event(request(
            "hollow_court_overture",
            "safe",
            "event:authored-chain:1",
        ))
        .expect("authored chain event");

    let chain = outcome.chain_event.expect("authored follow-up");
    assert_eq!(chain.event_id, "hollow_court_audience");
    assert_eq!(
        outcome
            .resolution
            .as_ref()
            .and_then(|resolution| resolution.chained_event_id.as_deref()),
        Some("hollow_court_audience")
    );
}

#[test]
fn completed_quest_finale_resumes_after_complete_first_failure() {
    let fixture = Fixture::new();
    fixture.seed_actor(ACTOR, 100, 100, 0);
    fixture
        .connection()
        .execute(
            "INSERT INTO dig_quests (
                 discord_id, guild_id, active_quest_id, active_quest_step,
                 completed_quests, last_updated_at
             ) VALUES (?1, ?2, 'agh_lost_trial', 5, '[]', ?3)",
            params![ACTOR, GUILD, NOW - 1],
        )
        .expect("seed final quest step");
    let key = key_for_outcome(&fixture.snapshot(), "agh_s5", "desperate", true);
    let event_request = request("agh_s5", "desperate", &key);
    let service = fixture.service();
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER reject_quest_finale
             BEFORE INSERT ON dig_actions
             WHEN NEW.action_type = 'quest_finale_jc'
             BEGIN SELECT RAISE(ABORT, 'forced finale failure'); END;",
        )
        .expect("install finale failure");

    let first = service
        .resolve_event(event_request)
        .expect("complete quest");
    assert!(first.success);
    assert!(first.quest_finale.is_none());
    let completed: String = fixture
        .connection()
        .query_row(
            "SELECT completed_quests FROM dig_quests
              WHERE discord_id=?1 AND guild_id=?2",
            params![ACTOR, GUILD],
            |row| row.get(0),
        )
        .expect("completed quest state");
    assert!(completed.contains("agh_lost_trial"));
    fixture
        .connection()
        .execute("DROP TRIGGER reject_quest_finale", [])
        .expect("remove finale failure");

    let recovered = service.resolve_event(event_request).expect("resume finale");
    assert!(!recovered.applied_now);
    let DigEventQuestFinale::JcAndModifier {
        quest_id,
        net_jc,
        modifier,
        ..
    } = recovered.quest_finale.expect("resumed finale")
    else {
        panic!("expected JC quest finale");
    };
    assert_eq!(quest_id, "agh_lost_trial");
    assert!(net_jc > 0);
    assert_eq!(
        modifier.expect("finale modifier").modifier_id,
        "reagent_spill"
    );
    assert_eq!(fixture.count_actions(ACTOR, "event"), 1);
    assert_eq!(fixture.count_actions(ACTOR, "quest_finale_jc"), 1);
}
