use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::thread;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const ACTOR: i64 = 501;
const GUILD: i64 = 9;
const OTHER_GUILD: i64 = 10;

struct Fixture {
    file: NamedTempFile,
    repository: DigRelicRecyclingRepository,
}

impl Fixture {
    fn new() -> Self {
        let file = NamedTempFile::new().expect("temporary relic-recycling database");
        Connection::open(file.path())
            .expect("open fixture")
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE dig_artifacts (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     artifact_id TEXT NOT NULL,
                     found_at INTEGER NOT NULL,
                     is_relic INTEGER NOT NULL DEFAULT 0,
                     equipped INTEGER NOT NULL DEFAULT 0
                 );",
            )
            .expect("create disposable artifact schema");
        let repository = DigRelicRecyclingRepository::new(file.path());
        Self { file, repository }
    }

    fn connection(&self) -> Connection {
        Connection::open(self.file.path()).expect("open fixture connection")
    }

    fn add(&self, artifact_id: &str, owner: RelicOwner, is_relic: bool, equipped: bool) -> i64 {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO dig_artifacts (
                     discord_id, guild_id, artifact_id, found_at, is_relic, equipped
                 ) VALUES (?1, ?2, ?3, 1, ?4, ?5)",
                params![
                    owner.discord_id,
                    DigRelicRecyclingRepository::normalize_guild_id(owner.guild_id),
                    artifact_id,
                    is_relic,
                    equipped
                ],
            )
            .expect("seed artifact");
        connection.last_insert_rowid()
    }
}

fn owner(guild_id: Option<i64>) -> RelicOwner {
    RelicOwner {
        discord_id: ACTOR,
        guild_id,
    }
}

fn common_rows(fixture: &Fixture, owner: RelicOwner) -> [i64; 3] {
    [
        fixture.add("crystal_compass", owner, true, false),
        fixture.add("obsidian_shield", owner, true, false),
        fixture.add("mycelium_link", owner, true, false),
    ]
}

#[test]
fn test_repository_recycle_rolls_back_when_a_selected_row_is_stale() {
    let fixture = Fixture::new();
    let rows = common_rows(&fixture, owner(Some(GUILD)));
    assert!(matches!(
        fixture.repository.atomic_recycle_relics(
            owner(Some(GUILD)),
            &[rows[0], rows[1], 999_999],
            "mole_claws",
            100,
        ),
        Err(DigRelicRecyclingRepositoryError::ExpectedThreeEligibleRelicRows)
    ));
    assert_eq!(
        fixture
            .repository
            .relic_rows(owner(Some(GUILD)))
            .expect("unchanged rows")
            .len(),
        3
    );
}

#[test]
fn atomic_recycle_deletes_three_and_inserts_one() {
    let fixture = Fixture::new();
    let rows = common_rows(&fixture, owner(Some(GUILD)));
    let output = fixture
        .repository
        .atomic_recycle_relics(owner(Some(GUILD)), &rows, "mole_claws", 123)
        .expect("recycle relics");
    assert_eq!(
        output,
        RecycledRelicRow {
            row_id: output.row_id,
            artifact_id: "mole_claws".to_owned(),
            found_at: 123,
        }
    );
    assert_eq!(
        fixture
            .repository
            .relic_rows(owner(Some(GUILD)))
            .expect("output rows"),
        [PersistedRelicRow {
            row_id: output.row_id,
            artifact_id: "mole_claws".to_owned(),
            is_relic: true,
            equipped: false,
        }]
    );
}

#[test]
fn duplicate_output_artifact_id_is_allowed() {
    let fixture = Fixture::new();
    fixture.add("mole_claws", owner(Some(GUILD)), true, false);
    let rows = common_rows(&fixture, owner(Some(GUILD)));
    fixture
        .repository
        .atomic_recycle_relics(owner(Some(GUILD)), &rows, "mole_claws", 123)
        .expect("duplicate output");
    assert_eq!(
        fixture
            .repository
            .relic_rows(owner(Some(GUILD)))
            .expect("rows")
            .iter()
            .filter(|row| row.artifact_id == "mole_claws")
            .count(),
        2
    );
}

#[test]
fn equipped_non_relic_and_cross_guild_rows_roll_back_everything() {
    let fixture = Fixture::new();
    let eligible = fixture.add("crystal_compass", owner(Some(GUILD)), true, false);
    let equipped = fixture.add("obsidian_shield", owner(Some(GUILD)), true, true);
    let non_relic = fixture.add("miner_s_lullaby", owner(Some(GUILD)), false, false);
    let cross_guild = fixture.add("mycelium_link", owner(Some(OTHER_GUILD)), true, false);
    for invalid in [equipped, non_relic, cross_guild] {
        assert!(matches!(
            fixture.repository.atomic_recycle_relics(
                owner(Some(GUILD)),
                &[eligible, invalid, 999_999],
                "mole_claws",
                123,
            ),
            Err(DigRelicRecyclingRepositoryError::ExpectedThreeEligibleRelicRows)
        ));
    }
    assert_eq!(
        fixture
            .connection()
            .query_row("SELECT COUNT(*) FROM dig_artifacts", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        4
    );
}

#[test]
fn concurrent_recycle_of_the_same_rows_has_exactly_one_winner() {
    let fixture = Fixture::new();
    let rows = common_rows(&fixture, owner(Some(GUILD)));
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let repository = fixture.repository.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                repository.atomic_recycle_relics(owner(Some(GUILD)), &rows, "mole_claws", 123)
            })
        })
        .collect::<Vec<_>>();
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().expect("recycle thread"))
        .collect::<Vec<_>>();
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(
                outcome,
                Err(DigRelicRecyclingRepositoryError::ExpectedThreeEligibleRelicRows)
            ))
            .count(),
        1
    );
    assert_eq!(
        fixture
            .repository
            .relic_rows(owner(Some(GUILD)))
            .expect("one output")
            .len(),
        1
    );
}

#[test]
fn signed_ids_and_none_guild_are_isolated() {
    let fixture = Fixture::new();
    let signed_owner = RelicOwner {
        discord_id: -501,
        guild_id: None,
    };
    let rows = common_rows(&fixture, signed_owner);
    fixture.add(
        "crystal_compass",
        RelicOwner {
            discord_id: -501,
            guild_id: Some(OTHER_GUILD),
        },
        true,
        false,
    );
    fixture
        .repository
        .atomic_recycle_relics(signed_owner, &rows, "mole_claws", 123)
        .expect("signed global recycle");
    assert_eq!(
        fixture
            .repository
            .relic_rows(signed_owner)
            .expect("global rows")
            .len(),
        1
    );
    assert_eq!(
        fixture
            .repository
            .relic_rows(RelicOwner {
                discord_id: -501,
                guild_id: Some(OTHER_GUILD),
            })
            .expect("other guild")
            .len(),
        1
    );
}

#[test]
#[ignore = "cross-language smoke; run explicitly when repository Python is available"]
fn unmapped_python_migrated_database_relic_recycling_interop_smoke() {
    let file = NamedTempFile::new().expect("interop database");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repository root");
    let python = root.join(".venv/bin/python");
    let initialize = Command::new(&python)
        .current_dir(&root)
        .args([
            "-c",
            "import sqlite3,sys\nfrom infrastructure.schema_manager import SchemaManager\nfrom repositories.dig_repository import DigRepository\np=sys.argv[1]; SchemaManager(p).initialize(); r=DigRepository(p)\nr.create_tunnel(-73001,0,'recycle-interop')\nids=[r.add_artifact(-73001,0,x,is_relic=True) for x in ('crystal_compass','obsidian_shield','mycelium_link')]\nopen(sys.argv[2],'w').write(','.join(map(str,ids)))",
        ])
        .arg(file.path())
        .arg(format!("{}.ids", file.path().display()))
        .output()
        .expect("Python schema initialization");
    assert!(
        initialize.status.success(),
        "Python init failed: {}",
        String::from_utf8_lossy(&initialize.stderr)
    );
    let ids_path = format!("{}.ids", file.path().display());
    let ids = std::fs::read_to_string(&ids_path)
        .expect("read row ids")
        .split(',')
        .map(|value| value.parse::<i64>().expect("numeric row id"))
        .collect::<Vec<_>>();
    DigRelicRecyclingRepository::new(file.path())
        .atomic_recycle_relics(
            RelicOwner {
                discord_id: -73_001,
                guild_id: None,
            },
            &ids,
            "mole_claws",
            1_786_051_200,
        )
        .expect("Rust recycle");
    let verify = Command::new(python)
        .current_dir(root)
        .args([
            "-c",
            "import sqlite3,sys\nc=sqlite3.connect(sys.argv[1]); rows=c.execute(\"SELECT artifact_id,found_at,is_relic,equipped FROM dig_artifacts WHERE discord_id=-73001 AND guild_id=0 ORDER BY id\").fetchall(); assert rows==[('mole_claws',1786051200,1,0)]; assert c.execute('PRAGMA quick_check').fetchone()[0]=='ok'; c.close()",
        ])
        .arg(file.path())
        .output()
        .expect("Python verification");
    assert!(
        verify.status.success(),
        "Python verification failed: {}",
        String::from_utf8_lossy(&verify.stderr)
    );
    let _ = std::fs::remove_file(ids_path);
}
