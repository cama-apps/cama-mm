use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

#[derive(Clone, Default)]
struct FixtureSource {
    hero_lists: Arc<AtomicUsize>,
    ability_lists: Arc<AtomicUsize>,
}

impl DotaInfoSource for FixtureSource {
    fn all_heroes(&self) -> Result<Vec<(String, i64)>, DotaInfoError> {
        self.hero_lists.fetch_add(1, Ordering::Relaxed);
        Ok((0..101)
            .map(|index| {
                if index == 0 {
                    ("Pudge".to_owned(), 14)
                } else {
                    (format!("Hero {index}"), i64::from(index + 100))
                }
            })
            .collect())
    }

    fn all_abilities(&self) -> Result<Vec<(String, i64)>, DotaInfoError> {
        self.ability_lists.fetch_add(1, Ordering::Relaxed);
        Ok((0..201)
            .map(|index| {
                if index == 0 {
                    ("Meat Hook".to_owned(), 14)
                } else {
                    (format!("Ability {index}"), i64::from(index + 200))
                }
            })
            .collect())
    }

    fn hero_by_name(&self, name: &str) -> Result<Option<DotaHero>, DotaInfoError> {
        let normalized = name.to_lowercase();
        Ok(matches!(normalized.as_str(), "anti-mage" | "am").then(anti_mage))
    }

    fn ability_by_name(&self, name: &str) -> Result<Option<DotaAbility>, DotaInfoError> {
        Ok(name.eq_ignore_ascii_case("Meat Hook").then(|| DotaAbility {
            id: 14,
            localized_name: "Meat Hook".to_owned(),
            behavior: Some("UNIT_TARGET|AOE".to_owned()),
            damage_type: Some("pure".to_owned()),
            description: Some("Launches a bloody hook toward a unit or location.".to_owned()),
            ability_special: Some(
                r#"[{"header":"DAMAGE:","value":"90 180 270 360"},{"header":"RANGE:","value":"1100 1200 1300 1400"}]"#
                    .to_owned(),
            ),
            lore: Some("The Butcher's hook is a symbolic nightmare.".to_owned()),
            ..DotaAbility::default()
        }))
    }
}

fn anti_mage() -> DotaHero {
    DotaHero {
        id: 1,
        name: "npc_dota_hero_antimage".to_owned(),
        localized_name: "Anti-Mage".to_owned(),
        aliases: Some("antimage|am|magina".to_owned()),
        roles: Some("Carry|Escape|Nuker".to_owned()),
        hype: Some("Magic shall not prevail.".to_owned()),
        primary_attribute: Some("agility".to_owned()),
        strength_base: Some(21),
        agility_base: Some(24),
        intelligence_base: Some(12),
        strength_gain: Some(1.6),
        agility_gain: Some(2.8),
        intelligence_gain: Some(1.8),
        attack_damage_min: Some(29),
        attack_damage_max: Some(33),
        attack_range: Some(150),
        is_melee: true,
        attack_rate: Some(1.4),
        base_movement: Some(310),
        base_armor: Some(2),
        magic_resistance: Some(25),
        abilities: vec!["Mana Break".to_owned(), "Blink".to_owned()],
        talents: (1..=8).map(|index| format!("Talent {index}")).collect(),
        facets: vec![
            DotaFacet {
                localized_name: "Magebane's Mirror".to_owned(),
                description: "Reflects targeted spells.".to_owned(),
            },
            DotaFacet {
                localized_name: "Mana Thirst".to_owned(),
                description: "Senses low-mana enemies.".to_owned(),
            },
        ],
    }
}

#[test]
fn test_get_all_heroes_shape() {
    let service = DotaInfoService::new(FixtureSource::default());
    let heroes = service.hero_autocomplete("").unwrap();
    assert_eq!(heroes.len(), 25);
    assert_eq!(heroes[0].name, "Pudge");
    assert_eq!(heroes[0].value, "Pudge");
}

#[test]
fn test_hero_by_name_lookup() {
    let service = DotaInfoService::new(FixtureSource::default());
    for query in ["Anti-Mage", "anti-mage", "am"] {
        assert_eq!(service.hero(query).unwrap().unwrap().title, "Anti-Mage");
    }
    assert!(service.hero("NotARealHero123").unwrap().is_none());
}

#[test]
fn test_hero_abilities_and_talents() {
    let embed = DotaInfoService::new(FixtureSource::default())
        .hero("Anti-Mage")
        .unwrap()
        .unwrap();
    let abilities = embed
        .fields
        .iter()
        .find(|field| field.name == "Abilities")
        .unwrap();
    assert!(abilities.value.contains("Blink"));
    let talents = embed
        .fields
        .iter()
        .find(|field| field.name == "Talents")
        .unwrap();
    assert_eq!(talents.value.lines().count(), 8);
}

#[test]
fn test_lookup_helpers_release_database_connections() {
    let source = FixtureSource::default();
    let hero_lists = Arc::clone(&source.hero_lists);
    let ability_lists = Arc::clone(&source.ability_lists);
    let service = DotaInfoService::new(source);
    service.hero_autocomplete("pud").unwrap();
    service.hero_autocomplete("udge").unwrap();
    service.ability_autocomplete("hook").unwrap();
    service.ability_autocomplete("meat").unwrap();
    assert_eq!(hero_lists.load(Ordering::Relaxed), 1);
    assert_eq!(ability_lists.load(Ordering::Relaxed), 1);
}

#[test]
fn test_get_all_abilities_shape() {
    let choices = DotaInfoService::new(FixtureSource::default())
        .ability_autocomplete("")
        .unwrap();
    assert_eq!(choices.len(), 25);
    assert_eq!(choices[0].name, "Meat Hook");
}

#[test]
fn test_ability_by_name_lookup() {
    let service = DotaInfoService::new(FixtureSource::default());
    for query in ["Meat Hook", "meat hook"] {
        let embed = service.ability(query).unwrap().unwrap();
        assert_eq!(embed.title, "Meat Hook");
        assert!(embed.description.unwrap().contains("bloody hook"));
    }
    assert!(service.ability("NotARealAbility123").unwrap().is_none());
}

#[test]
fn test_format_ability_values() {
    assert_eq!(format_ability_values(None), "");
    let result = format_ability_values(Some(
        r#"[{"header":"DAMAGE:","value":"90 180 270 360"},{"header":"RANGE:","value":"1100 1200 1300 1400"}]"#,
    ));
    assert!(result.contains("Damage"));
    assert!(result.contains("90 180 270 360"));
}

#[test]
fn test_hero_attributes_and_roles() {
    let embed = DotaInfoService::new(FixtureSource::default())
        .hero("Anti-Mage")
        .unwrap()
        .unwrap();
    assert_eq!(embed.color, 0x43_A0_47);
    assert!(embed.fields.iter().any(|field| {
        field.name == "Attributes (AGI)" && field.value.contains("**STR:** 21 (+1.6)")
    }));
    assert!(
        embed
            .fields
            .iter()
            .any(|field| field.name == "Roles" && field.value == "Carry, Escape, Nuker")
    );
}

#[test]
fn test_hero_has_facets_with_descriptions() {
    let embed = DotaInfoService::new(FixtureSource::default())
        .hero("Anti-Mage")
        .unwrap()
        .unwrap();
    let facets = embed
        .fields
        .iter()
        .find(|field| field.name == "Facets")
        .unwrap();
    assert_eq!(facets.value.lines().count(), 2);
    assert!(facets.value.contains("Magebane's Mirror"));
    assert!(facets.value.contains("Reflects targeted spells."));
}
