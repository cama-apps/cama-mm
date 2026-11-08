//! In-memory [`AIQueryRepository`] used by the SQL-query service tests.

use std::sync::RwLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

#[derive(Clone, Debug)]
struct InMemoryTable {
    schema: TableSchema,
    rows: Vec<QueryRow>,
}

/// Side-effect-free [`AIQueryRepository`] test double: a tiny `SELECT`
/// evaluator over rows inserted by the test.
#[derive(Debug, Default)]
pub struct InMemoryAIQueryRepository {
    tables: RwLock<BTreeMap<String, InMemoryTable>>,
    metadata_reads: AtomicUsize,
}

impl InMemoryAIQueryRepository {
    #[must_use]
    pub fn new(tables: impl IntoIterator<Item = TableSchema>) -> Self {
        let tables = tables
            .into_iter()
            .map(|schema| {
                (
                    schema.name.to_ascii_lowercase(),
                    InMemoryTable {
                        schema,
                        rows: Vec::new(),
                    },
                )
            })
            .collect();
        Self {
            tables: RwLock::new(tables),
            metadata_reads: AtomicUsize::new(0),
        }
    }

    pub fn insert_row(&self, table: &str, row: QueryRow) -> Result<(), QueryRepositoryError> {
        let mut tables = self
            .tables
            .write()
            .map_err(|_| QueryRepositoryError::Storage("table lock poisoned".to_owned()))?;
        let table = tables
            .get_mut(&table.to_ascii_lowercase())
            .ok_or_else(|| QueryRepositoryError::UnknownTable(table.to_owned()))?;
        table.rows.push(row);
        Ok(())
    }

    #[must_use]
    pub fn metadata_read_count(&self) -> usize {
        self.metadata_reads.load(Ordering::SeqCst)
    }

    fn execute(
        &self,
        sql: &str,
        params: &[Value],
        max_rows: usize,
        guild_id: Option<Option<i64>>,
    ) -> Result<Vec<QueryRow>, QueryRepositoryError> {
        let parsed = ParsedSelect::parse(sql, params)?;
        let tables = self
            .tables
            .read()
            .map_err(|_| QueryRepositoryError::Storage("table lock poisoned".to_owned()))?;
        let table = tables
            .get(&parsed.table.to_ascii_lowercase())
            .ok_or_else(|| QueryRepositoryError::UnknownTable(parsed.table.clone()))?;
        let normalized_guild = guild_id.flatten().unwrap_or(0);
        let mut rows = table
            .rows
            .iter()
            .filter(|row| {
                let visible_to_guild = if guild_id.is_some() && table.schema.is_guild_scoped() {
                    row.get("guild_id").and_then(Value::as_i64) == Some(normalized_guild)
                } else {
                    true
                };
                visible_to_guild && parsed.matches(row)
            })
            .cloned()
            .collect::<Vec<_>>();
        if let Some(order_by) = &parsed.order_by {
            rows.sort_by(|left, right| compare_values(left.get(order_by), right.get(order_by)));
        }
        rows.truncate(max_rows);
        rows.into_iter().map(|row| parsed.project(&row)).collect()
    }
}

impl AIQueryRepository for InMemoryAIQueryRepository {
    fn execute_readonly(
        &self,
        sql: &str,
        params: &[Value],
        max_rows: usize,
    ) -> Result<Vec<QueryRow>, QueryRepositoryError> {
        self.execute(sql, params, max_rows, None)
    }

    fn execute_readonly_guild_scoped(
        &self,
        sql: &str,
        guild_id: Option<i64>,
        params: &[Value],
        max_rows: usize,
    ) -> Result<Vec<QueryRow>, QueryRepositoryError> {
        self.execute(sql, params, max_rows, Some(guild_id))
    }

    fn get_all_tables(&self) -> Result<Vec<String>, QueryRepositoryError> {
        let tables = self
            .tables
            .read()
            .map_err(|_| QueryRepositoryError::Storage("table lock poisoned".to_owned()))?;
        Ok(tables
            .values()
            .map(|table| table.schema.name.clone())
            .collect())
    }

    fn get_guild_scoped_tables(&self) -> Result<BTreeSet<String>, QueryRepositoryError> {
        let tables = self
            .tables
            .read()
            .map_err(|_| QueryRepositoryError::Storage("table lock poisoned".to_owned()))?;
        Ok(tables
            .values()
            .filter(|table| table.schema.is_guild_scoped())
            .map(|table| table.schema.name.clone())
            .collect())
    }

    fn get_table_schema(&self, table: &str) -> Result<Vec<ColumnSchema>, QueryRepositoryError> {
        let tables = self
            .tables
            .read()
            .map_err(|_| QueryRepositoryError::Storage("table lock poisoned".to_owned()))?;
        tables
            .get(&table.to_ascii_lowercase())
            .map(|table| table.schema.columns.clone())
            .ok_or_else(|| QueryRepositoryError::UnknownTable(table.to_owned()))
    }

    fn get_foreign_keys(&self, table: &str) -> Result<Vec<ForeignKey>, QueryRepositoryError> {
        let tables = self
            .tables
            .read()
            .map_err(|_| QueryRepositoryError::Storage("table lock poisoned".to_owned()))?;
        tables
            .get(&table.to_ascii_lowercase())
            .map(|table| table.schema.foreign_keys.clone())
            .ok_or_else(|| QueryRepositoryError::UnknownTable(table.to_owned()))
    }

    fn get_schema_metadata(&self) -> Result<SchemaSnapshot, QueryRepositoryError> {
        self.metadata_reads.fetch_add(1, Ordering::SeqCst);
        let tables = self
            .tables
            .read()
            .map_err(|_| QueryRepositoryError::Storage("table lock poisoned".to_owned()))?;
        Ok(SchemaSnapshot {
            tables: tables.values().map(|table| table.schema.clone()).collect(),
        })
    }
}

fn compare_values(left: Option<&Value>, right: Option<&Value>) -> std::cmp::Ordering {
    match (left, right) {
        (Some(Value::Integer(left)), Some(Value::Integer(right))) => left.cmp(right),
        (Some(Value::Text(left)), Some(Value::Text(right))) => left.cmp(right),
        (None, None) => std::cmp::Ordering::Equal,
        (None, _) => std::cmp::Ordering::Less,
        (_, None) => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ComparisonOperator {
    Equal,
    GreaterOrEqual,
}

#[derive(Clone, Debug, PartialEq)]
struct Predicate {
    column: String,
    operator: ComparisonOperator,
    value: Value,
}

#[derive(Clone, Debug, PartialEq)]
struct ParsedSelect {
    columns: Vec<String>,
    table: String,
    predicate: Option<Predicate>,
    order_by: Option<String>,
}

impl ParsedSelect {
    fn parse(sql: &str, params: &[Value]) -> Result<Self, QueryRepositoryError> {
        let normalized = normalize_sql(sql);
        let tokens = lex_sql(&normalized);
        if tokens.first().and_then(SqlToken::word) != Some("select") {
            return Err(QueryRepositoryError::ReadOnlyViolation);
        }
        if tokens
            .iter()
            .filter_map(SqlToken::word)
            .any(|word| DANGEROUS_KEYWORDS.contains(&word))
        {
            return Err(QueryRepositoryError::ReadOnlyViolation);
        }
        let from = tokens
            .iter()
            .position(|token| token.word() == Some("from"))
            .ok_or_else(|| QueryRepositoryError::InvalidQuery("missing FROM".to_owned()))?;
        let table = tokens
            .get(from + 1)
            .and_then(SqlToken::identifier)
            .ok_or_else(|| QueryRepositoryError::InvalidQuery("missing table".to_owned()))?
            .to_owned();
        let columns = tokens[1..from]
            .split(|token| token.symbol() == Some(','))
            .filter_map(|part| {
                part.iter()
                    .rev()
                    .find_map(SqlToken::identifier)
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        if columns.is_empty() {
            return Err(QueryRepositoryError::InvalidQuery(
                "missing projection".to_owned(),
            ));
        }
        let mut parameter_index = 0;
        let predicate = tokens
            .iter()
            .position(|token| token.word() == Some("where"))
            .map(|where_index| {
                let column = tokens
                    .get(where_index + 1)
                    .and_then(SqlToken::identifier)
                    .ok_or_else(|| QueryRepositoryError::InvalidQuery("invalid WHERE".to_owned()))?
                    .to_owned();
                let operator = match tokens.get(where_index + 2) {
                    Some(SqlToken::Symbol('=')) => ComparisonOperator::Equal,
                    Some(SqlToken::Operator(operator)) if operator == ">=" => {
                        ComparisonOperator::GreaterOrEqual
                    }
                    _ => {
                        return Err(QueryRepositoryError::InvalidQuery(
                            "unsupported WHERE operator".to_owned(),
                        ));
                    }
                };
                let value = match tokens.get(where_index + 3) {
                    Some(SqlToken::Number(value)) => Value::Integer(*value),
                    Some(SqlToken::String(value)) => Value::Text(value.clone()),
                    Some(SqlToken::Question) => {
                        let value = params.get(parameter_index).cloned().ok_or_else(|| {
                            QueryRepositoryError::InvalidQuery("missing parameter".to_owned())
                        })?;
                        parameter_index += 1;
                        value
                    }
                    _ => {
                        return Err(QueryRepositoryError::InvalidQuery(
                            "unsupported WHERE value".to_owned(),
                        ));
                    }
                };
                Ok(Predicate {
                    column,
                    operator,
                    value,
                })
            })
            .transpose()?;
        let order_by = tokens
            .windows(2)
            .position(|window| window[0].word() == Some("order") && window[1].word() == Some("by"))
            .and_then(|index| tokens.get(index + 2))
            .and_then(SqlToken::identifier)
            .map(str::to_owned);
        Ok(Self {
            columns,
            table,
            predicate,
            order_by,
        })
    }

    fn matches(&self, row: &QueryRow) -> bool {
        let Some(predicate) = &self.predicate else {
            return true;
        };
        let value = row.get(&predicate.column);
        match (&predicate.operator, value, &predicate.value) {
            (ComparisonOperator::Equal, Some(Value::Integer(left)), Value::Integer(right)) => {
                left == right
            }
            (ComparisonOperator::Equal, Some(Value::Text(left)), Value::Text(right)) => {
                left == right
            }
            (
                ComparisonOperator::GreaterOrEqual,
                Some(Value::Integer(left)),
                Value::Integer(right),
            ) => left >= right,
            _ => false,
        }
    }

    fn project(&self, row: &QueryRow) -> Result<QueryRow, QueryRepositoryError> {
        if self.columns.iter().any(|column| column == "*") {
            return Ok(row.clone());
        }
        self.columns
            .iter()
            .map(|column| {
                row.get(column)
                    .cloned()
                    .map(|value| (column.clone(), value))
                    .ok_or_else(|| QueryRepositoryError::UnknownColumn(column.clone()))
            })
            .collect()
    }
}
