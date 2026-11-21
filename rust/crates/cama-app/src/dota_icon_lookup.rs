//! Item and ability icons by game key, read through the Dotabase catalog.
//!
//! OpenDota names a purchase by the item's internal key (`blink`,
//! `black_king_bar`); Steam serves the icon under the same key, so the key is
//! taken from the icon path. A missing or unreadable Dotabase yields no icons.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::dotabase_sqlite::{DotabaseSqliteSource, PRODUCTION_DOTABASE_PATH};
use crate::trivia_data::{
    RawAbility, RawFacet, RawHero, RawItem, RawVoiceline, TriviaDataError, TriviaDataRegistry,
    TriviaDataSource,
};

/// The most ability icons one hero block shows.
pub const MAX_HERO_ABILITY_ICONS: usize = 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DotaItem {
    /// OpenDota purchase-log key, such as `blink`.
    pub key: String,
    pub name: String,
    /// Shop price in gold; zero when Dotabase has none.
    pub cost: i64,
    pub icon_url: String,
}

#[derive(Debug, Default)]
struct Icons {
    items: Vec<DotaItem>,
    abilities: BTreeMap<i64, Vec<String>>,
}

#[derive(Debug)]
pub struct DotaIconLookup {
    dotabase_path: PathBuf,
    icons: OnceLock<Icons>,
}

impl DotaIconLookup {
    #[must_use]
    pub fn new(dotabase_path: impl AsRef<Path>) -> Self {
        Self {
            dotabase_path: dotabase_path.as_ref().to_path_buf(),
            icons: OnceLock::new(),
        }
    }

    #[must_use]
    pub fn production() -> Self {
        Self::new(PRODUCTION_DOTABASE_PATH)
    }

    /// A lookup over rows already in hand instead of a Dotabase file.
    #[must_use]
    pub fn from_rows(items: Vec<RawItem>, abilities: Vec<RawAbility>) -> Self {
        let icons = OnceLock::new();
        let _ = icons.set(catalog_icons(Rows { items, abilities }));
        Self {
            dotabase_path: PathBuf::new(),
            icons,
        }
    }

    fn icons(&self) -> &Icons {
        self.icons
            .get_or_init(|| catalog_icons(DotabaseSqliteSource::new(&self.dotabase_path)))
    }

    /// Every purchasable item with an icon, in Dotabase order.
    #[must_use]
    pub fn items(&self) -> &[DotaItem] {
        &self.icons().items
    }

    #[must_use]
    pub fn item(&self, key: &str) -> Option<&DotaItem> {
        self.items().iter().find(|item| item.key == key)
    }

    /// Icon URLs for the hero's abilities in cast order, at most
    /// [`MAX_HERO_ABILITY_ICONS`]. Talents and innate abilities are left out.
    #[must_use]
    pub fn ability_icon_urls(&self, hero_id: i64) -> &[String] {
        self.icons()
            .abilities
            .get(&hero_id)
            .map_or(&[], Vec::as_slice)
    }
}

/// Item and ability rows standing in for a Dotabase file.
struct Rows {
    items: Vec<RawItem>,
    abilities: Vec<RawAbility>,
}

impl TriviaDataSource for Rows {
    fn heroes(&self) -> Result<Vec<RawHero>, TriviaDataError> {
        Ok(Vec::new())
    }

    fn abilities(&self) -> Result<Vec<RawAbility>, TriviaDataError> {
        Ok(self.abilities.clone())
    }

    fn items(&self) -> Result<Vec<RawItem>, TriviaDataError> {
        Ok(self.items.clone())
    }

    fn voicelines(&self) -> Result<Vec<RawVoiceline>, TriviaDataError> {
        Ok(Vec::new())
    }

    fn facets(&self) -> Result<Vec<RawFacet>, TriviaDataError> {
        Ok(Vec::new())
    }
}

fn icon_key(url: &str) -> Option<&str> {
    let key = url.rsplit('/').next()?.strip_suffix(".png")?;
    (!key.is_empty()).then_some(key)
}

fn catalog_icons(source: impl TriviaDataSource) -> Icons {
    let catalog = TriviaDataRegistry::new(source);
    let mut icons = Icons::default();
    for item in catalog.load_items().unwrap_or_default() {
        let Some(icon_url) = item.icon_url.as_deref() else {
            continue;
        };
        let Some(key) = icon_key(icon_url) else {
            continue;
        };
        // Leveled items share an icon; the first row is the base item.
        if icons.items.iter().any(|known| known.key == key) {
            continue;
        }
        // The catalog numbers a leveled item's name ("Dagon 1"); the shop
        // calls it by the plain name.
        let level = item
            .base_level
            .filter(|_| item.is_leveled)
            .map(|level| format!(" {level}"));
        let name = level
            .as_deref()
            .and_then(|suffix| item.localized_name.strip_suffix(suffix))
            .unwrap_or(&item.localized_name);
        icons.items.push(DotaItem {
            key: key.to_owned(),
            name: name.to_owned(),
            cost: item.cost.unwrap_or(0),
            icon_url: icon_url.to_owned(),
        });
    }
    for ability in catalog.load_abilities().unwrap_or_default() {
        let (Some(hero_id), Some(url), false) =
            (ability.hero_id, ability.icon_url.as_ref(), ability.innate)
        else {
            continue;
        };
        let urls = icons.abilities.entry(hero_id).or_default();
        if urls.len() < MAX_HERO_ABILITY_ICONS && !urls.contains(url) {
            urls.push(url.clone());
        }
    }
    icons
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, cost: Option<i64>, key: Option<&str>) -> RawItem {
        RawItem {
            localized_name: Some(name.to_owned()),
            cost,
            icon_path: key.map(|key| format!("/panorama/images/items/{key}_png.png")),
            ..RawItem::default()
        }
    }

    fn ability(id: i64, name: &str, hero_id: i64, slug: &str) -> RawAbility {
        RawAbility {
            id,
            localized_name: Some(name.to_owned()),
            hero_id: Some(hero_id),
            icon_path: Some(format!("/panorama/images/spellicons/{slug}_png.png")),
            ..RawAbility::default()
        }
    }

    fn lookup() -> DotaIconLookup {
        let mut dagon = item("Dagon", Some(2850), Some("dagon"));
        dagon.base_level = Some(1);
        let mut dagon_two = item("Dagon", Some(4000), Some("dagon_2"));
        dagon_two.base_level = Some(2);
        let mut aegis = item("Aegis of the Immortal", None, Some("aegis"));
        aegis.purchasable = Some(false);
        let mut innate = ability(12, "Persecutor", 1, "antimage_persecutor");
        innate.innate = true;
        let mut talent = ability(13, "+10 Agility", 1, "talent");
        talent.is_talent = true;
        DotaIconLookup::from_rows(
            vec![
                item("Blink Dagger", Some(2250), Some("blink")),
                dagon,
                dagon_two,
                item("Tango", None, Some("tango")),
                item("hidden_item", Some(10), Some("hidden")),
                item("No Icon", Some(10), None),
                aegis,
            ],
            vec![
                ability(10, "Mana Break", 1, "antimage_mana_break"),
                ability(11, "Blink", 1, "antimage_blink"),
                innate,
                talent,
                ability(14, "Counterspell", 1, "antimage_counterspell"),
                ability(15, "Mana Void", 1, "antimage_mana_void"),
                ability(16, "Blink Fragment", 1, "antimage_blink_fragment"),
                ability(17, "hidden_ability", 1, "hidden"),
                ability(20, "Battle Hunger", 2, "axe_battle_hunger"),
            ],
        )
    }

    #[test]
    fn items_are_keyed_by_their_purchase_log_name() {
        let lookup = lookup();
        let keys = lookup
            .items()
            .iter()
            .map(|item| (item.key.as_str(), item.name.as_str(), item.cost))
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            [
                ("blink", "Blink Dagger", 2250),
                ("dagon", "Dagon", 2850),
                ("dagon_2", "Dagon", 4000),
                ("tango", "Tango", 0),
            ]
        );
        assert!(
            lookup
                .item("blink")
                .unwrap()
                .icon_url
                .ends_with("/items/blink.png")
        );
        assert!(lookup.item("rapier").is_none());
    }

    #[test]
    fn a_hero_shows_at_most_four_castable_abilities_in_order() {
        let lookup = lookup();
        let slugs = lookup
            .ability_icon_urls(1)
            .iter()
            .map(|url| icon_key(url).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            slugs,
            [
                "antimage_mana_break",
                "antimage_blink",
                "antimage_counterspell",
                "antimage_mana_void",
            ]
        );
        assert_eq!(lookup.ability_icon_urls(2).len(), 1);
        assert!(lookup.ability_icon_urls(99).is_empty());
    }

    #[test]
    fn a_missing_dotabase_yields_no_icons() {
        let directory = tempfile::tempdir().unwrap();
        let lookup = DotaIconLookup::new(directory.path().join("absent.db"));
        assert!(lookup.items().is_empty());
        assert!(lookup.ability_icon_urls(1).is_empty());
    }
}
