//! Semantic Wheel outcome history over the existing Python-owned table.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Debug, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(BTreeMap<String, JsonValue>),
}

impl JsonValue {
    #[must_use]
    pub fn to_compact_json(&self) -> String {
        let mut output = String::new();
        self.write_json(&mut output);
        output
    }

    fn write_json(&self, output: &mut String) {
        match self {
            Self::Null => output.push_str("null"),
            Self::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
            Self::Integer(value) => write!(output, "{value}").expect("write to String"),
            Self::Float(value) if value.is_nan() => output.push_str("NaN"),
            Self::Float(value) if *value == f64::INFINITY => output.push_str("Infinity"),
            Self::Float(value) if *value == f64::NEG_INFINITY => output.push_str("-Infinity"),
            Self::Float(value) => write!(output, "{value:?}").expect("write to String"),
            Self::String(value) => write_json_string(output, value),
            Self::Array(values) => {
                output.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    value.write_json(output);
                }
                output.push(']');
            }
            Self::Object(values) => {
                output.push('{');
                for (index, (key, value)) in values.iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    write_json_string(output, key);
                    output.push(':');
                    value.write_json(output);
                }
                output.push('}');
            }
        }
    }
}

fn write_json_string(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character <= '\u{1f}' => {
                write!(output, "\\u{:04x}", u32::from(character)).expect("write to String");
            }
            character if character <= '\u{7e}' => output.push(character),
            character if u32::from(character) <= 0xffff => {
                write!(output, "\\u{:04x}", u32::from(character)).expect("write to String");
            }
            character => {
                let scalar = u32::from(character) - 0x1_0000;
                let high_surrogate = 0xd800 + (scalar >> 10);
                let low_surrogate = 0xdc00 + (scalar & 0x3ff);
                write!(output, "\\u{high_surrogate:04x}\\u{low_surrogate:04x}")
                    .expect("write to String");
            }
        }
    }
    output.push('"');
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewWheelSpin {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub result: i64,
    pub spin_time: i64,
    pub is_bankrupt: bool,
    pub is_golden: bool,
    pub outcome_code: Option<String>,
    pub is_bonus: bool,
    pub event_id: Option<String>,
    pub outcome_metadata: Option<JsonValue>,
}

impl NewWheelSpin {
    #[must_use]
    pub fn legacy(discord_id: i64, guild_id: Option<i64>, result: i64, spin_time: i64) -> Self {
        Self {
            discord_id,
            guild_id,
            result,
            spin_time,
            is_bankrupt: false,
            is_golden: false,
            outcome_code: None,
            is_bonus: false,
            event_id: None,
            outcome_metadata: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WheelSpinRecord {
    pub spin_id: i64,
    pub guild_id: i64,
    pub discord_id: i64,
    pub result: i64,
    pub spin_time: i64,
    pub is_bankrupt: bool,
    pub is_golden: bool,
    pub outcome_code: Option<String>,
    pub is_bonus: bool,
    pub event_id: Option<String>,
    pub outcome_metadata: Option<String>,
}

#[derive(Debug, Error)]
pub enum WheelSpinRepositoryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

pub trait WheelSpinPort {
    type Error;

    fn log_wheel_spin(&self, spin: NewWheelSpin) -> Result<i64, Self::Error>;
}

#[derive(Clone, Debug)]
pub struct WheelSpinRepository {
    path: PathBuf,
}

impl WheelSpinRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn log_wheel_spin(&self, spin: &NewWheelSpin) -> Result<i64, WheelSpinRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let metadata = spin
            .outcome_metadata
            .as_ref()
            .map(JsonValue::to_compact_json);
        connection.execute(
            "INSERT INTO wheel_spins (
                 guild_id, discord_id, result, spin_time, is_bankrupt,
                 is_golden, outcome_code, is_bonus, event_id, outcome_metadata
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                spin.guild_id.unwrap_or(0),
                spin.discord_id,
                spin.result,
                spin.spin_time,
                i64::from(spin.is_bankrupt),
                i64::from(spin.is_golden),
                spin.outcome_code,
                i64::from(spin.is_bonus),
                spin.event_id,
                metadata,
            ],
        )?;
        Ok(connection.last_insert_rowid())
    }

    pub fn get_wheel_spin(
        &self,
        spin_id: i64,
    ) -> Result<Option<WheelSpinRecord>, WheelSpinRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT spin_id, guild_id, discord_id, result, spin_time,
                    COALESCE(is_bankrupt, 0), COALESCE(is_golden, 0),
                    outcome_code, COALESCE(is_bonus, 0), event_id, outcome_metadata
               FROM wheel_spins
              WHERE spin_id = ?1",
        )?;
        let mut rows = statement.query([spin_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(WheelSpinRecord {
            spin_id: row.get(0)?,
            guild_id: row.get(1)?,
            discord_id: row.get(2)?,
            result: row.get(3)?,
            spin_time: row.get(4)?,
            is_bankrupt: row.get::<_, i64>(5)? != 0,
            is_golden: row.get::<_, i64>(6)? != 0,
            outcome_code: row.get(7)?,
            is_bonus: row.get::<_, i64>(8)? != 0,
            event_id: row.get(9)?,
            outcome_metadata: row.get(10)?,
        }))
    }

    /// Return the most recent ordinary, non-bonus spin for Chain Reaction.
    pub fn get_last_normal_wheel_spin(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Option<WheelSpinRecord>, WheelSpinRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT spin_id, guild_id, discord_id, result, spin_time,
                    COALESCE(is_bankrupt, 0), COALESCE(is_golden, 0),
                    outcome_code, COALESCE(is_bonus, 0), event_id, outcome_metadata
               FROM wheel_spins
              WHERE guild_id = ?1 AND COALESCE(is_bankrupt, 0) = 0
                AND COALESCE(is_golden, 0) = 0 AND COALESCE(is_bonus, 0) = 0
              ORDER BY spin_time DESC, spin_id DESC LIMIT 1",
        )?;
        statement
            .query_row([guild_id.unwrap_or(0)], |row| {
                Ok(WheelSpinRecord {
                    spin_id: row.get(0)?,
                    guild_id: row.get(1)?,
                    discord_id: row.get(2)?,
                    result: row.get(3)?,
                    spin_time: row.get(4)?,
                    is_bankrupt: row.get::<_, i64>(5)? != 0,
                    is_golden: row.get::<_, i64>(6)? != 0,
                    outcome_code: row.get(7)?,
                    is_bonus: row.get::<_, i64>(8)? != 0,
                    event_id: row.get(9)?,
                    outcome_metadata: row.get(10)?,
                })
            })
            .optional()
            .map_err(Into::into)
    }
}

impl WheelSpinPort for WheelSpinRepository {
    type Error = WheelSpinRepositoryError;

    fn log_wheel_spin(&self, spin: NewWheelSpin) -> Result<i64, Self::Error> {
        WheelSpinRepository::log_wheel_spin(self, &spin)
    }
}

#[derive(Clone, Debug)]
pub struct WheelSpinService<P> {
    port: P,
}

impl<P> WheelSpinService<P> {
    pub fn new(port: P) -> Self {
        Self { port }
    }
}

impl<P: WheelSpinPort> WheelSpinService<P> {
    pub fn log_wheel_spin(&self, spin: NewWheelSpin) -> Result<i64, P::Error> {
        self.port.log_wheel_spin(spin)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    use super::*;

    fn fixture() -> NamedTempFile {
        let file = NamedTempFile::new().unwrap();
        let connection = Connection::open(file.path()).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE wheel_spins (
                     spin_id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     discord_id INTEGER NOT NULL,
                     result INTEGER NOT NULL,
                     spin_time INTEGER NOT NULL,
                     created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     is_bankrupt INTEGER DEFAULT 0,
                     is_golden INTEGER DEFAULT 0,
                     outcome_code TEXT,
                     is_bonus INTEGER NOT NULL DEFAULT 0,
                     event_id TEXT,
                     outcome_metadata TEXT
                 );",
            )
            .unwrap();
        file
    }

    #[test]
    fn test_log_wheel_spin_old_call_remains_compatible() {
        let file = fixture();
        let repository = WheelSpinRepository::new(file.path());
        let spin_id = repository
            .log_wheel_spin(&NewWheelSpin::legacy(
                9_101,
                Some(987_654_321),
                25,
                1_700_000_000,
            ))
            .unwrap();
        let row = repository
            .get_wheel_spin(spin_id)
            .unwrap()
            .expect("wheel spin");

        assert_eq!(row.result, 25);
        assert_eq!(row.outcome_code, None);
        assert!(!row.is_bonus);
        assert_eq!(row.event_id, None);
        assert_eq!(row.outcome_metadata, None);
    }

    #[test]
    fn test_log_wheel_spin_stores_deterministic_metadata() {
        let file = fixture();
        let repository = WheelSpinRepository::new(file.path());
        let metadata = JsonValue::Object(BTreeMap::from([
            ("z".to_owned(), JsonValue::Integer(2)),
            (
                "a".to_owned(),
                JsonValue::Object(BTreeMap::from([("b".to_owned(), JsonValue::Bool(true))])),
            ),
        ]));
        let spin_id = repository
            .log_wheel_spin(&NewWheelSpin {
                discord_id: 9_102,
                guild_id: Some(987_654_321),
                result: 0,
                spin_time: 1_700_000_001,
                is_bankrupt: false,
                is_golden: false,
                outcome_code: Some("LIGHTNING_BOLT".to_owned()),
                is_bonus: true,
                event_id: Some("discord-interaction-42".to_owned()),
                outcome_metadata: Some(metadata),
            })
            .unwrap();
        let row = repository
            .get_wheel_spin(spin_id)
            .unwrap()
            .expect("wheel spin");

        assert_eq!(row.outcome_code.as_deref(), Some("LIGHTNING_BOLT"));
        assert!(row.is_bonus);
        assert_eq!(row.event_id.as_deref(), Some("discord-interaction-42"));
        assert_eq!(
            row.outcome_metadata.as_deref(),
            Some(r#"{"a":{"b":true},"z":2}"#)
        );
    }

    #[derive(Default)]
    struct RecordingPort {
        calls: Mutex<Vec<NewWheelSpin>>,
    }

    impl WheelSpinPort for RecordingPort {
        type Error = std::convert::Infallible;

        fn log_wheel_spin(&self, spin: NewWheelSpin) -> Result<i64, Self::Error> {
            self.calls.lock().unwrap().push(spin);
            Ok(17)
        }
    }

    #[test]
    fn test_player_service_forwards_exact_wheel_fields() {
        let port = RecordingPort::default();
        let metadata = JsonValue::Object(BTreeMap::from([
            ("lightning_count".to_owned(), JsonValue::Integer(2)),
            ("lightning_total".to_owned(), JsonValue::Integer(40)),
        ]));
        let expected = NewWheelSpin {
            discord_id: 9_103,
            guild_id: Some(987_654_321),
            result: 0,
            spin_time: 1_700_000_002,
            is_bankrupt: true,
            is_golden: false,
            outcome_code: Some("LIGHTNING_BOLT".to_owned()),
            is_bonus: true,
            event_id: Some("event-17".to_owned()),
            outcome_metadata: Some(metadata),
        };
        let service = WheelSpinService::new(port);

        assert_eq!(service.log_wheel_spin(expected.clone()).unwrap(), 17);
        assert_eq!(*service.port.calls.lock().unwrap(), [expected]);
    }

    #[test]
    fn stronger_json_serializer_matches_python_compact_sorted_shape() {
        let value = JsonValue::Object(BTreeMap::from([
            (
                "array".to_owned(),
                JsonValue::Array(vec![
                    JsonValue::Null,
                    JsonValue::Float(1.5),
                    JsonValue::String("line\n\"quoted\" é😀\u{7f}".to_owned()),
                ]),
            ),
            ("flag".to_owned(), JsonValue::Bool(false)),
        ]));
        assert_eq!(
            value.to_compact_json(),
            r#"{"array":[null,1.5,"line\n\"quoted\" \u00e9\ud83d\ude00\u007f"],"flag":false}"#
        );
    }
}
