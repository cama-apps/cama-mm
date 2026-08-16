//! Migrated-SQLite production-path twins for the reward-ordering policy cases.

use cama_app::dig_loot::{LootEntropy, SeededLootEntropy};
use cama_app::dig_runtime::{DigRuntimeRequest, DigRuntimeService};
use cama_app::dig_service::layer_at;
use cama_db::core_repositories::{NewPlayer, PlayerRepository};
use cama_db::economy_event_repository::{
    EconomyEventRepository, EventDirection, EventDraft, EventEffects,
};
use cama_db::schema_manager::initialize_or_migrate;
use cama_domain::dig_economy::scale_positive_dig_jc;
use cama_domain::game_date::game_date_for_timestamp;
use rusqlite::{Connection, params};
use serde_json::Value;
use tempfile::NamedTempFile;

const NORMAL_ACTOR: i64 = 95_001;
const NORMAL_GUILD: i64 = 95_002;
const CAVE_ACTOR: i64 = 95_011;
const CAVE_GUILD: i64 = 95_012;
const START: i64 = 1_900_000_000;

fn seed_tunnel(
    database: &NamedTempFile,
    actor: i64,
    guild: i64,
    depth: i64,
    now: i64,
    mutations: &str,
) {
    initialize_or_migrate(database.path()).expect("canonical migration");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(actor, "reward-policy-live", Some(guild)))
        .expect("player");
    let connection = Connection::open(database.path()).expect("seed connection");
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=100
              WHERE discord_id=?1 AND guild_id=?2",
            params![actor, guild],
        )
        .expect("balance");
    connection
        .execute(
            "INSERT INTO tunnels (
                 discord_id,guild_id,tunnel_name,depth,max_depth,total_digs,
                 last_dig_at,luminosity,last_lum_update_at,prestige_level,
                 prestige_perks,boss_progress,streak_days,mutations
             ) VALUES (?1,?2,'Reward Policy',?3,?3,1,?4,100,?4,0,'[]',?5,0,?6)",
            params![
                actor,
                guild,
                depth,
                now - 7_200,
                r#"{"25":{"status":"defeated"},"50":{"status":"defeated"},"75":{"status":"defeated"},"100":{"status":"defeated"},"150":{"status":"defeated"},"200":{"status":"defeated"},"275":{"status":"defeated"}}"#,
                mutations,
            ],
        )
        .expect("tunnel");
}

fn seed_weather(database: &NamedTempFile, guild: i64, now: i64, layer: &str, weather: &str) {
    let date = game_date_for_timestamp(now as f64).expect("weather date");
    Connection::open(database.path())
        .expect("weather connection")
        .execute(
            "INSERT INTO dig_weather(guild_id,game_date,layer_name,weather_id)
             VALUES (?1,?2,?3,?4), (?1,?2,?5,'earthworm_migration')",
            params![
                guild,
                date,
                layer,
                weather,
                if layer == "Dirt" { "Stone" } else { "Dirt" }
            ],
        )
        .expect("weather rows");
}

fn seed_doubling_event(database: &NamedTempFile, guild: i64, now: i64) {
    let event_date = game_date_for_timestamp(now as f64).expect("event date");
    EconomyEventRepository::new(database.path())
        .activate_event_atomic(
            Some(guild),
            &EventDraft {
                event_date,
                name: "Double Dig rewards".to_owned(),
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
                announcement: "Double Dig rewards".to_owned(),
                starts_at: now - 60,
                ends_at: now + 60,
                created_at: now - 60,
            },
        )
        .expect("doubling event");
}

fn seed_for(actor: i64, guild: i64, now: i64) -> u64 {
    let mut value = actor as u64;
    value = value.rotate_left(17) ^ guild as u64;
    value = value.rotate_left(23) ^ now as u64;
    value
}

fn normal_dig_time(actor: i64, guild: i64, start: i64, depth: i64, wanted_jc: i64) -> i64 {
    let layer = layer_at(depth);
    (start..start.saturating_add(100_000))
        .find(|candidate| {
            let mut entropy = SeededLootEntropy::new(seed_for(actor, guild, *candidate));
            // Keep the normal branch away from cave/event paths so the test
            // isolates the positive payout boundary.
            if entropy.unit() <= 0.60 {
                return false;
            }
            let _advance = entropy.advance(layer.advance_range.0, layer.advance_range.1);
            if entropy.unit() < 0.27 {
                return false;
            }
            entropy.advance(layer.jc_range.0, layer.jc_range.1) == wanted_jc
        })
        .expect("normal reward seed")
}

fn cave_reward_time(actor: i64, guild: i64, start: i64) -> (i64, i64) {
    let layer = layer_at(10);
    (start..start.saturating_add(100_000))
        .find_map(|candidate| {
            let mut entropy = SeededLootEntropy::new(seed_for(actor, guild, candidate));
            if entropy.unit() >= layer.cave_in_chance {
                return None;
            }
            let _advance = entropy.advance(layer.advance_range.0, layer.advance_range.1);
            let block_loss = entropy.advance(6, 14);
            if entropy.unit() >= 0.30 {
                return None;
            }
            let lucky_rubble = entropy.advance(1, 3);
            if lucky_rubble != 1 {
                return None;
            }
            // Shallow-band catastrophe is zero probability and consumes no
            // draw. Select the no-debit Stun consequence for a clean wallet
            // assertion, matching the Python random=0 fixture.
            if entropy.advance(1, 100) > 30 {
                return None;
            }
            Some((candidate, block_loss))
        })
        .expect("cave reward seed")
}

fn latest_detail(database: &NamedTempFile, actor: i64, guild: i64) -> Value {
    let raw: String = Connection::open(database.path())
        .expect("detail connection")
        .query_row(
            "SELECT detail FROM dig_actions
              WHERE actor_id=?1 AND guild_id=?2 AND action_type='dig'
              ORDER BY id DESC LIMIT 1",
            params![actor, guild],
            |row| row.get(0),
        )
        .expect("dig detail");
    serde_json::from_str(&raw).expect("detail JSON")
}

// tests/test_dig_reward_policy.py::test_live_dig_scales_full_positive_payout_before_sinks
#[test]
fn live_dig_scales_full_positive_payout_before_sinks() {
    let database = NamedTempFile::new().expect("normal reward database");
    seed_tunnel(&database, NORMAL_ACTOR, NORMAL_GUILD, 76, START, "[]");
    seed_weather(&database, NORMAL_GUILD, START, "Magma", "fossil_rush");
    let now = normal_dig_time(NORMAL_ACTOR, NORMAL_GUILD, START, 76, 3);
    let before: i64 = Connection::open(database.path())
        .expect("normal balance connection")
        .query_row(
            "SELECT jopacoin_balance FROM players
              WHERE discord_id=?1 AND guild_id=?2",
            params![NORMAL_ACTOR, NORMAL_GUILD],
            |row| row.get(0),
        )
        .expect("normal balance");
    let outcome = DigRuntimeService::sqlite(database.path())
        .dig(DigRuntimeRequest {
            discord_id: NORMAL_ACTOR,
            guild_id: NORMAL_GUILD,
            now,
            paid: false,
            forced_event: false,
        })
        .expect("live normal Dig");
    assert!(outcome.success, "live Dig failed: {:?}", outcome.error);
    assert!(!outcome.cave_in);
    let detail = latest_detail(&database, NORMAL_ACTOR, NORMAL_GUILD);
    let gross = detail["gross_jc"].as_i64().expect("gross normal JC");
    assert!(gross > 0);
    assert_eq!(
        detail["economy_adjusted_jc"].as_i64(),
        Some(scale_positive_dig_jc(gross))
    );
    assert_eq!(outcome.jc_earned, scale_positive_dig_jc(gross));
    let after = before + outcome.jc_earned;
    assert_eq!(
        Connection::open(database.path())
            .expect("normal final connection")
            .query_row(
                "SELECT jopacoin_balance FROM players
                  WHERE discord_id=?1 AND guild_id=?2",
                params![NORMAL_ACTOR, NORMAL_GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("normal final balance"),
        after
    );
}

// tests/test_dig_reward_policy.py::test_cave_in_scales_combined_positive_reward_once
#[test]
fn cave_in_scales_combined_positive_reward_once() {
    let database = NamedTempFile::new().expect("cave reward database");
    seed_tunnel(
        &database,
        CAVE_ACTOR,
        CAVE_GUILD,
        10,
        START,
        r#"[{"id":"cave_in_loot"}]"#,
    );
    Connection::open(database.path())
        .expect("cave setup connection")
        .execute(
            "INSERT INTO dig_artifacts
                (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,'gamblers_charm',?3,1,1)",
            params![CAVE_ACTOR, CAVE_GUILD, START],
        )
        .expect("Gambler's Charm");
    let (now, block_loss) = cave_reward_time(CAVE_ACTOR, CAVE_GUILD, START);
    seed_weather(&database, CAVE_GUILD, now, "Dirt", "reward_policy_neutral");
    seed_doubling_event(&database, CAVE_GUILD, now);
    let before: i64 = Connection::open(database.path())
        .expect("cave balance connection")
        .query_row(
            "SELECT jopacoin_balance FROM players
              WHERE discord_id=?1 AND guild_id=?2",
            params![CAVE_ACTOR, CAVE_GUILD],
            |row| row.get(0),
        )
        .expect("cave balance");
    let outcome = DigRuntimeService::sqlite(database.path())
        .dig(DigRuntimeRequest {
            discord_id: CAVE_ACTOR,
            guild_id: CAVE_GUILD,
            now,
            paid: false,
            forced_event: false,
        })
        .expect("live cave Dig");
    assert!(outcome.success, "live cave Dig failed: {:?}", outcome.error);
    assert!(outcome.cave_in);
    let detail = latest_detail(&database, CAVE_ACTOR, CAVE_GUILD);
    let gross = detail["gross_jc"].as_i64().expect("cave gross JC");
    assert_eq!(gross, 1 + (block_loss / 2).max(1));
    assert_eq!(detail["economy_adjusted_jc"].as_i64(), Some(gross * 2));
    let expected_credit = scale_positive_dig_jc(gross * 2);
    assert_eq!(
        Connection::open(database.path())
            .expect("cave final connection")
            .query_row(
                "SELECT jopacoin_balance FROM players
                  WHERE discord_id=?1 AND guild_id=?2",
                params![CAVE_ACTOR, CAVE_GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("cave final balance"),
        before + expected_credit
    );
    assert_eq!(
        Connection::open(database.path())
            .expect("cave action connection")
            .query_row(
                "SELECT jc_delta FROM dig_actions
                  WHERE actor_id=?1 AND guild_id=?2 AND action_type='dig'
                  ORDER BY id DESC LIMIT 1",
                params![CAVE_ACTOR, CAVE_GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("cave action delta"),
        expected_credit
    );
}
