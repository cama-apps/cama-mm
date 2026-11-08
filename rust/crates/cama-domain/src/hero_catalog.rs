//! The bundled Dota hero-name catalog (`data/heroes.json`), parsed once and
//! shared by every surface that names a hero.

use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Every bundled hero name, keyed by Dota hero ID.
#[must_use]
pub fn hero_names() -> &'static BTreeMap<i64, String> {
    static NAMES: OnceLock<BTreeMap<i64, String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        serde_json::from_str::<BTreeMap<String, String>>(include_str!("../data/heroes.json"))
            .expect("the bundled hero catalog is valid JSON")
            .into_iter()
            .filter_map(|(id, name)| Some((id.parse().ok()?, name)))
            .collect()
    })
}

/// The bundled name for `hero_id`, if the catalog has one.
#[must_use]
pub fn bundled_hero_name(hero_id: i64) -> Option<&'static str> {
    hero_names().get(&hero_id).map(String::as_str)
}

/// The display name for `hero_id`; a hero the catalog does not know yet reads
/// `Hero <id>`.
#[must_use]
pub fn hero_name(hero_id: i64) -> String {
    bundled_hero_name(hero_id).map_or_else(|| format!("Hero {hero_id}"), str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_names_every_bundled_hero_by_numeric_id() {
        assert!(hero_names().len() > 100);
        assert_eq!(hero_name(1), "Anti-Mage");
        assert_eq!(hero_name(44), "Phantom Assassin");
        assert_eq!(bundled_hero_name(14), Some("Pudge"));
    }

    #[test]
    fn unknown_heroes_share_one_fallback_label() {
        assert_eq!(bundled_hero_name(9_999), None);
        assert_eq!(hero_name(9_999), "Hero 9999");
    }
}
