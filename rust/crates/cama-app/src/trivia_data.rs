//! Dotabase-independent trivia catalog loading and normalization.
//!
//! The immutable catalog reader is represented by [`TriviaDataSource`]; this module owns only the
//! filtering, derived-stat, URL, caching, lookup, and redaction policy applied
//! after a source has returned owned rows. No application database is opened.

use std::sync::OnceLock;

use thiserror::Error;

use crate::trivia_questions::{
    AbilityData as QuestionAbilityData, HeroData as QuestionHeroData, ItemData as QuestionItemData,
    ItemSpecialValue, TriviaCatalog, VoicelineData as QuestionVoicelineData,
};

const STEAM_CDN: &str = "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawHero {
    pub id: i64,
    pub name: Option<String>,
    pub localized_name: Option<String>,
    pub real_name: Option<String>,
    pub hype: Option<String>,
    pub bio: Option<String>,
    pub primary_attribute: Option<String>,
    pub is_melee: bool,
    pub base_movement: Option<i64>,
    pub base_armor: Option<f64>,
    pub attack_rate: Option<f64>,
    pub strength_base: Option<f64>,
    pub agility_base: Option<f64>,
    pub intelligence_base: Option<f64>,
    pub strength_gain: Option<f64>,
    pub agility_gain: Option<f64>,
    pub intelligence_gain: Option<f64>,
    pub night_vision: Option<i64>,
    pub turn_rate: Option<f64>,
    pub attack_damage_min: Option<i64>,
    pub attack_damage_max: Option<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RawAbility {
    pub id: i64,
    pub name: Option<String>,
    pub localized_name: Option<String>,
    pub is_talent: bool,
    pub hero_id: Option<i64>,
    pub hero_name: Option<String>,
    pub damage_type: Option<String>,
    pub damage: Option<String>,
    pub cooldown: Option<String>,
    pub lore: Option<String>,
    pub scepter_upgrades: bool,
    pub scepter_description: Option<String>,
    pub shard_upgrades: bool,
    pub shard_description: Option<String>,
    pub innate: bool,
    pub icon_path: Option<String>,
    pub mana_cost: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RawItem {
    pub id: i64,
    pub localized_name: Option<String>,
    pub cost: Option<i64>,
    pub lore: Option<String>,
    pub neutral_tier: Option<i64>,
    pub icon_path: Option<String>,
    pub is_neutral_enhancement: bool,
    pub special_values: Vec<ItemSpecialValue>,
    pub active_cooldown: Option<String>,
    /// Parsed `ItemPurchasable`; neutral drops are exempt when false.
    pub purchasable: Option<bool>,
    /// Parsed `ItemBaseLevel` from Valve's item JSON.
    pub base_level: Option<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RawVoiceline {
    pub hero_id: Option<i64>,
    pub text: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RawFacet {
    pub id: i64,
    pub localized_name: Option<String>,
    pub hero_id: i64,
    pub hero_name: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HeroData {
    pub id: i64,
    pub name: String,
    pub localized_name: String,
    pub real_name: Option<String>,
    pub hype: Option<String>,
    pub bio: Option<String>,
    pub primary_attribute: Option<String>,
    pub is_melee: bool,
    pub base_movement: Option<i64>,
    pub base_armor: Option<f64>,
    pub attack_rate: Option<f64>,
    pub strength_gain: Option<f64>,
    pub agility_gain: Option<f64>,
    pub intelligence_gain: Option<f64>,
    pub image_url: Option<String>,
    pub armor_at_level_one: f64,
    pub night_vision: Option<i64>,
    pub turn_rate: Option<f64>,
    pub attack_damage_min: Option<i64>,
    pub attack_damage_max: Option<i64>,
    pub damage_min_at_level_one: Option<i64>,
    pub damage_max_at_level_one: Option<i64>,
}

impl From<&HeroData> for QuestionHeroData {
    fn from(value: &HeroData) -> Self {
        Self {
            id: value.id,
            localized_name: value.localized_name.clone(),
            real_name: value.real_name.clone(),
            hype: value.hype.clone(),
            bio: value.bio.clone(),
            primary_attribute: value.primary_attribute.clone(),
            is_melee: value.is_melee,
            base_movement: value.base_movement,
            attack_rate: value.attack_rate,
            strength_gain: value.strength_gain,
            agility_gain: value.agility_gain,
            intelligence_gain: value.intelligence_gain,
            image_url: value.image_url.clone(),
            armor_at_level_one: Some(value.armor_at_level_one),
            night_vision: value.night_vision,
            turn_rate: value.turn_rate,
            damage_min_at_level_one: value.damage_min_at_level_one,
            damage_max_at_level_one: value.damage_max_at_level_one,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AbilityData {
    pub id: i64,
    pub name: String,
    pub localized_name: String,
    pub hero_id: Option<i64>,
    pub hero_name: Option<String>,
    pub damage_type: Option<String>,
    pub damage: Option<String>,
    pub cooldown: Option<String>,
    pub lore: Option<String>,
    pub scepter_upgrades: bool,
    pub scepter_description: Option<String>,
    pub shard_upgrades: bool,
    pub shard_description: Option<String>,
    pub innate: bool,
    pub icon_url: Option<String>,
    pub mana_cost: Option<String>,
}

impl From<&AbilityData> for QuestionAbilityData {
    fn from(value: &AbilityData) -> Self {
        Self {
            localized_name: value.localized_name.clone(),
            hero_id: value.hero_id,
            hero_name: value.hero_name.clone(),
            damage_type: value.damage_type.clone(),
            damage: value.damage.clone(),
            cooldown: value.cooldown.clone(),
            lore: value.lore.clone(),
            scepter_upgrades: value.scepter_upgrades,
            scepter_description: value.scepter_description.clone(),
            shard_upgrades: value.shard_upgrades,
            shard_description: value.shard_description.clone(),
            innate: value.innate,
            icon_url: value.icon_url.clone(),
            mana_cost: value.mana_cost.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemData {
    pub id: i64,
    pub localized_name: String,
    pub cost: Option<i64>,
    pub lore: Option<String>,
    pub neutral_tier: Option<i64>,
    pub icon_url: Option<String>,
    pub is_neutral_enhancement: bool,
    pub special_values: Vec<ItemSpecialValue>,
    pub active_cooldown: Option<String>,
    pub base_level: Option<i64>,
    pub is_leveled: bool,
}

impl From<&ItemData> for QuestionItemData {
    fn from(value: &ItemData) -> Self {
        Self {
            localized_name: value.localized_name.clone(),
            cost: value.cost,
            lore: value.lore.clone(),
            neutral_tier: value.neutral_tier,
            icon_url: value.icon_url.clone(),
            special_values: value.special_values.clone(),
            active_cooldown: value.active_cooldown.clone(),
            is_leveled: value.is_leveled,
        }
    }
}

pub type VoicelineData = QuestionVoicelineData;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FacetData {
    pub id: i64,
    pub localized_name: String,
    pub hero_id: i64,
    pub hero_name: Option<String>,
    pub description: String,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum TriviaDataError {
    #[error("trivia data source failed: {0}")]
    Source(String),
}

/// Asset-reader boundary. Implementations acquire and release any database
/// connection inside each method and return detached, owned rows.
pub trait TriviaDataSource {
    fn heroes(&self) -> Result<Vec<RawHero>, TriviaDataError>;
    fn abilities(&self) -> Result<Vec<RawAbility>, TriviaDataError>;
    fn items(&self) -> Result<Vec<RawItem>, TriviaDataError>;
    fn voicelines(&self) -> Result<Vec<RawVoiceline>, TriviaDataError>;
    fn facets(&self) -> Result<Vec<RawFacet>, TriviaDataError>;
}

/// Process-lifetime cache equivalent to Python's one-entry loader caches.
#[derive(Debug)]
pub struct TriviaDataRegistry<S> {
    source: S,
    heroes: OnceLock<Vec<HeroData>>,
    abilities: OnceLock<Vec<AbilityData>>,
    items: OnceLock<Vec<ItemData>>,
    voicelines: OnceLock<Vec<VoicelineData>>,
    facets: OnceLock<Vec<FacetData>>,
}

impl<S> TriviaDataRegistry<S> {
    #[must_use]
    pub const fn new(source: S) -> Self {
        Self {
            source,
            heroes: OnceLock::new(),
            abilities: OnceLock::new(),
            items: OnceLock::new(),
            voicelines: OnceLock::new(),
            facets: OnceLock::new(),
        }
    }

    #[must_use]
    pub const fn source(&self) -> &S {
        &self.source
    }

    /// Mirrors Python's `cache_clear` hooks for tests and asset refreshes.
    pub fn clear_caches(&mut self) {
        self.heroes = OnceLock::new();
        self.abilities = OnceLock::new();
        self.items = OnceLock::new();
        self.voicelines = OnceLock::new();
        self.facets = OnceLock::new();
    }
}

impl<S: TriviaDataSource> TriviaDataRegistry<S> {
    pub fn load_heroes(&self) -> Result<&[HeroData], TriviaDataError> {
        cached(&self.heroes, || self.source.heroes().map(normalize_heroes))
    }

    pub fn load_abilities(&self) -> Result<&[AbilityData], TriviaDataError> {
        cached(&self.abilities, || {
            self.source.abilities().map(normalize_abilities)
        })
    }

    pub fn load_items(&self) -> Result<&[ItemData], TriviaDataError> {
        cached(&self.items, || self.source.items().map(normalize_items))
    }

    pub fn load_voicelines(&self) -> Result<&[VoicelineData], TriviaDataError> {
        cached(&self.voicelines, || {
            self.source.voicelines().map(normalize_voicelines)
        })
    }

    pub fn load_facets(&self) -> Result<&[FacetData], TriviaDataError> {
        cached(&self.facets, || self.source.facets().map(normalize_facets))
    }

    pub fn get_ability_icon_url_by_name(
        &self,
        localized_name: &str,
    ) -> Result<Option<&str>, TriviaDataError> {
        let target = localized_name.trim().to_lowercase();
        if target.is_empty() {
            return Ok(None);
        }
        Ok(self
            .load_abilities()?
            .iter()
            .find(|ability| ability.localized_name.to_lowercase() == target)
            .and_then(|ability| ability.icon_url.as_deref()))
    }

    pub fn get_hero_by_id(&self, hero_id: i64) -> Result<Option<&HeroData>, TriviaDataError> {
        Ok(self.load_heroes()?.iter().find(|hero| hero.id == hero_id))
    }

    /// Reuse the existing question engine's catalog types without duplicating
    /// its generator registry.
    pub fn question_catalog(&self) -> Result<TriviaCatalog, TriviaDataError> {
        Ok(TriviaCatalog {
            heroes: self.load_heroes()?.iter().map(Into::into).collect(),
            abilities: self.load_abilities()?.iter().map(Into::into).collect(),
            items: self.load_items()?.iter().map(Into::into).collect(),
            voicelines: self.load_voicelines()?.to_vec(),
        })
    }
}

fn cached<T>(
    cell: &OnceLock<Vec<T>>,
    load: impl FnOnce() -> Result<Vec<T>, TriviaDataError>,
) -> Result<&[T], TriviaDataError> {
    if let Some(values) = cell.get() {
        return Ok(values);
    }
    let loaded = load()?;
    let _ = cell.set(loaded);
    Ok(cell
        .get()
        .expect("either this caller or a concurrent caller initialized the cache"))
}

#[must_use]
pub fn hero_image_url(hero_name: &str) -> Option<String> {
    let slug = hero_name.replace("npc_dota_hero_", "");
    (!slug.is_empty()).then(|| format!("{STEAM_CDN}/heroes/{slug}.png"))
}

#[must_use]
pub fn ability_icon_url(icon_path: Option<&str>) -> Option<String> {
    icon_url(icon_path, "/panorama/images/spellicons/", "abilities")
}

#[must_use]
pub fn item_icon_url(icon_path: Option<&str>) -> Option<String> {
    icon_url(icon_path, "/panorama/images/items/", "items")
}

fn icon_url(icon_path: Option<&str>, prefix: &str, category: &str) -> Option<String> {
    let path = icon_path.filter(|path| !path.is_empty())?;
    let mut slug = path.replace(prefix, "").replace("_png.png", "");
    if slug.ends_with(".png") {
        slug.truncate(slug.len() - 4);
    }
    (!slug.is_empty()).then(|| format!("{STEAM_CDN}/{category}/{slug}.png"))
}

#[must_use]
pub fn redact_hero_name(text: Option<&str>, hero_name: &str) -> String {
    let Some(text) = text.filter(|text| !text.is_empty()) else {
        return String::new();
    };
    if hero_name.is_empty() {
        return text.to_owned();
    }
    crate::trivia_questions::redact_hero_name(text, hero_name)
}

fn normalize_heroes(rows: Vec<RawHero>) -> Vec<HeroData> {
    rows.into_iter()
        .map(|hero| {
            let strength = hero.strength_base.unwrap_or(0.0);
            let agility = hero.agility_base.unwrap_or(0.0);
            let intelligence = hero.intelligence_base.unwrap_or(0.0);
            let bonus = level_one_attribute_bonus(
                hero.primary_attribute.as_deref(),
                strength,
                agility,
                intelligence,
            );
            let damage_min_at_level_one = hero
                .attack_damage_min
                .filter(|damage| *damage != 0)
                .map(|damage| (damage as f64 + bonus).round_ties_even() as i64);
            let damage_max_at_level_one = hero
                .attack_damage_max
                .filter(|damage| *damage != 0)
                .map(|damage| (damage as f64 + bonus).round_ties_even() as i64);
            let name = hero.name.unwrap_or_default();
            HeroData {
                id: hero.id,
                image_url: hero_image_url(&name),
                name,
                localized_name: hero.localized_name.unwrap_or_default(),
                real_name: nonempty(hero.real_name),
                hype: nonempty(hero.hype),
                bio: nonempty(hero.bio),
                primary_attribute: hero.primary_attribute,
                is_melee: hero.is_melee,
                base_movement: hero.base_movement,
                base_armor: hero.base_armor,
                attack_rate: nonzero_f64(hero.attack_rate),
                strength_gain: nonzero_f64(hero.strength_gain),
                agility_gain: nonzero_f64(hero.agility_gain),
                intelligence_gain: nonzero_f64(hero.intelligence_gain),
                armor_at_level_one: round_four(hero.base_armor.unwrap_or(0.0) + agility / 6.0),
                night_vision: nonzero_i64(hero.night_vision),
                turn_rate: nonzero_f64(hero.turn_rate),
                attack_damage_min: nonzero_i64(hero.attack_damage_min),
                attack_damage_max: nonzero_i64(hero.attack_damage_max),
                damage_min_at_level_one,
                damage_max_at_level_one,
            }
        })
        .collect()
}

fn normalize_abilities(rows: Vec<RawAbility>) -> Vec<AbilityData> {
    rows.into_iter()
        .filter_map(|ability| {
            let localized_name = ability.localized_name?;
            if ability.is_talent || localized_name.is_empty() || localized_name.contains('_') {
                return None;
            }
            Some(AbilityData {
                id: ability.id,
                name: ability.name.unwrap_or_default(),
                localized_name,
                hero_id: ability.hero_id,
                hero_name: nonempty(ability.hero_name),
                damage_type: nonempty(ability.damage_type),
                damage: nonzero_text(ability.damage),
                cooldown: nonzero_text(ability.cooldown),
                lore: nonempty(ability.lore),
                scepter_upgrades: ability.scepter_upgrades,
                scepter_description: nonempty(ability.scepter_description),
                shard_upgrades: ability.shard_upgrades,
                shard_description: nonempty(ability.shard_description),
                innate: ability.innate,
                icon_url: ability_icon_url(ability.icon_path.as_deref()),
                mana_cost: nonzero_text(ability.mana_cost),
            })
        })
        .collect()
}

fn normalize_items(rows: Vec<RawItem>) -> Vec<ItemData> {
    let filtered = rows
        .into_iter()
        .filter_map(|item| {
            let name = item.localized_name.as_deref()?;
            if name.is_empty() || name.contains('_') {
                return None;
            }
            if item.neutral_tier.is_none() && item.purchasable == Some(false) {
                return None;
            }
            Some(item)
        })
        .collect::<Vec<_>>();
    let mut name_counts = std::collections::BTreeMap::new();
    for item in &filtered {
        if let Some(name) = item.localized_name.as_ref() {
            *name_counts.entry(name.clone()).or_insert(0_usize) += 1;
        }
    }
    filtered
        .into_iter()
        .map(|item| {
            let mut localized_name = item.localized_name.unwrap_or_default();
            let is_leveled = name_counts.get(&localized_name).copied().unwrap_or(0) > 1
                && item.base_level.is_some();
            if let Some(level) = item.base_level.filter(|_| is_leveled) {
                let suffix = format!(" {level}");
                if !localized_name.ends_with(&suffix) {
                    localized_name.push_str(&suffix);
                }
            }
            ItemData {
                id: item.id,
                localized_name,
                cost: item.cost.filter(|cost| *cost > 0),
                lore: nonempty(item.lore),
                neutral_tier: item.neutral_tier,
                icon_url: item_icon_url(item.icon_path.as_deref()),
                is_neutral_enhancement: item.is_neutral_enhancement,
                special_values: item.special_values,
                active_cooldown: nonzero_text(item.active_cooldown),
                base_level: item.base_level,
                is_leveled,
            }
        })
        .collect()
}

fn normalize_voicelines(rows: Vec<RawVoiceline>) -> Vec<VoicelineData> {
    rows.into_iter()
        .filter_map(|voiceline| {
            let hero_id = voiceline.hero_id?;
            let text = voiceline.text.unwrap_or_default().trim().to_owned();
            let length = text.chars().count();
            if !(15..=120).contains(&length)
                || matches!(text.to_lowercase().as_str(), "" | "hahaha" | "ha ha ha")
            {
                return None;
            }
            Some(VoicelineData { hero_id, text })
        })
        .collect()
}

fn normalize_facets(rows: Vec<RawFacet>) -> Vec<FacetData> {
    rows.into_iter()
        .filter_map(|facet| {
            let localized_name = facet.localized_name.filter(|value| !value.is_empty())?;
            let description = facet.description.filter(|value| !value.is_empty())?;
            Some(FacetData {
                id: facet.id,
                localized_name,
                hero_id: facet.hero_id,
                hero_name: nonempty(facet.hero_name),
                description,
            })
        })
        .collect()
}

fn level_one_attribute_bonus(
    primary: Option<&str>,
    strength: f64,
    agility: f64,
    intelligence: f64,
) -> f64 {
    match primary {
        Some("strength") => strength,
        Some("agility") => agility,
        Some("intelligence") => intelligence,
        Some("universal") => 0.7 * (strength + agility + intelligence),
        _ => 0.0,
    }
}

fn round_four(value: f64) -> f64 {
    (value * 10_000.0).round_ties_even() / 10_000.0
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

fn nonzero_text(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty() && value != "0")
}

fn nonzero_i64(value: Option<i64>) -> Option<i64> {
    value.filter(|value| *value != 0)
}

fn nonzero_f64(value: Option<f64>) -> Option<f64> {
    value.filter(|value| *value != 0.0)
}

#[cfg(test)]
mod tests;
