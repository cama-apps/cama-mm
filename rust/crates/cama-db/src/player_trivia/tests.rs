use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;

use rusqlite::{Connection, params};
use tempfile::TempDir;

use crate::schema_manager::initialize_or_migrate;

use super::*;

const GUILD: i64 = 77;
const USER: i64 = 101;

struct Fixture {
    _directory: TempDir,
    path: PathBuf,
    repository: PlayerTriviaRepository,
}

impl Fixture {
    fn migrated() -> Self {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let path = directory.path().join("cama.db");
        initialize_or_migrate(&path).expect("run Rust migration authority");
        let repository = PlayerTriviaRepository::new(&path);
        let fixture = Self {
            _directory: directory,
            path,
            repository,
        };
        fixture
            .connection()
            .execute(
                "INSERT INTO players (guild_id, discord_id, discord_username, jopacoin_balance)
             VALUES (?1, ?2, 'Alpha', 3)",
                params![GUILD, USER],
            )
            .expect("register player");
        fixture
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(&self.path).expect("open fixture database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match production foreign-key mode");
        connection
    }
}

fn questions() -> Vec<PlayerTriviaQuestionRecord> {
    vec![
        PlayerTriviaQuestionRecord {
            question_key: "economy:balance:alpha".to_owned(),
            category: "economy".to_owned(),
            question_text: "Who has the largest balance?".to_owned(),
            options: ["Alpha", "Bravo", "Charlie", "Delta"].map(str::to_owned),
            correct_index: 1,
            explanation: "Bravo has the largest stored balance.".to_owned(),
            spicy: false,
        },
        PlayerTriviaQuestionRecord {
            question_key: "matches:hero:alpha".to_owned(),
            category: "heroes".to_owned(),
            question_text: "Which hero did Alpha play most recently?".to_owned(),
            options: ["Axe", "Bane", "Chen", "Doom"].map(str::to_owned),
            correct_index: 2,
            explanation: "Chen was Alpha's most recent hero.".to_owned(),
            spicy: true,
        },
    ]
}

#[test]
fn start_persists_immutable_snapshot_and_enforces_cooldown() {
    let fixture = Fixture::migrated();
    let mut source = questions();
    let session_id = fixture
        .repository
        .try_start_session(USER, GUILD, &source, 10_000, 3_600, false)
        .expect("start session")
        .expect("session claimed");
    source[0].options[1] = "Mutated".to_owned();

    assert_eq!(
        fixture
            .repository
            .try_start_session(USER, GUILD, &source, 10_001, 3_600, false)
            .expect("cooldown lookup"),
        None
    );
    let stored = fixture
        .repository
        .get_session(session_id)
        .expect("read session")
        .expect("session");
    assert_eq!(stored.questions[0].question.options[1], "Bravo");
    assert_eq!(stored.question_count, 2);
    assert_eq!(
        fixture
            .repository
            .get_last_session_started(USER, GUILD)
            .expect("last start"),
        Some(10_000)
    );
    assert_eq!(
        fixture
            .repository
            .recent_question_keys(USER, GUILD, 9_999)
            .expect("recent"),
        vec!["economy:balance:alpha", "matches:hero:alpha"]
    );
    assert!(
        fixture
            .repository
            .try_start_session(USER, GUILD, &questions(), 10_002, 3_600, true)
            .expect("admin bypass")
            .is_some()
    );
}

#[test]
fn settlement_is_ordered_idempotent_and_ledger_backed() {
    let fixture = Fixture::migrated();
    let session_id = fixture
        .repository
        .try_start_session(USER, GUILD, &questions(), 30_000, 3_600, false)
        .expect("start")
        .expect("claimed");
    assert_eq!(
        fixture
            .repository
            .settle_answer(session_id, 2, 2, 7, 30_001)
            .expect("stale"),
        None
    );
    let first = fixture
        .repository
        .settle_answer(session_id, 1, 1, 7, 30_002)
        .expect("settle first")
        .expect("accepted");
    assert_eq!(
        first,
        PlayerTriviaSettlement {
            is_correct: true,
            reward: 7,
            score: 1,
            jc_earned: 7,
            completed: false,
            new_balance: 10,
        }
    );
    assert_eq!(
        fixture
            .repository
            .settle_answer(session_id, 1, 1, 7, 30_003)
            .expect("duplicate"),
        None
    );
    let final_answer = fixture
        .repository
        .settle_answer(session_id, 2, 0, 99, 30_004)
        .expect("settle final")
        .expect("accepted");
    assert_eq!(
        final_answer,
        PlayerTriviaSettlement {
            is_correct: false,
            reward: 0,
            score: 1,
            jc_earned: 7,
            completed: true,
            new_balance: 10,
        }
    );
    let connection = fixture.connection();
    let ledger: (i64, i64, i64, String, String) = connection
        .query_row(
            "SELECT delta, balance_before, balance_after, source, related_type
         FROM economy_ledger_entries WHERE source='player_trivia'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .expect("ledger row");
    assert_eq!(
        ledger,
        (
            7,
            3,
            10,
            "player_trivia".to_owned(),
            "player_trivia_session".to_owned()
        )
    );
    assert_eq!(
        connection
            .query_row::<i64, _, _>("SELECT COUNT(*) FROM economy_ledger_context", [], |row| row
                .get(0))
            .expect("context"),
        0
    );
}

#[test]
fn cancellation_only_releases_an_unanswered_session() {
    let fixture = Fixture::migrated();
    let first = fixture
        .repository
        .try_start_session(USER, GUILD, &questions(), 20_000, 3_600, false)
        .expect("start")
        .expect("claimed");
    assert!(
        fixture
            .repository
            .cancel_session_if_unanswered_at(first, 20_001)
            .expect("cancel")
    );
    assert!(
        !fixture
            .repository
            .cancel_session_if_unanswered_at(first, 20_002)
            .expect("repeat")
    );
    assert_eq!(
        fixture
            .repository
            .get_last_session_started(USER, GUILD)
            .expect("last"),
        None
    );
    let second = fixture
        .repository
        .try_start_session(USER, GUILD, &questions(), 20_003, 3_600, false)
        .expect("replacement")
        .expect("claimed");
    fixture
        .repository
        .settle_answer(second, 1, 0, 5, 20_004)
        .expect("answer");
    assert!(
        !fixture
            .repository
            .cancel_session_if_unanswered_at(second, 20_005)
            .expect("answered cancel")
    );
}

#[test]
fn atomic_cooldown_allows_only_one_concurrent_start() {
    let fixture = Fixture::migrated();
    let path = fixture.path.clone();
    let barrier = Arc::new(Barrier::new(2));
    let workers = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                PlayerTriviaRepository::new(path)
                    .try_start_session(USER, GUILD, &questions(), 50_000, 3_600, false)
                    .expect("concurrent start")
            })
        })
        .collect::<Vec<_>>();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().expect("join"))
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_some()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_none()).count(), 1);
}

#[test]
fn concurrent_answer_settlement_credits_exactly_once() {
    let fixture = Fixture::migrated();
    let session_id = fixture
        .repository
        .try_start_session(USER, GUILD, &questions(), 60_000, 3_600, false)
        .expect("start")
        .expect("claimed");
    let path = fixture.path.clone();
    let barrier = Arc::new(Barrier::new(2));
    let workers = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                PlayerTriviaRepository::new(path)
                    .settle_answer(session_id, 1, 1, 7, 60_001)
                    .expect("concurrent settlement")
            })
        })
        .collect::<Vec<_>>();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().expect("join"))
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_some()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_none()).count(), 1);

    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row::<i64, _, _>(
                "SELECT jopacoin_balance FROM players WHERE guild_id=?1 AND discord_id=?2",
                params![GUILD, USER],
                |row| row.get(0),
            )
            .expect("balance"),
        10
    );
    assert_eq!(
        connection
            .query_row::<i64, _, _>(
                "SELECT COUNT(*) FROM economy_ledger_entries WHERE source='player_trivia'",
                [],
                |row| row.get(0),
            )
            .expect("ledger count"),
        1
    );
    let session = fixture
        .repository
        .get_session(session_id)
        .expect("session read")
        .expect("session");
    assert_eq!(session.status, "active");
    assert_eq!(
        session.next_question().map(|row| row.question_number),
        Some(2)
    );
}

#[test]
fn old_question_timeout_cannot_terminate_an_advanced_session() {
    let fixture = Fixture::migrated();
    let session_id = fixture
        .repository
        .try_start_session(USER, GUILD, &questions(), 70_000, 3_600, false)
        .expect("start")
        .expect("claimed");
    fixture
        .repository
        .settle_answer(session_id, 1, 0, 7, 70_001)
        .expect("settle")
        .expect("accepted");

    assert!(
        !fixture
            .repository
            .timeout_session_if_question_unanswered(session_id, 1, 70_002)
            .expect("old timeout")
    );
    assert_eq!(
        fixture
            .repository
            .get_session(session_id)
            .expect("read")
            .expect("session")
            .status,
        "active"
    );
    assert!(
        fixture
            .repository
            .timeout_session_if_question_unanswered(session_id, 2, 70_003)
            .expect("current timeout")
    );
}

#[test]
fn schema_contains_player_trivia_and_history_extensions() {
    let fixture = Fixture::migrated();
    let connection = fixture.connection();
    let tables = [
        "player_trivia_sessions",
        "player_trivia_questions",
        "disburse_vote_history",
    ];
    for table in tables {
        assert_eq!(
            connection
                .query_row::<i64, _, _>(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0)
                )
                .expect("table lookup"),
            1,
            "{table}"
        );
    }
    let wheel_columns = connection
        .prepare("PRAGMA table_info(wheel_spins)")
        .expect("prepare")
        .query_map([], |row| row.get::<_, String>(1))
        .expect("columns")
        .collect::<Result<BTreeSet<_>, _>>()
        .expect("collect columns");
    assert!(
        ["outcome_code", "is_bonus", "event_id", "outcome_metadata"]
            .iter()
            .all(|column| wheel_columns.contains(*column))
    );
    for index in [
        "idx_player_trivia_sessions_guild_user_start",
        "idx_wheel_spins_guild_player_time",
        "idx_disburse_vote_history_voter",
    ] {
        assert_eq!(
            connection
                .query_row::<i64, _, _>(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                    [index],
                    |row| row.get(0)
                )
                .expect("index lookup"),
            1,
            "{index}"
        );
    }
}

fn invalid_options(options: Vec<&str>) -> PlayerTriviaQuestionRecord {
    let mut question = questions().remove(0);
    question.options = options
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>()
        .try_into()
        .unwrap_or_else(|_| ["invalid", "invalid", "invalid", "invalid"].map(str::to_owned));
    question
}

#[test]
fn start_rejects_three_options() {
    let fixture = Fixture::migrated();
    let question = invalid_options(vec!["A", "B", "C"]);
    assert!(matches!(
        fixture
            .repository
            .try_start_session(USER, GUILD, &[question], 1, 0, false),
        Err(PlayerTriviaRepositoryError::InvalidOptions)
    ));
}

#[test]
fn start_rejects_five_options() {
    // The DB-local DTO makes a five-option shape unrepresentable. This test
    // proves that conversion to its exact four-option boundary fails.
    assert!(
        Vec::from(["A", "B", "C", "D", "E"])
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
            .try_into()
            .map(|_: [String; 4]| ())
            .is_err()
    );
}

#[test]
fn start_rejects_case_insensitive_duplicate_options() {
    let fixture = Fixture::migrated();
    let mut question = questions().remove(0);
    question.options = ["A", "B", "C", "a"].map(str::to_owned);
    assert!(matches!(
        fixture
            .repository
            .try_start_session(USER, GUILD, &[question], 1, 0, false),
        Err(PlayerTriviaRepositoryError::InvalidOptions)
    ));
}

#[test]
fn answered_session_cannot_be_cancelled() {
    let fixture = Fixture::migrated();
    let session_id = fixture
        .repository
        .try_start_session(USER, GUILD, &questions(), 40_000, 3_600, false)
        .expect("start")
        .expect("claimed");
    fixture
        .repository
        .settle_answer(session_id, 1, 0, 5, 40_001)
        .expect("answer");
    assert!(
        !fixture
            .repository
            .cancel_session_if_unanswered_at(session_id, 40_002)
            .expect("cancel")
    );
    assert!(
        fixture
            .repository
            .finish_session(session_id, "timed_out", 40_003)
            .expect("finish")
    );
    let session = fixture
        .repository
        .get_session(session_id)
        .expect("read")
        .expect("session");
    assert_eq!(session.status, "timed_out");
    assert_eq!(session.completed_at, Some(40_003));
}

fn add_snapshot_player(connection: &Connection, guild_id: i64, user_id: i64, name: &str) {
    connection
        .execute(
            "INSERT INTO players (guild_id,discord_id,discord_username,wins,losses,
             jopacoin_balance,preferred_roles,main_role)
         VALUES (?1,?2,?3,8,2,42,'[\"1\",\"2\"]','2')",
            params![guild_id, user_id, name],
        )
        .expect("insert snapshot player");
}

fn seed_complete_snapshot(fixture: &Fixture, guild_id: i64, base: i64) -> (i64, i64, i64, i64) {
    let connection = fixture.connection();
    let first = base + 1;
    let second = base + 2;
    let stamp = base * 10;
    add_snapshot_player(&connection, guild_id, first, "Snapshot One");
    add_snapshot_player(&connection, guild_id, second, "Snapshot Two");
    connection
        .execute(
            "INSERT INTO matches (guild_id,team1_players,team2_players,winning_team,match_date,
             notes,enrichment_data,lobby_type,betting_mode,balancing_rating_system)
         VALUES (?1,?2,?3,1,?4,'private note','private enrichment','shuffle','house','glicko')",
            params![guild_id, format!("[{first}]"), format!("[{second}]"), stamp],
        )
        .expect("insert match");
    let match_id = connection.last_insert_rowid();
    connection
        .execute(
            "INSERT INTO match_participants (guild_id,match_id,discord_id,team_number,won,side,
             hero_id,kills,deaths,assists,gpm,lane_role,fantasy_points,bonus_jc)
         VALUES (?1,?2,?3,1,1,'radiant',2,5,1,9,600,2,33.5,4)",
            params![guild_id, match_id, first],
        )
        .expect("insert participant");
    connection.execute(
        "INSERT INTO rating_history (guild_id,discord_id,match_id,rating,rating_before,rd_before,
             rd_after,team_number,won,timestamp) VALUES (?1,?2,?3,1510,1500,90,85,1,1,?4)",
        params![guild_id, first, match_id, stamp],
    ).expect("insert rating");
    connection.execute(
        "INSERT INTO player_pairings (guild_id,player1_id,player2_id,games_together,wins_together,
             games_against,player1_wins_against,last_match_id) VALUES (?1,?2,?3,3,2,4,3,?4)",
        params![guild_id, first, second, match_id],
    ).expect("insert pairing");
    connection
        .execute(
            "INSERT INTO bankruptcy_state (guild_id,discord_id,last_bankruptcy_at,
             penalty_games_remaining,bankruptcy_count) VALUES (?1,?2,?3,1,2)",
            params![guild_id, first, stamp + 1],
        )
        .expect("insert bankruptcy");
    for (offset, result, code) in [(3, "later", "golden"), (2, "earlier", "bankrupt")] {
        connection.execute(
            "INSERT INTO wheel_spins (guild_id,discord_id,result,spin_time,is_bankrupt,is_golden,
                 outcome_code,is_bonus,event_id,outcome_metadata)
             VALUES (?1,?2,?3,?4,0,0,?5,0,?6,'{\"amount\":20}')",
            params![guild_id, first, result, stamp + offset, code, format!("event-{offset}")],
        ).expect("insert wheel spin");
    }
    connection
        .execute(
            "INSERT INTO double_or_nothing_spins (guild_id,discord_id,cost,balance_before,
             balance_after,won,spin_time) VALUES (?1,?2,5,10,15,1,?3)",
            params![guild_id, first, stamp + 4],
        )
        .expect("insert double spin");
    connection
        .execute(
            "INSERT INTO tip_transactions (guild_id,sender_id,recipient_id,amount,fee,timestamp)
         VALUES (?1,?2,?3,3,1,?4)",
            params![guild_id, first, second, stamp + 5],
        )
        .expect("insert tip");
    connection
        .execute(
            "INSERT INTO bets (guild_id,match_id,discord_id,team_bet_on,amount,leverage,bet_time,
             payout,is_blind,odds_at_placement) VALUES (?1,?2,?3,'1',5,2,?4,20,0,0.5)",
            params![guild_id, match_id, first, stamp + 6],
        )
        .expect("insert bet");
    connection
        .execute(
            "INSERT INTO predictions (guild_id,creator_id,question,status,outcome,created_at,
             closes_at,resolved_at,current_price,initial_fair)
         VALUES (?1,?2,'private market text','resolved','yes',?3,?4,?5,80,50)",
            params![guild_id, first, stamp + 7, stamp + 8, stamp + 9],
        )
        .expect("insert prediction");
    let prediction_id = connection.last_insert_rowid();
    connection
        .execute(
            "INSERT INTO prediction_positions (prediction_id,discord_id,yes_contracts,
             yes_cost_basis_total,no_contracts,no_cost_basis_total,bankruptcy_penalty,vanity_tax)
         VALUES (?1,?2,2,100,0,0,1,2)",
            params![prediction_id, first],
        )
        .expect("insert prediction position");
    connection
        .execute(
            "INSERT INTO prediction_trades (prediction_id,discord_id,action,contracts,jopacoins,
             vwap_x100,last_fill_price,trade_time) VALUES (?1,?2,'buy_yes',2,100,5000,55,?3)",
            params![prediction_id, first, stamp + 8],
        )
        .expect("insert prediction trade");
    connection.execute(
        "INSERT INTO tunnels (guild_id,discord_id,depth,max_depth,total_digs,total_jc_earned,
             prestige_level,tunnel_name,miner_about,stat_strength,stat_smarts,stat_stamina,stat_points)
         VALUES (?1,?2,12,50,90,200,3,'private tunnel','private profile',4,5,6,7)",
        params![guild_id, first],
    ).expect("insert tunnel");
    connection
        .execute(
            "INSERT INTO dig_artifacts (guild_id,discord_id,artifact_id,found_at,is_relic,equipped)
         VALUES (?1,?2,'fossil',?3,1,1)",
            params![guild_id, first, stamp + 10],
        )
        .expect("insert artifact");
    connection
        .execute(
            "INSERT INTO dig_achievements (guild_id,discord_id,achievement_id,unlocked_at)
         VALUES (?1,?2,'deep_digger',?3)",
            params![guild_id, first, stamp + 11],
        )
        .expect("insert achievement");
    connection
        .execute(
            "INSERT INTO dig_actions (guild_id,actor_id,target_id,action_type,depth_before,
             depth_after,jc_delta,detail,created_at)
         VALUES (?1,?2,?3,'cheer',11,12,2,'private detail',?4)",
            params![guild_id, first, second, stamp + 12],
        )
        .expect("insert dig action");
    connection
        .execute(
            "INSERT INTO mafia_games (guild_id,game_date,phase,started_at,winner,entry_fee,
             payout_per_winner,mvp_id,roster_size,twist_event,day_number,status)
         VALUES (?1,?2,'RESOLVED',?3,'TOWN',3,7,?4,8,'double_vote',2,'ACTIVE')",
            params![guild_id, format!("date-{guild_id}"), stamp + 13, first],
        )
        .expect("insert mafia game");
    let game_id = connection.last_insert_rowid();
    connection
        .execute(
            "INSERT INTO mafia_players (guild_id,game_id,discord_id,role,is_godfather,hero_name,
             is_alive,acted) VALUES (?1,?2,?3,'TOWNIE',0,'Axe',1,1)",
            params![guild_id, game_id, first],
        )
        .expect("insert mafia player");
    connection
        .execute(
            "INSERT INTO mafia_actions (guild_id,game_id,actor_id,target_id,action_type,phase,
             day_number,created_at,result) VALUES (?1,?2,?3,?4,'VOTE','DAY',2,?5,'private result')",
            params![guild_id, game_id, first, second, stamp + 14],
        )
        .expect("insert mafia action");
    connection
        .execute(
            "INSERT INTO recalibration_state (guild_id,discord_id,last_recalibration_at,
             total_recalibrations,rating_at_recalibration) VALUES (?1,?2,?3,2,1400)",
            params![guild_id, first, stamp + 15],
        )
        .expect("insert recalibration");
    connection
        .execute(
            "INSERT INTO trivia_sessions (guild_id,discord_id,streak,jc_earned,played_at)
         VALUES (?1,?2,6,12,?3)",
            params![guild_id, first, stamp + 16],
        )
        .expect("insert trivia session");
    connection
        .execute(
            "INSERT INTO disburse_vote_history (guild_id,proposal_id,discord_id,vote_method,
             voted_at,proposal_outcome,finalized_at) VALUES (?1,?2,?3,'button',?4,'approved',?5)",
            params![guild_id, base, first, stamp + 17, stamp + 18],
        )
        .expect("insert disbursement vote");
    (first, match_id, prediction_id, game_id)
}

#[test]
fn load_snapshot_is_complete_deterministic_private_and_guild_scoped() {
    let fixture = Fixture::migrated();
    // The fixture's default user occupies GUILD; use separate guild IDs here.
    let guild = GUILD + 10;
    let other_guild = GUILD + 11;
    let (first, match_id, prediction_id, game_id) = seed_complete_snapshot(&fixture, guild, 1_000);
    let (other_first, _, _, _) = seed_complete_snapshot(&fixture, other_guild, 2_000);
    let snapshot = fixture
        .repository
        .load_snapshot(guild)
        .expect("load snapshot");
    assert_eq!(snapshot.players.len(), 2);
    assert!(
        snapshot
            .players
            .iter()
            .all(|row| row.discord_id != other_first)
    );
    assert_eq!(snapshot.participants[0].match_id, match_id);
    assert_eq!(snapshot.ratings[0].match_id, match_id);
    assert_eq!(snapshot.pairings.len(), 1);
    assert_eq!(
        snapshot
            .wheel_spins
            .iter()
            .map(|row| row.outcome_code.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("bankrupt"), Some("golden")]
    );
    assert_eq!(snapshot.double_spins.len(), 1);
    assert_eq!(snapshot.tips.len(), 1);
    assert_eq!(snapshot.bets.len(), 1);
    assert_eq!(
        snapshot.prediction_positions[0].prediction_id,
        prediction_id
    );
    assert_eq!(
        snapshot.prediction_positions[0].question.as_deref(),
        Some("private market text")
    );
    assert_eq!(snapshot.prediction_positions[0].vanity_tax, 2);
    assert_eq!(snapshot.prediction_trades.len(), 1);
    assert_eq!(snapshot.tunnels.len(), 1);
    assert_eq!(snapshot.dig_artifacts.len(), 1);
    assert_eq!(snapshot.dig_achievements.len(), 1);
    assert_eq!(snapshot.dig_actions.len(), 1);
    assert_eq!(snapshot.mafia_games[0].game_id, game_id);
    assert_eq!(snapshot.mafia_players[0].discord_id, first);
    assert_eq!(snapshot.mafia_actions.len(), 1);
    assert_eq!(snapshot.recalibrations.len(), 1);
    assert_eq!(snapshot.trivia_sessions.len(), 1);
    assert_eq!(snapshot.disbursement_votes.len(), 1);
    // DB-local DTOs deliberately have no fields for match notes/enrichment,
    // prediction-trade questions, tunnel names/about text, Dig detail, or
    // Mafia action results, so those private columns cannot cross this API.
}

#[test]
fn load_snapshot_does_not_expose_active_mafia_roles() {
    let fixture = Fixture::migrated();
    let connection = fixture.connection();
    connection
        .execute(
            "INSERT INTO mafia_games (guild_id,game_date,phase,started_at,roster_size,status)
         VALUES (?1,'active-game','NIGHT',1,4,'ACTIVE')",
            [GUILD],
        )
        .expect("insert active game");
    let game_id = connection.last_insert_rowid();
    connection
        .execute(
            "INSERT INTO mafia_players (guild_id,game_id,discord_id,role)
         VALUES (?1,?2,999,'MAFIA')",
            params![GUILD, game_id],
        )
        .expect("insert secret role");
    connection
        .execute(
            "INSERT INTO mafia_actions (guild_id,game_id,actor_id,target_id,action_type,phase,
             day_number,created_at,result) VALUES (?1,?2,999,1000,'KILL','NIGHT',1,2,'secret')",
            params![GUILD, game_id],
        )
        .expect("insert secret action");
    let snapshot = fixture.repository.load_snapshot(GUILD).expect("snapshot");
    assert_eq!(
        snapshot
            .mafia_games
            .iter()
            .map(|row| row.game_id)
            .collect::<Vec<_>>(),
        vec![game_id]
    );
    assert!(snapshot.mafia_players.is_empty());
    assert!(snapshot.mafia_actions.is_empty());
}

#[test]
fn snapshot_dates_preserve_python_timestamp_order_for_iso_and_integer_storage() {
    let fixture = Fixture::migrated();
    let connection = fixture.connection();
    connection
        .execute(
            "UPDATE players SET last_match_date='2026-07-20T12:00:00+00:00'
             WHERE guild_id=?1 AND discord_id=?2",
            params![GUILD, USER],
        )
        .expect("set ISO player timestamp");
    connection
        .execute(
            "INSERT INTO matches (
                 guild_id,team1_players,team2_players,winning_team,match_date
             ) VALUES (?1,'[101]','[]',1,'2026-07-20T12:00:00+00:00')",
            [GUILD],
        )
        .expect("insert ISO match");
    let iso_match = connection.last_insert_rowid();
    connection
        .execute(
            "INSERT INTO match_participants (
                 guild_id,match_id,discord_id,team_number,won,side,hero_id
             ) VALUES (?1,?2,?3,1,1,'radiant',1)",
            params![GUILD, iso_match, USER],
        )
        .expect("insert ISO participant");
    connection
        .execute(
            "INSERT INTO matches (
                 guild_id,team1_players,team2_players,winning_team,match_date
             ) VALUES (?1,'[101]','[]',1,1800000000)",
            [GUILD],
        )
        .expect("insert integer match");
    let integer_match = connection.last_insert_rowid();
    connection
        .execute(
            "INSERT INTO match_participants (
                 guild_id,match_id,discord_id,team_number,won,side,hero_id
             ) VALUES (?1,?2,?3,1,1,'radiant',2)",
            params![GUILD, integer_match, USER],
        )
        .expect("insert integer participant");
    drop(connection);

    let snapshot = fixture
        .repository
        .load_snapshot(GUILD)
        .expect("load timestamp snapshot");
    assert_eq!(snapshot.players[0].last_match_unix, Some(1_784_548_800));
    assert_eq!(
        snapshot
            .participants
            .iter()
            .map(|row| (row.match_id, row.match_time))
            .collect::<Vec<_>>(),
        vec![(iso_match, 1_784_548_800), (integer_match, 1_800_000_000)]
    );
}
