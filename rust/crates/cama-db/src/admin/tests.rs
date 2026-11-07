use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use crate::test_support::copy_migrated_database;

use super::*;

const GUILD: i64 = 84_001;

struct Fixture {
    database: NamedTempFile,
    repository: AdminRepository,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary Admin repository database");
        copy_migrated_database(database.path()).expect("migrate Python-compatible schema");
        let repository = AdminRepository::new(database.path());
        Self {
            database,
            repository,
        }
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(self.database.path()).expect("open Admin fixture");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("use Python-compatible foreign-key policy");
        connection
    }

    fn player(&self, discord_id: i64, guild_id: i64, rating: Option<f64>, rd: f64, vol: f64) {
        self.connection()
            .execute(
                "INSERT INTO players (
                     discord_id,guild_id,discord_username,glicko_rating,
                     glicko_rd,glicko_volatility
                 ) VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    discord_id,
                    guild_id,
                    format!("player-{discord_id}"),
                    rating,
                    rd,
                    vol
                ],
            )
            .expect("seed Admin player");
    }

    fn rating(&self, discord_id: i64, guild_id: i64) -> (Option<f64>, f64, f64) {
        self.connection()
            .query_row(
                "SELECT glicko_rating,glicko_rd,glicko_volatility FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("load rating")
    }

    fn rating_revision(&self, guild_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT COALESCE(MAX(revision),0) FROM openskill_rating_revisions
                 WHERE guild_id=?1",
                [guild_id],
                |row| row.get(0),
            )
            .expect("load rating revision")
    }
}

#[test]
fn test_set_glicko_uncertainty_invalidates_in_flight_match_recordings() {
    let fixture = Fixture::new();
    fixture.player(1, GUILD, Some(1_500.0), 50.0, 0.06);
    let before = fixture.rating_revision(GUILD);
    fixture
        .repository
        .set_glicko_uncertainty(1, GUILD, 1_400.0, 200.0, 0.07)
        .expect("set uncertainty");
    assert_eq!(fixture.rating(1, GUILD), (Some(1_400.0), 200.0, 0.07));
    // A match recorded from ratings read before the override must retry.
    assert_eq!(fixture.rating_revision(GUILD), before + 1);
    assert!(matches!(
        fixture
            .repository
            .set_glicko_uncertainty(2, GUILD, 1_400.0, 200.0, 0.07),
        Err(AdminRepositoryError::PlayerNotFound)
    ));
    assert_eq!(fixture.rating_revision(GUILD), before + 1);
}

#[test]
fn test_glicko_only_bump_invalidates_in_flight_match_recordings() {
    let fixture = Fixture::new();
    fixture.player(1, GUILD, Some(1_500.0), 50.0, 0.06);
    let before = fixture.rating_revision(GUILD);
    let result = fixture
        .repository
        .bump_uncertainties(GUILD, 100.0, 9)
        .expect("bump uncertainty")
        .expect("summary");
    assert_eq!(result.openskill_count, 0);
    assert_eq!(fixture.rating_revision(GUILD), before + 1);
}

#[test]
fn test_bumps_all_rated_players() {
    let fixture = Fixture::new();
    fixture.player(1, GUILD, Some(1_500.0), 50.0, 0.06);
    fixture.player(2, GUILD, Some(1_200.0), 120.0, 0.06);
    fixture.player(3, GUILD, Some(1_800.0), 80.0, 0.06);

    let result = fixture
        .repository
        .bump_uncertainties(GUILD, 100.0, 9)
        .expect("bump uncertainty")
        .expect("rated-player summary");
    assert_eq!(result.count, 3);
    assert!((result.average_rd_before - (50.0 + 120.0 + 80.0) / 3.0).abs() < 1e-9);
    assert!((result.average_rd_after - (150.0 + 220.0 + 180.0) / 3.0).abs() < 1e-9);
    assert_eq!(fixture.rating(1, GUILD).1, 150.0);
    assert_eq!(fixture.rating(2, GUILD).1, 220.0);
    assert_eq!(fixture.rating(3, GUILD).1, 180.0);
}

#[test]
fn test_caps_rd_at_shared_limit() {
    let fixture = Fixture::new();
    fixture.player(1, GUILD, Some(1_500.0), 300.0, 0.06);
    fixture.player(2, GUILD, Some(1_200.0), 340.0, 0.06);
    let result = fixture
        .repository
        .bump_uncertainties(GUILD, 100.0, 9)
        .expect("bump uncertainty")
        .expect("summary");
    assert_eq!(result.average_rd_after, 250.0);
    assert_eq!(fixture.rating(1, GUILD).1, 250.0);
    assert_eq!(fixture.rating(2, GUILD).1, 250.0);
}

#[test]
fn test_preserves_rating_and_volatility() {
    let fixture = Fixture::new();
    fixture.player(1, GUILD, Some(1_500.0), 50.0, 0.07);
    fixture
        .repository
        .bump_uncertainties(GUILD, 100.0, 9)
        .expect("bump uncertainty");
    assert_eq!(fixture.rating(1, GUILD), (Some(1_500.0), 150.0, 0.07));
}

#[test]
fn test_bumps_openskill_sigma_on_same_display_scale() {
    let fixture = Fixture::new();
    fixture.player(1, GUILD, Some(1_500.0), 50.0, 0.06);
    fixture
        .connection()
        .execute(
            "UPDATE players SET os_mu=45.0,os_sigma=4.0
             WHERE discord_id=1 AND guild_id=?1",
            [GUILD],
        )
        .expect("seed OpenSkill rating");
    let result = fixture
        .repository
        .bump_uncertainties(GUILD, 100.0, 9)
        .expect("bump uncertainty")
        .expect("summary");
    assert_eq!(result.openskill_count, 1);
    let openskill: (f64, f64) = fixture
        .connection()
        .query_row(
            "SELECT os_mu,os_sigma FROM players WHERE discord_id=1 AND guild_id=?1",
            [GUILD],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("load OpenSkill rating");
    assert_eq!(openskill, (45.0, 6.0));
}

#[test]
fn test_skips_unrated_players() {
    let fixture = Fixture::new();
    fixture.player(1, GUILD, Some(1_500.0), 50.0, 0.06);
    fixture.player(2, GUILD, None, 350.0, 0.06);
    let result = fixture
        .repository
        .bump_uncertainties(GUILD, 100.0, 9)
        .expect("bump uncertainty")
        .expect("summary");
    assert_eq!(result.count, 1);
    assert_eq!(fixture.rating(2, GUILD), (None, 350.0, 0.06));
}

#[test]
fn test_returns_none_for_no_rated_players() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture
            .repository
            .bump_uncertainties(GUILD, 100.0, 9)
            .expect("empty uncertainty bump"),
        None
    );
}

#[test]
fn test_guild_isolation() {
    let fixture = Fixture::new();
    fixture.player(1, GUILD, Some(1_500.0), 50.0, 0.06);
    fixture.player(2, 99_999, Some(1_200.0), 60.0, 0.06);
    let result = fixture
        .repository
        .bump_uncertainties(GUILD, 100.0, 9)
        .expect("bump uncertainty")
        .expect("summary");
    assert_eq!(result.count, 1);
    assert_eq!(fixture.rating(2, 99_999).1, 60.0);
}

#[test]
fn transfer_account_recovers_both_guilds_and_can_only_consume_the_source_once() {
    let f = Fixture::new();
    let (old, new) = (100_000_001, 100_000_002);
    f.player(old, GUILD, Some(1700.0), 100.0, 0.06);
    f.player(old, GUILD + 1, Some(1250.0), 110.0, 0.07);
    f.connection()
        .execute(
            "INSERT INTO player_steam_ids(discord_id, steam_id, is_primary, added_at)
         VALUES (?1, 12345, 1, 50), (?1, 67890, 0, 55)",
            [old],
        )
        .unwrap();
    let before = f.rating_revision(GUILD);
    f.repository
        .transfer_account(GUILD, old, new, "Bismi")
        .unwrap();
    let profile = f.repository.player(new, GUILD).unwrap().unwrap();
    assert_eq!(profile.username, "Bismi");
    assert_eq!(profile.glicko_rating, Some(1700.0));
    assert_eq!(f.rating(new, GUILD + 1), (Some(1250.0), 110.0, 0.07));
    assert_eq!(f.rating_revision(GUILD), before + 1);
    assert!(f.repository.player(old, GUILD).unwrap().is_none());
    assert!(
        f.repository
            .transfer_account(GUILD, old, new, "Bismi")
            .is_err()
    );
    let steam = crate::opendota_player::OpenDotaPlayerRepository::new(f.database.path());
    assert_eq!(steam.get_steam_ids(new).unwrap(), [12345, 67890]);
    assert!(steam.get_steam_ids(old).unwrap().is_empty());
}

#[test]
fn transfer_account_checks_guild_destination_and_live_games_before_moving_data() {
    let f = Fixture::new();
    let (old, new) = (100_000_001, 100_000_002);
    f.player(old, GUILD, Some(1500.0), 40.0, 0.06);
    assert!(
        f.repository
            .transfer_account(GUILD + 1, old, new, "Bismi")
            .is_err()
    );
    f.connection()
        .execute(
            "INSERT INTO deadlock_queue(guild_id, discord_id, joined_at) VALUES (?1,?2,0)",
            params![GUILD, old],
        )
        .unwrap();
    assert!(
        f.repository
            .transfer_account(GUILD, old, new, "Bismi")
            .is_err()
    );
    f.connection()
        .execute("DELETE FROM deadlock_queue", [])
        .unwrap();
    f.player(new, GUILD + 1, Some(1000.0), 50.0, 0.07);
    assert!(
        f.repository
            .transfer_account(GUILD, old, new, "Bismi")
            .is_err()
    );
    assert_eq!(f.rating(old, GUILD), (Some(1500.0), 40.0, 0.06));
    assert_eq!(f.rating(new, GUILD + 1), (Some(1000.0), 50.0, 0.07));
    assert_eq!(f.rating_revision(GUILD), 0);
}

#[test]
fn recovery_distinguishes_active_wagers_from_passive_tax_eligibility() {
    let f = Fixture::new();
    let (old, new) = (1111, 2222);
    f.player(old, GUILD, Some(1487.0), 70.0, 0.06);
    let conn = f.connection();
    let pending =
        r#"{"shuffle_timestamp":100,"recording_policy":{"vanity_taxable_ids":[1111,2222]}}"#;
    conn.execute(
        "INSERT INTO pending_matches(pending_match_id,guild_id,payload) VALUES(8,?1,?2)",
        params![GUILD, pending],
    )
    .unwrap();
    conn.execute("INSERT INTO bets(guild_id,discord_id,team_bet_on,amount,bet_time) VALUES(?1,?2,'radiant',5,100)", [GUILD, old]).unwrap();
    assert!(
        f.repository
            .transfer_account(GUILD, old, new, "New player")
            .unwrap_err()
            .to_string()
            .contains("active Dota wager")
    );
    assert_eq!(f.rating(old, GUILD).0, Some(1487.0));
    conn.execute("UPDATE bets SET bet_time=99", []).unwrap();
    f.repository
        .transfer_account(GUILD, old, new, "New player")
        .unwrap();
    assert_eq!(f.rating(new, GUILD).0, Some(1487.0));
    assert_eq!(
        conn.query_row("SELECT payload FROM pending_matches", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        pending
    );
}

#[test]
fn transfer_account_keeps_match_history_wallet_dig_progress_and_pet_ownership() {
    let f = Fixture::new();
    let (old, new) = (123456789012345678_i64, 223456789012345678_i64);
    f.player(old, GUILD, Some(1350.0), 60.0, 0.06);
    let conn = f.connection();
    conn.execute(
        "UPDATE players SET jopacoin_balance=123 WHERE discord_id=?1",
        [old],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO matches(team1_players,team2_players,guild_id) VALUES (?1,'[33]',?2)",
        params![format!("[{old}]"), GUILD],
    )
    .unwrap();
    let match_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO match_participants(match_id,discord_id,guild_id,won) VALUES (?1,?2,?3,1)",
        params![match_id, old, GUILD],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tunnels(discord_id,guild_id,total_digs) VALUES (?1,?2,407)",
        [old, GUILD],
    )
    .unwrap();
    conn.execute("INSERT INTO pets(discord_id,guild_id,name,species,adopted_at,hatched_at,adopt_fee,last_fed_at,hunger_at_last_fed)
        VALUES (?1,?2,'Pip','cat',1,1,5,3,99)", [old,GUILD]).unwrap();
    conn.execute(
        "INSERT INTO player_display_names(discord_id,guild_id,display_name,seen_at)
        VALUES (?1,?2,'Bismi',10)",
        [new, GUILD],
    )
    .unwrap();
    f.repository
        .transfer_account(GUILD, old, new, "Bismi")
        .unwrap();
    assert_eq!(
        f.repository.player(new, GUILD).unwrap().unwrap().balance,
        123
    );
    let history: (i64, i64) = conn
        .query_row(
            "SELECT json_extract(team1_players,'$[0]'),discord_id
        FROM matches JOIN match_participants USING (match_id) WHERE match_id=?1",
            [match_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(history, (new, new));
    assert_eq!(
        conn.query_row(
            "SELECT total_digs FROM tunnels WHERE discord_id=?1",
            [new],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        407
    );
    assert_eq!(
        conn.query_row("SELECT name FROM pets WHERE discord_id=?1", [new], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "Pip"
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM economy_ledger_entries WHERE account_id=?1",
            [old],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn transfer_account_rolls_back_every_change_on_late_failure() {
    let f = Fixture::new();
    let (old, new) = (100_000_001, 100_000_002);
    f.player(old, GUILD, Some(1500.0), 40.0, 0.06);
    let conn = f.connection();
    conn.execute(
        "INSERT INTO tunnels(discord_id,guild_id,total_digs) VALUES (?1,?2,35)",
        [old, GUILD],
    )
    .unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_transfer BEFORE INSERT ON openskill_rating_revisions
        BEGIN SELECT RAISE(ABORT,'injected late failure'); END;",
    )
    .unwrap();
    let err = f
        .repository
        .transfer_account(GUILD, old, new, "Bismi")
        .unwrap_err();
    assert!(err.to_string().contains("injected late failure"), "{err}");
    assert!(f.repository.player(new, GUILD).unwrap().is_none());
    assert!(f.repository.player(old, GUILD).unwrap().is_some());
    assert_eq!(
        conn.query_row("SELECT discord_id FROM tunnels", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        old
    );
}

#[test]
fn concurrent_transfers_can_only_move_the_source_once() {
    use std::sync::{Arc, Barrier};

    let fixture = Fixture::new();
    let old = 123456789012345670_i64;
    let destinations = [old + 1, old + 2];
    fixture.player(old, GUILD, Some(1430.0), 40.0, 0.06);
    let gate = Arc::new(Barrier::new(destinations.len()));
    let jobs = destinations.map(|new| {
        let gate = Arc::clone(&gate);
        let repo = fixture.repository.clone();
        std::thread::spawn(move || {
            gate.wait();
            (new, repo.transfer_account(GUILD, old, new, "Bismi"))
        })
    });
    let outcomes = jobs.map(|job| job.join().unwrap());
    assert_eq!(
        outcomes.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    for (new, result) in outcomes {
        assert_eq!(
            fixture.repository.player(new, GUILD).unwrap().is_some(),
            result.is_ok()
        );
    }
    assert!(fixture.repository.player(old, GUILD).unwrap().is_none());
    assert_eq!(fixture.rating_revision(GUILD), 1);
}

#[test]
fn recovery_closes_orphaned_player_trivia_session_and_preserves_earned_rewards() {
    let fixture = Fixture::new();
    let (old, new) = (100_000_001, 100_000_002);
    fixture.player(old, GUILD, None, 50.0, 0.06);
    let db = fixture.connection();
    db.execute(
        "INSERT INTO player_trivia_sessions(guild_id,discord_id,started_at,score,jc_earned)
        VALUES (?1,?2,1,2,7)",
        [GUILD, old],
    )
    .unwrap();
    fixture
        .repository
        .transfer_account(GUILD, old, new, "Bismi")
        .unwrap();
    let session: (i64, String, i64, i64) = db
        .query_row(
            "SELECT discord_id,status,score,jc_earned FROM player_trivia_sessions",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(session, (new, "cancelled".to_owned(), 2, 7));
}
