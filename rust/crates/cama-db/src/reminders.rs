//! Existing-schema reads needed to reconstruct free-dig reminder deadlines.
//!
//! This adapter never creates or migrates schema. Database admission must
//! complete before it is used by the runtime.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, params_from_iter};
use thiserror::Error;

use crate::open_runtime_connection;

const SQLITE_BIND_CHUNK: usize = 900;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigReminderAnchor {
    pub last_dig_at: i64,
    pub mutations_json: Option<String>,
    pub injury_state_json: Option<String>,
    pub stat_stamina: i64,
    pub temp_curse_json: Option<String>,
}

#[derive(Debug, Error)]
pub enum ReminderRepositoryError {
    #[error("reminder SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct ReminderRepository {
    path: PathBuf,
}

impl ReminderRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    pub fn dig_anchors_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, Option<DigReminderAnchor>>, ReminderRepositoryError> {
        let unique_ids: Vec<_> = discord_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut anchors: BTreeMap<_, _> = unique_ids
            .iter()
            .copied()
            .map(|discord_id| (discord_id, None))
            .collect();
        if unique_ids.is_empty() {
            return Ok(anchors);
        }

        let connection = self.connection()?;
        let guild_id = guild_id.unwrap_or_default();
        for chunk in unique_ids.chunks(SQLITE_BIND_CHUNK) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT discord_id,last_dig_at,mutations,injury_state,
                        COALESCE(stat_stamina,0),temp_curses
                   FROM tunnels
                  WHERE guild_id=? AND discord_id IN ({placeholders})"
            );
            let mut statement = connection.prepare(&sql)?;
            let parameters = std::iter::once(guild_id).chain(chunk.iter().copied());
            let rows = statement.query_map(params_from_iter(parameters), |row| {
                let last_dig_at = row.get::<_, Option<i64>>(1)?;
                let anchor = if let Some(last_dig_at) = last_dig_at {
                    Some(DigReminderAnchor {
                        last_dig_at,
                        mutations_json: row.get(2)?,
                        injury_state_json: row.get(3)?,
                        stat_stamina: row.get(4)?,
                        temp_curse_json: row.get(5)?,
                    })
                } else {
                    None
                };
                Ok((row.get::<_, i64>(0)?, anchor))
            })?;
            for row in rows {
                let (discord_id, anchor) = row?;
                anchors.insert(discord_id, anchor);
            }
        }
        Ok(anchors)
    }
}
