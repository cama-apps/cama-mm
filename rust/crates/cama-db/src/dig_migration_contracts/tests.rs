use std::collections::BTreeSet;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

fn compatible_fixture() -> NamedTempFile {
    let file = NamedTempFile::new().expect("Dig migration contract fixture");
    let connection = Connection::open(file.path()).expect("open fixture");
    connection
        .execute_batch(
            "
            CREATE TABLE schema_migrations (
                name TEXT PRIMARY KEY,
                applied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE tunnels (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                auto_buy_torch INTEGER NOT NULL DEFAULT 0,
                auto_buy_hard_hat INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (discord_id, guild_id)
            );
            CREATE TABLE dig_actions (
                id INTEGER PRIMARY KEY,
                guild_id INTEGER NOT NULL,
                actor_id INTEGER NOT NULL,
                target_id INTEGER,
                action_type TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                detail TEXT
            );
            CREATE INDEX idx_dig_actions_guild_actor_created
                ON dig_actions(guild_id, actor_id, created_at DESC);
            CREATE INDEX idx_dig_actions_guild_target_created
                ON dig_actions(guild_id, target_id, created_at DESC);
            CREATE INDEX idx_dig_actions_guild_type_created_actor
                ON dig_actions(guild_id, action_type, created_at DESC, actor_id);
            CREATE TABLE dig_gear (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                slot TEXT NOT NULL,
                tier INTEGER NOT NULL,
                durability INTEGER NOT NULL,
                equipped INTEGER NOT NULL DEFAULT 0,
                acquired_at INTEGER NOT NULL,
                source TEXT NOT NULL DEFAULT 'shop'
            );
            CREATE INDEX idx_dig_gear_player_slot
                ON dig_gear(discord_id, guild_id, slot);
            CREATE UNIQUE INDEX uq_dig_gear_one_equipped_per_slot
                ON dig_gear(discord_id, guild_id, slot) WHERE equipped=1;
            ",
        )
        .expect("create post-migration fixture");
    for migration in REQUIRED_DIG_MIGRATIONS {
        connection
            .execute(
                "INSERT INTO schema_migrations(name) VALUES (?1)",
                [migration],
            )
            .expect("seed applied migration");
    }
    drop(connection);
    file
}

fn json_text(raw: &str, path: &str) -> Option<String> {
    Connection::open_in_memory()
        .expect("JSON connection")
        .query_row("SELECT json_extract(?1, ?2)", params![raw, path], |row| {
            row.get(0)
        })
        .ok()
}

fn defeated(values: &[i64]) -> BTreeSet<i64> {
    values.iter().copied().collect()
}

fn scoped(prestige_level: i64, awards: &[i64]) -> AwardLedger {
    AwardLedger::Scoped {
        prestige_level,
        awards: awards.to_vec(),
    }
}

#[test]
#[ignore = "explicit read-only audit of a disposable Python-migrated database"]
fn unmapped_python_migrated_database_dig_migration_contract_smoke() {
    let path = std::env::var("CAMA_RUST_DIG_MIGRATION_DB")
        .expect("set CAMA_RUST_DIG_MIGRATION_DB to a disposable migrated database");
    let audit = audit_existing_schema(path).expect("audit Python-migrated Dig schema");
    assert!(audit.is_compatible(), "{audit:?}");
}

mod dig_auto_buy_settings_migration {
    use super::*;

    #[test]
    fn test_auto_buy_columns_default_and_update() {
        let mut settings = AutoBuySettings::default();
        assert!(!settings.torch);
        assert!(!settings.hard_hat);
        settings.update(true, true);
        assert!(settings.torch);
        assert!(settings.hard_hat);

        let file = compatible_fixture();
        let audit = audit_existing_schema(file.path()).expect("audit fixture");
        assert!(audit.is_compatible(), "{audit:?}");
    }
}

mod dig_action_history_indexes_migration {
    use super::*;

    #[test]
    fn test_replaces_prefix_indexes_with_chronological_indexes() {
        let file = compatible_fixture();
        let audit = audit_existing_schema(file.path()).expect("audit fixture");
        assert!(audit.legacy_indexes.is_empty());
        assert!(audit.missing_indexes.is_empty());
        assert!(audit.malformed_indexes.is_empty());

        let connection = Connection::open(file.path()).expect("open fixture");
        connection
            .execute_batch(
                "CREATE INDEX idx_dig_actions_guild_actor
                     ON dig_actions(guild_id, actor_id);
                 CREATE INDEX idx_dig_actions_guild_target
                     ON dig_actions(guild_id, target_id);",
            )
            .expect("simulate stale prefix indexes");
        drop(connection);
        let stale = audit_existing_schema(file.path()).expect("audit stale fixture");
        assert_eq!(
            stale.legacy_indexes,
            [
                "idx_dig_actions_guild_actor".to_owned(),
                "idx_dig_actions_guild_target".to_owned(),
            ]
        );
    }

    #[test]
    fn test_upgrade_restores_action_type_history_index() {
        let file = compatible_fixture();
        let connection = Connection::open(file.path()).expect("open fixture");
        connection
            .execute_batch(
                "DROP INDEX idx_dig_actions_guild_type_created_actor;
                 DELETE FROM schema_migrations
                 WHERE name='add_dig_action_type_history_index';",
            )
            .expect("simulate pre-upgrade schema");
        drop(connection);
        let before = audit_existing_schema(file.path()).expect("audit pre-upgrade fixture");
        assert!(
            before
                .missing_indexes
                .contains(&"idx_dig_actions_guild_type_created_actor".to_owned())
        );
        assert!(
            before
                .missing_migrations
                .contains(&"add_dig_action_type_history_index".to_owned())
        );

        let connection = Connection::open(file.path()).expect("open fixture");
        connection
            .execute_batch(
                "CREATE INDEX idx_dig_actions_guild_type_created_actor
                     ON dig_actions(guild_id, action_type, created_at DESC, actor_id);
                 INSERT INTO schema_migrations(name)
                     VALUES ('add_dig_action_type_history_index');",
            )
            .expect("model Python-owned upgrade postcondition");
        drop(connection);
        assert!(
            audit_existing_schema(file.path())
                .expect("audit upgraded fixture")
                .is_compatible()
        );
    }

    #[test]
    fn test_action_type_history_queries_use_composite_index() {
        let file = compatible_fixture();
        assert!(
            audit_existing_schema(file.path())
                .expect("audit query plan")
                .action_type_query_uses_index
        );
    }
}

mod clear_active_boss_ids_migration {
    use super::*;

    #[test]
    fn test_clears_active_boss_ids_only() {
        let raw = r#"{"25":{"boss_id":"grothak","status":"active"},"50":{"boss_id":"crystalia","status":"defeated"},"75":{"boss_id":"magmus_rex","status":"phase1_defeated"},"100":{"boss_id":"void_warden","status":"active"}}"#;
        let updated = clear_active_boss_ids(Some(raw))
            .expect("transform boss progress")
            .expect("non-null progress");
        assert_eq!(json_text(&updated, "$.25.boss_id").as_deref(), Some(""));
        assert_eq!(
            json_text(&updated, "$.50.boss_id").as_deref(),
            Some("crystalia")
        );
        assert_eq!(
            json_text(&updated, "$.75.boss_id").as_deref(),
            Some("magmus_rex")
        );
        assert_eq!(json_text(&updated, "$.100.boss_id").as_deref(), Some(""));
    }

    #[test]
    fn test_idempotent_when_boss_id_already_empty() {
        let raw = r#"{"25":{"boss_id":"","status":"active"},"50":{"boss_id":"crystalia","status":"defeated"}}"#;
        let first = clear_active_boss_ids(Some(raw))
            .expect("first transform")
            .expect("progress");
        let second = clear_active_boss_ids(Some(&first))
            .expect("second transform")
            .expect("progress");
        assert_eq!(first, second);
    }

    #[test]
    fn test_handles_legacy_string_entries() {
        let raw = r#"{"25":"active","50":"defeated"}"#;
        assert_eq!(
            clear_active_boss_ids(Some(raw)).expect("legacy transform"),
            Some(raw.to_owned())
        );
    }

    #[test]
    fn test_skips_tunnels_with_invalid_json() {
        let raw = "{not json";
        assert_eq!(
            clear_active_boss_ids(Some(raw)).expect("invalid transform"),
            Some(raw.to_owned())
        );
    }

    #[test]
    fn test_skips_tunnels_with_null_boss_progress() {
        assert_eq!(clear_active_boss_ids(None).expect("null transform"), None);
    }

    #[test]
    fn test_runs_during_normal_initialization() {
        let file = compatible_fixture();
        let audit = audit_existing_schema(file.path()).expect("audit fixture");
        assert!(
            !audit
                .missing_migrations
                .contains(&"clear_active_boss_ids_for_pool_reroll".to_owned())
        );
        assert!(crate::expected_migrations().contains(&"clear_active_boss_ids_for_pool_reroll"));
    }
}

mod reconcile_post_prestige_boss_stat_points {
    use super::*;

    #[test]
    fn test_grants_missing_current_prestige_boss_awards() {
        let state = StatRepairState {
            prestige_level: 1,
            stat_points: 6,
            ledger: scoped(1, &[25]),
            defeated: defeated(&[25, 50]),
        };
        let repaired = reconcile_post_prestige(state);
        assert_eq!(repaired.stat_points, 7);
        assert_eq!(repaired.ledger, scoped(1, &[25, 50]));
    }

    #[test]
    fn test_legacy_global_awards_do_not_block_prestiged_reclear() {
        let state = StatRepairState {
            prestige_level: 1,
            stat_points: 6,
            ledger: AwardLedger::Legacy(vec![25]),
            defeated: defeated(&[25]),
        };
        let repaired = reconcile_post_prestige(state);
        assert_eq!(repaired.stat_points, 7);
        assert_eq!(repaired.ledger, scoped(1, &[25]));
    }

    #[test]
    fn test_skips_point_grant_when_cumulative_total_already_correct() {
        let state = StatRepairState {
            prestige_level: 1,
            stat_points: 19,
            ledger: AwardLedger::Legacy(vec![25, 50, 75, 100, 150, 200, 275]),
            defeated: defeated(&[25, 50, 75, 100, 150, 200, 275]),
        };
        let repaired = reconcile_post_prestige(state);
        assert_eq!(repaired.stat_points, 19);
        assert_eq!(
            repaired.ledger,
            scoped(1, &[25, 50, 75, 100, 150, 200, 275])
        );
    }

    #[test]
    fn test_idempotent_after_reconciliation() {
        let state = StatRepairState {
            prestige_level: 2,
            stat_points: 5,
            ledger: scoped(2, &[]),
            defeated: defeated(&[25, 50]),
        };
        let first = reconcile_post_prestige(state);
        let second = reconcile_post_prestige(first.clone());
        assert_eq!(first, second);
        assert_eq!(second.stat_points, 7);
        assert_eq!(second.ledger, scoped(2, &[25, 50]));
    }

    #[test]
    fn test_skips_unprestiged_tunnels() {
        let state = StatRepairState {
            prestige_level: 0,
            stat_points: 5,
            ledger: AwardLedger::Legacy(vec![]),
            defeated: defeated(&[25]),
        };
        assert_eq!(reconcile_post_prestige(state.clone()), state);
    }

    #[test]
    fn test_runs_during_normal_initialization() {
        assert!(crate::expected_migrations().contains(&"reconcile_post_prestige_boss_stat_points"));
        let file = compatible_fixture();
        assert!(
            !audit_existing_schema(file.path())
                .expect("audit fixture")
                .missing_migrations
                .contains(&"reconcile_post_prestige_boss_stat_points".to_owned())
        );
    }
}

mod reconcile_cumulative_prestige_boss_stat_points {
    use super::*;

    #[test]
    fn test_completed_prestige_levels_imply_prior_tier_boss_points() {
        let state = StatRepairState {
            prestige_level: 5,
            stat_points: 16,
            ledger: scoped(5, &[25, 50, 75, 100]),
            defeated: defeated(&[25, 50, 75, 100]),
        };
        let repaired = reconcile_cumulative(state);
        assert_eq!(repaired.stat_points, 44);
        assert_eq!(repaired.ledger, scoped(5, &[25, 50, 75, 100]));
    }

    #[test]
    fn test_current_run_defeated_bosses_are_folded_into_awards() {
        let state = StatRepairState {
            prestige_level: 2,
            stat_points: 13,
            ledger: scoped(2, &[]),
            defeated: defeated(&[25]),
        };
        let repaired = reconcile_cumulative(state);
        assert_eq!(repaired.stat_points, 20);
        assert_eq!(repaired.ledger, scoped(2, &[25]));
    }

    #[test]
    fn test_does_not_lower_already_higher_totals() {
        let state = StatRepairState {
            prestige_level: 1,
            stat_points: 99,
            ledger: scoped(1, &[25]),
            defeated: defeated(&[25]),
        };
        assert_eq!(reconcile_cumulative(state.clone()), state);
    }

    #[test]
    fn test_runs_during_normal_initialization() {
        assert!(
            crate::expected_migrations()
                .contains(&"reconcile_cumulative_prestige_boss_stat_points")
        );
        let file = compatible_fixture();
        assert!(
            !audit_existing_schema(file.path())
                .expect("audit fixture")
                .missing_migrations
                .contains(&"reconcile_cumulative_prestige_boss_stat_points".to_owned())
        );
    }
}

mod dig_gear_migration {
    use super::*;

    fn seeds() -> Vec<TunnelGearSeed> {
        vec![
            TunnelGearSeed {
                discord_id: 1,
                guild_id: 0,
                pickaxe_tier: 3,
                last_dig_at: Some(1_700_000_000),
                created_at: None,
            },
            TunnelGearSeed {
                discord_id: 2,
                guild_id: 999,
                pickaxe_tier: 5,
                last_dig_at: Some(1_700_001_000),
                created_at: None,
            },
            TunnelGearSeed {
                discord_id: 3,
                guild_id: 0,
                pickaxe_tier: 0,
                last_dig_at: None,
                created_at: None,
            },
        ]
    }

    #[test]
    fn test_fresh_init_creates_table_and_indexes() {
        let file = compatible_fixture();
        let audit = audit_existing_schema(file.path()).expect("audit fixture");
        assert!(audit.missing_columns.is_empty());
        assert!(audit.missing_indexes.is_empty());
        assert!(audit.malformed_indexes.is_empty());
    }

    #[test]
    fn test_fresh_init_registers_migration() {
        assert!(crate::expected_migrations().contains(&"create_dig_gear_system"));
        let file = compatible_fixture();
        assert!(
            !audit_existing_schema(file.path())
                .expect("audit fixture")
                .missing_migrations
                .contains(&"create_dig_gear_system".to_owned())
        );
    }

    #[test]
    fn test_backfills_weapon_for_each_existing_tunnel() {
        let rows = initial_weapon_backfill(&seeds(), false, 1_800_000_000);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.iter().map(|row| row.tier).collect::<Vec<_>>(),
            [3, 5, 0]
        );
        assert!(rows.iter().all(|row| {
            row.slot == "weapon"
                && row.durability == 20
                && row.equipped
                && row.source == "migration"
        }));
        assert_eq!(rows[2].acquired_at, 1_800_000_000);
    }

    #[test]
    fn test_initialize_is_idempotent() {
        assert!(initial_weapon_backfill(&seeds(), true, 1_800_000_000).is_empty());
    }
}

mod missing_dig_weapon_backfill_migration {
    use super::*;

    #[test]
    fn test_backfills_only_tunnels_without_any_weapon_row() {
        let tunnels = vec![
            TunnelGearSeed {
                discord_id: 1,
                guild_id: 0,
                pickaxe_tier: 3,
                last_dig_at: None,
                created_at: Some(100),
            },
            TunnelGearSeed {
                discord_id: 2,
                guild_id: 0,
                pickaxe_tier: 2,
                last_dig_at: None,
                created_at: Some(200),
            },
        ];
        let existing = [(2, 0)].into_iter().collect();
        let rows = missing_weapon_backfill(&tunnels, &existing, 300);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].discord_id, 1);
        assert_eq!(rows[0].tier, 3);
        assert_eq!(rows[0].source, "missing_weapon_backfill");
        assert_eq!(rows[0].acquired_at, 100);
    }
}
