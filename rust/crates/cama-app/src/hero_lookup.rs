//! Cached Dota hero names and read-only Dotabase metadata.
//!
//! Names are compiled from the same `utils/heroes.json` snapshot used by the
//! Python bot. Rich metadata is loaded once from the immutable Dotabase SQLite
//! artifact and detached before the connection closes.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use rusqlite::{Connection, OpenFlags};
use thiserror::Error;

use crate::dotabase_sqlite::PRODUCTION_DOTABASE_PATH;

const STEAM_CDN_BASE: &str =
    "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes";

static HERO_NAMES: OnceLock<BTreeMap<String, String>> = OnceLock::new();

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeroMetadata {
    pub slug: String,
    pub color: Option<u32>,
    pub localized_name: String,
    pub roles: Vec<String>,
}

#[derive(Debug, Error)]
pub enum HeroLookupError {
    #[error("Dotabase hero metadata query failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Debug)]
pub struct HeroLookup {
    dotabase_path: PathBuf,
    metadata: OnceLock<Result<BTreeMap<i64, HeroMetadata>, String>>,
}

impl HeroLookup {
    #[must_use]
    pub fn new(dotabase_path: impl AsRef<Path>) -> Self {
        Self {
            dotabase_path: dotabase_path.as_ref().to_path_buf(),
            metadata: OnceLock::new(),
        }
    }

    #[must_use]
    pub fn production() -> Self {
        Self::new(PRODUCTION_DOTABASE_PATH)
    }

    fn metadata(&self) -> Result<&BTreeMap<i64, HeroMetadata>, HeroLookupError> {
        self.metadata
            .get_or_init(|| {
                load_dotabase_metadata(&self.dotabase_path).map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(|error| {
                HeroLookupError::Sqlite(rusqlite::Error::InvalidParameterName(error.clone()))
            })
    }

    pub fn image_url(&self, hero_id: i64, size: HeroImageSize) -> Option<String> {
        let slug = self.metadata().ok()?.get(&hero_id)?.slug.trim();
        if slug.is_empty() {
            return None;
        }
        Some(match size {
            HeroImageSize::Full => format!("{STEAM_CDN_BASE}/{slug}.png"),
            HeroImageSize::Icon => format!("{STEAM_CDN_BASE}/icons/{slug}.png"),
        })
    }

    pub fn color(&self, hero_id: i64) -> Option<u32> {
        self.metadata().ok()?.get(&hero_id)?.color
    }

    #[must_use]
    pub fn roles(&self, hero_id: i64) -> Vec<String> {
        self.metadata()
            .ok()
            .and_then(|metadata| metadata.get(&hero_id))
            .map(|hero| hero.roles.clone())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn classify_role(&self, hero_id: i64) -> HeroRoleClass {
        if self.roles(hero_id).iter().any(|role| role == "Support") {
            HeroRoleClass::Support
        } else {
            HeroRoleClass::Core
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeroImageSize {
    Full,
    Icon,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeroRoleClass {
    Core,
    Support,
}

impl HeroRoleClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Core => "Core",
            Self::Support => "Support",
        }
    }
}

#[must_use]
pub fn hero_name(hero_id: i64) -> String {
    hero_names()
        .get(&hero_id.to_string())
        .cloned()
        .unwrap_or_else(|| format!("Hero {hero_id}"))
}

#[must_use]
pub fn hero_short_name(hero_id: i64) -> String {
    if let Some(abbreviation) = hero_abbreviation(hero_id) {
        return abbreviation.to_owned();
    }
    let name = hero_name(hero_id);
    name.split_whitespace().next().unwrap_or(&name).to_owned()
}

#[must_use]
pub fn all_heroes() -> BTreeMap<String, String> {
    hero_names().clone()
}

fn hero_names() -> &'static BTreeMap<String, String> {
    HERO_NAMES.get_or_init(|| {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../utils/heroes.json"
        )))
        .unwrap_or_default()
    })
}

const fn hero_abbreviation(hero_id: i64) -> Option<&'static str> {
    match hero_id {
        1 => Some("AM"),
        5 => Some("CM"),
        6 => Some("Drow"),
        7 => Some("ES"),
        11 => Some("SF"),
        12 => Some("PL"),
        17 => Some("Storm"),
        21 => Some("WR"),
        27 => Some("SS"),
        36 => Some("Necro"),
        39 => Some("QoP"),
        41 => Some("Void"),
        42 => Some("WK"),
        43 => Some("DP"),
        44 => Some("PA"),
        46 => Some("TA"),
        49 => Some("DK"),
        51 => Some("Clock"),
        53 => Some("NP"),
        54 => Some("LS"),
        55 => Some("DS"),
        60 => Some("NS"),
        62 => Some("BH"),
        68 => Some("AA"),
        71 => Some("SB"),
        72 => Some("Gyro"),
        74 => Some("Invo"),
        76 => Some("OD"),
        80 => Some("LD"),
        81 => Some("CK"),
        83 => Some("Treant"),
        84 => Some("Ogre"),
        88 => Some("Nyx"),
        90 => Some("KotL"),
        96 => Some("Centaur"),
        98 => Some("Timber"),
        99 => Some("BB"),
        101 => Some("Sky"),
        103 => Some("ET"),
        104 => Some("LC"),
        106 => Some("Ember"),
        107 => Some("Earth"),
        109 => Some("TB"),
        112 => Some("WW"),
        113 => Some("AW"),
        114 => Some("MK"),
        119 => Some("Willow"),
        120 => Some("Pango"),
        126 => Some("Void Spirit"),
        135 => Some("Dawn"),
        137 => Some("PB"),
        _ => None,
    }
}

fn load_dotabase_metadata(path: &Path) -> Result<BTreeMap<i64, HeroMetadata>, HeroLookupError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.pragma_update(None, "query_only", true)?;
    let mut statement = connection.prepare(
        "SELECT id,COALESCE(name,''),COALESCE(color,''),
                COALESCE(localized_name,''),COALESCE(roles,'')
         FROM heroes ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| {
        let slug = row.get::<_, String>(1)?;
        let slug = slug
            .strip_prefix("npc_dota_hero_")
            .unwrap_or(&slug)
            .to_owned();
        let color = parse_color(&row.get::<_, String>(2)?);
        let roles = row
            .get::<_, String>(4)?
            .split('|')
            .filter(|role| !role.is_empty())
            .map(str::to_owned)
            .collect();
        Ok((
            row.get(0)?,
            HeroMetadata {
                slug,
                color,
                localized_name: row.get(3)?,
                roles,
            },
        ))
    })?;
    rows.collect::<Result<_, _>>().map_err(Into::into)
}

fn parse_color(value: &str) -> Option<u32> {
    let value = value.strip_prefix('#').unwrap_or(value);
    (!value.is_empty())
        .then(|| u32::from_str_radix(value, 16).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    use super::*;

    fn fixture() -> NamedTempFile {
        let file = NamedTempFile::new().expect("create hero fixture");
        let connection = Connection::open(file.path()).expect("open hero fixture");
        connection
            .execute_batch(
                "CREATE TABLE heroes (
                    id INTEGER PRIMARY KEY,name TEXT,color TEXT,localized_name TEXT,roles TEXT
                 );
                 INSERT INTO heroes VALUES
                    (1,'npc_dota_hero_antimage','#784094','Anti-Mage','Carry|Escape|Nuker'),
                    (5,'npc_dota_hero_crystal_maiden','#52ecff','Crystal Maiden',
                     'Support|Disabler|Nuker');",
            )
            .expect("populate hero fixture");
        file
    }

    #[test]
    fn test_get_hero_name_known_hero() {
        assert_eq!(hero_name(1), "Anti-Mage");
        assert_eq!(hero_name(2), "Axe");
        assert_eq!(hero_name(14), "Pudge");
        assert_eq!(hero_name(44), "Phantom Assassin");
    }

    #[test]
    fn test_get_hero_name_unknown_hero() {
        assert_eq!(hero_name(9_999), "Hero 9999");
    }

    #[test]
    fn test_get_hero_short_name_abbreviations() {
        assert_eq!(hero_short_name(1), "AM");
        assert_eq!(hero_short_name(5), "CM");
        assert_eq!(hero_short_name(44), "PA");
        assert_eq!(hero_short_name(46), "TA");
    }

    #[test]
    fn test_get_hero_short_name_first_word() {
        assert_eq!(hero_short_name(3), "Bane");
        assert_eq!(hero_short_name(2), "Axe");
    }

    #[test]
    fn test_get_all_heroes() {
        let heroes = all_heroes();
        assert!(heroes.len() > 100);
        assert_eq!(heroes.get("1").map(String::as_str), Some("Anti-Mage"));
    }

    #[test]
    fn test_hero_lookup_is_cached() {
        let first = hero_names() as *const _;
        assert_eq!(hero_name(1), "Anti-Mage");
        let second = hero_names() as *const _;
        assert_eq!(first, second);
    }

    #[test]
    fn test_all_hero_ids_are_strings() {
        assert!(all_heroes().keys().all(|key| key.parse::<i64>().is_ok()));
    }

    #[test]
    fn test_common_heroes_exist() {
        let heroes = all_heroes();
        for (id, name) in [
            ("1", "Anti-Mage"),
            ("14", "Pudge"),
            ("74", "Invoker"),
            ("129", "Mars"),
        ] {
            assert_eq!(heroes.get(id).map(String::as_str), Some(name));
        }
    }

    #[test]
    fn test_image_and_color_come_from_bundled_hero_data() {
        let file = fixture();
        let lookup = HeroLookup::new(file.path());
        assert_eq!(
            lookup.image_url(1, HeroImageSize::Full).as_deref(),
            Some(
                "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/\
                 dota_react/heroes/antimage.png"
            )
        );
        assert_eq!(
            lookup.image_url(1, HeroImageSize::Icon).as_deref(),
            Some(
                "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/\
                 dota_react/heroes/icons/antimage.png"
            )
        );
        assert_eq!(lookup.color(1), Some(0x78_40_94));
    }

    #[test]
    fn test_roles_come_from_bundled_hero_data() {
        let file = fixture();
        let lookup = HeroLookup::new(file.path());
        assert_eq!(lookup.roles(1), ["Carry", "Escape", "Nuker"]);
        assert_eq!(lookup.classify_role(1).as_str(), "Core");
        assert_eq!(lookup.roles(5), ["Support", "Disabler", "Nuker"]);
        assert_eq!(lookup.classify_role(5).as_str(), "Support");
    }

    #[test]
    fn test_metadata_loaders_release_database_connections() {
        let file = fixture();
        let lookup = HeroLookup::new(file.path());
        assert_eq!(lookup.roles(1), ["Carry", "Escape", "Nuker"]);

        let mut connection = Connection::open(file.path()).expect("reopen detached fixture");
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Exclusive)
            .expect("metadata reader released its connection");
        transaction
            .rollback()
            .expect("release exclusive fixture lock");
    }
}
