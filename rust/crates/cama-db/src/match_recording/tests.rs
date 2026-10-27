use std::collections::BTreeMap;
use std::sync::OnceLock;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::schema_manager::initialize_or_migrate;
use crate::test_support::FastTestDatabase;

const GUILD: i64 = 9_001;

fn gc_statistics_fixture() -> (NamedTempFile, MatchRecordingRepository) {
    let file = NamedTempFile::new().unwrap();
    initialize_or_migrate(file.path()).unwrap();
    let connection = Connection::open(file.path()).unwrap();
    connection.execute("INSERT INTO matches(match_id,guild_id,valve_match_id,winning_team,team1_players,team2_players,enrichment_data) VALUES(1,9001,900,1,'[]','[]','{\"duration\":20}'),(2,0,901,2,'[]','[]',NULL)", []).unwrap();
    let repository = MatchRecordingRepository::new(file.path());
    (file, repository)
}

#[test]
fn gc_statistics_preserve_first_sparse_snapshot_across_retries_and_guild_scoped_reads() {
    let (file, repository) = gc_statistics_fixture();
    let payload = serde_json::json!({"match_id":900,"radiant_win":true,"players":[{"account_id":10,"kills":0}]});
    repository
        .save_gc_statistics(1, Some(GUILD), 900, &payload.to_string())
        .unwrap();
    repository
        .save_gc_statistics(1, Some(GUILD), 900, &payload.to_string())
        .unwrap();
    repository
        .save_gc_statistics(1, Some(GUILD), 900, "{\"match_id\":900,\"duration\":null}")
        .unwrap();
    let reopened = MatchRecordingRepository::new(file.path());
    assert_eq!(
        serde_json::from_str::<JsonValue>(
            &reopened.gc_statistics(1, Some(GUILD)).unwrap().unwrap()
        )
        .unwrap(),
        payload
    );
    assert!(
        reopened
            .gc_statistics(1, Some(GUILD + 1))
            .unwrap()
            .is_none()
    );
    assert!(reopened.gc_statistics(1, None).unwrap().is_none());
    reopened
        .save_gc_statistics(2, None, 901, "{\"match_id\":901,\"radiant_win\":false}")
        .unwrap();
    assert!(reopened.gc_statistics(2, Some(0)).unwrap().is_some());

    let read_repository = crate::core_repositories::MatchRepository::new(file.path());
    assert_eq!(
        read_repository.gc_statistics(1, Some(GUILD)).unwrap(),
        reopened.gc_statistics(1, Some(GUILD)).unwrap()
    );
    assert!(read_repository.gc_statistics(1, None).unwrap().is_none());
    assert_eq!(
        read_repository
            .raw_enrichment_data(1, Some(GUILD))
            .unwrap()
            .as_deref(),
        Some("{\"duration\":20}")
    );
    assert!(
        read_repository
            .raw_enrichment_data(1, Some(GUILD + 1))
            .unwrap()
            .is_none()
    );
    assert!(
        read_repository
            .raw_enrichment_data(2, None)
            .unwrap()
            .is_none()
    );
}

#[test]
fn gc_statistics_reject_invalid_identity_and_winner_without_persisting() {
    let (_file, repository) = gc_statistics_fixture();
    for (guild, valve_id, payload) in [
        (Some(GUILD + 1), 900, "{\"match_id\":900}"),
        (Some(GUILD), 901, "{\"match_id\":901}"),
        (Some(GUILD), 900, "{\"match_id\":901}"),
        (Some(GUILD), 900, "{\"match_id\":900,\"radiant_win\":false}"),
        (Some(GUILD), 900, "{\"match_id\":900,\"radiant_win\":1}"),
        (Some(GUILD), 900, "[]"),
        (Some(GUILD), 900, "broken"),
        (Some(GUILD), 900, "{}"),
    ] {
        assert!(matches!(
            repository.save_gc_statistics(1, guild, valve_id, payload),
            Err(MatchRecordingRepositoryError::InvalidGcStatistics(_))
        ));
        assert!(repository.gc_statistics(1, Some(GUILD)).unwrap().is_none());
    }
    assert!(
        repository
            .save_gc_statistics(999, Some(GUILD), 900, "{\"match_id\":900}")
            .is_err()
    );
}

#[test]
fn gc_statistics_cannot_be_rebound_when_external_match_identity_changes() {
    let (file, repository) = gc_statistics_fixture();
    repository
        .save_gc_statistics(1, Some(GUILD), 900, "{\"match_id\":900}")
        .unwrap();
    Connection::open(file.path())
        .unwrap()
        .execute("UPDATE matches SET valve_match_id=999 WHERE match_id=1", [])
        .unwrap();
    assert!(
        repository.gc_statistics(1, Some(GUILD)).unwrap().is_none(),
        "old statistics must never be returned for another Valve match"
    );
    assert!(
        repository
            .save_gc_statistics(1, Some(GUILD), 999, "{\"match_id\":999}")
            .is_err()
    );
}

#[test]
fn gc_statistics_prevent_enrichment_from_replacing_the_valve_match_association() {
    let (file, repository) = gc_statistics_fixture();
    repository
        .save_gc_statistics(1, Some(GUILD), 900, "{\"match_id\":900}")
        .unwrap();
    assert!(matches!(
        repository.apply_enrichment_atomic(MatchEnrichmentRequest {
            match_id: 1,
            guild_id: Some(GUILD),
            valve_match_id: 999,
            duration_seconds: 999,
            radiant_score: 99,
            dire_score: 99,
            game_mode: 2,
            enrichment_data: Some("{\"match_id\":999}"),
            enrichment_source: Some("opendota"),
            enrichment_confidence: None,
            participant_updates: &[],
            wrapped_facts: &[],
        }),
        Err(MatchRecordingRepositoryError::InvalidGcStatistics(_))
    ));
    let (valve_id, payload): (i64, String) = Connection::open(file.path())
        .unwrap()
        .query_row(
            "SELECT valve_match_id,enrichment_data FROM matches WHERE match_id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(valve_id, 900);
    assert_eq!(payload, "{\"duration\":20}");
    assert!(repository.gc_statistics(1, Some(GUILD)).unwrap().is_some());
}

static FIXTURE_DATABASE_TEMPLATE: OnceLock<NamedTempFile> = OnceLock::new();

#[test]
fn gc_statistics_saved_after_api_read_reject_stale_projection_until_gc_merge() {
    let (file, repository) = gc_statistics_fixture();
    // An API request was prepared before the GC snapshot was available.
    let mut request = MatchEnrichmentRequest {
        match_id: 1,
        guild_id: Some(GUILD),
        valve_match_id: 900,
        duration_seconds: 999,
        radiant_score: 0,
        dire_score: 0,
        game_mode: 2,
        enrichment_data: Some("{\"match_id\":900,\"duration\":999}"),
        enrichment_source: Some("opendota"),
        enrichment_confidence: None,
        participant_updates: &[],
        wrapped_facts: &[],
    };
    repository
        .save_gc_statistics(1, Some(GUILD), 900, "{\"match_id\":900,\"duration\":1000}")
        .unwrap();
    for payload in [
        None,
        Some("{\"match_id\":900,\"duration\":999}"),
        Some("{\"_cama_gc_statistics\":false}"),
        Some("{\"_cama_gc_statistics\":\"true\"}"),
    ] {
        request.enrichment_data = payload;
        assert!(
            matches!(repository.apply_enrichment_atomic(request), Err(MatchRecordingRepositoryError::InvalidGcStatistics(message)) if message.contains("reload statistics"))
        );
        let stored: String = Connection::open(file.path())
            .unwrap()
            .query_row(
                "SELECT enrichment_data FROM matches WHERE match_id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            stored, "{\"duration\":20}",
            "stale writes cannot partially replace canonical data"
        );
    }
    request.enrichment_data =
        Some("{\"match_id\":900,\"duration\":1000,\"_cama_gc_statistics\":true}");
    request.duration_seconds = 1000;
    repository.apply_enrichment_atomic(request).unwrap();
    let (duration, stored): (i64, String) = Connection::open(file.path())
        .unwrap()
        .query_row(
            "SELECT duration_seconds,enrichment_data FROM matches WHERE match_id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(duration, 1000);
    assert_eq!(stored, request.enrichment_data.unwrap());
}

struct Fixture {
    file: FastTestDatabase,
    repository: MatchRecordingRepository,
}

impl Fixture {
    fn new() -> Self {
        let template = FIXTURE_DATABASE_TEMPLATE.get_or_init(|| {
            let file = NamedTempFile::new().expect("temporary SQLite template");
            let connection = Connection::open(file.path()).expect("open fixture template");
            connection
                .execute_batch(
                    "PRAGMA journal_mode = WAL;
                 PRAGMA foreign_keys = OFF;
                 CREATE TABLE players (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     discord_username TEXT NOT NULL,
                     wins INTEGER DEFAULT 0,
                     losses INTEGER DEFAULT 0,
                     glicko_rating REAL,
                     glicko_rd REAL,
                     glicko_volatility REAL,
                     exclusion_count INTEGER DEFAULT 0,
                     jopacoin_balance INTEGER DEFAULT 3,
                     lowest_balance_ever INTEGER,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE pending_matches(pending_match_id INTEGER PRIMARY KEY,guild_id INTEGER NOT NULL,payload TEXT NOT NULL);
                 CREATE TABLE matches (
                     match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     team1_players TEXT NOT NULL,
                     team2_players TEXT NOT NULL,
                     winning_team INTEGER,
                     lobby_type TEXT,
                     balancing_rating_system TEXT,
                     betting_mode TEXT,
                     win_reward_jc INTEGER,
                     pending_match_id INTEGER,
                     valve_match_id INTEGER,
                     duration_seconds INTEGER,
                     radiant_score INTEGER,
                     dire_score INTEGER,
                     game_mode INTEGER,
                     enrichment_data TEXT,
                     enrichment_source TEXT,
                     enrichment_confidence REAL,
                     parsed_refresh_attempts INTEGER NOT NULL DEFAULT 0,
                     match_date TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                 );
                 CREATE UNIQUE INDEX idx_matches_guild_pending
                     ON matches(guild_id, pending_match_id)
                     WHERE pending_match_id IS NOT NULL;
                 CREATE TABLE match_participants (
                     match_id INTEGER NOT NULL,
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     team_number INTEGER,
                     won INTEGER,
                     side TEXT,
                     hero_id INTEGER,
                     kills INTEGER,
                     deaths INTEGER,
                     assists INTEGER,
                     gpm INTEGER,
                     xpm INTEGER,
                     hero_damage INTEGER,
                     tower_damage INTEGER,
                     last_hits INTEGER,
                     denies INTEGER,
                     net_worth INTEGER,
                     hero_healing INTEGER,
                     lane_role INTEGER,
                     lane_efficiency INTEGER,
                     gold_at_10 INTEGER,
                     last_hits_at_10 INTEGER,
                     derived_role TEXT,
                     towers_killed INTEGER,
                     roshans_killed INTEGER,
                     teamfight_participation REAL,
                     obs_placed INTEGER,
                     sen_placed INTEGER,
                     camps_stacked INTEGER,
                     rune_pickups INTEGER,
                     firstblood_claimed INTEGER,
                     stuns REAL,
                     fantasy_points REAL,
                     PRIMARY KEY (match_id, discord_id)
                 );
                 CREATE TABLE wrapped_enrichment_facts (
                     guild_id INTEGER NOT NULL,
                     match_id INTEGER NOT NULL,
                     discord_id INTEGER NOT NULL,
                     actions_per_min INTEGER,
                     courier_kills INTEGER,
                     pings INTEGER,
                     rapier_count INTEGER NOT NULL DEFAULT 0,
                     lane_role INTEGER,
                     comeback INTEGER,
                     throw INTEGER,
                     PRIMARY KEY (guild_id, match_id, discord_id)
                 );
                 CREATE TABLE match_bans (
                     match_id INTEGER NOT NULL,
                     ban_index INTEGER NOT NULL,
                     team INTEGER NOT NULL,
                     hero_id INTEGER NOT NULL,
                     PRIMARY KEY (match_id, ban_index)
                 );
                 CREATE TABLE match_draft_analysis (
                     match_id INTEGER PRIMARY KEY
                 );
                 CREATE TABLE openskill_replay_jobs (
                     guild_id INTEGER PRIMARY KEY,
                     reason TEXT NOT NULL,
                     requested_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                     last_error TEXT
                 );
                 CREATE TABLE rating_history (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     discord_id INTEGER,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     rating REAL,
                     rating_before REAL,
                     rd_before REAL,
                     rd_after REAL,
                     volatility_before REAL,
                     volatility_after REAL,
                     team_number INTEGER,
                     won INTEGER,
                     match_id INTEGER
                 );
                 CREATE TABLE bets (
                     bet_id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     match_id INTEGER,
                     discord_id INTEGER NOT NULL,
                     team_bet_on TEXT NOT NULL,
                     amount INTEGER NOT NULL,
                     bet_time INTEGER NOT NULL,
                     pending_match_id INTEGER,
                     created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                 );
                 CREATE TABLE loan_state (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     last_loan_at INTEGER,
                     total_loans_taken INTEGER DEFAULT 0,
                     total_fees_paid INTEGER DEFAULT 0,
                     negative_loans_taken INTEGER DEFAULT 0,
                     outstanding_principal INTEGER DEFAULT 0,
                     outstanding_fee INTEGER DEFAULT 0,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE match_gc_statistics (
                     guild_id INTEGER NOT NULL,
                     match_id INTEGER NOT NULL,
                     valve_match_id INTEGER NOT NULL,
                     payload_json TEXT NOT NULL,
                     captured_at INTEGER NOT NULL DEFAULT 0,
                     PRIMARY KEY (guild_id, match_id)
                 );
                 CREATE TABLE nonprofit_fund (
                     guild_id INTEGER PRIMARY KEY DEFAULT 0,
                     total_collected INTEGER DEFAULT 0,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                 );
                 CREATE TABLE player_pairings (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL,
                     player1_id INTEGER NOT NULL,
                     player2_id INTEGER NOT NULL,
                     games_together INTEGER NOT NULL DEFAULT 0,
                     wins_together INTEGER NOT NULL DEFAULT 0,
                     games_against INTEGER NOT NULL DEFAULT 0,
                     player1_wins_against INTEGER NOT NULL DEFAULT 0,
                     last_match_id INTEGER,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     UNIQUE(guild_id, player1_id, player2_id)
                 );",
                )
                .expect("create disposable match-recording schema template");
            drop(connection);
            file
        });
        let file = FastTestDatabase::from_template(template.path());
        let repository = MatchRecordingRepository::new(file.path());
        Self { file, repository }
    }

    fn connection(&self) -> Connection {
        Connection::open(self.file.path()).expect("open fixture connection")
    }

    fn seed(&self, ids: &[i64]) {
        let mut connection = self.connection();
        let transaction = connection.transaction().expect("seed players transaction");
        for discord_id in ids {
            transaction
                .execute(
                    "INSERT INTO players (
                         discord_id, guild_id, discord_username, wins, losses,
                         glicko_rating, glicko_rd, glicko_volatility,
                         jopacoin_balance, lowest_balance_ever, updated_at
                     ) VALUES (?1, ?2, ?3, 0, 0, 1500.0, 350.0, 0.06, 3, 3,
                               CURRENT_TIMESTAMP)",
                    params![discord_id, GUILD, format!("Player{discord_id}")],
                )
                .expect("seed player");
        }
        transaction.commit().expect("commit seeded players");
    }
}

fn teams(start: i64) -> (Vec<i64>, Vec<i64>) {
    (
        (start..start + 5).collect(),
        (start + 5..start + 10).collect(),
    )
}

/// Insert a recorded Radiant win with its participant rows, the state
/// enrichment expects to find. Production recording lives in
/// `MatchRepository::record_match_core_atomic`.
fn insert_recorded_match(fixture: &Fixture, radiant: &[i64], dire: &[i64]) -> i64 {
    let encode = |ids: &[i64]| {
        format!(
            "[{}]",
            ids.iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let mut connection = fixture.connection();
    let transaction = connection.transaction().expect("fixture match transaction");
    transaction
        .execute(
            "INSERT INTO matches (
                 guild_id, team1_players, team2_players, winning_team,
                 lobby_type, balancing_rating_system, betting_mode, match_date
             ) VALUES (?1, ?2, ?3, 1, 'shuffle', 'glicko', 'house', CURRENT_TIMESTAMP)",
            params![GUILD, encode(radiant), encode(dire)],
        )
        .expect("insert fixture match");
    let match_id = transaction.last_insert_rowid();
    for (team, ids) in [(TeamSide::Radiant, radiant), (TeamSide::Dire, dire)] {
        for discord_id in ids {
            transaction
                .execute(
                    "INSERT INTO match_participants (
                         match_id, discord_id, guild_id, team_number, won, side
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        match_id,
                        discord_id,
                        GUILD,
                        team.number(),
                        team == TeamSide::Radiant,
                        team.name()
                    ],
                )
                .expect("insert fixture participant");
        }
    }
    transaction.commit().expect("commit fixture match");
    match_id
}

fn recorded_enrichment_match(fixture: &Fixture, start: i64) -> i64 {
    let (radiant, dire) = teams(start);
    fixture.seed(&[radiant.clone(), dire.clone()].concat());
    insert_recorded_match(fixture, &radiant, &dire)
}

fn apply_enrichment_payload(
    fixture: &Fixture,
    match_id: i64,
    enrichment_data: &str,
    source: &str,
) -> usize {
    fixture
        .repository
        .apply_enrichment_atomic(MatchEnrichmentRequest {
            match_id,
            guild_id: Some(GUILD),
            valve_match_id: 8_181_518_332,
            duration_seconds: 2_400,
            radiant_score: 35,
            dire_score: 22,
            game_mode: 2,
            enrichment_data: Some(enrichment_data),
            enrichment_source: Some(source),
            enrichment_confidence: None,
            participant_updates: &[],
            wrapped_facts: &[],
        })
        .expect("apply enrichment payload")
}

fn projected_bans(fixture: &Fixture, match_id: i64) -> Vec<(i64, i64, i64)> {
    fixture
        .connection()
        .prepare(
            "SELECT ban_index, team, hero_id FROM match_bans
             WHERE match_id = ?1 ORDER BY ban_index",
        )
        .and_then(|mut statement| {
            statement
                .query_map([match_id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?
                .collect::<Result<Vec<_>, _>>()
        })
        .expect("read normalized bans")
}

type WrappedProjectionRow = (
    i64,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    i64,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

fn projected_wrapped_facts(fixture: &Fixture, match_id: i64) -> Vec<WrappedProjectionRow> {
    fixture
        .connection()
        .prepare(
            "SELECT discord_id,actions_per_min,courier_kills,pings,
                    rapier_count,lane_role,comeback,throw
               FROM wrapped_enrichment_facts
              WHERE guild_id=?1 AND match_id=?2 ORDER BY discord_id",
        )
        .and_then(|mut statement| {
            statement
                .query_map(params![GUILD, match_id], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()
        })
        .expect("read compact Wrapped projection")
}

fn fantasy_update(discord_id: i64, fantasy_points: f64) -> EnrichmentParticipantUpdate {
    EnrichmentParticipantUpdate {
        discord_id,
        hero_id: None,
        kills: None,
        deaths: None,
        assists: None,
        gpm: None,
        xpm: None,
        hero_damage: None,
        tower_damage: None,
        last_hits: None,
        denies: None,
        net_worth: None,
        hero_healing: None,
        lane_role: None,
        lane_efficiency: None,
        gold_at_10: None,
        last_hits_at_10: None,
        towers_killed: None,
        roshans_killed: None,
        teamfight_participation: None,
        obs_placed: None,
        sen_placed: None,
        camps_stacked: None,
        rune_pickups: None,
        firstblood_claimed: None,
        stuns: None,
        fantasy_points: Some(fantasy_points),
    }
}

#[test]
fn test_extract_match_bans_keeps_only_valid_bans() {
    let fixture = Fixture::new();
    let match_id = recorded_enrichment_match(&fixture, 34_001);

    assert_eq!(
        apply_enrichment_payload(
            &fixture,
            match_id,
            r#"{
                "picks_bans": [
                    {"is_pick": false, "team": 1, "hero_id": 10},
                    {"is_pick": true, "team": 0, "hero_id": 20},
                    {"is_pick": false, "team": 3, "hero_id": 30},
                    {"is_pick": false, "team": 0, "hero_id": 0},
                    "corrupt",
                    {"is_pick": 0, "team": "0", "hero_id": "40"},
                    {"is_pick": 0.0, "team": 0.9, "hero_id": 50.9},
                    {"is_pick": false, "team": " 1 ", "hero_id": " 6_000 "},
                    {"is_pick": false, "team": false, "hero_id": true}
                ]
            }"#,
            "manual",
        ),
        0
    );
    assert_eq!(
        projected_bans(&fixture, match_id),
        [(0, 1, 10), (5, 0, 40), (6, 0, 50), (7, 1, 6_000)]
    );

    assert_eq!(
        apply_enrichment_payload(&fixture, match_id, "{not-json", "manual"),
        0
    );
    assert!(projected_bans(&fixture, match_id).is_empty());
}

#[test]
fn test_reenrichment_replaces_normalized_bans() {
    let fixture = Fixture::new();
    let match_id = recorded_enrichment_match(&fixture, 35_001);
    apply_enrichment_payload(
        &fixture,
        match_id,
        r#"{"picks_bans":[
            {"is_pick":false,"team":1,"hero_id":10},
            {"is_pick":false,"team":1,"hero_id":20}
        ]}"#,
        "manual",
    );
    assert_eq!(projected_bans(&fixture, match_id), [(0, 1, 10), (1, 1, 20)]);

    apply_enrichment_payload(
        &fixture,
        match_id,
        r#"{"picks_bans":[
            {"is_pick":false,"team":0,"hero_id":30},
            {"is_pick":false,"team":1,"hero_id":40}
        ]}"#,
        "manual",
    );
    assert_eq!(projected_bans(&fixture, match_id), [(0, 0, 30), (1, 1, 40)]);
}

#[test]
fn test_atomic_enrichment_projects_bans() {
    let fixture = Fixture::new();
    let match_id = recorded_enrichment_match(&fixture, 36_001);

    assert_eq!(
        apply_enrichment_payload(
            &fixture,
            match_id,
            r#"{"picks_bans":[{"is_pick":false,"team":1,"hero_id":10}]}"#,
            "auto",
        ),
        0
    );
    assert_eq!(projected_bans(&fixture, match_id), [(0, 1, 10)]);
}

#[test]
fn test_atomic_write_wipe_and_reenrich_keep_compact_facts_consistent() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(37_001);
    let participants = [radiant.clone(), dire.clone()].concat();
    fixture.seed(&participants);
    let match_id = insert_recorded_match(&fixture, &radiant, &dire);
    let initial = [
        WrappedEnrichmentFactUpdate {
            discord_id: radiant[0],
            actions_per_min: Some(321),
            courier_kills: Some(2),
            pings: Some(44),
            rapier_count: 2,
            lane_role: Some(2),
            comeback: Some(9_000),
            throw_amount: Some(4_000),
        },
        WrappedEnrichmentFactUpdate {
            discord_id: dire[0],
            actions_per_min: None,
            courier_kills: None,
            pings: None,
            rapier_count: 0,
            lane_role: None,
            comeback: Some(9_000),
            throw_amount: Some(4_000),
        },
        WrappedEnrichmentFactUpdate {
            discord_id: 999_999,
            actions_per_min: Some(9_999),
            courier_kills: None,
            pings: None,
            rapier_count: 9,
            lane_role: None,
            comeback: None,
            throw_amount: None,
        },
    ];
    fixture
        .repository
        .apply_enrichment_atomic(MatchEnrichmentRequest {
            match_id,
            guild_id: Some(GUILD),
            valve_match_id: 8_000_000_001,
            duration_seconds: 2_400,
            radiant_score: 30,
            dire_score: 20,
            game_mode: 2,
            enrichment_data: Some(r#"{"players":[]}"#),
            enrichment_source: Some("manual"),
            enrichment_confidence: None,
            participant_updates: &[],
            wrapped_facts: &initial,
        })
        .expect("write initial compact projection");
    assert_eq!(
        projected_wrapped_facts(&fixture, match_id),
        [
            (
                radiant[0],
                Some(321),
                Some(2),
                Some(44),
                2,
                Some(2),
                Some(9_000),
                Some(4_000),
            ),
            (dire[0], None, None, None, 0, None, Some(9_000), Some(4_000),),
        ]
    );

    let discovery =
        crate::match_discovery_repository::MatchDiscoveryRepository::new(fixture.file.path());
    assert!(
        !discovery
            .wipe_match_enrichment(match_id, Some(GUILD + 1))
            .expect("other guild cannot wipe projection")
    );
    assert_eq!(projected_wrapped_facts(&fixture, match_id).len(), 2);
    assert!(
        discovery
            .wipe_match_enrichment(match_id, Some(GUILD))
            .expect("owning guild wipes projection")
    );
    assert!(projected_wrapped_facts(&fixture, match_id).is_empty());

    let replacement = [
        WrappedEnrichmentFactUpdate {
            discord_id: radiant[0],
            actions_per_min: Some(999),
            courier_kills: Some(2),
            pings: Some(44),
            rapier_count: 2,
            lane_role: Some(2),
            comeback: Some(123),
            throw_amount: Some(456),
        },
        WrappedEnrichmentFactUpdate {
            discord_id: dire[0],
            actions_per_min: None,
            courier_kills: None,
            pings: None,
            rapier_count: 0,
            lane_role: None,
            comeback: Some(123),
            throw_amount: Some(456),
        },
    ];
    fixture
        .repository
        .apply_enrichment_atomic(MatchEnrichmentRequest {
            match_id,
            guild_id: Some(GUILD),
            valve_match_id: 8_000_000_002,
            duration_seconds: 2_400,
            radiant_score: 30,
            dire_score: 20,
            game_mode: 2,
            enrichment_data: Some(r#"{"players":[]}"#),
            enrichment_source: Some("manual"),
            enrichment_confidence: None,
            participant_updates: &[],
            wrapped_facts: &replacement,
        })
        .expect("write replacement projection");
    assert_eq!(
        projected_wrapped_facts(&fixture, match_id),
        [
            (
                radiant[0],
                Some(999),
                Some(2),
                Some(44),
                2,
                Some(2),
                Some(123),
                Some(456),
            ),
            (dire[0], None, None, None, 0, None, Some(123), Some(456),),
        ]
    );
}

#[test]
fn test_complete_enrichment_marks_replay_pending_in_same_transaction() {
    let fixture = Fixture::new();
    let (radiant, dire) = teams(38_001);
    let participants = [radiant.clone(), dire.clone()].concat();
    fixture.seed(&participants);
    let match_id = insert_recorded_match(&fixture, &radiant, &dire);
    let updates = participants
        .iter()
        .enumerate()
        .map(|(index, discord_id)| fantasy_update(*discord_id, 10.0 + index as f64))
        .collect::<Vec<_>>();
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER fail_replay_marker
             BEFORE INSERT ON openskill_replay_jobs BEGIN
               SELECT RAISE(ABORT,'injected replay marker failure');
             END;",
        )
        .expect("install marker failure trigger");
    let failed = fixture
        .repository
        .apply_enrichment_atomic(MatchEnrichmentRequest {
            match_id,
            guild_id: Some(GUILD),
            valve_match_id: 777,
            duration_seconds: 2_400,
            radiant_score: 30,
            dire_score: 20,
            game_mode: 22,
            enrichment_data: None,
            enrichment_source: Some("test"),
            enrichment_confidence: Some(1.0),
            participant_updates: &updates,
            wrapped_facts: &[],
        });
    assert!(failed.is_err());
    let connection = fixture.connection();
    let state: (Option<i64>, i64, i64) = connection
        .query_row(
            "SELECT m.valve_match_id,
                    (SELECT COUNT(*) FROM match_participants
                      WHERE match_id=m.match_id AND fantasy_points IS NOT NULL),
                    (SELECT COUNT(*) FROM openskill_replay_jobs WHERE guild_id=?2)
               FROM matches AS m WHERE m.match_id=?1",
            params![match_id, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("inspect rolled-back enrichment");
    assert_eq!(state, (None, 0, 0));
    connection
        .execute("DROP TRIGGER fail_replay_marker", [])
        .expect("remove marker failure trigger");
    drop(connection);

    assert_eq!(
        fixture
            .repository
            .apply_enrichment_atomic(MatchEnrichmentRequest {
                match_id,
                guild_id: Some(GUILD),
                valve_match_id: 777,
                duration_seconds: 2_400,
                radiant_score: 30,
                dire_score: 20,
                game_mode: 22,
                enrichment_data: None,
                enrichment_source: Some("test"),
                enrichment_confidence: Some(1.0),
                participant_updates: &updates,
                wrapped_facts: &[],
            })
            .expect("commit complete enrichment"),
        10
    );
    let reason: String = fixture
        .connection()
        .query_row(
            "SELECT reason FROM openskill_replay_jobs WHERE guild_id=?1",
            [GUILD],
            |row| row.get(0),
        )
        .expect("durable replay marker");
    assert_eq!(reason, format!("match_enrichment:{match_id}"));
}

fn migrated_award_database() -> NamedTempFile {
    let database = NamedTempFile::new().expect("temporary migrated award database");
    initialize_or_migrate(database.path()).expect("initialize Python-compatible schema");
    database
}

fn insert_award_player(connection: &Connection, discord_id: i64, balance: i64) {
    connection
        .execute_batch("PRAGMA foreign_keys=OFF")
        .expect("disable unrelated legacy foreign keys in fixture");
    connection
        .execute(
            "INSERT INTO players (
                 discord_id,guild_id,discord_username,jopacoin_balance
             ) VALUES (?1,?2,?3,?4)",
            params![discord_id, GUILD, format!("award-{discord_id}"), balance],
        )
        .expect("insert award player");
}

#[test]
fn test_win_bonus_in_guild1_does_not_touch_guild2() {
    const OTHER_GUILD: i64 = 9_002;
    const PLAYER: i64 = 70_000;
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open guild-scoped award fixture");
    insert_award_player(&connection, PLAYER, 200);
    connection
        .execute(
            "INSERT INTO players (
                 discord_id,guild_id,discord_username,jopacoin_balance
             ) VALUES (?1,?2,?3,200)",
            params![PLAYER, OTHER_GUILD, "award-other-guild"],
        )
        .expect("insert same player in other guild");
    drop(connection);

    let receipt = MatchRecordingRepository::new(database.path())
        .credit_income_awards_atomic(
            Some(GUILD),
            &[IncomeAwardRequest {
                discord_id: PLAYER,
                gross: 20,
                source: "match_win_bonus",
                related_type: Some("match_win_bonus"),
                related_id: Some(912),
                reason: "match win bonus",
                exact_once: true,
                ..IncomeAwardRequest::ordinary(PLAYER, 20, "match_win_bonus")
            }],
        )
        .expect("credit match win only in requested guild");
    assert_eq!(receipt[0].balance_after, 220);

    let connection = Connection::open(database.path()).expect("inspect guild-scoped award");
    let balances = connection
        .prepare(
            "SELECT guild_id,jopacoin_balance FROM players
             WHERE discord_id=?1 ORDER BY guild_id",
        )
        .expect("prepare guild balances")
        .query_map([PLAYER], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .expect("read guild balances")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect guild balances");
    assert_eq!(balances, vec![(GUILD, 220), (OTHER_GUILD, 200)]);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries
                 WHERE guild_id=?1 AND account_id=?2
                   AND source='match_win_bonus' AND related_id='912'",
                params![OTHER_GUILD, PLAYER],
                |row| row.get::<_, i64>(0),
            )
            .expect("count cross-guild match-win ledger rows"),
        0
    );
}

#[test]
fn canonical_migrated_sqlite_income_order_and_match_marker_match_python() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open migrated award database");
    insert_award_player(&connection, 70_001, -1_000);
    connection
        .execute(
            "INSERT INTO bankruptcy_state (
                 discord_id,guild_id,last_bankruptcy_at,
                 penalty_games_remaining,bankruptcy_count
             ) VALUES (?1,?2,0,2,1)",
            params![70_001, GUILD],
        )
        .expect("insert bankruptcy state");
    drop(connection);

    let repository = MatchRecordingRepository::new(database.path());
    let award = IncomeAwardRequest {
        discord_id: 70_001,
        gross: 100,
        garnishment_rate: 0.5,
        apply_bankruptcy_penalty: true,
        // Two outstanding games at 12.5% each withhold 25% of the net.
        bankruptcy_penalty_rate_per_game: 0.125,
        vanity_tax_rate: 0.10,
        low_priority_tax_rate: 0.0,
        decrement_bankruptcy_penalty: true,
        source: "match_win_bonus",
        related_type: Some("match_win_bonus"),
        related_id: Some(811),
        reason: "match win bonus",
        exact_once: true,
    };
    let first = repository
        .credit_income_awards_atomic(Some(GUILD), &[award])
        .expect("credit canonical award");
    assert_eq!(
        first,
        vec![IncomeAwardReceipt {
            discord_id: 70_001,
            gross: 100,
            garnished: 50,
            bankruptcy_penalty: 12,
            vanity_tax: 5,
            low_priority_tax: 0,
            net: 33,
            balance_delta: 83,
            balance_after: -917,
            penalty_games_remaining: 1,
            applied: true,
        }]
    );

    let retry = repository
        .credit_income_awards_atomic(Some(GUILD), &[award])
        .expect("retry exact-once award");
    assert_eq!(retry[0].balance_delta, 83);
    assert!(!retry[0].applied);
    let connection = Connection::open(database.path()).expect("reopen migrated award database");
    let state: (i64, i64, i64, i64) = connection
        .query_row(
            "SELECT p.jopacoin_balance,b.penalty_games_remaining,
                    (SELECT COALESCE(SUM(delta),0) FROM economy_ledger_entries
                     WHERE guild_id=?1 AND account_id=?2
                       AND related_type='match_win_bonus' AND related_id='811'),
                    (SELECT COUNT(*) FROM economy_ledger_entries
                     WHERE guild_id=?1 AND account_id=?2
                       AND related_type='match_win_bonus' AND related_id='811')
             FROM players p JOIN bankruptcy_state b
               ON b.discord_id=p.discord_id AND b.guild_id=p.guild_id
             WHERE p.guild_id=?1 AND p.discord_id=?2",
            params![GUILD, 70_001],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read canonical award state");
    assert_eq!(state, (-917, 1, 83, 3));
}

#[test]
fn low_priority_tax_is_debited_beside_the_vanity_tax_row() {
    const PLAYER: i64 = 70_301;
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open low-priority award database");
    insert_award_player(&connection, PLAYER, 0);
    drop(connection);

    let repository = MatchRecordingRepository::new(database.path());
    let receipts = repository
        .credit_income_awards_atomic(
            Some(GUILD),
            &[IncomeAwardRequest {
                vanity_tax_rate: 0.10,
                low_priority_tax_rate: 0.10,
                related_type: Some("match_participation"),
                related_id: Some(931),
                ..IncomeAwardRequest::ordinary(PLAYER, 100, "match_participation")
            }],
        )
        .expect("credit doubly taxed award");
    // Both rates apply to the same 100 JC profit basis rather than the
    // post-vanity remainder.
    assert_eq!(receipts[0].vanity_tax, 10);
    assert_eq!(receipts[0].low_priority_tax, 10);
    assert_eq!(receipts[0].net, 80);
    assert_eq!(receipts[0].balance_delta, 80);
    assert_eq!(receipts[0].balance_after, 80);

    let connection = Connection::open(database.path()).expect("reopen low-priority award database");
    let (vanity_rows, vanity_delta): (i64, i64) = connection
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(delta),0) FROM economy_ledger_entries
             WHERE guild_id=?1 AND account_id=?2 AND source='vanity_tax'",
            params![GUILD, PLAYER],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read vanity tax ledger rows");
    assert_eq!((vanity_rows, vanity_delta), (1, -10));
    let (low_priority_rows, low_priority_delta, reason): (i64, i64, String) = connection
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(delta),0), COALESCE(MAX(reason),'')
             FROM economy_ledger_entries
             WHERE guild_id=?1 AND account_id=?2 AND source='low_priority_tax'",
            params![GUILD, PLAYER],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read low priority tax ledger rows");
    assert_eq!(
        (low_priority_rows, low_priority_delta, reason.as_str()),
        (1, -10, "low priority tax on JC profit")
    );
    let balance: i64 = connection
        .query_row(
            "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
            |row| row.get(0),
        )
        .expect("read taxed balance");
    assert_eq!(balance, 80);
}

#[test]
fn low_priority_tax_never_withholds_more_than_the_profit_basis() {
    const PLAYER: i64 = 70_302;
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open clamped award database");
    insert_award_player(&connection, PLAYER, 500);
    connection
        .execute(
            "INSERT INTO bankruptcy_state (
                 discord_id,guild_id,last_bankruptcy_at,
                 penalty_games_remaining,bankruptcy_count
             ) VALUES (?1,?2,0,2,1)",
            params![PLAYER, GUILD],
        )
        .expect("insert bankruptcy state");
    drop(connection);

    let receipts = MatchRecordingRepository::new(database.path())
        .credit_income_awards_atomic(
            Some(GUILD),
            &[IncomeAwardRequest {
                apply_bankruptcy_penalty: true,
                // Two outstanding games at 25% each withhold half the basis.
                bankruptcy_penalty_rate_per_game: 0.25,
                vanity_tax_rate: 0.40,
                low_priority_tax_rate: 0.25,
                related_type: Some("match_win_bonus"),
                related_id: Some(932),
                ..IncomeAwardRequest::ordinary(PLAYER, 100, "match_win_bonus")
            }],
        )
        .expect("credit over-withheld award");
    // 50 penalty + 40 vanity leaves only 10 of the 100 JC basis, so the
    // 25 JC low-priority share is trimmed instead of overdrawing the profit.
    assert_eq!(receipts[0].bankruptcy_penalty, 50);
    assert_eq!(receipts[0].vanity_tax, 40);
    assert_eq!(receipts[0].low_priority_tax, 10);
    assert_eq!(receipts[0].net, 0);
    assert_eq!(receipts[0].balance_delta, 0);
    assert_eq!(receipts[0].balance_after, 500);
}

#[test]
fn recovered_income_award_receipt_replays_the_low_priority_tax() {
    const PLAYER: i64 = 70_303;
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open recovered award database");
    insert_award_player(&connection, PLAYER, 0);
    drop(connection);

    let repository = MatchRecordingRepository::new(database.path());
    let award = IncomeAwardRequest {
        vanity_tax_rate: 0.10,
        low_priority_tax_rate: 0.10,
        related_type: Some("match_win_bonus"),
        related_id: Some(933),
        exact_once: true,
        ..IncomeAwardRequest::ordinary(PLAYER, 100, "match_win_bonus")
    };
    let first = repository
        .credit_income_awards_atomic(Some(GUILD), &[award])
        .expect("credit exact-once award");
    let retry = repository
        .credit_income_awards_atomic(Some(GUILD), &[award])
        .expect("recover exact-once award");
    assert!(!retry[0].applied);
    assert_eq!(retry[0].low_priority_tax, first[0].low_priority_tax);
    assert_eq!(retry[0].vanity_tax, first[0].vanity_tax);
    assert_eq!(retry[0].balance_delta, first[0].balance_delta);
    assert_eq!(retry[0].net, first[0].net);
}

#[test]
fn low_priority_tax_skips_non_positive_income() {
    const PLAYER: i64 = 70_304;
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open untaxed award database");
    insert_award_player(&connection, PLAYER, 40);
    drop(connection);

    let receipts = MatchRecordingRepository::new(database.path())
        .credit_income_awards_atomic(
            Some(GUILD),
            &[IncomeAwardRequest {
                low_priority_tax_rate: 0.10,
                ..IncomeAwardRequest::ordinary(PLAYER, 0, "match_participation")
            }],
        )
        .expect("credit zero award");
    assert_eq!(receipts[0].low_priority_tax, 0);

    let connection = Connection::open(database.path()).expect("reopen untaxed award database");
    let rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM economy_ledger_entries
             WHERE guild_id=?1 AND account_id=?2 AND source='low_priority_tax'",
            params![GUILD, PLAYER],
            |row| row.get(0),
        )
        .expect("count low priority tax rows");
    assert_eq!(rows, 0);
}

#[test]
fn exact_once_income_marker_and_recovery_are_isolated_by_source() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open migrated award database");
    insert_award_player(&connection, 70_002, 0);
    drop(connection);

    let repository = MatchRecordingRepository::new(database.path());
    let participation = IncomeAwardRequest {
        discord_id: 70_002,
        gross: 100,
        vanity_tax_rate: 0.10,
        source: "match_participation",
        related_type: Some("match"),
        related_id: Some(812),
        reason: "match participation bonus",
        exact_once: true,
        ..IncomeAwardRequest::ordinary(70_002, 100, "match_participation")
    };
    let streaming = IncomeAwardRequest {
        gross: 50,
        source: "match_streaming_bonus",
        reason: "match streaming bonus",
        ..participation
    };
    let first = repository
        .credit_income_awards_atomic(Some(GUILD), &[participation, streaming])
        .expect("credit two independent match awards");
    assert_eq!(first[0].balance_delta, 90);
    assert_eq!(first[1].balance_delta, 45);
    assert!(first.iter().all(|receipt| receipt.applied));

    let retry = repository
        .credit_income_awards_atomic(Some(GUILD), &[streaming, participation])
        .expect("recover each award by its source marker");
    assert_eq!(retry[0].balance_delta, 45);
    assert_eq!(retry[0].vanity_tax, 5);
    assert_eq!(retry[1].balance_delta, 90);
    assert_eq!(retry[1].vanity_tax, 10);
    assert!(retry.iter().all(|receipt| !receipt.applied));
    let balance: i64 = Connection::open(database.path())
        .expect("reopen exact-once database")
        .query_row(
            "SELECT jopacoin_balance FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![70_002, GUILD],
            |row| row.get(0),
        )
        .expect("read exactly-once balance");
    assert_eq!(balance, 135);
}

#[test]
fn live_migrated_sqlite_batch_rechecks_debt_and_rolls_back_as_one_unit() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open migrated award database");
    insert_award_player(&connection, 70_101, -5);
    drop(connection);
    let repository = MatchRecordingRepository::new(database.path());
    let award = IncomeAwardRequest {
        garnishment_rate: 0.5,
        reason: "participation reward",
        ..IncomeAwardRequest::ordinary(70_101, 10, "match_participation")
    };
    let receipts = repository
        .credit_income_awards_atomic(Some(GUILD), &[award, award])
        .expect("credit ordered live-balance awards");
    assert_eq!(receipts[0].garnished, 5);
    assert_eq!(receipts[1].garnished, 0);
    assert_eq!(receipts[1].balance_after, 15);

    let missing = IncomeAwardRequest::ordinary(99_999, 10, "match_participation");
    let failed = repository.credit_income_awards_atomic(Some(GUILD), &[award, missing]);
    assert!(matches!(
        failed,
        Err(MatchRecordingRepositoryError::MissingPlayer(99_999))
    ));
    let balance: i64 = Connection::open(database.path())
        .expect("reopen rolled-back award database")
        .query_row(
            "SELECT jopacoin_balance FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![70_101, GUILD],
            |row| row.get(0),
        )
        .expect("read balance after rollback");
    assert_eq!(balance, 15);
}

#[test]
fn penalty_base_uses_live_positive_balance_without_stale_garnishment_peek() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open positive penalty fixture");
    insert_award_player(&connection, 70_111, 50);
    connection
        .execute(
            "INSERT INTO bankruptcy_state(
                 discord_id,guild_id,last_bankruptcy_at,penalty_games_remaining,bankruptcy_count
             ) VALUES (?1,?2,0,2,1)",
            params![70_111, GUILD],
        )
        .expect("positive penalty state");
    drop(connection);
    let receipt = MatchRecordingRepository::new(database.path())
        .credit_income_awards_atomic(
            Some(GUILD),
            &[IncomeAwardRequest {
                garnishment_rate: 0.5,
                apply_bankruptcy_penalty: true,
                // Two outstanding games at 25% each withhold half the net.
                bankruptcy_penalty_rate_per_game: 0.25,
                ..IncomeAwardRequest::ordinary(70_111, 10, "match_participation")
            }],
        )
        .expect("positive live penalty award")[0];
    assert_eq!(
        (receipt.garnished, receipt.bankruptcy_penalty, receipt.net),
        (0, 5, 5)
    );
}

#[test]
fn penalty_base_uses_atomic_garnished_net_while_player_is_in_debt() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open debt penalty fixture");
    insert_award_player(&connection, 70_112, -100);
    connection
        .execute(
            "INSERT INTO bankruptcy_state(
                 discord_id,guild_id,last_bankruptcy_at,penalty_games_remaining,bankruptcy_count
             ) VALUES (?1,?2,0,2,1)",
            params![70_112, GUILD],
        )
        .expect("debt penalty state");
    drop(connection);
    let receipt = MatchRecordingRepository::new(database.path())
        .credit_income_awards_atomic(
            Some(GUILD),
            &[IncomeAwardRequest {
                garnishment_rate: 0.5,
                apply_bankruptcy_penalty: true,
                // Two outstanding games at 25% each withhold half the net.
                bankruptcy_penalty_rate_per_game: 0.25,
                ..IncomeAwardRequest::ordinary(70_112, 10, "match_participation")
            }],
        )
        .expect("debt live penalty award")[0];
    assert_eq!(
        (receipt.garnished, receipt.bankruptcy_penalty, receipt.net),
        (5, 2, 3)
    );
}

#[test]
fn race_fix3_participation_without_garnishment_is_atomic() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open race participation fixture");
    insert_award_player(&connection, 70_113, 0);
    drop(connection);
    let repository = MatchRecordingRepository::new(database.path());
    let award = IncomeAwardRequest::ordinary(70_113, 10, "match_participation");
    let missing = IncomeAwardRequest::ordinary(70_999, 10, "match_participation");
    assert!(matches!(
        repository.credit_income_awards_atomic(Some(GUILD), &[award, missing]),
        Err(MatchRecordingRepositoryError::MissingPlayer(70_999))
    ));
    let balance: i64 = Connection::open(database.path())
        .expect("reopen race participation fixture")
        .query_row(
            "SELECT jopacoin_balance FROM players WHERE guild_id=?1 AND discord_id=?2",
            params![GUILD, 70_113],
            |row| row.get(0),
        )
        .expect("read rolled-back participation");
    assert_eq!(balance, 0);
}

#[test]
fn race_fix3_participation_reports_net_without_garnishment_service() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open race net fixture");
    insert_award_player(&connection, 70_114, 20);
    drop(connection);
    let receipt = MatchRecordingRepository::new(database.path())
        .credit_income_awards_atomic(
            Some(GUILD),
            &[IncomeAwardRequest::ordinary(
                70_114,
                10,
                "match_participation",
            )],
        )
        .expect("no-garnishment participation")[0];
    assert_eq!((receipt.gross, receipt.garnished, receipt.net), (10, 0, 10));
}

#[test]
fn race_fix4_garnishment_runs_before_bankruptcy_penalty() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open race ordering fixture");
    insert_award_player(&connection, 70_115, -100);
    connection
        .execute(
            "INSERT INTO bankruptcy_state(
                 discord_id,guild_id,last_bankruptcy_at,penalty_games_remaining,bankruptcy_count
             ) VALUES (?1,?2,0,2,1)",
            params![70_115, GUILD],
        )
        .expect("race ordering bankruptcy");
    drop(connection);
    let receipt = MatchRecordingRepository::new(database.path())
        .credit_income_awards_atomic(
            Some(GUILD),
            &[IncomeAwardRequest {
                garnishment_rate: 0.5,
                apply_bankruptcy_penalty: true,
                // Two outstanding games at 25% each withhold half the net.
                bankruptcy_penalty_rate_per_game: 0.25,
                ..IncomeAwardRequest::ordinary(70_115, 10, "match_win_bonus")
            }],
        )
        .expect("ordered garnishment and penalty")[0];
    assert_eq!(
        (receipt.garnished, receipt.bankruptcy_penalty, receipt.net),
        (5, 2, 3)
    );
}

#[test]
fn race_fix4_garnishment_matches_positive_balance_ordering() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open race positive fixture");
    insert_award_player(&connection, 70_116, 100);
    drop(connection);
    let receipt = MatchRecordingRepository::new(database.path())
        .credit_income_awards_atomic(
            Some(GUILD),
            &[IncomeAwardRequest {
                garnishment_rate: 0.75,
                ..IncomeAwardRequest::ordinary(70_116, 10, "match_participation")
            }],
        )
        .expect("positive ordering")[0];
    assert_eq!((receipt.garnished, receipt.net), (0, 10));
}

fn ordinary_garnishment_receipt(
    balance: i64,
    gross: i64,
    rate: f64,
    guild_id: i64,
) -> (NamedTempFile, IncomeAwardReceipt) {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open garnishment database");
    insert_award_player(&connection, 71_001, balance);
    drop(connection);
    let receipt = MatchRecordingRepository::new(database.path())
        .credit_income_awards_atomic(
            Some(guild_id),
            &[IncomeAwardRequest {
                garnishment_rate: rate,
                ..IncomeAwardRequest::ordinary(71_001, gross, "participation")
            }],
        )
        .expect("credit garnishable income")
        .into_iter()
        .next()
        .expect("one garnishment receipt");
    (database, receipt)
}

#[test]
fn test_no_garnishment_when_positive_balance() {
    let (_, receipt) = ordinary_garnishment_receipt(3, 100, 1.0, GUILD);
    assert_eq!(
        (receipt.gross, receipt.garnished, receipt.net),
        (100, 0, 100)
    );
    assert_eq!(receipt.balance_after, 103);
}

#[test]
fn test_full_garnishment_when_in_debt() {
    let (_, receipt) = ordinary_garnishment_receipt(-50, 30, 1.0, GUILD);
    assert_eq!((receipt.gross, receipt.garnished, receipt.net), (30, 30, 0));
    assert_eq!(receipt.balance_after, -20);
}

#[test]
fn test_partial_garnishment_with_50_percent_rate() {
    let (_, receipt) = ordinary_garnishment_receipt(-100, 40, 0.5, GUILD);
    assert_eq!(
        (receipt.gross, receipt.garnished, receipt.net),
        (40, 20, 20)
    );
    assert_eq!(receipt.balance_after, -60);
}

#[test]
fn test_zero_income_no_garnishment() {
    let (_, receipt) = ordinary_garnishment_receipt(-50, 0, 1.0, GUILD);
    assert_eq!((receipt.gross, receipt.garnished, receipt.net), (0, 0, 0));
    assert_eq!(receipt.balance_after, -50);
}

#[test]
fn test_negative_income_no_garnishment() {
    let (_, receipt) = ordinary_garnishment_receipt(-50, -20, 1.0, GUILD);
    assert_eq!(
        (receipt.gross, receipt.garnished, receipt.net),
        (-20, 0, -20)
    );
    assert_eq!(receipt.balance_after, -50);
}

#[test]
fn test_income_pays_off_debt_completely() {
    let (_, receipt) = ordinary_garnishment_receipt(-30, 50, 1.0, GUILD);
    assert_eq!((receipt.gross, receipt.garnished, receipt.net), (50, 50, 0));
    assert_eq!(receipt.balance_after, 20);
}

#[test]
fn test_exactly_at_zero_balance() {
    let (_, receipt) = ordinary_garnishment_receipt(0, 100, 1.0, GUILD);
    assert_eq!(
        (receipt.gross, receipt.garnished, receipt.net),
        (100, 0, 100)
    );
    assert_eq!(receipt.balance_after, 100);
}

#[test]
fn test_custom_garnishment_rate() {
    let (_, receipt) = ordinary_garnishment_receipt(-100, 40, 0.75, GUILD);
    assert_eq!((receipt.garnished, receipt.net), (30, 10));
}

#[test]
fn leveraged_bet_income_is_garnished_while_in_debt() {
    let (_, receipt) = ordinary_garnishment_receipt(-100, 20, 0.5, GUILD);
    assert_eq!(
        (receipt.gross, receipt.garnished, receipt.net),
        (20, 10, 10)
    );
    assert_eq!(receipt.balance_after, -80);
}

#[test]
fn leveraged_bet_income_skips_garnishment_with_positive_balance() {
    let (_, receipt) = ordinary_garnishment_receipt(10, 20, 0.5, GUILD);
    assert_eq!((receipt.gross, receipt.garnished, receipt.net), (20, 0, 20));
    assert_eq!(receipt.balance_after, 30);
}

#[test]
fn leveraged_bet_income_uses_configured_garnishment_service_rate() {
    let (_, receipt) = ordinary_garnishment_receipt(-50, 10, 0.75, GUILD);
    assert_eq!((receipt.gross, receipt.garnished, receipt.net), (10, 7, 3));
    assert_eq!(receipt.balance_after, -40);
}

#[test]
fn test_zero_garnishment_rate() {
    let (_, receipt) = ordinary_garnishment_receipt(-100, 50, 0.0, GUILD);
    assert_eq!((receipt.garnished, receipt.net), (0, 50));
    assert_eq!(receipt.balance_after, -50);
}

#[test]
fn test_bulk_income_uses_one_transaction_and_live_duplicate_balance() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open bulk garnishment database");
    insert_award_player(&connection, 71_001, -3);
    drop(connection);
    let award = IncomeAwardRequest {
        garnishment_rate: 0.5,
        ..IncomeAwardRequest::ordinary(71_001, 4, "participation")
    };
    let receipts = MatchRecordingRepository::new(database.path())
        .credit_income_awards_atomic(Some(GUILD), &[award, award])
        .expect("credit duplicate player in one transaction");
    assert_eq!(
        receipts
            .iter()
            .map(|receipt| receipt.garnished)
            .collect::<Vec<_>>(),
        [2, 0]
    );
    assert_eq!(
        receipts
            .iter()
            .map(|receipt| receipt.net)
            .collect::<Vec<_>>(),
        [2, 4]
    );
    assert_eq!(receipts[1].balance_after, 5);
}

#[test]
fn test_bulk_income_rolls_back_all_players_on_error() {
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open rollback garnishment database");
    insert_award_player(&connection, 71_001, 3);
    drop(connection);
    let awards = [
        IncomeAwardRequest::ordinary(71_001, 10, "participation"),
        IncomeAwardRequest::ordinary(999_999, 10, "participation"),
    ];
    assert!(matches!(
        MatchRecordingRepository::new(database.path())
            .credit_income_awards_atomic(Some(GUILD), &awards),
        Err(MatchRecordingRepositoryError::MissingPlayer(999_999))
    ));
    let balance: i64 = Connection::open(database.path())
        .expect("reopen rollback garnishment database")
        .query_row(
            "SELECT jopacoin_balance FROM players
             WHERE discord_id=71001 AND guild_id=?1",
            [GUILD],
            |row| row.get(0),
        )
        .expect("read rolled-back garnishment balance");
    assert_eq!(balance, 3);
}

#[test]
fn test_garnishment_uses_correct_guild_balance() {
    const OTHER_GUILD: i64 = 9_002;
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open guild garnishment database");
    insert_award_player(&connection, 71_001, -50);
    connection
        .execute(
            "INSERT INTO players(discord_id,guild_id,discord_username,jopacoin_balance)
             VALUES(71001,?1,'other-guild',100)",
            [OTHER_GUILD],
        )
        .expect("insert other-guild player");
    drop(connection);
    let repository = MatchRecordingRepository::new(database.path());
    let award = IncomeAwardRequest {
        garnishment_rate: 1.0,
        ..IncomeAwardRequest::ordinary(71_001, 30, "participation")
    };
    let indebted = repository
        .credit_income_awards_atomic(Some(GUILD), &[award])
        .expect("credit indebted guild");
    let positive = repository
        .credit_income_awards_atomic(Some(OTHER_GUILD), &[award])
        .expect("credit positive guild");
    assert_eq!(indebted[0].garnished, 30);
    assert_eq!(positive[0].garnished, 0);
}

/// (lane_role, last_hits_at_10) for a standard lineup, applied to each side.
const ENRICHMENT_LANES: [(i64, i64); 5] =
    [(1, 4_000), (2, 3_800), (3, 3_200), (3, 1_800), (1, 1_500)];

fn lane_participant_updates(all_ids: &[i64]) -> Vec<EnrichmentParticipantUpdate> {
    all_ids
        .iter()
        .enumerate()
        .map(|(index, discord_id)| {
            let (lane_role, last_hits_at_10) = ENRICHMENT_LANES[index % 5];
            EnrichmentParticipantUpdate {
                discord_id: *discord_id,
                hero_id: Some(discord_id % 100 + 1),
                kills: None,
                deaths: None,
                assists: None,
                gpm: None,
                xpm: None,
                hero_damage: None,
                tower_damage: None,
                last_hits: None,
                denies: None,
                net_worth: None,
                hero_healing: None,
                lane_role: Some(lane_role),
                lane_efficiency: None,
                gold_at_10: None,
                last_hits_at_10: Some(last_hits_at_10),
                towers_killed: None,
                roshans_killed: None,
                teamfight_participation: None,
                obs_placed: None,
                sen_placed: None,
                camps_stacked: None,
                rune_pickups: None,
                firstblood_claimed: None,
                stuns: None,
                fantasy_points: None,
            }
        })
        .collect()
}

fn enrich_with_lanes(
    fixture: &Fixture,
    match_id: i64,
    updates: &[EnrichmentParticipantUpdate],
) -> usize {
    fixture
        .repository
        .apply_enrichment_atomic(MatchEnrichmentRequest {
            match_id,
            guild_id: Some(GUILD),
            valve_match_id: 9_000_000_001,
            duration_seconds: 2_100,
            radiant_score: 30,
            dire_score: 20,
            game_mode: 2,
            enrichment_data: Some(r#"{"stub":true}"#),
            enrichment_source: Some("manual"),
            enrichment_confidence: None,
            participant_updates: updates,
            wrapped_facts: &[],
        })
        .expect("atomic enrichment")
}

fn stored_derived_roles(fixture: &Fixture, match_id: i64) -> Vec<(i64, Option<String>)> {
    fixture
        .connection()
        .prepare(
            "SELECT discord_id, derived_role FROM match_participants
              WHERE match_id = ?1 AND guild_id = ?2 ORDER BY discord_id",
        )
        .expect("prepare derived read")
        .query_map(params![match_id, GUILD], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("query derived roles")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect derived roles")
}

#[test]
fn test_enrichment_derives_played_roles_in_the_same_transaction() {
    let fixture = Fixture::new();
    let radiant: Vec<i64> = (0..5).map(|index| 4_100 + index).collect();
    let dire: Vec<i64> = (0..5).map(|index| 4_200 + index).collect();
    let all_ids = [radiant.clone(), dire.clone()].concat();
    fixture.seed(&all_ids);
    let match_id = insert_recorded_match(&fixture, &radiant, &dire);

    // New matches must get a played role without waiting for a backfill pass.
    enrich_with_lanes(&fixture, match_id, &lane_participant_updates(&all_ids));

    let roles = stored_derived_roles(&fixture, match_id);
    let by_id: std::collections::BTreeMap<i64, Option<String>> = roles.into_iter().collect();
    for base in [4_100_i64, 4_200] {
        assert_eq!(by_id[&base], Some("1".to_owned()));
        assert_eq!(by_id[&(base + 1)], Some("2".to_owned()));
        assert_eq!(by_id[&(base + 2)], Some("3".to_owned()));
        assert_eq!(by_id[&(base + 3)], Some("4".to_owned()));
        assert_eq!(by_id[&(base + 4)], Some("5".to_owned()));
    }
}

#[test]
fn test_enrichment_leaves_roles_null_when_lanes_are_ambiguous() {
    let fixture = Fixture::new();
    let radiant: Vec<i64> = (0..5).map(|index| 4_300 + index).collect();
    let dire: Vec<i64> = (0..5).map(|index| 4_400 + index).collect();
    let all_ids = [radiant.clone(), dire.clone()].concat();
    fixture.seed(&all_ids);
    let match_id = insert_recorded_match(&fixture, &radiant, &dire);

    // Two mids on every side: unresolvable, so nothing is written rather than
    // a guess being stored.
    let mut updates = lane_participant_updates(&all_ids);
    for update in &mut updates {
        update.lane_role = Some(2);
    }
    enrich_with_lanes(&fixture, match_id, &updates);

    assert!(
        stored_derived_roles(&fixture, match_id)
            .iter()
            .all(|(_, role)| role.is_none())
    );
}

#[test]
fn reversing_a_win_bonus_reopens_the_exact_once_marker_for_a_later_correction() {
    // Correcting a match away from its original winner and then back must pay
    // the win bonus again. The reversal has to be recorded as a compensation of
    // the original award, or the exact-once marker still stands, the re-award
    // moves no money, and those players lose the bonus permanently while their
    // participant row is snapshotted as if it had been paid.
    const PLAYER: i64 = 70_401;
    const MATCH_ID: i64 = 941;
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open correction award database");
    insert_award_player(&connection, PLAYER, 0);
    connection
        .execute(
            "INSERT INTO matches(
                 match_id,guild_id,winning_team,match_date,win_reward_jc,
                 team1_players,team2_players
             ) VALUES (?1,?2,1,'2026-08-01T00:00:00+00:00',25,?3,'[]')",
            params![MATCH_ID, GUILD, format!("[{PLAYER}]")],
        )
        .expect("insert corrected match");
    connection
        .execute(
            "INSERT INTO match_participants(
                 match_id,guild_id,discord_id,team_number,won,win_bonus_jc
             ) VALUES (?1,?2,?3,1,1,25)",
            params![MATCH_ID, GUILD, PLAYER],
        )
        .expect("insert corrected participant");
    drop(connection);

    let award = IncomeAwardRequest {
        related_type: Some("match_win_bonus"),
        related_id: Some(MATCH_ID),
        exact_once: true,
        ..IncomeAwardRequest::ordinary(PLAYER, 25, "match_win_bonus")
    };
    let repository = MatchRecordingRepository::new(database.path());
    let credited = repository
        .credit_income_awards_atomic(Some(GUILD), &[award])
        .expect("credit the original win bonus");
    assert!(credited[0].applied, "the first award moves money");
    assert_eq!(credited[0].balance_after, 25);

    // Without a reversal the marker blocks a re-award, which is the intended
    // exact-once behavior.
    let duplicate = repository
        .credit_income_awards_atomic(Some(GUILD), &[award])
        .expect("retry the award");
    assert!(!duplicate[0].applied);

    crate::match_correction_repository::MatchCorrectionRepository::new(database.path())
        .reverse_win_bonuses_atomic(MATCH_ID, Some(GUILD), &BTreeMap::from([(PLAYER, 25)]))
        .expect("reverse the win bonus");

    let recredited = repository
        .credit_income_awards_atomic(Some(GUILD), &[award])
        .expect("re-award after the reversal");
    assert!(
        recredited[0].applied,
        "a reversed award must be creditable again"
    );
    assert_eq!(
        recredited[0].balance_after, 25,
        "the player ends up back at the awarded balance"
    );
}

#[test]
fn recovered_receipt_reports_one_application_after_a_compensate_reapply_cycle() {
    // The saga can pay an award, compensate it when a later player fails, and
    // reapply it on retry. Summing every ledger row for the key would then
    // report twice what the player actually received -- and that figure is
    // snapshotted as the participant's win bonus, so a later correction would
    // debit double.
    const PLAYER: i64 = 70_501;
    const MATCH_ID: i64 = 953;
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open cycle award database");
    insert_award_player(&connection, PLAYER, 0);
    drop(connection);

    let award = IncomeAwardRequest {
        related_type: Some("match_win_bonus"),
        related_id: Some(MATCH_ID),
        exact_once: true,
        ..IncomeAwardRequest::ordinary(PLAYER, 40, "match_win_bonus")
    };
    let repository = MatchRecordingRepository::new(database.path());
    let credited = repository
        .credit_income_awards_atomic(Some(GUILD), &[award])
        .expect("first award");
    assert!(credited[0].applied);
    assert_eq!(credited[0].balance_after, 40);

    repository
        .compensate_income_awards_atomic(
            Some(GUILD),
            &[IncomeAwardCompensation {
                discord_id: PLAYER,
                source: "match_win_bonus",
                related_id: MATCH_ID,
                amount: credited[0].balance_delta,
            }],
        )
        .expect("compensate the partial payout");

    let reapplied = repository
        .credit_income_awards_atomic(Some(GUILD), &[award])
        .expect("reapply on retry");
    assert!(reapplied[0].applied, "the rollback reopened the marker");
    assert_eq!(reapplied[0].balance_after, 40);

    // A later phase fails and the saga re-enters: the recovered receipt must
    // describe one application, not the sum of both.
    let recovered = repository
        .credit_income_awards_atomic(Some(GUILD), &[award])
        .expect("recover the standing award");
    assert!(
        !recovered[0].applied,
        "the standing award is not re-credited"
    );
    assert_eq!(
        recovered[0].balance_delta, credited[0].balance_delta,
        "the recovered movement matches one application"
    );

    // The compensating row is still in the ledger: it is a real balance
    // movement and the audit trail has to keep it.
    let rollbacks: i64 = Connection::open(database.path())
        .expect("reopen cycle award database")
        .query_row(
            "SELECT COUNT(*) FROM economy_ledger_entries
             WHERE guild_id=?1 AND account_id=?2 AND source='match_bonus_rollback'",
            params![GUILD, PLAYER],
            |row| row.get(0),
        )
        .expect("count rollbacks");
    assert_eq!(rollbacks, 1);
}

#[test]
fn bankruptcy_withholding_scales_with_penalty_games_remaining() {
    const PLAYER: i64 = 70_303;
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open scaled award database");
    insert_award_player(&connection, PLAYER, 500);
    connection
        .execute(
            "INSERT INTO bankruptcy_state (
                 discord_id,guild_id,last_bankruptcy_at,
                 penalty_games_remaining,bankruptcy_count
             ) VALUES (?1,?2,0,1,1)",
            params![PLAYER, GUILD],
        )
        .expect("insert bankruptcy state");
    drop(connection);
    let repository = MatchRecordingRepository::new(database.path());
    let award = IncomeAwardRequest {
        apply_bankruptcy_penalty: true,
        bankruptcy_penalty_rate_per_game: 0.05,
        ..IncomeAwardRequest::ordinary(PLAYER, 120, "match_participation")
    };

    // One outstanding game withholds one 5% step of the 120 JC income.
    let receipts = repository
        .credit_income_awards_atomic(Some(GUILD), &[award])
        .expect("credit one-game-left award");
    assert_eq!(receipts[0].bankruptcy_penalty, 6);
    assert_eq!(receipts[0].net, 114);

    // Every further outstanding game adds another 5% until the whole
    // income is withheld at 20 games.
    for (remaining, penalty) in [(3, 18), (6, 36), (20, 120)] {
        Connection::open(database.path())
            .expect("reopen scaled award database")
            .execute(
                "UPDATE bankruptcy_state SET penalty_games_remaining=?1
                 WHERE discord_id=?2 AND guild_id=?3",
                params![remaining, PLAYER, GUILD],
            )
            .expect("extend bankruptcy penalty");
        let receipts = repository
            .credit_income_awards_atomic(Some(GUILD), &[award])
            .expect("credit extended award");
        assert_eq!(receipts[0].bankruptcy_penalty, penalty, "{remaining} games");
        assert_eq!(receipts[0].net, 120 - penalty, "{remaining} games");
    }
}

#[test]
fn compensating_a_win_bonus_restores_the_consumed_penalty_game() {
    const PLAYER: i64 = 70_502;
    const MATCH_ID: i64 = 954;
    let database = migrated_award_database();
    let connection = Connection::open(database.path()).expect("open compensation database");
    insert_award_player(&connection, PLAYER, 500);
    connection
        .execute(
            "INSERT INTO bankruptcy_state (
                 discord_id,guild_id,last_bankruptcy_at,
                 penalty_games_remaining,bankruptcy_count
             ) VALUES (?1,?2,0,2,1)",
            params![PLAYER, GUILD],
        )
        .expect("insert bankruptcy state");
    drop(connection);
    let remaining = || -> i64 {
        Connection::open(database.path())
            .expect("reopen compensation database")
            .query_row(
                "SELECT penalty_games_remaining FROM bankruptcy_state
                 WHERE discord_id=?1 AND guild_id=?2",
                params![PLAYER, GUILD],
                |row| row.get(0),
            )
            .expect("read penalty games")
    };

    let win = IncomeAwardRequest {
        apply_bankruptcy_penalty: true,
        // Two outstanding games at 12.5% each withhold 25% of the bonus.
        bankruptcy_penalty_rate_per_game: 0.125,
        decrement_bankruptcy_penalty: true,
        related_type: Some("match_win_bonus"),
        related_id: Some(MATCH_ID),
        exact_once: true,
        ..IncomeAwardRequest::ordinary(PLAYER, 120, "match_win_bonus")
    };
    let repository = MatchRecordingRepository::new(database.path());
    let credited = repository
        .credit_income_awards_atomic(Some(GUILD), &[win])
        .expect("first win bonus");
    assert_eq!(credited[0].bankruptcy_penalty, 30);
    assert_eq!(remaining(), 1);

    repository
        .compensate_income_awards_atomic(
            Some(GUILD),
            &[IncomeAwardCompensation {
                discord_id: PLAYER,
                source: "match_win_bonus",
                related_id: MATCH_ID,
                amount: credited[0].balance_delta,
            }],
        )
        .expect("compensate the win bonus");
    assert_eq!(remaining(), 2, "the rollback hands the consumed game back");

    let reapplied = repository
        .credit_income_awards_atomic(Some(GUILD), &[win])
        .expect("retry the win bonus");
    assert!(reapplied[0].applied);
    assert_eq!(
        reapplied[0].bankruptcy_penalty, 30,
        "same share as the first attempt"
    );
    assert_eq!(remaining(), 1, "the retry decrements exactly once");

    // A phase that never consumed a game leaves the counter alone.
    let participation = IncomeAwardRequest {
        related_type: Some("match_participation"),
        related_id: Some(MATCH_ID),
        exact_once: true,
        ..IncomeAwardRequest::ordinary(PLAYER, 10, "match_participation")
    };
    let paid = repository
        .credit_income_awards_atomic(Some(GUILD), &[participation])
        .expect("participation");
    repository
        .compensate_income_awards_atomic(
            Some(GUILD),
            &[IncomeAwardCompensation {
                discord_id: PLAYER,
                source: "match_participation",
                related_id: MATCH_ID,
                amount: paid[0].balance_delta,
            }],
        )
        .expect("compensate participation");
    assert_eq!(remaining(), 1);
}
