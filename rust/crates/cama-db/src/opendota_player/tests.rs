use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::thread;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const GUILD: i64 = 42_424;
const OTHER_GUILD: i64 = 42_425;
type WrappedFactRow = (
    i64,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    i64,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

struct Fixture {
    database: NamedTempFile,
    repository: OpenDotaPlayerRepository,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary SQLite database");
        Connection::open(database.path())
            .expect("fixture connection")
            .execute_batch(FIXTURE_SCHEMA)
            .expect("fixture-only schema");
        let repository = OpenDotaPlayerRepository::new(database.path());
        Self {
            database,
            repository,
        }
    }

    fn connection(&self) -> Connection {
        Connection::open(self.database.path()).expect("fixture connection")
    }

    fn player(&self, discord_id: i64, guild_id: i64, name: &str, dotabuff: Option<&str>) {
        self.connection()
            .execute(
                "INSERT INTO players (
                     discord_id,guild_id,discord_username,dotabuff_url
                 ) VALUES (?1,?2,?3,?4)",
                params![discord_id, guild_id, name, dotabuff],
            )
            .expect("seed player");
    }

    fn legacy_steam_id(&self, discord_id: i64, guild_id: i64, steam_id: i64) {
        self.connection()
            .execute(
                "UPDATE players SET steam_id=?1 WHERE discord_id=?2 AND guild_id=?3",
                params![steam_id, discord_id, guild_id],
            )
            .expect("seed legacy Steam ID");
    }

    fn enriched_match(&self, discord_ids: &[i64], enrichment_data: &str) -> i64 {
        let mut connection = self.connection();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("begin enriched-match fixture");
        transaction
            .execute(
                "INSERT INTO matches (guild_id,enrichment_data) VALUES (?1,?2)",
                params![GUILD, enrichment_data],
            )
            .expect("insert enriched match");
        let match_id = transaction.last_insert_rowid();
        for discord_id in discord_ids {
            transaction
                .execute(
                    "INSERT INTO match_participants (match_id,discord_id,guild_id)
                     VALUES (?1,?2,?3)",
                    params![match_id, discord_id, GUILD],
                )
                .expect("insert affected participant");
        }
        refresh_wrapped_enrichment_facts(&transaction, discord_ids)
            .expect("derive initial compact facts");
        transaction.commit().expect("commit enriched match");
        match_id
    }

    fn wrapped_facts(&self, match_id: i64) -> Vec<WrappedFactRow> {
        self.connection()
            .prepare(
                "SELECT discord_id,actions_per_min,courier_kills,pings,
                        rapier_count,lane_role,comeback,throw
                   FROM wrapped_enrichment_facts
                  WHERE guild_id=?1 AND match_id=?2
                  ORDER BY discord_id",
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
            .expect("read compact Wrapped facts")
    }
}

fn link(fixture: &Fixture, discord_id: i64, steam_id: i64, is_primary: bool, added_at: i64) {
    fixture
        .repository
        .add_steam_id(discord_id, steam_id, is_primary, added_at)
        .expect("link Steam ID");
}

#[test]
fn test_add_steam_id() {
    let fixture = Fixture::new();
    fixture.player(12_345, GUILD, "TestPlayer", None);
    link(&fixture, 12_345, 100_001, true, 10);
    assert_eq!(
        fixture.repository.get_steam_ids(12_345).expect("accounts"),
        vec![100_001]
    );
    assert_eq!(
        fixture.repository.get_steam_id(12_345).expect("primary"),
        Some(100_001)
    );
}

#[test]
fn test_add_multiple_steam_ids() {
    let fixture = Fixture::new();
    fixture.player(12_345, GUILD, "TestPlayer", None);
    link(&fixture, 12_345, 100_001, true, 10);
    link(&fixture, 12_345, 100_002, false, 20);
    link(&fixture, 12_345, 100_003, false, 30);
    assert_eq!(
        fixture.repository.get_steam_ids(12_345).expect("accounts"),
        vec![100_001, 100_002, 100_003]
    );
}

#[test]
fn test_steam_id_uniqueness_across_players() {
    let fixture = Fixture::new();
    fixture.player(-12_345, GUILD, "Player1", None);
    fixture.player(-67_890, GUILD, "Player2", None);
    link(&fixture, -12_345, 100_001, true, 10);
    link(&fixture, -67_890, 200_001, true, 20);

    assert!(matches!(
        fixture.repository.add_steam_id(-67_890, 100_001, true, 30),
        Err(OpenDotaPlayerRepositoryError::SteamIdAlreadyLinked {
            steam_id: 100_001,
            owner_id: -12_345
        })
    ));
    assert_eq!(
        fixture.repository.get_steam_id(-67_890).expect("rollback"),
        Some(200_001)
    );
}

#[test]
fn test_get_player_by_any_steam_id() {
    let fixture = Fixture::new();
    fixture.player(12_345, GUILD, "TestPlayer", None);
    link(&fixture, 12_345, 100_001, true, 10);
    link(&fixture, 12_345, 100_002, false, 20);

    for steam_id in [100_001, 100_002] {
        assert_eq!(
            fixture
                .repository
                .get_player_by_any_steam_id(steam_id, Some(GUILD))
                .expect("lookup")
                .expect("linked player")
                .discord_id,
            12_345
        );
    }
    assert_eq!(
        fixture
            .repository
            .get_player_by_any_steam_id(999_999, Some(GUILD))
            .expect("missing lookup"),
        None
    );
}

#[test]
fn test_get_by_steam_id_uses_junction_table() {
    let fixture = Fixture::new();
    fixture.player(12_345, GUILD, "TestPlayer", None);
    link(&fixture, 12_345, 100_001, true, 10);
    assert_eq!(
        fixture
            .repository
            .get_by_steam_id(100_001, Some(GUILD))
            .expect("lookup")
            .expect("linked player")
            .discord_id,
        12_345
    );
}

#[test]
fn test_set_primary_steam_id() {
    let fixture = Fixture::new();
    fixture.player(12_345, GUILD, "TestPlayer", None);
    link(&fixture, 12_345, 100_001, true, 10);
    link(&fixture, 12_345, 100_002, false, 20);
    assert!(
        fixture
            .repository
            .set_primary_steam_id(12_345, 100_002)
            .expect("promote")
    );
    assert_eq!(
        fixture.repository.get_steam_id(12_345).expect("primary"),
        Some(100_002)
    );
    assert_eq!(
        fixture.repository.get_steam_ids(12_345).expect("accounts"),
        vec![100_002, 100_001]
    );
}

#[test]
fn test_set_primary_steam_id_not_linked() {
    let fixture = Fixture::new();
    fixture.player(12_345, GUILD, "TestPlayer", None);
    link(&fixture, 12_345, 100_001, true, 10);
    assert!(
        !fixture
            .repository
            .set_primary_steam_id(12_345, 999_999)
            .expect("typed not-linked outcome")
    );
    assert_eq!(
        fixture.repository.get_steam_id(12_345).expect("unchanged"),
        Some(100_001)
    );
}

#[test]
fn test_remove_steam_id() {
    let fixture = Fixture::new();
    fixture.player(12_345, GUILD, "TestPlayer", None);
    link(&fixture, 12_345, 100_001, true, 10);
    link(&fixture, 12_345, 100_002, false, 20);
    assert!(
        fixture
            .repository
            .remove_steam_id(12_345, 100_002)
            .expect("remove")
    );
    assert_eq!(
        fixture.repository.get_steam_ids(12_345).expect("accounts"),
        vec![100_001]
    );
}

#[test]
fn test_remove_primary_promotes_next() {
    let fixture = Fixture::new();
    fixture.player(12_345, GUILD, "TestPlayer", None);
    link(&fixture, 12_345, 100_001, true, 10);
    link(&fixture, 12_345, 100_002, false, 20);
    assert!(
        fixture
            .repository
            .remove_steam_id(12_345, 100_001)
            .expect("remove primary")
    );
    assert_eq!(
        fixture.repository.get_steam_id(12_345).expect("promoted"),
        Some(100_002)
    );
    assert_eq!(
        fixture.repository.get_steam_ids(12_345).expect("accounts"),
        vec![100_002]
    );
}

#[test]
fn test_remove_last_steam_id_clears_legacy() {
    let fixture = Fixture::new();
    fixture.player(12_345, GUILD, "TestPlayer", None);
    link(&fixture, 12_345, 100_001, true, 10);
    assert!(
        fixture
            .repository
            .remove_steam_id(12_345, 100_001)
            .expect("remove final membership")
    );
    assert!(
        fixture
            .repository
            .get_steam_ids(12_345)
            .expect("empty")
            .is_empty()
    );
    assert_eq!(
        fixture.repository.get_steam_id(12_345).expect("legacy"),
        None
    );
}

#[test]
fn test_remove_lone_nonprimary_does_not_resurrect_legacy_id() {
    let fixture = Fixture::new();
    fixture.player(12_345, GUILD, "TestPlayer", None);
    link(&fixture, 12_345, 100_001, true, 10);
    link(&fixture, 12_345, 100_001, false, 20);
    assert!(
        fixture
            .repository
            .remove_steam_id(12_345, 100_001)
            .expect("remove stale non-primary")
    );
    assert!(
        fixture
            .repository
            .get_steam_ids(12_345)
            .expect("empty")
            .is_empty()
    );
    assert_eq!(
        fixture.repository.get_steam_id(12_345).expect("legacy"),
        None
    );
}

#[test]
fn test_remove_nonexistent_steam_id() {
    let fixture = Fixture::new();
    fixture.player(12_345, GUILD, "TestPlayer", None);
    assert!(
        !fixture
            .repository
            .remove_steam_id(12_345, 999_999)
            .expect("not found")
    );
}

#[test]
fn test_get_steam_ids_bulk() {
    let fixture = Fixture::new();
    for discord_id in [1, 2, 3] {
        fixture.player(discord_id, GUILD, &format!("Player{discord_id}"), None);
    }
    link(&fixture, 1, 100_001, true, 10);
    link(&fixture, 1, 100_002, false, 20);
    link(&fixture, 2, 200_001, true, 30);
    let result = fixture
        .repository
        .get_steam_ids_bulk(&[1, 2, 3, 1], None)
        .expect("bulk accounts");
    assert_eq!(result.get(&1), Some(&vec![100_001, 100_002]));
    assert_eq!(result.get(&2), Some(&vec![200_001]));
    assert_eq!(result.get(&3), Some(&Vec::new()));
    assert_eq!(result.len(), 3);
}

#[test]
fn test_get_steam_ids_bulk_scopes_legacy_fallback_to_guild() {
    let fixture = Fixture::new();
    fixture.player(12_345, GUILD, "Guild A", None);
    fixture.player(12_345, 99_999, "Guild B", None);
    fixture.legacy_steam_id(12_345, GUILD, 100_001);
    fixture.legacy_steam_id(12_345, 99_999, 200_001);
    assert_eq!(
        fixture
            .repository
            .get_steam_ids_bulk(&[12_345], Some(GUILD))
            .expect("guild A")
            .get(&12_345),
        Some(&vec![100_001])
    );
    assert_eq!(
        fixture
            .repository
            .get_steam_ids_bulk(&[12_345], Some(99_999))
            .expect("guild B")
            .get(&12_345),
        Some(&vec![200_001])
    );
}

#[test]
fn test_set_steam_id_updates_junction_table() {
    let fixture = Fixture::new();
    fixture.player(12_345, GUILD, "TestPlayer", None);
    fixture
        .repository
        .set_steam_id(12_345, 100_001, 10)
        .expect("legacy-compatible set");
    assert_eq!(
        fixture.repository.get_steam_ids(12_345).expect("accounts"),
        vec![100_001]
    );
    assert_eq!(
        fixture.repository.get_steam_id(12_345).expect("primary"),
        Some(100_001)
    );
}

#[test]
fn test_set_steam_id_changes_primary() {
    let fixture = Fixture::new();
    fixture.player(12_345, GUILD, "TestPlayer", None);
    link(&fixture, 12_345, 100_001, true, 10);
    fixture
        .repository
        .set_steam_id(12_345, 100_002, 20)
        .expect("replace primary");
    assert_eq!(
        fixture.repository.get_steam_id(12_345).expect("primary"),
        Some(100_002)
    );
    assert_eq!(
        fixture.repository.get_steam_ids(12_345).expect("accounts"),
        vec![100_002, 100_001]
    );
}

#[test]
fn test_legacy_fallback_get_steam_id() {
    let fixture = Fixture::new();
    fixture.player(12_345, GUILD, "TestPlayer", None);
    fixture.legacy_steam_id(12_345, GUILD, 100_001);
    assert_eq!(
        fixture
            .repository
            .get_steam_id(12_345)
            .expect("legacy fallback"),
        Some(100_001)
    );
}

#[test]
fn test_remove_legacy_only_steam_id() {
    let fixture = Fixture::new();
    fixture.player(12_345, GUILD, "TestPlayer", None);
    fixture.legacy_steam_id(12_345, GUILD, 100_001);
    assert_eq!(
        fixture
            .repository
            .get_steam_ids(12_345)
            .expect("legacy fallback"),
        vec![100_001]
    );
    assert!(
        fixture
            .repository
            .remove_steam_id(12_345, 100_001)
            .expect("legacy unlink")
    );
    assert!(
        fixture
            .repository
            .get_steam_ids(12_345)
            .expect("empty")
            .is_empty()
    );
}

#[test]
fn test_get_steam_id_owner_detects_cross_player() {
    let fixture = Fixture::new();
    fixture.player(-111, GUILD, "A", None);
    fixture.player(-222, OTHER_GUILD, "B", None);
    link(&fixture, -111, 500_001, true, 10);
    fixture.legacy_steam_id(-222, OTHER_GUILD, 500_002);
    assert_eq!(
        fixture
            .repository
            .get_steam_id_owner(500_001)
            .expect("junction owner"),
        Some(-111)
    );
    assert_eq!(
        fixture
            .repository
            .get_steam_id_owner(500_002)
            .expect("legacy owner"),
        Some(-222)
    );
    assert_eq!(
        fixture
            .repository
            .get_steam_id_owner(999_999)
            .expect("unowned"),
        None
    );
}

#[test]
fn test_migration_existing_steam_ids() {
    let fixture = Fixture::new();
    fixture.player(12_345, GUILD, "TestPlayer", None);
    fixture
        .repository
        .set_steam_id(12_345, 100_001, 10)
        .expect("legacy setter migration path");
    link(&fixture, 12_345, 100_002, false, 20);
    assert_eq!(
        fixture.repository.get_steam_ids(12_345).expect("accounts"),
        vec![100_001, 100_002]
    );
    for steam_id in [100_001, 100_002] {
        assert_eq!(
            fixture
                .repository
                .get_by_steam_id(steam_id, Some(GUILD))
                .expect("lookup")
                .expect("player")
                .discord_id,
            12_345
        );
    }
}

#[test]
fn test_add_steam_ids_bulk_preserves_primary_and_conflict_semantics() {
    let fixture = Fixture::new();
    for discord_id in [111, 222, 333] {
        fixture.player(discord_id, GUILD, &format!("P{discord_id}"), None);
    }
    link(&fixture, 222, 900_001, true, 10);
    let results = fixture
        .repository
        .add_steam_ids_bulk(
            &[
                (111, 800_001),
                (111, 800_002),
                (333, 900_001),
                (333, 800_003),
            ],
            20,
        )
        .expect("bulk links");
    assert_eq!(
        results,
        vec![
            BulkSteamIdLinkResult {
                success: true,
                is_primary: true,
                error: None,
            },
            BulkSteamIdLinkResult {
                success: true,
                is_primary: false,
                error: None,
            },
            BulkSteamIdLinkResult {
                success: false,
                is_primary: false,
                error: Some("Steam ID 900001 is already linked to another player".to_owned(),),
            },
            BulkSteamIdLinkResult {
                success: true,
                is_primary: true,
                error: None,
            },
        ]
    );
    assert_eq!(
        fixture.repository.get_steam_ids(111).expect("player 111"),
        vec![800_001, 800_002]
    );
    assert_eq!(
        fixture.repository.get_steam_ids(333).expect("player 333"),
        vec![800_003]
    );
}

#[test]
fn stronger_primary_link_rolls_back_on_legacy_update_failure() {
    let fixture = Fixture::new();
    fixture.player(444, GUILD, "Rollback", None);
    link(&fixture, 444, 700_001, true, 10);
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER reject_new_primary
             BEFORE UPDATE OF steam_id ON players
             WHEN NEW.steam_id=700002
             BEGIN SELECT RAISE(ABORT,'fixture rejection'); END;",
        )
        .expect("fixture-only fault injection");
    assert!(matches!(
        fixture.repository.add_steam_id(444, 700_002, true, 20),
        Err(OpenDotaPlayerRepositoryError::Sqlite(_))
    ));
    assert_eq!(
        fixture
            .repository
            .get_steam_ids(444)
            .expect("rolled back memberships"),
        vec![700_001]
    );
    assert_eq!(
        fixture
            .repository
            .get_steam_id(444)
            .expect("rolled back primary"),
        Some(700_001)
    );
}

#[test]
fn stronger_concurrent_global_claim_has_exactly_one_winner() {
    let fixture = Fixture::new();
    fixture.player(501, GUILD, "First claimant", None);
    fixture.player(502, GUILD, "Second claimant", None);
    let barrier = Arc::new(Barrier::new(3));
    let handles = [501, 502].map(|discord_id| {
        let repository = fixture.repository.clone();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            repository.add_steam_id(discord_id, 777_777, true, 10)
        })
    });
    barrier.wait();
    let outcomes = handles.map(|handle| handle.join().expect("claim thread"));
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(
                outcome,
                Err(OpenDotaPlayerRepositoryError::SteamIdAlreadyLinked { .. })
            ))
            .count(),
        1
    );
    let owner = fixture
        .repository
        .get_steam_id_owner(777_777)
        .expect("global owner")
        .expect("one winner");
    assert!([501, 502].contains(&owner));
    let loser = if owner == 501 { 502 } else { 501 };
    assert!(
        fixture
            .repository
            .get_steam_ids(loser)
            .expect("loser memberships")
            .is_empty()
    );
}

#[test]
fn test_set_and_get_steam_id() {
    let fixture = Fixture::new();
    fixture.player(100, GUILD, "TestUser", None);
    assert_eq!(fixture.repository.get_steam_id(100).expect("initial"), None);
    fixture
        .repository
        .set_steam_id(100, 12_345_678, 1_700_000_000)
        .expect("set Steam ID");
    assert_eq!(
        fixture.repository.get_steam_id(100).expect("primary"),
        Some(12_345_678)
    );
    let persisted: (i64, i64) = fixture
        .connection()
        .query_row(
            "SELECT p.steam_id,psi.is_primary FROM players p
             JOIN player_steam_ids psi ON p.discord_id=psi.discord_id
             WHERE p.discord_id=100 AND psi.steam_id=12345678",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("compatibility and junction state");
    assert_eq!(persisted, (12_345_678, 1));
}

#[test]
fn test_get_by_steam_id() {
    let fixture = Fixture::new();
    fixture.player(100, GUILD, "TestUser", None);
    fixture.player(100, OTHER_GUILD, "OtherGuildName", None);
    fixture
        .repository
        .set_steam_id(100, 12_345_678, 1_700_000_000)
        .expect("link account");
    assert_eq!(
        fixture
            .repository
            .get_by_steam_id(12_345_678, Some(GUILD))
            .expect("lookup"),
        Some(SteamLinkedPlayer {
            discord_id: 100,
            guild_id: GUILD,
            name: "TestUser".to_owned(),
            steam_id: Some(12_345_678),
            dotabuff_url: None,
        })
    );
    assert_eq!(
        fixture
            .repository
            .get_by_steam_id(12_345_678, Some(OTHER_GUILD))
            .expect("other guild")
            .expect("other-guild player")
            .name,
        "OtherGuildName"
    );
}

#[test]
fn test_get_by_steam_id_not_found() {
    let fixture = Fixture::new();
    fixture.player(100, GUILD, "TestUser", None);
    assert_eq!(
        fixture
            .repository
            .get_by_steam_id(99_999_999, Some(GUILD))
            .expect("not-found lookup"),
        None
    );
    fixture
        .repository
        .set_steam_id(100, 12_345_678, 1_700_000_000)
        .expect("link account");
    assert_eq!(
        fixture
            .repository
            .get_by_steam_id(12_345_678, Some(999_999))
            .expect("wrong-guild lookup"),
        None
    );
}

#[test]
fn test_get_all_with_dotabuff_no_steam_id() {
    let fixture = Fixture::new();
    fixture.player(
        100,
        GUILD,
        "HasBoth",
        Some("https://dotabuff.com/players/123"),
    );
    fixture
        .repository
        .set_steam_id(100, 12_345, 1_700_000_000)
        .expect("link account");
    fixture.player(
        101,
        GUILD,
        "NeedsSteamId",
        Some("https://dotabuff.com/players/456"),
    );
    fixture.player(102, GUILD, "NoDotabuff", None);
    assert_eq!(
        fixture
            .repository
            .get_all_with_dotabuff_no_steam_id()
            .expect("backfill candidates"),
        vec![SteamIdBackfillCandidate {
            discord_id: 101,
            guild_id: GUILD,
            dotabuff_url: "https://dotabuff.com/players/456".to_owned(),
        }]
    );
}

fn assert_late_link_rebuilds_history(method: &str) {
    let fixture = Fixture::new();
    fixture.player(100, GUILD, "late-link", None);
    let match_id = fixture.enriched_match(
        &[100],
        r#"{
            "players":[{
                "account_id":12345,
                "actions_per_min":321,
                "courier_kills":2,
                "pings":44,
                "purchase_log":[{"key":"rapier"},{"key":"rapier"}],
                "lane_role":2
            }],
            "comeback":111,
            "throw":222
        }"#,
    );
    assert_eq!(
        fixture.wrapped_facts(match_id),
        [(100, None, None, None, 0, None, Some(111), Some(222))]
    );

    match method {
        "set" => fixture
            .repository
            .set_steam_id(100, 12_345, 10)
            .expect("set primary Steam ID"),
        "add" => fixture
            .repository
            .add_steam_id(100, 12_345, true, 10)
            .expect("add Steam membership"),
        "bulk" => {
            let results = fixture
                .repository
                .add_steam_ids_bulk(&[(100, 12_345)], 10)
                .expect("bulk Steam membership");
            assert_eq!(
                results,
                [BulkSteamIdLinkResult {
                    success: true,
                    is_primary: true,
                    error: None,
                }]
            );
        }
        _ => panic!("unknown link method"),
    }

    assert_eq!(
        fixture.wrapped_facts(match_id),
        [(
            100,
            Some(321),
            Some(2),
            Some(44),
            2,
            Some(2),
            Some(111),
            Some(222),
        )]
    );
}

#[test]
fn test_steam_id_link_rebuilds_previously_unmatched_history_set() {
    assert_late_link_rebuilds_history("set");
}

#[test]
fn test_steam_id_link_rebuilds_previously_unmatched_history_add() {
    assert_late_link_rebuilds_history("add");
}

#[test]
fn test_steam_id_link_rebuilds_previously_unmatched_history_bulk() {
    assert_late_link_rebuilds_history("bulk");
}

#[test]
fn test_wrapped_refresh_matches_integral_float_account_id_and_preserves_real_match_facts() {
    let fixture = Fixture::new();
    fixture.player(100, GUILD, "float-account", None);
    link(&fixture, 100, 12_345, true, 10);
    let match_id = fixture.enriched_match(
        &[100],
        r#"{
            "players":[{"account_id":12345.0,"actions_per_min":444}],
            "comeback":50.25,
            "throw":25.75
        }"#,
    );

    let row = fixture
        .connection()
        .query_row(
            "SELECT actions_per_min,comeback,throw
               FROM wrapped_enrichment_facts
              WHERE guild_id=?1 AND match_id=?2 AND discord_id=100",
            params![GUILD, match_id],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<f64>>(1)?,
                    row.get::<_, Option<f64>>(2)?,
                ))
            },
        )
        .expect("read float-compatible Wrapped facts");
    assert_eq!(row, (Some(444), Some(50.25), Some(25.75)));
}

#[test]
fn test_wrapped_refresh_rejects_fractional_and_string_account_ids() {
    let fixture = Fixture::new();
    fixture.player(100, GUILD, "strict-account", None);
    link(&fixture, 100, 12_345, true, 10);
    for account_id in ["12345.9", r#""12345""#] {
        let payload =
            format!(r#"{{"players":[{{"account_id":{account_id},"actions_per_min":444}}]}}"#);
        let match_id = fixture.enriched_match(&[100], &payload);
        assert_eq!(fixture.wrapped_facts(match_id)[0].1, None);
    }
}

#[test]
fn test_steam_id_unlink_removes_stale_historical_attribution() {
    let fixture = Fixture::new();
    fixture.player(100, GUILD, "unlink", None);
    link(&fixture, 100, 11_111, true, 10);
    link(&fixture, 100, 12_345, false, 20);
    let match_id = fixture.enriched_match(
        &[100],
        r#"{
            "players":[{
                "account_id":12345,
                "actions_per_min":444,
                "purchase_log":[{"key":"rapier"}]
            }],
            "comeback":333,
            "throw":222
        }"#,
    );
    assert_eq!(fixture.wrapped_facts(match_id)[0].1, Some(444));

    assert!(
        fixture
            .repository
            .remove_steam_id(100, 12_345)
            .expect("unlink secondary account")
    );
    assert_eq!(
        fixture.wrapped_facts(match_id),
        [(100, None, None, None, 0, None, Some(333), Some(222))]
    );
}

#[test]
fn test_steam_id_reassignment_moves_historical_attribution() {
    let fixture = Fixture::new();
    fixture.player(100, GUILD, "player-100", None);
    fixture.player(200, GUILD, "player-200", None);
    link(&fixture, 100, 12_345, true, 10);
    let match_id = fixture.enriched_match(
        &[100, 200],
        r#"{
            "players":[{
                "account_id":12345,
                "actions_per_min":555,
                "lane_role":3
            }],
            "comeback":50,
            "throw":25
        }"#,
    );
    assert_eq!(
        fixture.wrapped_facts(match_id),
        [
            (100, Some(555), None, None, 0, Some(3), Some(50), Some(25)),
            (200, None, None, None, 0, None, Some(50), Some(25)),
        ]
    );

    assert!(
        fixture
            .repository
            .remove_steam_id(100, 12_345)
            .expect("remove former owner")
    );
    fixture
        .repository
        .add_steam_id(200, 12_345, true, 20)
        .expect("assign new owner");
    assert_eq!(
        fixture.wrapped_facts(match_id),
        [
            (100, None, None, None, 0, None, Some(50), Some(25)),
            (200, Some(555), None, None, 0, Some(3), Some(50), Some(25)),
        ]
    );
}

#[test]
fn test_bulk_link_parses_a_shared_historical_match_once() {
    let fixture = Fixture::new();
    fixture.player(100, GUILD, "bulk-100", None);
    fixture.player(200, GUILD, "bulk-200", None);
    let match_id = fixture.enriched_match(
        &[100, 200],
        r#"{"players":[
            {"account_id":11111,"actions_per_min":111},
            {"account_id":22222,"actions_per_min":222}
        ]}"#,
    );

    let results = fixture
        .repository
        .add_steam_ids_bulk(&[(100, 11_111), (200, 22_222)], 10)
        .expect("bulk link shared match");
    assert!(results.iter().all(|result| result.success));
    assert_eq!(fixture.wrapped_facts(match_id)[0].1, Some(111));
    assert_eq!(fixture.wrapped_facts(match_id)[1].1, Some(222));

    let mut connection = fixture.connection();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("begin parse-count proof");
    assert_eq!(
        refresh_wrapped_enrichment_facts(&transaction, &[100, 200]).expect("refresh shared match"),
        1
    );
    transaction.rollback().expect("rollback parse-count proof");
}

#[test]
fn stronger_primary_replacement_retains_membership_and_is_atomic_on_conflict() {
    let fixture = Fixture::new();
    fixture.player(-70_001, 0, "Signed", None);
    fixture.player(-70_002, 0, "Other", None);
    fixture
        .repository
        .set_steam_id(-70_001, 111, 10)
        .expect("first account");
    fixture
        .repository
        .set_steam_id(-70_001, 222, 20)
        .expect("replacement account");
    let memberships: Vec<(i64, i64)> = {
        let connection = fixture.connection();
        let mut statement = connection
            .prepare(
                "SELECT steam_id,is_primary FROM player_steam_ids
                 WHERE discord_id=-70001 ORDER BY steam_id",
            )
            .expect("membership query");
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("membership rows")
            .collect::<Result<_, _>>()
            .expect("memberships")
    };
    assert_eq!(memberships, vec![(111, 0), (222, 1)]);

    let conflict = fixture.repository.set_steam_id(-70_002, 222, 30);
    assert!(matches!(
        conflict,
        Err(OpenDotaPlayerRepositoryError::Sqlite(_))
    ));
    assert_eq!(
        fixture.repository.get_steam_id(-70_002).expect("rollback"),
        None
    );
}

#[test]
#[ignore = "cross-language smoke; run explicitly when repository Python is available"]
fn unmapped_python_migrated_database_opendota_player_interop_smoke() {
    let database = NamedTempFile::new().expect("temporary interop database");
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repository root");
    let python = repository_root.join(".venv/bin/python");
    let initialize = Command::new(&python)
        .current_dir(&repository_root)
        .args([
            "-c",
            "import sqlite3,sys\nfrom infrastructure.schema_manager import SchemaManager\np=sys.argv[1]\nSchemaManager(p).initialize()\nc=sqlite3.connect(p)\nc.execute(\"INSERT INTO players (discord_id,guild_id,discord_username,dotabuff_url) VALUES (?,?,?,?)\",(-70001,4242,'interop','https://dotabuff.com/players/765'))\nc.commit(); c.close()",
        ])
        .arg(database.path())
        .output()
        .expect("run Python schema authority");
    assert!(
        initialize.status.success(),
        "Python initialization failed: {}",
        String::from_utf8_lossy(&initialize.stderr)
    );

    let repository = OpenDotaPlayerRepository::new(database.path());
    assert_eq!(
        repository
            .get_all_with_dotabuff_no_steam_id()
            .expect("Python-shaped backfill row")[0]
            .discord_id,
        -70_001
    );
    repository
        .set_steam_id(-70_001, 765, 1_700_000_000)
        .expect("Rust account link");

    let verify = Command::new(python)
        .current_dir(repository_root)
        .args([
            "-c",
            "import sqlite3,sys\nc=sqlite3.connect(sys.argv[1])\np=c.execute(\"SELECT steam_id FROM players WHERE discord_id=-70001 AND guild_id=4242\").fetchone()\nj=c.execute(\"SELECT steam_id,is_primary,added_at FROM player_steam_ids WHERE discord_id=-70001\").fetchone()\nassert p==(765,),p\nassert j==(765,1,1700000000),j\nc.close()",
        ])
        .arg(database.path())
        .output()
        .expect("run Python verification");
    assert!(
        verify.status.success(),
        "Python verification failed: {}",
        String::from_utf8_lossy(&verify.stderr)
    );
}

const FIXTURE_SCHEMA: &str = r#"
    PRAGMA journal_mode=WAL;
    PRAGMA busy_timeout=5000;
    CREATE TABLE players (
        discord_id INTEGER NOT NULL,
        guild_id INTEGER NOT NULL DEFAULT 0,
        discord_username TEXT NOT NULL,
        dotabuff_url TEXT,
        steam_id INTEGER,
        updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
        PRIMARY KEY(discord_id,guild_id)
    );
    CREATE TABLE player_steam_ids (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        discord_id INTEGER NOT NULL,
        steam_id INTEGER NOT NULL,
        is_primary INTEGER DEFAULT 0,
        added_at INTEGER NOT NULL,
        UNIQUE(discord_id,steam_id),
        UNIQUE(steam_id)
    );
    CREATE TABLE matches (
        match_id INTEGER PRIMARY KEY AUTOINCREMENT,
        guild_id INTEGER NOT NULL DEFAULT 0,
        enrichment_data TEXT
    );
    CREATE TABLE match_participants (
        match_id INTEGER NOT NULL,
        discord_id INTEGER NOT NULL,
        guild_id INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY(match_id,discord_id)
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
        PRIMARY KEY(guild_id,match_id,discord_id)
    );
"#;
