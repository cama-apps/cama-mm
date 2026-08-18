use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::manashop_rework_repository::ManashopRepository;
use crate::test_support::copy_migrated_database;

const GUILD: i64 = 9_001;
const PLAYER: i64 = 42;
const MANA_DATE: &str = "2026-08-11";
const DAY_START: i64 = 1_754_912_400;
const NOW: i64 = DAY_START + 3_600;

struct Fixture {
    database: NamedTempFile,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary mana protection database");
        copy_migrated_database(database.path()).expect("copy migrated temporary database");
        let fixture = Self { database };
        let connection = fixture.connection();
        connection
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username, jopacoin_balance
                 ) VALUES (?1, ?2, 'guardian-fixture', 75)",
                params![PLAYER, GUILD],
            )
            .expect("insert player");
        connection
            .execute(
                "INSERT INTO player_mana (
                     discord_id, guild_id, current_land, assigned_date,
                     consumed_today, white_shield_remaining
                 ) VALUES (?1, ?2, 'Plains', ?3, 0, 25)",
                params![PLAYER, GUILD, MANA_DATE],
            )
            .expect("insert Plains claim");
        fixture
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(self.database.path()).expect("open fixture database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match runtime SQLite policy");
        connection
    }

    fn repository(&self) -> ManaProtectionRepository {
        ManaProtectionRepository::new(self.database.path())
    }

    fn add_loss(&self, event_key: &str, applied: i64, occurred_at: i64) -> i64 {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO hostile_loss_events (
                     guild_id, victim_id, actor_id, event_key, kind,
                     destination, requested, attempted, absorbed, applied,
                     victim_balance_before, victim_balance_after, shieldable,
                     retro_covered, occurred_at, created_at
                 ) VALUES (
                     ?1, ?2, 99, ?3, 'tax_fine', 'burn', ?4, ?4, 0, ?4,
                     100, 100 - ?4, 1, 0, ?5, ?5
                 )",
                params![GUILD, PLAYER, event_key, applied, occurred_at],
            )
            .expect("insert hostile loss");
        connection.last_insert_rowid()
    }

    fn balance(&self) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![PLAYER, GUILD],
                |row| row.get(0),
            )
            .expect("read player balance")
    }

    fn capacity(&self) -> i64 {
        self.connection()
            .query_row(
                "SELECT white_shield_remaining FROM player_mana
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![PLAYER, GUILD],
                |row| row.get(0),
            )
            .expect("read Guardian capacity")
    }
}

#[test]
fn fresh_plains_claim_reconciles_prior_losses_once_and_uses_python_rounding() {
    let fixture = Fixture::new();
    let first = fixture.add_loss("loss:first", 9, DAY_START + 10);
    let second = fixture.add_loss("loss:second", 80, DAY_START + 20);

    let refunded = fixture
        .repository()
        .reconcile_guardian(PLAYER, Some(GUILD), DAY_START, MANA_DATE, NOW)
        .expect("reconcile Guardian");

    // floor(9 * .5)=4, then the remaining 21 points cap the second refund.
    assert_eq!(refunded, 25);
    assert_eq!(fixture.balance(), 100);
    assert_eq!(fixture.capacity(), 0);
    let connection = fixture.connection();
    let rows = connection
        .prepare(
            "SELECT hostile_loss_event_id, amount, capacity_before, capacity_after,
                    retroactive, pool_key
             FROM mana_protection_events ORDER BY protection_event_id",
        )
        .expect("prepare protection query")
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .expect("query protection rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect protection rows");
    assert_eq!(
        rows,
        vec![
            (first, 4, 25, 21, 1, format!("guardian:{MANA_DATE}")),
            (second, 21, 21, 0, 1, format!("guardian:{MANA_DATE}")),
        ]
    );
    let ledger: (String, String, i64) = connection
        .query_row(
            "SELECT source, reason, SUM(delta) FROM economy_ledger_entries
             WHERE account_type = 'player' AND account_id = ?1
               AND source = 'mana_protection'",
            [PLAYER],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read contextual ledger");
    assert_eq!(
        ledger,
        (
            "mana_protection".to_owned(),
            "retroactive guardian reimbursement".to_owned(),
            25,
        )
    );

    assert_eq!(
        fixture
            .repository()
            .reconcile_guardian(PLAYER, Some(GUILD), DAY_START, MANA_DATE, NOW)
            .expect("idempotent reconciliation"),
        0
    );
    assert_eq!(fixture.balance(), 100);
}

#[test]
fn reconciliation_ignores_old_self_inflicted_and_already_tagged_losses() {
    let fixture = Fixture::new();
    fixture.add_loss("old", 20, DAY_START - 1);
    let connection = fixture.connection();
    connection
        .execute(
            "INSERT INTO hostile_loss_events (
                 guild_id, victim_id, actor_id, event_key, kind, destination,
                 requested, attempted, absorbed, applied, victim_balance_before,
                 victim_balance_after, shieldable, retro_covered, occurred_at, created_at
             ) VALUES (?1, ?2, ?2, 'self', 'duel', 'burn', 20, 20, 0, 20,
                       100, 80, 0, 0, ?3, ?3)",
            params![GUILD, PLAYER, DAY_START + 1],
        )
        .expect("insert self-inflicted loss");
    let tagged = fixture.add_loss("tagged", 20, DAY_START + 2);
    connection
        .execute(
            "INSERT INTO mana_protection_events (
                 hostile_loss_event_id, guild_id, victim_id, protection_type,
                 pool_key, amount, rate, retroactive, created_at
             ) VALUES (?1, ?2, ?3, 'guardian', ?4, 1, 0.5, 1, ?5)",
            params![tagged, GUILD, PLAYER, format!("guardian:{MANA_DATE}"), NOW],
        )
        .expect("insert prior pool tag");

    assert_eq!(
        fixture
            .repository()
            .reconcile_guardian(PLAYER, Some(GUILD), DAY_START, MANA_DATE, NOW)
            .expect("reconcile ineligible events"),
        0
    );
    assert_eq!(fixture.balance(), 75);
    assert_eq!(fixture.capacity(), 25);
}

#[test]
fn test_protection_guardian_retro_reimburses_once() {
    let fixture = Fixture::new();
    fixture
        .connection()
        .execute(
            "UPDATE players SET jopacoin_balance=80
             WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
        )
        .unwrap();
    fixture
        .connection()
        .execute(
            "UPDATE player_mana SET current_land='Forest',assigned_date='old',
             white_shield_remaining=0 WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
        )
        .unwrap();
    fixture.add_loss("wildfire:before-mana", 20, NOW - 10);
    assert!(
        ManashopRepository::new(fixture.database.path())
            .claim_mana_atomic(PLAYER, Some(GUILD), "Plains", MANA_DATE)
            .unwrap()
    );

    assert_eq!(
        fixture
            .repository()
            .reconcile_guardian(PLAYER, Some(GUILD), DAY_START - 1, MANA_DATE, NOW)
            .unwrap(),
        10
    );
    assert_eq!(
        fixture
            .repository()
            .reconcile_guardian(PLAYER, Some(GUILD), DAY_START - 1, MANA_DATE, NOW)
            .unwrap(),
        0
    );
    assert_eq!(fixture.balance(), 90);
    assert_eq!(fixture.capacity(), 15);
    let covered: i64 = fixture
        .connection()
        .query_row(
            "SELECT retro_covered FROM hostile_loss_events
             WHERE event_key='wildfire:before-mana'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(covered, 10);
}

#[test]
fn test_protection_guardian_retro_batch_reconciles_each_player_once() {
    let fixture = Fixture::new();
    fixture
        .connection()
        .execute(
            "UPDATE players SET jopacoin_balance=80
             WHERE discord_id=?1 AND guild_id=?2",
            params![PLAYER, GUILD],
        )
        .unwrap();
    fixture
        .connection()
        .execute(
            "INSERT INTO players(
                 discord_id,guild_id,discord_username,jopacoin_balance
             ) VALUES(43,?1,'guardian-batch',80)",
            [GUILD],
        )
        .unwrap();
    fixture
        .connection()
        .execute(
            "INSERT INTO player_mana(
                 discord_id,guild_id,current_land,assigned_date,consumed_today,
                 white_shield_remaining
             ) VALUES(43,?1,'Plains',?2,0,25)",
            params![GUILD, MANA_DATE],
        )
        .unwrap();
    fixture.add_loss("wildfire:batch:42", 20, NOW - 10);
    fixture
        .connection()
        .execute(
            "INSERT INTO hostile_loss_events (
                 guild_id,victim_id,actor_id,event_key,kind,destination,
                 requested,attempted,absorbed,applied,victim_balance_before,
                 victim_balance_after,shieldable,retro_covered,occurred_at,created_at
             ) VALUES (?1,43,99,'wildfire:batch:43','wildfire','burn',20,20,0,20,
                       100,80,1,0,?2,?2)",
            params![GUILD, NOW - 10],
        )
        .unwrap();

    let refunds = fixture
        .repository()
        .reconcile_guardians(
            &[PLAYER, 43, PLAYER],
            Some(GUILD),
            DAY_START - 1,
            MANA_DATE,
            NOW,
        )
        .unwrap();
    assert_eq!(
        refunds,
        std::collections::BTreeMap::from([(PLAYER, 10), (43, 10)])
    );
    assert_eq!(
        fixture
            .repository()
            .reconcile_guardians(&[], Some(GUILD), DAY_START - 1, MANA_DATE, NOW)
            .unwrap(),
        std::collections::BTreeMap::new()
    );
    assert_eq!(fixture.balance(), 90);
    let other_balance: i64 = fixture
        .connection()
        .query_row(
            "SELECT jopacoin_balance FROM players WHERE discord_id=43 AND guild_id=?1",
            [GUILD],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(other_balance, 90);
}
