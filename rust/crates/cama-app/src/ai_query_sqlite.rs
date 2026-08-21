//! Existing-schema SQLite adapter for the natural-language `/ask` service.
//!
//! Generated SQL is validated by [`crate::ai_services::SQLQueryService`]. This
//! adapter supplies the second safety boundary used by the Python runtime: the
//! main database is opened read-only, `query_only` is enabled, result counts
//! are bounded, and every guild-scoped table is shadowed by a filtered TEMP
//! view before a generated query runs.
//!
//! The adapter is synchronous by design; runtime callers must invoke it on
//! Tokio's blocking pool so SQLite work never runs on the async executor.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Duration;

use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, OpenFlags, params_from_iter};

use crate::ai_services::{
    AIQueryRepository, ColumnSchema, ForeignKey, QueryRepositoryError, QueryRow, SchemaSnapshot,
    TableSchema, Value,
};
/// SQLite virtual-machine steps between progress callbacks.
const GENERATED_QUERY_PROGRESS_INTERVAL: std::ffi::c_int = 10_000;
/// Callbacks a generated query may take before it is aborted, so roughly a
/// hundred million virtual-machine steps -- far beyond any legitimate `/ask`
/// over this schema, and short of hanging a blocking thread indefinitely.
const GENERATED_QUERY_PROGRESS_BUDGET: u64 = 10_000;

/// Abort a generated query that runs away.
///
/// The SQL executed on these connections is model-generated, so a cross join
/// with an ORDER BY can run effectively forever and pin the blocking thread it
/// runs on. Bounding virtual-machine steps rather than wall-clock time keeps
/// the limit independent of machine speed.
fn bound_generated_query_cost(connection: &Connection) -> Result<(), QueryRepositoryError> {
    let mut callbacks = 0_u64;
    connection
        .progress_handler(
            GENERATED_QUERY_PROGRESS_INTERVAL,
            Some(move || {
                callbacks = callbacks.saturating_add(1);
                callbacks > GENERATED_QUERY_PROGRESS_BUDGET
            }),
        )
        .map_err(storage)
}

#[derive(Debug)]
pub struct SqliteAIQueryRepository {
    path: PathBuf,
    guild_scoped_tables: RwLock<Option<BTreeSet<String>>>,
}

impl SqliteAIQueryRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            guild_scoped_tables: RwLock::new(None),
        }
    }

    fn connection(&self) -> Result<Connection, QueryRepositoryError> {
        let connection = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI,
        )
        .map_err(storage)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(storage)?;
        Ok(connection)
    }

    fn readonly_connection(&self) -> Result<Connection, QueryRepositoryError> {
        let connection = self.connection()?;
        connection
            .pragma_update(None, "query_only", true)
            .map_err(storage)?;
        bound_generated_query_cost(&connection)?;
        Ok(connection)
    }

    fn scoped_connection(&self, guild_id: i64) -> Result<Connection, QueryRepositoryError> {
        let tables = self.get_guild_scoped_tables()?;
        let connection = self.connection()?;
        for table in tables {
            let quoted = quote_identifier(&table);
            connection
                .execute_batch(&format!(
                    "CREATE TEMP VIEW {quoted} AS SELECT * FROM main.{quoted} WHERE guild_id = {guild_id};"
                ))
                .map_err(storage)?;
        }
        connection
            .pragma_update(None, "query_only", true)
            .map_err(storage)?;
        bound_generated_query_cost(&connection)?;
        Ok(connection)
    }

    fn execute_on(
        &self,
        connection: &Connection,
        sql: &str,
        params: &[Value],
        max_rows: usize,
    ) -> Result<Vec<QueryRow>, QueryRepositoryError> {
        if !sql.trim_start().to_ascii_lowercase().starts_with("select") {
            return Err(QueryRepositoryError::ReadOnlyViolation);
        }
        let sql_params = params
            .iter()
            .map(sql_value)
            .collect::<Result<Vec<_>, _>>()?;
        let mut statement = connection.prepare(sql).map_err(storage)?;
        let names = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut rows = statement
            .query(params_from_iter(sql_params.iter()))
            .map_err(storage)?;
        let mut output = Vec::new();
        while output.len() < max_rows {
            let Some(row) = rows.next().map_err(storage)? else {
                break;
            };
            let mut projected = BTreeMap::new();
            for (index, name) in names.iter().enumerate() {
                projected.insert(
                    name.clone(),
                    app_value(row.get_ref(index).map_err(storage)?),
                );
            }
            output.push(projected);
        }
        Ok(output)
    }

    fn table_names(&self, connection: &Connection) -> Result<Vec<String>, QueryRepositoryError> {
        let mut statement = connection
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                 ORDER BY rowid",
            )
            .map_err(storage)?;
        statement
            .query_map([], |row| row.get(0))
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)
    }

    fn table_schema_on(
        &self,
        connection: &Connection,
        table: &str,
    ) -> Result<Vec<ColumnSchema>, QueryRepositoryError> {
        validate_identifier(table)?;
        let mut statement = connection
            .prepare(&format!("PRAGMA table_info({})", quote_identifier(table)))
            .map_err(storage)?;
        statement
            .query_map([], |row| {
                Ok(ColumnSchema::new(
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)? != 0,
                    row.get::<_, i64>(5)? != 0,
                ))
            })
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)
    }

    fn foreign_keys_on(
        &self,
        connection: &Connection,
        table: &str,
    ) -> Result<Vec<ForeignKey>, QueryRepositoryError> {
        validate_identifier(table)?;
        let mut statement = connection
            .prepare(&format!(
                "PRAGMA foreign_key_list({})",
                quote_identifier(table)
            ))
            .map_err(storage)?;
        statement
            .query_map([], |row| {
                Ok(ForeignKey {
                    table: row.get(2)?,
                    from: row.get(3)?,
                    to: row.get(4)?,
                })
            })
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)
    }
}

impl AIQueryRepository for SqliteAIQueryRepository {
    fn execute_readonly(
        &self,
        sql: &str,
        params: &[Value],
        max_rows: usize,
    ) -> Result<Vec<QueryRow>, QueryRepositoryError> {
        let connection = self.readonly_connection()?;
        self.execute_on(&connection, sql, params, max_rows)
    }

    fn execute_readonly_guild_scoped(
        &self,
        sql: &str,
        guild_id: Option<i64>,
        params: &[Value],
        max_rows: usize,
    ) -> Result<Vec<QueryRow>, QueryRepositoryError> {
        let connection = self.scoped_connection(guild_id.unwrap_or_default())?;
        self.execute_on(&connection, sql, params, max_rows)
    }

    fn get_all_tables(&self) -> Result<Vec<String>, QueryRepositoryError> {
        let connection = self.readonly_connection()?;
        self.table_names(&connection)
    }

    fn get_guild_scoped_tables(&self) -> Result<BTreeSet<String>, QueryRepositoryError> {
        if let Some(cached) = self
            .guild_scoped_tables
            .read()
            .map_err(|_| QueryRepositoryError::Storage("schema cache poisoned".to_owned()))?
            .as_ref()
        {
            return Ok(cached.clone());
        }
        let connection = self.readonly_connection()?;
        let mut scoped = BTreeSet::new();
        for table in self.table_names(&connection)? {
            if self
                .table_schema_on(&connection, &table)?
                .iter()
                .any(|column| column.name == "guild_id")
            {
                scoped.insert(table);
            }
        }
        *self
            .guild_scoped_tables
            .write()
            .map_err(|_| QueryRepositoryError::Storage("schema cache poisoned".to_owned()))? =
            Some(scoped.clone());
        Ok(scoped)
    }

    fn get_table_schema(&self, table: &str) -> Result<Vec<ColumnSchema>, QueryRepositoryError> {
        let connection = self.readonly_connection()?;
        self.table_schema_on(&connection, table)
    }

    fn get_foreign_keys(&self, table: &str) -> Result<Vec<ForeignKey>, QueryRepositoryError> {
        let connection = self.readonly_connection()?;
        self.foreign_keys_on(&connection, table)
    }

    fn get_schema_metadata(&self) -> Result<SchemaSnapshot, QueryRepositoryError> {
        let connection = self.readonly_connection()?;
        let tables = self
            .table_names(&connection)?
            .into_iter()
            .map(|name| {
                Ok(TableSchema {
                    columns: self.table_schema_on(&connection, &name)?,
                    foreign_keys: self.foreign_keys_on(&connection, &name)?,
                    name,
                })
            })
            .collect::<Result<Vec<_>, QueryRepositoryError>>()?;
        Ok(SchemaSnapshot { tables })
    }
}

fn validate_identifier(value: &str) -> Result<(), QueryRepositoryError> {
    if value.is_empty()
        || !value.chars().enumerate().all(|(index, character)| {
            character == '_'
                || character.is_ascii_alphanumeric()
                    && (index != 0 || character.is_ascii_alphabetic())
        })
    {
        return Err(QueryRepositoryError::InvalidQuery(format!(
            "invalid SQLite identifier {value:?}"
        )));
    }
    Ok(())
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn sql_value(value: &Value) -> Result<SqlValue, QueryRepositoryError> {
    match value {
        Value::Null => Ok(SqlValue::Null),
        Value::Bool(value) => Ok(SqlValue::Integer(i64::from(*value))),
        Value::Integer(value) => Ok(SqlValue::Integer(*value)),
        Value::Real(value) => Ok(SqlValue::Real(*value)),
        Value::Text(value) => Ok(SqlValue::Text(value.clone())),
        Value::List(_) | Value::Object(_) => Err(QueryRepositoryError::InvalidQuery(
            "structured values cannot be bound as SQLite parameters".to_owned(),
        )),
    }
}

fn app_value(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(value) => Value::Integer(value),
        ValueRef::Real(value) => Value::Real(value),
        ValueRef::Text(value) => Value::Text(String::from_utf8_lossy(value).into_owned()),
        ValueRef::Blob(value) => Value::List(
            value
                .iter()
                .map(|byte| Value::Integer(i64::from(*byte)))
                .collect(),
        ),
    }
}

fn storage(error: rusqlite::Error) -> QueryRepositoryError {
    QueryRepositoryError::Storage(error.to_string())
}

#[cfg(test)]
#[path = "ai_query_sqlite/tests.rs"]
mod tests;
