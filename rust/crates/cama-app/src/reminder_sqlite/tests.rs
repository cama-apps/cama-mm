use std::collections::BTreeMap;

use cama_db::pet_repository::PetRepository;
use cama_db::schema_manager::initialize_or_migrate;
use cama_domain::game_date::get_game_date;
use cama_domain::pet::WARNING_HUNGER;
use rusqlite::Connection;
use tempfile::NamedTempFile;

use super::*;

const GUILD: i64 = 700;
const USER: i64 = 701;
const NOW: i64 = 1_800_000_000;

fn migrated_database() -> NamedTempFile {
    let file = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(file.path()).expect("migrate database");
    file
}

#[test]
fn migrated_sqlite_adapter_persists_preferences_and_reads_every_recovery_anchor() {
    let file = migrated_database();
    let connection = Connection::open(file.path()).expect("open migrated database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match runtime SQLite compatibility mode");
    connection
        .execute(
            "INSERT INTO players
                (discord_id,guild_id,discord_username,last_wheel_spin,last_trivia_session)
             VALUES (?1,?2,'reminder-user',?3,?4)",
            (USER, GUILD, NOW - 30, NOW - 60),
        )
        .expect("seed player anchors");
    connection
        .execute(
            "INSERT INTO tunnels
                (discord_id,guild_id,last_dig_at,mutations,injury_state,
                 stat_stamina,temp_curses)
             VALUES (?1,?2,?3,?4,?5,10,?6)",
            (
                USER,
                GUILD,
                NOW,
                r#"[{"id":"restless"}]"#,
                r#"{"type":"slower_cooldown","digs_remaining":2}"#,
                r#"{"id":"hex","name":"Hex","digs_remaining":2,"effect":{"cooldown_penalty":0.25}}"#,
            ),
        )
        .expect("seed dig anchor");
    connection
        .execute(
            "INSERT INTO player_mana
                (discord_id,guild_id,current_land,assigned_date,consumed_today)
             VALUES (?1,?2,'Forest',?3,0)",
            (USER, GUILD, get_game_date()),
        )
        .expect("seed mana");
    connection
        .execute(
            "INSERT INTO pets
                (discord_id,guild_id,name,species,adopted_at,hatched_at,adopt_fee,
                 last_fed_at,hunger_at_last_fed,dig_work_at)
             VALUES (?1,?2,'Alpaca','common_cama',?3,?3,0,?3,100,?3)",
            (USER, GUILD, NOW),
        )
        .expect("seed pet");
    drop(connection);

    let store = SqliteReminderStore::new(file.path(), 20);
    assert_eq!(
        store
            .get_preferences(UserId::new(USER), GuildId::new(GUILD))
            .expect("default preferences"),
        ReminderPreferences::default()
    );
    store
        .set_preference(
            UserId::new(USER),
            GuildId::new(GUILD),
            ReminderKind::Wheel,
            true,
        )
        .expect("set wheel preference");
    store
        .set_preference(
            UserId::new(USER),
            GuildId::new(GUILD),
            ReminderKind::Dig,
            true,
        )
        .expect("set dig preference");
    let preferences = store
        .get_preferences(UserId::new(USER), GuildId::new(GUILD))
        .expect("stored preferences");
    assert!(preferences.enabled(ReminderKind::Wheel));
    assert!(preferences.enabled(ReminderKind::Dig));
    assert!(!preferences.enabled(ReminderKind::Trivia));
    assert_eq!(
        store
            .get_enabled_users_by_type_bulk(
                GuildId::new(GUILD),
                &[ReminderKind::Wheel, ReminderKind::Trivia, ReminderKind::Dig],
            )
            .expect("bulk preferences"),
        BTreeMap::from([
            (ReminderKind::Wheel, vec![UserId::new(USER)]),
            (ReminderKind::Trivia, Vec::new()),
            (ReminderKind::Dig, vec![UserId::new(USER)]),
        ])
    );

    assert_eq!(
        store
            .get_reminder_timestamps_bulk(&[UserId::new(USER)], GuildId::new(GUILD))
            .expect("timestamp snapshot")
            .get(&UserId::new(USER)),
        Some(&ReminderTimestamps {
            last_wheel_spin: Some(NOW - 30),
            last_trivia_session: Some(NOW - 60),
        })
    );

    // Injury override (6h), then 40% stamina reduction, 25% curse, and the
    // active Forest-mana 30-second reduction: 21600*0.6*1.25-30 = 16170.
    assert_eq!(
        store
            .get_free_dig_ready_at(UserId::new(USER), GuildId::new(GUILD), NOW)
            .expect("dig ready time"),
        Some(NOW + 16_170)
    );

    let warning = store
        .get_pet_warning(UserId::new(USER), GuildId::new(GUILD), NOW)
        .expect("pet warning")
        .expect("living pet warning");
    let pet = PetRepository::new(file.path())
        .get_active_pet(USER, Some(GUILD))
        .expect("read pet")
        .expect("seeded pet");
    assert_eq!(warning.pet_name, "Alpaca");
    assert_eq!(
        warning.warning_at,
        pet.hunger_crossing_time(WARNING_HUNGER, 20)
            .expect("warning crossing")
    );
}
