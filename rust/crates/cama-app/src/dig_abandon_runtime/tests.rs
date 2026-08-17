use crate::test_support::copy_migrated_database as initialize_or_migrate;
use cama_db::economy_event_repository::{
    EconomyEventRepository, EventDirection, EventDraft, EventEffects,
};
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const USER: i64 = 84_001;
const GUILD: i64 = 84_002;
const NOW: i64 = 1_800_000_000;

fn fixture(depth: Option<i64>) -> NamedTempFile {
    let database = NamedTempFile::new().unwrap();
    initialize_or_migrate(database.path()).unwrap();
    let connection = Connection::open(database.path()).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match Python foreign-key behavior");
    connection
        .execute(
            "INSERT INTO players
                (discord_id,guild_id,discord_username,jopacoin_balance)
             VALUES (?1,?2,'abandon-app',100)",
            params![USER, GUILD],
        )
        .unwrap();
    if let Some(depth) = depth {
        connection
            .execute(
                "INSERT INTO tunnels
                    (discord_id,guild_id,depth,max_depth,prestige_level,pickaxe_tier,
                     boss_progress,pinnacle_boss_id,pinnacle_phase,route_state)
                 VALUES (?1,?2,?3,350,2,4,'{}','forgotten_king',2,'{}')",
                params![USER, GUILD, depth],
            )
            .unwrap();
    }
    database
}

#[test]
fn preview_rejects_missing_and_too_shallow_tunnels_with_python_copy() {
    let missing = fixture(None);
    assert_eq!(
        DigAbandonRuntimeService::sqlite(missing.path())
            .preview_at(USER, GUILD, NOW)
            .unwrap_err()
            .to_string(),
        "You don't have a tunnel."
    );
    let shallow = fixture(Some(9));
    assert_eq!(
        DigAbandonRuntimeService::sqlite(shallow.path())
            .preview_at(USER, GUILD, NOW)
            .unwrap_err()
            .to_string(),
        "Tunnel must be at least 10 blocks deep to abandon."
    );
}

#[test]
fn neutral_preview_and_settlement_scale_refund_and_preserve_prestige() {
    let database = fixture(Some(50));
    let service = DigAbandonRuntimeService::sqlite(database.path());
    assert_eq!(
        service.preview_at(USER, GUILD, NOW).unwrap(),
        DigAbandonPreview {
            refund: 3,
            gross_refund: 5,
            current_depth: 50,
        }
    );
    let result = service.abandon(USER, GUILD, NOW).unwrap();
    assert_eq!(result.depth_lost, 50);
    assert_eq!(result.refund, 3);
    assert_eq!(result.balance_after, 103);
    assert_eq!(result.prestige_level, 2);
    let connection = Connection::open(database.path()).unwrap();
    let detail: String = connection
        .query_row(
            "SELECT detail FROM dig_actions WHERE id=?1",
            [result.audit_action_id],
            |row| row.get(0),
        )
        .unwrap();
    let detail: serde_json::Value = serde_json::from_str(&detail).unwrap();
    assert_eq!(detail["depth"], 50);
    assert_eq!(detail["refund"], 3);
    assert_eq!(detail["gross_jc"], 5);
    assert_eq!(detail["reward_multiplier"], 0.65);
}

#[test]
fn daily_economy_reward_is_applied_before_positive_dig_scaling() {
    let database = fixture(Some(100));
    let mut config = DigRuntimeConfig::default();
    config.economy_event.enabled = true;
    let event_date = cama_domain::game_date::game_date_for_timestamp(NOW as f64).unwrap();
    EconomyEventRepository::new(database.path())
        .activate_event_atomic(
            Some(GUILD),
            &EventDraft {
                event_date,
                name: "Double Dig".to_owned(),
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
                announcement: "Double Dig".to_owned(),
                starts_at: NOW - 60,
                ends_at: NOW + 60,
                created_at: NOW - 60,
            },
        )
        .unwrap();

    let service = DigAbandonRuntimeService::sqlite_with_config(database.path(), config);
    let preview = service.preview_at(USER, GUILD, NOW).unwrap();
    assert_eq!(preview.gross_refund, 10);
    assert_eq!(preview.refund, 13);
    let result = service.abandon(USER, GUILD, NOW).unwrap();
    assert_eq!(result.refund, 13);
    assert_eq!(result.balance_after, 113);
}

#[test]
fn sequential_duplicate_is_rejected_without_second_credit() {
    let database = fixture(Some(100));
    let service = DigAbandonRuntimeService::sqlite(database.path());
    assert_eq!(service.abandon(USER, GUILD, NOW).unwrap().refund, 7);
    Connection::open(database.path())
        .unwrap()
        .execute(
            "UPDATE tunnels SET depth=100 WHERE discord_id=?1 AND guild_id=?2",
            params![USER, GUILD],
        )
        .unwrap();
    assert_eq!(
        service
            .abandon(USER, GUILD, NOW + 1)
            .unwrap_err()
            .to_string(),
        "You can only abandon once every 24 hours."
    );
    assert_eq!(
        Connection::open(database.path())
            .unwrap()
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![USER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        107
    );
}
