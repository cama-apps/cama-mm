use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use cama_db::manashop_rework_repository::{
    BuffData, DarkBargainStatus, GrantBuffRequest, ManashopRepository,
};

use super::*;

const USER: i64 = 111;
const ALLY: i64 = 222;
const TARGET: i64 = 333;
const GUILD: i64 = 9_001;
const OTHER_GUILD: i64 = 9_002;
const NOW: i64 = 2_000_000_000;

struct Fixture {
    path: PathBuf,
    repository: ManashopRepository,
}

impl Fixture {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!(
            "cama-manashop-app-{}-{}.db",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let schema = "PRAGMA journal_mode = WAL;
                 PRAGMA foreign_keys = OFF;
                 CREATE TABLE players (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL,
                     discord_username TEXT,
                     jopacoin_balance INTEGER DEFAULT 3,
                     lowest_balance_ever INTEGER,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE manashop_buffs (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     buff_type TEXT NOT NULL,
                     target_id INTEGER,
                     granted_at INTEGER NOT NULL,
                     expires_at INTEGER NOT NULL,
                     triggered INTEGER NOT NULL DEFAULT 0,
                     data TEXT
                 );
                 CREATE TABLE bankruptcy_state (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL,
                     last_bankruptcy_at INTEGER,
                     penalty_games_remaining INTEGER NOT NULL DEFAULT 0,
                     bankruptcy_count INTEGER NOT NULL DEFAULT 0,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE economy_ledger_context (
                     id INTEGER PRIMARY KEY CHECK(id = 1),
                     source TEXT,
                     actor_id INTEGER,
                     related_type TEXT,
                     related_id TEXT,
                     reason TEXT,
                     metadata TEXT
                 );";
        run_python(
            "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.executescript(sys.argv[2]); c.commit(); c.close()",
            &[path.to_str().expect("UTF-8 fixture path"), schema],
        );
        Self {
            repository: ManashopRepository::new(&path),
            path,
        }
    }

    fn service(&self) -> BuffService {
        BuffService::new(self.repository.clone(), NOW)
    }

    fn register(&self, discord_id: i64, guild_id: i64, balance: i64) {
        run_python(
            "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute(\"INSERT INTO players (discord_id,guild_id,discord_username,jopacoin_balance) VALUES (?,?,?,?)\",(int(sys.argv[2]),int(sys.argv[3]),'player',int(sys.argv[4]))); c.commit(); c.close()",
            &[
                self.path.to_str().expect("UTF-8 fixture path"),
                &discord_id.to_string(),
                &guild_id.to_string(),
                &balance.to_string(),
            ],
        );
    }

    fn grant_raw(
        &self,
        owner: i64,
        guild: i64,
        kind: BuffType,
        target: Option<i64>,
        expires_at: i64,
        data: Option<&BuffData>,
    ) -> i64 {
        self.repository
            .grant_buff(GrantBuffRequest {
                discord_id: owner,
                guild_id: Some(guild),
                buff_type: kind.as_str(),
                target_id: target,
                granted_at: NOW - 100,
                expires_at,
                data,
            })
            .expect("grant fixture buff")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for candidate in [
            self.path.clone(),
            self.path.with_extension("db-wal"),
            self.path.with_extension("db-shm"),
        ] {
            let _ = std::fs::remove_file(candidate);
        }
    }
}

fn run_python(script: &str, arguments: &[&str]) {
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repository root");
    let status = Command::new(repository_root.join(".venv/bin/python"))
        .current_dir(repository_root)
        .arg("-c")
        .arg(script)
        .args(arguments)
        .status()
        .expect("run Python fixture helper");
    assert!(status.success(), "Python fixture helper failed");
}

#[derive(Default)]
struct RecordingGateway {
    requests: Vec<HostileLossRequest>,
    settlement: Option<HostileLossSettlement>,
    fail: bool,
}

impl ProtectionGateway for RecordingGateway {
    type Error = &'static str;

    fn apply_hostile_loss(
        &mut self,
        request: HostileLossRequest,
    ) -> Result<HostileLossSettlement, Self::Error> {
        self.requests.push(request);
        if self.fail {
            return Err("protection failure");
        }
        Ok(self.settlement.unwrap_or(HostileLossSettlement {
            attempted_loss: 0,
            absorbed_amount: 0,
            applied_loss: 0,
        }))
    }
}

struct FailingTransfer;

impl BalanceTransferPort for FailingTransfer {
    type Error = &'static str;

    fn transfer(
        &mut self,
        _from_id: i64,
        _to_id: i64,
        _guild_id: i64,
        _amount: i64,
    ) -> Result<(), Self::Error> {
        Err("transfer failed")
    }
}

#[test]
fn test_buff_service_grant_and_active_for() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.grant_counterspell(USER, GUILD).unwrap();
    let active = service
        .active_for(USER, GUILD, BuffType::Counterspell)
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].buff_type, "counterspell");
    assert!(active[0].expires_at > NOW);
}

#[test]
fn test_pvp_immunity_via_counterspell_or_sanctuary() {
    let fixture = Fixture::new();
    let service = fixture.service();
    assert!(!service.has_pvp_immunity(USER, GUILD).unwrap());
    service.grant_counterspell(USER, GUILD).unwrap();
    assert!(service.has_pvp_immunity(USER, GUILD).unwrap());
}

#[test]
fn test_sanctuary_protects_both_caster_and_ally() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.grant_sanctuary(USER, GUILD, ALLY).unwrap();
    assert!(service.has_pvp_immunity(USER, GUILD).unwrap());
    assert!(service.has_pvp_immunity(ALLY, GUILD).unwrap());
    let active = service
        .active_for(USER, GUILD, BuffType::Sanctuary)
        .unwrap();
    let row = &active[0];
    assert_eq!(row.target_id, Some(ALLY));
    assert_eq!(row.data.capacity, Some(150));
    assert_eq!(row.data.capacity_remaining, Some(150));
    assert_eq!(row.data.rate, Some(1.0));
    assert_eq!(row.data.shared, Some(true));
    assert_eq!(row.data.rolling_retroactive, Some(false));
    assert_eq!(row.data.caster_id, Some(USER));
    assert_eq!(row.data.ally_id, Some(ALLY));
    assert_eq!(row.data.protected_user_ids, vec![USER, ALLY]);
    assert!(!service.has_sanctuary_match_bonus(USER, GUILD));
    assert!(!service.has_sanctuary_match_bonus(ALLY, GUILD));
}

#[test]
fn test_reprieve_grants_rolling_partial_protection_pool() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.grant_reprieve(USER, GUILD).unwrap();
    let data = &service.active_for(USER, GUILD, BuffType::Reprieve).unwrap()[0].data;
    assert_eq!(data.capacity, Some(25));
    assert_eq!(data.capacity_remaining, Some(25));
    assert_eq!(data.rate, Some(0.5));
    assert_eq!(data.shared, Some(false));
    assert_eq!(data.rolling_retroactive, Some(true));
}

#[test]
fn test_aegis_grants_full_75_jc_protection_pool() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.grant_aegis(USER, GUILD).unwrap();
    let data = &service.active_for(USER, GUILD, BuffType::Aegis).unwrap()[0].data;
    assert_eq!(data.capacity, Some(75));
    assert_eq!(data.capacity_remaining, Some(75));
    assert_eq!(data.rate, Some(1.0));
    assert_eq!(data.shared, Some(false));
    assert_eq!(data.rolling_retroactive, Some(false));
}

#[test]
fn test_swamp_siphon_routes_through_protection_gateway() {
    let mut gateway = RecordingGateway {
        settlement: Some(HostileLossSettlement {
            attempted_loss: 3,
            absorbed_amount: 1,
            applied_loss: 2,
        }),
        ..RecordingGateway::default()
    };
    let result = execute_swamp_siphon(USER, GUILD, TARGET, 100, 3, 7, &mut gateway).unwrap();
    assert_eq!(result.amount, 2);
    assert_eq!(result.attempted_amount, 3);
    assert_eq!(result.absorbed_amount, 1);
    let request = &gateway.requests[0];
    assert_eq!(request.target_id, TARGET);
    assert_eq!(request.guild_id, GUILD);
    assert_eq!(request.attempted_loss, 3);
    assert_eq!(request.kind, "swamp_siphon");
    assert_eq!(request.actor_id, USER);
    assert!(request.event_key.starts_with("swamp_siphon:"));
    assert_eq!(request.destination, "player");
    assert_eq!(request.recipient_id, USER);
    assert!(request.clamp_to_balance);
}

#[test]
fn test_consume_aegis_charge_returns_true_then_false() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.grant_aegis(USER, GUILD).unwrap();
    assert!(service.consume_aegis_charge(USER, GUILD).unwrap());
    assert!(!service.consume_aegis_charge(USER, GUILD).unwrap());
}

#[test]
fn test_blood_pact_skim_state_persists() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.grant_blood_pact(USER, GUILD, TARGET).unwrap();
    let pact = service.blood_pact_skimmer(TARGET, GUILD).unwrap().unwrap();
    assert_eq!(pact.discord_id, USER);
    assert_eq!(pact.data.cap, Some(150));
    assert_eq!(pact.data.skim_rate, Some(0.25));
    service
        .record_blood_pact_skim(pact.id, &pact.data, 25)
        .unwrap();
    assert_eq!(
        service
            .blood_pact_skimmer(TARGET, GUILD)
            .unwrap()
            .unwrap()
            .data
            .skimmed_total,
        Some(25)
    );
}

#[test]
fn test_blood_pact_target_snapshot_is_batched_and_guild_scoped() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.grant_blood_pact(USER, GUILD, TARGET).unwrap();
    service.grant_blood_pact(USER, OTHER_GUILD, ALLY).unwrap();
    let triggered = service.grant_blood_pact(USER, GUILD, ALLY).unwrap();
    fixture.repository.consume_buff_atomic(triggered).unwrap();
    assert_eq!(
        service
            .blood_pact_targets(&[ALLY, TARGET, 999_999, TARGET], GUILD)
            .unwrap(),
        BTreeSet::from([TARGET])
    );
}

#[test]
fn test_blood_pact_skim_calculation_respects_rate_and_cap() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.grant_blood_pact(USER, GUILD, TARGET).unwrap();
    let first = service
        .claim_blood_pact_skim(TARGET, GUILD, 400)
        .unwrap()
        .unwrap();
    let second = service
        .claim_blood_pact_skim(TARGET, GUILD, 400)
        .unwrap()
        .unwrap();
    assert_eq!(first.skimmer_id, USER);
    assert_eq!((first.amount, first.new_total), (100, 100));
    assert_eq!(second.buff_id, first.buff_id);
    assert_eq!(second.skimmer_id, USER);
    assert_eq!((second.amount, second.new_total), (50, 150));
    assert_eq!(
        service
            .blood_pact_skimmer(TARGET, GUILD)
            .unwrap()
            .unwrap()
            .data
            .skimmed_total,
        Some(150)
    );
    assert!(
        service
            .claim_blood_pact_skim(TARGET, GUILD, 400)
            .unwrap()
            .is_none()
    );
}

#[test]
fn test_blood_pact_skim_reverts_when_transfer_fails() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.register(USER, GUILD, 3);
    fixture.register(TARGET, GUILD, 500);
    service.grant_blood_pact(USER, GUILD, TARGET).unwrap();
    let mut transfer = FailingTransfer;
    assert_eq!(
        service
            .apply_blood_pact_with_transfer(TARGET, GUILD, 400, &mut transfer)
            .unwrap(),
        0
    );
    assert_eq!(
        service
            .blood_pact_skimmer(TARGET, GUILD)
            .unwrap()
            .unwrap()
            .data
            .skimmed_total,
        Some(0)
    );
    assert_eq!(
        fixture.repository.balance(TARGET, Some(GUILD)).unwrap(),
        Some(500)
    );
}

#[test]
fn test_blood_pact_apply_transfers_balances() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.register(USER, GUILD, 3);
    fixture.register(TARGET, GUILD, 500);
    service.grant_blood_pact(USER, GUILD, TARGET).unwrap();
    let mut transfer = fixture.repository.clone();
    assert_eq!(
        service
            .apply_blood_pact_with_transfer(TARGET, GUILD, 400, &mut transfer)
            .unwrap(),
        100
    );
    assert_eq!(
        fixture.repository.balance(TARGET, Some(GUILD)).unwrap(),
        Some(400)
    );
    assert_eq!(
        fixture.repository.balance(USER, Some(GUILD)).unwrap(),
        Some(103)
    );
}

#[test]
fn test_blood_pact_uses_applied_loss_and_releases_shielded_capacity() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.grant_blood_pact(USER, GUILD, TARGET).unwrap();
    let mut gateway = RecordingGateway {
        settlement: Some(HostileLossSettlement {
            attempted_loss: 100,
            absorbed_amount: 60,
            applied_loss: 40,
        }),
        ..RecordingGateway::default()
    };
    assert_eq!(
        service
            .apply_blood_pact_with_protection(TARGET, GUILD, 400, 8, &mut gateway)
            .unwrap(),
        40
    );
    assert_eq!(
        service
            .blood_pact_skimmer(TARGET, GUILD)
            .unwrap()
            .unwrap()
            .data
            .skimmed_total,
        Some(40)
    );
    let request = &gateway.requests[0];
    assert_eq!(request.attempted_loss, 100);
    assert_eq!(request.kind, "blood_pact");
    assert_eq!(request.destination, "player");
    assert_eq!(request.recipient_id, USER);
}

#[test]
fn test_record_blood_pact_skim_preserves_stored_cap_and_rate() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let data = BuffData {
        skimmed_total: Some(5),
        cap: Some(80),
        skim_rate: Some(0.25),
        ..BuffData::default()
    };
    let id = fixture.grant_raw(
        USER,
        GUILD,
        BuffType::BloodPact,
        Some(TARGET),
        NOW + 3_600,
        Some(&data),
    );
    let pact = service.blood_pact_skimmer(TARGET, GUILD).unwrap().unwrap();
    service.record_blood_pact_skim(id, &pact.data, 30).unwrap();
    let refreshed = service.blood_pact_skimmer(TARGET, GUILD).unwrap().unwrap();
    assert_eq!(refreshed.data.skimmed_total, Some(30));
    assert_eq!(refreshed.data.cap, Some(80));
    assert_eq!(refreshed.data.skim_rate, Some(0.25));
}

#[test]
fn test_dark_bargain_debt_fetches() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.grant_dark_bargain_debt(USER, GUILD, 700).unwrap();
    let debt = service.dark_bargain_debt(USER, GUILD).unwrap().unwrap();
    assert_eq!(debt.data.amount_due, Some(700));
    assert_eq!(debt.data.default_penalty, Some(1_600));
    assert_eq!(debt.data.default_penalty_games, Some(5));
}

fn expired_dark_data() -> BuffData {
    BuffData {
        amount_due: Some(700),
        default_penalty: Some(1_600),
        default_penalty_games: Some(5),
        ..BuffData::default()
    }
}

#[test]
fn test_settle_due_dark_bargain_paid_once() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.register(USER, GUILD, 1_103);
    fixture.grant_raw(
        USER,
        GUILD,
        BuffType::DarkBargain,
        None,
        NOW - 1,
        Some(&expired_dark_data()),
    );
    let first = service.settle_due_dark_bargains().unwrap();
    let second = service.settle_due_dark_bargains().unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].status, DarkBargainStatus::Paid);
    assert_eq!(first[0].amount, 700);
    assert!(second.is_empty());
    assert_eq!(
        fixture.repository.balance(USER, Some(GUILD)).unwrap(),
        Some(403)
    );
}

#[test]
fn test_settle_due_dark_bargain_default_adds_penalty() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.register(USER, GUILD, 303);
    fixture.grant_raw(
        USER,
        GUILD,
        BuffType::DarkBargain,
        None,
        NOW - 1,
        Some(&expired_dark_data()),
    );
    let settled = service.settle_due_dark_bargains().unwrap();
    assert_eq!(settled[0].status, DarkBargainStatus::Defaulted);
    assert_eq!(settled[0].amount, 1_600);
    assert_eq!(
        fixture.repository.balance(USER, Some(GUILD)).unwrap(),
        Some(-1_297)
    );
    assert_eq!(
        fixture
            .repository
            .bankruptcy_penalty_games(USER, Some(GUILD))
            .unwrap(),
        5
    );
}

#[test]
fn test_settle_due_dark_bargain_missing_player_keeps_debt_active() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let id = fixture.grant_raw(
        999_999,
        GUILD,
        BuffType::DarkBargain,
        None,
        NOW - 1,
        Some(&expired_dark_data()),
    );
    let settled = service.settle_due_dark_bargains().unwrap();
    assert_eq!(settled[0].status, DarkBargainStatus::MissingPlayer);
    assert_eq!(settled[0].amount, 0);
    assert!(
        !fixture
            .repository
            .buff_by_id(id)
            .unwrap()
            .unwrap()
            .triggered
    );
}

#[test]
fn test_overgrowth_active_for_user_only() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.grant_overgrowth(USER, GUILD).unwrap();
    assert!(service.has_overgrowth(USER, GUILD).unwrap());
    assert!(!service.has_overgrowth(ALLY, GUILD).unwrap());
}

#[test]
fn test_overgrowth_grants_ten_dig_charges() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.grant_overgrowth(USER, GUILD).unwrap();
    assert_eq!(
        service
            .active_for(USER, GUILD, BuffType::Overgrowth)
            .unwrap()[0]
            .data
            .charges_remaining,
        Some(10)
    );
    for _ in 0..10 {
        assert!(service.consume_overgrowth_charge(USER, GUILD).unwrap());
    }
    assert!(!service.consume_overgrowth_charge(USER, GUILD).unwrap());
    assert!(!service.has_overgrowth(USER, GUILD).unwrap());
}

#[test]
fn test_overgrowth_regrant_refreshes_not_extends() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let first = service.grant_overgrowth(USER, GUILD).unwrap();
    let second = service.grant_overgrowth(USER, GUILD).unwrap();
    let active = service
        .active_for(USER, GUILD, BuffType::Overgrowth)
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_ne!(first, second);
    assert_eq!(active[0].id, second);
}

#[test]
fn test_overgrowth_regrant_collapses_pre_existing_duplicates() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let leaked_a = fixture.grant_raw(
        USER,
        GUILD,
        BuffType::Overgrowth,
        None,
        NOW + 12 * HOURS,
        None,
    );
    let leaked_b = fixture.grant_raw(
        USER,
        GUILD,
        BuffType::Overgrowth,
        None,
        NOW + 12 * HOURS,
        None,
    );
    assert_eq!(
        service
            .active_for(USER, GUILD, BuffType::Overgrowth)
            .unwrap()
            .len(),
        2
    );
    let new_id = service.grant_overgrowth(USER, GUILD).unwrap();
    let active = service
        .active_for(USER, GUILD, BuffType::Overgrowth)
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, new_id);
    assert!(![leaked_a, leaked_b].contains(&new_id));
}

#[test]
fn test_cleanup_expired_prunes_old_rows() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.grant_raw(USER, GUILD, BuffType::Counterspell, None, NOW - 10, None);
    assert!(service.cleanup_expired().unwrap() >= 1);
    assert!(!service.has_pvp_immunity(USER, GUILD).unwrap());
}
