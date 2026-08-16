use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use crate::schema_manager::initialize_or_migrate;

use super::*;

const GUILD: i64 = 84_001;

struct Fixture {
    database: NamedTempFile,
    repository: AdminRepository,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary Admin repository database");
        initialize_or_migrate(database.path()).expect("migrate Python-compatible schema");
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
fn test_caps_rd_at_350() {
    let fixture = Fixture::new();
    fixture.player(1, GUILD, Some(1_500.0), 300.0, 0.06);
    fixture.player(2, GUILD, Some(1_200.0), 340.0, 0.06);
    let result = fixture
        .repository
        .bump_uncertainties(GUILD, 100.0, 9)
        .expect("bump uncertainty")
        .expect("summary");
    assert_eq!(result.average_rd_after, 350.0);
    assert_eq!(fixture.rating(1, GUILD).1, 350.0);
    assert_eq!(fixture.rating(2, GUILD).1, 350.0);
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
