use std::collections::BTreeSet;

use cama_db::economy_event_repository::{
    EconomyEventRepository, EventDirection, EventDraft, EventEffects,
};
use cama_db::schema_manager::initialize_or_migrate;
use rusqlite::{Connection, params};
use serde_json::json;
use tempfile::NamedTempFile;

use super::*;

fn owned_perks(perks: &[&str]) -> Vec<String> {
    perks.iter().map(|perk| (*perk).to_owned()).collect()
}

#[test]
fn exact_catalog_cap_and_empty_ownership_match_python() {
    assert_eq!(PRESTIGE_PERKS.len(), 12);
    assert_eq!(PRESTIGE_PERK_STACK_CAP, 5);
    assert_eq!(
        eligible_prestige_perks(&[]),
        PRESTIGE_PERKS.into_iter().collect::<Vec<_>>()
    );
}

#[test]
fn under_cap_perk_remains_eligible() {
    let owned = owned_perks(&["loot_multiplier"; 4]);
    assert!(eligible_prestige_perks(&owned).contains(&"loot_multiplier"));
}

#[test]
fn at_cap_perk_is_hidden() {
    let owned = owned_perks(&["loot_multiplier"; PRESTIGE_PERK_STACK_CAP]);
    let eligible = eligible_prestige_perks(&owned);
    assert!(!eligible.contains(&"loot_multiplier"));
    assert_eq!(eligible.len(), 11);
}

#[test]
fn every_at_cap_perk_is_hidden() {
    let mut owned = owned_perks(&["loot_multiplier"; PRESTIGE_PERK_STACK_CAP]);
    owned.extend(owned_perks(&["advance_boost"; PRESTIGE_PERK_STACK_CAP]));
    let eligible = eligible_prestige_perks(&owned);
    assert!(!eligible.contains(&"loot_multiplier"));
    assert!(!eligible.contains(&"advance_boost"));
    assert_eq!(eligible.len(), 10);
}

#[test]
fn sha256_little_endian_seed_and_python_sample_are_exact() {
    assert_eq!(prestige_picker_seed(123, 5), 14_752_632_679_902_096_577);
    let pool = PRESTIGE_PERKS.into_iter().collect::<Vec<_>>();
    assert_eq!(
        prestige_perk_choices(&pool, 123, 5),
        [
            "the_endless",
            "reading_the_stone",
            "steady_hands",
            "patient_step"
        ]
    );
}

#[test]
fn same_attempt_is_stable_across_service_reconstruction() {
    let pool = PRESTIGE_PERKS.into_iter().collect::<Vec<_>>();
    assert_eq!(
        prestige_perk_choices(&pool, 123, 5),
        prestige_perk_choices(&pool, 123, 5)
    );
}

#[test]
fn different_user_picks_a_different_python_subset() {
    let pool = PRESTIGE_PERKS.into_iter().collect::<Vec<_>>();
    assert_ne!(
        prestige_perk_choices(&pool, 123, 5),
        prestige_perk_choices(&pool, 456, 5)
    );
}

#[test]
fn different_level_picks_a_different_python_subset() {
    let pool = PRESTIGE_PERKS.into_iter().collect::<Vec<_>>();
    assert_ne!(
        prestige_perk_choices(&pool, 123, 5),
        prestige_perk_choices(&pool, 123, 6)
    );
}

#[test]
fn seed_is_stable_sha256_not_process_hash_entropy() {
    assert_eq!(prestige_picker_seed(123, 5), 14_752_632_679_902_096_577);
    assert_eq!(prestige_picker_seed(123, 5), prestige_picker_seed(123, 5));
}

#[test]
fn capped_catalog_entry_changes_the_exact_python_sample_pool() {
    let owned = owned_perks(&["loot_multiplier"; PRESTIGE_PERK_STACK_CAP]);
    let pool = eligible_prestige_perks(&owned);
    assert_eq!(
        prestige_perk_choices(&pool, 123, 5),
        [
            "patient_step",
            "reading_the_stone",
            "steady_hands",
            "dark_adaptation"
        ]
    );
}

#[test]
fn different_users_and_levels_match_python_vectors() {
    let pool = PRESTIGE_PERKS.into_iter().collect::<Vec<_>>();
    let first = prestige_perk_choices(&pool, 123, 5);
    let other_user = prestige_perk_choices(&pool, 456, 5);
    let other_level = prestige_perk_choices(&pool, 123, 6);
    assert_eq!(
        other_user,
        [
            "cave_in_resistance",
            "dark_adaptation",
            "steady_hands",
            "deep_sight"
        ]
    );
    assert_eq!(
        other_level,
        [
            "advance_boost",
            "tunnel_mastery",
            "deep_sight",
            "veteran_miner"
        ]
    );
    assert_ne!(first, other_user);
    assert_ne!(first, other_level);
}

#[test]
fn prestige_mutation_shuffle_matches_python_string_seed_vectors() {
    let vectors = [
        (
            DigPrestigeKey {
                discord_id: 4_242,
                guild_id: 99,
            },
            8,
            ["second_wind", "event_magnet", "jinxed", "paranoia"],
        ),
        (
            DigPrestigeKey {
                discord_id: 4_243,
                guild_id: 99,
            },
            8,
            ["thick_skin", "dark_sight", "paranoia", "brittle_walls"],
        ),
        (
            DigPrestigeKey {
                discord_id: 4_242,
                guild_id: 99,
            },
            9,
            ["fragile", "event_magnet", "dark_sight", "jinxed"],
        ),
    ];
    for (key, level, expected) in vectors {
        let preview = mutation_preview(key, level).expect("P8+ mutation preview");
        let actual = std::iter::once(preview.forced.id.as_str())
            .chain(preview.choices.iter().map(|choice| choice.id.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }
}

#[test]
fn picker_samples_every_remaining_perk_when_pool_is_short() {
    let selected = prestige_perk_choices(&["advance_boost", "deep_sight"], 123, 5);
    assert_eq!(selected.len(), 2);
    assert_eq!(
        selected.into_iter().collect::<BTreeSet<_>>(),
        BTreeSet::from(["advance_boost".to_owned(), "deep_sight".to_owned()])
    );
}

#[test]
fn display_names_match_override_and_titlecase_fallback() {
    assert_eq!(prestige_perk_display_name("loot_multiplier"), "Loot Bonus");
    assert_eq!(prestige_perk_display_name("advance_boost"), "Advance Boost");
    assert_eq!(prestige_perk_display_name("the_endless"), "The Endless");
}

#[test]
fn picking_past_cap_rejects_without_mutating_the_tunnel() {
    let key = crate::dig_tunnels::TunnelKey::new(10_001, 12_345);
    let mut tunnel = crate::dig_tunnels::Tunnel::new(key, "Amber Descent", 3_000);
    tunnel.mark_prestige_ready();
    tunnel.prestige_perks = owned_perks(&["loot_multiplier"; PRESTIGE_PERK_STACK_CAP]);
    let expected = tunnel.clone();
    let mut repository = crate::dig_tunnels::InMemoryTunnelRepository::default();
    repository.insert(tunnel);
    let mut service = crate::dig_tunnels::TunnelService::new(
        repository,
        crate::dig_tunnels::SplitMixEntropy::new(7),
    );

    assert_eq!(
        service.prestige(key, "loot_multiplier", None, 1_000_000),
        Err(crate::dig_tunnels::TunnelError::PerkAtCap)
    );
    assert_eq!(service.repository().tunnel(key), Some(&expected));
    assert!(service.repository().actions().is_empty());
}

#[test]
fn loot_multiplier_display_name_is_loot_bonus() {
    assert_eq!(prestige_perk_display_name("loot_multiplier"), "Loot Bonus");
}

#[test]
fn unmapped_display_names_fall_back_to_titlecase() {
    assert_eq!(prestige_perk_display_name("advance_boost"), "Advance Boost");
    assert_eq!(prestige_perk_display_name("the_endless"), "The Endless");
}

#[test]
fn risk_detects_negative_failure_and_rejects_positive_only_event() {
    assert!(event_has_risk(&json!({
        "risky_option": {"success": {"jc": 5}, "failure": {"jc": -3}}
    })));
    assert!(!event_has_risk(&json!({
        "safe_option": {"success": {"jc": 2}, "failure": {"jc": 0}}
    })));
}

#[test]
fn risk_detects_negative_jc_range_and_legacy_outcome() {
    assert!(event_has_risk(&json!({
        "risky_option": {
            "success": {"jc": [3, 8]},
            "failure": {"jc": [-5, -1]}
        }
    })));
    assert!(event_has_risk(&json!({
        "outcomes": {"safe": {"jc": 1}, "risky": {"jc": -10}}
    })));
}

#[test]
fn risk_detects_every_non_jc_downside() {
    for failure in [
        json!({"advance": -1}),
        json!({"cave_in": true}),
        json!({"streak_loss": 1}),
        json!({"curse": {"id": "bad_luck"}}),
    ] {
        assert!(event_has_risk(&json!({
            "risky_option": {"success": {"jc": 5}, "failure": failure}
        })));
    }
}

#[test]
fn negative_advance_is_risky() {
    assert!(event_has_risk(&json!({
        "risky_option": {"success": {"jc": 5}, "failure": {"advance": -1}}
    })));
}

#[test]
fn cave_in_is_risky() {
    assert!(event_has_risk(&json!({
        "risky_option": {"success": {"jc": 5}, "failure": {"cave_in": true}}
    })));
}

#[test]
fn streak_loss_is_risky() {
    assert!(event_has_risk(&json!({
        "risky_option": {"success": {"jc": 5}, "failure": {"streak_loss": 1}}
    })));
}

#[test]
fn curse_is_risky() {
    assert!(event_has_risk(&json!({
        "risky_option": {
            "success": {"jc": 5},
            "failure": {"curse": {"id": "bad_luck"}}
        }
    })));
}

#[test]
fn empty_event_has_no_risk_or_reading_hint() {
    assert!(!event_has_risk(&json!({})));
    assert_eq!(reading_the_stone_hint(&json!({}), 0), None);
    assert_eq!(
        reading_the_stone_hint(&json!({"name": "no choices"}), 0),
        None
    );
}

#[test]
fn reading_the_stone_selects_the_highest_expected_value_direction() {
    let safe = json!({
        "safe_option": {
            "success_chance": 1.0,
            "success": {"jc": 10},
            "failure": {"jc": 0}
        },
        "risky_option": {
            "success_chance": 0.5,
            "success": {"jc": 5},
            "failure": {"jc": -10}
        }
    });
    assert_eq!(
        reading_the_stone_direction(&safe),
        Some(ReadingStoneDirection::Safe)
    );
    assert!(READING_STONE_SAFE_HINTS.contains(&reading_the_stone_hint(&safe, 1).unwrap()));

    let risky = json!({
        "safe_option": {
            "success_chance": 1.0,
            "success": {"jc": 1},
            "failure": {"jc": 0}
        },
        "risky_option": {
            "success_chance": 0.9,
            "success": {"jc": 20},
            "failure": {"jc": 0}
        }
    });
    assert_eq!(
        reading_the_stone_direction(&risky),
        Some(ReadingStoneDirection::Risky)
    );
    assert!(READING_STONE_RISKY_HINTS.contains(&reading_the_stone_hint(&risky, 2).unwrap()));
}

#[test]
fn reading_the_stone_picks_safe_when_safe_has_higher_ev() {
    let event = json!({
        "safe_option": {
            "success_chance": 1.0,
            "success": {"jc": 10},
            "failure": {"jc": 0}
        },
        "risky_option": {
            "success_chance": 0.5,
            "success": {"jc": 5},
            "failure": {"jc": -10}
        }
    });
    assert!(READING_STONE_SAFE_HINTS.contains(&reading_the_stone_hint(&event, 0).unwrap()));
}

#[test]
fn reading_the_stone_picks_risky_when_risky_has_higher_ev() {
    let event = json!({
        "safe_option": {
            "success_chance": 1.0,
            "success": {"jc": 1},
            "failure": {"jc": 0}
        },
        "risky_option": {
            "success_chance": 0.9,
            "success": {"jc": 20},
            "failure": {"jc": 0}
        }
    });
    assert!(READING_STONE_RISKY_HINTS.contains(&reading_the_stone_hint(&event, 0).unwrap()));
}

#[test]
fn reading_the_stone_hints_are_atmospheric_and_number_free() {
    let event = json!({
        "desperate_option": {
            "success_chance": 0.3,
            "success": {"jc": 50},
            "failure": {"jc": -20}
        }
    });
    for index in 0..READING_STONE_DESPERATE_HINTS.len() {
        let hint = reading_the_stone_hint(&event, index).unwrap();
        assert!(!hint.chars().any(|character| character.is_ascii_digit()));
    }
}

#[test]
fn orphan_perks_expose_the_exact_consumer_keys() {
    assert!(
        prestige_perk_effects("veteran_miner")
            .iter()
            .any(|(key, _)| *key == "risky_success_bonus")
    );
    assert!(
        prestige_perk_effects("tunnel_mastery")
            .iter()
            .any(|(key, _)| *key == "expedition_reward_bonus")
    );
    assert!(
        prestige_perk_effects("reading_the_stone")
            .iter()
            .any(|(key, _)| *key == "event_choice_reveal")
    );
}

#[test]
fn perk_aggregator_sums_duplicate_stacks() {
    let loot = aggregate_prestige_perk_effects(&owned_perks(&["loot_multiplier"; 3]));
    assert_eq!(loot.get("jc_bonus"), Some(&3.0));
    let veteran = aggregate_prestige_perk_effects(&owned_perks(&["veteran_miner"; 2]));
    assert_eq!(veteran.get("risky_success_bonus"), Some(&0.10));
}

#[test]
fn every_catalog_perk_has_a_nonempty_policy() {
    assert!(
        PRESTIGE_PERKS
            .iter()
            .all(|perk| !prestige_perk_effects(perk).is_empty())
    );
}

#[derive(Clone, Copy, Debug)]
struct FixedRelicEntropy(usize);

impl PrestigeRelicEntropy for FixedRelicEntropy {
    fn choose_index(&mut self, upper_bound: usize) -> usize {
        self.0 % upper_bound
    }
}

struct SqliteFixture {
    database: NamedTempFile,
}

impl SqliteFixture {
    fn new(prestige_level: i64, perks: &[&str]) -> Self {
        let database = NamedTempFile::new().expect("temporary SQLite database");
        initialize_or_migrate(database.path()).expect("canonical migrated schema");
        let fixture = Self { database };
        let connection = fixture.connection();
        connection
            .execute(
                "INSERT INTO players
                    (discord_id, guild_id, discord_username, jopacoin_balance)
                 VALUES (123, 5, 'prestiger', 500)",
                [],
            )
            .expect("seed player");
        let mut progress = serde_json::Map::new();
        for boundary in BOSS_BOUNDARIES {
            progress.insert(boundary.to_string(), Value::String("defeated".to_owned()));
        }
        progress.insert(
            PINNACLE_DEPTH.to_string(),
            json!({"status": "defeated", "boss_id": "forgotten_king"}),
        );
        connection
            .execute(
                "INSERT INTO tunnels (
                     discord_id, guild_id, depth, max_depth, total_digs,
                     total_jc_earned, last_dig_at, luminosity, pickaxe_tier,
                     prestige_level, prestige_perks, boss_progress,
                     boss_attempts, best_run_score, current_run_jc,
                     current_run_artifacts, current_run_events,
                     total_prestige_score, streak_days, stat_boss_awards,
                     pinnacle_boss_id, pinnacle_phase, pinnacle_hp_remaining,
                     pinnacle_last_engaged_at, route_state
                 ) VALUES (
                     123, 5, ?1, ?1, 88, 900, 123, 17, 4,
                     ?2, ?3, ?4,
                     3, 10, 50, 3, 7, 20, 4, '{\"awards\":[25]}',
                     'forgotten_king', 2, 9, 456, '{\"route\":\"left\"}'
                 )",
                params![
                    PINNACLE_DEPTH,
                    prestige_level,
                    serde_json::to_string(perks).unwrap(),
                    Value::Object(progress).to_string(),
                ],
            )
            .expect("seed tunnel");
        fixture
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(self.database.path()).expect("fixture connection");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match Python foreign-key behavior");
        connection
    }

    fn service(&self) -> DigPrestigeRuntimeService<FixedRelicEntropy> {
        let mut config = DigRuntimeConfig::default();
        config.economy_event.enabled = false;
        DigPrestigeRuntimeService::with_entropy(self.database.path(), config, FixedRelicEntropy(0))
    }

    fn balance(&self) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                  WHERE discord_id=123 AND guild_id=5",
                [],
                |row| row.get(0),
            )
            .expect("player balance")
    }
}

#[test]
fn migrated_preview_is_restart_stable_and_carries_exact_run_policy() {
    let fixture = SqliteFixture::new(4, &[]);
    let first = fixture.service().preview(123, 5).expect("first preview");
    let after_restart = fixture
        .service()
        .preview(123, 5)
        .expect("restarted preview");
    assert!(first.can_prestige);
    assert_eq!(first.current_level, 4);
    assert_eq!(first.target_level, 5);
    assert_eq!(first.offered_perks.len(), 4);
    assert_eq!(first.offered_perks, after_restart.offered_perks);
    assert_eq!(first.run_score, 1_183);
    assert_eq!(first.ascension_unlock.unwrap().name, "Erosion");
}

#[test]
fn migrated_preview_reports_tier_pinnacle_and_max_blockers_in_python_order() {
    let fixture = SqliteFixture::new(0, &[]);
    fixture
        .connection()
        .execute(
            "UPDATE tunnels SET boss_progress='{}' WHERE discord_id=123 AND guild_id=5",
            [],
        )
        .unwrap();
    let blocked = fixture.service().preview(123, 5).unwrap();
    assert!(!blocked.can_prestige);
    assert!(
        blocked
            .reason
            .unwrap()
            .starts_with("Bosses remaining: 25, 50")
    );

    let maxed = SqliteFixture::new(i64::from(MAX_PRESTIGE), &[]);
    let blocked = maxed.service().preview(123, 5).unwrap();
    assert_eq!(
        blocked.reason.as_deref(),
        Some("Already at max prestige (20).")
    );
}

#[test]
fn migrated_settlement_commits_full_reset_grant_relic_and_audit() {
    let fixture = SqliteFixture::new(0, &[]);
    let mut service = fixture.service();
    let preview = service.preview(123, 5).unwrap();
    let selected = preview.offered_perks[0].id.clone();
    let result = service
        .prestige(DigPrestigeRequest {
            discord_id: 123,
            guild_id: 5,
            perk_choice: &selected,
            mutation_choice: None,
            now: 1_700_000_000,
        })
        .expect("prestige settlement");
    assert_eq!(result.prestige_level, 1);
    assert_eq!(result.prestige_grant.jc, 650);
    assert_eq!(result.prestige_grant.relic.unwrap().id, "echo_stone");
    assert_eq!(fixture.balance(), 1_150);
    let connection = fixture.connection();
    let tunnel: (i64, i64, i64, String, Option<String>) = connection
        .query_row(
            "SELECT CAST(depth AS INTEGER), CAST(prestige_level AS INTEGER),
                    CAST(pickaxe_tier AS INTEGER), prestige_perks, pinnacle_boss_id
               FROM tunnels WHERE discord_id=123 AND guild_id=5",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(tunnel.0, 0);
    assert_eq!(tunnel.1, 1);
    assert_eq!(tunnel.2, 4, "pickaxe carries across prestige");
    assert!(tunnel.3.contains(&selected));
    assert_eq!(tunnel.4, None);
    let detail: Value = connection
        .query_row(
            "SELECT detail FROM dig_actions WHERE id=?1",
            [result.audit_action_id],
            |row| row.get::<_, String>(0),
        )
        .map(|detail| serde_json::from_str(&detail).unwrap())
        .unwrap();
    assert_eq!(detail["level"], 1);
    assert_eq!(detail["perk"], selected);
    assert_eq!(detail["gross_jc"], 1_000);
    assert_eq!(detail["reward_multiplier"], 0.65);
}

#[test]
fn prestige_grant_applies_daily_economy_before_positive_reward_scaling() {
    const NOW: i64 = 1_700_000_000;
    let fixture = SqliteFixture::new(0, &[]);
    let event_date = cama_domain::game_date::game_date_for_timestamp(NOW as f64).unwrap();
    EconomyEventRepository::new(fixture.database.path())
        .activate_event_atomic(
            Some(5),
            &EventDraft {
                event_date,
                name: "Double Prestige".to_owned(),
                hero: "Earthshaker".to_owned(),
                direction: EventDirection::Neutral,
                severity: 1,
                target_effect_jc: 0,
                forecast_flow_jc: 0,
                expected_effect_jc: 0,
                monetary_stock_before: 0,
                effects: EventEffects {
                    reward_multiplier: 2.0,
                    ..EventEffects::default()
                },
                announcement: "Double Prestige".to_owned(),
                starts_at: NOW - 60,
                ends_at: NOW + 60,
                created_at: NOW - 60,
            },
        )
        .unwrap();
    let preview = fixture.service().preview(123, 5).unwrap();
    let selected = preview.offered_perks[0].id.clone();
    let mut config = DigRuntimeConfig::default();
    config.economy_event.enabled = true;
    let result = DigPrestigeRuntimeService::with_entropy(
        fixture.database.path(),
        config,
        FixedRelicEntropy(0),
    )
    .prestige(DigPrestigeRequest {
        discord_id: 123,
        guild_id: 5,
        perk_choice: &selected,
        mutation_choice: None,
        now: NOW,
    })
    .unwrap();

    assert_eq!(result.prestige_grant.jc, 1_300);
    assert_eq!(result.balance_after, 1_800);
    assert_eq!(fixture.balance(), 1_800);
}

#[test]
fn forged_unoffered_perk_is_rejected_without_any_sqlite_mutation() {
    let fixture = SqliteFixture::new(0, &[]);
    let mut service = fixture.service();
    let preview = service.preview(123, 5).unwrap();
    let forged = PRESTIGE_PERKS
        .iter()
        .find(|perk| !preview.offered_perks.iter().any(|shown| shown.id == **perk))
        .unwrap();
    assert!(matches!(
        service.prestige(DigPrestigeRequest {
            discord_id: 123,
            guild_id: 5,
            perk_choice: forged,
            mutation_choice: None,
            now: 1_700_000_000,
        }),
        Err(DigPrestigeRuntimeError::InvalidPerk)
    ));
    assert_eq!(fixture.balance(), 500);
    assert_eq!(service.preview(123, 5).unwrap().current_level, 0);
}

#[test]
fn capped_perk_rejection_is_atomic_on_migrated_sqlite() {
    let fixture = SqliteFixture::new(0, &["loot_multiplier"; PRESTIGE_PERK_STACK_CAP]);
    let mut service = fixture.service();
    assert!(matches!(
        service.prestige(DigPrestigeRequest {
            discord_id: 123,
            guild_id: 5,
            perk_choice: "loot_multiplier",
            mutation_choice: None,
            now: 1_700_000_000,
        }),
        Err(DigPrestigeRuntimeError::PerkAtCap)
    ));
    assert_eq!(fixture.balance(), 500);
    assert_eq!(service.preview(123, 5).unwrap().current_level, 0);
}

#[test]
fn p8_mutation_preview_and_settlement_use_the_same_stable_choices() {
    let fixture = SqliteFixture::new(7, &[]);
    let mut service = fixture.service();
    let preview = service.preview(123, 5).unwrap();
    let mutations = preview.mutation.expect("P8 mutation preview");
    assert_eq!(mutations.choices.len(), 3);
    let mutation = mutations.choices[1].id.clone();
    let perk = preview.offered_perks[0].id.clone();
    let result = service
        .prestige(DigPrestigeRequest {
            discord_id: 123,
            guild_id: 5,
            perk_choice: &perk,
            mutation_choice: Some(&mutation),
            now: 1_700_000_000,
        })
        .unwrap();
    assert_eq!(
        result
            .mutations
            .as_ref()
            .and_then(|mutations| mutations.chosen.as_ref())
            .map(|chosen| chosen.id.as_str()),
        Some(mutation.as_str())
    );
    let stored: Value = fixture
        .connection()
        .query_row(
            "SELECT mutations FROM tunnels WHERE discord_id=123 AND guild_id=5",
            [],
            |row| row.get::<_, String>(0),
        )
        .map(|value| serde_json::from_str(&value).unwrap())
        .unwrap();
    assert_eq!(stored.as_array().unwrap().len(), 2);
    assert_eq!(stored[1]["id"], mutation);
}

#[test]
fn forged_mutation_is_rejected_before_grant_or_reset() {
    let fixture = SqliteFixture::new(7, &[]);
    let mut service = fixture.service();
    let preview = service.preview(123, 5).unwrap();
    let perk = preview.offered_perks[0].id.clone();
    assert!(matches!(
        service.prestige(DigPrestigeRequest {
            discord_id: 123,
            guild_id: 5,
            perk_choice: &perk,
            mutation_choice: Some("not-a-shown-mutation"),
            now: 1_700_000_000,
        }),
        Err(DigPrestigeRuntimeError::InvalidMutation)
    ));
    assert_eq!(fixture.balance(), 500);
    assert_eq!(service.preview(123, 5).unwrap().current_level, 7);
}

#[test]
fn post_commit_audit_failure_preserves_python_visible_failure_boundary() {
    let fixture = SqliteFixture::new(0, &[]);
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER fail_prestige_audit
             BEFORE INSERT ON dig_actions
             WHEN NEW.action_type='prestige'
             BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END;",
        )
        .unwrap();
    let mut service = fixture.service();
    let perk = service.preview(123, 5).unwrap().offered_perks[0].id.clone();
    assert!(matches!(
        service.prestige(DigPrestigeRequest {
            discord_id: 123,
            guild_id: 5,
            perk_choice: &perk,
            mutation_choice: None,
            now: 1_700_000_000,
        }),
        Err(DigPrestigeRuntimeError::Repository(_))
    ));
    assert_eq!(fixture.balance(), 1_150);
    assert_eq!(service.preview(123, 5).unwrap().current_level, 1);
}
