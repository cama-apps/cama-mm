//! Dota hero and ability reference policy over an immutable catalog.

use std::sync::Mutex;

use cama_domain::embed_safety::truncate_field;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DotaChoice {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DotaHero {
    pub id: i64,
    pub name: String,
    pub localized_name: String,
    pub aliases: Option<String>,
    pub roles: Option<String>,
    pub hype: Option<String>,
    pub primary_attribute: Option<String>,
    pub strength_base: Option<i64>,
    pub agility_base: Option<i64>,
    pub intelligence_base: Option<i64>,
    pub strength_gain: Option<f64>,
    pub agility_gain: Option<f64>,
    pub intelligence_gain: Option<f64>,
    pub attack_damage_min: Option<i64>,
    pub attack_damage_max: Option<i64>,
    pub attack_range: Option<i64>,
    pub is_melee: bool,
    pub attack_rate: Option<f64>,
    pub base_movement: Option<i64>,
    pub base_armor: Option<i64>,
    pub magic_resistance: Option<i64>,
    pub abilities: Vec<String>,
    pub talents: Vec<String>,
    pub facets: Vec<DotaFacet>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DotaFacet {
    pub localized_name: String,
    pub description: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DotaAbility {
    pub id: i64,
    pub localized_name: String,
    pub behavior: Option<String>,
    pub damage_type: Option<String>,
    pub description: Option<String>,
    pub ability_special: Option<String>,
    pub scepter_upgrades: bool,
    pub scepter_description: Option<String>,
    pub shard_upgrades: bool,
    pub shard_description: Option<String>,
    pub lore: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DotaInfoEmbed {
    pub title: String,
    pub description: Option<String>,
    pub color: u32,
    pub thumbnail_url: Option<String>,
    pub fields: Vec<DotaInfoField>,
    pub footer: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DotaInfoField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum DotaInfoError {
    #[error("Dota catalog failed: {0}")]
    Catalog(String),
}

pub trait DotaInfoSource: Send + Sync + 'static {
    fn all_heroes(&self) -> Result<Vec<(String, i64)>, DotaInfoError>;
    fn all_abilities(&self) -> Result<Vec<(String, i64)>, DotaInfoError>;
    fn hero_by_name(&self, name: &str) -> Result<Option<DotaHero>, DotaInfoError>;
    fn ability_by_name(&self, name: &str) -> Result<Option<DotaAbility>, DotaInfoError>;
}

pub struct DotaInfoService<S> {
    source: S,
    hero_choices: Mutex<Option<Vec<(String, i64)>>>,
    ability_choices: Mutex<Option<Vec<(String, i64)>>>,
}

impl<S> DotaInfoService<S> {
    #[must_use]
    pub fn new(source: S) -> Self {
        Self {
            source,
            hero_choices: Mutex::new(None),
            ability_choices: Mutex::new(None),
        }
    }

    #[must_use]
    pub const fn source(&self) -> &S {
        &self.source
    }
}

impl<S: DotaInfoSource> DotaInfoService<S> {
    pub fn hero_autocomplete(&self, current: &str) -> Result<Vec<DotaChoice>, DotaInfoError> {
        let values = cached_choices(&self.hero_choices, || self.source.all_heroes())?;
        Ok(filter_choices(&values, current))
    }

    pub fn ability_autocomplete(&self, current: &str) -> Result<Vec<DotaChoice>, DotaInfoError> {
        let values = cached_choices(&self.ability_choices, || self.source.all_abilities())?;
        Ok(filter_choices(&values, current))
    }

    pub fn hero(&self, name: &str) -> Result<Option<DotaInfoEmbed>, DotaInfoError> {
        self.source
            .hero_by_name(name)
            .map(|hero| hero.map(|hero| hero_embed(&hero)))
    }

    pub fn ability(&self, name: &str) -> Result<Option<DotaInfoEmbed>, DotaInfoError> {
        self.source
            .ability_by_name(name)
            .map(|ability| ability.map(|ability| ability_embed(&ability)))
    }
}

fn cached_choices(
    cache: &Mutex<Option<Vec<(String, i64)>>>,
    load: impl FnOnce() -> Result<Vec<(String, i64)>, DotaInfoError>,
) -> Result<Vec<(String, i64)>, DotaInfoError> {
    let mut guard = cache
        .lock()
        .map_err(|_| DotaInfoError::Catalog("Dota catalog cache lock poisoned".to_owned()))?;
    if let Some(values) = guard.as_ref() {
        return Ok(values.clone());
    }
    let values = load()?;
    *guard = Some(values.clone());
    Ok(values)
}

fn filter_choices(values: &[(String, i64)], current: &str) -> Vec<DotaChoice> {
    let current = current.to_lowercase();
    values
        .iter()
        .filter(|(name, _)| name.to_lowercase().contains(&current))
        .take(25)
        .map(|(name, _)| DotaChoice {
            name: name.clone(),
            value: name.clone(),
        })
        .collect()
}

#[must_use]
pub fn format_ability_values(ability_special: Option<&str>) -> String {
    let Some(serde_json::Value::Array(values)) =
        ability_special.and_then(|value| serde_json::from_str(value).ok())
    else {
        return String::new();
    };
    values
        .iter()
        .take(6)
        .filter_map(|value| {
            let value = value.as_object()?;
            let header = value
                .get("header")
                .or_else(|| value.get("key"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let raw_value = value
                .get("value")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if header.is_empty() || raw_value.is_empty() {
                return None;
            }
            let header = title_words(header.trim_end_matches(':').replace('_', " ").trim());
            Some(format!("• {header}: {raw_value}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn title_words(value: &str) -> String {
    value
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn hero_embed(hero: &DotaHero) -> DotaInfoEmbed {
    let (attribute_name, color) = match hero.primary_attribute.as_deref() {
        Some("strength") => ("STR", 0xE5_39_35),
        Some("agility") => ("AGI", 0x43_A0_47),
        Some("intelligence") => ("INT", 0x1E_88_E5),
        _ => ("UNI", 0x8E_24_AA),
    };
    let mut embed = DotaInfoEmbed {
        title: hero.localized_name.clone(),
        description: hero.hype.as_deref().map(|hype| truncate_python(hype, 200)),
        color,
        thumbnail_url: Some(format!(
            "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes/{}.png",
            hero.name.trim_start_matches("npc_dota_hero_")
        )),
        fields: Vec::new(),
        footer: None,
    };
    let attributes = [
        ("STR", hero.strength_base, hero.strength_gain),
        ("AGI", hero.agility_base, hero.agility_gain),
        ("INT", hero.intelligence_base, hero.intelligence_gain),
    ]
    .into_iter()
    .filter_map(|(name, base, gain)| {
        base.filter(|base| *base != 0).map(|base| {
            format!(
                "**{name}:** {base} (+{})",
                python_float(gain.unwrap_or(0.0))
            )
        })
    })
    .collect::<Vec<_>>();
    push_lines(
        &mut embed,
        &format!("Attributes ({attribute_name})"),
        attributes,
        true,
    );

    let mut combat = Vec::new();
    if let (Some(minimum), Some(maximum)) = (hero.attack_damage_min, hero.attack_damage_max)
        && minimum != 0
        && maximum != 0
    {
        combat.push(format!("**Damage:** {minimum}-{maximum}"));
    }
    if let Some(range) = hero.attack_range.filter(|range| *range != 0) {
        combat.push(format!(
            "**Range:** {range} ({})",
            if hero.is_melee { "Melee" } else { "Ranged" }
        ));
    }
    if let Some(rate) = hero.attack_rate.filter(|rate| *rate != 0.0) {
        combat.push(format!("**Attack Time:** {}", python_float(rate)));
    }
    if let Some(speed) = hero.base_movement.filter(|speed| *speed != 0) {
        combat.push(format!("**Move Speed:** {speed}"));
    }
    push_lines(&mut embed, "Combat", combat, true);

    let mut defense = Vec::new();
    if let Some(armor) = hero.base_armor {
        defense.push(format!("**Armor:** {armor}"));
    }
    if let Some(resistance) = hero.magic_resistance.filter(|value| *value != 0) {
        defense.push(format!("**Magic Resist:** {resistance}%"));
    }
    push_lines(&mut embed, "Defense", defense, true);
    if let Some(roles) = hero.roles.as_deref().filter(|roles| !roles.is_empty()) {
        embed.fields.push(DotaInfoField {
            name: "Roles".to_owned(),
            value: roles.replace('|', ", "),
            inline: false,
        });
    }
    push_lines(
        &mut embed,
        "Abilities",
        hero.abilities
            .iter()
            .take(6)
            .map(|name| format!("• {name}"))
            .collect(),
        true,
    );
    push_lines(
        &mut embed,
        "Talents",
        hero.talents
            .iter()
            .take(8)
            .map(|name| format!("• {name}"))
            .collect(),
        true,
    );
    push_lines(
        &mut embed,
        "Facets",
        hero.facets
            .iter()
            .map(|facet| {
                format!(
                    "**{}**: {}",
                    facet.localized_name,
                    truncate_python(&facet.description, 60)
                )
            })
            .collect(),
        false,
    );
    let slug = hero
        .name
        .trim_start_matches("npc_dota_hero_")
        .replace('_', "-");
    embed.fields.push(DotaInfoField {
        name: "Links".to_owned(),
        value: format!(
            "[Dotabuff](https://www.dotabuff.com/heroes/{slug}) | [OpenDota](https://www.opendota.com/heroes/{})",
            hero.id
        ),
        inline: false,
    });
    embed
}

fn ability_embed(ability: &DotaAbility) -> DotaInfoEmbed {
    let mut embed = DotaInfoEmbed {
        title: ability.localized_name.clone(),
        description: Some(truncate_field(
            ability
                .description
                .as_deref()
                .unwrap_or("No description available."),
            4_096,
        )),
        color: 0x72_89_DA,
        thumbnail_url: None,
        fields: Vec::new(),
        footer: ability
            .lore
            .as_deref()
            .map(|lore| truncate_python(lore, 150)),
    };
    let mut properties = Vec::new();
    if let Some(behavior) = ability
        .behavior
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        properties.push(format!(
            "**Behavior:** {}",
            title_words(&behavior.replace(['|', '_'], " "))
        ));
    }
    if let Some(damage_type) = ability
        .damage_type
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        properties.push(format!("**Damage Type:** {}", title_words(damage_type)));
    }
    push_lines(&mut embed, "Properties", properties, false);
    let values = format_ability_values(ability.ability_special.as_deref());
    if !values.is_empty() {
        embed.fields.push(DotaInfoField {
            name: "Values".to_owned(),
            value: values,
            inline: false,
        });
    }
    let mut upgrades = Vec::new();
    if ability.scepter_upgrades
        && let Some(description) = ability.scepter_description.as_deref()
    {
        upgrades.push(format!(
            "**Scepter:** {}",
            truncate_python(description, 100)
        ));
    }
    if ability.shard_upgrades
        && let Some(description) = ability.shard_description.as_deref()
    {
        upgrades.push(format!("**Shard:** {}", truncate_python(description, 100)));
    }
    push_lines(&mut embed, "Upgrades", upgrades, false);
    embed
}

fn push_lines(embed: &mut DotaInfoEmbed, name: &str, lines: Vec<String>, inline: bool) {
    if !lines.is_empty() {
        embed.fields.push(DotaInfoField {
            name: name.to_owned(),
            value: lines.join("\n"),
            inline,
        });
    }
}

fn truncate_python(value: &str, maximum: usize) -> String {
    if value.chars().count() > maximum {
        format!("{}...", value.chars().take(maximum).collect::<String>())
    } else {
        value.to_owned()
    }
}

fn python_float(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.1}")
    } else {
        value.to_string()
    }
}

#[cfg(test)]
#[path = "dota_info/tests.rs"]
mod tests;
