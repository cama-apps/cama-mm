use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

#[derive(Debug)]
struct FixtureRows {
    heroes: Vec<RawHero>,
    abilities: Vec<RawAbility>,
    items: Vec<RawItem>,
    voicelines: Vec<RawVoiceline>,
    facets: Vec<RawFacet>,
}

#[derive(Clone, Debug)]
struct FixtureSource {
    rows: Arc<FixtureRows>,
    checked_out: Arc<AtomicUsize>,
}

impl FixtureSource {
    fn new() -> Self {
        Self {
            rows: Arc::new(fixture_rows()),
            checked_out: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn connection(&self) -> ConnectionGuard<'_> {
        self.checked_out.fetch_add(1, Ordering::SeqCst);
        ConnectionGuard(&self.checked_out)
    }

    fn checked_out(&self) -> usize {
        self.checked_out.load(Ordering::SeqCst)
    }
}

struct ConnectionGuard<'a>(&'a AtomicUsize);

impl Drop for ConnectionGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl TriviaDataSource for FixtureSource {
    fn heroes(&self) -> Result<Vec<RawHero>, TriviaDataError> {
        let _connection = self.connection();
        Ok(self.rows.heroes.clone())
    }

    fn abilities(&self) -> Result<Vec<RawAbility>, TriviaDataError> {
        let _connection = self.connection();
        Ok(self.rows.abilities.clone())
    }

    fn items(&self) -> Result<Vec<RawItem>, TriviaDataError> {
        let _connection = self.connection();
        Ok(self.rows.items.clone())
    }

    fn voicelines(&self) -> Result<Vec<RawVoiceline>, TriviaDataError> {
        let _connection = self.connection();
        Ok(self.rows.voicelines.clone())
    }

    fn facets(&self) -> Result<Vec<RawFacet>, TriviaDataError> {
        let _connection = self.connection();
        Ok(self.rows.facets.clone())
    }
}

fn registry() -> TriviaDataRegistry<FixtureSource> {
    TriviaDataRegistry::new(FixtureSource::new())
}

fn fixture_rows() -> FixtureRows {
    let heroes = (0..101)
        .map(|index| RawHero {
            id: i64::from(index + 1),
            name: Some(if index == 0 {
                "npc_dota_hero_antimage".to_owned()
            } else {
                format!("npc_dota_hero_fixture_{index}")
            }),
            localized_name: Some(if index == 0 {
                "Anti-Mage".to_owned()
            } else {
                format!("Fixture Hero {index}")
            }),
            primary_attribute: Some("agility".to_owned()),
            is_melee: index % 2 == 0,
            base_movement: Some(300),
            base_armor: Some(1.25),
            attack_rate: Some(1.7),
            strength_base: Some(20.0),
            agility_base: Some(24.0 + f64::from(index % 3)),
            intelligence_base: Some(18.0),
            strength_gain: Some(2.0),
            agility_gain: Some(3.0),
            intelligence_gain: Some(1.5),
            night_vision: Some(800),
            turn_rate: Some(0.6),
            attack_damage_min: Some(20 + i64::from(index % 5)),
            attack_damage_max: Some(24 + i64::from(index % 5)),
            ..RawHero::default()
        })
        .collect();

    let mut abilities = (0..501)
        .map(|index| RawAbility {
            id: i64::from(index + 1),
            name: Some(if index == 0 {
                "silencer_global_silence".to_owned()
            } else {
                format!("fixture_ability_{index}")
            }),
            localized_name: Some(if index == 0 {
                "Global Silence".to_owned()
            } else {
                format!("Fixture Ability {index}")
            }),
            hero_id: Some(i64::from(index % 101 + 1)),
            hero_name: Some(format!("Fixture Hero {}", index % 101)),
            icon_path: Some(if index == 0 {
                "/panorama/images/spellicons/silencer_global_silence_png.png".to_owned()
            } else {
                format!("/panorama/images/spellicons/fixture_{index}_png.png")
            }),
            ..RawAbility::default()
        })
        .collect::<Vec<_>>();
    abilities.extend([
        RawAbility {
            id: 90_001,
            localized_name: Some("internal_hidden_ability".to_owned()),
            ..RawAbility::default()
        },
        RawAbility {
            id: 90_002,
            localized_name: Some("Talent Display Name".to_owned()),
            is_talent: true,
            ..RawAbility::default()
        },
        RawAbility {
            id: 90_003,
            localized_name: None,
            ..RawAbility::default()
        },
    ]);

    let mut items = (0..101)
        .map(|index| RawItem {
            id: i64::from(index + 1),
            localized_name: Some(format!("Fixture Item {index}")),
            cost: Some(50 + i64::from(index)),
            icon_path: Some(format!("/panorama/images/items/fixture_{index}_png.png")),
            purchasable: Some(true),
            ..RawItem::default()
        })
        .collect::<Vec<_>>();
    items.push(RawItem {
        id: 1_001,
        localized_name: Some("Magic Wand Recipe".to_owned()),
        cost: Some(150),
        purchasable: Some(true),
        ..RawItem::default()
    });
    items.extend((1..=5).map(|level| RawItem {
        id: 1_100 + level,
        localized_name: Some("Dagon".to_owned()),
        cost: Some(2_700 + level * 500),
        purchasable: Some(true),
        base_level: Some(level),
        ..RawItem::default()
    }));
    items.push(RawItem {
        id: 1_200,
        localized_name: Some("Dagon Recipe".to_owned()),
        cost: Some(1_000),
        purchasable: Some(true),
        ..RawItem::default()
    });
    for (offset, name) in [
        "Trident",
        "Iron Talon",
        "Crystal Raindrop",
        "Faded Broach",
        "Gossamer Cape",
        "Broom Handle",
    ]
    .into_iter()
    .enumerate()
    {
        items.push(RawItem {
            id: 2_000 + i64::try_from(offset).unwrap(),
            localized_name: Some(name.to_owned()),
            purchasable: Some(false),
            ..RawItem::default()
        });
    }
    items.push(RawItem {
        id: 3_000,
        localized_name: Some("item_internal_fixture".to_owned()),
        purchasable: Some(true),
        ..RawItem::default()
    });

    let mut voicelines = (0..101)
        .map(|index| RawVoiceline {
            hero_id: Some(i64::from(index % 101 + 1)),
            text: Some(format!(
                "The battle begins and our enemies will remember this moment {index}."
            )),
        })
        .collect::<Vec<_>>();
    voicelines.extend([
        RawVoiceline {
            hero_id: Some(1),
            text: Some("hahaha".to_owned()),
        },
        RawVoiceline {
            hero_id: None,
            text: Some("This otherwise valid line has no hero identifier.".to_owned()),
        },
    ]);

    let mut facets = (0..31)
        .map(|index| RawFacet {
            id: i64::from(index + 1),
            localized_name: Some(format!("Fixture Facet {index}")),
            hero_id: i64::from(index % 101 + 1),
            hero_name: Some(format!("Fixture Hero {}", index % 101)),
            description: Some(format!("A complete facet description for case {index}.")),
        })
        .collect::<Vec<_>>();
    facets.push(RawFacet {
        id: 99_999,
        localized_name: Some("Missing Description".to_owned()),
        hero_id: 1,
        description: None,
        ..RawFacet::default()
    });

    FixtureRows {
        heroes,
        abilities,
        items,
        voicelines,
        facets,
    }
}

#[test]
fn test_cdn_url_hero_image() {
    let result = hero_image_url("npc_dota_hero_antimage").unwrap();
    assert!(result.contains("/dota_react/heroes/antimage.png"));
    assert!(!result.contains(".png.png"));
}

#[test]
fn test_cdn_url_empty_hero_image() {
    assert_eq!(hero_image_url(""), None);
}

#[test]
fn test_cdn_url_ability_icon_png_suffix() {
    let result = ability_icon_url(Some(
        "/panorama/images/spellicons/antimage_mana_break_png.png",
    ))
    .unwrap();
    assert!(result.contains("/dota_react/abilities/antimage_mana_break.png"));
    assert!(!result.contains(".png.png"));
}

#[test]
fn test_cdn_url_ability_icon_plain_suffix() {
    let result = ability_icon_url(Some("/panorama/images/spellicons/some_ability.png")).unwrap();
    assert!(result.contains("/dota_react/abilities/some_ability.png"));
    assert!(!result.contains(".png.png"));
}

#[test]
fn test_cdn_url_ability_icon_none() {
    assert_eq!(ability_icon_url(None), None);
}

#[test]
fn test_cdn_url_item_icon_png_suffix() {
    let result = item_icon_url(Some("/panorama/images/items/blink_png.png")).unwrap();
    assert!(result.contains("/dota_react/items/blink.png"));
    assert!(!result.contains(".png.png"));
}

#[test]
fn test_cdn_url_item_icon_plain_suffix() {
    let result = item_icon_url(Some("/panorama/images/items/recipe.png")).unwrap();
    assert!(result.contains("/dota_react/items/recipe.png"));
    assert!(!result.contains(".png.png"));
}

#[test]
fn test_cdn_url_item_icon_none() {
    assert_eq!(item_icon_url(None), None);
}

#[test]
fn test_load_heroes() {
    let registry = registry();
    let heroes = registry.load_heroes().unwrap();
    assert!(heroes.len() > 100);
    assert!(!heroes[0].localized_name.is_empty());
    assert!(heroes[0].id > 0);
    assert_eq!(
        registry.question_catalog().unwrap().heroes.len(),
        heroes.len()
    );
}

#[test]
fn test_load_abilities() {
    let registry = registry();
    let abilities = registry.load_abilities().unwrap();
    assert!(abilities.len() > 500);
    assert!(
        abilities
            .iter()
            .take(20)
            .all(|ability| !ability.localized_name.is_empty())
    );
}

#[test]
fn test_load_items() {
    let registry = registry();
    let items = registry.load_items().unwrap();
    assert!(items.len() > 100);
    assert!(items.iter().filter(|item| item.cost.is_some()).count() > 50);
}

#[test]
fn test_load_items_excludes_internal_names() {
    let registry = registry();
    assert!(
        registry
            .load_items()
            .unwrap()
            .iter()
            .all(|item| !item.localized_name.contains('_'))
    );
}

#[test]
fn test_load_items_excludes_unavailable() {
    let registry = registry();
    let names = registry
        .load_items()
        .unwrap()
        .iter()
        .map(|item| item.localized_name.as_str())
        .collect::<Vec<_>>();
    for unavailable in [
        "Trident",
        "Iron Talon",
        "Crystal Raindrop",
        "Faded Broach",
        "Gossamer Cape",
        "Broom Handle",
    ] {
        assert!(!names.contains(&unavailable), "leaked {unavailable}");
    }
}

#[test]
fn test_load_items_keeps_recipe_scrolls() {
    let registry = registry();
    assert!(
        registry
            .load_items()
            .unwrap()
            .iter()
            .any(|item| item.localized_name == "Magic Wand Recipe")
    );
}

#[test]
fn test_load_items_dagon_level_suffix() {
    let registry = registry();
    let names = registry
        .load_items()
        .unwrap()
        .iter()
        .map(|item| item.localized_name.as_str())
        .collect::<Vec<_>>();
    for level in 1..=5 {
        assert!(names.contains(&format!("Dagon {level}").as_str()));
    }
    assert!(!names.contains(&"Dagon"));
}

#[test]
fn test_load_items_dagon_is_leveled() {
    let registry = registry();
    let mut leveled = registry
        .load_items()
        .unwrap()
        .iter()
        .filter(|item| item.is_leveled)
        .collect::<Vec<_>>();
    leveled.sort_by_key(|item| item.base_level);
    assert_eq!(
        leveled
            .iter()
            .map(|item| item.localized_name.as_str())
            .collect::<Vec<_>>(),
        ["Dagon 1", "Dagon 2", "Dagon 3", "Dagon 4", "Dagon 5"]
    );
    assert_eq!(
        leveled
            .iter()
            .map(|item| item.base_level.unwrap())
            .collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
}

#[test]
fn test_hero_damage_at_level1() {
    let registry = registry();
    for hero in registry.load_heroes().unwrap() {
        if let Some(raw_minimum) = hero.attack_damage_min {
            let minimum = hero.damage_min_at_level_one.unwrap();
            let maximum = hero.damage_max_at_level_one.unwrap();
            assert!(maximum >= minimum);
            assert!(minimum >= raw_minimum);
        }
    }
}

#[test]
fn test_hero_armor_at_level1() {
    let registry = registry();
    let heroes = registry.load_heroes().unwrap();
    assert!(
        heroes
            .iter()
            .all(|hero| hero.armor_at_level_one.is_finite())
    );
    let anti_mage = heroes
        .iter()
        .find(|hero| hero.localized_name == "Anti-Mage")
        .unwrap();
    let expected = 1.25 + 24.0 / 6.0;
    assert!((anti_mage.armor_at_level_one - expected).abs() < 0.0001);
}

#[test]
fn test_loaders_release_database_connections() {
    let mut registry = registry();
    registry.clear_caches();
    let before = registry.source().checked_out();

    registry.load_heroes().unwrap();
    registry.load_abilities().unwrap();
    registry.load_items().unwrap();
    registry.load_voicelines().unwrap();
    registry.load_facets().unwrap();

    assert_eq!(registry.source().checked_out(), before);
}

#[test]
fn test_load_abilities_excludes_internal_names() {
    let registry = registry();
    assert!(
        registry
            .load_abilities()
            .unwrap()
            .iter()
            .all(|ability| !ability.localized_name.contains('_'))
    );
}

#[test]
fn test_get_ability_icon_url_by_name_is_case_insensitive() {
    let registry = registry();
    assert_eq!(
        registry
            .get_ability_icon_url_by_name("  global silence  ")
            .unwrap(),
        Some(
            "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/abilities/silencer_global_silence.png"
        )
    );
}

#[test]
fn test_get_ability_icon_url_by_name_missing() {
    let registry = registry();
    assert_eq!(
        registry
            .get_ability_icon_url_by_name("Definitely Not An Ability")
            .unwrap(),
        None
    );
}

#[test]
fn test_load_voicelines() {
    let registry = registry();
    let voicelines = registry.load_voicelines().unwrap();
    assert!(voicelines.len() > 100);
    assert!(
        voicelines
            .iter()
            .take(10)
            .all(|voiceline| voiceline.hero_id != 0 && voiceline.text.chars().count() >= 15)
    );
}

#[test]
fn test_load_facets() {
    let registry = registry();
    let facets = registry.load_facets().unwrap();
    assert!(facets.len() > 30);
    assert!(
        facets
            .iter()
            .take(10)
            .all(|facet| !facet.localized_name.is_empty() && facet.hero_id != 0)
    );
}

#[test]
fn test_get_hero_by_id() {
    let registry = registry();
    let id = registry.load_heroes().unwrap()[0].id;
    assert_eq!(registry.get_hero_by_id(id).unwrap().unwrap().id, id);
}

#[test]
fn test_get_hero_by_id_missing() {
    let registry = registry();
    assert_eq!(registry.get_hero_by_id(999_999).unwrap(), None);
}

#[test]
fn test_redacts_full_name() {
    let result = redact_hero_name(Some("Anti-Mage is a powerful hero"), "Anti-Mage");
    assert!(!result.contains("Anti-Mage"));
    assert!(result.contains("???"));
}

#[test]
fn test_redacts_name_parts() {
    let result = redact_hero_name(Some("The Phantom Assassin strikes"), "Phantom Assassin");
    assert!(!result.contains("Phantom"));
    assert!(!result.contains("Assassin"));
}

#[test]
fn test_does_not_redact_short_words() {
    let result = redact_hero_name(Some("The io of the game"), "Io");
    assert!(result.contains("game"));
}

#[test]
fn test_empty_text() {
    assert_eq!(redact_hero_name(Some(""), "Hero"), "");
}

#[test]
fn test_none_text() {
    assert_eq!(redact_hero_name(None, "Hero"), "");
}
