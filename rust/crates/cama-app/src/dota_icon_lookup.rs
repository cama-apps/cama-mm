//! Item and ability icons by game key, read from the bundled Dotabase.
//!
//! OpenDota names a purchase by the item's internal key (`blink`,
//! `black_king_bar`); Steam serves the icon under the same key, so the key is
//! taken from the icon path. A missing or unreadable Dotabase yields no icons.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rusqlite::{Connection, OpenFlags};

use crate::dotabase_sqlite::PRODUCTION_DOTABASE_PATH;
use crate::trivia_data::{ability_icon_url, item_icon_url};

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

    fn icons(&self) -> &Icons {
        self.icons
            .get_or_init(|| load(&self.dotabase_path).unwrap_or_default())
    }

    /// Every item with an icon, in Dotabase order.
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

fn icon_key(url: &str) -> Option<&str> {
    let key = url.rsplit('/').next()?.strip_suffix(".png")?;
    (!key.is_empty()).then_some(key)
}

fn load(path: &Path) -> Result<Icons, rusqlite::Error> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.pragma_update(None, "query_only", true)?;
    let mut icons = Icons::default();

    let mut statement =
        connection.prepare("SELECT localized_name,cost,icon FROM items ORDER BY id")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let name = row.get::<_, Option<String>>(0)?.unwrap_or_default();
        let Some(icon_url) = item_icon_url(row.get::<_, Option<String>>(2)?.as_deref()) else {
            continue;
        };
        let Some(key) = icon_key(&icon_url).map(str::to_owned) else {
            continue;
        };
        // Leveled items share a name; the first row is the base item.
        if name.is_empty() || name.contains('_') || icons.items.iter().any(|item| item.key == key) {
            continue;
        }
        icons.items.push(DotaItem {
            key,
            name,
            cost: row.get::<_, Option<i64>>(1)?.unwrap_or(0).max(0),
            icon_url,
        });
    }

    let mut statement = connection.prepare(
        "SELECT a.hero_id,a.icon FROM abilities a
         WHERE a.hero_id IS NOT NULL
           AND a.localized_name IS NOT NULL AND instr(a.localized_name,'_')=0
           AND COALESCE(a.innate,0)=0
           AND NOT EXISTS (SELECT 1 FROM talents t WHERE t.ability_id=a.id)
         ORDER BY a.hero_id,a.id",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let hero_id: i64 = row.get(0)?;
        let Some(url) = ability_icon_url(row.get::<_, Option<String>>(1)?.as_deref()) else {
            continue;
        };
        let urls = icons.abilities.entry(hero_id).or_default();
        if urls.len() < MAX_HERO_ABILITY_ICONS && !urls.contains(&url) {
            urls.push(url);
        }
    }
    Ok(icons)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dotabase(directory: &Path) -> PathBuf {
        let path = directory.join("dotabase.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE items (id INTEGER PRIMARY KEY,localized_name TEXT,cost INTEGER,icon TEXT);
                 CREATE TABLE abilities (
                     id INTEGER PRIMARY KEY,localized_name TEXT,hero_id INTEGER,icon TEXT,innate INTEGER
                 );
                 CREATE TABLE talents (ability_id INTEGER);
                 INSERT INTO items VALUES
                     (1,'Blink Dagger',2250,'/panorama/images/items/blink_png.png'),
                     (2,'Dagon',2850,'/panorama/images/items/dagon_png.png'),
                     (3,'Dagon',4000,'/panorama/images/items/dagon_png.png'),
                     (4,'Tango',NULL,'/panorama/images/items/tango_png.png'),
                     (5,'hidden_item',10,'/panorama/images/items/hidden_png.png'),
                     (6,'No Icon',10,NULL);
                 INSERT INTO abilities VALUES
                     (10,'Mana Break',1,'/panorama/images/spellicons/antimage_mana_break_png.png',0),
                     (11,'Blink',1,'/panorama/images/spellicons/antimage_blink_png.png',0),
                     (12,'Persecutor',1,'/panorama/images/spellicons/antimage_persecutor_png.png',1),
                     (13,'+10 Agility',1,'/panorama/images/spellicons/talent_png.png',0),
                     (14,'Counterspell',1,'/panorama/images/spellicons/antimage_counterspell_png.png',0),
                     (15,'Mana Void',1,'/panorama/images/spellicons/antimage_mana_void_png.png',0),
                     (16,'Blink Fragment',1,'/panorama/images/spellicons/antimage_blink_fragment_png.png',0),
                     (17,'hidden_ability',1,'/panorama/images/spellicons/hidden_png.png',0),
                     (20,'Berserker''s Call',2,'/panorama/images/spellicons/axe_berserkers_call_png.png',0);
                 INSERT INTO talents VALUES (13);",
            )
            .unwrap();
        path
    }

    #[test]
    fn items_are_keyed_by_their_purchase_log_name() {
        let directory = tempfile::tempdir().unwrap();
        let lookup = DotaIconLookup::new(dotabase(directory.path()));
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
        let directory = tempfile::tempdir().unwrap();
        let lookup = DotaIconLookup::new(dotabase(directory.path()));
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
