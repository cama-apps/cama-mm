use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
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

fn member(user_id: i64, role_names: &[&str]) -> crate::mana_provider::ManaGuildMember {
    crate::mana_provider::ManaGuildMember {
        user_id,
        display_name: format!("member-{user_id}"),
        avatar_url: None,
        role_names: role_names.iter().map(|role| (*role).to_owned()).collect(),
    }
}

struct FakeManaDiscord {
    members: Mutex<BTreeMap<i64, Vec<crate::mana_provider::ManaGuildMember>>>,
    fail_member_lookups: BTreeSet<i64>,
}

impl FakeManaDiscord {
    fn with_members(members: BTreeMap<i64, Vec<crate::mana_provider::ManaGuildMember>>) -> Self {
        Self {
            members: Mutex::new(members),
            fail_member_lookups: BTreeSet::new(),
        }
    }

    fn set_members(&self, guild_id: i64, members: Vec<crate::mana_provider::ManaGuildMember>) {
        self.members
            .lock()
            .expect("member map lock")
            .insert(guild_id, members);
    }
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
        if self.fail_member_lookups.contains(&guild_id) {
            return Err(format!("member lookup failed for guild {guild_id}"));
        }
        Ok(self
            .members
            .lock()
            .expect("member map lock")
            .get(&guild_id)
            .cloned()
            .unwrap_or_default())
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

    fn balance(&self, discord_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, GUILD],
                |row| row.get(0),
            )
            .expect("read player balance")
    }

    fn seed_reserve(&self, total: i64) {
        self.connection()
            .execute(
                "INSERT INTO nonprofit_fund(guild_id, total_collected) VALUES (?1, ?2)",
                params![GUILD, total],
            )
            .expect("seed nonprofit reserve");
    }

    fn reserve(&self) -> i64 {
        self.connection()
            .query_row(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?1",
                params![GUILD],
                |row| row.get(0),
            )
            .expect("read nonprofit reserve")
    }
}

fn warm_members() -> BTreeMap<i64, Vec<crate::mana_provider::ManaGuildMember>> {
    BTreeMap::from([(GUILD, vec![member(USER, &[])])])
}

fn worker_for(
    path: &Path,
    guilds: Vec<i64>,
    discord: Arc<FakeManaDiscord>,
) -> ManaAutoAssignWorker {
    ManaAutoAssignWorker::with_clock_and_interval(
        path,
        5,
        Arc::new(FixedGuilds(guilds)),
        discord,
        Arc::new(FixedClock {
            now: NOW,
            calls: Arc::new(AtomicUsize::new(0)),
        }),
        Duration::from_secs(900),
    )
}

fn test_worker(path: &Path, calls: Arc<AtomicUsize>, interval: Duration) -> ManaAutoAssignWorker {
    ManaAutoAssignWorker::with_clock_and_interval(
        path,
        5,
        Arc::new(FixedGuilds(vec![GUILD])),
        Arc::new(FakeManaDiscord::with_members(warm_members())),
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

#[test]
fn white_stipend_is_paid_atomically_with_the_batch_claim_and_only_once() {
    let fixture = Fixture::migrated();
    fixture.register(USER, 0);
    fixture.register(USER + 1, 0);
    fixture.seed_reserve(20);
    let repository = ManaRepository::new(&fixture.path);
    let assignments = [(USER, "Plains".to_owned()), (USER + 1, "Forest".to_owned())];

    let claimed = repository
        .claim_mana_batch_atomic(&assignments, Some(GUILD), "2033-05-18", 5)
        .expect("batch claim pays the stipend in the same transaction");

    assert_eq!(claimed.len(), 2);
    assert_eq!(
        fixture.balance(USER),
        5,
        "bankrupt Plains player is paid with the claim"
    );
    assert_eq!(fixture.balance(USER + 1), 0, "non-Plains player is skipped");
    assert_eq!(fixture.reserve(), 15, "reserve is debited once");

    let retry = repository
        .claim_mana_batch_atomic(&assignments, Some(GUILD), "2033-05-18", 5)
        .expect("idempotent retry");
    assert!(retry.is_empty(), "the day is already claimed");
    assert_eq!(fixture.balance(USER), 5, "retry never double-pays");
    assert_eq!(fixture.reserve(), 15, "retry never re-debits the reserve");
}

#[tokio::test]
async fn cold_member_cache_defers_claims_until_members_are_visible() {
    let fixture = Fixture::migrated();
    fixture.register(USER, 100);
    let discord = Arc::new(FakeManaDiscord::with_members(BTreeMap::new()));
    let worker = worker_for(&fixture.path, vec![GUILD], Arc::clone(&discord));

    let cold = worker
        .assign_once()
        .await
        .expect("cold pass succeeds without claiming");
    assert!(
        cold.is_empty(),
        "cold member cache must not produce a board"
    );
    assert!(
        fixture.player_land(USER, GUILD).is_none(),
        "no land may be claimed while the member cache is cold"
    );

    discord.set_members(GUILD, vec![member(USER, &["Ash Fan Club"])]);
    let warm = worker.assign_once().await.expect("warm pass assigns");
    assert_eq!(warm.len(), 1);
    assert_eq!(warm[0].assignments.len(), 1);
    let (land, date) = fixture
        .player_land(USER, GUILD)
        .expect("assigned once members are visible");
    assert!(!land.is_empty());
    assert_eq!(date, "2033-05-18");
}

#[tokio::test]
async fn failing_guild_is_reported_only_after_every_guild_is_processed() {
    let fixture = Fixture::migrated();
    fixture.register_in(FOREIGN_GUILD, USER, 100);
    fixture.register(USER + 1, 100);
    let discord = Arc::new(FakeManaDiscord {
        members: Mutex::new(BTreeMap::from([(GUILD, vec![member(USER + 1, &[])])])),
        fail_member_lookups: BTreeSet::from([FOREIGN_GUILD]),
    });
    let worker = worker_for(&fixture.path, vec![FOREIGN_GUILD, GUILD], discord);

    let error = worker
        .assign_once()
        .await
        .expect_err("a genuine guild failure must surface to the supervisor");

    assert!(
        error.contains("failed for 1 of 2 guilds"),
        "unexpected error: {error}"
    );
    assert!(
        error.contains(&format!("guild {FOREIGN_GUILD}")),
        "error must name the failing guild: {error}"
    );
    assert!(
        fixture.player_land(USER + 1, GUILD).is_some(),
        "the healthy guild is still assigned before the failure is reported"
    );
    assert!(
        fixture.player_land(USER, FOREIGN_GUILD).is_none(),
        "the failing guild claims nothing"
    );
}

#[tokio::test]
async fn every_guild_failing_still_surfaces_an_error() {
    let fixture = Fixture::migrated();
    fixture.register(USER, 100);
    let discord = Arc::new(FakeManaDiscord {
        members: Mutex::new(BTreeMap::new()),
        fail_member_lookups: BTreeSet::from([GUILD]),
    });
    let worker = worker_for(&fixture.path, vec![GUILD], discord);

    let error = worker
        .assign_once()
        .await
        .expect_err("a pass where every guild fails must surface an error");
    assert!(
        error.contains("failed for 1 of 1 guilds"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn persistent_cold_cache_escalates_after_more_than_four_deferrals() {
    let fixture = Fixture::migrated();
    fixture.register(USER, 100);
    let discord = Arc::new(FakeManaDiscord::with_members(BTreeMap::new()));
    let worker = worker_for(&fixture.path, vec![GUILD], Arc::clone(&discord));

    for wake in 1_u32..=4 {
        worker.assign_once().await.expect("deferral pass succeeds");
        assert_eq!(worker.cold_cache_deferral_streak(GUILD), wake);
        assert!(
            worker.cold_cache_deferral_streak(GUILD) <= COLD_CACHE_WARN_DEFERRALS,
            "wake {wake} must still be within the quiet deferral budget"
        );
    }

    worker.assign_once().await.expect("fifth deferral pass");
    assert!(
        worker.cold_cache_deferral_streak(GUILD) > COLD_CACHE_WARN_DEFERRALS,
        "the fifth consecutive deferral (over an hour) takes the warning path"
    );
    assert!(
        fixture.player_land(USER, GUILD).is_none(),
        "escalation still never claims on an empty member list"
    );

    discord.set_members(GUILD, vec![member(USER, &[])]);
    worker.assign_once().await.expect("warm pass assigns");
    assert_eq!(
        worker.cold_cache_deferral_streak(GUILD),
        0,
        "a warm member list resets the deferral streak"
    );
    assert!(fixture.player_land(USER, GUILD).is_some());
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
        Arc::new(FakeManaDiscord::with_members(BTreeMap::new())),
    );
    assert_eq!(spec.name, MANA_AUTO_ASSIGN_WORKER_NAME);
    assert_eq!(MANA_AUTO_ASSIGN_WAKE_INTERVAL, Duration::from_secs(900));
}
