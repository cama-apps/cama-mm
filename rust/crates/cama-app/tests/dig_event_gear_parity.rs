//! Exact public gear-registry, schema, and hydration counterparts.

use cama_db::schema_manager::initialize_or_migrate;
use cama_domain::dig_gear::{
    Artifact, GearLoadout, GearPiece, GearService, GearSlot, GearStore, Tunnel, unique_gear,
};
use rusqlite::Connection;
use tempfile::NamedTempFile;

const ACTOR: i64 = 101;
const GUILD: i64 = 7;

fn unique_piece(item_id: &str, slot: GearSlot, durability: i32) -> GearPiece {
    GearPiece {
        id: 41,
        discord_id: ACTOR,
        guild_id: GUILD,
        slot,
        tier: 3,
        durability,
        equipped: true,
        acquired_at: 1,
        source: "event:test".to_owned(),
        item_id: Some(item_id.to_owned()),
    }
}

fn service_with_piece(piece: GearPiece) -> GearService {
    GearService {
        store: GearStore::from_persisted_player(
            ACTOR,
            GUILD,
            Tunnel {
                depth: 75,
                max_depth: 75,
                prestige_level: 0,
                pickaxe_tier: 3,
                relic_trim_notice: false,
                cavein_free_streak: 0,
            },
            100,
            vec![piece],
            Vec::<Artifact>::new(),
            42,
            1,
            2,
        ),
    }
}

#[test]
fn dig_event_gear_registry_contains_exact_approved_sidegrades() {
    let expected = [
        ("glassbreaker_pick", GearSlot::Weapon, 8),
        ("needle_pick", GearSlot::Weapon, 16),
        ("briarplate", GearSlot::Armor, 14),
        ("nullweave_mantle", GearSlot::Armor, 12),
        ("springheel_boots", GearSlot::Boots, 14),
        ("anchor_boots", GearSlot::Boots, 16),
        ("loaded_die", GearSlot::Amulet, 12),
        ("blood_locket", GearSlot::Amulet, 14),
        ("surveyors_loop", GearSlot::Ring, 14),
        ("ruinwager_signet", GearSlot::Ring, 14),
        ("red_thread_band", GearSlot::Ring, 14),
    ];
    for (item_id, slot, durability) in expected {
        let definition = unique_gear(item_id).expect("approved unique gear");
        assert_eq!(definition.item_id, item_id);
        assert_eq!(definition.slot, slot);
        assert_eq!(definition.reference_tier, 3);
        assert_eq!(definition.repair_value, 200);
        assert_eq!(definition.max_durability, durability);
        assert!(!definition.effect_summary.is_empty());
    }
    assert_eq!(unique_gear("glassbreaker_pick").unwrap().player_dmg, 2);
    assert_eq!(unique_gear("glassbreaker_pick").unwrap().player_hit, -0.08);
    assert_eq!(unique_gear("needle_pick").unwrap().crit_chance, 0.03);
    assert_eq!(unique_gear("loaded_die").unwrap().crit_bonus, 1);
    assert_eq!(
        unique_gear("red_thread_band").unwrap().effect_id,
        Some("reroll_first_miss")
    );
}

#[test]
fn dig_event_gear_fresh_schema_has_nullable_item_id() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("migrated schema");
    let connection = Connection::open(database.path()).expect("open database");
    let mut statement = connection
        .prepare("PRAGMA table_info(dig_gear)")
        .expect("gear columns");
    let columns = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(3)?))
        })
        .expect("query columns")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect columns");

    assert!(
        columns
            .iter()
            .any(|(name, not_null)| { name == "item_id" && *not_null == 0 })
    );
}

#[test]
fn dig_event_gear_hydrates_and_serializes_unique_piece() {
    let piece = unique_piece("briarplate", GearSlot::Armor, 14);
    assert_eq!(piece.name(), Some("Briarplate"));
    assert_eq!(piece.max_durability(), 14);
    assert_eq!(
        piece.effect_summary(),
        Some("+1 HP; reflect 1 damage on the first boss hit.")
    );
    let loadout = GearLoadout {
        armor: Some(piece),
        ..GearLoadout::default()
    };
    assert_eq!(loadout.combat_modifiers().player_hp_bonus, 1);
}

#[test]
fn dig_event_gear_hydrates_and_serializes_equipped_ring() {
    let ring = unique_piece("surveyors_loop", GearSlot::Ring, 14);
    assert_eq!(ring.name(), Some("Surveyor's Loop"));
    assert_eq!(ring.max_durability(), 14);
    assert_eq!(
        ring.effect_summary(),
        Some(
            "Safe event successes gain +1 block; risky and desperate choices lose 5% success chance."
        )
    );
}

#[test]
fn dig_event_gear_unique_weapon_uses_authored_dig_modifiers() {
    let service = service_with_piece(unique_piece("glassbreaker_pick", GearSlot::Weapon, 8));
    let modifiers = service
        .active_pickaxe_data(ACTOR, GUILD)
        .expect("active unique weapon");
    assert_eq!(modifiers.name, "Glassbreaker Pick");
    assert_eq!(modifiers.advance_bonus, 2);
    assert_eq!(modifiers.cave_in_reduction, 0.05);
    assert_eq!(modifiers.loot_bonus, 2);
}

#[test]
fn dig_event_gear_unknown_unique_item_fails_closed_during_hydration() {
    let piece = unique_piece("not_registered", GearSlot::Armor, 14);
    assert_eq!(piece.name(), None);
    assert_eq!(piece.effect_summary(), None);
    let loadout = GearLoadout {
        armor: Some(piece),
        ..GearLoadout::default()
    };
    assert_eq!(loadout.combat_modifiers(), Default::default());
}
