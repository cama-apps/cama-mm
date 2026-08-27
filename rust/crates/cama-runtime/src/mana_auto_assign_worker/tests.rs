use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use cama_app::mana_service::{AssignmentBoard, DailyAssignment, Land, ManaDetails};
use cama_db::schema_manager::{MigrationSettings, initialize_or_migrate_with_settings};
use rusqlite::{Connection, params};
use tempfile::TempDir;
use tokio::sync::watch;

use super::*;

const NOW: i64 = 2_000_086_400; // May 19, 2033, 4 PM UTC = 9 AM PT
const USER: i64 = 31_337;
const GUILD: i64 = 9_001;
const FOREIGN_GUILD: i64 = 9_002;

struct FixedGuilds(Vec<i64>);

impl FirstGamePoolGuildSource for FixedGuilds {
    fn live_guild_ids(&self) -> Result<Vec<i64>, String> {
        Ok(self.0.clone())
    }
}

struct FixedClock {
    now: i64,
    calls: Arc<AtomicUsize>,
}

impl ManaClock for FixedClock {
    fn moment(&self) -> Result<crate::mana_provider::ManaMoment, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(crate::mana_provider::ManaMoment {
            now: self.now,
            la_offset_seconds: -7 * 3_600,
        })
    }
}

struct FakeManaDiscord {
    members: BTreeMap<i64, Vec<crate::mana_provider::ManaGuildMember>>,
}

#[async_trait]
impl crate::mana_provider::ManaDiscordPort for FakeManaDiscord {
    async fn mana_channel_is_gamba(&self, _: i64, _: i64) -> Result<bool, String> {
        Ok(false)
    }

    fn mana_guild_members(
        &self,
        guild_id: i64,
    ) -> Result<Vec<crate::mana_provider::ManaGuildMember>, String> {
        Ok(self.members.get(&guild_id).cloned().unwrap_or_default())
    }

    async fn mana_guild_member(
        &self,
        guild_id: i64,
        user_id: i64,
    ) -> Result<Option<crate::mana_provider::ManaGuildMember>, String> {
        Ok(self
            .mana_guild_members(guild_id)?
            .into_iter()
            .find(|m| m.user_id == user_id))
    }
}

struct Fixture {
    _directory: TempDir,
    path: PathBuf,
}

impl Fixture {
    fn migrated() -> Self {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let path = directory.path().join("cama.db");
        initialize_or_migrate_with_settings(&path, &MigrationSettings::default())
            .expect("migrate temporary database");
        Self {
            _directory: directory,
            path,
        }
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(&self.path).expect("open temporary database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match production SQLite foreign-key mode");
        connection
    }

    fn register(&self, discord_id: i64, balance: i64) {
        self.register_in(GUILD, discord_id, balance);
    }

    fn register_in(&self, guild_id: i64, discord_id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players
                     (discord_id, guild_id, discord_username, jopacoin_balance)
                 VALUES (?1, ?2, 'worker-fixture', ?3)",
                params![discord_id, guild_id, balance],
            )
            .expect("insert player");
    }

    fn player_land(&self, discord_id: i64, guild_id: i64) -> Option<(String, String)> {
        self.connection()
            .query_row(
                "SELECT current_land, assigned_date FROM player_mana
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok()
    }
}

fn test_worker(path: &Path, calls: Arc<AtomicUsize>, interval: Duration) -> ManaAutoAssignWorker {
    ManaAutoAssignWorker::with_clock_and_interval(
        path,
        5,
        Arc::new(FixedGuilds(vec![GUILD])),
        Arc::new(FakeManaDiscord {
            members: BTreeMap::new(),
        }),
        Arc::new(FixedClock { now: NOW, calls }),
        interval,
    )
}

#[tokio::test]
async fn auto_assigns_every_registered_player_in_the_guild() {
    let fixture = Fixture::migrated();
    fixture.register(USER, 100);
    fixture.register(USER + 1, 100);
    fixture.register(USER + 2, 100);
    let worker = test_worker(
        &fixture.path,
        Arc::new(AtomicUsize::new(0)),
        Duration::from_secs(900),
    );

    let boards = worker.assign_once().await.expect("auto-assign succeeds");

    assert_eq!(boards.len(), 1);
    assert_eq!(boards[0].assignments.len(), 3);
    for user_offset in 0..3 {
        let (land, date) = fixture
            .player_land(USER + user_offset, GUILD)
            .expect("player has mana");
        assert!(!land.is_empty(), "player should have a land assigned");
        assert_eq!(date, "2033-05-18", "assigned date should be today");
    }
}

#[tokio::test]
async fn foreign_guild_players_are_left_untouched() {
    let fixture = Fixture::migrated();
    fixture.register(USER, 100);
    fixture.register_in(FOREIGN_GUILD, USER + 1, 100);
    let worker = test_worker(
        &fixture.path,
        Arc::new(AtomicUsize::new(0)),
        Duration::from_secs(900),
    );

    let boards = worker.assign_once().await.expect("guild-scoped assignment");

    assert_eq!(boards.len(), 1);
    assert_eq!(boards[0].assignments.len(), 1);
    assert!(fixture.player_land(USER, GUILD).is_some());
    assert!(fixture.player_land(USER + 1, FOREIGN_GUILD).is_none());
}

#[tokio::test]
async fn players_already_assigned_today_are_left_unchanged() {
    let fixture = Fixture::migrated();
    fixture.register(USER, 100);
    fixture.register(USER + 1, 100);
    {
        let conn = fixture.connection();
        conn.execute(
            "INSERT INTO player_mana(discord_id, guild_id, current_land, assigned_date)
             VALUES (?1, ?2, 'Forest', '2033-05-18')",
            params![USER, GUILD],
        )
        .expect("pre-assign first player");
    }

    let worker = test_worker(
        &fixture.path,
        Arc::new(AtomicUsize::new(0)),
        Duration::from_secs(900),
    );

    let boards = worker
        .assign_once()
        .await
        .expect("auto-assign with existing");

    assert_eq!(
        boards[0].assignments.len(),
        1,
        "only unassigned player should be in assignments"
    );
    let (land, _) = fixture
        .player_land(USER, GUILD)
        .expect("pre-assigned player");
    assert_eq!(land, "Forest", "pre-assigned land should not change");
}

#[tokio::test]
async fn second_wake_is_idempotent_and_assigns_nothing_new() {
    let fixture = Fixture::migrated();
    fixture.register(USER, 100);
    let worker = test_worker(
        &fixture.path,
        Arc::new(AtomicUsize::new(0)),
        Duration::from_secs(900),
    );

    let first = worker.assign_once().await.expect("first wake");
    let second = worker.assign_once().await.expect("idempotent retry wake");

    assert_eq!(first[0].assignments.len(), 1);
    assert!(
        second[0].assignments.is_empty(),
        "second wake should assign nothing new"
    );
}

fn details(land: Land) -> ManaDetails {
    ManaDetails {
        land,
        color: land.color(),
        emoji: land.emoji(),
        assigned_date: "2033-05-18".to_owned(),
        retro_refund: 0,
        guardian_remaining: if land == Land::Plains { 25 } else { 0 },
        consumed: false,
    }
}

#[tokio::test]
async fn plains_assigned_players_receive_the_white_stipend() {
    let fixture = Fixture::migrated();
    fixture.register(USER, 0);
    let board = AssignmentBoard {
        assignments: vec![DailyAssignment {
            discord_id: USER,
            details: details(Land::Plains),
        }],
        board: Vec::new(),
    };
    {
        let conn = fixture.connection();
        conn.execute(
            "INSERT INTO nonprofit_fund(guild_id, total_collected) VALUES (?1, 20)",
            params![GUILD],
        )
        .expect("seed reserve");
    }

    pay_batch_stipends_sqlite(fixture.path.clone(), &board, GUILD, 5).await;

    let balance: i64 = fixture
        .connection()
        .query_row(
            "SELECT jopacoin_balance FROM players WHERE discord_id = ?1 AND guild_id = ?2",
            params![USER, GUILD],
            |row| row.get(0),
        )
        .expect("read player balance");
    assert_eq!(balance, 5, "Plains player should receive the White stipend");
}

#[tokio::test]
async fn run_assigns_immediately_then_cancels_cleanly_during_sleep() {
    let fixture = Fixture::migrated();
    fixture.register(USER, 100);
    let clock_calls = Arc::new(AtomicUsize::new(0));
    let worker = test_worker(
        &fixture.path,
        Arc::clone(&clock_calls),
        MANA_AUTO_ASSIGN_WAKE_INTERVAL,
    );
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let task = tokio::spawn(async move { worker.run(WorkerContext::new(shutdown_receiver)).await });

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if fixture.player_land(USER, GUILD).is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("startup assignment before sleep");
    assert_eq!(clock_calls.load(Ordering::SeqCst), 1);

    shutdown_sender.send(true).expect("request worker shutdown");
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("sleep cancelled promptly")
        .expect("worker task did not panic")
        .expect("clean worker shutdown");
}

#[tokio::test]
async fn shutdown_requested_before_start_is_clean_and_skips_sqlite() {
    let fixture = Fixture::migrated();
    let clock_calls = Arc::new(AtomicUsize::new(0));
    let worker = test_worker(
        &fixture.path,
        Arc::clone(&clock_calls),
        MANA_AUTO_ASSIGN_WAKE_INTERVAL,
    );
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    shutdown_sender.send(true).expect("request shutdown");

    worker
        .run(WorkerContext::new(shutdown_receiver))
        .await
        .expect("clean pre-start shutdown");
    assert_eq!(clock_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn production_spec_uses_the_exact_worker_name_and_wake_interval() {
    let fixture = Fixture::migrated();
    let spec = mana_auto_assign_worker_spec(
        &fixture.path,
        5,
        Arc::new(FixedGuilds(vec![GUILD])),
        Arc::new(FakeManaDiscord {
            members: BTreeMap::new(),
        }),
    );
    assert_eq!(spec.name, MANA_AUTO_ASSIGN_WORKER_NAME);
    assert_eq!(MANA_AUTO_ASSIGN_WAKE_INTERVAL, Duration::from_secs(900));
}
