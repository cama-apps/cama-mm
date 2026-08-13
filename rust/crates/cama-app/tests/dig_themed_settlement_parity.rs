//! Existing-schema end-to-end checks for themed social Dig events.
//!
//! The canonical event resolver owns the authored event values and policy
//! scalars.  The existing-schema repositories then own the wallet movement:
//! tunnel encounters settle burns in one transaction, while the dedicated
//! cross-player repository settles each steal pair atomically.  Keeping the
//! two paths explicit matters because a steal is zero-sum and a burn is not.

use cama_app::dig_loot::{
    CanonicalEventPolicy, CanonicalEventResolution, CanonicalEventRolls,
    resolve_canonical_event_with_policy,
};
use cama_db::dig_new_events_repository::{
    DigNewEventsRepository, StealSettlement, StealSettlementRequest,
};
use cama_db::dig_tunnel_encounters_repository::{
    DigTunnelEncounterRepository, EncounterCandidateQuery, EncounterOutcome, EncounterSettlement,
    EncounterSettlementRequest, EncounterVictimStrategy,
};
use cama_db::schema_manager;
use cama_domain::dig_splash::{HOSTILE_LOSS_MIN_BALANCE, strengthen_dig_event_penalty};
use cama_domain::economy_scaling::{
    DEFAULT_MINIGAME_JC_DELTA_SCALE, scale_deflationary_minigame_jc_delta, scale_minigame_jc_delta,
};
use rusqlite::{Connection, TransactionBehavior, params};
use tempfile::NamedTempFile;

const ACTOR: i64 = 10_001;
const GUILD: i64 = 12_345;
const NOW: i64 = 2_000_000_000;

struct Fixture {
    database: NamedTempFile,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary SQLite database");
        schema_manager::initialize_or_migrate(database.path()).expect("canonical schema");
        Self { database }
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(self.database.path()).expect("fixture connection");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("Python-compatible foreign key mode");
        connection
    }

    fn seed_player(&self, discord_id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username, jopacoin_balance
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![discord_id, GUILD, format!("themed-{discord_id}"), balance],
            )
            .expect("seed player");
    }

    fn seed_tunnel(&self, discord_id: i64, depth: i64) {
        self.connection()
            .execute(
                "INSERT INTO tunnels (
                     discord_id, guild_id, depth, max_depth, luminosity,
                     prestige_level, prestige_perks, boss_progress, streak_days
                 ) VALUES (?1, ?2, ?3, ?3, 100, 0, '[]', '{}', 0)",
                params![discord_id, GUILD, depth],
            )
            .expect("seed tunnel");
    }

    fn balance(&self, discord_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                  WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, GUILD],
                |row| row.get(0),
            )
            .expect("balance")
    }

    fn credit_actor_event(&self, resolution: &CanonicalEventResolution, event_key: &str) {
        let detail = format!(
            "dig-encounter:{}:{}:{}",
            resolution.event_id, event_key, resolution.event_name
        );
        let mut connection = self.connection();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("actor event transaction");
        transaction
            .execute(
                "INSERT OR REPLACE INTO economy_ledger_context (
                     id, source, actor_id, related_type, related_id, reason, metadata
                 ) VALUES (1, 'dig', ?1, 'event', ?2, 'dig event result', '{}')",
                params![ACTOR, resolution.event_id],
            )
            .expect("event ledger context");
        transaction
            .execute(
                "UPDATE players
                    SET jopacoin_balance = jopacoin_balance + ?1,
                        updated_at = CURRENT_TIMESTAMP
                  WHERE discord_id = ?2 AND guild_id = ?3",
                params![resolution.jc, ACTOR, GUILD],
            )
            .expect("credit actor event");
        transaction
            .execute(
                "INSERT INTO dig_actions (
                     guild_id, actor_id, target_id, action_type,
                     depth_before, depth_after, jc_delta, detail, created_at
                 ) VALUES (?1, ?2, NULL, 'event', 0, 0, ?3, ?4, ?5)",
                params![GUILD, ACTOR, resolution.jc, detail, NOW],
            )
            .expect("event audit");
        transaction
            .execute("DELETE FROM economy_ledger_context WHERE id = 1", [])
            .expect("clear event ledger context");
        transaction.commit().expect("commit actor event");
    }

    fn tunnel_repository(&self) -> DigTunnelEncounterRepository {
        DigTunnelEncounterRepository::new(self.database.path())
    }

    fn steal_repository(&self) -> DigNewEventsRepository {
        DigNewEventsRepository::new(self.database.path())
    }
}

fn risky_success(event_id: &str, depth: i64) -> CanonicalEventResolution {
    let resolution = resolve_canonical_event_with_policy(
        event_id,
        "risky",
        CanonicalEventRolls {
            success_roll: 0.0,
            jc_jitter_multiplier: 1.0,
            ..CanonicalEventRolls::default()
        },
        CanonicalEventPolicy {
            current_depth: depth,
            ..CanonicalEventPolicy::default()
        },
    )
    .expect("canonical themed event resolution");
    assert!(resolution.succeeded, "{event_id} should take risky success");
    assert!(
        resolution.jc > 0,
        "{event_id} should pay a positive finder's share"
    );
    assert_eq!(
        resolution
            .splash
            .as_ref()
            .map(|splash| splash.strategy.as_str()),
        Some("richest_n")
    );
    resolution
}

fn settle_burn(
    fixture: &Fixture,
    resolution: &CanonicalEventResolution,
    event_key: &str,
) -> EncounterSettlement {
    let splash = resolution.splash.as_ref().expect("themed splash");
    assert_eq!(splash.mode, "burn");
    let settlement = fixture
        .tunnel_repository()
        .settle_encounter_atomic(&EncounterSettlementRequest {
            digger_discord_id: ACTOR,
            guild_id: Some(GUILD),
            event_id: resolution.event_id.clone(),
            event_name: resolution.event_name.clone(),
            event_key: event_key.to_owned(),
            outcome: EncounterOutcome::Success {
                strategy: EncounterVictimStrategy::RichestN,
                victim_count: splash.victim_count,
                authored_penalty_jc: splash.penalty_jc,
                jittered_authored_reward_jc: resolution.economy_gross_jc,
                preferred_victim_ids: Vec::new(),
            },
            minigame_jc_delta_scale: DEFAULT_MINIGAME_JC_DELTA_SCALE,
            active_since: 0,
            now: NOW,
        })
        .expect("atomic themed burn settlement");
    assert!(settlement.succeeded);
    assert!(settlement.applied_now);
    assert_eq!(settlement.actor_delta, resolution.jc);
    settlement
}

fn expected_burn(authored_penalty: i64) -> i64 {
    scale_deflationary_minigame_jc_delta(
        strengthen_dig_event_penalty(authored_penalty) as f64,
        DEFAULT_MINIGAME_JC_DELTA_SCALE,
    )
}

fn richest_ids(fixture: &Fixture, limit: usize) -> Vec<i64> {
    fixture
        .tunnel_repository()
        .candidate_ids(EncounterCandidateQuery {
            guild_id: Some(GUILD),
            digger_discord_id: ACTOR,
            strategy: EncounterVictimStrategy::RichestN,
            active_since: 0,
            limit: Some(limit),
        })
        .expect("richest candidates")
}

// tests/test_dig_themed_ev_events.py::test_rhystic_tollgate_steals_from_the_richest
#[test]
fn dig_themed_rhystic_tollgate_transfers_scaled_pot_from_three_richest() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 500);
    fixture.seed_tunnel(ACTOR, 60);
    for (discord_id, balance) in [
        (10_002, 1_300),
        (10_003, 1_200),
        (10_004, 1_100),
        (10_005, 900),
    ] {
        fixture.seed_player(discord_id, balance);
    }

    let resolution = risky_success("rhystic_tollgate", 60);
    let splash = resolution.splash.as_ref().expect("steal splash");
    assert_eq!(splash.mode, "steal");
    assert_eq!(splash.penalty_jc, 12);
    let actor_before = fixture.balance(ACTOR);
    let victim_before =
        [10_002, 10_003, 10_004, 10_005].map(|discord_id| fixture.balance(discord_id));
    let victims = richest_ids(&fixture, splash.victim_count);
    assert_eq!(victims, vec![10_002, 10_003, 10_004]);

    // Event payout and each existing-schema transfer are deliberately
    // separate commits, matching Python's event/splash failure boundary.
    fixture.credit_actor_event(&resolution, "rhystic-themed");
    let amount = scale_minigame_jc_delta(splash.penalty_jc as f64, 1.0);
    let mut moved = 0;
    for (index, victim_id) in victims.iter().copied().enumerate() {
        let result = fixture
            .steal_repository()
            .settle_steal_atomic(StealSettlementRequest {
                thief_discord_id: ACTOR,
                victim_discord_id: victim_id,
                guild_id: Some(GUILD),
                amount,
                minimum_victim_balance: HOSTILE_LOSS_MIN_BALANCE,
                event_name: &resolution.event_name,
                event_key: &format!("rhystic-themed-{index}"),
                detail: "rhystic tollgate richest transfer",
                now: NOW,
            })
            .expect("atomic steal settlement");
        assert!(matches!(
            result,
            StealSettlement::Applied {
                applied_now: true,
                ..
            }
        ));
        moved += amount;
    }

    assert_eq!(moved, splash.victim_count as i64 * amount);
    for (index, victim_id) in victims.iter().copied().enumerate() {
        assert_eq!(
            fixture.balance(victim_id),
            victim_before[index] - amount,
            "victim {victim_id} loses the scaled toll"
        );
    }
    assert_eq!(fixture.balance(10_005), victim_before[3]);
    assert_eq!(
        fixture.balance(ACTOR) - actor_before,
        resolution.jc + moved,
        "actor receives the finder's share plus the transferred pot"
    );

    let actions = fixture
        .steal_repository()
        .splash_actions(Some(GUILD))
        .expect("steal audit actions");
    let victim_actions = actions
        .iter()
        .filter(|action| action.action_type == "splash_victim")
        .collect::<Vec<_>>();
    let thief_actions = actions
        .iter()
        .filter(|action| action.action_type == "splash_thief")
        .collect::<Vec<_>>();
    assert_eq!(
        victim_actions
            .iter()
            .map(|action| action.actor_id)
            .collect::<Vec<_>>(),
        victims
    );
    assert_eq!(
        victim_actions
            .iter()
            .map(|action| -action.jc_delta)
            .sum::<i64>(),
        moved
    );
    assert_eq!(
        thief_actions
            .iter()
            .map(|action| action.jc_delta)
            .sum::<i64>(),
        moved
    );
}

// tests/test_dig_themed_ev_events.py::test_underworld_reclaims_burns_the_richest
#[test]
fn dig_themed_underworld_reclaims_burns_three_richest_and_stays_deflationary() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 500);
    fixture.seed_tunnel(ACTOR, 80);
    for (discord_id, balance) in [
        (10_002, 1_300),
        (10_003, 1_200),
        (10_004, 1_100),
        (10_005, 900),
    ] {
        fixture.seed_player(discord_id, balance);
    }

    let resolution = risky_success("underworld_reclaims", 80);
    let splash = resolution.splash.as_ref().expect("burn splash");
    let victims = richest_ids(&fixture, splash.victim_count);
    assert_eq!(victims, vec![10_002, 10_003, 10_004]);
    let before = victims
        .iter()
        .map(|victim_id| fixture.balance(*victim_id))
        .collect::<Vec<_>>();
    let actor_before = fixture.balance(ACTOR);
    let settlement = settle_burn(&fixture, &resolution, "underworld-themed");
    let burn = expected_burn(splash.penalty_jc);
    let total_burned = settlement.splash.expect("burn result").total_burned;

    assert_eq!(total_burned, splash.victim_count as i64 * burn);
    for (victim_id, balance_before) in victims.iter().copied().zip(before) {
        assert_eq!(fixture.balance(victim_id), balance_before - burn);
    }
    assert_eq!(fixture.balance(10_005), 900);
    assert_eq!(fixture.balance(ACTOR) - actor_before, resolution.jc);
    assert!(total_burned > resolution.jc);
}

// tests/test_dig_themed_ev_events.py::test_ember_clearinghouse_burns_only_the_three_richest
#[test]
fn dig_themed_ember_clearinghouse_burns_only_three_richest() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 500);
    fixture.seed_tunnel(ACTOR, 80);
    for (discord_id, balance) in [
        (10_002, 1_300),
        (10_003, 1_200),
        (10_004, 1_100),
        (10_005, 1_000),
    ] {
        fixture.seed_player(discord_id, balance);
    }

    let resolution = risky_success("ember_clearinghouse", 80);
    let splash = resolution.splash.as_ref().expect("burn splash");
    let victims = richest_ids(&fixture, splash.victim_count);
    assert_eq!(victims, vec![10_002, 10_003, 10_004]);
    let before = victims
        .iter()
        .map(|victim_id| fixture.balance(*victim_id))
        .collect::<Vec<_>>();
    let actor_before = fixture.balance(ACTOR);
    let settlement = settle_burn(&fixture, &resolution, "ember-themed");
    let burn = expected_burn(splash.penalty_jc);
    let total_burned = settlement.splash.expect("burn result").total_burned;

    assert_eq!(total_burned, splash.victim_count as i64 * burn);
    for (victim_id, balance_before) in victims.iter().copied().zip(before) {
        assert_eq!(fixture.balance(victim_id), balance_before - burn);
    }
    assert_eq!(fixture.balance(10_005), 1_000);
    assert_eq!(fixture.balance(ACTOR) - actor_before, resolution.jc);
    assert!(total_burned > resolution.jc);
}

// tests/test_dig_event_balance.py::test_social_burn_events_are_real_global_sinks
#[test]
fn dig_event_balance_hungering_dark_is_a_real_global_sink() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 10_000);
    fixture.seed_tunnel(ACTOR, 120);
    for discord_id in [10_002, 10_003, 10_004] {
        fixture.seed_player(discord_id, 1_000);
    }

    let resolution = risky_success("hungering_dark", 120);
    let splash = resolution.splash.as_ref().expect("burn splash");
    let victims = richest_ids(&fixture, splash.victim_count);
    assert_eq!(victims, vec![10_002, 10_003]);
    let before = victims
        .iter()
        .map(|victim_id| fixture.balance(*victim_id))
        .collect::<Vec<_>>();
    let actor_before = fixture.balance(ACTOR);
    let settlement = settle_burn(&fixture, &resolution, "hungering-themed");
    let total_burned = settlement.splash.expect("burn result").total_burned;
    let burned_from_balances = victims
        .iter()
        .copied()
        .zip(before)
        .map(|(victim_id, balance_before)| balance_before - fixture.balance(victim_id))
        .sum::<i64>();

    assert_eq!(splash.mode, "burn");
    assert_eq!(total_burned, burned_from_balances);
    assert_eq!(fixture.balance(10_004), 1_000);
    assert!(burned_from_balances > fixture.balance(ACTOR) - actor_before);
    assert!(fixture.balance(ACTOR) - actor_before > 0);
}
