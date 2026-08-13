use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const GUILD: i64 = 42_424;

struct Fixture {
    database: NamedTempFile,
    repository: RegistrationRepository,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary registration database");
        Connection::open(database.path())
            .expect("open fixture")
            .execute_batch(FIXTURE_SCHEMA)
            .expect("create disposable schema");
        let repository = RegistrationRepository::new(database.path());
        Self {
            database,
            repository,
        }
    }

    fn connection(&self) -> Connection {
        Connection::open(self.database.path()).expect("open fixture")
    }

    fn add_player(&self, discord_id: i64, roles: Option<&str>) {
        self.connection()
            .execute(
                "INSERT INTO players
                     (discord_id, guild_id, discord_username, preferred_roles)
                 VALUES (?1, ?2, ?3, ?4)",
                params![discord_id, GUILD, format!("P{discord_id}"), roles],
            )
            .expect("insert player");
    }
}

fn roles(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn request(discord_id: i64, steam_id: i64) -> RegisterPlayerRequest<'static> {
    RegisterPlayerRequest {
        discord_id,
        guild_id: Some(GUILD),
        discord_username: "Registrant",
        steam_id,
        dotabuff_url: "https://www.dotabuff.com/players/76561197960389184",
        initial_mmr: 3_000,
        glicko_rating: 1_500.0,
        glicko_rd: 350.0,
        glicko_volatility: 0.06,
        os_mu: 40.0,
        os_sigma: 8.333,
        exclusion_count: 4,
        added_at: 1_700_000_000,
    }
}

#[test]
fn test_set_roles_persists_to_database() {
    let fixture = Fixture::new();
    fixture.add_player(12_346, None);
    let desired = roles(&["1", "2", "3"]);
    assert_eq!(
        fixture
            .repository
            .set_roles_atomic(12_346, Some(GUILD), &desired)
            .expect("set roles"),
        SetRolesOutcome::Updated(desired.clone())
    );
    assert_eq!(
        fixture
            .repository
            .get_roles(12_346, Some(GUILD))
            .expect("read roles"),
        Some(desired)
    );
}

#[test]
fn test_set_roles_updates_existing_roles() {
    let fixture = Fixture::new();
    fixture.add_player(12_347, Some("[\"1\",\"2\"]"));
    let desired = roles(&["4", "5"]);
    fixture
        .repository
        .set_roles_atomic(12_347, Some(GUILD), &desired)
        .expect("update roles");
    assert_eq!(
        fixture
            .repository
            .get_roles(12_347, Some(GUILD))
            .expect("read roles"),
        Some(desired)
    );
}

#[test]
fn test_set_roles_unregistered_player_raises() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture
            .repository
            .set_roles_atomic(99_999, Some(GUILD), &roles(&["1", "2"]))
            .expect("missing player is a typed outcome"),
        SetRolesOutcome::MissingPlayer
    );
}

#[test]
fn test_duplicate_steam_id_rejected_across_players() {
    let fixture = Fixture::new();
    fixture
        .repository
        .register_player_atomic(&request(1, 123_456))
        .expect("first registration");
    assert!(matches!(
        fixture
            .repository
            .register_player_atomic(&request(2, 123_456)),
        Err(RegistrationRepositoryError::SteamAlreadyOwned {
            steam_id: 123_456,
            owner_id: 1
        })
    ));
    assert_eq!(
        fixture.repository.steam_owner(123_456).expect("owner"),
        Some(1)
    );
    assert!(
        !fixture
            .repository
            .player_exists(2, Some(GUILD))
            .expect("loser absent")
    );
}

#[test]
fn test_register_rolls_back_player_on_steam_link_failure() {
    let fixture = Fixture::new();
    // Fixture-only fault injection at the canonical transaction seam.
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER reject_registration_link
             BEFORE INSERT ON player_steam_ids
             WHEN NEW.steam_id = 98765
             BEGIN
                 SELECT RAISE(ABORT, 'simulated concurrent Steam claim');
             END;",
        )
        .expect("install fixture trigger");
    assert!(matches!(
        fixture
            .repository
            .register_player_atomic(&request(42, 98_765)),
        Err(RegistrationRepositoryError::Sqlite(_))
    ));
    assert!(
        !fixture
            .repository
            .player_exists(42, Some(GUILD))
            .expect("rollback")
    );
}

#[test]
fn test_register_populates_junction_table() {
    let fixture = Fixture::new();
    fixture
        .repository
        .register_player_atomic(&request(7, 222_333))
        .expect("register");
    assert_eq!(
        fixture.repository.primary_steam_id(7).expect("primary"),
        Some(222_333)
    );
    let legacy: i64 = fixture
        .connection()
        .query_row(
            "SELECT steam_id FROM players WHERE discord_id = 7 AND guild_id = ?1",
            [GUILD],
            |row| row.get(0),
        )
        .expect("legacy Steam column");
    assert_eq!(legacy, 222_333);
}

#[test]
fn test_high_uint32_account_id_accepted() {
    let fixture = Fixture::new();
    let registered = fixture
        .repository
        .register_player_atomic(&request(9, 3_000_000_000))
        .expect("valid high uint32 account ID");
    assert_eq!(registered.initial_mmr, 3_000);
    assert_eq!(
        fixture.repository.primary_steam_id(9).expect("primary"),
        Some(3_000_000_000)
    );
}

#[test]
fn test_update_and_read_preferred_region() {
    let fixture = Fixture::new();
    fixture.add_player(1, None);

    assert!(
        fixture
            .repository
            .set_preferred_region(1, Some(GUILD), "USW")
            .expect("update preferred region")
    );

    let preferences = fixture
        .repository
        .player_preferences(1, Some(GUILD))
        .expect("read preferences")
        .expect("registered player");
    assert_eq!(preferences.preferred_region.as_deref(), Some("USW"));
    assert_eq!(preferences.inferred_region, None);
}

#[test]
fn test_inferred_region_and_sentinel_round_trip() {
    let fixture = Fixture::new();
    fixture.add_player(2, None);

    assert!(
        fixture
            .repository
            .set_inferred_region(2, Some(GUILD), "USE")
            .expect("set inferred region")
    );
    assert_eq!(
        fixture
            .repository
            .player_preferences(2, Some(GUILD))
            .expect("read preferences")
            .expect("registered player")
            .inferred_region
            .as_deref(),
        Some("USE")
    );

    assert!(
        fixture
            .repository
            .set_inferred_region(2, Some(GUILD), "NONE")
            .expect("set sentinel")
    );
    assert_eq!(
        fixture
            .repository
            .player_preferences(2, Some(GUILD))
            .expect("read sentinel")
            .expect("registered player")
            .inferred_region
            .as_deref(),
        Some("NONE")
    );
}

#[test]
fn test_region_is_guild_scoped() {
    const SECONDARY_GUILD: i64 = 43;

    let fixture = Fixture::new();
    fixture.add_player(3, None);
    fixture
        .connection()
        .execute(
            "INSERT INTO players (discord_id,guild_id,discord_username)
             VALUES (?1,?2,?3)",
            params![3, SECONDARY_GUILD, "P3"],
        )
        .expect("insert second guild player");

    assert!(
        fixture
            .repository
            .set_preferred_region(3, Some(GUILD), "USE")
            .expect("set primary guild region")
    );

    assert_eq!(
        fixture
            .repository
            .player_preferences(3, Some(GUILD))
            .expect("read primary guild")
            .expect("primary guild row")
            .preferred_region
            .as_deref(),
        Some("USE")
    );
    assert_eq!(
        fixture
            .repository
            .player_preferences(3, Some(SECONDARY_GUILD))
            .expect("read secondary guild")
            .expect("secondary guild row")
            .preferred_region,
        None
    );
}

#[test]
fn test_returns_only_unchecked_players_with_steam_id() {
    let fixture = Fixture::new();
    for (discord_id, steam_id, inferred_region) in [
        (10, Some(111), None),
        (11, Some(222), Some("USE")),
        (12, None, None),
        (-5, Some(333), None),
    ] {
        fixture
            .connection()
            .execute(
                "INSERT INTO players(
                     discord_id,guild_id,discord_username,steam_id,inferred_region
                 ) VALUES(?1,?2,?3,?4,?5)",
                params![
                    discord_id,
                    GUILD,
                    format!("P{discord_id}"),
                    steam_id,
                    inferred_region
                ],
            )
            .expect("seed backfill candidate");
    }

    let candidates = fixture
        .repository
        .region_backfill_candidates()
        .expect("read backfill candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].discord_id, 10);
    assert_eq!(candidates[0].guild_id, GUILD);
    assert_eq!(candidates[0].steam_id, 111);
}

#[test]
fn test_real_repository_backfill_uses_candidate_read_and_one_bulk_write() {
    let fixture = Fixture::new();
    for (discord_id, guild_id, steam_id) in
        [(9_101, GUILD, 99), (9_101, 43, 99), (9_102, GUILD, 77)]
    {
        fixture
            .connection()
            .execute(
                "INSERT INTO players(
                     discord_id,guild_id,discord_username,steam_id,inferred_region
                 ) VALUES(?1,?2,?3,?4,NULL)",
                params![discord_id, guild_id, format!("P{discord_id}"), steam_id],
            )
            .expect("seed backfill player");
    }
    let candidates = fixture
        .repository
        .region_backfill_candidates()
        .expect("read candidates");
    assert_eq!(candidates.len(), 3);
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.steam_id)
            .collect::<Vec<_>>(),
        [99, 99, 77]
    );
    let updates = candidates
        .into_iter()
        .map(|candidate| (candidate.discord_id, candidate.guild_id, "USE".to_owned()))
        .collect::<Vec<_>>();
    assert_eq!(
        fixture
            .repository
            .set_inferred_regions_bulk(&updates)
            .expect("bulk write"),
        3
    );
    let regions = fixture
        .connection()
        .prepare(
            "SELECT inferred_region FROM players
             WHERE discord_id IN (9101,9102) ORDER BY discord_id,guild_id",
        )
        .expect("prepare region read")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("read regions")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect regions");
    assert_eq!(regions, ["USE", "USE", "USE"]);
}

const FIXTURE_SCHEMA: &str = "
CREATE TABLE players (
    discord_id INTEGER NOT NULL,
    guild_id INTEGER NOT NULL DEFAULT 0,
    discord_username TEXT NOT NULL,
    dotabuff_url TEXT,
    steam_id INTEGER,
    initial_mmr INTEGER,
    current_mmr REAL,
    preferred_roles TEXT,
    glicko_rating REAL,
    glicko_rd REAL,
    glicko_volatility REAL,
    os_mu REAL,
    os_sigma REAL,
    exclusion_count INTEGER DEFAULT 0,
    jopacoin_balance INTEGER DEFAULT 3,
    updated_at TEXT DEFAULT CURRENT_TIMESTAMP,
    preferred_region TEXT,
    inferred_region TEXT,
    PRIMARY KEY (discord_id, guild_id)
);
CREATE TABLE player_steam_ids (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    discord_id INTEGER NOT NULL,
    steam_id INTEGER NOT NULL,
    is_primary INTEGER DEFAULT 0,
    added_at INTEGER NOT NULL,
    UNIQUE (discord_id, steam_id),
    UNIQUE (steam_id)
);
";
