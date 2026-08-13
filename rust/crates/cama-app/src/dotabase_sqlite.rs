//! Read-only access to the immutable SQLite catalog bundled by Dotabase.
//!
//! The runtime image supplies the same pinned `dotabase.db` artifact as the
//! Python package. Every call opens it read-only, enables `query_only`, returns
//! detached rows, and closes the connection before application policy runs.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;

use crate::dota_info::{DotaAbility, DotaFacet, DotaHero, DotaInfoError, DotaInfoSource};
use crate::trivia_data::{
    RawAbility, RawFacet, RawHero, RawItem, RawVoiceline, TriviaDataError, TriviaDataSource,
};
use crate::trivia_questions::ItemSpecialValue;

/// Location copied into the Rust production image from the pinned package.
pub const PRODUCTION_DOTABASE_PATH: &str = "/app/assets/dotabase.db";

#[derive(Clone, Debug)]
pub struct DotabaseSqliteSource {
    path: PathBuf,
}

impl DotabaseSqliteSource {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    #[must_use]
    pub fn production() -> Self {
        Self::new(PRODUCTION_DOTABASE_PATH)
    }

    fn connection(&self) -> Result<Connection, TriviaDataError> {
        let connection = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(source_error)?;
        connection
            .pragma_update(None, "query_only", true)
            .map_err(source_error)?;
        Ok(connection)
    }
}

impl TriviaDataSource for DotabaseSqliteSource {
    fn heroes(&self) -> Result<Vec<RawHero>, TriviaDataError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT id,name,localized_name,real_name,hype,bio,attr_primary,is_melee,
                        base_movement,base_armor,attack_rate,attr_strength_base,
                        attr_agility_base,attr_intelligence_base,attr_strength_gain,
                        attr_agility_gain,attr_intelligence_gain,vision_night,turn_rate,
                        attack_damage_min,attack_damage_max
                 FROM heroes ORDER BY id",
            )
            .map_err(source_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok(RawHero {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    localized_name: row.get(2)?,
                    real_name: row.get(3)?,
                    hype: row.get(4)?,
                    bio: row.get(5)?,
                    primary_attribute: row.get(6)?,
                    is_melee: row.get::<_, Option<bool>>(7)?.unwrap_or(false),
                    base_movement: row.get(8)?,
                    base_armor: row.get(9)?,
                    attack_rate: row.get(10)?,
                    strength_base: row.get(11)?,
                    agility_base: row.get(12)?,
                    intelligence_base: row.get(13)?,
                    strength_gain: row.get(14)?,
                    agility_gain: row.get(15)?,
                    intelligence_gain: row.get(16)?,
                    night_vision: row.get(17)?,
                    turn_rate: row.get(18)?,
                    attack_damage_min: row.get(19)?,
                    attack_damage_max: row.get(20)?,
                })
            })
            .map_err(source_error)?;
        collect_rows(rows)
    }

    fn abilities(&self) -> Result<Vec<RawAbility>, TriviaDataError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT a.id,a.name,a.localized_name,
                        EXISTS(SELECT 1 FROM talents t WHERE t.ability_id=a.id),
                        a.hero_id,h.localized_name,a.damage_type,a.damage,a.cooldown,a.lore,
                        a.scepter_upgrades,a.scepter_description,a.shard_upgrades,
                        a.shard_description,a.innate,a.icon,a.mana_cost
                 FROM abilities a LEFT JOIN heroes h ON h.id=a.hero_id
                 ORDER BY a.id",
            )
            .map_err(source_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok(RawAbility {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    localized_name: row.get(2)?,
                    is_talent: row.get(3)?,
                    hero_id: row.get(4)?,
                    hero_name: row.get(5)?,
                    damage_type: row.get(6)?,
                    damage: row.get(7)?,
                    cooldown: row.get(8)?,
                    lore: row.get(9)?,
                    scepter_upgrades: row.get::<_, Option<bool>>(10)?.unwrap_or(false),
                    scepter_description: row.get(11)?,
                    shard_upgrades: row.get::<_, Option<bool>>(12)?.unwrap_or(false),
                    shard_description: row.get(13)?,
                    innate: row.get::<_, Option<bool>>(14)?.unwrap_or(false),
                    icon_path: row.get(15)?,
                    mana_cost: row.get(16)?,
                })
            })
            .map_err(source_error)?;
        collect_rows(rows)
    }

    fn items(&self) -> Result<Vec<RawItem>, TriviaDataError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT id,localized_name,cost,lore,neutral_tier,icon,
                        is_neutral_enhancement,ability_special,cooldown,json_data
                 FROM items ORDER BY id",
            )
            .map_err(source_error)?;
        let rows = statement
            .query_map([], |row| {
                let ability_special: Option<String> = row.get(7)?;
                let json_data: Option<String> = row.get(9)?;
                let item_json = parse_json_object(json_data.as_deref());
                Ok(RawItem {
                    id: row.get(0)?,
                    localized_name: row.get(1)?,
                    cost: row.get(2)?,
                    lore: row.get(3)?,
                    neutral_tier: optional_integer(row.get::<_, Option<String>>(4)?),
                    icon_path: row.get(5)?,
                    is_neutral_enhancement: row.get::<_, Option<bool>>(6)?.unwrap_or(false),
                    special_values: parse_special_values(ability_special.as_deref()),
                    active_cooldown: row.get(8)?,
                    purchasable: item_json.get("ItemPurchasable").and_then(json_bool),
                    base_level: item_json.get("ItemBaseLevel").and_then(json_integer),
                })
            })
            .map_err(source_error)?;
        collect_rows(rows)
    }

    fn voicelines(&self) -> Result<Vec<RawVoiceline>, TriviaDataError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT hero_id,text_simple FROM responses
                 WHERE hero_id IS NOT NULL AND text_simple IS NOT NULL
                 ORDER BY fullname",
            )
            .map_err(source_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok(RawVoiceline {
                    hero_id: row.get(0)?,
                    text: row.get(1)?,
                })
            })
            .map_err(source_error)?;
        collect_rows(rows)
    }

    fn facets(&self) -> Result<Vec<RawFacet>, TriviaDataError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT f.id,f.localized_name,f.hero_id,h.localized_name,f.description
                 FROM facets f LEFT JOIN heroes h ON h.id=f.hero_id
                 ORDER BY f.hero_id,f.slot,f.id",
            )
            .map_err(source_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok(RawFacet {
                    id: row.get(0)?,
                    localized_name: row.get(1)?,
                    hero_id: row.get(2)?,
                    hero_name: row.get(3)?,
                    description: row.get(4)?,
                })
            })
            .map_err(source_error)?;
        collect_rows(rows)
    }
}

impl DotaInfoSource for DotabaseSqliteSource {
    fn all_heroes(&self) -> Result<Vec<(String, i64)>, DotaInfoError> {
        let connection = self.connection().map_err(dota_error)?;
        let mut statement = connection
            .prepare(
                "SELECT localized_name,id FROM heroes
                 WHERE localized_name IS NOT NULL ORDER BY id",
            )
            .map_err(dota_source_error)?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(dota_source_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(dota_source_error)
    }

    fn all_abilities(&self) -> Result<Vec<(String, i64)>, DotaInfoError> {
        let connection = self.connection().map_err(dota_error)?;
        let mut statement = connection
            .prepare(
                "SELECT a.localized_name,a.id FROM abilities a
                 WHERE a.localized_name IS NOT NULL
                   AND instr(a.localized_name,'_')=0
                   AND NOT EXISTS (SELECT 1 FROM talents t WHERE t.ability_id=a.id)
                 ORDER BY a.id",
            )
            .map_err(dota_source_error)?;
        let rows = statement
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get(1)?)))
            .map_err(dota_source_error)?;
        let mut seen = BTreeSet::new();
        let mut abilities = Vec::new();
        for row in rows {
            let (name, id) = row.map_err(dota_source_error)?;
            if !name.is_empty() && seen.insert(name.clone()) {
                abilities.push((name, id));
            }
        }
        Ok(abilities)
    }

    fn hero_by_name(&self, name: &str) -> Result<Option<DotaHero>, DotaInfoError> {
        let connection = self.connection().map_err(dota_error)?;
        let exact = connection
            .query_row(
                "SELECT id FROM heroes WHERE localized_name=?1 COLLATE NOCASE ORDER BY id LIMIT 1",
                [name],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(dota_source_error)?;
        let hero_id = match exact {
            Some(hero_id) => Some(hero_id),
            None => connection
                .query_row(
                    "SELECT id FROM heroes
                     WHERE aliases IS NOT NULL AND instr(lower(aliases),lower(?1))>0
                     ORDER BY id LIMIT 1",
                    [name],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(dota_source_error)?,
        };
        hero_id
            .map(|hero_id| load_dota_hero(&connection, hero_id))
            .transpose()
    }

    fn ability_by_name(&self, name: &str) -> Result<Option<DotaAbility>, DotaInfoError> {
        let connection = self.connection().map_err(dota_error)?;
        connection
            .query_row(
                "SELECT a.id,a.localized_name,a.behavior,a.damage_type,a.description,
                        a.ability_special,a.scepter_upgrades,a.scepter_description,
                        a.shard_upgrades,a.shard_description,a.lore
                 FROM abilities a
                 WHERE a.localized_name=?1 COLLATE NOCASE
                   AND NOT EXISTS (SELECT 1 FROM talents t WHERE t.ability_id=a.id)
                 ORDER BY a.id LIMIT 1",
                [name],
                dota_ability_row,
            )
            .optional()
            .map_err(dota_source_error)
    }
}

fn load_dota_hero(connection: &Connection, hero_id: i64) -> Result<DotaHero, DotaInfoError> {
    let mut hero = connection
        .query_row(
            "SELECT id,name,localized_name,aliases,roles,hype,attr_primary,
                    attr_strength_base,attr_agility_base,attr_intelligence_base,
                    attr_strength_gain,attr_agility_gain,attr_intelligence_gain,
                    attack_damage_min,attack_damage_max,attack_range,is_melee,attack_rate,
                    base_movement,base_armor,magic_resistance
             FROM heroes WHERE id=?1",
            [hero_id],
            |row| {
                Ok(DotaHero {
                    id: row.get(0)?,
                    name: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    localized_name: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    aliases: row.get(3)?,
                    roles: row.get(4)?,
                    hype: row.get(5)?,
                    primary_attribute: row.get(6)?,
                    strength_base: row.get(7)?,
                    agility_base: row.get(8)?,
                    intelligence_base: row.get(9)?,
                    strength_gain: row.get(10)?,
                    agility_gain: row.get(11)?,
                    intelligence_gain: row.get(12)?,
                    attack_damage_min: row.get(13)?,
                    attack_damage_max: row.get(14)?,
                    attack_range: row.get(15)?,
                    is_melee: row.get::<_, Option<bool>>(16)?.unwrap_or(false),
                    attack_rate: row.get(17)?,
                    base_movement: row.get(18)?,
                    base_armor: row.get(19)?,
                    magic_resistance: row.get(20)?,
                    abilities: Vec::new(),
                    talents: Vec::new(),
                    facets: Vec::new(),
                })
            },
        )
        .map_err(dota_source_error)?;
    hero.abilities = load_names(
        connection,
        "SELECT a.localized_name FROM abilities a
         WHERE a.hero_id=?1 AND a.localized_name IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM talents t WHERE t.ability_id=a.id)
         ORDER BY a.slot,a.id",
        hero_id,
    )?;
    hero.talents = load_names(
        connection,
        "SELECT a.localized_name FROM talents t JOIN abilities a ON a.id=t.ability_id
         WHERE t.hero_id=?1 AND a.localized_name IS NOT NULL ORDER BY t.slot,a.id",
        hero_id,
    )?;
    let mut statement = connection
        .prepare(
            "SELECT localized_name,description FROM facets
             WHERE hero_id=?1 AND localized_name IS NOT NULL ORDER BY slot,id",
        )
        .map_err(dota_source_error)?;
    hero.facets = statement
        .query_map([hero_id], |row| {
            Ok(DotaFacet {
                localized_name: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                description: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            })
        })
        .map_err(dota_source_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(dota_source_error)?;
    Ok(hero)
}

fn load_names(
    connection: &Connection,
    query: &str,
    hero_id: i64,
) -> Result<Vec<String>, DotaInfoError> {
    let mut statement = connection.prepare(query).map_err(dota_source_error)?;
    statement
        .query_map([hero_id], |row| row.get(0))
        .map_err(dota_source_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(dota_source_error)
}

fn dota_ability_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DotaAbility> {
    Ok(DotaAbility {
        id: row.get(0)?,
        localized_name: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        behavior: row.get(2)?,
        damage_type: row.get(3)?,
        description: row.get(4)?,
        ability_special: row.get(5)?,
        scepter_upgrades: row.get::<_, Option<bool>>(6)?.unwrap_or(false),
        scepter_description: row.get(7)?,
        shard_upgrades: row.get::<_, Option<bool>>(8)?.unwrap_or(false),
        shard_description: row.get(9)?,
        lore: row.get(10)?,
    })
}

fn dota_error(error: TriviaDataError) -> DotaInfoError {
    DotaInfoError::Catalog(error.to_string())
}

fn dota_source_error(error: rusqlite::Error) -> DotaInfoError {
    DotaInfoError::Catalog(error.to_string())
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>, TriviaDataError> {
    rows.collect::<Result<Vec<_>, _>>().map_err(source_error)
}

fn source_error(error: rusqlite::Error) -> TriviaDataError {
    TriviaDataError::Source(error.to_string())
}

fn parse_json_object(text: Option<&str>) -> serde_json::Map<String, Value> {
    text.and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn optional_integer(value: Option<String>) -> Option<i64> {
    value.and_then(|value| value.parse().ok())
}

fn json_bool(value: &Value) -> Option<bool> {
    value.as_bool().or_else(|| {
        value
            .as_i64()
            .map(|value| value != 0)
            .or_else(|| value.as_str().map(|value| value != "0"))
    })
}

fn json_integer(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn parse_special_values(text: Option<&str>) -> Vec<ItemSpecialValue> {
    let Some(Value::Array(values)) = text.and_then(|text| serde_json::from_str(text).ok()) else {
        return Vec::new();
    };
    values
        .into_iter()
        .filter_map(|value| {
            let object = value.as_object()?;
            Some(ItemSpecialValue {
                key: json_text(object.get("key")),
                header: json_text(object.get("header")),
                value: json_text(object.get("value")),
            })
        })
        .collect()
}

fn json_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Null) | None => String::new(),
        Some(value) => value.to_string(),
    }
}

#[cfg(test)]
#[path = "dotabase_sqlite/tests.rs"]
mod tests;
