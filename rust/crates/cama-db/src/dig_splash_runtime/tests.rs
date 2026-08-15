use cama_domain::dig_splash::{HOSTILE_LOSS_MIN_BALANCE, strengthen_dig_event_penalty};
use cama_domain::economy_scaling::scale_deflationary_minigame_jc_delta;
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::schema_manager::initialize_or_migrate;

const GUILD: i64 = 6_201;
const ACTOR: i64 = 6_202;
const NOW: i64 = 2_000_000_000;

struct Fixture {
    database: NamedTempFile,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary splash database");
        initialize_or_migrate(database.path()).expect("canonical schema");
        Self { database }
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(self.database.path()).expect("fixture connection");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("Python-compatible foreign-key mode");
        connection
    }

    fn player(&self, id: i64, balance: i64, recent: bool) {
        self.connection()
            .execute(
                "INSERT INTO players(
                     discord_id,guild_id,discord_username,jopacoin_balance,last_match_date
                 ) VALUES(?1,?2,?3,?4,CASE WHEN ?5 THEN '2033-05-18T03:33:20+00:00' ELSE NULL END)",
                params![id, GUILD, format!("db-splash-{id}"), balance, recent],
            )
            .expect("seed player");
    }
}

#[test]
fn typed_strategy_and_mode_parsers_fail_closed() {
    assert_eq!(
        SplashStrategy::parse("random_active"),
        Some(SplashStrategy::RandomActive)
    );
    assert_eq!(SplashStrategy::parse("unknown"), None);
    assert_eq!(SplashMode::parse("burn"), Some(SplashMode::Burn));
    assert_eq!(SplashMode::parse("unknown"), None);
}

#[test]
fn candidate_projection_excludes_actor_and_orders_richest() {
    let fixture = Fixture::new();
    fixture.player(ACTOR, 900, false);
    fixture.player(6_203, 300, false);
    fixture.player(6_204, 800, false);
    let repository = DigSplashRuntimeRepository::new(fixture.database.path());
    let ids = repository
        .candidate_ids(SplashCandidateQuery {
            guild_id: GUILD,
            digger_id: ACTOR,
            strategy: SplashStrategy::RichestN,
            victim_count: 2,
            active_since: 0,
        })
        .expect("candidate projection");
    assert_eq!(ids, vec![6_204, 6_203]);
    assert!(!ids.contains(&ACTOR));
}

#[test]
fn immediate_batch_is_idempotent_and_preserves_ledger_context() {
    let fixture = Fixture::new();
    fixture.player(ACTOR, 100, true);
    fixture.player(6_203, 100, true);
    let repository = DigSplashRuntimeRepository::new(fixture.database.path());
    let ids = vec![6_203];
    let amount = scale_deflationary_minigame_jc_delta(strengthen_dig_event_penalty(10) as f64, 1.0);
    let request = SplashSettlementRequest {
        guild_id: GUILD,
        digger_id: ACTOR,
        event_name: "DB splash",
        strategy: SplashStrategy::RandomActive,
        mode: SplashMode::Burn,
        amount,
        victim_ids: &ids,
        event_key_prefix: "db:splash",
        now: NOW,
        mana_date: "2033-05-17",
    };
    let first = repository.settle_batch(&request).expect("first settlement");
    let second = repository
        .settle_batch(&request)
        .expect("idempotent settlement");
    assert_eq!(first.total_moved, amount);
    assert_eq!(second.total_moved, amount);
    assert!(!second.applied_now);
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![6_203, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        100 - amount
    );
    assert_eq!(
        fixture
            .connection()
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    assert_eq!(HOSTILE_LOSS_MIN_BALANCE, 50);
}
