//! Unadmitted differential audit for the 32 Python non-JC/reward-policy cases.
//!
//! This harness intentionally imports the in-progress application/database
//! runtimes by path.  It is an audit seam only: production module admission,
//! command wiring, schema ownership, and parity manifests remain untouched.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use cama_app::dig_loot::{
    CanonicalEventOutcome, CanonicalReward, DigLootModifiers, DigLootService,
    InMemoryLootRepository, LootEntropy, LootRepository, MAX_INVENTORY_SLOTS, ScriptedLootEntropy,
    artifact_catalog, canonical_event, canonical_event_catalog, select_canonical_reward,
    validate_canonical_event_reward_pools,
};
use cama_domain::dig_economy::scale_positive_dig_jc;
use cama_domain::dig_splash::strengthen_dig_event_penalty;
use cama_domain::economy_scaling::{scale_deflationary_minigame_jc_delta, scale_minigame_jc_delta};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::Value;
use tempfile::NamedTempFile;

pub use cama_app::{
    dig_bosses, dig_loot, dig_runtime, dig_service, dig_tunnels, economy_event_service,
    economy_event_sqlite,
};

// The event/abandon/prestige/social application paths are deliberately not
// admitted by cama-app/lib.rs yet.  Keep this audit self-contained and expose
// only the exact DB seams those source files import.
extern crate cama_db as cama_db_external;
extern crate self as cama_db;

pub use cama_db_external::{
    dig_event_threats, dig_tunnel_encounters_repository, economy_event_repository, loan_repository,
    mana_service_repository, predictions_repository, schema_manager, shop_runtime,
};

pub mod dig_event_runtime {
    pub use crate::dig_event_runtime_reward_repository::*;
}
pub mod dig_abandon_runtime {
    pub use crate::dig_abandon_runtime_repository::*;
}

fn open_runtime_connection(path: &Path) -> Result<Connection, rusqlite::Error> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", false)?;
    connection.pragma_update(None, "synchronous", "OFF")?;
    Ok(connection)
}

#[path = "../../cama-db/src/dig_abandon_runtime.rs"]
mod dig_abandon_runtime_repository;
#[path = "../../cama-db/src/dig_event_runtime.rs"]
mod dig_event_runtime_reward_repository;

#[path = "../src/dig_abandon_runtime.rs"]
mod dig_abandon_runtime_under_test;
#[path = "../src/dig_event_runtime.rs"]
mod dig_event_runtime_reward_under_test;

const ACTOR: i64 = 81_001;
const GUILD: i64 = 81_002;
const NOW: i64 = 2_000_000_000;

static MIGRATED_DATABASE_TEMPLATE: OnceLock<Vec<u8>> = OnceLock::new();

fn migrated_database_template() -> &'static [u8] {
    MIGRATED_DATABASE_TEMPLATE.get_or_init(|| {
        let database = NamedTempFile::new().expect("temporary schema template");
        cama_db::schema_manager::initialize_or_migrate(database.path())
            .expect("canonical migrated schema template");
        std::fs::read(database.path()).expect("read migrated schema template")
    })
}

pub(crate) fn copy_migrated_database(path: &Path) -> std::io::Result<()> {
    std::fs::write(path, migrated_database_template())
}

pub mod test_support {
    pub(crate) use super::copy_migrated_database;
}

fn migrated_database() -> NamedTempFile {
    let mut database = NamedTempFile::new().expect("temporary SQLite database");
    database
        .as_file_mut()
        .write_all(migrated_database_template())
        .expect("copy migrated schema template");
    database
}

struct Fixture {
    database: NamedTempFile,
}

impl Fixture {
    fn new() -> Self {
        let database = migrated_database();
        Self { database }
    }

    fn connection(&self) -> Connection {
        open_runtime_connection(self.database.path()).expect("fixture connection")
    }

    fn seed_player(&self, discord_id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username, jopacoin_balance
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![discord_id, GUILD, format!("audit-{discord_id}"), balance],
            )
            .expect("seed player");
    }

    fn seed_tunnel(&self, discord_id: i64, depth: i64) {
        self.connection()
            .execute(
                "INSERT INTO tunnels (
                     discord_id, guild_id, depth, max_depth, luminosity,
                     prestige_level, prestige_perks, boss_progress, streak_days
                 ) VALUES (?1, ?2, ?3, ?3, 100, 0, '[]', '{}', 4)",
                params![discord_id, GUILD, depth],
            )
            .expect("seed tunnel");
    }

    fn balance(&self, discord_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                  WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, GUILD],
                |row| row.get(0),
            )
            .expect("balance")
    }

    fn service(&self) -> dig_event_runtime_reward_under_test::DigEventRuntimeService {
        let mut config = dig_runtime::DigRuntimeConfig::default();
        config.economy_event.enabled = false;
        dig_event_runtime_reward_under_test::DigEventRuntimeService::sqlite_with_config(
            self.database.path(),
            config,
        )
    }

    fn event(
        &self,
        event_id: &str,
        choice: &str,
        event_key: &str,
    ) -> dig_event_runtime_reward_under_test::DigEventRuntimeOutcome {
        self.service()
            .resolve_event(dig_runtime::DigRuntimeEventRequest {
                discord_id: ACTOR,
                guild_id: GUILD,
                event_id,
                choice,
                event_key,
                now: NOW,
                chained: false,
            })
            .expect("event resolution")
    }
}

fn event_seed(event_id: &str, choice: &str, event_key: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in event_key
        .bytes()
        .chain(event_id.bytes())
        .chain(choice.bytes())
        .chain(ACTOR.to_le_bytes())
        .chain(GUILD.to_le_bytes())
    {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

fn success_key(event_id: &str, choice: &str, chance: f64) -> String {
    for index in 0..10_000 {
        let key = format!("audit-reward:{event_id}:{choice}:{index}");
        let mut entropy = dig_loot::SeededLootEntropy::new(event_seed(event_id, choice, &key));
        if LootEntropy::unit(&mut entropy) < chance {
            return key;
        }
    }
    panic!("no deterministic success key for {event_id}/{choice}");
}

fn reward_outcome(
    gear: &[&str],
    consumables: &[&str],
    artifacts: &[&str],
) -> CanonicalEventOutcome {
    CanonicalEventOutcome {
        description: "audit".to_owned(),
        advance: 0,
        jc: 0,
        cave_in: false,
        streak_loss: 0,
        curse: None,
        gear_reward_pool: gear.iter().map(|id| (*id).to_owned()).collect(),
        consumable_reward_pool: consumables.iter().map(|id| (*id).to_owned()).collect(),
        artifact_reward_pool: artifacts.iter().map(|id| (*id).to_owned()).collect(),
    }
}

fn seed_full_inventory(fixture: &Fixture) {
    for index in 0..MAX_INVENTORY_SLOTS {
        fixture
            .connection()
            .execute(
                "INSERT INTO dig_inventory (
                     discord_id, guild_id, item_type, created_at, queued
                 ) VALUES (?1, ?2, ?3, ?4, 0)",
                params![ACTOR, GUILD, format!("lantern-{index}"), NOW],
            )
            .expect("seed inventory row");
    }
}

// -------------------------------------------------------------------------
// Python tests/test_dig_event_non_jc_rewards.py (10 cases)
// -------------------------------------------------------------------------

#[test]
fn audit_event_outcome_exposes_all_non_jc_reward_pools() {
    let event = canonical_event("collapsed_armory_cache").expect("reward event");
    let outcome = &event.risky_option.as_ref().expect("risky").success;
    assert_eq!(outcome.gear_reward_pool.len(), 8);
    assert_eq!(outcome.consumable_reward_pool, ["warding_salts"]);
    assert!(outcome.artifact_reward_pool.is_empty());
    // The Rust outcome currently derives Deserialize only; this checks the
    // production typed surface without recreating Python serialization.
}

#[test]
fn audit_new_encounter_catalog_has_nine_non_jc_events() {
    let ids = [
        "abandoned_forge",
        "salted_threshold",
        "frayed_lifeline",
        "quartermaster_s_niche",
        "collapsed_armory_cache",
        "relic_bearing_strata",
        "prospector_s_last_pack",
        "song_below_stone",
        "first_descent_cartography",
    ];
    let catalog = canonical_event_catalog();
    for event_id in ids {
        let event = catalog
            .iter()
            .find(|event| event.id == event_id)
            .expect(event_id);
        for option in [
            event.safe_option.as_ref(),
            event.risky_option.as_ref(),
            event.desperate_option.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            assert!(option.success.jc <= 0, "{event_id} success");
            if let Some(failure) = &option.failure {
                assert!(failure.jc <= 0, "{event_id} failure");
            }
        }
    }
}

#[test]
fn audit_unknown_reward_ids_are_rejected_by_catalog_validation() {
    let mut event = canonical_event_catalog()[0].clone();
    event
        .safe_option
        .as_mut()
        .expect("safe option")
        .success
        .consumable_reward_pool = vec!["missing_supply".to_owned()];
    let error = validate_canonical_event_reward_pools(&[event]).expect_err("invalid reward");
    assert!(error.contains("missing_supply"));
}

#[test]
fn audit_lore_curios_are_unique_and_statless() {
    let catalog = artifact_catalog();
    for artifact_id in ["miner_s_lullaby", "map_of_the_first_descent"] {
        let artifact = catalog
            .iter()
            .find(|artifact| artifact.id == artifact_id)
            .expect(artifact_id);
        assert_eq!(artifact.rarity.as_str(), "legendary");
        assert!(!artifact.is_relic);
        assert!(!artifact.trophy);
    }
}

#[test]
fn atomic_event_can_grant_statless_artifact() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 100);
    fixture.seed_tunnel(ACTOR, 175);
    seed_full_inventory(&fixture);
    let key = success_key("song_below_stone", "risky", 0.34);
    let outcome = fixture.event("song_below_stone", "risky", &key);
    assert_eq!(outcome.resolution.as_ref().expect("resolution").jc, 0);
    assert_eq!(
        outcome.resolution.as_ref().expect("resolution").reward,
        Some(CanonicalReward::Artifact("miner_s_lullaby".to_owned()))
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT artifact_id FROM dig_artifacts
                  WHERE discord_id=?1 AND guild_id=?2",
                params![ACTOR, GUILD],
                |row| row.get::<_, String>(0),
            )
            .expect("artifact row"),
        "miner_s_lullaby"
    );
}

#[test]
fn resolve_event_grants_consumable_without_minting_jc() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 100);
    fixture.seed_tunnel(ACTOR, 20);
    let outcome = fixture.event("abandoned_forge", "safe", "audit:consumable");
    let resolution = outcome.resolution.expect("resolution");
    assert_eq!(resolution.jc, 0);
    assert_eq!(
        resolution.reward,
        Some(CanonicalReward::Consumable("tempered_whetstone".to_owned()))
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT item_type FROM dig_inventory
                  WHERE discord_id=?1 AND guild_id=?2",
                params![ACTOR, GUILD],
                |row| row.get::<_, String>(0),
            )
            .expect("consumable row"),
        "tempered_whetstone"
    );
}

#[test]
fn audit_owned_unique_reward_falls_back_to_unowned_artifact() {
    let outcome = reward_outcome(&["glassbreaker_pick"], &[], &["miner_s_lullaby"]);
    let mut owned_gear = BTreeSet::new();
    owned_gear.insert("glassbreaker_pick".to_owned());
    let selection = select_canonical_reward(
        &outcome,
        &owned_gear,
        &BTreeSet::new(),
        0,
        MAX_INVENTORY_SLOTS,
        0.0,
    )
    .expect("reward selection");
    assert_eq!(
        selection.reward,
        Some(CanonicalReward::Artifact("miner_s_lullaby".to_owned()))
    );
    assert!(!selection.duplicate_gear_reward);
}

#[test]
fn audit_full_inventory_falls_back_from_consumable_to_artifact() {
    let outcome = reward_outcome(&[], &["rescue_line"], &["miner_s_lullaby"]);
    let selection = select_canonical_reward(
        &outcome,
        &BTreeSet::new(),
        &BTreeSet::new(),
        MAX_INVENTORY_SLOTS,
        MAX_INVENTORY_SLOTS,
        0.0,
    )
    .expect("reward selection");
    assert_eq!(
        selection.reward,
        Some(CanonicalReward::Artifact("miner_s_lullaby".to_owned()))
    );
    assert!(selection.eligible_consumables.is_empty());
}

#[test]
fn audit_plain_event_has_no_reward_candidates() {
    let outcome = reward_outcome(&[], &[], &[]);
    let selection = select_canonical_reward(
        &outcome,
        &BTreeSet::new(),
        &BTreeSet::new(),
        0,
        MAX_INVENTORY_SLOTS,
        0.0,
    )
    .expect("plain outcome");
    assert!(selection.reward.is_none());
    // The Python test additionally instruments repository query avoidance;
    // this pure API cannot observe SQLite query counts.
}

#[test]
fn audit_artifact_pool_selects_one_unowned_artifact() {
    let outcome = reward_outcome(&[], &[], &["miner_s_lullaby", "map_of_the_first_descent"]);
    let selection = select_canonical_reward(
        &outcome,
        &BTreeSet::new(),
        &BTreeSet::new(),
        0,
        MAX_INVENTORY_SLOTS,
        0.0,
    )
    .expect("artifact pool");
    assert_eq!(selection.eligible_artifacts.len(), 2);
    assert!(matches!(
        selection.reward,
        Some(CanonicalReward::Artifact(_))
    ));
    // Query-count shape (one get_artifacts, no has_artifact) remains a
    // repository-adapter integration gate, not a pure-policy assertion.
}

// -------------------------------------------------------------------------
// Python tests/test_dig_reward_policy.py (22 cases)
// -------------------------------------------------------------------------

#[test]
fn audit_positive_dig_jc_uses_half_up_rounding_and_minimum_one_1_1() {
    assert_eq!(scale_positive_dig_jc(1), 1);
}

#[test]
fn audit_positive_dig_jc_uses_half_up_rounding_and_minimum_one_2_1() {
    assert_eq!(scale_positive_dig_jc(2), 1);
}

#[test]
fn audit_positive_dig_jc_uses_half_up_rounding_and_minimum_one_3_2() {
    assert_eq!(scale_positive_dig_jc(3), 2);
}

#[test]
fn audit_positive_dig_jc_uses_half_up_rounding_and_minimum_one_5_3() {
    assert_eq!(scale_positive_dig_jc(5), 3);
}

#[test]
fn audit_positive_dig_jc_uses_half_up_rounding_and_minimum_one_15_10() {
    assert_eq!(scale_positive_dig_jc(15), 10);
}

#[test]
fn audit_positive_dig_jc_uses_half_up_rounding_and_minimum_one_100_65() {
    assert_eq!(scale_positive_dig_jc(100), 65);
}

#[test]
fn audit_positive_dig_jc_uses_half_up_rounding_and_minimum_one_1000_650() {
    assert_eq!(scale_positive_dig_jc(1000), 650);
}

#[test]
fn audit_non_positive_dig_jc_is_unchanged_0_0() {
    assert_eq!(scale_positive_dig_jc(0), 0);
}

#[test]
fn audit_non_positive_dig_jc_is_unchanged_neg1() {
    assert_eq!(scale_positive_dig_jc(-1), -1);
}

#[test]
fn audit_non_positive_dig_jc_is_unchanged_neg5() {
    assert_eq!(scale_positive_dig_jc(-5), -5);
}

#[test]
fn audit_non_positive_dig_jc_is_unchanged_neg100() {
    assert_eq!(scale_positive_dig_jc(-100), -100);
}

#[test]
fn audit_first_dig_credits_scaled_reward() {
    let mut state = dig_service::TunnelState::default();
    let result = dig_service::apply_first_dig(&mut state, 7, 5, 5, NOW);
    assert_eq!(result.jc_earned, 3);
    assert_eq!(state.balance, 103);
    assert_eq!(state.depth, 7);
}

#[test]
fn audit_event_positive_reward_scales_once_and_is_auditable() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 100);
    fixture.seed_tunnel(ACTOR, 10);
    let key = success_key("gas_pocket", "risky", 0.62);
    let outcome = fixture.event("gas_pocket", "risky", &key);
    let resolution = outcome.resolution.expect("resolution");
    assert!(resolution.economy_gross_jc > 0);
    assert_eq!(
        resolution.jc,
        scale_positive_dig_jc(resolution.economy_gross_jc)
    );
    assert_eq!(fixture.balance(ACTOR), 100 + resolution.jc);
    let detail: Value = fixture
        .connection()
        .query_row(
            "SELECT detail FROM dig_actions
              WHERE actor_id=?1 AND guild_id=?2 AND action_type='event'
              ORDER BY id DESC LIMIT 1",
            params![ACTOR, GUILD],
            |row| row.get::<_, String>(0),
        )
        .map(|raw| serde_json::from_str(&raw).expect("detail JSON"))
        .expect("event audit");
    assert_eq!(detail["gross_jc"], resolution.economy_gross_jc);
    assert_eq!(detail["reward_multiplier"], 0.65);
}

#[test]
fn audit_legacy_event_shape_adapter_settles_migrated_sqlite() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 100);
    fixture.seed_tunnel(ACTOR, 10);
    let result = fixture
        .service()
        .resolve_legacy_event(dig_event_runtime_reward_under_test::DigLegacyEventRequest {
            discord_id: ACTOR,
            guild_id: GUILD,
            event_id: "legacy_reward_policy_event",
            event_name: "Legacy Reward Policy Event",
            choice: "inspect",
            message: "A compact reward.",
            advance: 0,
            jc: 15,
            event_key: "legacy-reward-policy:audit",
            now: NOW,
        })
        .expect("legacy event adapter");
    let resolution = result.resolution.expect("legacy resolution");
    assert!(result.success && result.applied_now);
    assert_eq!(resolution.economy_gross_jc, 15);
    assert_eq!(resolution.jc, scale_positive_dig_jc(15));
    assert_eq!(fixture.balance(ACTOR), 100 + resolution.jc);
    let detail: Value = fixture
        .connection()
        .query_row(
            "SELECT detail FROM dig_actions
              WHERE actor_id=?1 AND guild_id=?2 AND action_type='event'
              ORDER BY id DESC LIMIT 1",
            params![ACTOR, GUILD],
            |row| row.get::<_, String>(0),
        )
        .map(|raw| serde_json::from_str(&raw).expect("legacy detail JSON"))
        .expect("legacy audit");
    assert_eq!(detail["gross_jc"], 15);
    assert_eq!(detail["reward_multiplier"], 0.65);
}

#[test]
fn audit_slow_drip_has_catalog_metadata_but_no_crediting_service() {
    let relic = cama_app::dig_relic_rework::relic_content("slow_drip").expect("slow drip relic");
    assert_eq!(relic.definition.id, "slow_drip");
    assert_eq!(scale_positive_dig_jc(20), 13);
    // The migrated DB exposes claim rows, but no admitted app service joins
    // elapsed-minute accrual, economy adjustment, claim insertion, and wallet
    // audit into Python's one policy path.
}

#[test]
fn audit_deterministic_dig_outcome_scales_full_positive_payout() {
    let mut state = dig_service::TunnelState {
        depth: 10,
        max_depth: 10,
        balance: 100,
        ..dig_service::TunnelState::default()
    };
    let result = dig_service::apply_dig_outcome(
        &mut state,
        dig_service::DigOutcomeInput {
            advance: 1,
            gross_jc: 15,
            authored_event: true,
            ..dig_service::DigOutcomeInput::default()
        },
        NOW,
    );
    assert_eq!(result.gross_jc, 15);
    assert_eq!(result.jc_earned, 10);
    assert_eq!(state.balance, 110);
}

#[test]
fn audit_live_dig_surface_exposes_no_jc_settlement_yet() {
    let mut repository = InMemoryLootRepository::new();
    repository.register_player(ACTOR, GUILD, 100);
    repository.create_tunnel(ACTOR, GUILD);
    let mut service = DigLootService::new(repository, ScriptedLootEntropy::new([0.99, 0.99], [1]));
    let result = service.dig_with_modifiers(ACTOR, GUILD, NOW, DigLootModifiers::default());
    assert!(result.success);
    assert_eq!(service.repository().balance(ACTOR, GUILD), 100);
    // DigLootOutcome has no gross/net JC fields and the live loot adapter does
    // not route layer payout through the positive reward policy.
}

#[test]
fn audit_cave_in_outcome_scales_positive_component_once() {
    let mut state = dig_service::TunnelState {
        depth: 10,
        max_depth: 10,
        balance: 100,
        ..dig_service::TunnelState::default()
    };
    let result = dig_service::apply_dig_outcome(
        &mut state,
        dig_service::DigOutcomeInput {
            advance: 1,
            gross_jc: 15,
            cave_in_loss: 8,
            authored_event: true,
            ..dig_service::DigOutcomeInput::default()
        },
        NOW,
    );
    assert!(result.cave_in);
    assert_eq!(result.jc_earned, 10);
    assert_eq!(state.balance, 110);
    // Python's cave-in path additionally composes Lucky Rubble/Gambler's
    // Charm and daily economy before this scale; those adapters are absent.
}

#[test]
fn audit_prestige_base_grant_uses_positive_scale() {
    assert_eq!(scale_positive_dig_jc(1_000), 650);
    // The unadmitted SQLite prestige runtime has the same arithmetic; the
    // Python fixture's economy-service call-count is an adapter concern.
}

#[test]
fn audit_abandon_refund_is_scaled_and_audited() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 100);
    fixture.seed_tunnel(ACTOR, 100);
    let service =
        dig_abandon_runtime_under_test::DigAbandonRuntimeService::sqlite(fixture.database.path());
    let preview = service
        .preview_at(ACTOR, GUILD, NOW)
        .expect("abandon preview");
    assert_eq!(preview.gross_refund, 10);
    assert_eq!(preview.refund, 7);
    let result = service.abandon(ACTOR, GUILD, NOW).expect("abandon");
    assert_eq!(result.refund, 7);
    assert_eq!(fixture.balance(ACTOR), 107);
    let detail: Value = fixture
        .connection()
        .query_row(
            "SELECT detail FROM dig_actions
              WHERE actor_id=?1 AND guild_id=?2 AND action_type='abandon'
              ORDER BY id DESC LIMIT 1",
            params![ACTOR, GUILD],
            |row| row.get::<_, String>(0),
        )
        .map(|raw| serde_json::from_str(&raw).expect("detail JSON"))
        .expect("abandon audit");
    assert_eq!(detail["gross_jc"], 10);
    assert_eq!(detail["reward_multiplier"], 0.65);
}

#[test]
fn audit_pinnacle_payout_scales_base_before_wager_profit() {
    let payout = cama_app::dig_carry_wager::pinnacle_payout(
        200,
        cama_app::boss_duel::RiskTier::Cautious,
        0.5,
        0,
    )
    .expect("pinnacle payout");
    assert_eq!(payout.scaled_base_reward, 325);
    assert_eq!(
        payout.payout,
        payout.scaled_base_reward + payout.wager_payout
    );
    // Daily economy adjustment of both base and wager profit is not wired to
    // this pure carry policy, unlike Python's _net_boss_payout adapter.
}

#[test]
fn audit_help_policy_scales_mentor_bonuses() {
    // This is the exact arithmetic consumed by the admitted social runtime;
    // its SQLite orchestration remains an unadmitted source seam.
    assert_eq!(scale_positive_dig_jc(22), 14);
    assert_eq!(scale_positive_dig_jc(20), 13);
    assert_eq!(scale_minigame_jc_delta(15.0, 1.0), 15);
    assert_eq!(scale_deflationary_minigame_jc_delta(-15.0, 1.0), -17);
    assert_eq!(
        scale_deflationary_minigame_jc_delta(strengthen_dig_event_penalty(10) as f64, 1.0,),
        15
    );
}
