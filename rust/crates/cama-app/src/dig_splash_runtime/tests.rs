use cama_db::schema_manager::initialize_or_migrate;
use cama_domain::dig_splash::{HOSTILE_LOSS_MIN_BALANCE, strengthen_dig_event_penalty};
use cama_domain::economy_scaling::{scale_deflationary_minigame_jc_delta, scale_minigame_jc_delta};
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const ACTOR: i64 = 10_001;
const GUILD: i64 = 12_345;
const NOW: i64 = 2_000_000_000;
const RECENT_ISO: &str = "2033-05-18T03:33:20+00:00";

struct Fixture {
    database: NamedTempFile,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary SQLite database");
        initialize_or_migrate(database.path()).expect("canonical migrated schema");
        Self { database }
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(self.database.path()).expect("fixture connection");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("Python-compatible foreign-key mode");
        connection
    }

    fn seed_player(&self, discord_id: i64, balance: i64, recent: bool) {
        self.connection()
            .execute(
                "INSERT INTO players(
                     discord_id,guild_id,discord_username,jopacoin_balance,last_match_date
                 ) VALUES(?1,?2,?3,?4,?5)",
                params![
                    discord_id,
                    GUILD,
                    format!("splash-{discord_id}"),
                    balance,
                    recent.then_some(RECENT_ISO),
                ],
            )
            .expect("seed player");
    }

    fn seed_tunnel(&self, discord_id: i64, depth: i64) {
        self.connection()
            .execute(
                "INSERT INTO tunnels(discord_id,guild_id,depth,max_depth,luminosity)
                 VALUES(?1,?2,?3,?3,100)",
                params![discord_id, GUILD, depth],
            )
            .expect("seed tunnel");
    }

    fn mark_dig(&self, discord_id: i64, created_at: i64) {
        self.connection()
            .execute(
                "INSERT INTO dig_actions(
                     guild_id,actor_id,target_id,action_type,depth_before,depth_after,
                     jc_delta,detail,created_at
                 ) VALUES(?1,?2,NULL,'dig',1,2,0,'{}',?3)",
                params![GUILD, discord_id, created_at],
            )
            .expect("seed dig action");
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

    fn service(&self) -> DigSplashRuntimeService {
        DigSplashRuntimeService::sqlite(self.database.path())
    }

    fn request<'a>(
        &self,
        event_name: &'a str,
        strategy: &'a str,
        victim_count: usize,
        penalty_jc: i64,
        mode: &'a str,
        key: &'a str,
    ) -> DigSplashRequest<'a> {
        DigSplashRequest::from_spec(DigSplashRequestSpec {
            guild_id: GUILD,
            digger_id: ACTOR,
            event_name,
            strategy,
            victim_count,
            penalty_jc,
            mode,
            event_key_prefix: key,
            now: NOW,
        })
    }

    fn resolve<'a>(
        &self,
        event_name: &'a str,
        strategy: &'a str,
        victim_count: usize,
        penalty_jc: i64,
        mode: &'a str,
        key: &'a str,
    ) -> DigSplashResult {
        self.service()
            .resolve(self.request(event_name, strategy, victim_count, penalty_jc, mode, key))
            .expect("splash resolution")
    }
}

#[test]
fn test_random_active_excludes_digger() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 100, true);
    fixture.seed_player(10_002, 100, true);
    fixture.seed_player(10_003, 100, true);
    let result = fixture.resolve("Test Event", "random_active", 2, 5, "burn", "splash:random");
    assert_eq!(result.victims.len(), 2);
    assert!(result.victims.iter().all(|(id, _)| *id != ACTOR));
}

#[test]
fn test_richest_n_excludes_digger_and_sorts() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 1_000, false);
    fixture.seed_player(10_002, 500, false);
    fixture.seed_player(10_003, 800, false);
    let result = fixture.resolve("Test Event", "richest_n", 2, 10, "burn", "splash:richest");
    assert_eq!(
        result.victims.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![10_003, 10_002]
    );
}

#[test]
fn test_active_diggers_only_includes_recent_diggers() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 100, false);
    fixture.seed_player(10_002, 100, false);
    fixture.seed_player(10_003, 100, false);
    fixture.mark_dig(10_002, NOW - 30);
    fixture.mark_dig(10_003, NOW - 8 * 86_400);
    let result = fixture.resolve(
        "Test Event",
        "active_diggers",
        3,
        5,
        "burn",
        "splash:active",
    );
    assert_eq!(
        result.victims.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![10_002]
    );
}

#[test]
fn test_empty_pool_returns_empty_result() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 100, true);
    let result = fixture.resolve("Test Event", "random_active", 3, 5, "burn", "splash:empty");
    assert!(result.victims.is_empty());
    assert_eq!(result.total_burned, 0);
}

#[test]
fn test_unknown_strategy_returns_empty_result() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 100, true);
    fixture.seed_player(10_002, 100, true);
    let result = fixture.resolve(
        "Test Event",
        "not_a_real_strategy",
        3,
        5,
        "burn",
        "splash:unknown",
    );
    assert!(result.victims.is_empty());
    assert_eq!(result.mode, "burn");
}

#[test]
fn test_burns_exactly_penalty_per_victim() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 500, true);
    fixture.seed_player(10_002, 500, true);
    fixture.seed_player(10_003, 500, true);
    let before = fixture.balance(10_002) + fixture.balance(10_003);
    let result = fixture.resolve("Burn Test", "random_active", 2, 20, "burn", "splash:burn");
    let expected =
        scale_deflationary_minigame_jc_delta(strengthen_dig_event_penalty(20) as f64, 1.0);
    assert_eq!(result.total_burned, expected * 2);
    assert_eq!(
        before - fixture.balance(10_002) - fixture.balance(10_003),
        expected * 2
    );
    assert_eq!(fixture.balance(ACTOR), 500);
}

#[test]
fn test_skips_players_below_auto_blind_threshold() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 500, true);
    fixture.seed_player(10_002, 3, true);
    let result = fixture.resolve(
        "Clamp Test",
        "random_active",
        1,
        100,
        "burn",
        "splash:clamp",
    );
    assert!(result.victims.is_empty());
    assert_eq!(fixture.balance(10_002), 3);
}

#[test]
fn test_skips_debtors() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 500, true);
    fixture.seed_player(10_002, -50, true);
    fixture.seed_player(10_003, 100, true);
    let result = fixture.resolve(
        "Debtor Test",
        "random_active",
        2,
        10,
        "burn",
        "splash:debtor",
    );
    assert_eq!(
        result.victims.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![10_003]
    );
    assert_eq!(fixture.balance(10_002), -50);
}

#[test]
fn test_burn_routes_through_white_protection_gateway() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 500, true);
    fixture.seed_player(10_002, 500, true);
    let mana_date = cama_domain::game_date::game_date_for_timestamp(NOW as f64).unwrap();
    fixture
        .connection()
        .execute(
            "INSERT INTO player_mana(
                 discord_id,guild_id,current_land,assigned_date,consumed_today,
                 white_shield_remaining
             ) VALUES(?1,?2,'Plains',?3,0,10)",
            params![10_002, GUILD, mana_date],
        )
        .expect("seed White protection");
    let result = fixture.resolve(
        "Shield Test",
        "random_active",
        1,
        20,
        "burn",
        "splash:white",
    );
    let expected =
        scale_deflationary_minigame_jc_delta(strengthen_dig_event_penalty(20) as f64, 1.0);
    assert_eq!(result.victims, vec![(10_002, expected - 10)]);
    assert_eq!(result.absorbed_total, 10);
    assert_eq!(result.shielded_count, 1);
    assert_eq!(fixture.balance(10_002), 500 - expected + 10);
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM mana_protection_events WHERE victim_id=?1",
                [10_002],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn test_real_protection_batches_multi_victim_burns_on_one_connection() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 500, true);
    for id in [10_002, 10_003, 10_004] {
        fixture.seed_player(id, 500, true);
    }
    let result = fixture.resolve(
        "Batched Burn",
        "random_active",
        3,
        10,
        "burn",
        "splash:batch",
    );
    let expected =
        scale_deflationary_minigame_jc_delta(strengthen_dig_event_penalty(10) as f64, 1.0);
    assert_eq!(result.victims.len(), 3);
    assert_eq!(result.total_burned, expected * 3);
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM hostile_loss_events WHERE kind='dig_splash_burn'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        3
    );
}

#[test]
fn test_lookback_excludes_old_digs() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 100, false);
    fixture.seed_player(10_002, 100, false);
    fixture.mark_dig(10_002, NOW - 8 * 86_400);
    let result = fixture.resolve("Stale Dig", "active_diggers", 3, 5, "burn", "splash:stale");
    assert!(result.victims.is_empty());
}

#[test]
fn test_grant_credits_recipient() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 100, true);
    fixture.seed_player(10_002, 100, true);
    let mut request = fixture.request(
        "Wisp Tether",
        "random_active",
        1,
        10,
        "grant",
        "splash:grant",
    );
    request.grant_reward_multiplier = 2.0;
    let result = fixture
        .service()
        .resolve(request)
        .expect("grant resolution");
    let expected = scale_positive_dig_jc(20);
    assert_eq!(result.victims, vec![(10_002, expected)]);
    assert_eq!(fixture.balance(10_002), 100 + expected);
    assert_eq!(fixture.balance(ACTOR), 100);
}

#[test]
fn test_grant_ignores_zero_balance_guard() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 100, true);
    fixture.seed_player(10_002, -25, true);
    let result = fixture.resolve(
        "Wisp Tether",
        "random_active",
        1,
        10,
        "grant",
        "splash:grant-debt",
    );
    let expected = scale_positive_dig_jc(scale_minigame_jc_delta(10.0, 1.0));
    assert_eq!(result.victims, vec![(10_002, expected)]);
    assert_eq!(fixture.balance(10_002), -25 + expected);
}

#[test]
fn test_steal_does_not_strengthen_authored_amount() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 100, true);
    fixture.seed_player(10_002, 100, true);
    let result = fixture.resolve(
        "Steal Test",
        "random_active",
        1,
        10,
        "steal",
        "splash:steal",
    );
    let expected = scale_minigame_jc_delta(10.0, 1.0);
    assert_eq!(result.victims, vec![(10_002, expected)]);
    assert_eq!(fixture.balance(ACTOR), 100 + expected);
    assert_eq!(fixture.balance(10_002), 100 - expected);
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM dig_actions WHERE action_type='splash_thief'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn test_deepest_n_targets_deepest_excluding_digger() {
    let fixture = Fixture::new();
    fixture.seed_player(ACTOR, 100, false);
    fixture.seed_tunnel(ACTOR, 500);
    for (id, depth) in [(10_002, 200), (10_003, 150), (10_004, 50)] {
        fixture.seed_player(id, 100, false);
        fixture.seed_tunnel(id, depth);
    }
    let result = fixture.resolve(
        "The Deep Hunter",
        "deepest_n",
        2,
        10,
        "burn",
        "splash:deepest",
    );
    assert_eq!(
        result.victims.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![10_002, 10_003]
    );
    assert!(result.victims.iter().all(|(id, _)| *id != ACTOR));
    assert_eq!(fixture.balance(10_004), 100);
    assert_eq!(HOSTILE_LOSS_MIN_BALANCE, 50);
}
