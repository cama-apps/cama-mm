//! Existing-schema persistence and selection policy for daily Dig weather.
//!
//! Python stores two rows in `dig_weather` lazily on the first lookup for a
//! guild and game day. This repository preserves that contract without owning
//! any DDL: `BEGIN IMMEDIATE` makes the two-row roll atomic and ensures
//! concurrent command/worker lookups observe the same forecast.

use std::path::{Path, PathBuf};

use rusqlite::{Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

pub const DIG_WEATHER_ACTIVE_WINDOW_SECONDS: i64 = 7 * 86_400;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigWeatherDefinition {
    pub id: &'static str,
    pub name: &'static str,
    pub layer: &'static str,
    pub description: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DigLayerDefinition {
    name: &'static str,
    minimum_depth: i64,
    weather: [DigWeatherDefinition; 3],
}

const DIG_LAYERS: [DigLayerDefinition; 8] = [
    DigLayerDefinition {
        name: "Dirt",
        minimum_depth: 0,
        weather: [
            DigWeatherDefinition {
                id: "earthworm_migration",
                name: "Earthworm Migration",
                layer: "Dirt",
                description: "Worms churn the soil. Digging is easy, but they ate all the coins.",
            },
            DigWeatherDefinition {
                id: "mudslide_warning",
                name: "Mudslide Warning",
                layer: "Dirt",
                description: "The ground is slick. Cave-ins happen more, but the mud cushions the fall.",
            },
            DigWeatherDefinition {
                id: "root_overgrowth",
                name: "Root Overgrowth",
                layer: "Dirt",
                description: "Ancient roots crack the earth open, revealing buried things.",
            },
        ],
    },
    DigLayerDefinition {
        name: "Stone",
        minimum_depth: 26,
        weather: [
            DigWeatherDefinition {
                id: "fossil_rush",
                name: "Fossil Rush",
                layer: "Stone",
                description: "The stone is unusually rich with fossils today.",
            },
            DigWeatherDefinition {
                id: "seismic_tremors",
                name: "Seismic Tremors",
                layer: "Stone",
                description: "The ground won't stop shaking. Things keep falling out of the walls.",
            },
            DigWeatherDefinition {
                id: "mineral_vein",
                name: "Mineral Vein",
                layer: "Stone",
                description: "A rich vein of ore runs through the entire layer.",
            },
        ],
    },
    DigLayerDefinition {
        name: "Crystal",
        minimum_depth: 51,
        weather: [
            DigWeatherDefinition {
                id: "crystal_resonance",
                name: "Crystal Resonance",
                layer: "Crystal",
                description: "The crystals hum in harmony. Fortune favors the bold today.",
            },
            DigWeatherDefinition {
                id: "prismatic_surge",
                name: "Prismatic Surge",
                layer: "Crystal",
                description: "Light refracts wildly. Strange things emerge from the rainbows.",
            },
            DigWeatherDefinition {
                id: "shatter_warning",
                name: "Shatter Warning",
                layer: "Crystal",
                description: "The crystals are unstable. Dangerous, but the shards are valuable.",
            },
        ],
    },
    DigLayerDefinition {
        name: "Magma",
        minimum_depth: 76,
        weather: [
            DigWeatherDefinition {
                id: "eruption",
                name: "Eruption",
                layer: "Magma",
                description: "The magma is surging. Everything is more dangerous and more rewarding.",
            },
            DigWeatherDefinition {
                id: "cooling_period",
                name: "Cooling Period",
                layer: "Magma",
                description: "The lava recedes. Safe, but the good stuff cooled over.",
            },
            DigWeatherDefinition {
                id: "lava_bloom",
                name: "Lava Bloom",
                layer: "Magma",
                description: "Rare minerals crystallize in the cooling lava pools.",
            },
        ],
    },
    DigLayerDefinition {
        name: "Abyss",
        minimum_depth: 101,
        weather: [
            DigWeatherDefinition {
                id: "void_tide",
                name: "Void Tide",
                layer: "Abyss",
                description: "The void pushes back harder, but rewards those who push through.",
            },
            DigWeatherDefinition {
                id: "whisper_storm",
                name: "Whisper Storm",
                layer: "Abyss",
                description: "The whispers are deafening. Events cascade into each other.",
            },
            DigWeatherDefinition {
                id: "deep_calm",
                name: "Deep Calm",
                layer: "Abyss",
                description: "An eerie stillness. Safe, but boring.",
            },
        ],
    },
    DigLayerDefinition {
        name: "Fungal Depths",
        minimum_depth: 151,
        weather: [
            DigWeatherDefinition {
                id: "spore_bloom",
                name: "Spore Bloom",
                layer: "Fungal Depths",
                description: "Bioluminescent spores flood the tunnels. You dig faster but the light burns out.",
            },
            DigWeatherDefinition {
                id: "mycelium_pulse",
                name: "Mycelium Pulse",
                layer: "Fungal Depths",
                description: "The fungal network is active. It shares wealth with those who listen.",
            },
            DigWeatherDefinition {
                id: "fungal_frenzy",
                name: "Fungal Frenzy",
                layer: "Fungal Depths",
                description: "Everything is growing. Including the things that shouldn't be.",
            },
        ],
    },
    DigLayerDefinition {
        name: "Frozen Core",
        minimum_depth: 201,
        weather: [
            DigWeatherDefinition {
                id: "time_dilation",
                name: "Time Dilation",
                layer: "Frozen Core",
                description: "Time runs thick here today. Every coin is worth double, but digging is slow.",
            },
            DigWeatherDefinition {
                id: "frozen_stillness",
                name: "Frozen Stillness",
                layer: "Frozen Core",
                description: "Absolute zero. Nothing collapses. Nothing happens. Nothing at all.",
            },
            DigWeatherDefinition {
                id: "temporal_storm",
                name: "Temporal Storm",
                layer: "Frozen Core",
                description: "Time fractures. Chaos, but the fragments are valuable.",
            },
        ],
    },
    DigLayerDefinition {
        name: "The Hollow",
        minimum_depth: 276,
        weather: [
            DigWeatherDefinition {
                id: "hollow_breathes",
                name: "The Hollow Breathes",
                layer: "The Hollow",
                description: "The Hollow inhales. Everything amplifies. Tread carefully.",
            },
            DigWeatherDefinition {
                id: "void_harvest",
                name: "Void Harvest",
                layer: "The Hollow",
                description: "The void gives up its treasures. It will want them back.",
            },
            DigWeatherDefinition {
                id: "deep_silence",
                name: "Deep Silence",
                layer: "The Hollow",
                description: "The Hollow holds its breath. Safer, but it takes the colour from everything.",
            },
        ],
    },
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigWeatherEntry {
    pub guild_id: i64,
    pub game_date: String,
    pub layer_name: String,
    pub weather_id: String,
}

impl DigWeatherEntry {
    #[must_use]
    pub fn definition(&self) -> Option<&'static DigWeatherDefinition> {
        weather_by_id(&self.weather_id)
    }
}

#[must_use]
pub fn weather_by_id(weather_id: &str) -> Option<&'static DigWeatherDefinition> {
    DIG_LAYERS
        .iter()
        .flat_map(|layer| layer.weather.iter())
        .find(|weather| weather.id == weather_id)
}

#[derive(Debug, Error)]
pub enum DigWeatherRepositoryError {
    #[error("Dig weather invariant expected exactly two rows but found {0}")]
    InvalidRowCount(usize),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

trait WeatherPicker {
    fn below(&mut self, exclusive_upper_bound: usize) -> usize;
}

struct RandomWeatherPicker;

impl WeatherPicker for RandomWeatherPicker {
    fn below(&mut self, exclusive_upper_bound: usize) -> usize {
        fastrand::usize(..exclusive_upper_bound)
    }
}

#[derive(Clone, Debug)]
pub struct DigWeatherRepository {
    path: PathBuf,
}

impl DigWeatherRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Return the persisted forecast, atomically creating its two rows when
    /// this is the first lookup for the guild and game day.
    pub fn ensure_for_day(
        &self,
        guild_id: i64,
        game_date: &str,
        now: i64,
    ) -> Result<Vec<DigWeatherEntry>, DigWeatherRepositoryError> {
        let mut picker = RandomWeatherPicker;
        self.ensure_for_day_with_picker(guild_id, game_date, now, &mut picker)
    }

    fn ensure_for_day_with_picker(
        &self,
        guild_id: i64,
        game_date: &str,
        now: i64,
        picker: &mut impl WeatherPicker,
    ) -> Result<Vec<DigWeatherEntry>, DigWeatherRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut entries = load_entries(&transaction, guild_id, game_date)?;

        if entries.len() > 2 {
            return Err(DigWeatherRepositoryError::InvalidRowCount(entries.len()));
        }

        if entries.len() < 2 {
            let populations = active_layer_populations(
                &transaction,
                guild_id,
                now.saturating_sub(DIG_WEATHER_ACTIVE_WINDOW_SECONDS),
            )?;
            if entries.is_empty() {
                let first_layer = choose_first_layer(&populations, picker);
                insert_random_weather(&transaction, guild_id, game_date, first_layer, picker)?;
                entries = load_entries(&transaction, guild_id, game_date)?;
            }

            if entries.len() == 1 {
                let first_layer = entries[0].layer_name.as_str();
                let second_layer = choose_second_layer(&populations, first_layer, picker);
                insert_random_weather(&transaction, guild_id, game_date, second_layer, picker)?;
                entries = load_entries(&transaction, guild_id, game_date)?;
            }
        }

        if entries.len() != 2 {
            return Err(DigWeatherRepositoryError::InvalidRowCount(entries.len()));
        }
        transaction.commit()?;
        Ok(entries)
    }
}

fn load_entries(
    transaction: &Transaction<'_>,
    guild_id: i64,
    game_date: &str,
) -> Result<Vec<DigWeatherEntry>, rusqlite::Error> {
    let mut statement = transaction.prepare(
        "SELECT guild_id, game_date, layer_name, weather_id
         FROM dig_weather
         WHERE guild_id=?1 AND game_date=?2
         ORDER BY rowid",
    )?;
    statement
        .query_map(params![guild_id, game_date], |row| {
            Ok(DigWeatherEntry {
                guild_id: row.get(0)?,
                game_date: row.get(1)?,
                layer_name: row.get(2)?,
                weather_id: row.get(3)?,
            })
        })?
        .collect()
}

fn active_layer_populations(
    transaction: &Transaction<'_>,
    guild_id: i64,
    active_cutoff: i64,
) -> Result<[usize; DIG_LAYERS.len()], rusqlite::Error> {
    let mut populations = [0; DIG_LAYERS.len()];
    let mut statement = transaction.prepare(
        "SELECT depth FROM tunnels
         WHERE guild_id=?1 AND COALESCE(last_dig_at, 0) >= ?2",
    )?;
    let depths =
        statement.query_map(params![guild_id, active_cutoff], |row| row.get::<_, i64>(0))?;
    for depth in depths {
        populations[layer_index_for_depth(depth?)] += 1;
    }
    Ok(populations)
}

fn layer_index_for_depth(depth: i64) -> usize {
    DIG_LAYERS
        .iter()
        .rposition(|layer| depth >= layer.minimum_depth)
        .unwrap_or(0)
}

fn choose_first_layer(
    populations: &[usize; DIG_LAYERS.len()],
    picker: &mut impl WeatherPicker,
) -> usize {
    let total_population = populations.iter().sum::<usize>();
    if total_population == 0 {
        return picker.below(DIG_LAYERS.len());
    }

    let ticket = picker.below(total_population);
    let mut cumulative = 0;
    for (index, population) in populations.iter().copied().enumerate() {
        cumulative += population;
        if ticket < cumulative {
            return index;
        }
    }
    DIG_LAYERS.len() - 1
}

fn choose_second_layer(
    populations: &[usize; DIG_LAYERS.len()],
    first_layer_name: &str,
    picker: &mut impl WeatherPicker,
) -> usize {
    let populated = DIG_LAYERS
        .iter()
        .enumerate()
        .filter(|(index, layer)| populations[*index] > 0 && layer.name != first_layer_name)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if !populated.is_empty() {
        return populated[picker.below(populated.len())];
    }

    let remaining = DIG_LAYERS
        .iter()
        .enumerate()
        .filter(|(_, layer)| layer.name != first_layer_name)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    remaining[picker.below(remaining.len())]
}

fn insert_random_weather(
    transaction: &Transaction<'_>,
    guild_id: i64,
    game_date: &str,
    layer_index: usize,
    picker: &mut impl WeatherPicker,
) -> Result<(), rusqlite::Error> {
    let layer = &DIG_LAYERS[layer_index];
    let weather = layer.weather[picker.below(layer.weather.len())];
    transaction.execute(
        "INSERT OR IGNORE INTO dig_weather(guild_id, game_date, layer_name, weather_id)
         VALUES (?1, ?2, ?3, ?4)",
        params![guild_id, game_date, layer.name, weather.id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Barrier};

    use crate::schema_manager::{MigrationSettings, initialize_or_migrate_with_settings};
    use rusqlite::{Connection, params};
    use tempfile::TempDir;

    use super::*;

    const GUILD: i64 = 91;
    const NOW: i64 = 2_000_000_000;
    const TODAY: &str = "2033-05-18";

    struct Fixture {
        _directory: TempDir,
        path: PathBuf,
    }

    impl Fixture {
        fn migrated() -> Self {
            let directory = tempfile::tempdir().expect("temporary database directory");
            let path = directory.path().join("cama.db");
            initialize_or_migrate_with_settings(&path, &MigrationSettings::default())
                .expect("migrate temporary database");
            Self {
                _directory: directory,
                path,
            }
        }

        fn tunnel(&self, player: i64, depth: i64, last_dig_at: Option<i64>) {
            Connection::open(&self.path)
                .expect("open fixture")
                .execute(
                    "INSERT INTO tunnels(discord_id, guild_id, depth, last_dig_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![player, GUILD, depth, last_dig_at],
                )
                .expect("insert tunnel");
        }
    }

    struct SequencePicker(VecDeque<usize>);

    impl SequencePicker {
        fn new(values: impl IntoIterator<Item = usize>) -> Self {
            Self(values.into_iter().collect())
        }
    }

    impl WeatherPicker for SequencePicker {
        fn below(&mut self, exclusive_upper_bound: usize) -> usize {
            let value = self.0.pop_front().expect("deterministic picker exhausted");
            assert!(value < exclusive_upper_bound);
            value
        }
    }

    #[test]
    fn first_layer_is_population_weighted_and_second_is_distinct_populated_layer() {
        let fixture = Fixture::migrated();
        fixture.tunnel(1, 0, Some(NOW));
        fixture.tunnel(2, 80, Some(NOW));
        fixture.tunnel(3, 80, Some(NOW));
        fixture.tunnel(4, 80, Some(NOW));
        fixture.tunnel(5, 280, Some(NOW - DIG_WEATHER_ACTIVE_WINDOW_SECONDS - 1));
        let repository = DigWeatherRepository::new(&fixture.path);
        // Weighted ticket 1 falls inside Magma's three-player bucket. Weather
        // index 2 is Lava Bloom; the only remaining populated layer is Dirt.
        let mut picker = SequencePicker::new([1, 2, 0, 1]);

        let entries = repository
            .ensure_for_day_with_picker(GUILD, TODAY, NOW, &mut picker)
            .expect("roll weighted forecast");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].layer_name, "Magma");
        assert_eq!(entries[0].weather_id, "lava_bloom");
        assert_eq!(entries[1].layer_name, "Dirt");
        assert_eq!(entries[1].weather_id, "mudslide_warning");
    }

    #[test]
    fn no_active_tunnels_still_produces_two_distinct_catalog_layers() {
        let fixture = Fixture::migrated();
        fixture.tunnel(1, 280, None);
        let repository = DigWeatherRepository::new(&fixture.path);
        let mut picker = SequencePicker::new([7, 0, 0, 2]);

        let entries = repository
            .ensure_for_day_with_picker(GUILD, TODAY, NOW, &mut picker)
            .expect("roll unpopulated forecast");

        assert_eq!(entries[0].layer_name, "The Hollow");
        assert_eq!(entries[0].weather_id, "hollow_breathes");
        assert_eq!(entries[1].layer_name, "Dirt");
        assert_eq!(entries[1].weather_id, "root_overgrowth");
    }

    #[test]
    fn persisted_forecast_is_idempotent_without_consuming_more_randomness() {
        let fixture = Fixture::migrated();
        let repository = DigWeatherRepository::new(&fixture.path);
        let mut first_picker = SequencePicker::new([0, 0, 0, 0]);
        let first = repository
            .ensure_for_day_with_picker(GUILD, TODAY, NOW, &mut first_picker)
            .expect("first forecast");
        let mut unused_picker = SequencePicker::new([]);
        let second = repository
            .ensure_for_day_with_picker(GUILD, TODAY, NOW, &mut unused_picker)
            .expect("persisted forecast");

        assert_eq!(second, first);
    }

    #[test]
    fn concurrent_first_lookups_commit_one_atomic_two_row_forecast() {
        let fixture = Fixture::migrated();
        let path = Arc::new(fixture.path.clone());
        let barrier = Arc::new(Barrier::new(3));
        let handles = (0..2)
            .map(|_| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let repository = DigWeatherRepository::new(path.as_ref());
                    barrier.wait();
                    repository.ensure_for_day(GUILD, TODAY, NOW)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("weather thread did not panic"))
            .collect::<Result<Vec<_>, _>>()
            .expect("concurrent weather lookups");

        assert_eq!(results[0], results[1]);
        assert_eq!(results[0].len(), 2);
        let row_count = Connection::open(path.as_ref())
            .expect("open fixture")
            .query_row(
                "SELECT COUNT(*) FROM dig_weather WHERE guild_id=?1 AND game_date=?2",
                params![GUILD, TODAY],
                |row| row.get::<_, i64>(0),
            )
            .expect("count weather rows");
        assert_eq!(row_count, 2);
    }

    #[test]
    fn catalog_pins_all_python_weather_copy() {
        assert_eq!(
            DIG_LAYERS
                .iter()
                .flat_map(|layer| layer.weather.iter())
                .count(),
            24
        );
        assert_eq!(
            weather_by_id("temporal_storm"),
            Some(&DigWeatherDefinition {
                id: "temporal_storm",
                name: "Temporal Storm",
                layer: "Frozen Core",
                description: "Time fractures. Chaos, but the fragments are valuable.",
            })
        );
    }
}
