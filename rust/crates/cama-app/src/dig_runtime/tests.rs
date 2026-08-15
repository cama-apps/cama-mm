use cama_db::core_repositories::{NewPlayer, PlayerRepository};
use cama_db::dig_blood_pact::{DigBloodPactRepository, DigBloodPactSettlementRequest};
use cama_db::dig_event_runtime::{
    DigEventActorKey, DigEventQuestMutation, DigEventRuntimeRepository,
};
use cama_db::dig_guild_modifiers::DigGuildModifierRepository;
use cama_db::dig_inventory_repository::{BuyInsuranceOutcome, SetTrapOutcome};
use cama_db::economy_event_repository::{
    EconomyEventRepository, EventDirection, EventDraft, EventEffects,
};
use cama_db::loan_repository::LoanRepository;
use cama_db::manashop_rework_repository::{BuffData, GrantBuffRequest, ManashopRepository};
use cama_db::schema_manager::initialize_or_migrate;
use cama_domain::dig_cave_in::{CAVE_IN_BLOCK_LOSS_RANGES, CAVE_IN_CATASTROPHIC_PCT_BY_BAND};
use cama_domain::dig_economy::scale_positive_dig_jc;
use cama_domain::dig_gear::ARMOR_TIERS;
use cama_domain::pet::DIG_WORK_UNITS_PER_BLOCK;
use rusqlite::{Connection, params};
use serde_json::Value;
use tempfile::NamedTempFile;

use crate::dig_loot::{LootEntropy, SeededLootEntropy};
use crate::economy_event_service::EconomyEventConfig;

use super::{
    DigAdminMutationOutcome, DigRuntimeBloodPactSnapshot, DigRuntimeCommit, DigRuntimeConfig,
    DigRuntimeDeliveryContext, DigRuntimeDeliveryDraft, DigRuntimeDeliveryPart,
    DigRuntimeMarkDelivered, DigRuntimePendingDeliveryQuery, DigRuntimeRebindDeliveryChannel,
    DigRuntimeRequest, DigRuntimeService, DigRuntimeSettleBloodPact, DigRuntimeSnapshot,
    DigRuntimeStore, DigRuntimeStoreError, DigRuntimeVersion, InMemoryDigRuntimeStore,
    SqliteDigRuntimeStore, proportional_mana_yield_tax, scale_dig_minigame_jc,
};
use crate::vanity_tax_service::{VanityMember, VanityTaxService};
use cama_domain::game_date::game_date_for_timestamp;
use std::sync::Arc;

const PET_DAY: i64 = DIG_WORK_UNITS_PER_BLOCK;

fn seed_runtime_pet(connection: &Connection, now: i64, work_units: i64) -> i64 {
    connection
        .execute(
            "INSERT INTO pets (
                     discord_id,guild_id,name,species,adopted_at,hatched_at,
                     adopt_fee,last_fed_at,hunger_at_last_fed,
                     dig_work_units,dig_work_at
                 ) VALUES (7,9,'Blep','common_cama',?1,?1,0,?1,100,?2,?1)",
            params![now - 2 * PET_DAY, work_units],
        )
        .expect("seed runtime pet");
    connection.last_insert_rowid()
}

fn live_event_fixture() -> (NamedTempFile, SqliteDigRuntimeStore, DigRuntimeSnapshot) {
    let database = NamedTempFile::new().expect("temporary event picker database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(8_701, "live-event-picker", Some(8_702)))
        .expect("seed event picker player");
    let connection = Connection::open(database.path()).expect("open event picker database");
    connection
        .execute(
            "INSERT INTO tunnels (
                     discord_id, guild_id, depth, max_depth, luminosity,
                     total_digs, last_dig_at, prestige_level,
                     prestige_perks, boss_progress, streak_days
                 ) VALUES (?1, ?2, 25, 25, 100, 1, ?3, 0, '[]', '{}', 4)",
            params![8_701_i64, 8_702_i64, 1_699_996_400_i64],
        )
        .expect("seed event picker tunnel");
    drop(connection);
    let store = SqliteDigRuntimeStore::new(database.path());
    let snapshot = store.snapshot(8_701, 8_702).expect("event picker snapshot");
    (database, store, snapshot)
}

fn set_live_picker_quest(database: &NamedTempFile, step: i64) {
    DigEventRuntimeRepository::new(database.path())
        .apply_quest_mutation(
            DigEventActorKey {
                discord_id: 8_701,
                guild_id: Some(8_702),
            },
            DigEventQuestMutation::SetActive {
                quest_id: "agh_lost_trial",
                next_step: step,
            },
            1_700_000_000,
        )
        .expect("set live picker quest");
}

fn canonical_seed_for(
    snapshot: &DigRuntimeSnapshot,
    quest: &cama_db::dig_event_runtime::DigEventQuestSnapshot,
    in_boss: bool,
    target: &str,
) -> u64 {
    let service = crate::dig_event_runtime::DigEventRuntimeService::sqlite("");
    let tunnel = snapshot.tunnel.as_ref().expect("live tunnel");
    let actor = cama_db::dig_event_runtime::DigEventActorSnapshot {
        key: DigEventActorKey {
            discord_id: tunnel.discord_id,
            guild_id: Some(tunnel.guild_id),
        },
        depth: tunnel.depth,
        luminosity: tunnel.luminosity,
        prestige_level: tunnel.prestige_level,
        prestige_perks_json: tunnel.prestige_perks.clone(),
        boss_progress_json: tunnel.boss_progress.clone(),
        streak_days: tunnel.streak_days,
        temp_buff_json: tunnel.temp_buffs.clone(),
        temp_curse_json: tunnel.temp_curses.clone(),
        balance: snapshot.balance,
        inventory_count: snapshot.inventory.len(),
        owned_gear: snapshot
            .gear
            .iter()
            .filter_map(|piece| piece.item_id.clone())
            .collect(),
        equipped_gear: snapshot
            .gear
            .iter()
            .filter(|piece| piece.equipped && piece.durability > 0)
            .filter_map(|piece| piece.item_id.clone())
            .collect(),
        owned_artifacts: snapshot
            .artifacts
            .iter()
            .map(|artifact| artifact.artifact_id.clone())
            .collect(),
        equipped_relics: snapshot
            .artifacts
            .iter()
            .filter(|artifact| artifact.is_relic && artifact.equipped)
            .map(|artifact| artifact.artifact_id.clone())
            .collect(),
    };
    for seed in 0..10_000_u64 {
        let mut entropy = SeededLootEntropy::new(seed);
        if service
            .roll_event_for_snapshot(&actor, quest, true, in_boss, &mut entropy)
            .is_some_and(|event| event.event_id == target)
        {
            return seed;
        }
    }
    panic!("no deterministic seed for canonical event {target}");
}

#[test]
fn test_live_dig_roll_event_excludes_quest_events_without_player_context() {
    let (_database, store, mut snapshot) = live_event_fixture();
    snapshot.tunnel = None;
    assert_eq!(
        store
            .canonical_event_id(&snapshot, 1_700_000_000, false, 0)
            .expect("missing actor remains fail-soft"),
        None
    );
}

#[test]
fn test_live_dig_roll_event_includes_quest_starter_when_eligible() {
    let (database, store, snapshot) = live_event_fixture();
    let quest = DigEventRuntimeRepository::new(database.path())
        .quest_snapshot(
            DigEventActorKey {
                discord_id: 8_701,
                guild_id: Some(8_702),
            },
            1_700_000_000,
        )
        .expect("default quest snapshot");
    let seed = canonical_seed_for(&snapshot, &quest, false, "agh_s1");
    assert_eq!(
        store
            .canonical_event_id(&snapshot, 1_700_000_000, false, seed)
            .expect("live canonical picker"),
        Some("agh_s1".to_owned())
    );
}

#[test]
fn test_live_dig_roll_event_excludes_quest_event_when_player_on_different_step() {
    let (database, store, snapshot) = live_event_fixture();
    set_live_picker_quest(&database, 3);
    let quest = DigEventRuntimeRepository::new(database.path())
        .quest_snapshot(
            DigEventActorKey {
                discord_id: 8_701,
                guild_id: Some(8_702),
            },
            1_700_000_000,
        )
        .expect("active quest snapshot");
    let seed = canonical_seed_for(&snapshot, &quest, false, "agh_s3");
    assert_eq!(
        store
            .canonical_event_id(&snapshot, 1_700_000_000, false, seed)
            .expect("live canonical picker"),
        Some("agh_s3".to_owned())
    );
    for seed in 0..128_u64 {
        assert_ne!(
            store
                .canonical_event_id(&snapshot, 1_700_000_000, false, seed)
                .expect("live canonical picker"),
            Some("agh_s1".to_owned())
        );
    }
}

#[test]
fn test_live_dig_roll_event_excludes_quest_event_during_boss_combat() {
    let (database, store, snapshot) = live_event_fixture();
    let quest = DigEventRuntimeRepository::new(database.path())
        .quest_snapshot(
            DigEventActorKey {
                discord_id: 8_701,
                guild_id: Some(8_702),
            },
            1_700_000_000,
        )
        .expect("default quest snapshot");
    let _ = quest;
    for seed in 0..128_u64 {
        let event_id = store
            .canonical_event_id(&snapshot, 1_700_000_000, true, seed)
            .expect("live canonical picker");
        if let Some(event_id) = event_id {
            assert!(!event_id.starts_with("agh_s"));
        }
    }
}

#[test]
fn test_live_dig_roll_event_passes_tunnel_through_to_quest_filter() {
    let (database, store, mut snapshot) = live_event_fixture();
    let quest = DigEventRuntimeRepository::new(database.path())
        .quest_snapshot(
            DigEventActorKey {
                discord_id: 8_701,
                guild_id: Some(8_702),
            },
            1_700_000_000,
        )
        .expect("default quest snapshot");
    let at_gate_seed = canonical_seed_for(&snapshot, &quest, false, "agh_s1");
    snapshot.tunnel.as_mut().expect("live tunnel").depth = 24;
    assert_ne!(
        store
            .canonical_event_id(&snapshot, 1_700_000_000, false, at_gate_seed)
            .expect("live canonical picker"),
        Some("agh_s1".to_owned())
    );
    snapshot.tunnel.as_mut().expect("live tunnel").depth = 25;
    assert_eq!(
        store
            .canonical_event_id(&snapshot, 1_700_000_000, false, at_gate_seed)
            .expect("live canonical picker"),
        Some("agh_s1".to_owned())
    );
}

#[test]
fn first_dig_is_atomic_and_deterministic() {
    assert_eq!(
        super::DigRuntimeConfig::default().asset_root,
        std::path::PathBuf::from(super::DEFAULT_DIG_ASSET_ROOT)
    );
    let snapshot = DigRuntimeSnapshot::fresh(7, 9, 100, 1_700_000_000);
    let store = InMemoryDigRuntimeStore::new(snapshot);
    let service = DigRuntimeService::new(store.clone());
    let outcome = service
        .dig(DigRuntimeRequest {
            discord_id: 7,
            guild_id: 9,
            now: 1_700_000_000,
            paid: false,
            forced_event: false,
        })
        .expect("first dig should commit");
    assert!(outcome.success);
    assert!(outcome.first_dig);
    assert!((3..=7).contains(&outcome.advance));
    let current = store.current().expect("snapshot remains available");
    assert_eq!(
        current.tunnel.as_ref().map(|tunnel| tunnel.total_digs),
        Some(1)
    );
    assert_eq!(
        current.tunnel.as_ref().map(|tunnel| tunnel.last_dig_at),
        Some(Some(1_700_000_000))
    );
}

#[test]
fn sqlite_delivery_outbox_round_trips_and_marks_main_part_once() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(7, "delivery-test", Some(9)))
        .expect("seed player");
    let service = DigRuntimeService::sqlite(database.path());
    let execution = service
        .dig_with_delivery(
            DigRuntimeRequest {
                discord_id: 7,
                guild_id: 9,
                now: 1_700_000_000,
                paid: false,
                forced_event: false,
            },
            DigRuntimeDeliveryContext::new(99, 11, "Delivery Miner", None),
        )
        .expect("dig and outbox attach");
    let delivery = execution.delivery.expect("delivery snapshot");
    assert_eq!(delivery.source_key, format!("dig:{}", delivery.action_id));
    assert_eq!(delivery.context.display_name, "Delivery Miner");
    let settled = service
        .settle_blood_pact_delivery(DigRuntimeSettleBloodPact {
            action_id: delivery.action_id,
            source_key: delivery.source_key.clone(),
            occurred_at: 1_700_000_000,
        })
        .expect("settle delivery economy effects");
    assert!(settled.blood_pact.is_terminal());
    assert_eq!(
        service
            .pending_deliveries(DigRuntimePendingDeliveryQuery {
                guild_id: Some(9),
                discord_id: Some(7),
                limit: 10,
            })
            .expect("pending outbox")
            .len(),
        1
    );
    assert!(
        service
            .mark_delivery_delivered(DigRuntimeMarkDelivered {
                action_id: delivery.action_id,
                source_key: delivery.source_key.clone(),
                delivered_at: 1_700_000_001,
                part: DigRuntimeDeliveryPart::Main,
            })
            .expect("mark main")
    );
    assert!(
        service
            .pending_deliveries(DigRuntimePendingDeliveryQuery {
                guild_id: Some(9),
                discord_id: Some(7),
                limit: 10,
            })
            .expect("pending after main")
            .is_empty()
    );
}

#[test]
fn sqlite_blood_pact_delivery_applies_once_and_persists_terminal_state() {
    let (database, service, execution, now) = live_blood_pact_delivery_fixture();
    let delivery = execution.delivery.expect("Blood Pact delivery");
    let request = DigRuntimeSettleBloodPact {
        action_id: delivery.action_id,
        source_key: delivery.source_key.clone(),
        occurred_at: now,
    };
    let settled = service
        .settle_blood_pact_delivery(request.clone())
        .expect("apply Blood Pact delivery");
    let skimmed = match settled.blood_pact {
        DigRuntimeBloodPactSnapshot::Applied { skimmed } => skimmed,
        other => panic!("expected applied Blood Pact state, got {other:?}"),
    };
    assert!(skimmed > 0);

    let replay = service
        .settle_blood_pact_delivery(request)
        .expect("terminal Blood Pact replay");
    assert_eq!(
        replay.blood_pact,
        DigRuntimeBloodPactSnapshot::Applied { skimmed }
    );
    let connection = Connection::open(database.path()).expect("inspect Blood Pact action");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM hostile_loss_events
                     WHERE guild_id=?1 AND victim_id=?2 AND event_key=?3",
                params![
                    settled.guild_id,
                    settled.discord_id,
                    format!(
                        "dig-blood-pact:{}:{}",
                        settled.action_id, settled.discord_id
                    ),
                ],
                |row| row.get::<_, i64>(0),
            )
            .expect("Blood Pact event count"),
        1
    );
    let detail: String = connection
        .query_row(
            "SELECT detail FROM dig_actions WHERE id=?1",
            params![settled.action_id],
            |row| row.get(0),
        )
        .expect("Blood Pact detail");
    let detail = serde_json::from_str::<Value>(&detail).expect("Blood Pact detail JSON");
    assert_eq!(
        detail["delivery"]["blood_pact"],
        serde_json::json!({"Applied": {"skimmed": skimmed}})
    );
    assert_eq!(
        detail["delivery"]["outcome"]["jc_earned"],
        serde_json::json!(settled.outcome.jc_earned)
    );
}

#[test]
fn sqlite_blood_pact_delivery_reconciles_after_effect_before_snapshot() {
    let (database, service, execution, now) = live_blood_pact_delivery_fixture();
    let delivery = execution.delivery.expect("Blood Pact delivery");
    let event_key = format!(
        "dig-blood-pact:{}:{}",
        delivery.action_id, delivery.discord_id
    );
    let mana_date = game_date_for_timestamp(now as f64).expect("Blood Pact game date");
    let mut settlement_request = DigBloodPactSettlementRequest::with_default_scale(
        delivery.discord_id,
        delivery.guild_id,
        delivery.outcome.jc_earned,
        event_key.clone(),
        now,
        mana_date,
    );
    settlement_request.minigame_jc_delta_scale = 1.0;
    let first = DigBloodPactRepository::new(database.path())
        .settle(&settlement_request)
        .expect("simulate committed Blood Pact effect");
    assert!(!first.duplicate);
    let reconciled = service
        .settle_blood_pact_delivery(DigRuntimeSettleBloodPact {
            action_id: delivery.action_id,
            source_key: delivery.source_key.clone(),
            occurred_at: now,
        })
        .expect("reconcile Blood Pact snapshot after crash");
    assert_eq!(
        reconciled.blood_pact,
        DigRuntimeBloodPactSnapshot::Applied {
            skimmed: first.applied_amount,
        }
    );
    let replay = service
        .settle_blood_pact_delivery(DigRuntimeSettleBloodPact {
            action_id: delivery.action_id,
            source_key: delivery.source_key,
            occurred_at: now,
        })
        .expect("replay reconciled Blood Pact snapshot");
    assert_eq!(replay.blood_pact, reconciled.blood_pact);
    assert_eq!(first.event_key, event_key);
}

#[test]
fn sqlite_blood_pact_delivery_skips_cave_reward() {
    let database = NamedTempFile::new().expect("Blood Pact cave database");
    let actor = 61_111;
    let guild = 61_113;
    let start = 1_900_121_000;
    seed_live_runtime_tunnel(&database, actor, guild, start, 76, 1, Some(start - 7_200));
    Connection::open(database.path())
            .expect("Blood Pact cave progress connection")
            .execute(
                "UPDATE tunnels SET boss_progress=?1
                 WHERE discord_id=?2 AND guild_id=?3",
                params![
                    r#"{"25":{"status":"defeated"},"50":{"status":"defeated"},"75":{"status":"defeated"}}"#,
                    actor,
                    guild,
                ],
            )
            .expect("seed cave defeated bosses");
    let now = find_cave_dig_time(actor, guild, start);
    let service = DigRuntimeService::sqlite(database.path());
    let execution = service
        .dig_with_delivery(
            DigRuntimeRequest {
                discord_id: actor,
                guild_id: guild,
                now,
                paid: false,
                forced_event: false,
            },
            DigRuntimeDeliveryContext::new(61_114, 61_115, "Cave Miner", None),
        )
        .expect("cave Dig delivery");
    assert!(execution.outcome.cave_in);
    assert_eq!(execution.outcome.jc_earned, 0);
    let delivery = execution.delivery.expect("cave delivery");
    assert_eq!(delivery.blood_pact, DigRuntimeBloodPactSnapshot::Skipped);
    let settled = service
        .settle_blood_pact_delivery(DigRuntimeSettleBloodPact {
            action_id: delivery.action_id,
            source_key: delivery.source_key,
            occurred_at: now,
        })
        .expect("skip cave Blood Pact");
    assert_eq!(settled.blood_pact, DigRuntimeBloodPactSnapshot::Skipped);
    assert_eq!(
        Connection::open(database.path())
            .expect("inspect cave Blood Pact events")
            .query_row("SELECT COUNT(*) FROM hostile_loss_events", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("hostile event count"),
        0
    );
}

#[test]
fn sqlite_blood_pact_changed_earning_retry_is_rejected() {
    let (database, service, execution, now) = live_blood_pact_delivery_fixture();
    let delivery = execution.delivery.expect("Blood Pact delivery");
    let settled = service
        .settle_blood_pact_delivery(DigRuntimeSettleBloodPact {
            action_id: delivery.action_id,
            source_key: delivery.source_key.clone(),
            occurred_at: now,
        })
        .expect("initial Blood Pact settlement");
    assert!(matches!(
        settled.blood_pact,
        DigRuntimeBloodPactSnapshot::Applied { .. }
    ));
    let connection = Connection::open(database.path()).expect("mutate Blood Pact detail");
    let detail: String = connection
        .query_row(
            "SELECT detail FROM dig_actions WHERE id=?1",
            params![delivery.action_id],
            |row| row.get(0),
        )
        .expect("read Blood Pact detail");
    let mut value = serde_json::from_str::<Value>(&detail).expect("parse Blood Pact detail");
    value["delivery"]["blood_pact"] = serde_json::json!("Pending");
    value["delivery"]["outcome"]["jc_earned"] = serde_json::json!(delivery.outcome.jc_earned + 1);
    connection
        .execute(
            "UPDATE dig_actions SET detail=?1 WHERE id=?2",
            params![value.to_string(), delivery.action_id],
        )
        .expect("mutate Blood Pact detail");
    drop(connection);
    let error = service
        .settle_blood_pact_delivery(DigRuntimeSettleBloodPact {
            action_id: delivery.action_id,
            source_key: delivery.source_key,
            occurred_at: now,
        })
        .expect_err("changed Blood Pact earning must fail closed");
    assert!(matches!(error, DigRuntimeStoreError::BloodPact(_)));
    assert!(error.to_string().contains("different payload"));
}

#[test]
fn sqlite_delivery_channel_rebind_is_persisted_and_pending_part_guarded() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(7, "delivery-rebind", Some(9)))
        .expect("seed player");
    let service = DigRuntimeService::sqlite(database.path());
    let delivery = service
        .dig_with_delivery(
            DigRuntimeRequest {
                discord_id: 7,
                guild_id: 9,
                now: 1_700_000_000,
                paid: false,
                forced_event: false,
            },
            DigRuntimeDeliveryContext::new(99, 101, "Delivery Rebind", None),
        )
        .expect("dig and outbox commit")
        .delivery
        .expect("delivery snapshot");
    let rebound = service
        .rebind_pending_delivery_channel(DigRuntimeRebindDeliveryChannel {
            action_id: delivery.action_id,
            source_key: delivery.source_key.clone(),
            part: DigRuntimeDeliveryPart::Main,
            expected_channel_id: 101,
            fallback_channel_id: 202,
        })
        .expect("pending delivery channel rebind");
    assert_eq!(rebound.context.channel_id, 202);
    let restarted = DigRuntimeService::sqlite(database.path());
    let recovered = restarted
        .pending_deliveries(DigRuntimePendingDeliveryQuery {
            guild_id: Some(9),
            discord_id: Some(7),
            limit: 10,
        })
        .expect("restart pending delivery");
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].context.channel_id, 202);
    assert!(matches!(
        restarted.rebind_pending_delivery_channel(DigRuntimeRebindDeliveryChannel {
            action_id: delivery.action_id,
            source_key: delivery.source_key.clone(),
            part: DigRuntimeDeliveryPart::Main,
            expected_channel_id: 101,
            fallback_channel_id: 303,
        }),
        Err(DigRuntimeStoreError::StateConflict)
    ));
    assert!(
        restarted
            .mark_delivery_delivered(DigRuntimeMarkDelivered {
                action_id: delivery.action_id,
                source_key: delivery.source_key.clone(),
                delivered_at: 1_700_000_001,
                part: DigRuntimeDeliveryPart::Main,
            })
            .expect("mark rebound delivery")
    );
    assert!(matches!(
        restarted.rebind_pending_delivery_channel(DigRuntimeRebindDeliveryChannel {
            action_id: delivery.action_id,
            source_key: delivery.source_key,
            part: DigRuntimeDeliveryPart::Main,
            expected_channel_id: 202,
            fallback_channel_id: 303,
        }),
        Err(DigRuntimeStoreError::StateConflict)
    ));
}

#[test]
fn sqlite_delivery_outbox_invalid_detail_rolls_back_actor_and_action() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(7, "delivery-rollback", Some(9)))
        .expect("seed player");
    let store = SqliteDigRuntimeStore::new(database.path());
    let service = DigRuntimeService::new(store.clone());
    let first = service
        .dig(DigRuntimeRequest {
            discord_id: 7,
            guild_id: 9,
            now: 1_700_000_000,
            paid: false,
            forced_event: false,
        })
        .expect("seed first dig");
    let before = store.snapshot(7, 9).expect("snapshot before rollback");
    let mut next = before.clone();
    let tunnel = next.tunnel.as_mut().expect("seed tunnel");
    tunnel.depth = tunnel.depth.saturating_add(1);
    tunnel.total_digs = tunnel.total_digs.saturating_add(1);
    tunnel.last_dig_at = Some(1_700_000_001);
    let request = DigRuntimeCommit {
        expected: DigRuntimeVersion::from(&before),
        next,
        delivery_draft: None,
        consumed_item_ids: Vec::new(),
        pet_work_claim: None,
        consume_overgrowth: false,
        depth_before: before.tunnel.as_ref().map_or(0, |tunnel| tunnel.depth),
        depth_after: before
            .tunnel
            .as_ref()
            .map_or(1, |tunnel| tunnel.depth.saturating_add(1)),
        jc_delta: 0,
        vanity_tax: 0,
        balance_cost: 0,
        action_type: "dig".to_owned(),
        detail: "[]".to_owned(),
        now: 1_700_000_001,
    };
    let error = store
        .commit_with_delivery(
            request,
            DigRuntimeDeliveryDraft {
                discord_id: 7,
                guild_id: 9,
                outcome: first,
                context: DigRuntimeDeliveryContext::new(99, 11, "Delivery Rollback", None),
                committed_at: 1_700_000_001,
            },
        )
        .expect_err("invalid detail must abort the transaction");
    assert!(matches!(error, DigRuntimeStoreError::InvalidJson(_)));
    assert_eq!(
        store.snapshot(7, 9).expect("snapshot after rollback"),
        before
    );
    let action_count: i64 = Connection::open(database.path())
        .expect("reopen database")
        .query_row(
            "SELECT COUNT(*) FROM dig_actions WHERE actor_id=?1 AND guild_id=?2",
            params![7_i64, 9_i64],
            |row| row.get(0),
        )
        .expect("action count");
    assert_eq!(action_count, 1);
}

#[test]
fn stale_snapshot_is_rejected_by_cas() {
    let snapshot = DigRuntimeSnapshot::fresh(7, 9, 100, 1_700_000_000);
    let store = InMemoryDigRuntimeStore::new(snapshot.clone());
    let service = DigRuntimeService::new(store.clone());
    service
        .dig(DigRuntimeRequest {
            discord_id: 7,
            guild_id: 9,
            now: 1_700_000_000,
            paid: false,
            forced_event: false,
        })
        .expect("first commit");
    let stale = super::DigRuntimeCommit {
        expected: super::DigRuntimeVersion::from(&snapshot),
        next: snapshot,
        delivery_draft: None,
        consumed_item_ids: Vec::new(),
        pet_work_claim: None,
        consume_overgrowth: false,
        depth_before: 0,
        depth_after: 1,
        jc_delta: 1,
        vanity_tax: 0,
        balance_cost: 0,
        action_type: "dig".to_owned(),
        detail: "{}".to_owned(),
        now: 1_700_000_001,
    };
    assert!(matches!(
        store.commit(stale),
        Err(super::DigRuntimeStoreError::Conflict)
    ));
}

#[test]
fn migrated_sqlite_store_commits_and_reloads_the_full_stage() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    let connection = Connection::open(database.path()).expect("open migrated database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match Python foreign-key behavior");
    connection
        .execute(
            "INSERT INTO players
                 (discord_id,guild_id,discord_username,jopacoin_balance)
                 VALUES (?1,?2,?3,?4)",
            params![-9001_i64, 42_i64, "runtime-test", 100_i64],
        )
        .expect("insert player");
    drop(connection);

    let store = SqliteDigRuntimeStore::new(database.path());
    let service = DigRuntimeService::new(store.clone());
    let outcome = service
        .dig(DigRuntimeRequest {
            discord_id: -9001,
            guild_id: 42,
            now: 1_700_000_000,
            paid: false,
            forced_event: false,
        })
        .expect("migrated SQLite first dig");
    assert!(outcome.success);
    let reloaded = store.snapshot(-9001, 42).expect("reload snapshot");
    assert_eq!(reloaded.balance, 100 + outcome.jc_earned);
    assert_eq!(
        reloaded.tunnel.as_ref().map(|tunnel| tunnel.total_digs),
        Some(1)
    );
    assert_eq!(reloaded.gear.len(), 1);
    assert_eq!(reloaded.gear[0].slot, "weapon");
    assert_eq!(reloaded.gear[0].tier, 0);
    assert_eq!(reloaded.gear[0].durability, 20);
    assert!(reloaded.gear[0].equipped);
    assert_eq!(reloaded.gear[0].source, "starter");
    let action_count: i64 = Connection::open(database.path())
        .expect("reopen database")
        .query_row(
            "SELECT COUNT(*) FROM dig_actions WHERE actor_id=?1 AND guild_id=?2",
            params![-9001_i64, 42_i64],
            |row| row.get(0),
        )
        .expect("action audit");
    assert_eq!(action_count, 1);
}

#[test]
fn paid_dig_debits_exactly_once_and_records_a_distinct_cost_ledger_entry() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    let connection = Connection::open(database.path()).expect("open migrated database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match Python foreign-key behavior");
    connection
        .execute(
            "INSERT INTO players
                 (discord_id,guild_id,discord_username,jopacoin_balance)
                 VALUES (?1,?2,?3,?4)",
            params![-9_006_i64, 42_i64, "paid-once", 100_i64],
        )
        .expect("insert player");
    connection
        .execute(
            "INSERT INTO tunnels
                 (discord_id,guild_id,depth,max_depth,total_digs,last_dig_at)
                 VALUES (?1,?2,10,10,1,?3)",
            params![-9_006_i64, 42_i64, 1_700_000_000_i64],
        )
        .expect("insert tunnel");
    connection
        .execute("DELETE FROM economy_ledger_entries", [])
        .expect("clear setup ledger");
    drop(connection);

    let outcome = DigRuntimeService::sqlite(database.path())
        .dig(DigRuntimeRequest {
            discord_id: -9_006,
            guild_id: 42,
            now: 1_700_000_001,
            paid: true,
            forced_event: false,
        })
        .expect("paid dig transaction");
    assert!(outcome.success);
    assert_eq!(outcome.paid_dig_cost, 3);

    let connection = Connection::open(database.path()).expect("reopen paid database");
    let balance = connection
        .query_row(
            "SELECT jopacoin_balance FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
            params![-9_006_i64, 42_i64],
            |row| row.get::<_, i64>(0),
        )
        .expect("paid balance");
    assert_eq!(balance, 100 - outcome.paid_dig_cost + outcome.jc_earned);
    assert_eq!(balance, outcome.balance_after);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE account_id=?1 AND guild_id=?2 AND delta=?3
                       AND source='dig' AND reason='paid dig cost'",
                params![-9_006_i64, 42_i64, -outcome.paid_dig_cost],
                |row| row.get::<_, i64>(0),
            )
            .expect("paid cost ledger count"),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COALESCE(SUM(delta),0) FROM economy_ledger_entries
                     WHERE account_id=?1 AND guild_id=?2",
                params![-9_006_i64, 42_i64],
                |row| row.get::<_, i64>(0),
            )
            .expect("paid ledger net"),
        balance - 100
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT paid_digs_today FROM tunnels
                     WHERE discord_id=?1 AND guild_id=?2",
                params![-9_006_i64, 42_i64],
                |row| row.get::<_, i64>(0),
            )
            .expect("paid counter"),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("ledger context cleared"),
        0
    );
}

#[test]
fn sqlite_commit_preserves_distinct_trailing_tunnel_columns() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    let connection = Connection::open(database.path()).expect("open migrated database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match Python foreign-key behavior");
    connection
        .execute(
            "INSERT INTO players
                 (discord_id,guild_id,discord_username,jopacoin_balance)
                 VALUES (?1,?2,?3,?4)",
            params![-9002_i64, 42_i64, "trailing-columns", 100_i64],
        )
        .expect("insert player");
    connection
        .execute(
            "INSERT INTO tunnels
                 (discord_id,guild_id,depth,max_depth,total_digs,last_dig_at,
                  pinnacle_last_engaged_at,retreat_cooldown_until,last_cheer_at,
                  cavein_free_streak,relic_trim_notice)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                -9002_i64, 42_i64, 10_i64, 20_i64, 30_i64, 40_i64, 51_i64, 62_i64, 73_i64, 84_i64,
                1_i64,
            ],
        )
        .expect("insert tunnel sentinels");
    drop(connection);

    let store = SqliteDigRuntimeStore::new(database.path());
    let snapshot = store.snapshot(-9002, 42).expect("snapshot");
    let mut next = snapshot.clone();
    let tunnel = next.tunnel.as_mut().expect("tunnel");
    tunnel.depth = 11;
    tunnel.max_depth = 21;
    store
        .commit(super::DigRuntimeCommit {
            expected: super::DigRuntimeVersion::from(&snapshot),
            next,
            delivery_draft: None,
            consumed_item_ids: Vec::new(),
            pet_work_claim: None,
            consume_overgrowth: false,
            depth_before: 10,
            depth_after: 11,
            jc_delta: 0,
            vanity_tax: 0,
            balance_cost: 0,
            action_type: "dig".to_owned(),
            detail: "{}".to_owned(),
            now: 1_700_000_001,
        })
        .expect("commit stage");

    let reloaded = store.snapshot(-9002, 42).expect("reload snapshot");
    let tunnel = reloaded.tunnel.expect("persisted tunnel");
    assert_eq!(tunnel.pinnacle_last_engaged_at, Some(51));
    assert_eq!(tunnel.retreat_cooldown_until, Some(62));
    assert_eq!(tunnel.last_cheer_at, Some(73));
    assert_eq!(tunnel.cavein_free_streak, 84);
    assert!(tunnel.relic_trim_notice);
}

#[test]
fn admin_tunnel_mutations_report_missing_and_preserve_permanent_depth() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    let connection = Connection::open(database.path()).expect("open migrated database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match Python foreign-key behavior");
    connection
        .execute(
            "INSERT INTO tunnels
                 (discord_id,guild_id,depth,max_depth,last_dig_at)
                 VALUES (?1,?2,?3,?4,?5)",
            params![-9_003_i64, 42_i64, 75_i64, 275_i64, 1_700_000_000_i64],
        )
        .expect("insert tunnel sentinels");
    drop(connection);

    let service = DigRuntimeService::sqlite(database.path());
    assert_eq!(
        service
            .set_depth(-9_003, 42, 12)
            .expect("set current depth"),
        DigAdminMutationOutcome::Applied
    );
    let (depth, max_depth, last_dig_at) = Connection::open(database.path())
        .expect("reopen migrated database")
        .query_row(
            "SELECT depth,max_depth,last_dig_at FROM tunnels
                 WHERE discord_id=?1 AND guild_id=?2",
            params![-9_003_i64, 42_i64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .expect("read admin mutation");
    assert_eq!((depth, max_depth, last_dig_at), (12, 275, 0));

    assert_eq!(
        service
            .reset_cooldown(-9_003, 42)
            .expect("reset existing cooldown"),
        DigAdminMutationOutcome::Applied
    );
    assert_eq!(
        service
            .set_depth(-9_999, 42, 12)
            .expect("missing set-depth target"),
        DigAdminMutationOutcome::MissingTunnel
    );
    assert_eq!(
        service
            .reset_cooldown(-9_999, 42)
            .expect("missing reset target"),
        DigAdminMutationOutcome::MissingTunnel
    );
}

#[test]
fn flex_projection_normalizes_boss_shapes_titles_and_prestige_badge() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(-9_004, "flex-test", Some(42)))
        .expect("insert player through the canonical repository");
    let connection = Connection::open(database.path()).expect("open migrated database");
    connection
            .execute(
                "INSERT INTO tunnels
                 (discord_id,guild_id,tunnel_name,depth,total_digs,total_jc_earned,
                  prestige_level,streak_days,boss_progress)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    -9_004_i64,
                    42_i64,
                    "The Test Shaft",
                    205_i64,
                    123_i64,
                    456_i64,
                    8_i64,
                    9_i64,
                    r#"{"25":"defeated","50":{"status":"defeated"},"75":"defeated","100":{"status":"defeated"},"150":"defeated","200":"defeated","275":{"status":"defeated"}}"#,
                ],
            )
            .expect("insert flex tunnel");
    drop(connection);

    let flex = DigRuntimeService::sqlite(database.path())
        .flex_data(-9_004, 42)
        .expect("flex projection")
        .expect("existing tunnel");
    assert_eq!(flex.tunnel_name, "The Test Shaft");
    assert_eq!(flex.depth, 205);
    assert_eq!(flex.total_digs, 123);
    assert_eq!(flex.total_jc_earned, 456);
    assert_eq!(flex.prestige_level, 8);
    assert_eq!(flex.prestige_emoji, "⭐⭐⭐⭐⭐");
    assert_eq!(flex.titles, ["Boss Slayer"]);
    assert_eq!(flex.streak, 9);
    assert_eq!(flex.layer, "Frozen Core");

    Connection::open(database.path())
        .expect("reopen migrated database")
        .execute(
            "UPDATE tunnels SET boss_progress=?1
                 WHERE discord_id=?2 AND guild_id=?3",
            params![r#"{"25":"defeated"}"#, -9_004_i64, 42_i64],
        )
        .expect("make boss progress partial");
    let partial = DigRuntimeService::sqlite(database.path())
        .flex_data(-9_004, 42)
        .expect("partial flex projection")
        .expect("existing tunnel");
    assert!(partial.titles.is_empty());
    assert_eq!(
        DigRuntimeService::sqlite(database.path())
            .flex_data(-9_999, 42)
            .expect("missing flex projection"),
        None
    );
}

#[test]
fn test_dig_atomic_balance_update_records_dig_context() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    let connection = Connection::open(database.path()).expect("open migrated database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match Python foreign-key behavior");
    connection
        .execute(
            "INSERT INTO players
                 (discord_id,guild_id,discord_username,jopacoin_balance)
                 VALUES (?1,?2,?3,?4)",
            params![-301_i64, 42_i64, "event-test", 10_i64],
        )
        .expect("insert player");
    connection
        .execute(
            "INSERT INTO tunnels (discord_id,guild_id,depth)
                 VALUES (?1,?2,0)",
            params![-301_i64, 42_i64],
        )
        .expect("insert tunnel");
    connection
        .execute("DELETE FROM economy_ledger_entries", [])
        .expect("clear setup ledger");
    drop(connection);

    let detail = r#"{"event_id":"crystal_garden","choice":"safe"}"#;
    let action_id = SqliteDigRuntimeStore::new(database.path())
        .atomic_tunnel_balance_update(super::AtomicTunnelBalanceUpdate {
            discord_id: -301,
            guild_id: 42,
            balance_delta: 5,
            balance_cost: 0,
            depth_after: Some(3),
            detail,
            action_type: "event",
            now: 2_000_000_005,
        })
        .expect("dig balance update");
    let connection = Connection::open(database.path()).expect("reopen database");
    let ledger = connection
        .query_row(
            "SELECT delta,source,actor_id,related_type,related_id,reason,metadata
                   FROM economy_ledger_entries",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .expect("event ledger row");
    assert_eq!(ledger.0, 5);
    assert_eq!(ledger.1, "dig");
    assert_eq!(ledger.2, Some(-301));
    assert_eq!(ledger.3.as_deref(), Some("event"));
    assert_eq!(ledger.4.as_deref(), Some("crystal_garden"));
    assert_eq!(ledger.5.as_deref(), Some("dig event credit"));
    assert_eq!(ledger.6, detail);
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players
                      WHERE discord_id=?1 AND guild_id=?2",
                params![-301_i64, 42_i64],
                |row| row.get::<_, i64>(0),
            )
            .expect("balance"),
        15
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT depth FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![-301_i64, 42_i64],
                |row| row.get::<_, i64>(0),
            )
            .expect("depth"),
        3
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT action_type,jc_delta FROM dig_actions WHERE id=?1",
                [action_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .expect("event action"),
        ("event".to_owned(), 5)
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("context cleared"),
        0
    );
}

#[test]
fn test_dig_atomic_paid_cost_rejects_insufficient_funds() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    let connection = Connection::open(database.path()).expect("open migrated database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match Python foreign-key behavior");
    connection
        .execute(
            "INSERT INTO players
                 (discord_id,guild_id,discord_username,jopacoin_balance)
                 VALUES (?1,?2,?3,?4)",
            params![-302_i64, 42_i64, "paid-event-test", 2_i64],
        )
        .expect("insert player");
    connection
        .execute(
            "INSERT INTO tunnels (discord_id,guild_id,depth)
                 VALUES (?1,?2,0)",
            params![-302_i64, 42_i64],
        )
        .expect("insert tunnel");
    connection
        .execute("DELETE FROM economy_ledger_entries", [])
        .expect("clear setup ledger");
    drop(connection);

    let result = SqliteDigRuntimeStore::new(database.path()).atomic_tunnel_balance_update(
        super::AtomicTunnelBalanceUpdate {
            discord_id: -302,
            guild_id: 42,
            balance_delta: 0,
            balance_cost: 3,
            depth_after: Some(3),
            detail: r#"{"event_id":"crystal_garden","choice":"safe"}"#,
            action_type: "event",
            now: 2_000_000_006,
        },
    );
    assert!(matches!(
        result,
        Err(super::DigRuntimeStoreError::InsufficientFunds)
    ));
    let connection = Connection::open(database.path()).expect("reopen database");
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players
                      WHERE discord_id=?1 AND guild_id=?2",
                params![-302_i64, 42_i64],
                |row| row.get::<_, i64>(0),
            )
            .expect("balance"),
        2
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT depth FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![-302_i64, 42_i64],
                |row| row.get::<_, i64>(0),
            )
            .expect("depth"),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM economy_ledger_entries", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("no failed settlement ledger row"),
        0
    );
}

#[test]
fn test_miner_respec_records_sink_context_and_action() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    let connection = Connection::open(database.path()).expect("open migrated database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match Python foreign-key behavior");
    connection
        .execute(
            "INSERT INTO players
                 (discord_id,guild_id,discord_username,jopacoin_balance)
                 VALUES (?1,?2,?3,?4)",
            params![-401_i64, 42_i64, "respec-test", 100_i64],
        )
        .expect("insert player");
    connection
        .execute(
            "INSERT INTO tunnels
                 (discord_id,guild_id,stat_strength,stat_smarts,stat_stamina,stat_points)
                 VALUES (?1,?2,?3,?4,?5,?6)",
            params![-401_i64, 42_i64, 2_i64, 1_i64, 7_i64, 12_i64],
        )
        .expect("insert miner tunnel");
    connection
        .execute("DELETE FROM economy_ledger_entries", [])
        .expect("clear setup ledger");
    drop(connection);

    let service = DigRuntimeService::sqlite(database.path());
    let result = service.respec(-401, 42, 2_000_000_003).expect("respec");
    assert!(result.contains("Respec complete"));
    let repeated = service
        .respec(-401, 42, 2_000_000_004)
        .expect("repeat respec");
    assert!(repeated.contains("allocated S points"));

    let connection = Connection::open(database.path()).expect("reopen database");
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players
                      WHERE discord_id=?1 AND guild_id=?2",
                params![-401_i64, 42_i64],
                |row| row.get::<_, i64>(0),
            )
            .expect("balance"),
        50
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT stat_strength,stat_smarts,stat_stamina,stat_points
                       FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![-401_i64, 42_i64],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .expect("miner stats"),
        (0, 0, 0, 22)
    );
    let ledger = connection
        .query_row(
            "SELECT delta,source,actor_id,related_type,related_id,reason,metadata
                   FROM economy_ledger_entries",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .expect("respec ledger row");
    assert_eq!(ledger.0, -50);
    assert_eq!(ledger.1, "dig");
    assert_eq!(ledger.2, Some(-401));
    assert_eq!(ledger.3.as_deref(), Some("miner_respec"));
    assert_eq!(ledger.4.as_deref(), Some("s_points"));
    assert_eq!(ledger.5.as_deref(), Some("dig miner respec debit"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&ledger.6).expect("ledger metadata"),
        serde_json::json!({
            "cost": 50,
            "returned_points": 10,
            "previous_stats": {"strength": 2, "smarts": 1, "stamina": 7},
        })
    );
    let action = connection
        .query_row(
            "SELECT action_type,jc_delta,detail FROM dig_actions
                  WHERE actor_id=?1 AND guild_id=?2 ORDER BY id",
            params![-401_i64, 42_i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .expect("respec action");
    assert_eq!(action.0, "miner_respec");
    assert_eq!(action.1, -50);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&action.2).expect("action detail"),
        serde_json::json!({
            "cost": 50,
            "returned_points": 10,
            "previous_stats": {"strength": 2, "smarts": 1, "stamina": 7},
        })
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM economy_ledger_entries", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("only first respec was charged"),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                .get::<_, i64>(0))
            .expect("only first respec was audited"),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("context cleared"),
        0
    );
}

#[test]
fn migrated_sqlite_defense_and_weather_actions_use_app_boundary() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(9_001, "defense-test", Some(42)))
        .expect("insert player");
    let service = DigRuntimeService::sqlite(database.path());
    service
        .dig(DigRuntimeRequest {
            discord_id: 9_001,
            guild_id: 42,
            now: 1_700_000_000,
            paid: false,
            forced_event: false,
        })
        .expect("first dig");

    assert!(matches!(
        service.set_trap(9_001, 42, "2023-11-14"),
        Ok(SetTrapOutcome::Set {
            cost: 0,
            balance_after: _
        })
    ));
    assert!(matches!(
        service.buy_insurance(9_001, 42, 1_700_000_000),
        Ok(BuyInsuranceOutcome::Purchased {
            cost: 5,
            expires_at: 1_700_086_400,
            balance_after: _
        })
    ));
    let weather = service
        .weather(42, "2023-11-14", 1_700_000_000)
        .expect("weather through app service");
    assert_eq!(weather.len(), 2);
    assert!(weather.iter().all(|entry| entry.definition().is_some()));
}

#[test]
fn first_dig_defers_weather_and_second_dig_returns_stable_typed_effects() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(9_005, "weather-test", Some(42)))
        .expect("insert player");
    let service = DigRuntimeService::sqlite(database.path());
    service
        .dig(DigRuntimeRequest {
            discord_id: 9_005,
            guild_id: 42,
            now: 1_700_000_000,
            paid: false,
            forced_event: false,
        })
        .expect("first dig");
    let connection = Connection::open(database.path()).expect("open weather database");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_weather", [], |row| row
                .get::<_, i64>(0))
            .expect("weather row count"),
        0
    );
    connection
        .execute(
            "UPDATE tunnels SET last_dig_at=?1 WHERE discord_id=?2 AND guild_id=?3",
            params![1_699_990_000_i64, 9_005_i64, 42_i64],
        )
        .expect("clear cooldown");
    drop(connection);

    let outcome = service
        .dig(DigRuntimeRequest {
            discord_id: 9_005,
            guild_id: 42,
            now: 1_700_003_601,
            paid: false,
            forced_event: false,
        })
        .expect("second dig");
    assert!(outcome.success);
    let first = service
        .weather_projection(42, "2023-11-14", 1_700_003_601)
        .expect("typed weather projection");
    let second = service
        .weather_projection(42, "2023-11-14", 1_700_003_999)
        .expect("stable weather projection");
    assert_eq!(first, second);
    assert_eq!(first.len(), 2);
    assert!(first.iter().any(|weather| weather.layer == "Dirt"));
    assert!(first.iter().all(|weather| {
        !weather.name.is_empty()
            && !weather.description.is_empty()
            && weather.effects != super::DigWeatherEffects::neutral()
    }));
}

#[test]
fn test_weather_effects_in_dig_result_live_sqlite_second_dig() {
    let database = NamedTempFile::new().expect("temporary weather result database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(9_006, "weather-result-test", Some(42)))
        .expect("insert weather result player");
    let service = DigRuntimeService::sqlite(database.path());
    let first = service
        .dig(DigRuntimeRequest {
            discord_id: 9_006,
            guild_id: 42,
            now: 1_700_000_000,
            paid: false,
            forced_event: false,
        })
        .expect("first dig");
    assert!(first.success);
    assert!(first.weather.is_none(), "first Dig defers weather");

    let second_now = 1_700_003_601;
    let today = game_date_for_timestamp(second_now as f64).expect("weather result date");
    seed_two_weather_rows(&database, 42, &today, "Dirt", "earthworm_migration");
    Connection::open(database.path())
        .expect("reopen weather result database")
        .execute(
            "UPDATE tunnels SET last_dig_at=?1 WHERE discord_id=?2 AND guild_id=?3",
            params![second_now - 10_000_i64, 9_006_i64, 42_i64],
        )
        .expect("clear weather result cooldown");

    let execution = service
        .dig_with_delivery(
            DigRuntimeRequest {
                discord_id: 9_006,
                guild_id: 42,
                now: second_now,
                paid: false,
                forced_event: false,
            },
            DigRuntimeDeliveryContext::new(90_060, 420, "Weather Miner", None),
        )
        .expect("second live Dig");
    let weather = execution
        .outcome
        .weather
        .as_ref()
        .expect("affected-layer weather in live Dig result");
    assert_eq!(weather.name, "Earthworm Migration");
    assert_eq!(
        weather.description,
        "Worms churn the soil. Digging is easy, but they ate all the coins."
    );
    let delivery = execution.delivery.expect("durable render snapshot");
    assert_eq!(delivery.render.weather.as_ref(), Some(weather));
    let mut legacy = serde_json::to_value(&delivery).expect("serialize delivery snapshot");
    legacy["render"]
        .as_object_mut()
        .expect("render object")
        .remove("weather");
    let restored = serde_json::from_value::<super::DigRuntimeDeliverySnapshot>(legacy)
        .expect("legacy delivery snapshot remains readable");
    assert!(restored.render.weather.is_none());
}

#[test]
fn sqlite_pet_work_first_dig_is_settled_and_claimed_atomically() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(7, "pet-dig", Some(9)))
        .expect("insert player");
    let connection = Connection::open(database.path()).expect("open database");
    let pet_id = seed_runtime_pet(&connection, 1_800_000_000, 0);
    drop(connection);

    let service = DigRuntimeService::with_config(
        SqliteDigRuntimeStore::new(database.path()),
        super::DigRuntimeConfig::default().with_pet_decay_per_day(20),
    );
    let outcome = service
        .dig(DigRuntimeRequest {
            discord_id: 7,
            guild_id: 9,
            now: 1_800_000_000,
            paid: false,
            forced_event: false,
        })
        .expect("first pet-assisted dig");
    assert!(outcome.success);
    assert_eq!(outcome.pet_dig_bonus, 12);
    assert_eq!(outcome.pet_name.as_deref(), Some("Blep"));

    let connection = Connection::open(database.path()).expect("reopen database");
    assert_eq!(
        connection
            .query_row(
                "SELECT dig_work_units,dig_work_at FROM pets WHERE pet_id=?1",
                [pet_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .expect("settled pet work"),
        (10 * PET_DAY + PET_DAY / 5, 1_800_000_000)
    );
}

#[test]
fn sqlite_pet_work_cave_in_does_not_consume_the_offer() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(7, "pet-cave", Some(9)))
        .expect("insert player");
    let connection = Connection::open(database.path()).expect("open database");
    connection
        .execute(
            "INSERT INTO tunnels (
                     discord_id,guild_id,tunnel_name,depth,max_depth,total_digs,
                     last_dig_at,prestige_perks,boss_progress,boss_attempts
                 ) VALUES (7,9,'Pet Mine',10,10,1,0,'[]','{}','{}')",
            [],
        )
        .expect("seed tunnel");
    connection
        .execute(
            "INSERT INTO pets (
                     discord_id,guild_id,name,species,adopted_at,hatched_at,
                     adopt_fee,last_fed_at,hunger_at_last_fed,
                     dig_work_units,dig_work_at
                 ) VALUES (7,9,'Blep','common_cama',?1,?1,0,?1,100,?2,?1)",
            params![1_800_000_000_i64, 12 * PET_DAY],
        )
        .expect("seed pet");
    drop(connection);

    let now = (1_800_000_000_i64..1_800_010_000)
        .find(|candidate| {
            super::SeededLootEntropy::new(super::seed_for(DigRuntimeRequest {
                discord_id: 7,
                guild_id: 9,
                now: *candidate,
                paid: false,
                forced_event: false,
            }))
            .unit()
                < 0.05
        })
        .expect("deterministic cave-in seed");
    let service = DigRuntimeService::with_config(
        SqliteDigRuntimeStore::new(database.path()),
        super::DigRuntimeConfig::default().with_pet_decay_per_day(20),
    );
    let outcome = service
        .dig(DigRuntimeRequest {
            discord_id: 7,
            guild_id: 9,
            now,
            paid: false,
            forced_event: false,
        })
        .expect("cave-in dig");
    assert!(outcome.cave_in);
    assert_eq!(outcome.pet_dig_bonus, 0);
    let connection = Connection::open(database.path()).expect("reopen database");
    assert_eq!(
        connection
            .query_row("SELECT dig_work_units FROM pets", [], |row| row
                .get::<_, i64>(0))
            .expect("pet work"),
        12 * PET_DAY
    );
}

#[test]
fn sqlite_pet_work_conflict_rolls_back_the_whole_commit() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(7, "pet-conflict", Some(9)))
        .expect("insert player");
    let connection = Connection::open(database.path()).expect("open database");
    connection
        .execute(
            "INSERT INTO tunnels (
                     discord_id,guild_id,tunnel_name,depth,max_depth,total_digs,
                     last_dig_at,prestige_perks,boss_progress,boss_attempts
                 ) VALUES (7,9,'Pet Mine',10,10,1,0,'[]','{}','{}')",
            [],
        )
        .expect("seed tunnel");
    connection
        .execute(
            "INSERT INTO pets (
                     discord_id,guild_id,name,species,adopted_at,hatched_at,
                     adopt_fee,last_fed_at,hunger_at_last_fed,
                     dig_work_units,dig_work_at
                 ) VALUES (7,9,'Blep','common_cama',1,1,0,1,100,?1,1)",
            [12 * PET_DAY],
        )
        .expect("seed pet");
    let pet_id = connection.last_insert_rowid();
    drop(connection);

    let store = SqliteDigRuntimeStore::new(database.path());
    let snapshot = store.snapshot(7, 9).expect("snapshot");
    let mut next = snapshot.clone();
    next.tunnel.as_mut().expect("tunnel").depth = 20;
    let error = store
        .commit(super::DigRuntimeCommit {
            expected: super::DigRuntimeVersion::from(&snapshot),
            next,
            delivery_draft: None,
            consumed_item_ids: Vec::new(),
            pet_work_claim: Some(cama_domain::pet::PetDigWorkClaim {
                pet_id,
                expected_units: 13 * PET_DAY,
                expected_at: 1,
                new_units: 0,
                new_at: 1,
            }),
            consume_overgrowth: false,
            depth_before: 10,
            depth_after: 20,
            jc_delta: 0,
            vanity_tax: 0,
            balance_cost: 0,
            action_type: "dig".to_owned(),
            detail: "{}".to_owned(),
            now: 1_800_000_000,
        })
        .expect_err("stale pet claim");
    assert!(matches!(
        error,
        super::DigRuntimeStoreError::PetWorkConflict
    ));
    let connection = Connection::open(database.path()).expect("reopen database");
    assert_eq!(
        connection
            .query_row("SELECT depth FROM tunnels", [], |row| row.get::<_, i64>(0))
            .expect("depth"),
        10
    );
}

#[test]
fn test_drain_at_cap_is_double() {
    let depth = super::PRESTIGE_HARD_CAP - 1;
    let drain = live_prestige_luminosity_drain(62_101, 62_102, depth);
    let expected =
        super::layer_at(depth).luminosity_drain + super::deep_luminosity_drain_bonus(depth);
    assert_eq!(super::deep_luminosity_drain_bonus(depth), 7);
    assert_eq!(expected, 17);
    assert_eq!(drain.returned, expected);
    assert_eq!(drain.persisted, expected);
}

#[test]
fn test_drain_at_start_depth_matches_base() {
    let depth = super::LUMINOSITY_DEEP_DRAIN_START_DEPTH;
    let drain = live_prestige_luminosity_drain(62_111, 62_112, depth);
    let expected = super::layer_at(depth).luminosity_drain;
    assert_eq!(super::deep_luminosity_drain_bonus(depth), 0);
    assert_eq!(expected, 10, "The Hollow's authored base drain");
    assert_eq!(drain.returned, expected);
    assert_eq!(drain.persisted, expected);
}

#[test]
fn test_drain_below_start_depth_unchanged() {
    let depth = super::LUMINOSITY_DEEP_DRAIN_START_DEPTH - 100;
    let drain = live_prestige_luminosity_drain(62_121, 62_122, depth);
    let expected = super::layer_at(depth).luminosity_drain;
    assert_eq!(super::deep_luminosity_drain_bonus(depth), 0);
    assert_eq!(expected, 7, "Frozen Core's authored base drain");
    assert_eq!(drain.returned, expected);
    assert_eq!(drain.persisted, expected);
}

#[test]
fn test_drain_increases_monotonically() {
    let drains = [350_i64, 400, 450]
        .into_iter()
        .enumerate()
        .map(|(index, depth)| {
            live_prestige_luminosity_drain(
                62_131 + i64::try_from(index).expect("small test index"),
                62_140 + i64::try_from(index).expect("small test index"),
                depth,
            )
        })
        .collect::<Vec<_>>();
    let returned = drains
        .iter()
        .map(|drain| drain.returned)
        .collect::<Vec<_>>();
    let persisted = drains
        .iter()
        .map(|drain| drain.persisted)
        .collect::<Vec<_>>();
    assert_eq!(returned, [10, 12, 15]);
    assert_eq!(persisted, returned);
    assert!(
        returned.windows(2).all(|pair| pair[0] < pair[1]),
        "{returned:?}"
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LivePrestigeLuminosityDrain {
    returned: i64,
    persisted: i64,
}

fn live_prestige_luminosity_drain(
    discord_id: i64,
    guild_id: i64,
    depth: i64,
) -> LivePrestigeLuminosityDrain {
    let database = NamedTempFile::new().expect("prestige drain database");
    let seed_now = 1_900_300_000;
    seed_live_runtime_tunnel(
        &database,
        discord_id,
        guild_id,
        seed_now,
        depth,
        1,
        Some(seed_now - 7_200),
    );
    let now = find_non_cave_dig_time_for_depth(discord_id, guild_id, seed_now, depth);
    let today = game_date_for_timestamp(now as f64).expect("prestige drain game date");
    let boss_progress = r#"{"25":{"status":"defeated"},"50":{"status":"defeated"},"75":{"status":"defeated"},"100":{"status":"defeated"},"150":{"status":"defeated"},"200":{"status":"defeated"},"275":{"status":"defeated"},"350":{"status":"defeated"}}"#;
    let layer = super::layer_at(depth).name;
    let connection = Connection::open(database.path()).expect("prestige drain connection");
    connection
        .execute(
            "UPDATE tunnels
                 SET depth=?1,max_depth=?1,luminosity=100,last_dig_at=0,
                     last_lum_update_at=?2,boss_progress=?3
                 WHERE discord_id=?4 AND guild_id=?5",
            params![depth, now, boss_progress, discord_id, guild_id],
        )
        .expect("seed prestige drain tunnel");
    // The Python fixture disables weather.  Two unknown persisted IDs
    // provide the same neutral mechanical effect while still exercising
    // the migrated weather lookup and its two-row invariant.
    connection
        .execute(
            "INSERT INTO dig_weather(guild_id,game_date,layer_name,weather_id)
                 VALUES(?1,?2,?3,'prestige_drain_neutral')",
            params![guild_id, today, layer],
        )
        .expect("seed neutral active weather");
    connection
        .execute(
            "INSERT INTO dig_weather(guild_id,game_date,layer_name,weather_id)
                 VALUES(?1,?2,'Dirt','prestige_drain_neutral_other')",
            params![guild_id, today],
        )
        .expect("seed neutral secondary weather");
    drop(connection);

    let service = DigRuntimeService::sqlite(database.path());
    let before = service
        .tunnel_info(discord_id, guild_id)
        .expect("read prestige drain input")
        .expect("prestige drain tunnel")
        .luminosity;
    let outcome = service
        .dig(DigRuntimeRequest {
            discord_id,
            guild_id,
            now,
            paid: false,
            forced_event: false,
        })
        .expect("live prestige drain Dig");
    assert!(
        outcome.success,
        "live prestige drain Dig failed at depth {depth}: {:?}",
        outcome.error
    );
    assert_eq!(
        outcome.boss_boundary, None,
        "prestige drain fixture must clear bosses"
    );
    assert!(
        outcome.action_id.is_some(),
        "live Dig must persist an action"
    );

    // `tunnel_info` is the typed application return path used by the
    // runtime adapter; compare it with the direct migrated-SQLite row so
    // the test covers both the returned and durable luminosity values.
    let returned_after = service
        .tunnel_info(discord_id, guild_id)
        .expect("read returned prestige drain tunnel")
        .expect("returned prestige drain tunnel")
        .luminosity;
    let persisted_after = Connection::open(database.path())
        .expect("reload prestige drain database")
        .query_row(
            "SELECT luminosity FROM tunnels
                 WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| row.get::<_, i64>(0),
        )
        .expect("read persisted prestige luminosity");
    assert_eq!(returned_after, persisted_after);
    LivePrestigeLuminosityDrain {
        returned: before.saturating_sub(returned_after),
        persisted: before.saturating_sub(persisted_after),
    }
}

fn find_non_cave_dig_time_for_depth(discord_id: i64, guild_id: i64, start: i64, depth: i64) -> i64 {
    let cave_chance = super::layer_at(depth).cave_in_chance;
    (start..start.saturating_add(100_000))
        .find(|candidate| {
            let mut entropy = SeededLootEntropy::new(super::seed_for(DigRuntimeRequest {
                discord_id,
                guild_id,
                now: *candidate,
                paid: false,
                forced_event: false,
            }));
            entropy.unit() >= cave_chance
        })
        .expect("deterministic non-cave prestige drain seed")
}

fn seed_cap_tunnel(database: &NamedTempFile, depth: i64, luminosity: i64) {
    initialize_or_migrate(database.path()).expect("canonical migration");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(50_001, "cap-test", Some(50_002)))
        .expect("seed player");
    let connection = Connection::open(database.path()).expect("cap database");
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=777
                 WHERE discord_id=50001 AND guild_id=50002",
            [],
        )
        .expect("seed balance");
    connection
        .execute(
            "INSERT INTO tunnels(
                     discord_id,guild_id,tunnel_name,depth,max_depth,total_digs,
                     total_jc_earned,last_dig_at,luminosity,prestige_level,
                     prestige_perks,boss_progress,boss_attempts
                 ) VALUES(50001,50002,'Cap Tunnel',?1,?1,1,0,0,?2,1,'[]','{}','{}')",
            params![depth, luminosity],
        )
        .expect("seed tunnel");
}

fn live_blood_pact_delivery_fixture() -> (
    NamedTempFile,
    DigRuntimeService<SqliteDigRuntimeStore>,
    super::DigRuntimeExecution,
    i64,
) {
    let database = NamedTempFile::new().expect("Blood Pact delivery database");
    let actor = 61_101;
    let skimmer = 61_102;
    let guild = 61_103;
    let start = 1_900_120_000;
    seed_live_runtime_tunnel(&database, actor, guild, start, 76, 1, Some(start - 7_200));
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(skimmer, "blood-pact-skimmer", Some(guild)))
        .expect("seed Blood Pact skimmer");
    let now = find_non_cave_dig_time_with_jc(actor, guild, start, 76, 3);
    Connection::open(database.path())
            .expect("Blood Pact fixture connection")
            .execute(
                "UPDATE tunnels SET boss_progress=?1
                 WHERE discord_id=?2 AND guild_id=?3",
                params![
                    r#"{"25":{"status":"defeated"},"50":{"status":"defeated"},"75":{"status":"defeated"}}"#,
                    actor,
                    guild,
                ],
            )
            .expect("seed defeated Blood Pact bosses");
    Connection::open(database.path())
        .expect("Blood Pact buff connection")
        .execute(
            "INSERT INTO manashop_buffs(
                     discord_id,guild_id,buff_type,target_id,granted_at,expires_at,
                     triggered,data
                 ) VALUES(?1,?2,'blood_pact',?3,?4,?5,0,?6)",
            params![
                skimmer,
                guild,
                actor,
                now - 1,
                now + 86_400,
                serde_json::json!({
                    "skimmed_total": 0,
                    "cap": 100,
                    "skim_rate": 1.0,
                })
                .to_string(),
            ],
        )
        .expect("seed Blood Pact buff");
    let service = DigRuntimeService::sqlite(database.path());
    let execution = service
        .dig_with_delivery(
            DigRuntimeRequest {
                discord_id: actor,
                guild_id: guild,
                now,
                paid: false,
                forced_event: false,
            },
            DigRuntimeDeliveryContext::new(61_104, 61_105, "Blood Pact Miner", None),
        )
        .expect("Blood Pact delivery Dig");
    assert!(execution.outcome.success);
    assert!(!execution.outcome.cave_in);
    assert!(execution.outcome.jc_earned > 0);
    assert_eq!(
        execution
            .delivery
            .as_ref()
            .expect("Blood Pact delivery")
            .blood_pact,
        DigRuntimeBloodPactSnapshot::Pending
    );
    (database, service, execution, now)
}

#[test]
fn test_dig_at_cap_is_rejected() {
    let database = NamedTempFile::new().expect("cap database");
    seed_cap_tunnel(&database, super::PRESTIGE_HARD_CAP, 77);
    let service = DigRuntimeService::sqlite(database.path());
    let before = Connection::open(database.path()).expect("open cap database");
    let before_balance = before
        .query_row(
            "SELECT jopacoin_balance FROM players
                 WHERE discord_id=50001 AND guild_id=50002",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("balance");
    drop(before);

    let result = service
        .dig(DigRuntimeRequest {
            discord_id: 50_001,
            guild_id: 50_002,
            now: 1_900_000_000,
            paid: false,
            forced_event: true,
        })
        .expect("hard cap response");
    assert!(!result.success);
    assert!(
        result
            .error
            .as_deref()
            .is_some_and(|message| message.contains("prestige cap"))
    );
    assert_eq!(result.depth_before, super::PRESTIGE_HARD_CAP);
    assert_eq!(result.depth_after, super::PRESTIGE_HARD_CAP);
    assert_eq!(result.jc_earned, 0);

    let connection = Connection::open(database.path()).expect("reopen cap database");
    assert_eq!(
        connection
            .query_row("SELECT jopacoin_balance FROM players", [], |row| row
                .get::<_, i64>(0))
            .expect("balance unchanged"),
        before_balance
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT depth,luminosity,last_dig_at FROM tunnels",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                }
            )
            .expect("tunnel unchanged"),
        (super::PRESTIGE_HARD_CAP, 77, 0)
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_weather", [], |row| row
                .get::<_, i64>(0))
            .expect("weather unchanged"),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                .get::<_, i64>(0))
            .expect("no audit action"),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM pets", [], |row| row.get::<_, i64>(0))
            .expect("no pet mutation"),
        0
    );
}

#[test]
fn test_dig_one_below_cap_allowed() {
    let database = NamedTempFile::new().expect("cap database");
    seed_cap_tunnel(&database, super::PRESTIGE_HARD_CAP - 1, 100);
    let service = DigRuntimeService::sqlite(database.path());
    let result = service
        .dig(DigRuntimeRequest {
            discord_id: 50_001,
            guild_id: 50_002,
            now: 1_900_000_000,
            paid: false,
            forced_event: false,
        })
        .expect("cap-1 dig response");
    assert!(
        !result
            .error
            .as_deref()
            .is_some_and(|message| message.contains("prestige cap"))
    );
    assert_eq!(
        Connection::open(database.path())
            .expect("reopen cap-1 database")
            .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                .get::<_, i64>(0))
            .expect("cap-1 action"),
        1
    );
}

#[test]
fn test_helltide_modifier_taxes_dig_yield() {
    fn seed(database: &NamedTempFile) {
        initialize_or_migrate(database.path()).expect("canonical migration");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(51_001, "helltide-test", Some(51_002)))
            .expect("seed player");
        let connection = Connection::open(database.path()).expect("helltide database");
        connection
            .execute(
                "UPDATE players SET jopacoin_balance=777
                     WHERE discord_id=51001 AND guild_id=51002",
                [],
            )
            .expect("seed balance");
        connection
            .execute(
                "INSERT INTO tunnels(
                         discord_id,guild_id,tunnel_name,depth,max_depth,total_digs,
                         total_jc_earned,last_dig_at,luminosity,prestige_level,
                         prestige_perks,boss_progress,boss_attempts
                     ) VALUES(51001,51002,'Helltide Tunnel',10,10,1,0,0,100,0,'[]','{}','{}')",
                [],
            )
            .expect("seed tunnel");
    }

    let baseline_db = NamedTempFile::new().expect("baseline database");
    seed(&baseline_db);
    let baseline = DigRuntimeService::sqlite(baseline_db.path())
        .dig(DigRuntimeRequest {
            discord_id: 51_001,
            guild_id: 51_002,
            now: 1_910_000_000,
            paid: false,
            forced_event: false,
        })
        .expect("baseline dig");

    let helltide_db = NamedTempFile::new().expect("helltide database");
    seed(&helltide_db);
    DigGuildModifierRepository::new(helltide_db.path())
        .set_modifier_at(
            Some(51_002),
            "helltide_active",
            600,
            &serde_json::json!({"tax_per_dig": 5}),
            1_910_000_000,
        )
        .expect("activate helltide");
    let taxed = DigRuntimeService::sqlite(helltide_db.path())
        .dig(DigRuntimeRequest {
            discord_id: 51_001,
            guild_id: 51_002,
            now: 1_910_000_000,
            paid: false,
            forced_event: false,
        })
        .expect("taxed dig");

    assert!(baseline.success && taxed.success);
    assert!(taxed.jc_earned <= baseline.jc_earned);
    assert!(taxed.balance_after >= 0);
    let baseline_detail = baseline
        .action_id
        .and_then(|action_id| {
            Connection::open(baseline_db.path())
                .ok()?
                .query_row(
                    "SELECT detail FROM dig_actions WHERE id=?1",
                    [action_id],
                    |row| row.get::<_, String>(0),
                )
                .ok()
        })
        .expect("inactive tax detail");
    assert!(baseline_detail.contains("\"helltide_tax\":0"));
    let detail = taxed
        .action_id
        .and_then(|action_id| {
            Connection::open(helltide_db.path())
                .ok()?
                .query_row(
                    "SELECT detail FROM dig_actions WHERE id=?1",
                    [action_id],
                    |row| row.get::<_, String>(0),
                )
                .ok()
        })
        .expect("tax detail");
    assert!(detail.contains("\"helltide_tax\":5"));
}

#[test]
fn test_slow_drip_tracks_gross_cap_but_credits_scaled_reward() {
    const ACTOR: i64 = 52_001;
    const GUILD: i64 = 52_002;
    const NOW: i64 = 1_700_000_000;
    let database = NamedTempFile::new().expect("slow drip database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(ACTOR, "slow-drip-test", Some(GUILD)))
        .expect("seed player");
    let connection = Connection::open(database.path()).expect("slow drip connection");
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=100
                 WHERE discord_id=?1 AND guild_id=?2",
            params![ACTOR, GUILD],
        )
        .expect("seed balance");
    connection
        .execute(
            "INSERT INTO tunnels(
                     discord_id,guild_id,tunnel_name,depth,max_depth,total_digs,
                     total_jc_earned,last_dig_at,luminosity,prestige_level,
                     prestige_perks,boss_progress,boss_attempts
                 ) VALUES(?1,?2,'Slow Drip Tunnel',10,10,1,0,?3,100,0,'[]','{}','{}')",
            params![ACTOR, GUILD, NOW - 4_000],
        )
        .expect("seed tunnel");
    connection
        .execute(
            "INSERT INTO dig_artifacts(
                     discord_id,guild_id,artifact_id,found_at,is_relic,equipped
                 ) VALUES(?1,?2,'slow_drip',?3,1,1)",
            params![ACTOR, GUILD, NOW - 5_000],
        )
        .expect("equip slow drip");
    let claim_date =
        cama_domain::game_date::game_date_for_timestamp(NOW as f64).expect("claim date");
    connection
        .execute(
            "INSERT INTO slow_drip_claims(
                     discord_id,guild_id,claim_date,claimed_today,last_claim_at
                 ) VALUES(?1,?2,?3,0,?4)",
            params![ACTOR, GUILD, claim_date, NOW - 1_200],
        )
        .expect("seed slow drip anchor");
    drop(connection);

    EconomyEventRepository::new(database.path())
        .activate_event_atomic(
            Some(GUILD),
            &EventDraft {
                event_date: cama_domain::game_date::game_date_for_timestamp(NOW as f64)
                    .expect("event date"),
                name: "Double slow drip".to_owned(),
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
                announcement: "Double slow drip".to_owned(),
                starts_at: NOW - 60,
                ends_at: NOW + 60,
                created_at: NOW - 60,
            },
        )
        .expect("activate slow drip economy event");

    let mut config = DigRuntimeConfig::default();
    config.economy_event.enabled = true;
    let service = DigRuntimeService::sqlite_with_config(database.path(), config);
    let outcome = service
        .dig(DigRuntimeRequest {
            discord_id: ACTOR,
            guild_id: GUILD,
            now: NOW,
            paid: false,
            forced_event: false,
        })
        .expect("slow drip Dig");
    assert!(outcome.success);
    let connection = Connection::open(database.path()).expect("reload slow drip");
    let (claimed_today, last_claim_at): (i64, i64) = connection
        .query_row(
            "SELECT claimed_today,last_claim_at FROM slow_drip_claims
                 WHERE discord_id=?1 AND guild_id=?2 AND claim_date=?3",
            params![ACTOR, GUILD, claim_date],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("persisted gross claim");
    assert_eq!((claimed_today, last_claim_at), (10, NOW));
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players
                     WHERE discord_id=?1 AND guild_id=?2",
                params![ACTOR, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("slow drip wallet"),
        outcome.balance_after
    );
    assert!(outcome.balance_after >= 113);
    let action_detail: String = connection
        .query_row(
            "SELECT detail FROM dig_actions WHERE id=?1",
            [outcome.action_id.expect("slow drip action")],
            |row| row.get(0),
        )
        .expect("slow drip action detail");
    let action_json: Value = serde_json::from_str(&action_detail).expect("action JSON");
    assert_eq!(action_json["slow_drip"]["gross_jc"], 10);
    assert_eq!(action_json["slow_drip"]["credit_jc"], 13);
    let metadata: String = connection
        .query_row(
            "SELECT metadata FROM economy_ledger_entries
                 WHERE guild_id=?1 AND actor_id=?2 AND source='dig'
                   AND related_type='slow_drip'
                 ORDER BY ledger_id DESC LIMIT 1",
            params![GUILD, ACTOR],
            |row| row.get(0),
        )
        .expect("slow drip ledger context");
    let metadata_json: Value = serde_json::from_str(&metadata).expect("ledger metadata JSON");
    assert_eq!(metadata_json["gross_jc"], 10);
    assert_eq!(metadata_json["credit_jc"], 13);

    // A second request cannot backfill or double-claim the same anchor;
    // the first dig's cooldown blocks it before any wallet/claim write.
    let before_retry = connection
        .query_row(
            "SELECT COUNT(*) FROM economy_ledger_entries
                 WHERE guild_id=?1 AND actor_id=?2 AND source='dig'
                   AND related_type='slow_drip'",
            params![GUILD, ACTOR],
            |row| row.get::<_, i64>(0),
        )
        .expect("ledger count before retry");
    drop(connection);
    let retry = service
        .dig(DigRuntimeRequest {
            discord_id: ACTOR,
            guild_id: GUILD,
            now: NOW,
            paid: false,
            forced_event: false,
        })
        .expect("slow drip retry");
    assert!(!retry.success);
    assert_eq!(
        Connection::open(database.path())
            .expect("reload retry")
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE guild_id=?1 AND actor_id=?2 AND source='dig'
                       AND related_type='slow_drip'",
                params![GUILD, ACTOR],
                |row| row.get::<_, i64>(0),
            )
            .expect("ledger count after retry"),
        before_retry
    );
}

#[test]
fn test_parked_boss_reopens_after_slow_drip_before_cooldown() {
    const ACTOR: i64 = 52_101;
    const GUILD: i64 = 52_102;
    const LAST_DIG: i64 = 1_700_000_000;
    const NOW: i64 = LAST_DIG + 10;
    let database = NamedTempFile::new().expect("parked boss database");
    initialize_or_migrate(database.path()).expect("canonical migration");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(ACTOR, "parked-boss", Some(GUILD)))
        .expect("seed player");
    let connection = Connection::open(database.path()).expect("parked boss connection");
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=100
                 WHERE discord_id=?1 AND guild_id=?2",
            params![ACTOR, GUILD],
        )
        .expect("seed balance");
    connection
        .execute(
            "INSERT INTO tunnels(
                     discord_id,guild_id,tunnel_name,depth,max_depth,total_digs,
                     total_jc_earned,last_dig_at,luminosity,prestige_level,
                     prestige_perks,boss_progress,boss_attempts
                 ) VALUES(?1,?2,'Parked Boss',24,24,1,0,?3,100,0,'[]',?4,'{}')",
            params![ACTOR, GUILD, LAST_DIG, r#"{"25":"active"}"#],
        )
        .expect("seed parked tunnel");
    connection
        .execute(
            "INSERT INTO dig_artifacts(
                     discord_id,guild_id,artifact_id,found_at,is_relic,equipped
                 ) VALUES(?1,?2,'slow_drip',?3,1,1)",
            params![ACTOR, GUILD, LAST_DIG - 100],
        )
        .expect("equip slow drip");
    let claim_date =
        cama_domain::game_date::game_date_for_timestamp(NOW as f64).expect("claim date");
    connection
        .execute(
            "INSERT INTO slow_drip_claims(
                     discord_id,guild_id,claim_date,claimed_today,last_claim_at
                 ) VALUES(?1,?2,?3,0,?4)",
            params![ACTOR, GUILD, claim_date, LAST_DIG - 600],
        )
        .expect("seed slow drip");
    drop(connection);

    let outcome = DigRuntimeService::sqlite(database.path())
        .dig(DigRuntimeRequest {
            discord_id: ACTOR,
            guild_id: GUILD,
            now: NOW,
            paid: true,
            forced_event: false,
        })
        .expect("parked boss reopen");
    assert!(outcome.success);
    assert_eq!(outcome.boss_boundary, Some(25));
    assert_eq!(outcome.advance, 0);
    assert_eq!(outcome.jc_earned, 0);
    assert_eq!(outcome.balance_after, 103);
    let connection = Connection::open(database.path()).expect("reload parked boss");
    assert_eq!(
        connection
            .query_row("SELECT claimed_today FROM slow_drip_claims", [], |row| row
                .get::<_, i64>(
                0
            ))
            .expect("gross claim"),
        5
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                .get::<_, i64>(0))
            .expect("no parked action"),
        0
    );
}

#[test]
fn test_broken_gear_is_not_an_applicable_gear_nick_target() {
    let actor = 62_021;
    let guild = 62_022;
    let seed_now = 1_900_091_000;
    let roll = find_live_cave_roll(actor, guild, seed_now, 180, Some(false), None, false);
    let database = NamedTempFile::new().expect("broken-gear cave database");
    seed_live_cave_in_fixture(
        &database,
        actor,
        guild,
        seed_now,
        roll.now,
        180,
        0,
        Some(0),
        None,
    );

    let outcome = DigRuntimeService::sqlite(database.path())
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now: roll.now,
            paid: false,
            forced_event: false,
        })
        .expect("broken-gear cave Dig");
    let detail = serde_json::from_str::<Value>(
        outcome
            .cave_in_detail
            .as_deref()
            .expect("broken-gear cave detail"),
    )
    .expect("broken-gear cave detail JSON");
    assert!(outcome.success && outcome.cave_in);
    assert!(matches!(
        detail.get("type").and_then(Value::as_str),
        Some("stun" | "injury" | "medical_bill")
    ));
    assert!(detail.get("gear_broken").is_none());
    assert_eq!(detail["block_loss"], roll.block_loss);
    assert_eq!(outcome.depth_after, 180 - roll.block_loss);

    let connection = Connection::open(database.path()).expect("reload broken-gear cave");
    let gear = connection
        .query_row(
            "SELECT durability,equipped FROM dig_gear
                 WHERE discord_id=?1 AND guild_id=?2 AND slot='armor'",
            params![actor, guild],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .expect("persisted broken gear");
    assert_eq!(gear, (0, 1));
    assert_eq!(
        latest_dig_detail(&database, actor, guild)["block_loss"],
        roll.block_loss
    );
}

#[test]
fn test_gear_nick_reports_newly_broken_piece() {
    let actor = 62_001;
    let guild = 62_002;
    let seed_now = 1_900_090_000;
    let roll = find_live_cave_roll(actor, guild, seed_now, 180, Some(false), None, true);
    let database = NamedTempFile::new().expect("gear-nick cave database");
    seed_live_cave_in_fixture(
        &database,
        actor,
        guild,
        seed_now,
        roll.now,
        180,
        0,
        Some(1),
        None,
    );

    let outcome = DigRuntimeService::sqlite(database.path())
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now: roll.now,
            paid: false,
            forced_event: false,
        })
        .expect("gear-nick cave Dig");
    let detail = serde_json::from_str::<Value>(
        outcome
            .cave_in_detail
            .as_deref()
            .expect("gear-nick cave detail"),
    )
    .expect("gear-nick cave detail JSON");
    assert!(outcome.success && outcome.cave_in);
    assert_eq!(detail["type"], "gear_nick");
    assert_eq!(detail["block_loss"], roll.block_loss);
    assert_eq!(
        detail["gear_broken"],
        serde_json::json!([ARMOR_TIERS[1].name])
    );
    assert_eq!(outcome.depth_after, 180 - roll.block_loss);
    assert_eq!(outcome.jc_earned, 0);
    assert_eq!(outcome.balance_after, 10_000);

    let action = latest_dig_detail(&database, actor, guild);
    assert_eq!(action["block_loss"], roll.block_loss);
    assert_eq!(action["cave_in_detail"]["block_loss"], roll.block_loss);
    let connection = Connection::open(database.path()).expect("reload gear-nick cave");
    assert_eq!(
        connection
            .query_row(
                "SELECT durability,equipped FROM dig_gear
                     WHERE discord_id=?1 AND guild_id=?2 AND slot='armor'",
                params![actor, guild],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .expect("persisted gear-nick gear"),
        (0, 1)
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE account_id=?1 AND guild_id=?2 AND source='dig'",
                params![actor, guild],
                |row| row.get::<_, i64>(0),
            )
            .expect("gear-nick ledger count"),
        0
    );
}

#[test]
fn test_catastrophic_cave_in_reports_newly_broken_piece() {
    let actor = 62_011;
    let guild = 62_012;
    let seed_now = 1_900_090_500;
    let roll = find_live_cave_roll(actor, guild, seed_now, 180, Some(true), None, false);
    let database = NamedTempFile::new().expect("catastrophic gear cave database");
    seed_live_cave_in_fixture(
        &database,
        actor,
        guild,
        seed_now,
        roll.now,
        180,
        0,
        Some(1),
        None,
    );

    let outcome = DigRuntimeService::sqlite(database.path())
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now: roll.now,
            paid: false,
            forced_event: false,
        })
        .expect("catastrophic gear cave Dig");
    let detail = serde_json::from_str::<Value>(
        outcome
            .cave_in_detail
            .as_deref()
            .expect("catastrophic gear cave detail"),
    )
    .expect("catastrophic gear cave detail JSON");
    assert!(outcome.success && outcome.cave_in);
    assert_eq!(detail["type"], "catastrophic");
    assert_eq!(detail["depth_after"], 175);
    assert_eq!(detail["block_loss"], roll.block_loss);
    assert_eq!(detail["insurance_saved"], false);
    assert_eq!(
        detail["gear_broken"],
        serde_json::json!([ARMOR_TIERS[1].name])
    );
    let jc_lost = detail["jc_lost"].as_i64().expect("catastrophic JC loss");
    assert!((50..=200).contains(&jc_lost));
    assert!(
        !detail["message"]
            .as_str()
            .unwrap_or_default()
            .contains("{jc_lost}")
    );

    let action = latest_dig_detail(&database, actor, guild);
    assert_eq!(action["block_loss"], roll.block_loss);
    assert_eq!(action["cave_in_detail"]["block_loss"], roll.block_loss);
    let connection = Connection::open(database.path()).expect("reload catastrophic gear cave");
    assert_eq!(
        connection
            .query_row(
                "SELECT jc_delta FROM dig_actions
                     WHERE actor_id=?1 AND guild_id=?2 ORDER BY id DESC LIMIT 1",
                params![actor, guild],
                |row| row.get::<_, i64>(0),
            )
            .expect("catastrophic action delta"),
        -jc_lost
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT durability,equipped FROM dig_gear
                     WHERE discord_id=?1 AND guild_id=?2 AND slot='armor'",
                params![actor, guild],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .expect("persisted catastrophic gear"),
        (0, 1)
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players
                     WHERE discord_id=?1 AND guild_id=?2",
                params![actor, guild],
                |row| row.get::<_, i64>(0),
            )
            .expect("catastrophic balance"),
        10_000 - jc_lost
    );
    let ledger = connection
        .query_row(
            "SELECT delta,source,reason FROM economy_ledger_entries
                 WHERE account_id=?1 AND guild_id=?2 AND source='dig'",
            params![actor, guild],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .expect("catastrophic ledger");
    assert_eq!(ledger, (-jc_lost, "dig".to_owned(), "dig debit".to_owned()));
}

#[test]
fn test_force_cave_in_at_deep() {
    let actor = 62_031;
    let guild = 62_032;
    let seed_now = 1_900_091_500;
    let roll = find_live_cave_roll(actor, guild, seed_now, 180, None, None, false);
    let database = NamedTempFile::new().expect("deep cave database");
    seed_live_cave_in_fixture(
        &database, actor, guild, seed_now, roll.now, 180, 100, None, None,
    );

    let outcome = DigRuntimeService::sqlite(database.path())
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now: roll.now,
            paid: false,
            forced_event: false,
        })
        .expect("deep cave Dig");
    let detail =
        serde_json::from_str::<Value>(outcome.cave_in_detail.as_deref().expect("deep cave detail"))
            .expect("deep cave detail JSON");
    assert!(outcome.success && outcome.cave_in);
    assert!(matches!(
        detail.get("type").and_then(Value::as_str),
        Some(
            "stun"
                | "injury"
                | "medical_bill"
                | "gear_nick"
                | "spilled_satchel"
                | "snuffed_light"
                | "cracked_hat"
                | "catastrophic"
        )
    ));
    assert_eq!(detail["block_loss"], roll.block_loss);
    assert!((12..=25).contains(&detail["block_loss"].as_i64().unwrap_or_default()));
    let expected_depth = if roll.catastrophic {
        175
    } else {
        180 - roll.block_loss
    };
    assert_eq!(outcome.depth_after, expected_depth);
    let action = latest_dig_detail(&database, actor, guild);
    assert_eq!(action["block_loss"], roll.block_loss);
    assert_eq!(action["cave_in_detail"]["block_loss"], roll.block_loss);
}

#[test]
fn test_catastrophic_overrides_to_milestone() {
    let actor = 62_041;
    let guild = 62_042;
    let seed_now = 1_900_092_000;
    // Require a raw loss below the 15-block milestone rollback so this
    // test catches an audit field that accidentally records final depth
    // delta instead of Python's ordinary rolled loss.
    let roll = find_live_cave_roll(actor, guild, seed_now, 240, Some(true), Some(15), false);
    let database = NamedTempFile::new().expect("milestone cave database");
    seed_live_cave_in_fixture(
        &database, actor, guild, seed_now, roll.now, 240, 100, None, None,
    );

    let outcome = DigRuntimeService::sqlite(database.path())
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now: roll.now,
            paid: false,
            forced_event: false,
        })
        .expect("milestone cave Dig");
    let detail = serde_json::from_str::<Value>(
        outcome
            .cave_in_detail
            .as_deref()
            .expect("milestone cave detail"),
    )
    .expect("milestone cave detail JSON");
    assert!(outcome.success && outcome.cave_in);
    assert_eq!(detail["type"], "catastrophic");
    assert_eq!(detail["depth_after"], 225);
    assert_eq!(detail["block_loss"], 15);
    assert_eq!(detail["insurance_saved"], false);
    let jc_lost = detail["jc_lost"].as_i64().expect("milestone JC loss");
    assert!((50..=200).contains(&jc_lost));
    assert_eq!(outcome.depth_after, 225);

    let action = latest_dig_detail(&database, actor, guild);
    assert_eq!(action["block_loss"], roll.block_loss);
    assert_eq!(action["cave_in_detail"]["block_loss"], 15);
    let connection = Connection::open(database.path()).expect("reload milestone cave");
    assert_eq!(
        connection
            .query_row(
                "SELECT jc_delta FROM dig_actions
                     WHERE actor_id=?1 AND guild_id=?2 ORDER BY id DESC LIMIT 1",
                params![actor, guild],
                |row| row.get::<_, i64>(0),
            )
            .expect("milestone action delta"),
        -jc_lost
    );
    let persisted = connection
        .query_row(
            "SELECT depth,temp_buffs,cavein_free_streak,injury_state
                 FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![actor, guild],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .expect("persisted milestone cave");
    assert_eq!(persisted.0, 225);
    assert_eq!(persisted.1, None);
    assert_eq!(persisted.2, 0);
    let injury = persisted.3.expect("persisted catastrophic injury");
    assert_eq!(
        serde_json::from_str::<Value>(&injury).expect("injury JSON")["type"],
        "slower_cooldown"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players
                     WHERE discord_id=?1 AND guild_id=?2",
                params![actor, guild],
                |row| row.get::<_, i64>(0),
            )
            .expect("milestone balance"),
        10_000 - jc_lost
    );
    let ledger = connection
        .query_row(
            "SELECT delta,source,reason FROM economy_ledger_entries
                 WHERE account_id=?1 AND guild_id=?2 AND source='dig'",
            params![actor, guild],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .expect("milestone ledger");
    assert_eq!(ledger, (-jc_lost, "dig".to_owned(), "dig debit".to_owned()));
}

#[test]
fn test_insurance_protects_catastrophic_depth() {
    let actor = 62_051;
    let guild = 62_052;
    let seed_now = 1_900_092_500;
    let roll = find_live_cave_roll(actor, guild, seed_now, 240, Some(true), None, false);
    let database = NamedTempFile::new().expect("insured cave database");
    let insured_until = roll.now + 86_400;
    seed_live_cave_in_fixture(
        &database,
        actor,
        guild,
        seed_now,
        roll.now,
        240,
        100,
        None,
        Some(insured_until),
    );

    let outcome = DigRuntimeService::sqlite(database.path())
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now: roll.now,
            paid: false,
            forced_event: false,
        })
        .expect("insured cave Dig");
    let detail = serde_json::from_str::<Value>(
        outcome
            .cave_in_detail
            .as_deref()
            .expect("insured cave detail"),
    )
    .expect("insured cave detail JSON");
    assert!(outcome.success && outcome.cave_in);
    assert_eq!(detail["type"], "catastrophic");
    assert_eq!(detail["insurance_saved"], true);
    assert_eq!(detail["depth_after"], 240 - roll.block_loss);
    assert_eq!(detail["block_loss"], roll.block_loss);
    let jc_lost = detail["jc_lost"].as_i64().expect("insured JC loss");
    assert!((50..=200).contains(&jc_lost));
    assert_eq!(outcome.depth_after, 240 - roll.block_loss);

    let action = latest_dig_detail(&database, actor, guild);
    assert_eq!(action["block_loss"], roll.block_loss);
    assert_eq!(action["cave_in_detail"]["block_loss"], roll.block_loss);
    let connection = Connection::open(database.path()).expect("reload insured cave");
    assert_eq!(
        connection
            .query_row(
                "SELECT jc_delta FROM dig_actions
                     WHERE actor_id=?1 AND guild_id=?2 ORDER BY id DESC LIMIT 1",
                params![actor, guild],
                |row| row.get::<_, i64>(0),
            )
            .expect("insured action delta"),
        -jc_lost
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT depth,insured_until,cavein_free_streak
                     FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![actor, guild],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .expect("persisted insured cave"),
        (240 - roll.block_loss, Some(insured_until), 0)
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players
                     WHERE discord_id=?1 AND guild_id=?2",
                params![actor, guild],
                |row| row.get::<_, i64>(0),
            )
            .expect("insured balance"),
        10_000 - jc_lost
    );
    let ledger = connection
        .query_row(
            "SELECT delta,source,reason FROM economy_ledger_entries
                 WHERE account_id=?1 AND guild_id=?2 AND source='dig'",
            params![actor, guild],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .expect("insured ledger");
    assert_eq!(ledger, (-jc_lost, "dig".to_owned(), "dig debit".to_owned()));
}

#[derive(Clone, Copy, Debug)]
struct LiveCaveRoll {
    now: i64,
    block_loss: i64,
    catastrophic: bool,
}

fn find_live_cave_roll(
    discord_id: i64,
    guild_id: i64,
    start: i64,
    depth: i64,
    required_catastrophic: Option<bool>,
    maximum_block_loss: Option<i64>,
    require_gear_nick: bool,
) -> LiveCaveRoll {
    let layer = super::layer_at(depth);
    let band = cama_domain::dig_cave_in::cave_in_band(depth);
    let (minimum, maximum) = CAVE_IN_BLOCK_LOSS_RANGES[band];
    let catastrophic_probability = CAVE_IN_CATASTROPHIC_PCT_BY_BAND[band];
    for now in start..start.saturating_add(100_000) {
        let mut entropy = SeededLootEntropy::new(super::seed_for(DigRuntimeRequest {
            discord_id,
            guild_id,
            now,
            paid: false,
            forced_event: false,
        }));
        // The fixture has no hazard modifier and starts at luminosity 0
        // or 100, so a <.05 first draw is safely inside either live cave
        // probability while keeping the search independent of weather.
        if entropy.unit() >= 0.05 {
            continue;
        }
        let _advance = entropy.advance(layer.advance_range.0, layer.advance_range.1);
        let block_loss = entropy.advance(i64::from(minimum), i64::from(maximum));
        let catastrophic = entropy.unit() < catastrophic_probability;
        if required_catastrophic.is_some_and(|required| required != catastrophic) {
            continue;
        }
        if maximum_block_loss.is_some_and(|maximum| block_loss >= maximum) {
            continue;
        }
        if require_gear_nick {
            // With one durable armor piece, no inventory, zero luminosity,
            // and no hat charges, the deep filtered weight is 80 and the
            // Gear Nick interval is 66..=80.
            if catastrophic || !matches!(entropy.advance(1, 80), 66..=80) {
                continue;
            }
        }
        return LiveCaveRoll {
            now,
            block_loss,
            catastrophic,
        };
    }
    panic!(
        "no deterministic live cave roll for depth {depth} and requirements \
             catastrophic={required_catastrophic:?}, max_loss={maximum_block_loss:?}, \
             gear_nick={require_gear_nick}"
    );
}

#[allow(clippy::too_many_arguments)]
fn seed_live_cave_in_fixture(
    database: &NamedTempFile,
    discord_id: i64,
    guild_id: i64,
    seed_now: i64,
    dig_now: i64,
    depth: i64,
    luminosity: i64,
    gear_durability: Option<i64>,
    insured_until: Option<i64>,
) {
    seed_live_runtime_tunnel(
        database,
        discord_id,
        guild_id,
        seed_now,
        depth,
        1,
        Some(seed_now - 7_200),
    );
    let connection = Connection::open(database.path()).expect("live cave fixture connection");
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=10000
                 WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
        )
        .expect("seed live cave balance");
    connection
            .execute(
                "UPDATE tunnels SET boss_progress=?1,luminosity=?2,pickaxe_tier=1,
                    hard_hat_charges=0,grappling_hook_charges=0,reinforced_until=0,
                    route_state=NULL,temp_buffs=NULL,temp_curses=NULL,mutations='[]',
                    prestige_perks='[]',insured_until=?3
                 WHERE discord_id=?4 AND guild_id=?5",
                params![
                    r#"{"25":"defeated","50":"defeated","75":"defeated","100":"defeated","150":"defeated","200":"defeated","275":"defeated"}"#,
                    luminosity,
                    insured_until,
                    discord_id,
                    guild_id,
                ],
            )
            .expect("seed live cave tunnel");
    if let Some(durability) = gear_durability {
        connection
            .execute(
                "INSERT INTO dig_gear(
                         discord_id,guild_id,slot,tier,durability,equipped,
                         acquired_at,source,item_id
                     ) VALUES(?1,?2,'armor',1,?3,1,?4,'cave-test',NULL)",
                params![discord_id, guild_id, durability, seed_now],
            )
            .expect("seed live cave gear");
    }
    drop(connection);
    let today = game_date_for_timestamp(dig_now as f64).expect("live cave date");
    let weather = if depth >= 201 {
        "time_dilation"
    } else {
        "spore_bloom"
    };
    seed_two_weather_rows(
        database,
        guild_id,
        &today,
        super::layer_at(depth).name,
        weather,
    );
}

fn seed_live_runtime_tunnel(
    database: &NamedTempFile,
    discord_id: i64,
    guild_id: i64,
    now: i64,
    depth: i64,
    total_digs: i64,
    last_dig_at: Option<i64>,
) {
    initialize_or_migrate(database.path()).expect("canonical migration");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(discord_id, "batch-one", Some(guild_id)))
        .expect("seed player");
    let connection = Connection::open(database.path()).expect("open database");
    connection
        .execute(
            "INSERT INTO tunnels (
                     discord_id,guild_id,tunnel_name,depth,max_depth,total_digs,
                     last_dig_at,luminosity,last_lum_update_at,prestige_perks,
                     boss_progress,boss_attempts
                 ) VALUES (?1,?2,'Batch One',?3,?3,?4,?5,100,?6,'[]','{}','{}')",
            params![discord_id, guild_id, depth, total_digs, last_dig_at, now],
        )
        .expect("seed tunnel");
}

fn grant_overgrowth(
    database: &NamedTempFile,
    discord_id: i64,
    guild_id: i64,
    now: i64,
    charges: i64,
) {
    let data = BuffData {
        charges_remaining: Some(charges),
        ..BuffData::default()
    };
    ManashopRepository::new(database.path())
        .grant_buff(GrantBuffRequest {
            discord_id,
            guild_id: Some(guild_id),
            buff_type: "overgrowth",
            target_id: None,
            granted_at: now,
            expires_at: now.saturating_add(3_600),
            data: Some(&data),
        })
        .expect("grant Overgrowth");
}

fn find_non_cave_dig_time(discord_id: i64, guild_id: i64, start: i64) -> i64 {
    (start..start.saturating_add(10_000))
        .find(|candidate| {
            super::SeededLootEntropy::new(super::seed_for(super::DigRuntimeRequest {
                discord_id,
                guild_id,
                now: *candidate,
                paid: false,
                forced_event: false,
            }))
            .unit()
                > 0.30
        })
        .expect("deterministic non-cave seed")
}

fn find_cave_dig_time(discord_id: i64, guild_id: i64, start: i64) -> i64 {
    (start..start.saturating_add(10_000))
        .find(|candidate| {
            super::SeededLootEntropy::new(super::seed_for(super::DigRuntimeRequest {
                discord_id,
                guild_id,
                now: *candidate,
                paid: false,
                forced_event: false,
            }))
            .unit()
                < 0.05
        })
        .expect("deterministic cave seed")
}

fn find_paid_cave_dig_time(discord_id: i64, guild_id: i64, start: i64) -> i64 {
    (start..start.saturating_add(10_000))
        .find(|candidate| {
            super::SeededLootEntropy::new(super::seed_for(super::DigRuntimeRequest {
                discord_id,
                guild_id,
                now: *candidate,
                paid: true,
                forced_event: false,
            }))
            .unit()
                < 0.04
        })
        .expect("deterministic paid cave seed")
}

fn find_dig_time_with_unit_between(
    discord_id: i64,
    guild_id: i64,
    start: i64,
    minimum: f64,
    maximum: f64,
) -> i64 {
    (start..start.saturating_add(10_000))
        .find(|candidate| {
            let unit = super::SeededLootEntropy::new(super::seed_for(super::DigRuntimeRequest {
                discord_id,
                guild_id,
                now: *candidate,
                paid: false,
                forced_event: false,
            }))
            .unit();
            unit >= minimum && unit < maximum
        })
        .expect("deterministic cave probability seed")
}

fn find_non_cave_dig_time_with_jc(
    discord_id: i64,
    guild_id: i64,
    start: i64,
    depth: i64,
    wanted_jc: i64,
) -> i64 {
    let layer = super::layer_at(depth);
    (start..start.saturating_add(10_000))
        .find(|candidate| {
            let mut entropy =
                super::SeededLootEntropy::new(super::seed_for(super::DigRuntimeRequest {
                    discord_id,
                    guild_id,
                    now: *candidate,
                    paid: false,
                    forced_event: false,
                }));
            let safely_not_cave = entropy.unit() > 0.60;
            let _advance = entropy.advance(layer.advance_range.0, layer.advance_range.1);
            let _event_gate = entropy.unit();
            safely_not_cave && entropy.advance(layer.jc_range.0, layer.jc_range.1) == wanted_jc
        })
        .expect("deterministic non-cave JC roll")
}

fn seed_two_weather_rows(
    database: &NamedTempFile,
    guild_id: i64,
    game_date: &str,
    active_layer: &str,
    active_weather: &str,
) {
    let mut connection = Connection::open(database.path()).expect("weather connection");
    let transaction = connection.transaction().expect("weather transaction");
    transaction
        .execute(
            "INSERT INTO dig_weather(guild_id,game_date,layer_name,weather_id)
                 VALUES (?1,?2,?3,?4)",
            params![guild_id, game_date, active_layer, active_weather],
        )
        .expect("seed active weather row");
    transaction
        .execute(
            "INSERT INTO dig_weather(guild_id,game_date,layer_name,weather_id)
                 VALUES (?1,?2,?3,'earthworm_migration')",
            params![
                guild_id,
                game_date,
                if active_layer == "Dirt" {
                    "Stone"
                } else {
                    "Dirt"
                }
            ],
        )
        .expect("seed second weather row");
    transaction.commit().expect("commit weather rows");
}

fn seed_active_mana(
    database: &NamedTempFile,
    discord_id: i64,
    guild_id: i64,
    game_date: &str,
    land: &str,
) {
    Connection::open(database.path())
        .expect("Mana connection")
        .execute(
            "INSERT INTO player_mana(
                     discord_id,guild_id,current_land,assigned_date,consumed_today
                 ) VALUES(?1,?2,?3,?4,0)",
            params![discord_id, guild_id, land, game_date],
        )
        .expect("seed active Mana");
}

fn latest_dig_detail(database: &NamedTempFile, actor: i64, guild: i64) -> Value {
    let raw = Connection::open(database.path())
        .expect("Dig detail connection")
        .query_row(
            "SELECT detail FROM dig_actions
                 WHERE actor_id=?1 AND guild_id=?2 AND action_type='dig'
                 ORDER BY id DESC LIMIT 1",
            params![actor, guild],
            |row| row.get::<_, String>(0),
        )
        .expect("latest Dig detail");
    serde_json::from_str(&raw).expect("latest Dig detail JSON")
}

#[test]
fn mana_base_yield_variance_and_steady_bonus_share_live_entropy() {
    let red = cama_domain::mana::ManaEffects::for_color(Some("Red"), Some("Mountain"));
    let mut double = crate::dig_loot::ScriptedLootEntropy::new([0.149_999], []);
    let mut zero = crate::dig_loot::ScriptedLootEntropy::new([0.15], []);
    let mut ordinary = crate::dig_loot::ScriptedLootEntropy::new([0.30], []);
    assert_eq!(super::apply_mana_base_yield(5, &red, &mut double), 10);
    assert_eq!(super::apply_mana_base_yield(5, &red, &mut zero), 0);
    assert_eq!(super::apply_mana_base_yield(5, &red, &mut ordinary), 5);

    let green = cama_domain::mana::ManaEffects::for_color(Some("Green"), Some("Forest"));
    let mut no_roll = crate::dig_loot::ScriptedLootEntropy::default();
    assert_eq!(super::apply_mana_base_yield(5, &green, &mut no_roll), 6);
    assert_eq!(super::apply_mana_base_yield(0, &green, &mut no_roll), 0);
}

#[test]
fn sqlite_forest_mana_makes_free_dig_ready_at_exact_3570_seconds() {
    let control = NamedTempFile::new().expect("control cooldown database");
    let forest = NamedTempFile::new().expect("Forest cooldown database");
    let actor = 60_081;
    let guild = 60_082;
    let now = 1_900_080_000;
    for database in [&control, &forest] {
        seed_live_runtime_tunnel(database, actor, guild, now, 10, 1, Some(now - 3_570));
    }
    let today =
        cama_domain::game_date::game_date_for_timestamp(now as f64).expect("Forest mana date");
    Connection::open(forest.path())
        .expect("Forest mana connection")
        .execute(
            "INSERT INTO player_mana(
                     discord_id,guild_id,current_land,assigned_date,consumed_today
                 ) VALUES(?1,?2,'Forest',?3,0)",
            params![actor, guild, today],
        )
        .expect("seed Forest mana");

    let request = DigRuntimeRequest {
        discord_id: actor,
        guild_id: guild,
        now,
        paid: false,
        forced_event: false,
    };
    let blocked = DigRuntimeService::sqlite(control.path())
        .dig(request)
        .expect("control cooldown response");
    assert!(!blocked.success);
    assert_eq!(blocked.cooldown_remaining, 30);
    let admitted = DigRuntimeService::sqlite(forest.path())
        .dig(request)
        .expect("Forest cooldown Dig");
    assert!(admitted.success);
    assert_eq!(admitted.cooldown_remaining, 0);
}

fn assert_stamina_cooldown_boundary(mutations: &str, expected_seconds: i64) {
    let database = NamedTempFile::new().expect("stamina cooldown database");
    let actor = 60_087;
    let guild = 60_088;
    let last_dig_at = 1_900_083_000;
    seed_live_runtime_tunnel(
        &database,
        actor,
        guild,
        last_dig_at,
        10,
        1,
        Some(last_dig_at),
    );
    Connection::open(database.path())
        .expect("stamina cooldown connection")
        .execute(
            "UPDATE tunnels SET stat_stamina=13,mutations=?1
                 WHERE discord_id=?2 AND guild_id=?3",
            params![mutations, actor, guild],
        )
        .expect("seed stamina and mutations");
    let service = DigRuntimeService::sqlite(database.path());
    let request_at = |now| DigRuntimeRequest {
        discord_id: actor,
        guild_id: guild,
        now,
        paid: false,
        forced_event: false,
    };
    let blocked = service
        .dig(request_at(last_dig_at + expected_seconds - 1))
        .expect("stamina cooldown rejection");
    assert!(!blocked.success);
    assert_eq!(blocked.cooldown_remaining, 1);
    let admitted = service
        .dig(request_at(last_dig_at + expected_seconds))
        .expect("stamina cooldown boundary Dig");
    assert!(admitted.success);
    assert_eq!(admitted.cooldown_remaining, 0);
}

#[test]
fn authored_storm_ids_do_not_activate_generic_storm_combo_or_stormcaller() {
    let blue = cama_domain::mana::ManaEffects::for_color(Some("Blue"), Some("Island"));
    let relics = crate::dig_relic_rework::RelicSet::new(["stormcaller"]);
    for authored in ["temporal_storm", "whisper_storm"] {
        let code = super::weather_code(Some(authored));
        assert_eq!(code, Some(authored));
        assert!(!cama_domain::mana::weather_combo_modifiers(&blue, code).applied);
        assert!(!crate::dig_relic_rework::storm_negates_hazard(
            &relics, code
        ));
    }
    assert!(
        cama_domain::mana::weather_combo_modifiers(&blue, super::weather_code(Some("storm")),)
            .applied
    );
    assert!(crate::dig_relic_rework::storm_negates_hazard(
        &relics,
        super::weather_code(Some("storm")),
    ));
}

#[test]
fn event_chance_composes_ascension_weather_and_route_multiplicatively() {
    let factor = super::event_chance_factor(0.20, 0.50, 0.50);
    assert!((factor - 2.70).abs() < 1e-12);
    assert!(((0.31_f64 * factor).min(0.75) - 0.75).abs() < 1e-12);
    assert_eq!(super::event_chance_factor(0.20, -1.0, 0.0), 0.0);
}

#[test]
fn sqlite_live_event_gate_multiplies_ascension_weather_and_route_factors() {
    let database = NamedTempFile::new().expect("event multiplier database");
    let actor = 60_111;
    let guild = 60_112;
    let now = 1_900_086_006;
    seed_live_runtime_tunnel(&database, actor, guild, now, 240, 1, Some(now - 7_200));
    Connection::open(database.path())
            .expect("event multiplier connection")
            .execute(
                "UPDATE tunnels SET prestige_level=2,route_state=?1,boss_progress=?2
                 WHERE discord_id=?3 AND guild_id=?4",
                params![
                    r#"{"end_depth":275,"layer":"Frozen Core","offered":["timefracture","frozen_stillness","icebound_cache"],"selected":"timefracture","start_depth":200}"#,
                    r#"{"25":"defeated","50":{"status":"defeated"},"75":"defeated","100":{"status":"defeated"},"150":"defeated","200":"defeated"}"#,
                    actor,
                    guild,
                ],
            )
            .expect("seed event multiplier policy");
    let today =
        cama_domain::game_date::game_date_for_timestamp(now as f64).expect("event multiplier date");
    seed_two_weather_rows(&database, guild, &today, "Frozen Core", "temporal_storm");

    let outcome = DigRuntimeService::sqlite(database.path())
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now,
            paid: false,
            forced_event: false,
        })
        .expect("multiplicative event Dig");
    assert!(outcome.success && !outcome.cave_in);
    assert!(outcome.event_id.is_some());
    assert_eq!(
        Connection::open(database.path())
            .expect("inspect event multiplier Dig")
            .query_row(
                "SELECT current_run_events FROM tunnels
                     WHERE discord_id=?1 AND guild_id=?2",
                params![actor, guild],
                |row| row.get::<_, i64>(0),
            )
            .expect("event counter"),
        1,
    );
}

#[test]
fn sqlite_stamina_thirteen_caps_cooldown_after_mutations_plain() {
    assert_stamina_cooldown_boundary("[]", 1_800);
}

#[test]
fn sqlite_stamina_thirteen_caps_cooldown_after_mutations_restless() {
    assert_stamina_cooldown_boundary(r#"[{"id":"restless"}]"#, 3_600);
}

#[test]
fn sqlite_configured_minigame_scale_applies_to_first_and_normal_dig() {
    let first_control = NamedTempFile::new().expect("first control database");
    let first_scaled = NamedTempFile::new().expect("first scaled database");
    let first_actor = 60_087;
    let first_guild = 60_088;
    let first_now = 1_900_083_500;
    for database in [&first_control, &first_scaled] {
        seed_live_runtime_tunnel(database, first_actor, first_guild, first_now, 0, 0, None);
    }
    let first_request = DigRuntimeRequest {
        discord_id: first_actor,
        guild_id: first_guild,
        now: first_now,
        paid: false,
        forced_event: false,
    };
    let first_control_outcome = DigRuntimeService::sqlite(first_control.path())
        .dig(first_request)
        .expect("default-scale first Dig");
    let half_scale =
        DigRuntimeConfig::default().with_runtime_policy(0.5, EconomyEventConfig::default());
    let first_scaled_outcome =
        DigRuntimeService::sqlite_with_config(first_scaled.path(), half_scale.clone())
            .dig(first_request)
            .expect("half-scale first Dig");
    let first_control_detail = latest_dig_detail(&first_control, first_actor, first_guild);
    let first_scaled_detail = latest_dig_detail(&first_scaled, first_actor, first_guild);
    let first_structural = first_control_detail["minigame_scaled_jc"]
        .as_i64()
        .expect("first default-scale JC");
    let first_expected = scale_dig_minigame_jc(first_structural, 500_000);
    assert_eq!(
        first_scaled_detail["minigame_scaled_jc"].as_i64(),
        Some(first_expected),
    );
    assert_eq!(
        first_scaled_outcome.jc_earned,
        scale_positive_dig_jc(first_expected),
    );
    assert_eq!(
        SqliteDigRuntimeStore::new(first_scaled.path())
            .snapshot(first_actor, first_guild)
            .expect("reload scaled first Dig")
            .tunnel
            .expect("scaled first tunnel")
            .total_jc_earned,
        first_scaled_outcome.jc_earned,
    );
    assert!(first_control_outcome.first_dig);

    let normal_control = NamedTempFile::new().expect("normal control database");
    let normal_scaled = NamedTempFile::new().expect("normal scaled database");
    let normal_actor = 60_095;
    let normal_guild = 60_096;
    let normal_now = 1_900_084_750;
    for database in [&normal_control, &normal_scaled] {
        seed_live_runtime_tunnel(
            database,
            normal_actor,
            normal_guild,
            normal_now,
            76,
            1,
            Some(normal_now - 7_200),
        );
        Connection::open(database.path())
                .expect("normal scale boss-progress connection")
                .execute(
                    "UPDATE tunnels SET boss_progress=?1
                     WHERE discord_id=?2 AND guild_id=?3",
                    params![
                        r#"{"25":{"status":"defeated"},"50":{"status":"defeated"},"75":{"status":"defeated"}}"#,
                        normal_actor,
                        normal_guild,
                    ],
                )
                .expect("seed normal scale defeated bosses");
    }
    let normal_dig_now =
        find_non_cave_dig_time_with_jc(normal_actor, normal_guild, normal_now, 76, 3);
    let normal_today = game_date_for_timestamp(normal_dig_now as f64).expect("normal scale date");
    for database in [&normal_control, &normal_scaled] {
        seed_two_weather_rows(
            database,
            normal_guild,
            &normal_today,
            "Magma",
            "fossil_rush",
        );
    }
    let normal_request = DigRuntimeRequest {
        discord_id: normal_actor,
        guild_id: normal_guild,
        now: normal_dig_now,
        paid: false,
        forced_event: false,
    };
    let normal_control_outcome = DigRuntimeService::sqlite(normal_control.path())
        .dig(normal_request)
        .expect("default-scale normal Dig");
    let normal_scaled_outcome =
        DigRuntimeService::sqlite_with_config(normal_scaled.path(), half_scale)
            .dig(normal_request)
            .expect("half-scale normal Dig");
    let normal_control_detail = latest_dig_detail(&normal_control, normal_actor, normal_guild);
    let normal_scaled_detail = latest_dig_detail(&normal_scaled, normal_actor, normal_guild);
    let normal_structural = normal_control_detail["gross_jc"]
        .as_i64()
        .expect("normal default-scale gross");
    let normal_expected = scale_dig_minigame_jc(normal_structural, 500_000);
    assert_eq!(
        normal_scaled_detail["gross_jc"].as_i64(),
        Some(normal_expected)
    );
    assert_eq!(
        normal_scaled_outcome.jc_earned,
        scale_positive_dig_jc(normal_expected),
    );
    assert!(normal_control_outcome.jc_earned >= normal_scaled_outcome.jc_earned);
}

#[test]
fn sqlite_blue_mana_tax_is_one_net_reward_sink_without_a_second_ledger() {
    let control = NamedTempFile::new().expect("control Blue-tax database");
    let blue = NamedTempFile::new().expect("Blue-tax database");
    let actor = 60_089;
    let guild = 60_090;
    let now = 1_900_084_000;
    for database in [&control, &blue] {
        seed_live_runtime_tunnel(database, actor, guild, now, 76, 1, Some(now - 7_200));
        Connection::open(database.path())
                .expect("Blue-tax boss-progress connection")
                .execute(
                    "UPDATE tunnels SET boss_progress=?1
                     WHERE discord_id=?2 AND guild_id=?3",
                    params![
                        r#"{"25":{"status":"defeated"},"50":{"status":"defeated"},"75":{"status":"defeated"}}"#,
                        actor,
                        guild,
                    ],
                )
                .expect("seed defeated lower bosses");
    }
    let dig_now = find_non_cave_dig_time_with_jc(actor, guild, now, 76, 3);
    let today = game_date_for_timestamp(dig_now as f64).expect("Blue-tax date");
    for database in [&control, &blue] {
        seed_two_weather_rows(database, guild, &today, "Magma", "cooling_period");
    }
    seed_active_mana(&blue, actor, guild, &today, "Island");
    for database in [&control, &blue] {
        Connection::open(database.path())
            .expect("clear Blue-tax setup ledger")
            .execute("DELETE FROM economy_ledger_entries", [])
            .expect("clear Blue-tax setup ledger");
    }
    let request = DigRuntimeRequest {
        discord_id: actor,
        guild_id: guild,
        now: dig_now,
        paid: false,
        forced_event: false,
    };
    let control_outcome = DigRuntimeService::sqlite(control.path())
        .dig(request)
        .expect("control Blue-tax Dig");
    let blue_outcome = DigRuntimeService::sqlite(blue.path())
        .dig(request)
        .expect("Blue-tax Dig");
    let expected_tax = proportional_mana_yield_tax(control_outcome.jc_earned, 0.055);
    assert!(control_outcome.jc_earned > 0);
    assert!(expected_tax > 0);
    assert_eq!(
        blue_outcome.jc_earned,
        control_outcome.jc_earned - expected_tax
    );
    let connection = Connection::open(blue.path()).expect("inspect Blue-tax database");
    let (ledger_count, detail): (i64, String) = connection
        .query_row(
            "SELECT
                     (SELECT COUNT(*) FROM economy_ledger_entries
                       WHERE account_id=?1),
                     (SELECT detail FROM dig_actions
                       WHERE actor_id=?1 AND guild_id=?2 AND action_type='dig'
                       ORDER BY id DESC LIMIT 1)",
            params![actor, guild],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("Blue-tax ledger and audit");
    assert!(
        ledger_count <= 1,
        "Blue tax must not create a second debit ledger"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&detail)
            .expect("Blue-tax detail JSON")
            .get("mana_yield_tax")
            .and_then(Value::as_i64),
        Some(expected_tax),
    );
}

#[test]
fn sqlite_normal_dig_applies_vanity_tax_to_gross_and_audits_net() {
    let control = NamedTempFile::new().expect("control vanity-tax database");
    let taxable = NamedTempFile::new().expect("taxable vanity-tax database");
    let actor = 60_111;
    let guild = 60_112;
    let now = 1_900_110_000;
    for database in [&control, &taxable] {
        seed_live_runtime_tunnel(database, actor, guild, now, 76, 1, Some(now - 7_200));
        Connection::open(database.path())
            .expect("vanity-tax setup connection")
            .execute("DELETE FROM economy_ledger_entries", [])
            .expect("clear vanity-tax setup ledger");
    }
    let dig_now = find_non_cave_dig_time_with_jc(actor, guild, now, 76, 3);
    let request = DigRuntimeRequest {
        discord_id: actor,
        guild_id: guild,
        now: dig_now,
        paid: false,
        forced_event: false,
    };
    let today = game_date_for_timestamp(dig_now as f64).expect("vanity-tax date");
    for database in [&control, &taxable] {
        seed_two_weather_rows(database, guild, &today, "Magma", "fossil_rush");
    }
    let control_outcome = DigRuntimeService::sqlite(control.path())
        .dig(request)
        .expect("control normal Dig");
    assert!(control_outcome.success);
    assert!(!control_outcome.cave_in);
    assert!(control_outcome.jc_earned > 0);

    let vanity = Arc::new(VanityTaxService::with_rate_fraction(None, 0.5));
    vanity
        .refresh_guild(
            guild,
            &[VanityMember {
                discord_id: actor,
                nickname: None,
            }],
            None,
        )
        .expect("refresh taxable member");
    assert_eq!(vanity.calculate_tax(actor, Some(guild), 6), 3);
    let taxable_outcome = DigRuntimeService::sqlite(taxable.path())
        .with_vanity_tax(vanity)
        .dig(request)
        .expect("taxable normal Dig");
    let expected_tax = control_outcome.jc_earned / 2;
    assert!(expected_tax > 0);
    assert_eq!(taxable_outcome.vanity_tax, expected_tax);
    assert_eq!(
        taxable_outcome.jc_earned,
        control_outcome.jc_earned - expected_tax
    );
    let detail = latest_dig_detail(&taxable, actor, guild);
    assert_eq!(detail["vanity_tax"].as_i64(), Some(expected_tax));

    let connection = Connection::open(taxable.path()).expect("inspect vanity-tax ledger");
    let ledger = connection
        .prepare(
            "SELECT source,delta FROM economy_ledger_entries
                 WHERE account_id=?1 AND guild_id=?2 ORDER BY ledger_id",
        )
        .expect("prepare vanity-tax ledger")
        .query_map(params![actor, guild], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .expect("query vanity-tax ledger")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect vanity-tax ledger");
    assert_eq!(
        ledger,
        vec![
            ("dig".to_owned(), control_outcome.jc_earned),
            ("vanity_tax".to_owned(), -expected_tax),
        ]
    );
    let audit: (i64, i64) = connection
        .query_row(
            "SELECT jc_delta,COUNT(*) FROM dig_actions
                 WHERE actor_id=?1 AND guild_id=?2 AND action_type='dig'",
            params![actor, guild],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("vanity-tax audit");
    assert_eq!(audit, (taxable_outcome.jc_earned, 1));
}

#[test]
fn sqlite_first_dig_is_exempt_from_vanity_tax() {
    let database = NamedTempFile::new().expect("first-Dig vanity-tax database");
    let actor = 60_113;
    let guild = 60_114;
    let now = 1_900_113_000;
    initialize_or_migrate(database.path()).expect("first-Dig migration");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(actor, "first-vanity", Some(guild)))
        .expect("first-Dig player");
    Connection::open(database.path())
        .expect("first-Dig connection")
        .execute("DELETE FROM economy_ledger_entries", [])
        .expect("clear first-Dig setup ledger");
    let vanity = Arc::new(VanityTaxService::with_rate_fraction(None, 0.5));
    vanity
        .refresh_guild(
            guild,
            &[VanityMember {
                discord_id: actor,
                nickname: None,
            }],
            None,
        )
        .expect("refresh first-Dig member");
    let outcome = DigRuntimeService::sqlite(database.path())
        .with_vanity_tax(vanity)
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now,
            paid: false,
            forced_event: false,
        })
        .expect("first Dig");
    assert!(outcome.success);
    assert!(outcome.first_dig);
    assert_eq!(outcome.vanity_tax, 0);
    let connection = Connection::open(database.path()).expect("inspect first-Dig ledger");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE account_id=?1 AND guild_id=?2 AND source='vanity_tax'",
                params![actor, guild],
                |row| row.get::<_, i64>(0),
            )
            .expect("first-Dig vanity ledger count"),
        0
    );
}

#[test]
fn vanity_tax_manual_override_taxability_is_deterministic() {
    let vanity = VanityTaxService::with_rate_fraction(None, 0.1);
    vanity
        .refresh_guild(
            60_116,
            &[VanityMember {
                discord_id: 60_115,
                nickname: Some("decorated".to_owned()),
            }],
            None,
        )
        .expect("refresh manually exempt member");
    assert_eq!(vanity.calculate_tax(60_115, Some(60_116), 250), 0);
    vanity
        .set_manual_taxation(60_116, 60_115, true, 60_117)
        .expect("manual taxation");
    assert_eq!(vanity.calculate_tax(60_115, Some(60_116), 250), 25);
}

#[test]
fn sqlite_vanity_tax_rolls_back_on_audit_failure_without_duplicate_ledger() {
    let database = NamedTempFile::new().expect("vanity-tax rollback database");
    let actor = 60_118;
    let guild = 60_119;
    let now = 1_900_118_000;
    seed_live_runtime_tunnel(&database, actor, guild, now, 10, 1, Some(now - 7_200));
    let store = SqliteDigRuntimeStore::new(database.path());
    let before = store.snapshot(actor, guild).expect("rollback snapshot");
    let mut next = before.clone();
    next.balance = before.balance.saturating_add(10);
    let tunnel = next.tunnel.as_mut().expect("rollback tunnel");
    tunnel.depth = tunnel.depth.saturating_add(1);
    tunnel.total_digs = tunnel.total_digs.saturating_add(1);
    tunnel.last_dig_at = Some(now);
    Connection::open(database.path())
        .expect("audit trigger connection")
        .execute_batch(
            "CREATE TRIGGER reject_vanity_tax_audit
                 BEFORE INSERT ON dig_actions
                 BEGIN SELECT RAISE(ABORT, 'vanity audit failure'); END;",
        )
        .expect("install audit failure trigger");
    let error = store
        .commit(DigRuntimeCommit {
            expected: DigRuntimeVersion::from(&before),
            next,
            delivery_draft: None,
            consumed_item_ids: Vec::new(),
            pet_work_claim: None,
            consume_overgrowth: false,
            depth_before: 10,
            depth_after: 11,
            jc_delta: 10,
            vanity_tax: 2,
            balance_cost: 0,
            action_type: "dig".to_owned(),
            detail: "{}".to_owned(),
            now,
        })
        .expect_err("audit failure must roll back vanity settlement");
    assert!(matches!(error, DigRuntimeStoreError::Sqlite(_)));
    assert_eq!(
        store.snapshot(actor, guild).expect("rollback reload"),
        before
    );
    let connection = Connection::open(database.path()).expect("rollback ledger connection");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE account_id=?1 AND guild_id=?2
                       AND source IN ('dig','vanity_tax')",
                params![actor, guild],
                |row| row.get::<_, i64>(0),
            )
            .expect("rollback ledger count"),
        0
    );
}

#[test]
fn sqlite_bankruptcy_penalty_applies_to_normal_dig_without_consuming_games() {
    let control = NamedTempFile::new().expect("control bankruptcy database");
    let penalized = NamedTempFile::new().expect("penalized bankruptcy database");
    let actor = 60_099;
    let guild = 60_100;
    let now = 1_900_085_500;
    for database in [&control, &penalized] {
        seed_live_runtime_tunnel(database, actor, guild, now, 76, 1, Some(now - 7_200));
        Connection::open(database.path())
                .expect("bankruptcy boss-progress connection")
                .execute(
                    "UPDATE tunnels SET boss_progress=?1,temp_buffs=?2
                     WHERE discord_id=?3 AND guild_id=?4",
                    params![
                        r#"{"25":{"status":"defeated"},"50":{"status":"defeated"},"75":{"status":"defeated"}}"#,
                        r#"{"digs_remaining":1,"effect":{"yield_multiplier":2.0}}"#,
                        actor,
                        guild,
                    ],
                )
                .expect("seed bankruptcy defeated bosses");
    }
    Connection::open(penalized.path())
        .expect("bankruptcy state connection")
        .execute(
            "INSERT INTO bankruptcy_state(
                     discord_id,guild_id,penalty_games_remaining,bankruptcy_count
                 ) VALUES(?1,?2,2,1)",
            params![actor, guild],
        )
        .expect("seed bankruptcy penalty");
    let dig_now = find_non_cave_dig_time_with_jc(actor, guild, now, 76, 3);
    let today = game_date_for_timestamp(dig_now as f64).expect("bankruptcy Dig date");
    for database in [&control, &penalized] {
        seed_two_weather_rows(database, guild, &today, "Magma", "fossil_rush");
    }
    let request = DigRuntimeRequest {
        discord_id: actor,
        guild_id: guild,
        now: dig_now,
        paid: false,
        forced_event: false,
    };
    let control_outcome = DigRuntimeService::sqlite(control.path())
        .dig(request)
        .expect("control bankruptcy Dig");
    let penalized_config = DigRuntimeConfig::default().with_bankruptcy_penalty_rate(0.75);
    let penalized_outcome =
        DigRuntimeService::sqlite_with_config(penalized.path(), penalized_config)
            .dig(request)
            .expect("penalized bankruptcy Dig");
    let expected_penalty = control_outcome.jc_earned / 4;
    assert!(expected_penalty > 0);
    assert_eq!(
        penalized_outcome.jc_earned,
        control_outcome.jc_earned - expected_penalty,
    );
    let connection = Connection::open(penalized.path()).expect("inspect bankruptcy Dig");
    assert_eq!(
        connection
            .query_row(
                "SELECT penalty_games_remaining FROM bankruptcy_state
                     WHERE discord_id=?1 AND guild_id=?2",
                params![actor, guild],
                |row| row.get::<_, i64>(0),
            )
            .expect("bankruptcy games after Dig"),
        2,
        "ordinary Dig winnings do not consume a game-based penalty",
    );
    assert_eq!(
        latest_dig_detail(&penalized, actor, guild)["bankruptcy_penalty"].as_i64(),
        Some(expected_penalty),
    );
}

#[test]
fn sqlite_blue_paid_cave_refund_is_atomic_and_not_counted_as_dig_income() {
    let control = NamedTempFile::new().expect("control paid-cave database");
    let blue = NamedTempFile::new().expect("Blue paid-cave database");
    let actor = 60_093;
    let guild = 60_094;
    let now = 1_900_084_500;
    for database in [&control, &blue] {
        seed_live_runtime_tunnel(database, actor, guild, now, 10, 1, Some(now - 1));
        Connection::open(database.path())
            .expect("paid-cave setup connection")
            .execute(
                "UPDATE players SET jopacoin_balance=100 WHERE discord_id=?1 AND guild_id=?2",
                params![actor, guild],
            )
            .expect("seed paid-cave balance");
        Connection::open(database.path())
            .expect("paid-cave tunnel connection")
            .execute(
                "UPDATE tunnels
                     SET grappling_hook_charges=1,total_jc_earned=25,current_run_jc=9
                     WHERE discord_id=?1 AND guild_id=?2",
                params![actor, guild],
            )
            .expect("seed paid-cave tunnel counters");
    }
    let dig_now = find_paid_cave_dig_time(actor, guild, now);
    let today = game_date_for_timestamp(dig_now as f64).expect("paid-cave date");
    for database in [&control, &blue] {
        seed_two_weather_rows(database, guild, &today, "Dirt", "earthworm_migration");
    }
    seed_active_mana(&blue, actor, guild, &today, "Island");
    for database in [&control, &blue] {
        Connection::open(database.path())
            .expect("clear paid-cave setup ledger")
            .execute("DELETE FROM economy_ledger_entries", [])
            .expect("clear paid-cave setup ledger");
    }
    let request = DigRuntimeRequest {
        discord_id: actor,
        guild_id: guild,
        now: dig_now,
        paid: true,
        forced_event: false,
    };
    let control_outcome = DigRuntimeService::sqlite(control.path())
        .dig(request)
        .expect("control paid cave");
    let blue_outcome = DigRuntimeService::sqlite(blue.path())
        .dig(request)
        .expect("Blue paid cave");
    assert!(control_outcome.cave_in);
    assert!(blue_outcome.cave_in);
    assert_eq!(control_outcome.paid_dig_cost, 3);
    assert_eq!(blue_outcome.paid_dig_cost, 3);
    assert_eq!(control_outcome.jc_earned, 0);
    assert_eq!(blue_outcome.jc_earned, 0);
    assert_eq!(control_outcome.balance_after, 97);
    assert_eq!(blue_outcome.balance_after, 98);

    let connection = Connection::open(blue.path()).expect("inspect Blue paid cave");
    let ledger_deltas = connection
        .prepare(
            "SELECT delta FROM economy_ledger_entries
                 WHERE account_type='player' AND account_id=?1 ORDER BY ledger_id",
        )
        .expect("prepare paid-cave ledger query")
        .query_map([actor], |row| row.get::<_, i64>(0))
        .expect("query paid-cave ledger")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect paid-cave ledger");
    assert_eq!(ledger_deltas, vec![-3, 1]);
    let (total_jc, current_run_jc, audit_delta, detail): (i64, i64, i64, String) = connection
        .query_row(
            "SELECT t.total_jc_earned,t.current_run_jc,a.jc_delta,a.detail
                     FROM tunnels t JOIN dig_actions a
                       ON a.actor_id=t.discord_id AND a.guild_id=t.guild_id
                     WHERE t.discord_id=?1 AND t.guild_id=?2
                     ORDER BY a.id DESC LIMIT 1",
            params![actor, guild],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("paid-cave counters and audit");
    assert_eq!((total_jc, current_run_jc), (25, 9));
    assert_eq!(audit_delta, 1);
    assert_eq!(
        serde_json::from_str::<Value>(&detail)
            .expect("paid-cave detail JSON")
            .get("mana_paid_cave_refund")
            .and_then(Value::as_i64),
        Some(1),
    );
}

#[test]
fn sqlite_plains_tithe_deducts_only_after_fail_soft_reserve_credit() {
    let control = NamedTempFile::new().expect("control Plains database");
    let plains = NamedTempFile::new().expect("Plains database");
    let failing = NamedTempFile::new().expect("failing Plains database");
    let actor = 60_097;
    let guild = 60_098;
    let now = 1_900_085_000;
    for database in [&control, &plains, &failing] {
        seed_live_runtime_tunnel(database, actor, guild, now, 10, 1, Some(now - 7_200));
    }
    let dig_now = find_non_cave_dig_time_with_jc(actor, guild, now, 10, 1);
    let today = game_date_for_timestamp(dig_now as f64).expect("Plains tithe date");
    for database in [&control, &plains, &failing] {
        seed_two_weather_rows(database, guild, &today, "Dirt", "cooling_period");
    }
    seed_active_mana(&plains, actor, guild, &today, "Plains");
    seed_active_mana(&failing, actor, guild, &today, "Plains");
    Connection::open(failing.path())
        .expect("failing reserve connection")
        .execute(
            "INSERT INTO nonprofit_fund(guild_id,total_collected)
                 VALUES(?1,?2)",
            params![guild, i64::MAX],
        )
        .expect("seed overflowing reserve");
    let request = DigRuntimeRequest {
        discord_id: actor,
        guild_id: guild,
        now: dig_now,
        paid: false,
        forced_event: false,
    };
    let control_outcome = DigRuntimeService::sqlite(control.path())
        .dig(request)
        .expect("control Plains Dig");
    let plains_outcome = DigRuntimeService::sqlite(plains.path())
        .dig(request)
        .expect("Plains tithe Dig");
    let failing_outcome = DigRuntimeService::sqlite(failing.path())
        .dig(request)
        .expect("fail-soft Plains Dig");
    let tithe = proportional_mana_yield_tax(control_outcome.jc_earned, 0.05);
    assert_eq!(plains_outcome.jc_earned, control_outcome.jc_earned - tithe);
    assert_eq!(
        LoanRepository::new(plains.path())
            .get_nonprofit_fund(Some(guild))
            .expect("read credited reserve"),
        tithe,
    );
    assert_eq!(failing_outcome.jc_earned, control_outcome.jc_earned);
    assert_eq!(
        LoanRepository::new(failing.path())
            .get_nonprofit_fund(Some(guild))
            .expect("read overflowing reserve"),
        i64::MAX,
    );
}

#[test]
fn sqlite_temp_yield_buff_composes_once_and_expires() {
    let control = NamedTempFile::new().expect("control yield-buff database");
    let boosted = NamedTempFile::new().expect("boosted yield-buff database");
    let actor = 60_083;
    let guild = 60_084;
    let now = 1_900_081_000;
    let depth = 76;
    for database in [&control, &boosted] {
        seed_live_runtime_tunnel(database, actor, guild, now, depth, 1, Some(now - 7_200));
        Connection::open(database.path())
                .expect("yield-buff boss-progress connection")
                .execute(
                    "UPDATE tunnels SET boss_progress=?1
                     WHERE discord_id=?2 AND guild_id=?3",
                    params![
                        r#"{"25":{"status":"defeated"},"50":{"status":"defeated"},"75":{"status":"defeated"}}"#,
                        actor,
                        guild,
                    ],
                )
                .expect("seed defeated lower bosses");
    }
    let dig_now = find_non_cave_dig_time_with_jc(actor, guild, now, depth, 3);
    let today =
        cama_domain::game_date::game_date_for_timestamp(dig_now as f64).expect("yield-buff date");
    for database in [&control, &boosted] {
        seed_two_weather_rows(database, guild, &today, "Magma", "cooling_period");
    }
    Connection::open(boosted.path())
        .expect("yield-buff connection")
        .execute(
            "UPDATE tunnels SET temp_buffs=?1
                 WHERE discord_id=?2 AND guild_id=?3",
            params![
                r#"{"digs_remaining":1,"effect":{"yield_multiplier":1.75,"advance_bonus":2}}"#,
                actor,
                guild,
            ],
        )
        .expect("seed yield buff");
    let request = DigRuntimeRequest {
        discord_id: actor,
        guild_id: guild,
        now: dig_now,
        paid: false,
        forced_event: false,
    };
    let control_outcome = DigRuntimeService::sqlite(control.path())
        .dig(request)
        .expect("control yield Dig");
    let boosted_outcome = DigRuntimeService::sqlite(boosted.path())
        .dig(request)
        .expect("boosted yield Dig");
    assert!(!control_outcome.cave_in && !boosted_outcome.cave_in);
    assert_eq!(boosted_outcome.advance, control_outcome.advance + 2);
    assert!(boosted_outcome.jc_earned > control_outcome.jc_earned);
    let tunnel = SqliteDigRuntimeStore::new(boosted.path())
        .snapshot(actor, guild)
        .expect("reload yield-buff tunnel")
        .tunnel
        .expect("yield-buff tunnel");
    assert_eq!(tunnel.temp_buffs, None);
    assert_eq!(tunnel.current_run_jc, boosted_outcome.jc_earned);
    assert_eq!(tunnel.cavein_free_streak, 1);
}

#[test]
fn sqlite_relic_yield_uses_post_drain_luminosity() {
    let control = NamedTempFile::new().expect("control luminosity-yield database");
    let coal = NamedTempFile::new().expect("Deepveined Coal database");
    let actor = 60_085;
    let guild = 60_086;
    let now = 1_900_082_000;
    let depth = 76;
    for database in [&control, &coal] {
        seed_live_runtime_tunnel(database, actor, guild, now, depth, 1, Some(now - 7_200));
        Connection::open(database.path())
                .expect("luminosity-yield connection")
                .execute(
                    "UPDATE tunnels SET luminosity=26,boss_progress=?1
                     WHERE discord_id=?2 AND guild_id=?3",
                    params![
                        r#"{"25":{"status":"defeated"},"50":{"status":"defeated"},"75":{"status":"defeated"}}"#,
                        actor,
                        guild,
                    ],
                )
                .expect("seed dim luminosity");
    }
    let dig_now = find_non_cave_dig_time_with_jc(actor, guild, now, depth, 3);
    let today = cama_domain::game_date::game_date_for_timestamp(dig_now as f64)
        .expect("luminosity-yield date");
    for database in [&control, &coal] {
        seed_two_weather_rows(database, guild, &today, "Magma", "cooling_period");
    }
    Connection::open(coal.path())
        .expect("Coal connection")
        .execute(
            "INSERT INTO dig_artifacts(
                     discord_id,guild_id,artifact_id,found_at,is_relic,equipped
                 ) VALUES(?1,?2,'deepveined_coal',?3,1,1)",
            params![actor, guild, now - 100],
        )
        .expect("equip Deepveined Coal");
    let request = DigRuntimeRequest {
        discord_id: actor,
        guild_id: guild,
        now: dig_now,
        paid: false,
        forced_event: false,
    };
    let control_outcome = DigRuntimeService::sqlite(control.path())
        .dig(request)
        .expect("control luminosity-yield Dig");
    let coal_outcome = DigRuntimeService::sqlite(coal.path())
        .dig(request)
        .expect("Coal luminosity-yield Dig");
    assert_eq!(
        SqliteDigRuntimeStore::new(coal.path())
            .snapshot(actor, guild)
            .expect("reload Coal tunnel")
            .tunnel
            .expect("Coal tunnel")
            .luminosity,
        23,
        "Magma drain must make the request dark before relic yield resolves",
    );
    assert_eq!(coal_outcome.jc_earned, control_outcome.jc_earned + 1);
}

#[test]
fn sqlite_overgrowth_adds_ten_and_consumes_one_charge_exactly_once() {
    let control = NamedTempFile::new().expect("control Overgrowth database");
    let boosted = NamedTempFile::new().expect("boosted Overgrowth database");
    let actor = 60_091;
    let guild = 60_092;
    let now = 1_900_090_000;
    for database in [&control, &boosted] {
        seed_live_runtime_tunnel(database, actor, guild, now, 10, 1, Some(now - 7_200));
        Connection::open(database.path())
            .expect("seed cave-free streak")
            .execute(
                "UPDATE tunnels SET cavein_free_streak=9,current_run_jc=17
                     WHERE discord_id=?1 AND guild_id=?2",
                params![actor, guild],
            )
            .expect("seed cave-free counters");
    }
    grant_overgrowth(&boosted, actor, guild, now, 2);
    let dig_now = find_non_cave_dig_time(actor, guild, now);
    let request = DigRuntimeRequest {
        discord_id: actor,
        guild_id: guild,
        now: dig_now,
        paid: false,
        forced_event: false,
    };
    let control_outcome = DigRuntimeService::sqlite(control.path())
        .dig(request)
        .expect("control Dig");
    let service = DigRuntimeService::sqlite(boosted.path());
    let boosted_outcome = service.dig(request).expect("Overgrowth Dig");
    assert!(!control_outcome.cave_in && !boosted_outcome.cave_in);
    assert_eq!(boosted_outcome.jc_earned, control_outcome.jc_earned + 10);
    let active = ManashopRepository::new(boosted.path())
        .active_for(actor, Some(guild), "overgrowth", dig_now)
        .expect("read remaining Overgrowth");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].data.charges_remaining, Some(1));
    let tunnel = SqliteDigRuntimeStore::new(boosted.path())
        .snapshot(actor, guild)
        .expect("reload successful Overgrowth tunnel")
        .tunnel
        .expect("successful Overgrowth tunnel");
    assert_eq!(tunnel.cavein_free_streak, 10);
    assert_eq!(tunnel.current_run_jc, 17 + boosted_outcome.jc_earned);

    let retry = service.dig(request).expect("duplicate request response");
    assert!(!retry.success, "cooldown must reject duplicate Dig");
    let active = ManashopRepository::new(boosted.path())
        .active_for(actor, Some(guild), "overgrowth", dig_now)
        .expect("read charge after duplicate");
    assert_eq!(active[0].data.charges_remaining, Some(1));
}

#[test]
fn sqlite_cave_in_consumes_overgrowth_without_adding_ten() {
    let control = NamedTempFile::new().expect("control cave database");
    let boosted = NamedTempFile::new().expect("Overgrowth cave database");
    let actor = 60_093;
    let guild = 60_094;
    let now = 1_900_091_000;
    for database in [&control, &boosted] {
        seed_live_runtime_tunnel(database, actor, guild, now, 10, 1, Some(now - 7_200));
        Connection::open(database.path())
            .expect("seed cave counters")
            .execute(
                "UPDATE tunnels SET cavein_free_streak=9,current_run_jc=17
                     WHERE discord_id=?1 AND guild_id=?2",
                params![actor, guild],
            )
            .expect("seed cave counters");
    }
    grant_overgrowth(&boosted, actor, guild, now, 1);
    let dig_now = find_dig_time_with_unit_between(actor, guild, now, 0.0, 0.01);
    let request = DigRuntimeRequest {
        discord_id: actor,
        guild_id: guild,
        now: dig_now,
        paid: false,
        forced_event: false,
    };
    let control_outcome = DigRuntimeService::sqlite(control.path())
        .dig(request)
        .expect("control cave Dig");
    let boosted_outcome = DigRuntimeService::sqlite(boosted.path())
        .dig(request)
        .expect("Overgrowth cave Dig");
    assert!(control_outcome.cave_in && boosted_outcome.cave_in);
    assert_eq!(boosted_outcome.jc_earned, control_outcome.jc_earned);
    let connection = Connection::open(boosted.path()).expect("reload Overgrowth cave");
    let (triggered, data) = connection
        .query_row(
            "SELECT triggered,data FROM manashop_buffs
                 WHERE discord_id=?1 AND guild_id=?2 AND buff_type='overgrowth'",
            params![actor, guild],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("persisted Overgrowth charge");
    assert_eq!(triggered, 1);
    assert_eq!(
        serde_json::from_str::<Value>(&data)
            .expect("Overgrowth JSON")
            .get("charges_remaining")
            .and_then(Value::as_i64),
        Some(0)
    );
    let tunnel = SqliteDigRuntimeStore::new(boosted.path())
        .snapshot(actor, guild)
        .expect("reload Overgrowth cave tunnel")
        .tunnel
        .expect("Overgrowth cave tunnel");
    assert_eq!(tunnel.cavein_free_streak, 0);
    assert_eq!(tunnel.current_run_jc, 17);
}

#[test]
fn sqlite_overgrowth_spend_rolls_back_when_later_commit_leg_fails() {
    let database = NamedTempFile::new().expect("Overgrowth rollback database");
    let actor = 60_095;
    let guild = 60_096;
    let now = 1_900_092_000;
    seed_live_runtime_tunnel(&database, actor, guild, now, 10, 1, Some(now - 7_200));
    grant_overgrowth(&database, actor, guild, now, 1);
    let store = SqliteDigRuntimeStore::new(database.path());
    let snapshot = store.snapshot(actor, guild).expect("Overgrowth snapshot");
    let mut next = snapshot.clone();
    let tunnel = next.tunnel.as_mut().expect("Overgrowth tunnel");
    tunnel.depth = 11;
    tunnel.total_digs = 2;
    tunnel.last_dig_at = Some(now);
    assert!(matches!(
        store.commit(DigRuntimeCommit {
            expected: DigRuntimeVersion::from(&snapshot),
            next,
            delivery_draft: None,
            consumed_item_ids: Vec::new(),
            pet_work_claim: Some(cama_domain::pet::PetDigWorkClaim {
                pet_id: i64::MAX,
                expected_units: 0,
                expected_at: now,
                new_units: 0,
                new_at: now,
            }),
            consume_overgrowth: true,
            depth_before: 10,
            depth_after: 11,
            jc_delta: 10,
            vanity_tax: 0,
            balance_cost: 0,
            action_type: "dig".to_owned(),
            detail: "{}".to_owned(),
            now,
        }),
        Err(DigRuntimeStoreError::PetWorkConflict)
    ));
    let active = ManashopRepository::new(database.path())
        .active_for(actor, Some(guild), "overgrowth", now)
        .expect("read rolled-back charge");
    assert_eq!(active[0].data.charges_remaining, Some(1));
    let reloaded = store
        .snapshot(actor, guild)
        .expect("reload rolled-back tunnel");
    assert_eq!(reloaded, snapshot);
}

#[test]
fn sqlite_vanished_overgrowth_charge_rejects_whole_dig_stage() {
    let database = NamedTempFile::new().expect("Overgrowth conflict database");
    let actor = 60_097;
    let guild = 60_098;
    let now = 1_900_093_000;
    seed_live_runtime_tunnel(&database, actor, guild, now, 10, 1, Some(now - 7_200));
    grant_overgrowth(&database, actor, guild, now, 1);
    let store = SqliteDigRuntimeStore::new(database.path());
    let snapshot = store.snapshot(actor, guild).expect("Overgrowth snapshot");
    assert!(
        ManashopRepository::new(database.path())
            .consume_data_charge_atomic(actor, Some(guild), "overgrowth", now)
            .expect("consume charge before commit")
    );
    let mut next = snapshot.clone();
    let tunnel = next.tunnel.as_mut().expect("Overgrowth tunnel");
    tunnel.depth = 11;
    tunnel.total_digs = 2;
    tunnel.last_dig_at = Some(now);
    assert!(matches!(
        store.commit(DigRuntimeCommit {
            expected: DigRuntimeVersion::from(&snapshot),
            next,
            delivery_draft: None,
            consumed_item_ids: Vec::new(),
            pet_work_claim: None,
            consume_overgrowth: true,
            depth_before: 10,
            depth_after: 11,
            jc_delta: 10,
            vanity_tax: 0,
            balance_cost: 0,
            action_type: "dig".to_owned(),
            detail: "{}".to_owned(),
            now,
        }),
        Err(DigRuntimeStoreError::OvergrowthConflict)
    ));
    assert_eq!(
        store.snapshot(actor, guild).expect("reload conflict"),
        snapshot
    );
    assert_eq!(
        Connection::open(database.path())
            .expect("reload action audit")
            .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                .get::<_, i64>(0))
            .expect("action count"),
        0
    );
}

#[test]
fn sqlite_cave_in_does_not_persist_an_artifact() {
    let database = NamedTempFile::new().expect("cave artifact database");
    let actor = 60_101;
    let guild = 60_102;
    let now = 1_900_100_000;
    seed_live_runtime_tunnel(&database, actor, guild, now, 10, 1, Some(now - 7_200));
    let dig_now = find_cave_dig_time(actor, guild, now);
    let service = DigRuntimeService::sqlite(database.path());
    let outcome = service
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now: dig_now,
            paid: false,
            forced_event: false,
        })
        .expect("cave-in Dig");
    assert!(outcome.success && outcome.cave_in);
    assert_eq!(outcome.artifact_id, None);
    let snapshot = SqliteDigRuntimeStore::new(database.path())
        .snapshot(actor, guild)
        .expect("reload cave artifact database");
    assert_eq!(snapshot.artifacts.len(), 0);
    assert_eq!(
        snapshot.tunnel.expect("cave tunnel").current_run_artifacts,
        0
    );
}

#[test]
fn sqlite_successful_artifact_persists_and_increments_current_run_artifacts_once() {
    let database = NamedTempFile::new().expect("successful artifact database");
    let actor = 61_001;
    let guild = 61_002;
    let seed_now = 1_900_100_000;
    seed_live_runtime_tunnel(
        &database,
        actor,
        guild,
        seed_now,
        55,
        1,
        Some(seed_now - 7_200),
    );
    let today = cama_domain::game_date::game_date_for_timestamp(seed_now as f64)
        .expect("artifact game date");
    let connection = Connection::open(database.path()).expect("artifact connection");
    connection
            .execute(
                "UPDATE tunnels SET route_state=?1,mutations=?2
                 WHERE discord_id=?3 AND guild_id=?4",
                params![
                    r#"{"end_depth":75,"layer":"Crystal","offered":["glass_labyrinth","prismatic_fault","resonant_gallery"],"selected":"glass_labyrinth","start_depth":50}"#,
                    r#"[{"id":"treasure_sense"}]"#,
                    actor,
                    guild,
                ],
            )
            .expect("seed artifact route and mutation");
    connection
        .execute(
            "INSERT INTO dig_artifacts(
                     discord_id,guild_id,artifact_id,found_at,is_relic,equipped
                 ) VALUES(?1,?2,'echo_stone',?3,1,1)",
            params![actor, guild, seed_now - 100],
        )
        .expect("seed echo stone");
    connection
        .execute(
            "INSERT INTO dig_weather(guild_id,game_date,layer_name,weather_id)
                 VALUES (?1,?2,'Crystal','fossil_rush'),(?1,?2,'Dirt','earthworm_migration')",
            params![guild, today],
        )
        .expect("seed artifact weather");
    drop(connection);

    // The fixed request seed has a non-cave roll and a find roll inside
    // the composed 0.5% * (route * weather * Echo Stone * Treasure Sense)
    // rate. The artifact policy then consumes rarity and candidate entropy.
    let dig_now = seed_now + 312;
    let service = DigRuntimeService::sqlite(database.path());
    let outcome = service
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now: dig_now,
            paid: false,
            forced_event: false,
        })
        .expect("successful artifact Dig");
    let artifact_id = outcome.artifact_id.expect("artifact drop");
    let retry = service
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now: dig_now,
            paid: false,
            forced_event: false,
        })
        .expect("artifact retry response");
    assert!(
        !retry.success,
        "cooldown must prevent a duplicate artifact roll"
    );
    let snapshot = SqliteDigRuntimeStore::new(database.path())
        .snapshot(actor, guild)
        .expect("reload successful artifact database");
    assert_eq!(
        snapshot
            .artifacts
            .iter()
            .filter(|artifact| artifact.artifact_id == artifact_id)
            .count(),
        1
    );
    assert_eq!(
        snapshot
            .tunnel
            .expect("successful artifact tunnel")
            .current_run_artifacts,
        1
    );
}

#[test]
fn sqlite_artifact_commit_conflict_persists_neither_artifact_nor_counter() {
    let database = NamedTempFile::new().expect("artifact rollback database");
    let actor = 61_101;
    let guild = 61_102;
    let now = 1_900_110_000;
    seed_live_runtime_tunnel(&database, actor, guild, now, 10, 1, Some(now - 7_200));
    let store = SqliteDigRuntimeStore::new(database.path());
    let snapshot = store
        .snapshot(actor, guild)
        .expect("artifact rollback snapshot");
    let mut next = snapshot.clone();
    next.artifacts.push(super::DigRuntimeArtifact {
        id: 1,
        artifact_id: "mole_claws".to_owned(),
        is_relic: true,
        equipped: false,
    });
    next.tunnel
        .as_mut()
        .expect("artifact rollback tunnel")
        .current_run_artifacts = 1;
    let connection = Connection::open(database.path()).expect("artifact conflict connection");
    connection
        .execute(
            "UPDATE tunnels SET depth=depth+1 WHERE discord_id=?1 AND guild_id=?2",
            params![actor, guild],
        )
        .expect("force artifact conflict");
    drop(connection);
    assert!(matches!(
        store.commit(super::DigRuntimeCommit {
            expected: super::DigRuntimeVersion::from(&snapshot),
            next,
            delivery_draft: None,
            consumed_item_ids: Vec::new(),
            pet_work_claim: None,
            consume_overgrowth: false,
            depth_before: 10,
            depth_after: 11,
            jc_delta: 0,
            vanity_tax: 0,
            balance_cost: 0,
            action_type: "dig".to_owned(),
            detail: "{}".to_owned(),
            now,
        }),
        Err(super::DigRuntimeStoreError::Conflict)
    ));
    let connection = Connection::open(database.path()).expect("reload artifact rollback");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_artifacts", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("artifact count after rollback"),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT current_run_artifacts FROM tunnels", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("artifact counter after rollback"),
        0
    );
}

#[test]
fn sqlite_luminosity_refill_updates_timestamp_and_applies_torch_after_drain() {
    let database = NamedTempFile::new().expect("luminosity database");
    let discord_id = 60_001;
    let guild_id = 60_002;
    let now = 1_900_000_000;
    seed_live_runtime_tunnel(
        &database,
        discord_id,
        guild_id,
        now,
        10,
        1,
        Some(now - 7_200),
    );
    let connection = Connection::open(database.path()).expect("open luminosity database");
    connection
        .execute(
            "UPDATE tunnels SET luminosity=10,last_lum_update_at=?1
                 WHERE discord_id=?2 AND guild_id=?3",
            params![now - 86_400, discord_id, guild_id],
        )
        .expect("seed stale luminosity anchor");
    connection
        .execute(
            "INSERT INTO dig_inventory (discord_id,guild_id,item_type,queued,created_at)
                 VALUES (?1,?2,'torch',1,?3)",
            params![discord_id, guild_id, now],
        )
        .expect("queue torch");
    drop(connection);

    let dig_now = find_non_cave_dig_time(discord_id, guild_id, now);
    let outcome = DigRuntimeService::sqlite(database.path())
        .dig(DigRuntimeRequest {
            discord_id,
            guild_id,
            now: dig_now,
            paid: false,
            forced_event: false,
        })
        .expect("live luminosity Dig");
    assert!(outcome.success);
    let connection = Connection::open(database.path()).expect("reload luminosity database");
    let (luminosity, last_update) = connection
        .query_row(
            "SELECT luminosity,last_lum_update_at FROM tunnels
                 WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .expect("persisted luminosity");
    assert_eq!(last_update, dig_now);
    // 10 + one day's 20-point refill, less the Dirt drain, then +50 Torch.
    assert!(
        luminosity >= 60,
        "torch must be applied after the drain: {luminosity}"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_inventory
                     WHERE discord_id=?1 AND guild_id=?2 AND item_type='torch'",
                params![discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("torch consumption"),
        0
    );
}

#[test]
fn sqlite_queued_charges_stack_and_boss_prep_items_are_not_consumed() {
    let database = NamedTempFile::new().expect("queued-item database");
    let discord_id = 60_003;
    let guild_id = 60_004;
    let now = 1_900_010_000;
    seed_live_runtime_tunnel(
        &database,
        discord_id,
        guild_id,
        now,
        10,
        1,
        Some(now - 7_200),
    );
    let connection = Connection::open(database.path()).expect("open queued-item database");
    connection
        .execute(
            "UPDATE tunnels SET hard_hat_charges=2,grappling_hook_charges=4,
                 void_bait_digs=2 WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
        )
        .expect("seed existing charges");
    for item_type in [
        "hard_hat",
        "grappling_hook",
        "void_bait",
        "reinforcement",
        "tempered_whetstone",
    ] {
        connection
            .execute(
                "INSERT INTO dig_inventory (discord_id,guild_id,item_type,queued,created_at)
                     VALUES (?1,?2,?3,1,?4)",
                params![discord_id, guild_id, item_type, now],
            )
            .expect("queue consumable");
    }
    drop(connection);

    let dig_now = find_non_cave_dig_time(discord_id, guild_id, now);
    let outcome = DigRuntimeService::sqlite(database.path())
        .dig(DigRuntimeRequest {
            discord_id,
            guild_id,
            now: dig_now,
            paid: false,
            forced_event: false,
        })
        .expect("live queued-item Dig");
    assert!(outcome.success);
    let connection = Connection::open(database.path()).expect("reload queued-item database");
    let charges = connection
        .query_row(
            "SELECT hard_hat_charges,grappling_hook_charges,void_bait_digs,
                        reinforced_until
                 FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .expect("persisted queued charges");
    // The queued Hard Hat grants three charges, then the imminent Dig
    // consumes one unconditionally while bypassing the cave RNG.
    assert_eq!(charges.0, 4);
    assert_eq!(charges.1, 9);
    assert!(charges.2 >= 4);
    assert!(charges.3 > dig_now);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_inventory
                     WHERE discord_id=?1 AND guild_id=?2 AND item_type='tempered_whetstone'",
                params![discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("boss-prep inventory"),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_inventory
                     WHERE discord_id=?1 AND guild_id=?2 AND item_type IN
                       ('hard_hat','grappling_hook','void_bait','reinforcement')",
                params![discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("applied inventory consumed"),
        0
    );
}

#[test]
fn sqlite_cave_probability_uses_luminosity_perk_buff_stats_and_mana_modifiers() {
    let control_db = NamedTempFile::new().expect("control cave database");
    let modified_db = NamedTempFile::new().expect("modified cave database");
    let actor = 60_021;
    let guild = 60_022;
    let now = 1_900_015_000;
    seed_live_runtime_tunnel(&control_db, actor, guild, now, 10, 1, Some(now - 7_200));
    seed_live_runtime_tunnel(&modified_db, actor, guild, now, 10, 1, Some(now - 7_200));
    let today =
        cama_domain::game_date::game_date_for_timestamp(now as f64).expect("cave probability date");
    for database in [&control_db, &modified_db] {
        let connection = Connection::open(database.path()).expect("seed cave weather");
        for (layer, weather) in [("Dirt", "earthworm_migration"), ("Stone", "fossil_rush")] {
            connection
                .execute(
                    "INSERT INTO dig_weather(guild_id,game_date,layer_name,weather_id)
                         VALUES (?1,?2,?3,?4)",
                    params![guild, today, layer, weather],
                )
                .expect("seed neutral cave weather");
        }
    }
    let connection = Connection::open(modified_db.path()).expect("modified cave connection");
    connection
        .execute(
            "UPDATE tunnels SET luminosity=25,prestige_perks=?1,stat_smarts=1,
                 temp_buffs=?2 WHERE discord_id=?3 AND guild_id=?4",
            params![
                r#"["dark_adaptation"]"#,
                r#"{"digs_remaining":1,"effect":{"cave_in_reduction":0.01}}"#,
                actor,
                guild,
            ],
        )
        .expect("seed cave modifiers");
    connection
        .execute(
            "INSERT INTO dig_artifacts(
                     discord_id,guild_id,artifact_id,found_at,is_relic,equipped
                 ) VALUES(?1,?2,'crystal_compass',?3,1,1)",
            params![actor, guild, now - 100],
        )
        .expect("seed crystal compass");
    connection
        .execute(
            "INSERT INTO player_mana(
                     discord_id,guild_id,current_land,assigned_date,consumed_today
                 ) VALUES(?1,?2,'Swamp',?3,0)",
            params![actor, guild, today],
        )
        .expect("seed mana hazard");
    drop(connection);

    // The same request seed is safely outside the P0 control chance
    // (~4.5%) but inside the dark, buff/stat/mana-adjusted chance.
    let dig_now = find_dig_time_with_unit_between(actor, guild, now, 0.10, 0.18);
    let control = DigRuntimeService::sqlite(control_db.path())
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now: dig_now,
            paid: false,
            forced_event: false,
        })
        .expect("control cave probability dig");
    let modified = DigRuntimeService::sqlite(modified_db.path())
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now: dig_now,
            paid: false,
            forced_event: false,
        })
        .expect("modified cave probability dig");
    assert!(
        !control.cave_in,
        "control should remain below the cave roll"
    );
    assert!(
        modified.cave_in,
        "live policy modifiers should raise dark cave risk"
    );
    assert_eq!(modified.depth_before, 10);
    assert!(modified.depth_after < modified.depth_before);
    assert_eq!(
        Connection::open(modified_db.path())
            .expect("reload modified cave database")
            .query_row(
                "SELECT temp_buffs FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![actor, guild],
                |row| row.get::<_, Option<String>>(0),
            )
            .expect("expired cave buff"),
        None,
        "the active hazard buff is consumed by the same committed Dig",
    );
}

#[test]
fn sqlite_catastrophic_cave_in_does_not_resurrect_cleared_multidig_buff() {
    let database = NamedTempFile::new().expect("catastrophic cave database");
    let actor = 60_091;
    let guild = 60_092;
    let seed_now = 1_900_083_000;
    let dig_now = 1_900_083_041;
    seed_live_runtime_tunnel(
        &database,
        actor,
        guild,
        seed_now,
        240,
        1,
        Some(seed_now - 7_200),
    );
    let today = game_date_for_timestamp(dig_now as f64).expect("catastrophic cave date");
    seed_two_weather_rows(
        &database,
        guild,
        &today,
        crate::dig_service::layer_at(240).name,
        "cooling_period",
    );
    Connection::open(database.path())
        .expect("catastrophic buff connection")
        .execute(
            "UPDATE tunnels SET temp_buffs=?1
                 WHERE discord_id=?2 AND guild_id=?3",
            params![
                r#"{"digs_remaining":2,"effect":{"advance_bonus":2}}"#,
                actor,
                guild,
            ],
        )
        .expect("seed multi-Dig buff");

    let outcome = DigRuntimeService::sqlite(database.path())
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now: dig_now,
            paid: false,
            forced_event: false,
        })
        .expect("catastrophic cave Dig");
    let detail = serde_json::from_str::<Value>(
        outcome
            .cave_in_detail
            .as_deref()
            .expect("catastrophic detail"),
    )
    .expect("catastrophic detail JSON");
    assert!(outcome.cave_in);
    assert_eq!(
        detail.get("type").and_then(Value::as_str),
        Some("catastrophic")
    );
    assert_eq!(detail.get("depth_after").and_then(Value::as_i64), Some(225));
    assert_eq!(outcome.depth_after, 225);
    assert_eq!(
        SqliteDigRuntimeStore::new(database.path())
            .snapshot(actor, guild)
            .expect("reload catastrophic cave")
            .tunnel
            .expect("persisted tunnel")
            .temp_buffs,
        None,
        "the generic duration decrement must not recreate a catastrophic-cleared buff",
    );
}

#[test]
fn sqlite_queued_grapple_and_hard_hat_are_visible_to_cave_consequences() {
    let hard_hat_db = NamedTempFile::new().expect("hard-hat cave database");
    let grapple_db = NamedTempFile::new().expect("grapple cave database");
    let hard_hat_actor = 60_023;
    let grapple_actor = 60_024;
    let guild = 60_025;
    let now = 1_900_016_000;
    seed_live_runtime_tunnel(
        &hard_hat_db,
        hard_hat_actor,
        guild,
        now,
        10,
        1,
        Some(now - 7_200),
    );
    seed_live_runtime_tunnel(
        &grapple_db,
        grapple_actor,
        guild,
        now,
        180,
        1,
        Some(now - 7_200),
    );
    let connection = Connection::open(hard_hat_db.path()).expect("hard-hat connection");
    connection
        .execute(
            "UPDATE tunnels SET luminosity=40 WHERE discord_id=?1 AND guild_id=?2",
            params![hard_hat_actor, guild],
        )
        .expect("seed hard-hat luminosity");
    for item_type in ["hard_hat", "torch"] {
        connection
            .execute(
                "INSERT INTO dig_inventory(
                         discord_id,guild_id,item_type,queued,created_at
                     ) VALUES(?1,?2,?3,1,?4)",
                params![hard_hat_actor, guild, item_type, now],
            )
            .expect("queue hard-hat item");
    }
    drop(connection);
    let connection = Connection::open(grapple_db.path()).expect("grapple connection");
    connection
        .execute(
            "INSERT INTO dig_inventory(
                     discord_id,guild_id,item_type,queued,created_at
                 ) VALUES(?1,?2,'grappling_hook',1,?3)",
            params![grapple_actor, guild, now],
        )
        .expect("queue grapple");
    drop(connection);

    let hard_hat_now = find_non_cave_dig_time(hard_hat_actor, guild, now);
    let hard_hat = DigRuntimeService::sqlite(hard_hat_db.path())
        .dig(DigRuntimeRequest {
            discord_id: hard_hat_actor,
            guild_id: guild,
            now: hard_hat_now,
            paid: false,
            forced_event: false,
        })
        .expect("hard-hat dig");
    assert!(hard_hat.success);
    assert!(!hard_hat.cave_in);
    let hard_hat_snapshot = SqliteDigRuntimeStore::new(hard_hat_db.path())
        .snapshot(hard_hat_actor, guild)
        .expect("hard-hat snapshot");
    let hard_hat_tunnel = hard_hat_snapshot.tunnel.expect("hard-hat tunnel");
    assert_eq!(hard_hat_tunnel.hard_hat_charges, 2);
    // Dirt has no ordinary luminosity drain: 40 + 50 Torch - 10 Hard Hat.
    // This catches applying the protection cost before the Torch hook.
    assert_eq!(hard_hat_tunnel.luminosity, 80);

    let grapple_now = find_cave_dig_time(grapple_actor, guild, now);
    let grapple = DigRuntimeService::sqlite(grapple_db.path())
        .dig(DigRuntimeRequest {
            discord_id: grapple_actor,
            guild_id: guild,
            now: grapple_now,
            paid: false,
            forced_event: false,
        })
        .expect("grapple dig");
    assert!(grapple.success && grapple.cave_in);
    assert_eq!(grapple.depth_after, grapple.depth_before);
    let detail =
        serde_json::from_str::<Value>(&grapple.cave_in_detail.expect("cushioned cave detail"))
            .expect("cushioned cave JSON");
    assert_eq!(detail["type"], "cushioned");
    assert_eq!(detail["block_loss"], 0);
    let grapple_tunnel = SqliteDigRuntimeStore::new(grapple_db.path())
        .snapshot(grapple_actor, guild)
        .expect("grapple snapshot")
        .tunnel
        .expect("grapple tunnel");
    assert_eq!(grapple_tunnel.grappling_hook_charges, 4);
}

#[test]
fn sqlite_blocked_cooldown_does_not_initialize_weather() {
    let database = NamedTempFile::new().expect("cooldown database");
    let discord_id = 60_005;
    let guild_id = 60_006;
    let now = 1_900_020_000;
    seed_live_runtime_tunnel(&database, discord_id, guild_id, now, 10, 1, Some(now - 1));
    let outcome = DigRuntimeService::sqlite(database.path())
        .dig(DigRuntimeRequest {
            discord_id,
            guild_id,
            now,
            paid: false,
            forced_event: false,
        })
        .expect("blocked cooldown response");
    assert!(!outcome.success);
    assert!(outcome.cooldown_remaining > 0);
    let connection = Connection::open(database.path()).expect("reload cooldown database");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_weather", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("weather row count"),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("action row count"),
        0
    );
}

#[test]
fn sqlite_first_dig_requires_unstarted_tunnel_and_persists_streak() {
    let database = NamedTempFile::new().expect("first-dig database");
    let discord_id = 60_007;
    let guild_id = 60_008;
    let now = 1_900_030_000;
    seed_live_runtime_tunnel(&database, discord_id, guild_id, now, 0, 0, Some(now - 1));
    let service = DigRuntimeService::sqlite(database.path());
    let partial = service
        .dig(DigRuntimeRequest {
            discord_id,
            guild_id,
            now,
            paid: false,
            forced_event: false,
        })
        .expect("partial tunnel admission");
    assert!(!partial.success);
    assert!(!partial.first_dig);
    let connection = Connection::open(database.path()).expect("reopen first-dig database");
    connection
        .execute(
            "UPDATE tunnels SET last_dig_at=NULL WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
        )
        .expect("restore unstarted timestamp");
    drop(connection);

    let first = service
        .dig(DigRuntimeRequest {
            discord_id,
            guild_id,
            now,
            paid: false,
            forced_event: false,
        })
        .expect("unstarted first Dig");
    assert!(first.success);
    assert!(first.first_dig);
    let today = super::game_date_for_timestamp(now as f64).expect("game date");
    let connection = Connection::open(database.path()).expect("reload first-dig database");
    let streak = connection
        .query_row(
            "SELECT total_digs,streak_days,streak_last_date
                 FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .expect("first-dig streak");
    assert_eq!(streak, (1, 1, today));
}

#[test]
fn sqlite_reduced_injury_ticks_and_slower_cooldown_does_not_halve_advance() {
    let reduced_db = NamedTempFile::new().expect("reduced injury database");
    let normal_db = NamedTempFile::new().expect("normal injury database");
    let actor = 60_009;
    let guild = 60_010;
    let now = 1_900_040_000;
    seed_live_runtime_tunnel(&reduced_db, actor, guild, now, 10, 1, Some(now - 7_200));
    seed_live_runtime_tunnel(&normal_db, actor, guild, now, 10, 1, Some(now - 7_200));
    let reduced_connection = Connection::open(reduced_db.path()).expect("reduced connection");
    reduced_connection
        .execute(
            "UPDATE tunnels SET injury_state=?1 WHERE discord_id=?2 AND guild_id=?3",
            params![
                r#"{"type":"reduced_advance","digs_remaining":1}"#,
                actor,
                guild
            ],
        )
        .expect("seed reduced injury");
    drop(reduced_connection);
    let dig_now = find_non_cave_dig_time(actor, guild, now);
    let reduced = DigRuntimeService::sqlite(reduced_db.path())
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now: dig_now,
            paid: false,
            forced_event: false,
        })
        .expect("reduced injury Dig");
    let normal = DigRuntimeService::sqlite(normal_db.path())
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now: dig_now,
            paid: false,
            forced_event: false,
        })
        .expect("normal Dig");
    assert!(reduced.success && normal.success);
    assert!(reduced.advance <= normal.advance);
    let reduced_connection = Connection::open(reduced_db.path()).expect("reload reduced");
    assert_eq!(
        reduced_connection
            .query_row("SELECT injury_state FROM tunnels", [], |row| {
                row.get::<_, Option<String>>(0)
            })
            .expect("injury state"),
        None
    );
    drop(reduced_connection);

    let slower_db = NamedTempFile::new().expect("slower injury database");
    let slower_control_db = NamedTempFile::new().expect("slower injury control database");
    seed_live_runtime_tunnel(&slower_db, actor, guild, now, 10, 1, Some(now - 3_600));
    seed_live_runtime_tunnel(
        &slower_control_db,
        actor,
        guild,
        now,
        10,
        1,
        Some(now - 6 * 3_600),
    );
    let game_date = super::game_date_for_timestamp(now as f64).expect("injury game date");
    for database in [&slower_db, &slower_control_db] {
        let connection = Connection::open(database.path()).expect("weather connection");
        for (layer, weather) in [("Dirt", "earthworm_migration"), ("Stone", "fossil_rush")] {
            connection
                .execute(
                    "INSERT INTO dig_weather(guild_id,game_date,layer_name,weather_id)
                         VALUES (?1,?2,?3,?4)",
                    params![guild, game_date, layer, weather],
                )
                .expect("seed deterministic weather");
        }
    }
    let slower_connection = Connection::open(slower_db.path()).expect("slower connection");
    slower_connection
        .execute(
            "UPDATE tunnels SET injury_state=?1,temp_curses=?2
                 WHERE discord_id=?3 AND guild_id=?4",
            params![
                r#"{"type":"slower_cooldown","digs_remaining":1}"#,
                r#"{"digs_remaining":1,"effect":{"cooldown_penalty":1.0}}"#,
                actor,
                guild
            ],
        )
        .expect("seed slower injury");
    drop(slower_connection);
    let blocked = DigRuntimeService::sqlite(slower_db.path())
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now,
            paid: false,
            forced_event: false,
        })
        .expect("slower injury cooldown");
    assert!(!blocked.success);
    assert_eq!(blocked.cooldown_remaining, 13 * 30 * 60);
    let slower_connection = Connection::open(slower_db.path()).expect("reopen slower");
    slower_connection
        .execute(
            "UPDATE tunnels SET last_dig_at=?1 WHERE discord_id=?2 AND guild_id=?3",
            params![now - (15 * 3_600 / 2), actor, guild],
        )
        .expect("clear slower cooldown");
    drop(slower_connection);
    let admitted = DigRuntimeService::sqlite(slower_db.path())
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now,
            paid: false,
            forced_event: false,
        })
        .expect("admit slower injury");
    let control = DigRuntimeService::sqlite(slower_control_db.path())
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now,
            paid: false,
            forced_event: false,
        })
        .expect("admit no-injury control");
    assert!(admitted.success);
    assert!(control.success);
    assert_eq!(admitted.advance, control.advance);
}

#[test]
fn sqlite_first_dig_pet_work_respects_live_boss_boundary_cap() {
    let database = NamedTempFile::new().expect("first pet boundary database");
    let actor = 7;
    let guild = 9;
    let now = 1_900_050_000;
    seed_live_runtime_tunnel(&database, actor, guild, now, 0, 0, None);
    let connection = Connection::open(database.path()).expect("pet boundary connection");
    connection
        .execute(
            "UPDATE tunnels SET boss_progress=?1 WHERE discord_id=?2 AND guild_id=?3",
            params![r#"{"25":"active"}"#, actor, guild],
        )
        .expect("seed first boss boundary");
    seed_runtime_pet(&connection, now, 36 * DIG_WORK_UNITS_PER_BLOCK);
    drop(connection);
    let outcome = DigRuntimeService::with_config(
        SqliteDigRuntimeStore::new(database.path()),
        super::DigRuntimeConfig::default().with_pet_decay_per_day(20),
    )
    .dig(DigRuntimeRequest {
        discord_id: actor,
        guild_id: guild,
        now,
        paid: false,
        forced_event: false,
    })
    .expect("first pet Dig");
    assert!(outcome.success && outcome.first_dig);
    assert!(outcome.depth_after <= 24);
    assert!(outcome.pet_dig_bonus <= 12);
}

#[test]
fn sqlite_queued_reinforcement_does_not_cap_current_cave_loss() {
    let database = NamedTempFile::new().expect("reinforcement database");
    let actor = 60_013;
    let guild = 60_014;
    let now = 1_900_060_000;
    seed_live_runtime_tunnel(&database, actor, guild, now, 180, 1, Some(now - 7_200));
    let connection = Connection::open(database.path()).expect("reinforcement connection");
    connection
        .execute(
            "INSERT INTO dig_inventory (discord_id,guild_id,item_type,queued,created_at)
                 VALUES (?1,?2,'reinforcement',1,?3)",
            params![actor, guild, now],
        )
        .expect("queue reinforcement");
    drop(connection);
    let dig_now = find_cave_dig_time(actor, guild, now);
    let outcome = DigRuntimeService::sqlite(database.path())
        .dig(DigRuntimeRequest {
            discord_id: actor,
            guild_id: guild,
            now: dig_now,
            paid: false,
            forced_event: false,
        })
        .expect("reinforced cave Dig");
    assert!(outcome.success && outcome.cave_in);
    let detail = outcome.cave_in_detail.expect("cave detail");
    let detail = serde_json::from_str::<Value>(&detail).expect("cave JSON");
    assert!(
        detail
            .get("block_loss")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            > 8
    );
    let connection = Connection::open(database.path()).expect("reload reinforcement");
    assert!(
        connection
            .query_row("SELECT reinforced_until FROM tunnels", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("reinforcement stamp")
            > dig_now
    );
}

#[test]
fn sqlite_broken_equipped_pickaxe_does_not_reduce_luminosity_drain() {
    let active_db = NamedTempFile::new().expect("active pickaxe database");
    let broken_db = NamedTempFile::new().expect("broken pickaxe database");
    let actor = 60_015;
    let guild = 60_016;
    let now = 1_900_070_000;
    seed_live_runtime_tunnel(&active_db, actor, guild, now, 300, 1, Some(now - 7_200));
    seed_live_runtime_tunnel(&broken_db, actor, guild, now, 300, 1, Some(now - 7_200));
    for (database, durability) in [(&active_db, 20_i64), (&broken_db, 0_i64)] {
        let connection = Connection::open(database.path()).expect("pickaxe connection");
        connection
            .execute(
                "UPDATE tunnels SET pickaxe_tier=6,luminosity=100
                     WHERE discord_id=?1 AND guild_id=?2",
                params![actor, guild],
            )
            .expect("seed pickaxe fallback");
        connection
            .execute(
                "INSERT INTO dig_gear(
                         discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source
                     ) VALUES (?1,?2,'weapon',6,?3,1,?4,'batch-one')",
                params![actor, guild, durability, now],
            )
            .expect("seed equipped pickaxe");
    }
    let dig_now = find_non_cave_dig_time(actor, guild, now);
    for database in [&active_db, &broken_db] {
        DigRuntimeService::sqlite(database.path())
            .dig(DigRuntimeRequest {
                discord_id: actor,
                guild_id: guild,
                now: dig_now,
                paid: false,
                forced_event: false,
            })
            .expect("pickaxe Dig");
    }
    let active_luminosity = DigRuntimeService::sqlite(active_db.path())
        .snapshot(actor, guild)
        .expect("active snapshot")
        .tunnel
        .expect("active tunnel")
        .luminosity;
    let broken_luminosity = DigRuntimeService::sqlite(broken_db.path())
        .snapshot(actor, guild)
        .expect("broken snapshot")
        .tunnel
        .expect("broken tunnel")
        .luminosity;
    assert!(
        active_luminosity > broken_luminosity,
        "broken equipped pickaxe must not receive tier-6 drain reduction"
    );
}
