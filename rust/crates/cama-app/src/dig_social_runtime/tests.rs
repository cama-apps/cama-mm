use std::collections::VecDeque;

use cama_db::economy_event_repository::{
    EconomyEventRepository, EventDirection, EventDraft, EventEffects,
};
use cama_db::predictions_repository::{BookSide, NewLevel, PredictionRepository};
use cama_db::schema_manager::initialize_or_migrate;
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const HELPER: i64 = -91_001;
const TARGET: i64 = -91_002;
const GUILD: i64 = 91_003;
const NOW: i64 = 1_900_000_000;

#[derive(Debug)]
struct ScriptedEntropy {
    integers: VecDeque<i64>,
    units: VecDeque<f64>,
    indexes: VecDeque<usize>,
}

impl ScriptedEntropy {
    fn new(
        integers: impl IntoIterator<Item = i64>,
        units: impl IntoIterator<Item = f64>,
        indexes: impl IntoIterator<Item = usize>,
    ) -> Self {
        Self {
            integers: integers.into_iter().collect(),
            units: units.into_iter().collect(),
            indexes: indexes.into_iter().collect(),
        }
    }
}

impl TunnelNameEntropy for ScriptedEntropy {
    fn unit_f64(&mut self) -> f64 {
        self.units.pop_front().expect("scripted tunnel-name roll")
    }

    fn index(&mut self, upper_exclusive: usize) -> usize {
        self.indexes
            .pop_front()
            .expect("scripted tunnel-name index")
            % upper_exclusive
    }
}

impl DigSocialEntropy for ScriptedEntropy {
    fn inclusive_i64(&mut self, low: i64, high: i64) -> i64 {
        let value = self.integers.pop_front().expect("scripted help advance");
        assert!((low..=high).contains(&value));
        value
    }
}

fn fixture(helper_tunnel: bool, target_depth: i64) -> NamedTempFile {
    let database = NamedTempFile::new().expect("social application database");
    initialize_or_migrate(database.path()).expect("migrated social application schema");
    let connection = Connection::open(database.path()).expect("social fixture DB");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match Python legacy foreign-key behavior");
    for (id, name, balance) in [(HELPER, "helper", 100), (TARGET, "target", 200)] {
        connection
            .execute(
                "INSERT INTO players
                    (discord_id,guild_id,discord_username,jopacoin_balance)
                 VALUES (?1,?2,?3,?4)",
                params![id, GUILD, name, balance],
            )
            .expect("player fixture");
    }
    connection
        .execute(
            "INSERT INTO tunnels
                (discord_id,guild_id,tunnel_name,depth,max_depth,last_dig_at,
                 boss_progress,stat_stamina)
             VALUES (?1,?2,'Target Descent',?3,?3,0,'{}',0)",
            params![TARGET, GUILD, target_depth],
        )
        .expect("target tunnel fixture");
    if helper_tunnel {
        connection
            .execute(
                "INSERT INTO tunnels
                    (discord_id,guild_id,tunnel_name,depth,max_depth,last_dig_at,
                     boss_progress,stat_stamina)
                 VALUES (?1,?2,'Helper Hollow',4,4,0,'{}',0)",
                params![HELPER, GUILD],
            )
            .expect("helper tunnel fixture");
    }
    database
}

fn scripted_help(
    service: &DigSocialRuntimeService,
    advance: i64,
    name_rolls: impl IntoIterator<Item = f64>,
    name_indexes: impl IntoIterator<Item = usize>,
) -> Result<DigHelpResult, DigSocialRuntimeError> {
    service.help_with_entropy(
        HELPER,
        TARGET,
        GUILD,
        NOW,
        &mut ScriptedEntropy::new([advance], name_rolls, name_indexes),
    )
}

fn balance(database: &NamedTempFile, discord_id: i64) -> i64 {
    Connection::open(database.path())
        .expect("balance DB")
        .query_row(
            "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, GUILD],
            |row| row.get(0),
        )
        .expect("player balance")
}

#[test]
fn help_advances_named_target_rewards_one_and_consumes_helper_cooldown() {
    let database = fixture(true, 10);
    let result = scripted_help(&DigSocialRuntimeService::sqlite(database.path()), 2, [], [])
        .expect("help succeeds");
    assert_eq!(result.advance, 2);
    assert_eq!(result.target_tunnel, "Target Descent");
    assert_eq!(result.target_depth_after, 12);
    assert_eq!(result.helper_reward, 1);
    assert_eq!(result.helper_balance_after, 101);
    assert_eq!(result.helper_cooldown_until, NOW + 3_600);
    assert_eq!(balance(&database, HELPER), 101);

    let error = scripted_help(&DigSocialRuntimeService::sqlite(database.path()), 1, [], [])
        .expect_err("same helper cooldown is consumed");
    assert_eq!(error.to_string(), "You're on cooldown (3600s remaining).");
}

#[test]
fn first_help_creates_the_exact_python_seeded_tunnel_name() {
    let database = fixture(false, 10);
    let result = scripted_help(
        &DigSocialRuntimeService::sqlite(database.path()),
        3,
        [0.0],
        [0, 0],
    )
    .expect("first help succeeds");
    assert_eq!(result.target_depth_after, 13);
    let helper: (String, i64) = Connection::open(database.path())
        .unwrap()
        .query_row(
            "SELECT tunnel_name,last_dig_at FROM tunnels
             WHERE discord_id=?1 AND guild_id=?2",
            params![HELPER, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(helper, ("The Whispering Descent".to_owned(), NOW));
}

#[test]
fn mycelium_link_adds_one_then_caps_before_the_unfinished_boss() {
    let database = fixture(true, 23);
    let connection = Connection::open(database.path()).unwrap();
    connection
        .execute(
            "UPDATE tunnels SET boss_progress=?1
             WHERE discord_id=?2 AND guild_id=?3",
            params![r#"{"25":"active"}"#, TARGET, GUILD],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO dig_artifacts
                (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,'mycelium_link',?3,1,1)",
            params![HELPER, GUILD, NOW - 1],
        )
        .unwrap();

    let result = scripted_help(&DigSocialRuntimeService::sqlite(database.path()), 3, [], [])
        .expect("capped help succeeds");
    assert_eq!(result.advance, 1);
    assert_eq!(result.target_depth_after, 24);
}

#[test]
fn mentor_rewards_both_players_after_daily_economy_then_positive_scaling() {
    let database = fixture(true, 10);
    Connection::open(database.path())
        .unwrap()
        .execute(
            "INSERT INTO dig_artifacts
                (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,'mentors_lantern',?3,1,1)",
            params![HELPER, GUILD, NOW - 1],
        )
        .unwrap();
    let event_date = game_date_for_timestamp(NOW as f64).unwrap();
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
    let mut config = DigRuntimeConfig::default();
    config.economy_event.enabled = true;
    let result = scripted_help(
        &DigSocialRuntimeService::sqlite_with_config(database.path(), config),
        1,
        [],
        [],
    )
    .unwrap();
    assert_eq!(result.helper_reward, 14);
    assert_eq!(result.mentor_helper_bonus, 14);
    assert_eq!(result.mentor_target_bonus, 13);
    assert_eq!(balance(&database, HELPER), 114);
    assert_eq!(balance(&database, TARGET), 213);
}

#[test]
fn mentor_target_credit_is_fail_soft_after_the_atomic_actor_settlement() {
    let database = fixture(true, 10);
    let connection = Connection::open(database.path()).unwrap();
    connection
        .execute(
            "INSERT INTO dig_artifacts
                (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,'mentors_lantern',?3,1,1)",
            params![HELPER, GUILD, NOW - 1],
        )
        .unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER fail_mentor_target_credit
             BEFORE UPDATE OF jopacoin_balance ON players
             WHEN OLD.discord_id={TARGET}
             BEGIN SELECT RAISE(ABORT,'injected target credit failure'); END;"
        ))
        .unwrap();

    let result = scripted_help(&DigSocialRuntimeService::sqlite(database.path()), 1, [], [])
        .expect("target bonus failure does not undo help");
    assert_eq!(result.mentor_helper_bonus, 7);
    assert_eq!(result.mentor_target_bonus, 0);
    assert_eq!(result.target_depth_after, 11);
    assert_eq!(balance(&database, HELPER), 107);
    assert_eq!(balance(&database, TARGET), 200);
}

#[test]
fn help_cooldown_orders_injury_stamina_curse_then_fail_soft_forest_mana() {
    let database = fixture(true, 10);
    let today = game_date_for_timestamp(NOW as f64).unwrap();
    let connection = Connection::open(database.path()).unwrap();
    connection
        .execute(
            "UPDATE tunnels
             SET stat_stamina=5,
                 injury_state=?1,
                 temp_curses=?2
             WHERE discord_id=?3 AND guild_id=?4",
            params![
                r#"{"type":"slower_cooldown"}"#,
                r#"{"digs_remaining":2,"effect":{"cooldown_penalty":0.25}}"#,
                HELPER,
                GUILD
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO player_mana
                (discord_id,guild_id,current_land,assigned_date,consumed_today)
             VALUES (?1,?2,'Forest',?3,0)",
            params![HELPER, GUILD, today],
        )
        .unwrap();

    let result =
        scripted_help(&DigSocialRuntimeService::sqlite(database.path()), 1, [], []).unwrap();
    // 6h injury override * (1 - 5*4% stamina) * 1.25 curse - 30s Forest.
    assert_eq!(result.helper_cooldown_until, NOW + 21_570);
}

#[test]
fn help_errors_match_the_python_service_copy() {
    let database = fixture(true, 10);
    let service = DigSocialRuntimeService::sqlite(database.path());
    assert_eq!(
        service
            .help(HELPER, HELPER, GUILD, NOW)
            .unwrap_err()
            .to_string(),
        "You can't help yourself."
    );
    Connection::open(database.path())
        .unwrap()
        .execute(
            "DELETE FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![TARGET, GUILD],
        )
        .unwrap();
    assert_eq!(
        scripted_help(&service, 1, [], []).unwrap_err().to_string(),
        "That player doesn't have a tunnel."
    );
}

#[test]
fn gift_moves_the_owned_relic_and_returns_its_authored_display_name() {
    let database = fixture(true, 10);
    Connection::open(database.path())
        .unwrap()
        .execute(
            "INSERT INTO dig_artifacts
                (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,'mole_claws',?3,1,1)",
            params![HELPER, GUILD, NOW - 1],
        )
        .unwrap();
    let result = DigSocialRuntimeService::sqlite(database.path())
        .gift_relic(HELPER, TARGET, GUILD, "mole_claws", NOW)
        .unwrap();
    assert_eq!(result.artifact_id, "mole_claws");
    assert_eq!(result.artifact_name, "Mole Claws");
    let connection = Connection::open(database.path()).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_artifacts
                 WHERE discord_id=?1 AND guild_id=?2 AND artifact_id='mole_claws'",
                params![TARGET, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn gift_validation_errors_match_python_copy() {
    let database = fixture(true, 10);
    let service = DigSocialRuntimeService::sqlite(database.path());
    assert_eq!(
        service
            .gift_relic(HELPER, HELPER, GUILD, "mole_claws", NOW)
            .unwrap_err()
            .to_string(),
        "You can't gift to yourself."
    );
    assert_eq!(
        service
            .gift_relic(HELPER, TARGET, GUILD, "mole_claws", NOW)
            .unwrap_err()
            .to_string(),
        "You don't have that artifact."
    );
    let connection = Connection::open(database.path()).unwrap();
    connection
        .execute(
            "INSERT INTO dig_artifacts
                (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,'plain_rock',?3,0,0)",
            params![HELPER, GUILD, NOW - 1],
        )
        .unwrap();
    assert_eq!(
        service
            .gift_relic(HELPER, TARGET, GUILD, "plain_rock", NOW)
            .unwrap_err()
            .to_string(),
        "Only relics can be gifted."
    );
    connection
        .execute(
            "DELETE FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![TARGET, GUILD],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO dig_artifacts
                (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,'mole_claws',?3,1,0)",
            params![HELPER, GUILD, NOW - 1],
        )
        .unwrap();
    assert_eq!(
        service
            .gift_relic(HELPER, TARGET, GUILD, "mole_claws", NOW)
            .unwrap_err()
            .to_string(),
        "Receiver doesn't have a tunnel."
    );
}

fn sabotage_fixture(target_depth: i64) -> NamedTempFile {
    let database = fixture(true, target_depth);
    Connection::open(database.path())
        .unwrap()
        .execute(
            "UPDATE tunnels SET depth=40,max_depth=40,pickaxe_tier=2
             WHERE discord_id=?1 AND guild_id=?2",
            params![HELPER, GUILD],
        )
        .unwrap();
    database
}

fn sabotage_with(
    service: &DigSocialRuntimeService,
    integers: impl IntoIterator<Item = i64>,
    units: impl IntoIterator<Item = f64>,
    indexes: impl IntoIterator<Item = usize>,
) -> Result<DigSabotageResult, DigSocialRuntimeError> {
    service.sabotage_with_entropy(
        HELPER,
        TARGET,
        GUILD,
        NOW,
        &mut ScriptedEntropy::new(integers, units, indexes),
    )
}

#[test]
fn sabotage_preview_is_read_only_base_cost_while_red_mana_halves_live_cost() {
    let database = sabotage_fixture(50);
    let today = game_date_for_timestamp(NOW as f64).unwrap();
    Connection::open(database.path())
        .unwrap()
        .execute(
            "INSERT INTO player_mana
                (discord_id,guild_id,current_land,assigned_date,consumed_today)
             VALUES (?1,?2,'Mountain',?3,0)",
            params![HELPER, GUILD, today],
        )
        .unwrap();
    let service = DigSocialRuntimeService::sqlite(database.path());
    assert_eq!(
        service.sabotage_preview(HELPER, TARGET, GUILD).unwrap(),
        DigSabotagePreview {
            cost: 10,
            damage_range: "3-8",
            target_depth: 50,
        }
    );
    let result = sabotage_with(&service, [3], [0.49, 0.99], [0, 0]).unwrap();
    assert_eq!(result.cost, 5);
    assert!(result.sabotage_hit);
    assert_eq!(balance(&database, HELPER), 95);
}

#[test]
fn sabotage_hit_applies_stacked_reduction_reward_clue_reveal_revenge_and_cooldown() {
    let database = sabotage_fixture(150);
    let connection = Connection::open(database.path()).unwrap();
    connection
        .execute(
            "UPDATE tunnels SET insured_until=?1,reinforced_until=?1
             WHERE discord_id=?2 AND guild_id=?3",
            params![NOW + 60, TARGET, GUILD],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO dig_artifacts
                (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,'obsidian_shield',?3,1,1)",
            params![TARGET, GUILD, NOW - 100],
        )
        .unwrap();
    for created_at in [NOW - 2 * 86_400, NOW - 4 * 86_400] {
        connection
            .execute(
                "INSERT INTO dig_actions
                    (guild_id,actor_id,target_id,action_type,depth_before,depth_after,
                     jc_delta,detail,created_at)
                 VALUES (?1,?2,?3,'sabotage',0,0,0,?4,?5)",
                params![
                    GUILD,
                    HELPER,
                    TARGET,
                    format!(r#"{{"target_id":{TARGET}}}"#),
                    created_at
                ],
            )
            .unwrap();
    }
    let service = DigSocialRuntimeService::sqlite(database.path());
    let result = sabotage_with(&service, [8], [0.49, 0.99], [0, 2]).unwrap();
    assert_eq!(result.damage, 2);
    assert!(result.insurance_applied && result.damage_reduced);
    assert_eq!(result.attacker_block_reward, 5);
    assert_eq!(result.target_depth_after, 148);
    assert_eq!(
        result.clue,
        Some(DigSabotageClue {
            kind: "first_letter",
            hint: "Saboteur's tunnel starts with 'H'".to_owned(),
        })
    );
    assert!(result.is_reveal);
    let connection = Connection::open(database.path()).unwrap();
    let state: (i64, i64, String, i64) = connection
        .query_row(
            "SELECT depth,revenge_target,revenge_type,revenge_until FROM tunnels
             WHERE discord_id=?1 AND guild_id=?2",
            params![TARGET, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(state, (148, HELPER, "damage".to_owned(), NOW + 21_600));
    assert_eq!(
        connection
            .query_row(
                "SELECT depth FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![HELPER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        45
    );
    assert_eq!(
        sabotage_with(&service, [3], [0.49], [0, 0])
            .unwrap_err()
            .to_string(),
        "You already sabotaged this player in the last 12 hours."
    );
}

#[test]
fn sabotage_miss_charges_cost_without_damage_reward_or_prediction() {
    let database = sabotage_fixture(50);
    let result = sabotage_with(
        &DigSocialRuntimeService::sqlite(database.path()),
        [],
        [0.99],
        [],
    )
    .unwrap();
    assert!(!result.sabotage_hit);
    assert_eq!(result.damage, 0);
    assert_eq!(result.target_depth_after, 50);
    assert_eq!(result.attacker_block_reward, 0);
    assert_eq!(result.prediction_contract_steal, None);
    assert_eq!(balance(&database, HELPER), 90);
    assert_eq!(
        Connection::open(database.path())
            .unwrap()
            .query_row(
                "SELECT depth FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![HELPER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        40
    );
}

#[test]
fn sabotage_trap_backfire_charges_double_credits_tip_and_clears_trap() {
    let database = sabotage_fixture(30);
    Connection::open(database.path())
        .unwrap()
        .execute(
            "UPDATE tunnels SET trap_active=1 WHERE discord_id=?1 AND guild_id=?2",
            params![TARGET, GUILD],
        )
        .unwrap();
    let result = sabotage_with(
        &DigSocialRuntimeService::sqlite(database.path()),
        [4],
        [],
        [],
    )
    .unwrap();
    assert!(result.trap_triggered);
    assert_eq!(result.cost, 12);
    assert_eq!(result.victim_tip, 16);
    assert_eq!(result.trap_detail.unwrap().blocks_lost, 4);
    assert_eq!(balance(&database, HELPER), 88);
    assert_eq!(balance(&database, TARGET), 222);
    let connection = Connection::open(database.path()).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT depth FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![HELPER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        36
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT trap_active FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![TARGET, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn aegis_block_is_durable_funded_and_duplicate_does_not_retip_or_reconsume() {
    let database = sabotage_fixture(100);
    Connection::open(database.path())
        .unwrap()
        .execute(
            "INSERT INTO manashop_buffs
                (discord_id,guild_id,buff_type,granted_at,expires_at,triggered,data)
             VALUES (?1,?2,'aegis',?3,?4,0,?5)",
            params![
                TARGET,
                GUILD,
                NOW - 1,
                NOW + 100,
                r#"{"capacity":75,"capacity_remaining":75,"rate":1.0}"#
            ],
        )
        .unwrap();
    let service = DigSocialRuntimeService::sqlite(database.path());
    let first = sabotage_with(&service, [], [], []).unwrap();
    assert_eq!(first.damage, 0);
    assert!(first.absorbed_by_aegis);
    assert_eq!(first.protection_source.as_deref(), Some("aegis"));
    assert_eq!(first.victim_tip, 16);
    assert_eq!(balance(&database, HELPER), 80);
    assert_eq!(balance(&database, TARGET), 216);

    let duplicate = sabotage_with(&service, [], [], []).unwrap();
    assert_eq!(duplicate.damage, 0);
    assert_eq!(duplicate.victim_tip, 0);
    assert_eq!(balance(&database, HELPER), 60);
    assert_eq!(balance(&database, TARGET), 216);
    let connection = Connection::open(database.path()).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT triggered FROM manashop_buffs
                 WHERE discord_id=?1 AND guild_id=?2 AND buff_type='aegis'",
                params![TARGET, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM hostile_loss_events
                 WHERE guild_id=?1 AND victim_id=?2",
                params![GUILD, TARGET],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn counterspell_and_shared_sanctuary_block_live_sabotage_without_consumption() {
    for (buff_type, owner_id, target_id) in [
        ("counterspell", TARGET, None),
        ("sanctuary", HELPER - 100, Some(TARGET)),
    ] {
        let database = sabotage_fixture(100);
        Connection::open(database.path())
            .unwrap()
            .execute(
                "INSERT INTO manashop_buffs
                    (discord_id,guild_id,buff_type,target_id,granted_at,expires_at,
                     triggered,data)
                 VALUES (?1,?2,?3,?4,?5,?6,0,?7)",
                params![
                    owner_id,
                    GUILD,
                    buff_type,
                    target_id,
                    NOW - 1,
                    NOW + 100,
                    r#"{"capacity":150,"capacity_remaining":150,"rate":1.0,"shared":true}"#,
                ],
            )
            .unwrap();

        let result = sabotage_with(
            &DigSocialRuntimeService::sqlite(database.path()),
            [],
            [],
            [],
        )
        .unwrap();
        assert!(result.sabotage_hit);
        assert_eq!(result.damage, 0);
        assert!(result.damage_reduced);
        assert!(!result.absorbed_by_aegis);
        assert_eq!(result.protection_source.as_deref(), Some(buff_type));
        assert_eq!(result.victim_tip, 16);
        assert_eq!(balance(&database, HELPER), 80);
        assert_eq!(balance(&database, TARGET), 216);
        let connection = Connection::open(database.path()).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT triggered FROM manashop_buffs WHERE buff_type=?1",
                    [buff_type],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0,
            "{buff_type} is a non-consuming immunity"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM dig_actions
                     WHERE action_type='sabotage' AND json_extract(detail,'$.protection_source')=?1",
                    [buff_type],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }
}

#[test]
fn black_mana_prediction_steal_and_vendetta_settle_after_the_base_hit() {
    let database = sabotage_fixture(100);
    let today = game_date_for_timestamp(NOW as f64).unwrap();
    let connection = Connection::open(database.path()).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match Python prediction foreign-key behavior");
    connection
        .execute(
            "INSERT INTO player_mana
                (discord_id,guild_id,current_land,assigned_date,consumed_today)
             VALUES (?1,?2,'Swamp',?3,0)",
            params![HELPER, GUILD, today],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO dig_artifacts
                (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,'vendetta_coin',?3,1,1)",
            params![TARGET, GUILD, NOW - 1],
        )
        .unwrap();
    let predictions = PredictionRepository::new(database.path());
    let market = predictions
        .create_orderbook_prediction(
            Some(GUILD),
            TARGET,
            "Will sabotage matter?",
            50,
            None,
            &[
                NewLevel {
                    side: BookSide::YesAsk,
                    price: 55,
                    size: 10,
                },
                NewLevel {
                    side: BookSide::YesBid,
                    price: 45,
                    size: 10,
                },
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO prediction_positions
                (prediction_id,discord_id,yes_contracts,yes_cost_basis_total,
                 no_contracts,no_cost_basis_total)
             VALUES (?1,?2,8,24,0,0)",
            params![market, TARGET],
        )
        .unwrap();

    let result = sabotage_with(
        &DigSocialRuntimeService::sqlite(database.path()),
        [8, 4],
        [0.0, 0.0],
        [0, 0, 0],
    )
    .unwrap();
    assert_eq!(result.damage, 8);
    assert_eq!(result.mana_steal_jc, 8);
    assert_eq!(result.vendetta_reflect, 4);
    assert_eq!(result.vendetta_bonus, 3);
    assert_eq!(
        result.prediction_contract_steal,
        Some(DigPredictionContractSteal {
            prediction_id: market,
            side: "yes",
            contracts: 4,
        })
    );
    assert_eq!(balance(&database, HELPER), 84);
    assert_eq!(balance(&database, TARGET), 203);
    let victim = predictions
        .get_user_open_positions(TARGET, Some(GUILD))
        .unwrap();
    let actor = predictions
        .get_user_open_positions(HELPER, Some(GUILD))
        .unwrap();
    assert_eq!(victim[0].position.yes_contracts, 4);
    assert_eq!(actor[0].position.yes_contracts, 4);
    assert_eq!(
        Connection::open(database.path())
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM dig_actions
                 WHERE actor_id=?1 AND action_type='vendetta_reflect'",
                params![TARGET],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn sabotage_validation_errors_match_python_copy() {
    let database = sabotage_fixture(30);
    let service = DigSocialRuntimeService::sqlite(database.path());
    assert_eq!(
        service
            .sabotage_preview(HELPER, HELPER, GUILD)
            .unwrap_err()
            .to_string(),
        "You can't sabotage yourself."
    );
    Connection::open(database.path())
        .unwrap()
        .execute(
            "UPDATE players SET jopacoin_balance=0 WHERE discord_id=?1 AND guild_id=?2",
            params![HELPER, GUILD],
        )
        .unwrap();
    assert_eq!(
        sabotage_with(&service, [], [], []).unwrap_err().to_string(),
        "Sabotage costs 6 JC but you only have 0 JC."
    );
    Connection::open(database.path())
        .unwrap()
        .execute(
            "DELETE FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![TARGET, GUILD],
        )
        .unwrap();
    assert_eq!(
        service
            .sabotage_preview(HELPER, TARGET, GUILD)
            .unwrap_err()
            .to_string(),
        "That player doesn't have a tunnel."
    );
}
