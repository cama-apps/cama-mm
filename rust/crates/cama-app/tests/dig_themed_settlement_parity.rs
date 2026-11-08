//! Existing-schema end-to-end checks for themed social Dig events.
//!
//! The canonical event resolver owns the authored event values and policy
//! scalars, and the existing-schema tunnel-encounter repository selects the
//! richest splash targets. The live event runtime tests cover settlement.

use cama_app::dig_loot::{
    CanonicalEventPolicy, CanonicalEventResolution, CanonicalEventRolls,
    resolve_canonical_event_with_policy,
};
use cama_db::dig_tunnel_encounters_repository::{
    DigTunnelEncounterRepository, EncounterCandidateQuery, EncounterVictimStrategy,
};
use cama_db::schema_manager;
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

const ACTOR: i64 = 10_001;
const GUILD: i64 = 12_345;

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

    fn tunnel_repository(&self) -> DigTunnelEncounterRepository {
        DigTunnelEncounterRepository::new(self.database.path())
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
fn dig_themed_rhystic_tollgate_targets_the_three_richest() {
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
    let victims = richest_ids(&fixture, splash.victim_count);
    assert_eq!(victims, vec![10_002, 10_003, 10_004]);
}
