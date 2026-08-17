use cama_domain::openskill::{CamaOpenSkillSystem, OpenSkillConfig};
use rusqlite::{Connection, params};
use tempfile::TempDir;

use super::*;
use crate::match_correction_repository::{MatchCorrectionError, MatchCorrectionRepository};
use crate::test_support::copy_migrated_database;
use cama_domain::rating::CamaRatingSystem;

const GUILD: i64 = 4242;

fn migrated() -> (TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cama.sqlite3");
    copy_migrated_database(&path).unwrap();
    (directory, path)
}

fn connection(path: &Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    connection
}

fn seed_players(connection: &Connection, guild_id: i64, ids: &[i64], initial_mmr: i64) {
    for discord_id in ids {
        connection
            .execute(
                "INSERT INTO players(
                     discord_id,guild_id,discord_username,initial_mmr,
                     glicko_rating,glicko_rd,glicko_volatility
                 ) VALUES(?1,?2,?3,?4,1500.0,350.0,0.06)",
                params![
                    discord_id,
                    guild_id,
                    format!("player{discord_id}"),
                    initial_mmr
                ],
            )
            .unwrap();
    }
}

#[allow(clippy::too_many_arguments)]
fn seed_replay_match(
    connection: &Connection,
    guild_id: i64,
    match_id: i64,
    radiant: &[i64],
    dire: &[i64],
    winning_team: i64,
    match_date: &str,
    fantasy_points: Option<f64>,
    streak_rate: Option<f64>,
    streak_threshold: Option<i64>,
) {
    assert_eq!(radiant.len(), 5);
    assert_eq!(dire.len(), 5);
    connection
        .execute(
            "INSERT INTO matches(
                 match_id,guild_id,winning_team,match_date,team1_players,team2_players
             ) VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                match_id,
                guild_id,
                winning_team,
                match_date,
                serde_json::to_string(radiant).unwrap(),
                serde_json::to_string(dire).unwrap()
            ],
        )
        .unwrap();
    for (team_number, side, ids) in [(1_i64, "radiant", radiant), (2, "dire", dire)] {
        for discord_id in ids {
            let won = team_number == winning_team;
            connection
                .execute(
                    "INSERT INTO match_participants(
                         match_id,discord_id,guild_id,team_number,side,won,fantasy_points
                     ) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        match_id,
                        discord_id,
                        guild_id,
                        team_number,
                        side,
                        won,
                        fantasy_points
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO rating_history(
                         discord_id,guild_id,match_id,won,
                         streak_multiplier_per_game,streak_threshold
                     ) VALUES(?1,?2,?3,?4,?5,?6)",
                    params![
                        discord_id,
                        guild_id,
                        match_id,
                        won,
                        streak_rate.unwrap_or(0.20),
                        streak_threshold.unwrap_or(3)
                    ],
                )
                .unwrap();
        }
    }
}

fn request_replay(connection: &Connection, guild_id: i64, reason: &str) {
    connection
        .execute(
            "INSERT INTO openskill_replay_jobs(guild_id,reason,requested_at)
             VALUES(?1,?2,CURRENT_TIMESTAMP)
             ON CONFLICT(guild_id) DO UPDATE SET
                 reason=excluded.reason,
                 requested_at=excluded.requested_at,
                 last_error=NULL",
            params![guild_id, reason],
        )
        .unwrap();
}

#[test]
fn reads_player_history_and_comparison_rows_from_migrated_schema() {
    let (_directory, path) = migrated();
    let connection = connection(&path);
    connection.execute(
        "INSERT INTO players(discord_id,guild_id,discord_username,glicko_rating,glicko_rd,os_mu,os_sigma)
         VALUES(1,?1,'Ada',1600,75,31,3)",
        [GUILD],
    ).unwrap();
    connection.execute(
        "INSERT INTO matches(match_id,guild_id,winning_team,match_date,team1_players,team2_players)
         VALUES(10,?1,1,'2026-01-01 12:00:00','[1]','[2]')",
        [GUILD],
    ).unwrap();
    connection.execute(
        "INSERT INTO match_predictions(match_id,expected_radiant_win_prob,openskill_radiant_win_prob,openskill_raw_radiant_win_prob)
         VALUES(10,0.6,0.65,0.7)",
        [],
    ).unwrap();
    connection.execute(
        "INSERT INTO rating_history(discord_id,guild_id,match_id,os_mu_before,os_mu_after,os_sigma_before,os_sigma_after,won,fantasy_weight,streak_length,streak_multiplier)
         VALUES(1,?1,10,30,31,3.2,3,1,1.125,4,1.2)",
        [GUILD],
    ).unwrap();

    let repository = RatingAnalysisRepository::new(
        &path,
        CamaOpenSkillSystem::new(),
        CamaRatingSystem::default(),
        500,
    );
    let player = repository.player(1, Some(GUILD)).unwrap().unwrap();
    assert_eq!(player.glicko_rating, Some(1600.0));
    assert_eq!(player.os_mu, Some(31.0));
    let history = repository
        .player_openskill_history(1, Some(GUILD), 5)
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].won, Some(true));
    assert_eq!(history[0].fantasy_weight, Some(1.125));
    let matches = repository
        .all_matches_with_predictions(Some(GUILD))
        .unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].team1_players, vec![1]);
    assert_eq!(matches[0].openskill_radiant_win_prob, Some(0.65));
}

#[test]
fn configured_backfill_is_durable_and_clears_its_job_atomically() {
    let (_directory, path) = migrated();
    let connection = connection(&path);
    for discord_id in 1_i64..=10 {
        connection
            .execute(
                "INSERT INTO players(discord_id,guild_id,discord_username,initial_mmr)
             VALUES(?1,?2,?3,4000)",
                params![discord_id, GUILD, format!("p{discord_id}")],
            )
            .unwrap();
    }
    connection.execute(
        "INSERT INTO matches(match_id,guild_id,winning_team,match_date,team1_players,team2_players)
         VALUES(99,?1,1,'2026-01-01 12:00:00','[1,2,3,4,5]','[6,7,8,9,10]')",
        [GUILD],
    ).unwrap();
    for discord_id in 1_i64..=10 {
        let (team, side) = if discord_id <= 5 {
            (1, "radiant")
        } else {
            (2, "dire")
        };
        connection.execute(
            "INSERT INTO match_participants(match_id,discord_id,guild_id,team_number,side,fantasy_points)
             VALUES(99,?1,?2,?3,?4,?5)",
            params![discord_id, GUILD, team, side, 10.0 + discord_id as f64],
        ).unwrap();
    }

    let config = OpenSkillConfig {
        performance_strength: 0.9,
        ..OpenSkillConfig::default()
    };
    let system = CamaOpenSkillSystem::with_config(config).unwrap();
    let repository =
        RatingAnalysisRepository::new(&path, system.clone(), CamaRatingSystem::default(), 750);
    let result = repository.backfill_openskill(Some(GUILD)).unwrap();
    assert_eq!(result.matches_processed, 1);
    assert_eq!(result.total_matches, 1);
    assert_eq!(result.players_updated, 10);
    assert!(result.errors.is_empty());
    assert_eq!(repository.pending_replay_reason(Some(GUILD)).unwrap(), None);

    let (mu_before, fingerprint): (f64, String) = connection.query_row(
        "SELECT rh.os_mu_before,p.os_algorithm_fingerprint
         FROM rating_history rh JOIN players p ON p.discord_id=rh.discord_id AND p.guild_id=rh.guild_id
         WHERE rh.guild_id=?1 AND rh.match_id=99 AND rh.discord_id=1",
        [GUILD],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(mu_before, system.mmr_to_os_mu(3250));
    assert_eq!(fingerprint, system.algorithm_fingerprint());
}

#[test]
fn failed_replay_retains_the_durable_job_and_error() {
    let (_directory, path) = migrated();
    let connection = connection(&path);
    connection
        .execute(
            "INSERT INTO players(discord_id,guild_id,discord_username) VALUES(1,?1,'Ada')",
            [GUILD],
        )
        .unwrap();
    connection.execute(
        "INSERT INTO matches(match_id,guild_id,winning_team,match_date,team1_players,team2_players)
         VALUES(100,?1,1,'2026-01-01','[1]','[]')",
        [GUILD],
    ).unwrap();
    let repository = RatingAnalysisRepository::new(
        &path,
        CamaOpenSkillSystem::new(),
        CamaRatingSystem::default(),
        500,
    );
    let result = repository.backfill_openskill(Some(GUILD)).unwrap();
    assert_eq!(result.matches_processed, 0);
    assert!(!result.errors.is_empty());
    assert_eq!(
        repository
            .pending_replay_reason(Some(GUILD))
            .unwrap()
            .as_deref(),
        Some("ratinganalysis:backfill")
    );
    let last_error: String = connection
        .query_row(
            "SELECT last_error FROM openskill_replay_jobs WHERE guild_id=?1",
            [GUILD],
            |row| row.get(0),
        )
        .unwrap();
    assert!(last_error.contains("Match 100: invalid team sizes 1/0"));

    for discord_id in 2_i64..=10 {
        connection
            .execute(
                "INSERT INTO players(discord_id,guild_id,discord_username)
                 VALUES(?1,?2,?3)",
                params![discord_id, GUILD, format!("p{discord_id}")],
            )
            .unwrap();
    }
    connection
        .execute(
            "UPDATE matches
             SET team1_players='[1,2,3,4,5]',team2_players='[6,7,8,9,10]'
             WHERE match_id=100",
            [],
        )
        .unwrap();

    // A new process can safely retry from the durable request. The command
    // reasserts ownership of the job, replays the complete history, and only
    // clears the marker in the same commit that persists ratings.
    let restarted = RatingAnalysisRepository::new(
        &path,
        CamaOpenSkillSystem::new(),
        CamaRatingSystem::default(),
        500,
    );
    let recovered = restarted.backfill_openskill(Some(GUILD)).unwrap();
    assert_eq!(recovered.matches_processed, 1);
    assert!(recovered.errors.is_empty());
    assert_eq!(restarted.pending_replay_reason(Some(GUILD)).unwrap(), None);
}

#[test]
fn replay_moves_winners_up_and_losers_down_from_the_persisted_seed() {
    let (_directory, path) = migrated();
    let connection = connection(&path);
    let radiant = (10_001..10_006).collect::<Vec<_>>();
    let dire = (10_006..10_011).collect::<Vec<_>>();
    seed_players(
        &connection,
        GUILD,
        &radiant.iter().chain(&dire).copied().collect::<Vec<_>>(),
        3_000,
    );
    seed_replay_match(
        &connection,
        GUILD,
        501,
        &radiant,
        &dire,
        1,
        "2026-01-01 12:00:00",
        None,
        None,
        None,
    );
    request_replay(&connection, GUILD, "direction");

    let repository = MatchCorrectionRepository::new(&path);
    let summary = repository
        .replay_openskill_atomic(Some(GUILD))
        .expect("replay valid match");
    assert_eq!(summary.matches_processed, 1);
    assert!(summary.errors.is_empty());

    let system = CamaOpenSkillSystem::default();
    let baseline = system.mmr_to_os_mu(2_500);
    let ratings: Vec<(i64, f64, f64)> = connection
        .prepare(
            "SELECT discord_id,os_mu,os_sigma FROM players
             WHERE guild_id=?1 ORDER BY discord_id",
        )
        .unwrap()
        .query_map([GUILD], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(ratings.len(), 10);
    for (discord_id, mu, sigma) in ratings {
        assert!(sigma < CamaOpenSkillSystem::DEFAULT_SIGMA);
        if radiant.contains(&discord_id) {
            assert!(mu > baseline, "winner {discord_id} did not move up: {mu}");
        } else {
            assert!(mu < baseline, "loser {discord_id} did not move down: {mu}");
        }
    }
}

#[test]
fn replay_passes_one_shared_streak_multiplier_to_every_player() {
    let (_directory, path) = migrated();
    let connection = connection(&path);
    let radiant = (10_101..10_106).collect::<Vec<_>>();
    let dire = (10_106..10_111).collect::<Vec<_>>();
    let all_ids = radiant.iter().chain(&dire).copied().collect::<Vec<_>>();
    seed_players(&connection, GUILD, &all_ids, 3_000);
    for (match_id, date) in [
        (601, "2026-01-01 12:00:00"),
        (602, "2026-01-02 12:00:00"),
        (603, "2026-01-03 12:00:00"),
    ] {
        seed_replay_match(
            &connection,
            GUILD,
            match_id,
            &radiant,
            &dire,
            1,
            date,
            None,
            Some(0.30),
            Some(3),
        );
    }
    request_replay(&connection, GUILD, "shared-streak");

    let repository = MatchCorrectionRepository::new(&path);
    let summary = repository
        .replay_openskill_atomic(Some(GUILD))
        .expect("replay streak history");
    assert_eq!(summary.matches_processed, 3);
    let multipliers: Vec<f64> = connection
        .prepare(
            "SELECT streak_multiplier_per_game FROM rating_history
             WHERE guild_id=?1 AND match_id=603 ORDER BY discord_id",
        )
        .unwrap()
        .query_map([GUILD], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(multipliers.len(), 10);
    assert!(
        multipliers
            .iter()
            .all(|value| (*value - 0.30).abs() < 1e-12)
    );
}

#[test]
fn replay_without_fantasy_records_the_equal_weight_path() {
    let (_directory, path) = migrated();
    let connection = connection(&path);
    let radiant = (10_201..10_206).collect::<Vec<_>>();
    let dire = (10_206..10_211).collect::<Vec<_>>();
    let all_ids = radiant.iter().chain(&dire).copied().collect::<Vec<_>>();
    seed_players(&connection, GUILD, &all_ids, 3_000);
    seed_replay_match(
        &connection,
        GUILD,
        701,
        &radiant,
        &dire,
        2,
        "2026-01-01 12:00:00",
        None,
        None,
        None,
    );
    request_replay(&connection, GUILD, "equal-weight");

    let repository = MatchCorrectionRepository::new(&path);
    let summary = repository
        .replay_openskill_atomic(Some(GUILD))
        .expect("replay no-fantasy match");
    assert_eq!(summary.matches_with_fantasy, 0);
    assert_eq!(summary.matches_equal_weight, 1);
    let weighted_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM rating_history
             WHERE guild_id=?1 AND match_id=701 AND fantasy_weight IS NOT NULL",
            [GUILD],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(weighted_rows, 0);
}

#[test]
fn replay_rebuilds_a_corrupt_history_baseline_from_initial_mmr() {
    let (_directory, path) = migrated();
    let connection = connection(&path);
    let radiant = (10_301..10_306).collect::<Vec<_>>();
    let dire = (10_306..10_311).collect::<Vec<_>>();
    let all_ids = radiant.iter().chain(&dire).copied().collect::<Vec<_>>();
    seed_players(&connection, GUILD, &all_ids, 3_000);
    seed_replay_match(
        &connection,
        GUILD,
        801,
        &radiant,
        &dire,
        1,
        "2026-01-01 12:00:00",
        None,
        None,
        None,
    );
    let corrupt_id = radiant[0];
    connection
        .execute(
            "UPDATE rating_history SET os_mu_before=999.0,os_sigma_before=0.1
             WHERE guild_id=?1 AND match_id=801 AND discord_id=?2",
            params![GUILD, corrupt_id],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE players SET os_mu=NULL,os_sigma=NULL
             WHERE guild_id=?1 AND discord_id=?2",
            params![GUILD, corrupt_id],
        )
        .unwrap();
    request_replay(&connection, GUILD, "repair-baseline");

    let repository = MatchCorrectionRepository::new(&path);
    repository
        .replay_openskill_atomic(Some(GUILD))
        .expect("repair corrupt baseline");
    let expected_mu = CamaOpenSkillSystem::default().mmr_to_os_mu(2_500);
    let stored: (f64, f64, f64, f64) = connection
        .query_row(
            "SELECT os_mu_before,os_sigma_before,os_mu_after,os_sigma_after
             FROM rating_history WHERE guild_id=?1 AND match_id=801 AND discord_id=?2",
            params![GUILD, corrupt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert!((stored.0 - expected_mu).abs() < 1e-12);
    assert_eq!(stored.1, CamaOpenSkillSystem::DEFAULT_SIGMA);
    let live: (f64, f64) = connection
        .query_row(
            "SELECT os_mu,os_sigma FROM players
             WHERE guild_id=?1 AND discord_id=?2",
            params![GUILD, corrupt_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(live.0, stored.2);
    assert_eq!(live.1, stored.3);
}

#[test]
fn missing_match_returns_a_scoped_not_found_error() {
    let (_directory, path) = migrated();
    let repository = MatchCorrectionRepository::new(&path);
    let error = repository
        .current_winner(999_999, Some(GUILD))
        .expect_err("missing match must fail");
    assert!(matches!(
        error,
        MatchCorrectionError::MatchNotFound(999_999)
    ));
}

#[test]
fn backfill_moves_guild_zero_winners_up_and_losers_down_from_reseed() {
    let (_directory, path) = migrated();
    let connection = connection(&path);
    let radiant = (10_401..10_406).collect::<Vec<_>>();
    let dire = (10_406..10_411).collect::<Vec<_>>();
    let all_ids = radiant.iter().chain(&dire).copied().collect::<Vec<_>>();
    seed_players(&connection, 0, &all_ids, 3_000);
    seed_replay_match(
        &connection,
        0,
        901,
        &radiant,
        &dire,
        1,
        "2026-01-01 12:00:00",
        None,
        None,
        None,
    );

    let repository = RatingAnalysisRepository::new(
        &path,
        CamaOpenSkillSystem::new(),
        CamaRatingSystem::default(),
        500,
    );
    let result = repository.backfill_openskill(None).unwrap();
    assert_eq!(
        (
            result.matches_processed,
            result.total_matches,
            result.players_updated,
            result.errors.len()
        ),
        (1, 1, 10, 0)
    );
    let baseline = CamaOpenSkillSystem::default().mmr_to_os_mu(2_500);
    for (discord_id, mu) in connection
        .prepare("SELECT discord_id,os_mu FROM players WHERE guild_id=0 ORDER BY discord_id")
        .unwrap()
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    {
        if radiant.contains(&discord_id) {
            assert!(mu > baseline);
        } else {
            assert!(mu < baseline);
        }
    }
}

#[test]
fn backfill_writes_only_to_the_requested_guild() {
    let (_directory, path) = migrated();
    let connection = connection(&path);
    let radiant = (10_501..10_506).collect::<Vec<_>>();
    let dire = (10_506..10_511).collect::<Vec<_>>();
    let all_ids = radiant.iter().chain(&dire).copied().collect::<Vec<_>>();
    let other_guild = GUILD + 1;
    seed_players(&connection, GUILD, &all_ids, 3_000);
    seed_players(&connection, other_guild, &all_ids, 3_000);
    seed_replay_match(
        &connection,
        GUILD,
        1001,
        &radiant,
        &dire,
        1,
        "2026-01-01 12:00:00",
        None,
        None,
        None,
    );
    let before_other: Vec<(i64, Option<f64>, Option<f64>)> = connection
        .prepare(
            "SELECT discord_id,os_mu,os_sigma FROM players
             WHERE guild_id=?1 ORDER BY discord_id",
        )
        .unwrap()
        .query_map([other_guild], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    let repository = RatingAnalysisRepository::new(
        &path,
        CamaOpenSkillSystem::new(),
        CamaRatingSystem::default(),
        500,
    );
    let result = repository.backfill_openskill(Some(GUILD)).unwrap();
    assert_eq!(result.matches_processed, 1);
    let after_other: Vec<(i64, Option<f64>, Option<f64>)> = connection
        .prepare(
            "SELECT discord_id,os_mu,os_sigma FROM players
             WHERE guild_id=?1 ORDER BY discord_id",
        )
        .unwrap()
        .query_map([other_guild], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(after_other, before_other);
    let target_history: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM rating_history WHERE guild_id=?1 AND match_id=1001",
            [GUILD],
            |row| row.get(0),
        )
        .unwrap();
    let other_history: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM rating_history WHERE guild_id=?1",
            [other_guild],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(target_history, 10);
    assert_eq!(other_history, 0);
}

#[test]
fn backfill_with_no_matches_reports_all_seeded_players_updated() {
    let (_directory, path) = migrated();
    let connection = connection(&path);
    let ids = (10_601..10_611).collect::<Vec<_>>();
    seed_players(&connection, GUILD, &ids, 3_000);
    let repository = RatingAnalysisRepository::new(
        &path,
        CamaOpenSkillSystem::new(),
        CamaRatingSystem::default(),
        500,
    );
    let result = repository.backfill_openskill(Some(GUILD)).unwrap();
    assert_eq!(result.matches_processed, 0);
    assert_eq!(result.total_matches, 0);
    assert_eq!(result.players_updated, ids.len());
    assert!(result.errors.is_empty());
    let seeded: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM players
             WHERE guild_id=?1 AND os_mu IS NOT NULL AND os_sigma IS NOT NULL",
            [GUILD],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(seeded, ids.len() as i64);
}

#[test]
fn backfill_rolls_back_history_prediction_and_live_ratings_on_write_failure() {
    let (_directory, path) = migrated();
    let connection = connection(&path);
    let radiant = (10_701..10_706).collect::<Vec<_>>();
    let dire = (10_706..10_711).collect::<Vec<_>>();
    let all_ids = radiant.iter().chain(&dire).copied().collect::<Vec<_>>();
    seed_players(&connection, GUILD, &all_ids, 3_000);
    seed_replay_match(
        &connection,
        GUILD,
        1101,
        &radiant,
        &dire,
        1,
        "2026-01-01 12:00:00",
        None,
        None,
        None,
    );
    connection
        .execute_batch(
            "CREATE TRIGGER fail_openskill_replay_history
             AFTER UPDATE OF os_mu_after ON rating_history
             WHEN NEW.match_id=1101
             BEGIN SELECT RAISE(ABORT,'injected replay write failure'); END;",
        )
        .unwrap();
    let repository = RatingAnalysisRepository::new(
        &path,
        CamaOpenSkillSystem::new(),
        CamaRatingSystem::default(),
        500,
    );
    let error = repository
        .backfill_openskill(Some(GUILD))
        .expect_err("injected trigger must fail replay");
    assert!(error.to_string().contains("injected replay write failure"));
    assert_eq!(
        repository
            .pending_replay_reason(Some(GUILD))
            .unwrap()
            .as_deref(),
        Some("ratinganalysis:backfill")
    );
    let changed_live: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM players
             WHERE guild_id=?1 AND (os_mu IS NOT NULL OR os_sigma IS NOT NULL)",
            [GUILD],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(changed_live, 0);
    let changed_history: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM rating_history
             WHERE guild_id=?1 AND match_id=1101 AND os_mu_after IS NOT NULL",
            [GUILD],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(changed_history, 0);
    let prediction_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM match_predictions WHERE match_id=1101",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(prediction_rows, 0);
}

#[test]
fn backfill_replays_mixed_matches_and_flushes_one_final_live_snapshot() {
    let (_directory, path) = migrated();
    let connection = connection(&path);
    let radiant = (10_801..10_806).collect::<Vec<_>>();
    let dire = (10_806..10_811).collect::<Vec<_>>();
    let all_ids = radiant.iter().chain(&dire).copied().collect::<Vec<_>>();
    seed_players(&connection, GUILD, &all_ids, 3_000);
    seed_replay_match(
        &connection,
        GUILD,
        1201,
        &radiant,
        &dire,
        1,
        "2026-01-01 12:00:00",
        None,
        None,
        None,
    );
    seed_replay_match(
        &connection,
        GUILD,
        1202,
        &radiant,
        &dire,
        2,
        "2026-01-02 12:00:00",
        Some(15.0),
        None,
        None,
    );
    connection
        .execute_batch(
            "CREATE TABLE replay_player_updates(count INTEGER NOT NULL);
             INSERT INTO replay_player_updates VALUES(0);
             CREATE TRIGGER count_replay_player_updates
             AFTER UPDATE OF os_mu ON players
             WHEN NEW.guild_id=4242
             BEGIN UPDATE replay_player_updates SET count=count+1; END;",
        )
        .unwrap();

    request_replay(&connection, GUILD, "mixed-replay");
    let repository = MatchCorrectionRepository::new(&path);
    let result = repository.replay_openskill_atomic(Some(GUILD)).unwrap();
    assert_eq!(result.matches_processed, 2);
    assert_eq!(result.matches_equal_weight, 1);
    assert_eq!(result.matches_with_fantasy, 1);
    assert_eq!(result.players_updated, 10);
    let history_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM rating_history WHERE guild_id=?1 AND match_id IN (1201,1202)",
            [GUILD],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(history_rows, 20);
    let live_updates: i64 = connection
        .query_row("SELECT count FROM replay_player_updates", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(live_updates, 10);
    let latest_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM players
             WHERE guild_id=?1 AND os_rating_version=5 AND os_algorithm_fingerprint IS NOT NULL",
            [GUILD],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(latest_rows, 10);
}

#[test]
fn guild_scoped_prediction_uses_the_shared_raw_and_calibrated_model() {
    let (_directory, path) = migrated();
    let connection = connection(&path);
    let team1 = (10_901..10_906).collect::<Vec<_>>();
    let team2 = (10_906..10_911).collect::<Vec<_>>();
    let all_ids = team1.iter().chain(&team2).copied().collect::<Vec<_>>();
    let other_guild = GUILD + 1;
    seed_players(&connection, GUILD, &all_ids, 3_000);
    seed_players(&connection, other_guild, &all_ids, 3_000);
    for id in &team1 {
        connection
            .execute(
                "UPDATE players SET os_mu=60.0,os_sigma=4.0
                 WHERE discord_id=?1 AND guild_id=?2",
                params![id, GUILD],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE players SET os_mu=35.0,os_sigma=4.0
                 WHERE discord_id=?1 AND guild_id=?2",
                params![id, other_guild],
            )
            .unwrap();
    }
    for id in &team2 {
        connection
            .execute(
                "UPDATE players SET os_mu=35.0,os_sigma=4.0
                 WHERE discord_id=?1 AND guild_id=?2",
                params![id, GUILD],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE players SET os_mu=60.0,os_sigma=4.0
                 WHERE discord_id=?1 AND guild_id=?2",
                params![id, other_guild],
            )
            .unwrap();
    }

    let repository = RatingAnalysisRepository::new(
        &path,
        CamaOpenSkillSystem::new(),
        CamaRatingSystem::default(),
        500,
    );
    let target = |guild_id| {
        all_ids
            .iter()
            .map(|id| {
                let player = repository.player(*id, Some(guild_id)).unwrap().unwrap();
                (player.os_mu.unwrap(), player.os_sigma.unwrap())
            })
            .collect::<Vec<_>>()
    };
    let target_ratings = target(GUILD);
    let other_ratings = target(other_guild);
    let system = CamaOpenSkillSystem::default();
    let raw_target = system.os_predict_win_probability(
        &target_ratings[..5]
            .iter()
            .map(|(mu, sigma)| cama_domain::openskill::Rating::new(*mu, *sigma))
            .collect::<Vec<_>>(),
        &target_ratings[5..]
            .iter()
            .map(|(mu, sigma)| cama_domain::openskill::Rating::new(*mu, *sigma))
            .collect::<Vec<_>>(),
    );
    let calibrated_target = system.calibrate_win_probability(raw_target, None).unwrap();
    let raw_other = system.os_predict_win_probability(
        &other_ratings[..5]
            .iter()
            .map(|(mu, sigma)| cama_domain::openskill::Rating::new(*mu, *sigma))
            .collect::<Vec<_>>(),
        &other_ratings[5..]
            .iter()
            .map(|(mu, sigma)| cama_domain::openskill::Rating::new(*mu, *sigma))
            .collect::<Vec<_>>(),
    );
    let calibrated_other = system.calibrate_win_probability(raw_other, None).unwrap();
    assert!(raw_target > 0.5);
    assert!(calibrated_target > 0.5);
    assert!(calibrated_target < raw_target);
    assert!(raw_other < 0.5);
    assert!(calibrated_other < 0.5);
}
