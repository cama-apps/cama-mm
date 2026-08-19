use crate::test_support::{FastTestDatabase, fast_migrated_database};
use cama_db::core_repositories::{NewPlayer, PlayerRepository};
use cama_db::economy_event_repository::{
    EconomyEventRepository, EventDirection, EventDraft, EventEffects,
};
use rusqlite::{Connection, params};

use super::*;
use crate::boss_multi_tier::{
    BossAuditRecord, BossCombatPreparationPort, BossCombatPreparationRequest, BossRepositoryPort,
    BossStatus, CombatEffects, CombatState, EchoRecord, GearRepairPort, GearSnapshot,
    PausedBossDuel, PendingPrompt, PlayerKey, PromptOption, RepositoryCommit, SequenceEntropy,
    StartBossDuelOutcome,
};
use crate::vanity_tax_service::{VanityMember, VanityTaxService};

const PLAYER: i64 = -84_001;
const GUILD: i64 = 84_002;

fn key() -> PlayerKey {
    PlayerKey::new(PLAYER, Some(GUILD))
}

fn fixture() -> FastTestDatabase {
    let database = fast_migrated_database();
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(PLAYER, "boss-app", Some(GUILD)))
        .expect("player");
    let connection = Connection::open(database.path()).expect("fixture DB");
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=100
              WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
        )
        .expect("balance");
    connection
        .execute(
            "INSERT INTO tunnels
             (discord_id,guild_id,depth,max_depth,prestige_level,boss_progress,
              stinger_curse,boss_attempts,last_dig_at,cheer_data,stat_points,
              stat_boss_awards,tunnel_name)
             VALUES (?1,?2,24,24,0,?3,?4,1,99,?5,5,?6,'Boss App')",
            params![
                PLAYER,
                GUILD,
                r#"{"25":{"boss_id":"grothak","status":"active","active_prep":{"item":"whetstone"},"pending_phase_event_id":"falling_stars","future_field":17}}"#,
                r#"{"halve_next_wager":true,"_boss_id":"grothak","future_curse":9}"#,
                r#"[{"user_id":7,"expires_at":9999999999}]"#,
                r#"{"prestige_level":0,"awards":[]}"#,
            ],
        )
        .expect("tunnel");
    database
}

fn set_phase_two(database: &FastTestDatabase, pending_event_id: &str) {
    Connection::open(database.path())
        .expect("phase DB")
        .execute(
            "UPDATE tunnels
                SET prestige_level=2,
                    boss_progress=json_object(
                        '25',json_object(
                            'boss_id','grothak',
                            'status','phase1_defeated',
                            'pending_phase_event_id',?1,
                            'future_field',17
                        )
                    )
              WHERE discord_id=?2 AND guild_id=?3",
            params![pending_event_id, PLAYER, GUILD],
        )
        .expect("phase two");
}

fn pending_phase_event(database: &FastTestDatabase) -> Option<String> {
    let raw = Connection::open(database.path())
        .expect("phase DB")
        .query_row(
            "SELECT boss_progress FROM tunnels
              WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
            |row| row.get::<_, String>(0),
        )
        .expect("boss progress");
    serde_json::from_str::<Value>(&raw)
        .expect("boss progress JSON")
        .get("25")
        .and_then(|entry| entry.get("pending_phase_event_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn set_pinnacle(database: &FastTestDatabase, phase: u8, status: &str, pending_event: Option<&str>) {
    let mut progress = Map::new();
    for boundary in crate::dig_bosses::BOSS_BOUNDARIES {
        progress.insert(boundary.to_string(), Value::String("defeated".to_owned()));
    }
    let mut pinnacle = Map::new();
    pinnacle.insert("status".to_owned(), Value::String(status.to_owned()));
    pinnacle.insert(
        "boss_id".to_owned(),
        Value::String("forgotten_king".to_owned()),
    );
    pinnacle.insert("first_meet_seen".to_owned(), Value::Bool(true));
    if let Some(event) = pending_event {
        pinnacle.insert(
            "pending_phase_event_id".to_owned(),
            Value::String(event.to_owned()),
        );
    }
    progress.insert(PINNACLE_DEPTH.to_string(), Value::Object(pinnacle));
    Connection::open(database.path())
        .expect("pinnacle DB")
        .execute(
            "UPDATE tunnels
                SET depth=?1,max_depth=MAX(max_depth,?1),boss_progress=?2,
                    prestige_level=0,pinnacle_boss_id='forgotten_king',
                    pinnacle_phase=?3,luminosity=100,stinger_curse=NULL,
                    boss_attempts=0,cheer_data=NULL
              WHERE discord_id=?4 AND guild_id=?5",
            params![
                PINNACLE_DEPTH - 1,
                Value::Object(progress).to_string(),
                phase,
                PLAYER,
                GUILD
            ],
        )
        .expect("pinnacle state");
}

fn pinnacle_request(now: i64) -> DigBossRuntimeRequest {
    DigBossRuntimeRequest {
        discord_id: PLAYER,
        guild_id: GUILD,
        now,
    }
}

fn pinnacle_runtime(database: &FastTestDatabase) -> DigBossRuntimeService {
    let mut config = DigBossRuntimeConfig::new(database.path(), 20);
    config.economy_event.enabled = false;
    DigBossRuntimeService::sqlite(config)
}

fn regular_resolved_fixture(won: bool, broken_ids: Vec<i64>) -> ResolvedFight {
    ResolvedFight {
        won,
        boss_id: "grothak".to_owned(),
        boundary: 100,
        wager: 10,
        win_chance: 0.6,
        jc_delta: if won { 500 } else { -10 },
        gross_payout: if won { 500 } else { 0 },
        bankruptcy_penalty: 0,
        vanity_tax: 0,
        new_depth: if won { 100 } else { 95 },
        boss_hp_remaining: if won { 0 } else { 50 },
        boss_hp_max: 100,
        starting_boss_hp: 100,
        extra_knockback: 0,
        extra_cooldown_seconds: 0,
        round_log: Vec::new(),
        gear_wear: GearWearResult { broken_ids },
        phase_transition: None,
        boss_preparation: None,
        rescue_line_used: false,
        warding_salts_blocked: false,
    }
}

#[test]
fn regular_full_kill_persists_and_projects_both_loud_drop_policies() {
    let database = fixture();
    let connection = Connection::open(database.path()).expect("loud-drop DB");
    connection
        .execute(
            "UPDATE tunnels SET depth=100,max_depth=100,prestige_level=5
              WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
        )
        .expect("eligible tunnel");
    connection
        .execute(
            "INSERT INTO dig_gear
             (discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source)
             VALUES (?1,?2,'armor',4,0,1,1,'shop')",
            params![PLAYER, GUILD],
        )
        .expect("broken gear");
    let broken_id = connection.last_insert_rowid();
    drop(connection);

    let runtime = pinnacle_runtime(&database);
    let resolved = regular_resolved_fixture(true, vec![broken_id]);
    let mut gear_drop = None;
    let mut relic_drop = None;
    let mut broken = Vec::new();
    let mut warnings = Vec::new();
    let mut entropy = SequenceEntropy::new(vec![0.0, 0.0], vec![0, 0], Vec::new());
    runtime.enrich_regular_resolution(
        pinnacle_request(1_700_000_000),
        true,
        &resolved,
        &mut gear_drop,
        &mut relic_drop,
        &mut broken,
        &mut warnings,
        &mut entropy,
    );

    assert!(warnings.is_empty(), "{warnings:?}");
    let gear_drop = gear_drop.expect("7% gear drop");
    assert_eq!(gear_drop.slot, DomainGearSlot::Weapon);
    assert_eq!(gear_drop.tier, 4);
    assert_eq!(gear_drop.name, "Obsidian Pickaxe");
    let relic_drop = relic_drop.expect("10% prestige relic drop");
    assert_eq!(relic_drop.name, "Hollow Fang");
    assert_eq!(broken.len(), 1);
    assert_eq!(broken[0].gear_id, broken_id);
    assert_eq!(broken[0].name, "Obsidian Brigandine");

    let connection = Connection::open(database.path()).expect("loud-drop verify DB");
    let stored_gear: (String, i64, i64, String) = connection
        .query_row(
            "SELECT slot,tier,durability,source FROM dig_gear WHERE id=?1",
            [gear_drop.gear_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("stored boss gear");
    assert_eq!(
        stored_gear,
        ("weapon".to_owned(), 4, 20, "boss_drop".to_owned())
    );
    let stored_relic: (String, i64, i64) = connection
        .query_row(
            "SELECT artifact_id,is_relic,equipped FROM dig_artifacts WHERE id=?1",
            [relic_drop.database_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("stored boss relic");
    assert_eq!(
        stored_relic,
        (relic_drop.artifact_id.clone(), 1, 0),
        "the presentation receipt names the exact durable reward row"
    );
}

#[test]
fn regular_phase_transition_and_loss_suppress_loud_drop_rolls() {
    let database = fixture();
    Connection::open(database.path())
        .expect("suppression DB")
        .execute(
            "UPDATE tunnels SET depth=100,max_depth=100,prestige_level=5
              WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
        )
        .expect("eligible tunnel");
    let runtime = pinnacle_runtime(&database);
    for mut resolved in [
        regular_resolved_fixture(false, Vec::new()),
        regular_resolved_fixture(true, Vec::new()),
    ] {
        if resolved.won {
            resolved.phase_transition = Some(BossStatus::PhaseOneDefeated);
        }
        let mut gear_drop = None;
        let mut relic_drop = None;
        let mut broken = Vec::new();
        let mut warnings = Vec::new();
        runtime.enrich_regular_resolution(
            pinnacle_request(1_700_000_000),
            true,
            &resolved,
            &mut gear_drop,
            &mut relic_drop,
            &mut broken,
            &mut warnings,
            &mut SequenceEntropy::constant(0.0),
        );
        assert!(gear_drop.is_none());
        assert!(relic_drop.is_none());
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    let connection = Connection::open(database.path()).expect("suppression verify DB");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_gear WHERE source='boss_drop'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("boss gear count"),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_artifacts", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("boss relic count"),
        0
    );
}

#[test]
fn encounter_locks_and_marks_first_meet_without_losing_unknown_progress() {
    let database = fixture();
    let first = pinnacle_runtime(&database)
        .encounter(
            pinnacle_request(1_700_000_000),
            SequenceEntropy::new(Vec::new(), vec![0], Vec::new()),
        )
        .expect("regular encounter");
    assert_eq!(first.boundary, 25);
    assert_eq!(first.boss_id, "grothak");
    assert_eq!(first.boss_name, "Grothak the Unbreakable");
    assert_eq!(first.phase, 1);
    assert!(!first.is_pinnacle);
    assert!(first.wager_allowed);
    assert!(!first.dialogue.is_empty());

    let connection = Connection::open(database.path()).expect("encounter DB");
    let progress: String = connection
        .query_row(
            "SELECT boss_progress FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
            |row| row.get(0),
        )
        .expect("progress");
    let progress: Value = serde_json::from_str(&progress).expect("progress JSON");
    assert_eq!(progress["25"]["first_meet_seen"], true);
    assert_eq!(progress["25"]["future_field"], 17);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                .get::<_, i64>(0))
            .expect("audit count"),
        0,
        "encounter presentation is not a money-moving audit action"
    );

    let after_restart = pinnacle_runtime(&database)
        .encounter(
            pinnacle_request(1_700_000_001),
            SequenceEntropy::new(Vec::new(), vec![1], Vec::new()),
        )
        .expect("restart encounter");
    assert_eq!(after_restart.boss_id, first.boss_id);
}

#[test]
fn pinnacle_encounter_rehydrates_phase_and_carried_wager() {
    let database = fixture();
    set_pinnacle(&database, 2, "phase1_defeated", Some("quiet"));
    let connection = Connection::open(database.path()).expect("carry DB");
    let raw: String = connection
        .query_row(
            "SELECT boss_progress FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
            |row| row.get(0),
        )
        .expect("progress");
    let mut progress: Value = serde_json::from_str(&raw).expect("progress JSON");
    progress[PINNACLE_DEPTH.to_string()]["carried_wager"] = Value::from(75);
    progress[PINNACLE_DEPTH.to_string()]["carried_risk_tier"] = Value::from("bold");
    connection
        .execute(
            "UPDATE tunnels SET boss_progress=?1 WHERE discord_id=?2 AND guild_id=?3",
            params![progress.to_string(), PLAYER, GUILD],
        )
        .expect("seed carry");

    let encounter = pinnacle_runtime(&database)
        .encounter(
            pinnacle_request(1_700_000_000),
            SequenceEntropy::new(Vec::new(), vec![0], Vec::new()),
        )
        .expect("pinnacle encounter");
    assert!(encounter.is_pinnacle);
    assert_eq!(encounter.phase, 2);
    assert_eq!(encounter.boss_id, "forgotten_king");
    assert_eq!(encounter.carried_wager, Some(75));
    assert_eq!(encounter.carried_risk_tier, Some(RiskTier::Bold));
    assert!(encounter.wager_allowed);
}

#[test]
fn retreat_atomically_moves_depth_and_audits_once() {
    let database = fixture();
    let result = pinnacle_runtime(&database)
        .retreat(
            pinnacle_request(1_700_000_000),
            SequenceEntropy::new(Vec::new(), Vec::new(), vec![2]),
        )
        .expect("regular retreat");
    assert_eq!(result.outcome.boundary, 25);
    assert_eq!(result.outcome.block_loss, 2);
    assert_eq!(result.outcome.new_depth, 22);
    assert_eq!(result.outcome.carried_wager_forfeit, 0);
    assert!(result.action_id.is_some());
    let connection = Connection::open(database.path()).expect("retreat DB");
    let (depth, action_type, detail): (i64, String, String) = connection
        .query_row(
            "SELECT t.depth,a.action_type,a.detail
               FROM tunnels t JOIN dig_actions a
                 ON a.actor_id=t.discord_id AND a.guild_id=t.guild_id
              WHERE t.discord_id=?1 AND t.guild_id=?2",
            params![PLAYER, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("retreat state");
    assert_eq!(depth, 22);
    assert_eq!(action_type, "boss_retreat");
    assert_eq!(
        serde_json::from_str::<Value>(&detail).expect("detail")["loss"],
        2
    );
    assert!(matches!(
        pinnacle_runtime(&database).retreat(
            pinnacle_request(1_700_000_001),
            SequenceEntropy::new(Vec::new(), Vec::new(), vec![2]),
        ),
        Err(DigBossRuntimeError::Policy(
            BossServiceError::NotAtBossBoundary
        ))
    ));
}

#[test]
fn pinnacle_retreat_forfeits_half_carried_wager_in_actor_transaction() {
    let database = fixture();
    set_pinnacle(&database, 2, "phase1_defeated", None);
    let connection = Connection::open(database.path()).expect("carry retreat DB");
    let raw: String = connection
        .query_row(
            "SELECT boss_progress FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
            |row| row.get(0),
        )
        .expect("progress");
    let mut progress: Value = serde_json::from_str(&raw).expect("progress JSON");
    progress[PINNACLE_DEPTH.to_string()]["carried_wager"] = Value::from(75);
    progress[PINNACLE_DEPTH.to_string()]["carried_risk_tier"] = Value::from("bold");
    connection
        .execute(
            "UPDATE tunnels SET boss_progress=?1 WHERE discord_id=?2 AND guild_id=?3",
            params![progress.to_string(), PLAYER, GUILD],
        )
        .expect("seed carry");
    let result = pinnacle_runtime(&database)
        .retreat(
            pinnacle_request(1_700_000_000),
            SequenceEntropy::new(Vec::new(), Vec::new(), vec![3]),
        )
        .expect("pinnacle retreat");
    assert_eq!(result.outcome.block_loss, 3);
    assert_eq!(result.outcome.carried_wager_forfeit, 37);
    assert_eq!(result.outcome.actual_balance_delta, -37);
    let (balance, progress): (i64, String) = connection
        .query_row(
            "SELECT p.jopacoin_balance,t.boss_progress
               FROM players p JOIN tunnels t USING(discord_id,guild_id)
              WHERE p.discord_id=?1 AND p.guild_id=?2",
            params![PLAYER, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("retreat result");
    assert_eq!(balance, 63);
    let progress: Value = serde_json::from_str(&progress).expect("progress JSON");
    assert!(
        progress[PINNACLE_DEPTH.to_string()]
            .get("carried_wager")
            .is_none()
    );
    assert_eq!(
        progress[PINNACLE_DEPTH.to_string()]["last_outcome"],
        "retreat"
    );
}

#[test]
fn cheer_is_atomic_creates_missing_tunnel_and_enforces_cooldown() {
    let database = fixture();
    let cheerer = PLAYER - 1;
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(cheerer, "cheerer", Some(GUILD)))
        .expect("cheerer player");
    Connection::open(database.path())
        .expect("clear cheers DB")
        .execute(
            "UPDATE tunnels SET cheer_data=NULL WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
        )
        .expect("clear fixture cheer");
    let runtime = pinnacle_runtime(&database);
    let first = runtime
        .cheer(cheerer, PLAYER, GUILD, 1_700_000_000)
        .expect("first cheer");
    assert_eq!(first.cheer_count, 1);
    assert!((first.total_boost - 0.05).abs() < f64::EPSILON);
    let connection = Connection::open(database.path()).expect("cheer DB");
    let (last_cheer_at, target_cheers): (i64, String) = connection
        .query_row(
            "SELECT c.last_cheer_at,t.cheer_data
               FROM tunnels c JOIN tunnels t ON t.discord_id=?1 AND t.guild_id=c.guild_id
              WHERE c.discord_id=?2 AND c.guild_id=?3",
            params![PLAYER, cheerer, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("atomic cheer state");
    assert_eq!(last_cheer_at, 1_700_000_000);
    assert_eq!(
        serde_json::from_str::<Value>(&target_cheers)
            .expect("cheer JSON")
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    assert!(matches!(
        runtime.cheer(cheerer, PLAYER, GUILD, 1_700_000_001),
        Err(DigBossRuntimeError::CheerCooldown {
            remaining_seconds: 44
        })
    ));
}

#[test]
fn sqlite_repository_preserves_unknown_progress_and_curse_fields() {
    let database = fixture();
    let mut repository = SqliteBossRepository::new(database.path(), 1_700_000_000);
    let tunnel = repository.load_tunnel(key()).expect("tunnel");
    assert!(
        tunnel
            .stinger_curse
            .contains(crate::boss_multi_tier::CurseKind::HalveNextWager)
    );
    let mut next = tunnel.clone();
    let BossProgressValue::Entry(entry) = next.boss_progress.get_mut("25").expect("boss progress")
    else {
        panic!("expected structured boss entry");
    };
    entry.hp_remaining = Some(3);
    entry.hp_max = Some(7);
    next.stinger_curse
        .consume(crate::boss_multi_tier::CurseKind::HalveNextWager);
    repository
        .commit(
            key(),
            tunnel.revision,
            RepositoryCommit {
                next_tunnel: next,
                ..RepositoryCommit::default()
            },
        )
        .expect("guarded update");

    let connection = Connection::open(database.path()).expect("verify DB");
    let (progress, curse) = connection
        .query_row(
            "SELECT boss_progress,stinger_curse FROM tunnels
              WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("persisted JSON");
    let progress: Value = serde_json::from_str(&progress).expect("progress JSON");
    assert_eq!(progress["25"]["future_field"], 17);
    assert_eq!(progress["25"]["active_prep"]["item"], "whetstone");
    assert_eq!(progress["25"]["pending_phase_event_id"], "falling_stars");
    assert_eq!(progress["25"]["hp_remaining"], 3);
    let curse: Value = serde_json::from_str(&curse).expect("curse JSON");
    assert_eq!(curse["future_curse"], 9);
    assert!(curse.get("halve_next_wager").is_none());
}

#[test]
fn final_victory_atomically_awards_stat_point_and_keeps_run_scoped_ledger() {
    let database = fixture();
    let mut repository = SqliteBossRepository::new(database.path(), 1_700_000_000);
    let tunnel = repository.load_tunnel(key()).expect("tunnel");
    let mut next = tunnel.clone();
    next.depth = 25;
    next.max_depth = 25;
    next.balance = 155;
    next.boss_attempts = 0;
    let BossProgressValue::Entry(entry) = next.boss_progress.get_mut("25").expect("boss progress")
    else {
        panic!("expected structured boss entry");
    };
    entry.status = BossStatus::Defeated;
    repository
        .commit(
            key(),
            tunnel.revision,
            RepositoryCommit {
                next_tunnel: next,
                audit: Some(BossAuditRecord {
                    boss_id: "grothak".to_owned(),
                    boundary: 25,
                    won: true,
                    jc_delta: 55,
                    depth_after: 25,
                }),
                echo: Some(EchoRecord {
                    boss_id: "grothak".to_owned(),
                    boundary: 25,
                    killer_id: PLAYER,
                    weakened_until: 1_700_086_400,
                }),
                vanity_tax: 10,
                preparation_mutation: crate::boss_multi_tier::BossPreparationMutation::Clear {
                    boundary: 25,
                },
            },
        )
        .expect("victory");
    assert!(repository.last_action_id().is_some());

    let connection = Connection::open(database.path()).expect("verify DB");
    let row = connection
        .query_row(
            "SELECT stat_points,stat_boss_awards,cheer_data,jopacoin_balance
               FROM tunnels JOIN players USING(discord_id,guild_id)
              WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .expect("victory state");
    assert_eq!(row.0, 6);
    assert_eq!(row.2, None);
    assert_eq!(row.3, 155);
    let awards: Value = serde_json::from_str(&row.1).expect("awards JSON");
    assert_eq!(awards["prestige_level"], 0);
    assert_eq!(awards["awards"], json!([25]));
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries
                  WHERE account_id=?1 AND guild_id=?2 AND source='vanity_tax' AND delta=-10",
                params![PLAYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("tax ledger"),
        1
    );
}

fn paused_duel() -> PausedBossDuel {
    PausedBossDuel {
        boss_id: "grothak".to_owned(),
        boundary: 25,
        mechanic_id: "grothak_earthquake".to_owned(),
        risk_tier: RiskTier::Bold,
        wager: 20,
        combat: CombatState {
            player_hp: 3,
            boss_hp: 4,
            player_hit: 0.42,
            player_damage: 2,
            boss_hit: 0.45,
            boss_damage: 1,
            critical_chance: 0.15,
            critical_bonus: 1,
            effects: CombatEffects {
                burn_rounds_remaining: 2,
                boss_exposed_next_round: true,
                trophy_start_hp: Some(5),
                trophy_venom_rounds: 3,
                trophy_lifesteal: true,
                trophy_regrowth: true,
                trophy_forewarned: true,
                trophy_laststand: true,
                relic_berserk_rage: Some(2),
                relic_double_hit: true,
                relic_deaths_door: true,
                relic_paper_crane: true,
                relic_bottled_quake: true,
                gear_reflect_first_hit: true,
                gear_block_first_status: true,
                gear_springheel_counter: true,
                gear_block_first_skip: true,
                gear_heal_first_crit: true,
                gear_reroll_first_miss: true,
                paper_crane_blocked: Some(crate::boss_multi_tier::MechanicStatusEffect::Burn),
                nullweave_blocked: Some(crate::boss_multi_tier::MechanicStatusEffect::Silence),
                anchor_boots_blocked: true,
                ..CombatEffects::default()
            },
        },
        round_num: 2,
        round_log: vec![crate::boss_multi_tier::RoundRecord {
            round: 1,
            player_hp: 3,
            boss_hp: 4,
            player_hit: true,
            boss_hit: Some(false),
            pet_assist: None,
            pet_assist_damage: 0,
            effect_log: crate::boss_multi_tier::RoundEffectLog {
                critical: true,
                venom: true,
                deaths_door: true,
                bottled_quake: true,
                berserk_bonus: 2,
                laststand: true,
                double_hit: true,
                blood_locket_heal: true,
                lifesteal: true,
                forewarned: true,
                briarplate_reflect: true,
                springheel_counter: true,
                red_thread_reroll: true,
                regrowth: true,
                skipped_player: true,
                skipped_boss: true,
            },
            mechanic: Some(crate::boss_multi_tier::MechanicRoundRecord {
                mechanic_id: "grothak_earthquake".to_owned(),
                option_index: 0,
                option_label: "Brace".to_owned(),
                narrative: "You weather the quake.".to_owned(),
                warding_salts_blocked: false,
            }),
        }],
        pending_prompt: PendingPrompt {
            mechanic_id: "grothak_earthquake".to_owned(),
            prompt_title: "The Floor Breaks".to_owned(),
            prompt_description: "Choose quickly.".to_owned(),
            options: vec![PromptOption {
                option_index: 0,
                label: "Brace".to_owned(),
            }],
            safe_option_index: 0,
        },
        attempts_this_fight: 2,
        initial_win_chance: 0.43,
        payout_multiplier: 2.4,
        player_hp_max: 5,
        boss_hp_max: 7,
        starting_boss_hp: 7,
        gear_snapshot: GearSnapshot {
            gear_ids: vec![11, 12],
            armor_id: Some(12),
        },
        forced_no_wager_phase: false,
        echo_applied: true,
        echo_killer_id: Some(99),
    }
}

#[test]
fn paused_duel_round_trip_survives_adapter_restart_and_claims_once() {
    let database = fixture();
    SqliteBossRepository::new(database.path(), 1_700_000_000)
        .save_active_duel(key(), paused_duel())
        .expect("save duel");
    let mut restarted = SqliteBossRepository::new(database.path(), 1_700_000_100);
    assert_eq!(restarted.active_duel(key()), Some(paused_duel()));
    assert_eq!(
        restarted.claim_active_duel(key()).expect("claim"),
        Some(paused_duel())
    );
    assert_eq!(restarted.claim_active_duel(key()).expect("retry"), None);
}

#[test]
fn scout_lantern_claim_is_separate_and_exactly_once() {
    let database = fixture();
    let connection = Connection::open(database.path()).expect("inventory DB");
    connection
        .execute(
            "INSERT INTO dig_inventory
             (discord_id,guild_id,item_type,queued,created_at)
             VALUES (?1,?2,'lantern',0,100)",
            params![PLAYER, GUILD],
        )
        .expect("lantern");
    let mut repository = SqliteBossRepository::new(database.path(), 1_700_000_000);
    let tunnel = repository.load_tunnel(key()).expect("tunnel");
    assert_eq!(tunnel.lanterns, 1);
    let mut next = tunnel.clone();
    next.lanterns = 0;
    repository
        .commit(
            key(),
            tunnel.revision,
            RepositoryCommit {
                next_tunnel: next,
                ..RepositoryCommit::default()
            },
        )
        .expect("scout commit");
    assert_eq!(
        Connection::open(database.path())
            .expect("verify DB")
            .query_row(
                "SELECT COUNT(*) FROM dig_inventory
                  WHERE discord_id=?1 AND guild_id=?2 AND item_type='lantern'",
                params![PLAYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("lantern count"),
        0
    );
}

#[test]
fn gear_adapter_ticks_the_snapshotted_loadout() {
    let database = fixture();
    let connection = Connection::open(database.path()).expect("gear DB");
    connection
        .execute(
            "INSERT INTO dig_gear
             (discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source)
             VALUES (?1,?2,'weapon',1,2,1,100,'fixture')",
            params![PLAYER, GUILD],
        )
        .expect("weapon");
    let gear_id = connection.last_insert_rowid();
    let mut adapter = SqliteBossGearRepair::new(database.path());
    let snapshot = adapter.snapshot(key());
    assert_eq!(snapshot.gear_ids, vec![gear_id]);
    assert_eq!(
        adapter.tick_resolved_fight(key(), &snapshot, true),
        GearWearResult {
            broken_ids: Vec::new()
        }
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT durability FROM dig_gear WHERE id=?1",
                [gear_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("durability"),
        1
    );
}

#[test]
fn production_adapter_runs_policy_to_a_durable_mechanic_pause() {
    let database = fixture();
    let repository = SqliteBossRepository::new(database.path(), 1_700_000_000);
    let gear = SqliteBossGearRepair::new(database.path());
    let mut service = crate::boss_multi_tier::BossMultiTierService::new(
        repository,
        SequenceEntropy::new(vec![0.99], vec![0, 0], vec![11]),
        crate::boss_multi_tier::FixedClock {
            unix_seconds: 1_700_000_000,
        },
        crate::boss_multi_tier::NoEconomyEvent,
        crate::boss_multi_tier::NoBankruptcy,
        gear,
        crate::boss_multi_tier::InMemoryBossOutcomeSink::default(),
    );
    let outcome = service
        .start_boss_duel(key(), RiskTier::Bold, 0)
        .expect("start duel");
    let StartBossDuelOutcome::Paused(paused) = outcome else {
        panic!("expected authored mechanic pause");
    };
    let restarted = SqliteBossRepository::new(database.path(), 1_700_000_001);
    assert_eq!(restarted.active_duel(key()), Some(*paused));
}

#[test]
fn phase_event_is_consumed_before_a_pause_and_stays_consumed_after_restart() {
    let database = fixture();
    set_phase_two(&database, "void_pull");
    let mut service = crate::boss_multi_tier::BossMultiTierService::new(
        SqliteBossRepository::new(database.path(), 1_700_000_000),
        SequenceEntropy::constant(0.99),
        crate::boss_multi_tier::FixedClock {
            unix_seconds: 1_700_000_000,
        },
        crate::boss_multi_tier::NoEconomyEvent,
        crate::boss_multi_tier::NoBankruptcy,
        SqliteBossGearRepair::new(database.path()),
        crate::boss_multi_tier::InMemoryBossOutcomeSink::default(),
    );
    let outcome = service
        .start_boss_duel(key(), RiskTier::Cautious, 10)
        .expect("start phase two");
    let StartBossDuelOutcome::Paused(paused) = outcome else {
        panic!("expected authored mechanic pause");
    };
    assert_eq!(pending_phase_event(&database), None);
    assert_eq!(paused.combat.player_hp, 3);
    let restarted = SqliteBossRepository::new(database.path(), 1_700_000_001);
    assert_eq!(restarted.active_duel(key()), Some(*paused));
    assert_eq!(pending_phase_event(&database), None);
}

#[test]
fn direct_fight_phase_event_consumption_rolls_back_with_failed_settlement() {
    let database = fixture();
    set_phase_two(&database, "void_pull");
    let connection = Connection::open(database.path()).expect("trigger DB");
    connection
        .execute_batch(
            "CREATE TRIGGER fail_boss_audit BEFORE INSERT ON dig_actions
             WHEN NEW.action_type='boss_fight'
             BEGIN SELECT RAISE(ABORT,'injected boss audit failure'); END;",
        )
        .expect("failure trigger");
    let combat = CombatState {
        player_hp: 5,
        boss_hp: 1,
        player_hit: 1.0,
        player_damage: 9,
        boss_hit: 0.0,
        boss_damage: 1,
        critical_chance: 0.0,
        critical_bonus: 0,
        effects: CombatEffects::default(),
    };
    let mut failed = crate::boss_multi_tier::BossMultiTierService::new(
        SqliteBossRepository::new(database.path(), 1_700_000_000),
        SequenceEntropy::constant(0.0),
        crate::boss_multi_tier::FixedClock {
            unix_seconds: 1_700_000_000,
        },
        crate::boss_multi_tier::NoEconomyEvent,
        crate::boss_multi_tier::NoBankruptcy,
        SqliteBossGearRepair::new(database.path()),
        crate::boss_multi_tier::InMemoryBossOutcomeSink::default(),
    );
    assert!(
        failed
            .fight_boss_with_override(key(), RiskTier::Cautious, 10, Some(combat.clone()))
            .is_err()
    );
    assert_eq!(pending_phase_event(&database).as_deref(), Some("void_pull"));

    connection
        .execute_batch("DROP TRIGGER fail_boss_audit;")
        .expect("drop trigger");
    let mut retry = crate::boss_multi_tier::BossMultiTierService::new(
        SqliteBossRepository::new(database.path(), 1_700_000_001),
        SequenceEntropy::constant(0.0),
        crate::boss_multi_tier::FixedClock {
            unix_seconds: 1_700_000_001,
        },
        crate::boss_multi_tier::NoEconomyEvent,
        crate::boss_multi_tier::NoBankruptcy,
        SqliteBossGearRepair::new(database.path()),
        crate::boss_multi_tier::InMemoryBossOutcomeSink::default(),
    );
    assert!(
        retry
            .fight_boss_with_override(key(), RiskTier::Cautious, 10, Some(combat))
            .expect("retry settlement")
            .won
    );
    assert_eq!(pending_phase_event(&database), None);
}

#[test]
fn combat_preparation_composes_gear_mana_light_cheers_relics_pet_and_prep() {
    let database = fixture();
    let now = 1_700_000_000;
    let today = cama_domain::game_date::game_date_for_timestamp(now as f64).expect("game date");
    let connection = Connection::open(database.path()).expect("combat fixture DB");
    connection
        .execute(
            "UPDATE tunnels
                SET luminosity=0,
                    cheer_data=?1,
                    boss_progress=?2
              WHERE discord_id=?3 AND guild_id=?4",
            params![
                format!(
                    r#"[{{"user_id":7,"expires_at":{}}},{{"user_id":8,"expires_at":{}}}]"#,
                    now + 10,
                    now - 10
                ),
                r#"{"25":{"boss_id":"grothak","status":"active","active_prep":{"item_type":"tempered_whetstone","used":false}}}"#,
                PLAYER,
                GUILD
            ],
        )
        .expect("combat tunnel");
    connection
        .execute(
            "INSERT INTO dig_gear
             (discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source)
             VALUES (?1,?2,'amulet',7,20,1,100,'fixture')",
            params![PLAYER, GUILD],
        )
        .expect("amulet");
    connection
        .execute(
            "INSERT INTO dig_gear
             (discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source,item_id)
             VALUES (?1,?2,'armor',3,14,1,101,'event:test','briarplate'),
                    (?1,?2,'boots',3,14,1,102,'event:test','springheel_boots'),
                    (?1,?2,'ring',3,14,1,103,'event:test','red_thread_band')",
            params![PLAYER, GUILD],
        )
        .expect("unique gear");
    connection
        .execute(
            "INSERT INTO dig_artifacts
             (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,'hollow_fang',100,1,1),
                    (?1,?2,'shifting_idol',101,1,1),
                    (?1,?2,'weeping_fang',102,1,1),
                    (?1,?2,'deaths_door',103,1,1),
                    (?1,?2,'paper_crane',104,1,1),
                    (?1,?2,'bottled_quake',105,1,1)",
            params![PLAYER, GUILD],
        )
        .expect("relics");
    connection
        .execute(
            "INSERT INTO player_mana
             (discord_id,guild_id,current_land,assigned_date,consumed_today)
             VALUES (?1,?2,'Plains',?3,0)",
            params![PLAYER, GUILD, today],
        )
        .expect("mana");
    connection
        .execute(
            "INSERT INTO pets
             (discord_id,guild_id,name,species,adopted_at,hatched_at,adopt_fee,
              last_fed_at,hunger_at_last_fed,training_xp)
             VALUES (?1,?2,'Blep','common_cama',?3,?3,0,?4,100,20)",
            params![PLAYER, GUILD, now - 8 * 86_400, now],
        )
        .expect("adult trained pet");

    let repository = SqliteBossRepository::new(database.path(), now);
    let tunnel = repository.load_tunnel(key()).expect("tunnel");
    let base = CombatState {
        player_hp: 5,
        boss_hp: 4,
        player_hit: 0.50,
        player_damage: 4,
        boss_hit: 0.40,
        boss_damage: 1,
        critical_chance: 0.0,
        critical_bonus: 0,
        effects: CombatEffects::default(),
    };
    let adapter = SqliteBossCombatPreparation::new(database.path(), 20);
    let prepared = adapter.prepare(
        BossCombatPreparationRequest {
            key: key(),
            tunnel: &tunnel,
            boss: crate::dig_bosses::boss_by_id("grothak").expect("boss"),
            boundary: 25,
            risk_tier: RiskTier::Cautious,
            wager: 10,
            echo_applied: false,
            now,
        },
        base,
    );
    assert_eq!(prepared.base_combat.critical_chance, 0.10);
    assert_eq!(prepared.base_combat.critical_bonus, 1);
    assert!((prepared.player_hit_offset - -0.10).abs() < f64::EPSILON);
    assert!((prepared.player_damage_multiplier - 1.38).abs() < 1e-9);
    assert_eq!(prepared.player_damage_after_phase_delta, 1);
    assert!(prepared.shifting_idol);
    assert_eq!(prepared.base_combat.effects.trophy_venom_rounds, 4);
    assert!(prepared.base_combat.effects.relic_deaths_door);
    assert!(prepared.base_combat.effects.relic_paper_crane);
    assert!(prepared.base_combat.effects.relic_bottled_quake);
    assert!(prepared.base_combat.effects.gear_reflect_first_hit);
    assert!(prepared.base_combat.effects.gear_springheel_counter);
    assert!(prepared.base_combat.effects.gear_reroll_first_miss);
    assert_eq!(
        prepared
            .pet_assist
            .as_ref()
            .map(|assist| assist.bonus_percent),
        Some(12)
    );
    assert_eq!(adapter.take_error(), None);
}

#[test]
fn regular_boss_fresh_attempt_applies_shared_shifting_idol_policy() {
    let database = fixture();
    let connection = Connection::open(database.path()).expect("Shifting Idol DB");
    connection
        .execute(
            "UPDATE tunnels SET boss_progress=?1
              WHERE discord_id=?2 AND guild_id=?3",
            params![
                r#"{"25":{"boss_id":"grothak","status":"active"}}"#,
                PLAYER,
                GUILD
            ],
        )
        .expect("fresh boss progress");
    connection
        .execute(
            "INSERT INTO dig_artifacts
             (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,'shifting_idol',100,1,1)",
            params![PLAYER, GUILD],
        )
        .expect("equip Shifting Idol");
    let result = pinnacle_runtime(&database)
        .start_regular(
            pinnacle_request(1_700_000_000),
            RiskTier::Cautious,
            0,
            SequenceEntropy::constant(0.99),
        )
        .expect("regular boss start");
    let StartBossDuelOutcome::Paused(paused) = result.outcome else {
        panic!("expected durable regular-boss prompt");
    };
    assert_eq!(
        paused.combat.effects.shifting_idol_bonus.as_deref(),
        Some("hp")
    );
}

// tests/test_dig_boss_prep.py::test_arming_boss_prep_consumes_exact_queued_row_atomically
#[test]
fn runtime_activates_queued_prep_only_after_validation_and_reuses_it_for_pause() {
    let database = fixture();
    let connection = Connection::open(database.path()).expect("prep DB");
    connection
        .execute(
            "UPDATE tunnels SET boss_progress=?1
              WHERE discord_id=?2 AND guild_id=?3",
            params![
                r#"{"25":{"boss_id":"grothak","status":"active","future_field":17}}"#,
                PLAYER,
                GUILD
            ],
        )
        .expect("clear active prep");
    connection
        .execute(
            "INSERT INTO dig_inventory
             (discord_id,guild_id,item_type,queued,created_at)
             VALUES (?1,?2,'tempered_whetstone',1,100)",
            params![PLAYER, GUILD],
        )
        .expect("queued prep");
    let item_id = connection.last_insert_rowid();
    let mut config = DigBossRuntimeConfig::new(database.path(), 20);
    config.economy_event.enabled = false;
    let runtime = DigBossRuntimeService::sqlite(config);
    assert!(matches!(
        runtime.start_regular(
            DigBossRuntimeRequest {
                discord_id: PLAYER,
                guild_id: GUILD,
                now: 1_700_000_000,
            },
            RiskTier::Cautious,
            -1,
            SequenceEntropy::constant(0.99),
        ),
        Err(DigBossRuntimeError::Policy(
            crate::boss_multi_tier::BossServiceError::NegativeWager
        ))
    ));
    assert_eq!(
        connection
            .query_row(
                "SELECT queued FROM dig_inventory WHERE id=?1",
                [item_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("still queued"),
        1
    );

    let result = runtime
        .start_regular(
            DigBossRuntimeRequest {
                discord_id: PLAYER,
                guild_id: GUILD,
                now: 1_700_000_001,
            },
            RiskTier::Cautious,
            0,
            SequenceEntropy::constant(0.99),
        )
        .expect("valid start");
    let StartBossDuelOutcome::Paused(paused) = result.outcome else {
        panic!("expected mechanic pause");
    };
    assert_eq!(paused.combat.player_damage, 2);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_inventory WHERE id=?1",
                [item_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("consumed prep"),
        0
    );
    let progress = connection
        .query_row(
            "SELECT boss_progress FROM tunnels
              WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
            |row| row.get::<_, String>(0),
        )
        .expect("boss progress");
    let progress: Value = serde_json::from_str(&progress).expect("progress JSON");
    assert_eq!(
        progress["25"]["active_prep"],
        json!({"item_type":"tempered_whetstone","used":false})
    );
    assert_eq!(progress["25"]["future_field"], 17);
}

// tests/test_dig_boss_prep.py::test_whetstone_adds_exactly_one_after_damage_multipliers
#[test]
fn whetstone_adds_one_after_the_prepared_damage_value() {
    let database = fixture();
    Connection::open(database.path())
        .expect("whetstone helper DB")
        .execute(
            "UPDATE tunnels SET boss_progress=?1
              WHERE discord_id=?2 AND guild_id=?3",
            params![
                r#"{"25":{"boss_id":"grothak","status":"active","active_prep":{"item_type":"tempered_whetstone","used":false}}}"#,
                PLAYER,
                GUILD
            ],
        )
        .expect("active whetstone");
    let repository = SqliteBossRepository::new(database.path(), 1_700_000_000);
    let tunnel = repository.load_tunnel(key()).expect("tunnel");
    let prepared = SqliteBossCombatPreparation::new(database.path(), 20).prepare(
        BossCombatPreparationRequest {
            key: key(),
            tunnel: &tunnel,
            boss: crate::dig_bosses::boss_by_id("grothak").expect("boss"),
            boundary: 25,
            risk_tier: RiskTier::Reckless,
            wager: 0,
            echo_applied: false,
            now: 1_700_000_000,
        },
        CombatState {
            player_hp: 5,
            boss_hp: 8,
            player_hit: 0.5,
            player_damage: 4,
            boss_hit: 0.4,
            boss_damage: 1,
            critical_chance: 0.0,
            critical_bonus: 0,
            effects: CombatEffects::default(),
        },
    );
    let applied_damage = prepared
        .base_combat
        .player_damage
        .saturating_add(prepared.player_damage_after_phase_delta);
    assert_eq!(prepared.base_combat.player_damage, 4);
    assert_eq!(applied_damage, 5);
}

// tests/test_dig_boss_prep.py::test_existing_active_prep_is_reused_across_phases
#[test]
fn existing_active_prep_is_reused_and_does_not_consume_a_new_queue_row() {
    let database = fixture();
    let connection = Connection::open(database.path()).expect("reuse prep DB");
    connection
        .execute(
            "UPDATE tunnels SET boss_progress=?1
              WHERE discord_id=?2 AND guild_id=?3",
            params![
                r#"{"25":{"boss_id":"grothak","status":"phase1_defeated","active_prep":{"item_type":"rescue_line","used":false}}}"#,
                PLAYER,
                GUILD
            ],
        )
        .expect("active preparation");
    connection
        .execute(
            "INSERT INTO dig_inventory
             (discord_id,guild_id,item_type,queued,created_at)
             VALUES (?1,?2,'tempered_whetstone',1,101)",
            params![PLAYER, GUILD],
        )
        .expect("queued replacement preparation");
    let queued_id = connection.last_insert_rowid();
    drop(connection);

    let mut config = DigBossRuntimeConfig::new(database.path(), 20);
    config.economy_event.enabled = false;
    let result = DigBossRuntimeService::sqlite(config)
        .start_regular(
            pinnacle_request(1_700_000_000),
            RiskTier::Cautious,
            0,
            SequenceEntropy::constant(0.99),
        )
        .expect("reuse active preparation");
    let StartBossDuelOutcome::Paused(paused) = result.outcome else {
        panic!("expected authored preparation pause")
    };
    assert_eq!(
        paused
            .combat
            .effects
            .boss_preparation
            .as_ref()
            .map(|preparation| preparation.item_type.as_str()),
        Some("rescue_line")
    );
    assert_eq!(
        Connection::open(database.path())
            .expect("verify queued preparation")
            .query_row(
                "SELECT queued FROM dig_inventory WHERE id=?1",
                [queued_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("queued row"),
        1,
    );
}

// tests/test_dig_boss_prep.py::test_boss_prep_effect_helpers
#[test]
fn boss_prep_effect_helpers_are_applied_by_the_typed_combat_preparation() {
    let database = fixture();
    let connection = Connection::open(database.path()).expect("prep helper DB");
    connection
        .execute(
            "UPDATE tunnels SET boss_progress=?1
              WHERE discord_id=?2 AND guild_id=?3",
            params![
                r#"{"25":{"boss_id":"grothak","status":"active","active_prep":{"item_type":"tempered_whetstone","used":false}}}"#,
                PLAYER,
                GUILD
            ],
        )
        .expect("whetstone preparation");
    drop(connection);

    let repository = SqliteBossRepository::new(database.path(), 1_700_000_000);
    let tunnel = repository.load_tunnel(key()).expect("tunnel");
    let adapter = SqliteBossCombatPreparation::new(database.path(), 20);
    let prepared = adapter.prepare(
        BossCombatPreparationRequest {
            key: key(),
            tunnel: &tunnel,
            boss: crate::dig_bosses::boss_by_id("grothak").expect("boss"),
            boundary: 25,
            risk_tier: RiskTier::Cautious,
            wager: 0,
            echo_applied: false,
            now: 1_700_000_000,
        },
        CombatState {
            player_hp: 5,
            boss_hp: 8,
            player_hit: 0.5,
            player_damage: 3,
            boss_hit: 0.4,
            boss_damage: 1,
            critical_chance: 0.0,
            critical_bonus: 0,
            effects: CombatEffects::default(),
        },
    );
    assert_eq!(prepared.base_combat.player_damage, 3);
    assert_eq!(prepared.player_damage_after_phase_delta, 1);
    assert_eq!(
        prepared
            .boss_preparation
            .as_ref()
            .map(|preparation| preparation.item_type.as_str()),
        Some("tempered_whetstone")
    );

    let rescue_database = fixture();
    let rescue_repository = SqliteBossRepository::new(rescue_database.path(), 1_700_000_000);
    let rescue_tunnel = rescue_repository.load_tunnel(key()).expect("rescue tunnel");
    let rescue_connection = Connection::open(rescue_database.path()).expect("rescue prep DB");
    rescue_connection
        .execute(
            "UPDATE tunnels SET boss_progress=?1
              WHERE discord_id=?2 AND guild_id=?3",
            params![
                r#"{"25":{"boss_id":"grothak","status":"active","active_prep":{"item_type":"rescue_line","used":false}}}"#,
                PLAYER,
                GUILD
            ],
        )
        .expect("rescue preparation");
    drop(rescue_connection);
    let rescue_prepared = SqliteBossCombatPreparation::new(rescue_database.path(), 20).prepare(
        BossCombatPreparationRequest {
            key: key(),
            tunnel: &rescue_tunnel,
            boss: crate::dig_bosses::boss_by_id("grothak").expect("boss"),
            boundary: 25,
            risk_tier: RiskTier::Cautious,
            wager: 0,
            echo_applied: false,
            now: 1_700_000_000,
        },
        CombatState {
            player_hp: 5,
            boss_hp: 8,
            player_hit: 0.5,
            player_damage: 1,
            boss_hit: 0.4,
            boss_damage: 1,
            critical_chance: 0.0,
            critical_bonus: 0,
            effects: CombatEffects::default(),
        },
    );
    assert_eq!(rescue_prepared.player_damage_after_phase_delta, 0);
    assert_eq!(
        rescue_prepared
            .boss_preparation
            .as_ref()
            .map(|preparation| preparation.item_type.as_str()),
        Some("rescue_line")
    );
}

#[test]
fn rescue_line_halves_loss_knockback_skips_extra_armor_tick_and_clears_attempt() {
    let database = fixture();
    let connection = Connection::open(database.path()).expect("rescue DB");
    connection
        .execute(
            "UPDATE tunnels SET boss_progress=?1
              WHERE discord_id=?2 AND guild_id=?3",
            params![
                r#"{"25":{"boss_id":"grothak","status":"active","future_field":17}}"#,
                PLAYER,
                GUILD
            ],
        )
        .expect("clear active prep");
    connection
        .execute(
            "INSERT INTO dig_inventory
             (discord_id,guild_id,item_type,queued,created_at)
             VALUES (?1,?2,'rescue_line',1,100)",
            params![PLAYER, GUILD],
        )
        .expect("queued rescue line");
    connection
        .execute(
            "INSERT INTO dig_gear
             (discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source)
             VALUES (?1,?2,'armor',1,3,1,100,'fixture')",
            params![PLAYER, GUILD],
        )
        .expect("armor");
    let armor_id = connection.last_insert_rowid();
    let mut config = DigBossRuntimeConfig::new(database.path(), 20);
    config.economy_event.enabled = false;
    let result = DigBossRuntimeService::sqlite(config)
        .fight_regular(
            DigBossRuntimeRequest {
                discord_id: PLAYER,
                guild_id: GUILD,
                now: 1_700_000_000,
            },
            RiskTier::Cautious,
            0,
            SequenceEntropy::constant(0.99),
        )
        .expect("resolved loss");
    assert!(!result.outcome.won);
    assert!(result.outcome.rescue_line_used);
    assert_eq!(result.outcome.new_depth, 16);
    assert_eq!(
        result
            .outcome
            .boss_preparation
            .as_ref()
            .map(|preparation| preparation.item_type.as_str()),
        Some("rescue_line")
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT durability FROM dig_gear WHERE id=?1",
                [armor_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("armor durability"),
        2
    );
    assert_eq!(pending_phase_event(&database), None);
    let progress = connection
        .query_row(
            "SELECT boss_progress FROM tunnels
              WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
            |row| row.get::<_, String>(0),
        )
        .expect("boss progress");
    let progress: Value = serde_json::from_str(&progress).expect("progress JSON");
    assert!(progress["25"].get("active_prep").is_none());
    assert_eq!(progress["25"]["future_field"], 17);
}

// tests/test_dig_boss_prep.py::test_warding_salts_block_only_first_mechanic
#[test]
fn warding_salts_survive_pause_block_one_mechanic_and_clear_on_resolution() {
    let database = fixture();
    let connection = Connection::open(database.path()).expect("warding DB");
    connection
        .execute(
            "UPDATE tunnels SET boss_progress=?1
              WHERE discord_id=?2 AND guild_id=?3",
            params![
                r#"{"25":{"boss_id":"grothak","status":"active","future_field":17}}"#,
                PLAYER,
                GUILD
            ],
        )
        .expect("clear active prep");
    connection
        .execute(
            "INSERT INTO dig_inventory
             (discord_id,guild_id,item_type,queued,created_at)
             VALUES (?1,?2,'warding_salts',1,100)",
            params![PLAYER, GUILD],
        )
        .expect("queued salts");
    let mut config = DigBossRuntimeConfig::new(database.path(), 20);
    config.economy_event.enabled = false;
    let runtime = DigBossRuntimeService::sqlite(config.clone());
    let started = runtime
        .start_regular(
            DigBossRuntimeRequest {
                discord_id: PLAYER,
                guild_id: GUILD,
                now: 1_700_000_000,
            },
            RiskTier::Cautious,
            0,
            SequenceEntropy::constant(0.99),
        )
        .expect("start duel");
    let StartBossDuelOutcome::Paused(paused) = started.outcome else {
        panic!("expected mechanic pause");
    };
    assert_eq!(
        paused
            .combat
            .effects
            .boss_preparation
            .as_ref()
            .map(|preparation| (preparation.item_type.as_str(), preparation.used)),
        Some(("warding_salts", false))
    );

    let resolved = DigBossRuntimeService::sqlite(config)
        .resume_regular(
            DigBossRuntimeRequest {
                discord_id: PLAYER,
                guild_id: GUILD,
                now: 1_700_000_001,
            },
            0,
            SequenceEntropy::constant(0.0),
        )
        .expect("resume duel");
    assert!(resolved.outcome.warding_salts_blocked);
    assert_eq!(
        resolved
            .outcome
            .boss_preparation
            .as_ref()
            .map(|preparation| preparation.used),
        Some(true)
    );
    let progress = connection
        .query_row(
            "SELECT boss_progress FROM tunnels
              WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
            |row| row.get::<_, String>(0),
        )
        .expect("boss progress");
    let progress: Value = serde_json::from_str(&progress).expect("progress JSON");
    assert!(progress["25"].get("active_prep").is_none());
}

#[test]
fn abandoned_pause_ticks_its_snapshot_piece_before_replacement_fight() {
    let database = fixture();
    let connection = Connection::open(database.path()).expect("stale gear DB");
    connection
        .execute(
            "INSERT INTO dig_gear
             (discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source)
             VALUES (?1,?2,'armor',1,1,0,100,'stale-snapshot')",
            params![PLAYER, GUILD],
        )
        .expect("snapshot armor");
    let armor_id = connection.last_insert_rowid();
    let mut stale = paused_duel();
    stale.wager = 0;
    stale.gear_snapshot = GearSnapshot {
        gear_ids: vec![armor_id],
        armor_id: Some(armor_id),
    };
    SqliteBossRepository::new(database.path(), 1_700_000_000)
        .save_active_duel(key(), stale)
        .expect("stale pause");
    let mut config = DigBossRuntimeConfig::new(database.path(), 20);
    config.economy_event.enabled = false;
    let result = DigBossRuntimeService::sqlite(config)
        .start_regular(
            DigBossRuntimeRequest {
                discord_id: PLAYER,
                guild_id: GUILD,
                now: 1_700_000_001,
            },
            RiskTier::Cautious,
            0,
            SequenceEntropy::constant(0.99),
        )
        .expect("replacement fight");
    assert_eq!(result.abandoned_gear_wear.broken_ids, vec![armor_id]);
    assert_eq!(result.abandoned_wager_forfeit, 0);
    assert_eq!(result.abandoned_action_id, None);
    assert_eq!(
        connection
            .query_row(
                "SELECT durability FROM dig_gear WHERE id=?1",
                [armor_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("stale durability"),
        0
    );
}

#[test]
fn legacy_abandoned_pause_without_snapshot_ticks_live_loadout_once() {
    let database = fixture();
    let connection = Connection::open(database.path()).expect("legacy stale DB");
    connection
        .execute(
            "INSERT INTO dig_gear
             (discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source)
             VALUES (?1,?2,'armor',1,2,1,100,'live-fallback')",
            params![PLAYER, GUILD],
        )
        .expect("live armor");
    let armor_id = connection.last_insert_rowid();
    let mut stale = paused_duel();
    stale.wager = 0;
    stale.gear_snapshot = GearSnapshot::default();
    SqliteBossRepository::new(database.path(), 1_700_000_000)
        .save_active_duel(key(), stale)
        .expect("legacy stale pause");
    let mut config = DigBossRuntimeConfig::new(database.path(), 20);
    config.economy_event.enabled = false;
    let result = DigBossRuntimeService::sqlite(config)
        .start_regular(
            DigBossRuntimeRequest {
                discord_id: PLAYER,
                guild_id: GUILD,
                now: 1_700_000_001,
            },
            RiskTier::Cautious,
            0,
            SequenceEntropy::constant(0.99),
        )
        .expect("replacement fight");
    assert!(result.abandoned_gear_wear.broken_ids.is_empty());
    assert_eq!(
        connection
            .query_row(
                "SELECT durability FROM dig_gear WHERE id=?1",
                [armor_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("live durability"),
        1
    );
}

#[test]
fn abandoned_wager_settles_once_before_replacement_validation() {
    let database = fixture();
    let connection = Connection::open(database.path()).expect("abandoned wager DB");
    connection
        .execute(
            "INSERT INTO dig_gear
             (discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source)
             VALUES (?1,?2,'armor',1,20,1,100,'validation-order')",
            params![PLAYER, GUILD],
        )
        .expect("live armor");
    let armor_id = connection.last_insert_rowid();
    let mut stale = paused_duel();
    stale.wager = 80;
    stale.gear_snapshot = GearSnapshot {
        gear_ids: vec![armor_id],
        armor_id: Some(armor_id),
    };
    SqliteBossRepository::new(database.path(), 1_700_000_000)
        .save_active_duel(key(), stale)
        .expect("stale wager pause");
    let mut config = DigBossRuntimeConfig::new(database.path(), 20);
    config.economy_event.enabled = false;
    let runtime = DigBossRuntimeService::sqlite(config);
    let request = DigBossRuntimeRequest {
        discord_id: PLAYER,
        guild_id: GUILD,
        now: 1_700_000_001,
    };
    assert!(matches!(
        runtime.start_regular(
            request,
            RiskTier::Cautious,
            50,
            SequenceEntropy::constant(0.99),
        ),
        Err(DigBossRuntimeError::Policy(
            crate::boss_multi_tier::BossServiceError::InsufficientBalance {
                wager: 25,
                balance: 20
            }
        ))
    ));
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players
                  WHERE discord_id=?1 AND guild_id=?2",
                params![PLAYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("balance"),
        20
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT durability FROM dig_gear WHERE id=?1",
                [armor_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("validation does not tick gear"),
        20
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_actions
                  WHERE actor_id=?1 AND guild_id=?2
                    AND action_type='boss_duel_abandoned' AND jc_delta=-80",
                params![PLAYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("abandoned audit"),
        1
    );
    assert!(
        SqliteBossRepository::new(database.path(), 1_700_000_002)
            .active_duel(key())
            .is_none()
    );

    assert!(
        runtime
            .start_regular(
                DigBossRuntimeRequest {
                    now: 1_700_000_002,
                    ..request
                },
                RiskTier::Cautious,
                50,
                SequenceEntropy::constant(0.99),
            )
            .is_err()
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_actions
                  WHERE actor_id=?1 AND guild_id=?2
                    AND action_type='boss_duel_abandoned'",
                params![PLAYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("one abandoned audit"),
        1
    );
}

#[test]
fn runtime_service_applies_daily_profit_policies_and_returns_typed_receipt() {
    let database = fixture();
    let vanity = Arc::new(VanityTaxService::with_rate_fraction(None, 0.10));
    vanity
        .refresh_guild(
            GUILD,
            &[VanityMember {
                discord_id: PLAYER,
                nickname: None,
            }],
            None,
        )
        .expect("vanity snapshot");
    let mut config = DigBossRuntimeConfig::new(database.path(), 20);
    config.economy_event.enabled = false;
    let runtime = DigBossRuntimeService::sqlite(config).with_vanity_tax(vanity);
    let result = runtime
        .fight_regular(
            DigBossRuntimeRequest {
                discord_id: PLAYER,
                guild_id: GUILD,
                now: 1_700_000_000,
            },
            RiskTier::Cautious,
            0,
            SequenceEntropy::constant(0.0),
        )
        .expect("fight");
    assert!(result.outcome.won);
    assert!(result.outcome.vanity_tax > 0);
    assert!(result.action_id.is_some());
    assert!(result.warnings.is_empty());
    assert_eq!(result.notices.len(), 1);
    assert_eq!(
        Connection::open(database.path())
            .expect("verify DB")
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries
                  WHERE account_id=?1 AND guild_id=?2 AND source='vanity_tax'
                    AND delta=?3",
                params![PLAYER, GUILD, -result.outcome.vanity_tax],
                |row| row.get::<_, i64>(0),
            )
            .expect("tax ledger"),
        1
    );
}

#[test]
fn runtime_scout_claims_lantern_and_restart_observes_the_consumption() {
    let database = fixture();
    Connection::open(database.path())
        .expect("inventory DB")
        .execute(
            "INSERT INTO dig_inventory
             (discord_id,guild_id,item_type,queued,created_at)
             VALUES (?1,?2,'lantern',0,100)",
            params![PLAYER, GUILD],
        )
        .expect("lantern");
    let mut config = DigBossRuntimeConfig::new(database.path(), 20);
    config.economy_event.enabled = false;
    let runtime = DigBossRuntimeService::sqlite(config.clone());
    let result = runtime
        .scout_regular(
            DigBossRuntimeRequest {
                discord_id: PLAYER,
                guild_id: GUILD,
                now: 1_700_000_000,
            },
            SequenceEntropy::constant(0.5),
        )
        .expect("scout");
    assert_eq!(result.outcome.boss_id, "grothak");
    assert!(result.warnings.is_empty());
    let restarted = DigBossRuntimeService::sqlite(config);
    let error = restarted
        .scout_regular(
            DigBossRuntimeRequest {
                discord_id: PLAYER,
                guild_id: GUILD,
                now: 1_700_000_001,
            },
            SequenceEntropy::constant(0.5),
        )
        .expect_err("lantern was durably consumed");
    assert!(matches!(
        error,
        DigBossRuntimeError::Policy(crate::boss_multi_tier::BossServiceError::LanternRequired)
    ));
}

#[test]
fn pinnacle_phase_event_changes_hp_and_is_consumed_before_restart_pause() {
    let with_event = fixture();
    set_pinnacle(&with_event, 2, "phase1_defeated", Some("void_pull"));
    let control = fixture();
    set_pinnacle(&control, 2, "phase1_defeated", None);
    let mut event_entropy = SequenceEntropy::constant(0.99);
    let event_attempt = pinnacle_runtime(&with_event)
        .prepare_pinnacle_attempt(
            pinnacle_request(1_700_000_000),
            RiskTier::Cautious,
            0,
            &mut event_entropy,
        )
        .expect("event attempt");
    let mut control_entropy = SequenceEntropy::constant(0.99);
    let control_attempt = pinnacle_runtime(&control)
        .prepare_pinnacle_attempt(
            pinnacle_request(1_700_000_000),
            RiskTier::Cautious,
            0,
            &mut control_entropy,
        )
        .expect("control attempt");
    assert_eq!(event_attempt.boss_hp_max, control_attempt.boss_hp_max - 2);

    let start = pinnacle_runtime(&with_event)
        .run_pinnacle_attempt(
            pinnacle_request(1_700_000_000),
            event_attempt,
            &mut SequenceEntropy::constant(0.99),
        )
        .expect("pinnacle start");
    assert!(matches!(start.outcome, DigPinnacleStartOutcome::Paused(_)));
    let progress = Connection::open(with_event.path())
        .expect("event DB")
        .query_row(
            "SELECT boss_progress FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
            |row| row.get::<_, String>(0),
        )
        .expect("progress");
    assert!(
        serde_json::from_str::<Value>(&progress).expect("JSON")[PINNACLE_DEPTH.to_string()]
            .get("pending_phase_event_id")
            .is_none()
    );
    assert!(
        SqliteBossRepository::new(with_event.path(), 1_700_000_001)
            .active_duel(key())
            .is_some()
    );
}

// tests/test_dig_new_relic_effects.py::test_shifting_idol_rolls_a_fresh_attempt_bonus
#[test]
fn pinnacle_fresh_attempt_rolls_all_shared_shifting_idol_bonuses() {
    let prepare = |idol_choice: Option<usize>| {
        let database = fixture();
        set_pinnacle(&database, 1, "active", None);
        if idol_choice.is_some() {
            Connection::open(database.path())
                .expect("Shifting Idol DB")
                .execute(
                    "INSERT INTO dig_artifacts
                     (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
                     VALUES (?1,?2,'shifting_idol',100,1,1)",
                    params![PLAYER, GUILD],
                )
                .expect("equip Shifting Idol");
        }
        let mut entropy = SequenceEntropy::new(
            Vec::new(),
            idol_choice.map_or_else(|| vec![0, 0], |choice| vec![0, choice, 0]),
            Vec::new(),
        );
        pinnacle_runtime(&database)
            .prepare_pinnacle_attempt(
                pinnacle_request(1_700_000_000),
                RiskTier::Cautious,
                0,
                &mut entropy,
            )
            .expect("fresh pinnacle attempt")
            .combat
    };

    let control = prepare(None);
    let hp = prepare(Some(0));
    assert_eq!(hp.player_hp, control.player_hp + 1);
    assert_eq!(hp.player_hit, control.player_hit);
    assert_eq!(hp.critical_chance, control.critical_chance);
    assert_eq!(hp.effects.shifting_idol_bonus.as_deref(), Some("hp"));

    let hit = prepare(Some(1));
    assert_eq!(hit.player_hp, control.player_hp);
    assert!((hit.player_hit - (control.player_hit + 0.05).min(PLAYER_HIT_CEILING)).abs() < 1e-12);
    assert_eq!(hit.critical_chance, control.critical_chance);
    assert_eq!(hit.effects.shifting_idol_bonus.as_deref(), Some("hit"));

    let crit = prepare(Some(2));
    assert_eq!(crit.player_hp, control.player_hp);
    assert_eq!(crit.player_hit, control.player_hit);
    assert!((crit.critical_chance - (control.critical_chance + 0.05).min(1.0)).abs() < 1e-12);
    assert_eq!(crit.effects.shifting_idol_bonus.as_deref(), Some("crit"));
}

#[test]
fn pinnacle_pause_resume_forwards_carry_and_phase_event_once() {
    let database = fixture();
    set_pinnacle(&database, 1, "active", None);
    Connection::open(database.path())
        .expect("softened pinnacle DB")
        .execute(
            "UPDATE tunnels SET boss_progress=json_set(
                boss_progress,'$.\"350:1\"',json(?1))
              WHERE discord_id=?2 AND guild_id=?3",
            params![
                r#"{"status":"active","boss_id":"forgotten_king","hp_remaining":3,"hp_max":3,"last_engaged_at":1700000000}"#,
                PLAYER,
                GUILD
            ],
        )
        .expect("softened phase");
    let started = pinnacle_runtime(&database)
        .start_pinnacle(
            pinnacle_request(1_700_000_000),
            RiskTier::Reckless,
            25,
            SequenceEntropy::constant(0.99),
        )
        .expect("start pinnacle");
    let DigPinnacleStartOutcome::Paused(paused) = started.outcome else {
        panic!("expected durable pinnacle prompt")
    };
    assert_eq!(paused.boundary, PINNACLE_DEPTH);
    assert_eq!(paused.wager, 25);

    let resumed = pinnacle_runtime(&database)
        .resume_pinnacle(
            pinnacle_request(1_700_000_001),
            paused.pending_prompt.safe_option_index,
            SequenceEntropy::constant(0.0),
        )
        .expect("resume pinnacle");
    assert!(resumed.outcome.won);
    assert_eq!(resumed.outcome.phase, 1);
    assert_eq!(resumed.outcome.next_phase, 2);
    assert_eq!(resumed.outcome.jc_delta, 0);
    assert!(resumed.outcome.phase_event_id.is_some());
    let (phase, progress) = Connection::open(database.path())
        .expect("phase DB")
        .query_row(
            "SELECT pinnacle_phase,boss_progress FROM tunnels
              WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("phase state");
    assert_eq!(phase, 2);
    let progress: Value = serde_json::from_str(&progress).expect("progress JSON");
    assert_eq!(progress[PINNACLE_DEPTH.to_string()]["carried_wager"], 25);
    assert_eq!(
        progress[PINNACLE_DEPTH.to_string()]["carried_risk_tier"],
        "reckless"
    );
    assert!(
        progress[PINNACLE_DEPTH.to_string()]
            .get("pending_phase_event_id")
            .is_some()
    );
}

#[test]
fn inherited_pinnacle_carry_bypasses_new_cap_but_invalid_carry_does_not() {
    let database = fixture();
    set_pinnacle(&database, 2, "phase1_defeated", None);
    Connection::open(database.path())
        .expect("carry DB")
        .execute(
            "UPDATE tunnels SET boss_progress=json_set(
                boss_progress,'$.\"350\".carried_wager',1500,
                '$.\"350\".carried_risk_tier','bold')
              WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
        )
        .expect("carry");
    let started = pinnacle_runtime(&database)
        .start_pinnacle(
            pinnacle_request(1_700_000_000),
            RiskTier::Cautious,
            9_999,
            SequenceEntropy::constant(0.99),
        )
        .expect("carried wager continues");
    let DigPinnacleStartOutcome::Paused(paused) = started.outcome else {
        panic!("expected pause")
    };
    assert_eq!((paused.wager, paused.risk_tier), (1500, RiskTier::Bold));

    let invalid = fixture();
    set_pinnacle(&invalid, 1, "active", None);
    Connection::open(invalid.path())
        .expect("stale DB")
        .execute(
            "UPDATE tunnels SET boss_progress=json_set(
                boss_progress,'$.\"350\".carried_wager',20,
                '$.\"350\".carried_risk_tier','bold')
              WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
        )
        .expect("stale carry");
    assert!(matches!(
        pinnacle_runtime(&invalid).start_pinnacle(
            pinnacle_request(1_700_000_000),
            RiskTier::Bold,
            1_001,
            SequenceEntropy::constant(0.99),
        ),
        Err(DigBossRuntimeError::WagerTooLarge { .. })
    ));
    let progress = Connection::open(invalid.path())
        .expect("stale verify")
        .query_row(
            "SELECT boss_progress FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
            |row| row.get::<_, String>(0),
        )
        .expect("progress");
    assert_eq!(
        serde_json::from_str::<Value>(&progress).expect("JSON")[PINNACLE_DEPTH.to_string()]["carried_wager"],
        20
    );
}

// tests/test_dig_pinnacle_combat.py::test_pinnacle_loss_extra_wear_only_hits_armor
#[test]
fn pinnacle_loss_clamps_debit_and_only_extra_ticks_snapshot_armor() {
    let database = fixture();
    set_pinnacle(&database, 2, "phase1_defeated", None);
    let connection = Connection::open(database.path()).expect("loss DB");
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=7 WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
        )
        .expect("live balance");
    connection
        .execute(
            "UPDATE tunnels SET boss_progress=json_set(
                boss_progress,'$.\"350\".carried_wager',25,
                '$.\"350\".carried_risk_tier','reckless')
              WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
        )
        .expect("carry");
    connection
        .execute(
            "INSERT INTO dig_gear
             (discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source)
             VALUES (?1,?2,'weapon',1,3,1,100,'pinnacle'),
                    (?1,?2,'armor',1,3,1,101,'pinnacle'),
                    (?1,?2,'boots',1,3,1,102,'pinnacle')",
            params![PLAYER, GUILD],
        )
        .expect("gear");
    let started = pinnacle_runtime(&database)
        .start_pinnacle(
            pinnacle_request(1_700_000_000),
            RiskTier::Cautious,
            0,
            SequenceEntropy::constant(0.99),
        )
        .expect("start loss");
    let DigPinnacleStartOutcome::Paused(paused) = started.outcome else {
        panic!("expected pause")
    };
    let resolved = pinnacle_runtime(&database)
        .resume_pinnacle(
            pinnacle_request(1_700_000_001),
            paused.pending_prompt.safe_option_index,
            SequenceEntropy::constant(0.99),
        )
        .expect("loss");
    assert!(!resolved.outcome.won);
    assert_eq!(resolved.outcome.jc_delta, -7);
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![PLAYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("balance"),
        0
    );
    let durability = connection
        .prepare("SELECT slot,durability FROM dig_gear ORDER BY slot")
        .expect("gear query")
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .expect("gear rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("durability");
    assert_eq!(
        durability,
        vec![
            ("armor".to_owned(), 1),
            ("boots".to_owned(), 2),
            ("weapon".to_owned(), 2)
        ]
    );
}

// tests/test_dig_pinnacle_combat.py::test_pinnacle_scout_multiplier_matches_live_payout
#[test]
fn pinnacle_scout_multiplier_matches_the_settlement_policy() {
    let database = fixture();
    set_pinnacle(&database, 1, "active", None);
    Connection::open(database.path())
        .expect("multiplier DB")
        .execute(
            "INSERT INTO dig_inventory
             (discord_id,guild_id,item_type,queued,created_at)
             VALUES (?1,?2,'lantern',0,100)",
            params![PLAYER, GUILD],
        )
        .expect("lantern");
    let result = pinnacle_runtime(&database)
        .scout_pinnacle(
            pinnacle_request(1_700_000_000),
            false,
            SequenceEntropy::constant(0.5),
        )
        .expect("scout");
    let cautious = result.outcome.cautious;
    let expected = super::pinnacle_total_multiplier(RiskTier::Cautious, cautious.win_chance)
        .expect("valid multiplier");
    assert!((cautious.multiplier - expected).abs() < 1e-12);
    assert!(cautious.multiplier > 1.0);
}

// tests/test_dig_pinnacle_combat.py::test_scout_pinnacle_uses_the_locked_phase_boss
#[test]
fn pinnacle_scout_uses_the_locked_phase_boss() {
    let database = fixture();
    set_pinnacle(&database, 1, "active", None);
    Connection::open(database.path())
        .expect("locked scout DB")
        .execute(
            "INSERT INTO dig_inventory
             (discord_id,guild_id,item_type,queued,created_at)
             VALUES (?1,?2,'lantern',0,100)",
            params![PLAYER, GUILD],
        )
        .expect("lantern");
    let result = pinnacle_runtime(&database)
        .scout_pinnacle(
            pinnacle_request(1_700_000_000),
            false,
            SequenceEntropy::constant(0.5),
        )
        .expect("locked boss scout");
    assert_eq!(result.outcome.boss_id, "forgotten_king");
    assert_eq!(result.outcome.boss_name, "The Forgotten King");
    assert_eq!(result.outcome.phase, 1);
}

// tests/test_dig_pinnacle_combat.py::test_deaths_door_saves_a_killing_blow_without_a_pause
#[test]
fn pinnacle_deaths_door_saves_a_killing_blow_without_a_pause() {
    let mut combat = CombatState {
        player_hp: 1,
        boss_hp: 10,
        player_hit: 0.0,
        player_damage: 1,
        boss_hit: 1.0,
        boss_damage: 1,
        critical_chance: 0.0,
        critical_bonus: 0,
        effects: CombatEffects {
            relic_deaths_door: true,
            ..CombatEffects::default()
        },
    };
    let mut entropy = SequenceEntropy::new(vec![0.99, 0.0, 0.2, 0.99, 0.0], Vec::new(), Vec::new());
    let (saved, terminal) = crate::boss_multi_tier::run_combat_round(1, &mut combat, &mut entropy);
    assert_eq!(terminal, None);
    assert_eq!(saved.player_hp, 1);
    assert!(saved.effect_log.deaths_door);
    let (_, terminal) = crate::boss_multi_tier::run_combat_round(2, &mut combat, &mut entropy);
    assert_eq!(terminal, Some(false));
    assert_eq!(combat.player_hp, 0);
}

// tests/test_dig_pinnacle_combat.py::test_lifesteal_heals_on_first_landed_hit_without_a_pause
#[test]
fn pinnacle_lifesteal_heals_on_the_first_landed_hit_without_a_pause() {
    let mut combat = CombatState {
        player_hp: 2,
        boss_hp: 3,
        player_hit: 1.0,
        player_damage: 1,
        boss_hit: 0.0,
        boss_damage: 1,
        critical_chance: 0.0,
        critical_bonus: 0,
        effects: CombatEffects {
            trophy_lifesteal: true,
            ..CombatEffects::default()
        },
    };
    let mut entropy = SequenceEntropy::constant(0.0);
    let (record, terminal) = crate::boss_multi_tier::run_combat_round(1, &mut combat, &mut entropy);
    assert_eq!(terminal, None);
    assert!(record.player_hit);
    assert!(record.effect_log.lifesteal);
    assert_eq!(combat.player_hp, 3);
    assert!(!combat.effects.trophy_lifesteal);
}

// tests/test_dig_pinnacle_combat.py::test_pinnacle_fight_applies_pet_assist
#[test]
fn pinnacle_fight_applies_pet_assist_damage_to_a_landed_round() {
    let database = fixture();
    set_pinnacle(&database, 1, "active", None);
    let now = 1_700_000_000;
    Connection::open(database.path())
        .expect("pet fight DB")
        .execute(
            "INSERT INTO pets
             (discord_id,guild_id,name,species,adopted_at,hatched_at,adopt_fee,
              last_fed_at,hunger_at_last_fed,training_xp)
             VALUES (?1,?2,'Dane','common_cama',?3,?3,0,?4,100,20)",
            params![PLAYER, GUILD, now - 8 * 86_400, now],
        )
        .expect("trained pet");
    let started = pinnacle_runtime(&database)
        .start_pinnacle(
            pinnacle_request(now),
            RiskTier::Cautious,
            0,
            SequenceEntropy::constant(0.99),
        )
        .expect("pinnacle start");
    let DigPinnacleStartOutcome::Paused(paused) = started.outcome else {
        panic!("expected a durable mechanic pause")
    };
    let resumed = pinnacle_runtime(&database)
        .resume_pinnacle(
            pinnacle_request(now + 1),
            paused.pending_prompt.safe_option_index,
            SequenceEntropy::constant(0.0),
        )
        .expect("pinnacle resume");
    assert!(resumed.outcome.round_log.iter().any(|round| {
        round.pet_assist_damage > 0
            && round
                .pet_assist
                .as_ref()
                .is_some_and(|assist| assist.bonus_percent == 12)
    }));
}

// tests/test_dig_pinnacle_combat.py::test_pinnacle_victory_applies_bankruptcy_debuff
#[test]
fn pinnacle_final_victory_applies_the_persisted_bankruptcy_debuff() {
    let database = fixture();
    set_pinnacle(&database, 3, "phase2_defeated", None);
    Connection::open(database.path())
        .expect("bankruptcy DB")
        .execute(
            "INSERT INTO bankruptcy_state
             (discord_id,guild_id,last_bankruptcy_at,penalty_games_remaining,bankruptcy_count)
             VALUES (?1,?2,1,3,1)",
            params![PLAYER, GUILD],
        )
        .expect("bankruptcy state");
    let result = pinnacle_runtime(&database)
        .settle_pinnacle(
            pinnacle_request(1_700_000_000),
            PinnacleSettlementInput {
                boss_id: "forgotten_king".to_owned(),
                phase: 3,
                risk_tier: RiskTier::Cautious,
                wager: 0,
                won: true,
                ending_boss_hp: 0,
                boss_hp_max: 8,
                starting_boss_hp: 8,
                win_chance: 0.5,
                attempts: 1,
                round_log: Vec::new(),
                gear_wear: GearWearResult::default(),
                boss_preparation: None,
                warding_salts_blocked: false,
            },
            &mut SequenceEntropy::new(Vec::new(), vec![0, 0], Vec::new()),
        )
        .expect("bankruptcy-adjusted victory");
    assert!(result.outcome.won);
    assert!(result.outcome.bankruptcy_penalty > 0);
    assert!(result.outcome.jc_delta < result.outcome.gross_payout);
}

// tests/test_dig_pinnacle_combat.py::test_pinnacle_loss_debit_floors_at_live_balance
#[test]
fn pinnacle_loss_debit_floors_at_the_live_balance() {
    let database = fixture();
    set_pinnacle(&database, 2, "phase1_defeated", None);
    Connection::open(database.path())
        .expect("debit floor DB")
        .execute(
            "UPDATE players SET jopacoin_balance=7 WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
        )
        .expect("live balance");
    let result = pinnacle_runtime(&database)
        .settle_pinnacle(
            pinnacle_request(1_700_000_000),
            PinnacleSettlementInput {
                boss_id: "forgotten_king".to_owned(),
                phase: 2,
                risk_tier: RiskTier::Bold,
                wager: 25,
                won: false,
                ending_boss_hp: 4,
                boss_hp_max: 8,
                starting_boss_hp: 8,
                win_chance: 0.4,
                attempts: 1,
                round_log: Vec::new(),
                gear_wear: GearWearResult::default(),
                boss_preparation: None,
                warding_salts_blocked: false,
            },
            &mut SequenceEntropy::new(Vec::new(), Vec::new(), vec![8]),
        )
        .expect("loss settlement");
    assert_eq!(result.outcome.jc_delta, -7);
    assert_eq!(
        Connection::open(database.path())
            .expect("verify debit floor")
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![PLAYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("balance"),
        0
    );
}

// tests/test_dig_pinnacle_combat.py::test_pinnacle_scout_models_and_reports_pet_assist
#[test]
fn pinnacle_scout_reports_pet_and_consumes_one_lantern_across_restart() {
    let database = fixture();
    set_pinnacle(&database, 1, "active", None);
    let now = 1_700_000_000;
    let connection = Connection::open(database.path()).expect("scout DB");
    connection
        .execute(
            "INSERT INTO dig_inventory
             (discord_id,guild_id,item_type,queued,created_at)
             VALUES (?1,?2,'lantern',0,100)",
            params![PLAYER, GUILD],
        )
        .expect("lantern");
    connection
        .execute(
            "INSERT INTO pets
             (discord_id,guild_id,name,species,adopted_at,hatched_at,adopt_fee,
              last_fed_at,hunger_at_last_fed,training_xp)
             VALUES (?1,?2,'Dane','common_cama',?3,?3,0,?4,100,20)",
            params![PLAYER, GUILD, now - 8 * 86_400, now],
        )
        .expect("pet");
    let result = pinnacle_runtime(&database)
        .scout_pinnacle(pinnacle_request(now), true, SequenceEntropy::constant(0.5))
        .expect("scout");
    assert_eq!(result.outcome.boss_id, "forgotten_king");
    assert_eq!(result.outcome.phase, 1);
    assert_eq!(
        result
            .outcome
            .pet_assist
            .as_ref()
            .map(|assist| assist.bonus_percent),
        Some(12)
    );
    assert_eq!(result.outcome.enhanced_mechanic_ids.len(), 2);
    let control = fixture();
    set_pinnacle(&control, 1, "active", None);
    Connection::open(control.path())
        .expect("control DB")
        .execute(
            "INSERT INTO dig_inventory
             (discord_id,guild_id,item_type,queued,created_at)
             VALUES (?1,?2,'lantern',0,100)",
            params![PLAYER, GUILD],
        )
        .expect("control lantern");
    let without_pet = pinnacle_runtime(&control)
        .scout_pinnacle(pinnacle_request(now), false, SequenceEntropy::constant(0.5))
        .expect("control scout");
    assert!(
        result.outcome.cautious.win_chance > without_pet.outcome.cautious.win_chance,
        "fractional pet damage must participate in the scout model"
    );
    assert!(matches!(
        pinnacle_runtime(&database).scout_pinnacle(
            pinnacle_request(now + 1),
            false,
            SequenceEntropy::constant(0.5),
        ),
        Err(DigBossRuntimeError::Policy(
            crate::boss_multi_tier::BossServiceError::LanternRequired
        ))
    ));
}

#[test]
fn pinnacle_final_victory_fuses_reward_relic_status_and_audit() {
    let database = fixture();
    set_pinnacle(&database, 3, "phase2_defeated", None);
    let runtime = pinnacle_runtime(&database);
    let result = runtime
        .settle_pinnacle(
            pinnacle_request(1_700_000_000),
            PinnacleSettlementInput {
                boss_id: "forgotten_king".to_owned(),
                phase: 3,
                risk_tier: RiskTier::Bold,
                wager: 0,
                won: true,
                ending_boss_hp: 0,
                boss_hp_max: 12,
                starting_boss_hp: 12,
                win_chance: 0.5,
                attempts: 1,
                round_log: Vec::new(),
                gear_wear: GearWearResult::default(),
                boss_preparation: None,
                warding_salts_blocked: false,
            },
            &mut SequenceEntropy::new(Vec::new(), vec![0, 0], Vec::new()),
        )
        .expect("pinnacle victory");
    assert!(result.outcome.won);
    assert_eq!(result.outcome.next_phase, 0);
    let relic = result.outcome.relic_drop.expect("relic");
    assert!(relic.database_id > 0);
    assert_eq!(relic.relic.stat_ids.len(), 2);
    assert_ne!(relic.relic.stat_ids[0], relic.relic.stat_ids[1]);
    let connection = Connection::open(database.path()).expect("victory DB");
    let row = connection
        .query_row(
            "SELECT pinnacle_phase,depth,jopacoin_balance,boss_progress
               FROM tunnels JOIN players USING(discord_id,guild_id)
              WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .expect("victory state");
    assert_eq!((row.0, row.1, row.2), (0, PINNACLE_DEPTH.into(), 425));
    assert_eq!(
        serde_json::from_str::<Value>(&row.3).expect("JSON")[PINNACLE_DEPTH.to_string()]["status"],
        "defeated"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_artifacts WHERE discord_id=?1 AND guild_id=?2",
                params![PLAYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("artifact count"),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_actions WHERE actor_id=?1 AND guild_id=?2
                    AND action_type='pinnacle_fight'",
                params![PLAYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("audit count"),
        1
    );
}

// tests/test_dig_reward_policy.py::test_pinnacle_reward_applies_edict_to_base_and_wager_profit
#[test]
fn pinnacle_reward_applies_economy_event_to_base_and_wager_profit() {
    let database = fixture();
    set_pinnacle(&database, 3, "phase2_defeated", None);
    let now = 1_700_000_000;
    let event_date = game_date_for_timestamp(now as f64).expect("pinnacle event date");
    EconomyEventRepository::new(database.path())
        .activate_event_atomic(
            Some(GUILD),
            &EventDraft {
                event_date,
                name: "Double pinnacle rewards".to_owned(),
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
                announcement: "Double pinnacle rewards".to_owned(),
                starts_at: now - 60,
                ends_at: now + 60,
                created_at: now - 60,
            },
        )
        .expect("persist reward edict");
    let mut config = DigBossRuntimeConfig::new(database.path(), 20);
    config.economy_event.enabled = true;
    let runtime = DigBossRuntimeService::sqlite(config);
    let wager = 200;
    let win_chance = 0.5;
    let raw_wager_profit = pinnacle_wager_profit(wager, RiskTier::Cautious, win_chance)
        .expect("authored wager profit");
    let result = runtime
        .settle_pinnacle(
            pinnacle_request(now),
            PinnacleSettlementInput {
                boss_id: "forgotten_king".to_owned(),
                phase: 3,
                risk_tier: RiskTier::Cautious,
                wager,
                won: true,
                ending_boss_hp: 0,
                boss_hp_max: 8,
                starting_boss_hp: 8,
                win_chance,
                attempts: 1,
                round_log: Vec::new(),
                gear_wear: GearWearResult::default(),
                boss_preparation: None,
                warding_salts_blocked: false,
            },
            &mut SequenceEntropy::new(Vec::new(), vec![0, 0], Vec::new()),
        )
        .expect("pinnacle edict settlement");
    let expected_gross_base = 1_000;
    let expected_scaled_base = scale_positive_dig_jc(expected_gross_base);
    let expected_event_wager = raw_wager_profit * 2;
    let expected_gross_payout = expected_gross_base + expected_event_wager;
    let expected_jc = expected_scaled_base + expected_event_wager;
    assert_eq!(result.outcome.gross_jc, expected_gross_base);
    assert_eq!(result.outcome.scaled_base_jc, expected_scaled_base);
    assert_eq!(result.outcome.wager_payout, expected_event_wager);
    assert_eq!(result.outcome.gross_payout, expected_gross_payout);
    assert_eq!(result.outcome.payout, expected_jc);
    assert_eq!(result.outcome.jc_delta, expected_jc);
    assert_eq!(
        result.outcome.reward_multiplier,
        cama_domain::dig_economy::DIG_POSITIVE_JC_MULTIPLIER
    );
    assert_eq!(result.outcome.bankruptcy_penalty, 0);
    assert_eq!(result.outcome.vanity_tax, 0);
    assert_eq!(
        Connection::open(database.path())
            .expect("inspect pinnacle edict balance")
            .query_row(
                "SELECT jopacoin_balance FROM players
                  WHERE discord_id=?1 AND guild_id=?2",
                params![PLAYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("pinnacle balance"),
        100 + expected_jc
    );
    let detail: String = Connection::open(database.path())
        .expect("inspect pinnacle edict audit")
        .query_row(
            "SELECT detail FROM dig_actions
              WHERE actor_id=?1 AND guild_id=?2 AND action_type='pinnacle_fight'
              ORDER BY id DESC LIMIT 1",
            params![PLAYER, GUILD],
            |row| row.get(0),
        )
        .expect("pinnacle audit detail");
    let detail = serde_json::from_str::<Value>(&detail).expect("pinnacle detail JSON");
    assert_eq!(detail["jc_delta"], expected_jc);
    assert_eq!(detail["gross_jc"], expected_gross_base);
    assert_eq!(detail["scaled_base_jc"], expected_scaled_base);
    assert_eq!(detail["wager_payout"], expected_event_wager);
    assert_eq!(detail["gross_payout"], expected_gross_payout);
    assert_eq!(detail["reward_multiplier"], 0.65);
}

// tests/test_dig_pinnacle_combat.py::test_paused_pinnacle_duel_persists_pet_assist
#[test]
fn paused_pinnacle_duel_snapshots_pet_assist_across_restart() {
    let database = fixture();
    set_pinnacle(&database, 1, "active", None);
    let now = 1_700_000_000;
    let connection = Connection::open(database.path()).expect("pet DB");
    connection
        .execute(
            "INSERT INTO pets
             (discord_id,guild_id,name,species,adopted_at,hatched_at,adopt_fee,
              last_fed_at,hunger_at_last_fed,training_xp)
             VALUES (?1,?2,'Dane','common_cama',?3,?3,0,?4,100,20)",
            params![PLAYER, GUILD, now - 8 * 86_400, now],
        )
        .expect("pet");
    let start = pinnacle_runtime(&database)
        .start_pinnacle(
            pinnacle_request(now),
            RiskTier::Cautious,
            0,
            SequenceEntropy::constant(0.99),
        )
        .expect("start");
    let DigPinnacleStartOutcome::Paused(paused) = start.outcome else {
        panic!("expected pause")
    };
    assert_eq!(
        paused
            .combat
            .effects
            .pet_assist
            .as_ref()
            .map(|assist| assist.bonus_percent),
        Some(12)
    );
    connection
        .execute(
            "DELETE FROM pets WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
        )
        .expect("remove live pet");
    let resumed = pinnacle_runtime(&database)
        .resume_pinnacle(
            pinnacle_request(now + 1),
            paused.pending_prompt.safe_option_index,
            SequenceEntropy::constant(0.99),
        )
        .expect("resume");
    assert!(resumed.outcome.round_log.iter().any(|round| {
        round
            .pet_assist
            .as_ref()
            .is_some_and(|assist| assist.bonus_percent == 12)
    }));
}

// tests/test_dig_pinnacle_combat.py::test_wagered_pinnacle_fight_keeps_ten_percent_hit_floor
#[test]
fn wagered_pinnacle_attempt_keeps_ten_percent_hit_floor() {
    let database = fixture();
    set_pinnacle(&database, 3, "phase2_defeated", None);
    Connection::open(database.path())
        .expect("prestige DB")
        .execute(
            "UPDATE tunnels SET prestige_level=20 WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
        )
        .expect("extreme penalty");
    let mut paid_entropy = SequenceEntropy::constant(0.99);
    let paid = pinnacle_runtime(&database)
        .prepare_pinnacle_attempt(
            pinnacle_request(1_700_000_000),
            RiskTier::Reckless,
            10,
            &mut paid_entropy,
        )
        .expect("paid attempt");
    assert!((paid.combat.player_hit - WAGERED_PLAYER_HIT_FLOOR).abs() < f64::EPSILON);
}

// tests/test_dig_pinnacle_combat.py::test_pinnacle_loss_never_credits_a_negative_balance
#[test]
fn pinnacle_loss_never_credits_an_existing_negative_balance() {
    let database = fixture();
    set_pinnacle(&database, 1, "active", None);
    let connection = Connection::open(database.path()).expect("negative DB");
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=-9 WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
        )
        .expect("negative balance");
    let result = pinnacle_runtime(&database)
        .settle_pinnacle(
            pinnacle_request(1_700_000_000),
            PinnacleSettlementInput {
                boss_id: "forgotten_king".to_owned(),
                phase: 1,
                risk_tier: RiskTier::Bold,
                wager: 25,
                won: false,
                ending_boss_hp: 4,
                boss_hp_max: 8,
                starting_boss_hp: 8,
                win_chance: 0.4,
                attempts: 1,
                round_log: Vec::new(),
                gear_wear: GearWearResult::default(),
                boss_preparation: None,
                warding_salts_blocked: false,
            },
            &mut SequenceEntropy::new(Vec::new(), Vec::new(), vec![8]),
        )
        .expect("loss");
    assert_eq!(result.outcome.jc_delta, 0);
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![PLAYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("balance"),
        -9
    );
}

#[test]
fn pinnacle_relic_insert_failure_rolls_back_reward_and_defeat() {
    let database = fixture();
    set_pinnacle(&database, 3, "phase2_defeated", None);
    let connection = Connection::open(database.path()).expect("failure DB");
    connection
        .execute_batch(
            "CREATE TRIGGER fail_pinnacle_relic BEFORE INSERT ON dig_artifacts
             BEGIN SELECT RAISE(ABORT,'injected relic failure'); END;",
        )
        .expect("failure trigger");
    let error = pinnacle_runtime(&database)
        .settle_pinnacle(
            pinnacle_request(1_700_000_000),
            PinnacleSettlementInput {
                boss_id: "forgotten_king".to_owned(),
                phase: 3,
                risk_tier: RiskTier::Cautious,
                wager: 0,
                won: true,
                ending_boss_hp: 0,
                boss_hp_max: 8,
                starting_boss_hp: 8,
                win_chance: 0.5,
                attempts: 1,
                round_log: Vec::new(),
                gear_wear: GearWearResult::default(),
                boss_preparation: None,
                warding_salts_blocked: false,
            },
            &mut SequenceEntropy::new(Vec::new(), vec![0, 1], Vec::new()),
        )
        .expect_err("injected failure");
    assert!(matches!(error, DigBossRuntimeError::Infrastructure(_)));
    let row = connection
        .query_row(
            "SELECT pinnacle_phase,depth,jopacoin_balance,boss_progress
               FROM tunnels JOIN players USING(discord_id,guild_id)
              WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .expect("rolled back state");
    assert_eq!((row.0, row.1, row.2), (3, 349, 100));
    assert_eq!(
        serde_json::from_str::<Value>(&row.3).expect("JSON")[PINNACLE_DEPTH.to_string()]["status"],
        "phase2_defeated"
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                .get::<_, i64>(0))
            .expect("audit count"),
        0
    );
}

#[test]
fn quiet_phase_event_does_not_change_pinnacle_hp() {
    let with_event = fixture();
    set_pinnacle(&with_event, 2, "phase1_defeated", Some("quiet"));
    let control = fixture();
    set_pinnacle(&control, 2, "phase1_defeated", None);
    let mut event_entropy = SequenceEntropy::constant(0.99);
    let event = pinnacle_runtime(&with_event)
        .prepare_pinnacle_attempt(
            pinnacle_request(1_700_000_000),
            RiskTier::Cautious,
            0,
            &mut event_entropy,
        )
        .expect("quiet event");
    let mut control_entropy = SequenceEntropy::constant(0.99);
    let control = pinnacle_runtime(&control)
        .prepare_pinnacle_attempt(
            pinnacle_request(1_700_000_000),
            RiskTier::Cautious,
            0,
            &mut control_entropy,
        )
        .expect("control");
    assert_eq!(event.boss_hp_max, control.boss_hp_max);
}
