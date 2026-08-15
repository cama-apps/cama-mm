use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tempfile::NamedTempFile;

use super::*;

const GUILD: i64 = 123;

fn member(discord_id: i64, nickname: Option<&str>) -> VanityMember {
    VanityMember {
        discord_id,
        nickname: nickname.map(str::to_owned),
    }
}

#[derive(Debug, Default)]
struct MemoryPersistence {
    rows: Mutex<BTreeMap<i64, BTreeSet<i64>>>,
    reads: AtomicUsize,
    fail_reads_after: Option<usize>,
}

impl MemoryPersistence {
    fn failing() -> Self {
        Self {
            fail_reads_after: Some(0),
            ..Self::default()
        }
    }

    fn fail_after_first_read() -> Self {
        Self {
            fail_reads_after: Some(1),
            ..Self::default()
        }
    }
}

impl VanityTaxPersistence for MemoryPersistence {
    fn get_enforcements(&self, guild_id: i64) -> Result<BTreeSet<i64>, VanityTaxServiceError> {
        let read = self.reads.fetch_add(1, Ordering::SeqCst);
        if self.fail_reads_after.is_some_and(|limit| read >= limit) {
            return Err(VanityTaxServiceError::Persistence(
                "database unavailable".to_owned(),
            ));
        }
        Ok(self
            .rows
            .lock()
            .expect("memory persistence mutex")
            .get(&guild_id)
            .cloned()
            .unwrap_or_default())
    }

    fn set_enforcement(
        &self,
        guild_id: i64,
        discord_id: i64,
        enforced: bool,
        _actor_id: i64,
    ) -> Result<BTreeSet<i64>, VanityTaxServiceError> {
        let mut rows = self.rows.lock().expect("memory persistence mutex");
        let guild = rows.entry(guild_id).or_default();
        if enforced {
            guild.insert(discord_id);
        } else {
            guild.remove(&discord_id);
        }
        Ok(guild.clone())
    }
}

#[derive(Debug, Default)]
struct BlockingPersistence {
    rows: Mutex<BTreeMap<i64, BTreeSet<i64>>>,
    read_started: AtomicBool,
    gate: (Mutex<bool>, Condvar),
}

impl BlockingPersistence {
    fn wait_for_read(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if self.read_started.load(Ordering::Acquire) {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("refresh repository read did not start");
    }

    fn release(&self) {
        let mut open = self.gate.0.lock().expect("blocking gate mutex");
        *open = true;
        self.gate.1.notify_all();
    }
}

impl VanityTaxPersistence for BlockingPersistence {
    fn get_enforcements(&self, guild_id: i64) -> Result<BTreeSet<i64>, VanityTaxServiceError> {
        self.read_started.store(true, Ordering::Release);
        let mut open = self.gate.0.lock().expect("blocking gate mutex");
        while !*open {
            open = self.gate.1.wait(open).expect("blocking gate wait");
        }
        Ok(self
            .rows
            .lock()
            .expect("blocking rows mutex")
            .get(&guild_id)
            .cloned()
            .unwrap_or_default())
    }

    fn set_enforcement(
        &self,
        guild_id: i64,
        discord_id: i64,
        enforced: bool,
        _actor_id: i64,
    ) -> Result<BTreeSet<i64>, VanityTaxServiceError> {
        let mut rows = self.rows.lock().expect("blocking rows mutex");
        let guild = rows.entry(guild_id).or_default();
        if enforced {
            guild.insert(discord_id);
        } else {
            guild.remove(&discord_id);
        }
        Ok(guild.clone())
    }
}

fn sqlite_repository() -> (NamedTempFile, Arc<VanityTaxRepository>) {
    let file = NamedTempFile::new().expect("vanity-tax database");
    Connection::open(file.path())
        .expect("open vanity-tax database")
        .execute_batch(
            "CREATE TABLE vanity_tax_enforcements (
                 guild_id INTEGER NOT NULL,
                 discord_id INTEGER NOT NULL,
                 enforced_by INTEGER NOT NULL,
                 created_at INTEGER NOT NULL,
                 PRIMARY KEY (guild_id, discord_id)
             );
             CREATE TABLE vanity_tax_exemptions (
                 guild_id INTEGER NOT NULL,
                 discord_id INTEGER NOT NULL,
                 exempted_by INTEGER NOT NULL,
                 created_at INTEGER NOT NULL,
                 PRIMARY KEY (guild_id, discord_id)
             );",
        )
        .expect("create vanity-tax schema");
    let repository = Arc::new(VanityTaxRepository::new(file.path()));
    (file, repository)
}

#[test]
fn test_refresh_taxes_only_members_without_server_nicknames() {
    let service = VanityTaxService::in_memory();
    service
        .refresh_guild(
            GUILD,
            &[member(1, None), member(2, Some("Real Name"))],
            None,
        )
        .expect("refresh guild");
    assert_eq!(service.calculate_tax(1, Some(GUILD), 199), 19);
    assert_eq!(service.calculate_tax(2, Some(GUILD), 199), 0);
}

#[test]
fn test_global_display_name_does_not_exempt_without_server_nickname() {
    let service = VanityTaxService::in_memory();
    service
        .refresh_guild(GUILD, &[member(1, None)], None)
        .expect("refresh guild");
    assert_eq!(service.calculate_tax(1, Some(GUILD), 100), 10);
}

#[test]
fn test_member_updates_toggle_taxability_and_removal_fails_open() {
    let service = VanityTaxService::in_memory();
    service
        .refresh_guild(GUILD, &[member(1, None)], None)
        .expect("refresh guild");
    service.update_member(GUILD, 1, Some("Real Name"));
    assert_eq!(service.calculate_tax(1, Some(GUILD), 500), 0);
    service.update_member(GUILD, 1, None);
    assert_eq!(service.calculate_tax(1, Some(GUILD), 500), 50);
    service.remove_member(GUILD, 1);
    assert_eq!(service.calculate_tax(1, Some(GUILD), 500), 0);
}

#[test]
fn test_failed_enforcement_refresh_keeps_guild_eligibility_unknown() {
    let service = VanityTaxService::persistent(Arc::new(MemoryPersistence::failing()));
    let error = service
        .refresh_guild(GUILD, &[member(1, None)], None)
        .expect_err("refresh must fail");
    assert!(error.to_string().contains("database unavailable"));
    assert!(service.taxable_ids(Some(GUILD)).is_empty());
    assert_eq!(
        service.eligibility_status(Some(GUILD), 1),
        VanityEligibility::Unknown
    );
}

#[test]
fn test_manual_taxation_persists_and_overrides_server_nickname() {
    let (_file, repository) = sqlite_repository();
    let service = VanityTaxService::persistent(repository.clone());
    service
        .refresh_guild(GUILD, &[member(1, Some("Skater"))], None)
        .expect("initial refresh");
    assert_eq!(service.calculate_tax(1, Some(GUILD), 500), 0);
    service
        .set_manual_taxation(GUILD, 1, true, 99)
        .expect("enforce taxation");
    assert!(service.is_manually_taxed(Some(GUILD), 1));
    assert_eq!(service.calculate_tax(1, Some(GUILD), 500), 50);

    let reloaded = VanityTaxService::persistent(repository.clone());
    reloaded
        .refresh_guild(GUILD, &[member(1, Some("Skater"))], None)
        .expect("reload refresh");
    assert_eq!(reloaded.calculate_tax(1, Some(GUILD), 500), 50);
    reloaded
        .refresh_guild(GUILD + 1, &[member(1, Some("Skater"))], None)
        .expect("other guild refresh");
    assert_eq!(reloaded.calculate_tax(1, Some(GUILD + 1), 500), 0);
    reloaded
        .set_manual_taxation(GUILD, 1, false, 100)
        .expect("clear taxation");

    let cleared = VanityTaxService::persistent(repository);
    cleared
        .refresh_guild(GUILD, &[member(1, Some("Skater"))], None)
        .expect("cleared refresh");
    assert_eq!(cleared.calculate_tax(1, Some(GUILD), 500), 0);
}

#[test]
fn test_enforcement_migration_does_not_reinterpret_legacy_exemptions() {
    let (file, repository) = sqlite_repository();
    Connection::open(file.path())
        .expect("inspect database")
        .execute(
            "INSERT INTO vanity_tax_exemptions(guild_id,discord_id,exempted_by,created_at)
             VALUES (?1,1,99,1)",
            [GUILD],
        )
        .expect("insert historical exemption");
    let service = VanityTaxService::persistent(repository);
    service
        .refresh_guild(GUILD, &[member(1, Some("Skater"))], None)
        .expect("refresh final schema");
    assert!(!service.is_manually_taxed(Some(GUILD), 1));
    assert_eq!(service.calculate_tax(1, Some(GUILD), 500), 0);
}

#[test]
fn test_committed_manual_taxation_updates_cache_without_repository_reload() {
    let repository = Arc::new(MemoryPersistence::fail_after_first_read());
    let service = VanityTaxService::persistent(repository.clone());
    service
        .refresh_guild(GUILD, &[member(1, Some("Skater"))], None)
        .expect("initial read");
    service
        .set_manual_taxation(GUILD, 1, true, 99)
        .expect("persist enforcement without reload");
    assert_eq!(repository.reads.load(Ordering::SeqCst), 1);
    assert_eq!(
        service.eligibility_status(Some(GUILD), 1),
        VanityEligibility::ManualTaxation
    );
    assert_eq!(service.calculate_tax(1, Some(GUILD), 500), 50);
}

#[test]
fn test_eligibility_status_distinguishes_every_cached_state() {
    let service = VanityTaxService::in_memory();
    service
        .refresh_guild(
            GUILD,
            &[member(1, None), member(2, Some("Real Name"))],
            None,
        )
        .expect("refresh guild");
    assert_eq!(
        service.eligibility_status(Some(GUILD), 1),
        VanityEligibility::Taxable
    );
    assert_eq!(
        service.eligibility_status(Some(GUILD), 2),
        VanityEligibility::NicknameExemption
    );
    assert_eq!(
        service.eligibility_status(Some(GUILD), 3),
        VanityEligibility::Unknown
    );
    service
        .set_manual_taxation(GUILD, 2, true, 99)
        .expect("manual taxation");
    assert_eq!(
        service.eligibility_status(Some(GUILD), 2),
        VanityEligibility::ManualTaxation
    );
    service.update_member(GUILD, 3, None);
    assert_eq!(
        service.eligibility_status(Some(GUILD), 3),
        VanityEligibility::Taxable
    );
    service.remove_member(GUILD, 3);
    assert_eq!(
        service.eligibility_status(Some(GUILD), 3),
        VanityEligibility::Unknown
    );
}

#[test]
fn test_concurrent_manual_taxations_do_not_lose_cached_members() {
    let service = Arc::new(VanityTaxService::in_memory());
    service
        .refresh_guild(
            GUILD,
            &[member(1, Some("one")), member(2, Some("two"))],
            None,
        )
        .expect("refresh guild");
    let start = Arc::new(Barrier::new(3));
    let handles = [1, 2].map(|discord_id| {
        let service = service.clone();
        let start = start.clone();
        thread::spawn(move || {
            start.wait();
            service
                .set_manual_taxation(GUILD, discord_id, true, 99)
                .expect("manual taxation");
        })
    });
    start.wait();
    for handle in handles {
        handle.join().expect("taxation worker");
    }
    assert_eq!(service.taxable_ids(Some(GUILD)), BTreeSet::from([1, 2]));
}

#[test]
fn test_concurrent_persisted_taxations_do_not_lose_members() {
    let (_file, repository) = sqlite_repository();
    let service = Arc::new(VanityTaxService::persistent(repository.clone()));
    service
        .refresh_guild(
            GUILD,
            &[member(1, Some("one")), member(2, Some("two"))],
            None,
        )
        .expect("refresh guild");
    let start = Arc::new(Barrier::new(3));
    let handles = [1, 2].map(|discord_id| {
        let service = service.clone();
        let start = start.clone();
        thread::spawn(move || {
            start.wait();
            service
                .set_manual_taxation(GUILD, discord_id, true, 99)
                .expect("persist taxation");
        })
    });
    start.wait();
    for handle in handles {
        handle.join().expect("taxation worker");
    }
    assert_eq!(service.taxable_ids(Some(GUILD)), BTreeSet::from([1, 2]));
    let reloaded = VanityTaxService::persistent(repository);
    reloaded
        .refresh_guild(
            GUILD,
            &[member(1, Some("one")), member(2, Some("two"))],
            None,
        )
        .expect("reload persisted taxation");
    assert_eq!(reloaded.taxable_ids(Some(GUILD)), BTreeSet::from([1, 2]));
}

#[test]
fn test_enforcement_started_during_refresh_wins_cache_publication() {
    let repository = Arc::new(BlockingPersistence::default());
    let service = Arc::new(VanityTaxService::persistent(repository.clone()));
    let refresh_service = service.clone();
    let refresh = thread::spawn(move || {
        refresh_service
            .refresh_guild(GUILD, &[member(1, Some("Skater"))], None)
            .expect("refresh guild");
    });
    repository.wait_for_read();
    let write_service = service.clone();
    let write = thread::spawn(move || {
        write_service
            .set_manual_taxation(GUILD, 1, true, 99)
            .expect("manual taxation");
    });
    repository.release();
    refresh.join().expect("refresh worker");
    write.join().expect("write worker");
    assert_eq!(
        service.eligibility_status(Some(GUILD), 1),
        VanityEligibility::ManualTaxation
    );
    assert_eq!(service.calculate_tax(1, Some(GUILD), 500), 50);
}

#[test]
fn test_repository_io_runs_outside_shared_cache_lock() {
    let repository = Arc::new(BlockingPersistence::default());
    let service = Arc::new(VanityTaxService::persistent(repository.clone()));
    let refresh_service = service.clone();
    let refresh = thread::spawn(move || {
        refresh_service
            .refresh_guild(GUILD, &[member(1, Some("Skater"))], None)
            .expect("refresh guild");
    });
    repository.wait_for_read();
    let (sender, receiver) = mpsc::channel();
    let probe_service = service.clone();
    let probe = thread::spawn(move || {
        sender
            .send(probe_service.eligibility_status(Some(GUILD), 1))
            .expect("send probe result");
    });
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("cache lock must remain available"),
        VanityEligibility::Unknown
    );
    repository.release();
    probe.join().expect("probe worker");
    refresh.join().expect("refresh worker");
}

#[test]
fn test_refresh_does_not_clobber_member_events_landing_mid_refresh() {
    let repository = Arc::new(BlockingPersistence::default());
    let service = Arc::new(VanityTaxService::persistent(repository.clone()));
    let refresh_service = service.clone();
    let refresh = thread::spawn(move || {
        refresh_service
            .refresh_guild(GUILD, &[member(1, None), member(2, None)], None)
            .expect("refresh guild");
    });
    repository.wait_for_read();
    service.update_member(GUILD, 1, Some("Real Name"));
    service.remove_member(GUILD, 2);
    service.update_member(GUILD, 3, None);
    repository.release();
    refresh.join().expect("refresh worker");
    assert_eq!(
        service.eligibility_status(Some(GUILD), 1),
        VanityEligibility::NicknameExemption
    );
    assert_eq!(
        service.eligibility_status(Some(GUILD), 2),
        VanityEligibility::Unknown
    );
    assert_eq!(
        service.eligibility_status(Some(GUILD), 3),
        VanityEligibility::Taxable
    );
}

#[test]
fn test_begin_refresh_lets_the_fresh_snapshot_repair_missed_events() {
    let service = VanityTaxService::in_memory();
    service
        .refresh_guild(GUILD, &[member(1, None), member(2, None)], None)
        .expect("initial refresh");
    service.update_member(GUILD, 1, None);
    service.update_member(GUILD, 2, None);
    let generation = service.begin_refresh(GUILD);
    service
        .refresh_guild(GUILD, &[member(1, Some("Real Name"))], Some(generation))
        .expect("authoritative refresh");
    assert_eq!(
        service.eligibility_status(Some(GUILD), 1),
        VanityEligibility::NicknameExemption
    );
    assert_eq!(
        service.eligibility_status(Some(GUILD), 2),
        VanityEligibility::Unknown
    );
    assert!(service.taxable_ids(Some(GUILD)).is_empty());
}

#[test]
fn test_superseded_refresh_generation_does_not_clobber_the_newer_store() {
    let service = VanityTaxService::in_memory();
    let first = service.begin_refresh(GUILD);
    let second = service.begin_refresh(GUILD);
    service.update_member(GUILD, 1, Some("Real Name"));
    assert!(
        !service
            .refresh_guild(GUILD, &[member(1, None)], Some(first))
            .expect("discard stale refresh")
    );
    assert!(service.taxable_ids(Some(GUILD)).is_empty());
    assert!(
        service
            .refresh_guild(GUILD, &[member(1, None)], Some(second))
            .expect("store current refresh")
    );
    assert_eq!(
        service.eligibility_status(Some(GUILD), 1),
        VanityEligibility::NicknameExemption
    );
}

#[test]
fn test_tax_floors_ten_percent_and_ignores_unknown_or_nonpositive_profit() {
    let service = VanityTaxService::in_memory();
    service
        .refresh_guild(GUILD, &[member(1, None)], None)
        .expect("refresh guild");
    assert_eq!(service.calculate_tax(1, Some(GUILD), 9), 0);
    assert_eq!(service.calculate_tax(1, Some(GUILD), 99), 9);
    assert_eq!(service.calculate_tax(1, Some(GUILD), 100), 10);
    assert_eq!(service.calculate_tax(1, Some(GUILD), 0), 0);
    assert_eq!(service.calculate_tax(1, Some(GUILD), -100), 0);
    assert_eq!(service.calculate_tax(999, Some(GUILD), 1_000), 0);
    assert_eq!(service.calculate_tax(1, Some(999), 1_000), 0);
    assert_eq!(service.calculate_tax(1, None, 1_000), 0);
}

#[test]
fn test_settlement_announcements_show_vanity_tax() {
    let match_text = format_bet_distribution(
        &[BetWinnerAnnouncement {
            discord_id: 1,
            payout: 200,
            amount: 100,
        }],
        &BTreeMap::new(),
        &BTreeMap::from([(1, 1)]),
    );
    assert!(match_text.contains("won 199"));
    assert!(match_text.contains("−1"));
    assert!(match_text.contains("vanity tax"));

    let prediction_text = build_resolution_announcement_chunks(
        5,
        "yes",
        &[PredictionWinnerAnnouncement {
            discord_id: 1,
            cost_basis: 100,
            yes_contracts: 2,
            no_contracts: 0,
            payout: 199,
            profit: 99,
        }],
        0,
        1,
    )
    .join("\n");
    assert!(prediction_text.contains("1 JC vanity tax"));
}
