//! Existing-schema Dig action-history queries and flavor projection.
//!
//! No schema creation or migration happens here: startup remains the sole
//! migration authority.

use std::path::PathBuf;

use rusqlite::{Connection, params};
use serde_json::Value;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigActionRow {
    pub id: i64,
    pub guild_id: i64,
    pub actor_id: i64,
    pub target_id: Option<i64>,
    pub action_type: String,
    pub depth_before: i64,
    pub depth_after: i64,
    pub jc_delta: i64,
    pub detail: Option<String>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigActionDetailProjection {
    pub damage: i64,
    pub advance: i64,
    pub target_id: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigFlavorActionProjection {
    pub guild_id: i64,
    pub actor_id: i64,
    pub target_id: Option<i64>,
    pub action_type: String,
    pub depth_before: i64,
    pub depth_after: i64,
    pub jc_delta: i64,
    pub detail: DigActionDetailProjection,
    pub created_at: i64,
}

impl DigActionRow {
    /// Copy the persisted row into the typed fields consumed by the Dig
    /// flavor provider. Malformed or missing JSON remains fail-soft, matching
    /// Python's `detail or {}` behavior.
    #[must_use]
    pub fn project_for_flavor(&self) -> DigFlavorActionProjection {
        let detail = self
            .detail
            .as_deref()
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
            .unwrap_or(Value::Null);
        let object = detail.as_object();
        let integer = |key: &str| {
            object
                .and_then(|object| object.get(key))
                .and_then(Value::as_i64)
                .unwrap_or(0)
        };
        let detail_target_id = object
            .and_then(|object| object.get("target_id"))
            .and_then(Value::as_i64)
            .or(self.target_id);
        DigFlavorActionProjection {
            guild_id: self.guild_id,
            actor_id: self.actor_id,
            target_id: self.target_id,
            action_type: self.action_type.clone(),
            depth_before: self.depth_before,
            depth_after: self.depth_after,
            jc_delta: self.jc_delta,
            detail: DigActionDetailProjection {
                damage: integer("damage"),
                advance: integer("advance"),
                target_id: detail_target_id,
            },
            created_at: self.created_at,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigFlavorActionHistory {
    pub social_actions: Vec<DigFlavorActionProjection>,
    pub recent_actions: Vec<DigFlavorActionProjection>,
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigActionInsert<'a> {
    pub guild_id: i64,
    pub actor_id: i64,
    pub target_id: Option<i64>,
    pub action_type: &'a str,
    pub depth_before: i64,
    pub depth_after: i64,
    pub jc_delta: i64,
    pub detail: Option<&'a str>,
    pub created_at: i64,
}

/// Repository for the player-scoped action-history reads used by Dig flavor,
/// route/sabotage cooldowns, and balance-history reconstruction.
#[derive(Clone, Debug)]
pub struct DigActionHistoryRepository {
    path: PathBuf,
}

impl DigActionHistoryRepository {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn open(&self) -> rusqlite::Result<Connection> {
        crate::open_runtime_connection(&self.path)
    }

    /// Test/fixture helper; production writers remain the existing typed Dig
    /// repositories. This only appends an audit row to an already migrated DB.
    #[cfg(test)]
    pub fn insert_action(&self, action: DigActionInsert<'_>) -> rusqlite::Result<i64> {
        let connection = self.open()?;
        connection.execute(
            "INSERT INTO dig_actions(
                 guild_id, actor_id, target_id, action_type, depth_before,
                 depth_after, jc_delta, detail, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                action.guild_id,
                action.actor_id,
                action.target_id,
                action.action_type,
                action.depth_before,
                action.depth_after,
                action.jc_delta,
                action.detail,
                action.created_at,
            ],
        )?;
        Ok(connection.last_insert_rowid())
    }

    /// Merge actor and target rows, excluding the target branch's self rows
    /// so a self-action is returned exactly once. Ordering and limiting happen
    /// after the merge, not independently per branch.
    pub fn recent_actions(
        &self,
        player_id: i64,
        guild_id: i64,
        limit: usize,
        action_type: Option<&str>,
        since_unix: Option<i64>,
    ) -> rusqlite::Result<Vec<DigActionRow>> {
        let mut query = String::from(
            "SELECT id, guild_id, actor_id, target_id, action_type, depth_before,
                    depth_after, COALESCE(jc_delta, 0), detail, created_at
             FROM (
                SELECT id, guild_id, actor_id, target_id, action_type, depth_before,
                       depth_after, jc_delta, detail, created_at
                FROM dig_actions
                WHERE guild_id = ?1 AND actor_id = ?2",
        );
        if action_type.is_some() {
            query.push_str(" AND action_type = ?3");
        }
        if since_unix.is_some() {
            query.push_str(if action_type.is_some() {
                " AND created_at >= ?4"
            } else {
                " AND created_at >= ?3"
            });
        }
        query.push_str(
            "
                UNION ALL
                SELECT id, guild_id, actor_id, target_id, action_type, depth_before,
                       depth_after, jc_delta, detail, created_at
                FROM dig_actions
                WHERE guild_id = ?5 AND target_id = ?6 AND actor_id <> ?7",
        );
        if action_type.is_some() {
            query.push_str(" AND action_type = ?8");
        }
        if since_unix.is_some() {
            query.push_str(if action_type.is_some() {
                " AND created_at >= ?9"
            } else {
                " AND created_at >= ?8"
            });
        }
        query.push_str(" ) AS player_actions ORDER BY created_at DESC LIMIT ?10");

        let action_type_owned = action_type.map(str::to_owned);
        let connection = self.open()?;
        let mut statement = connection.prepare(&query)?;
        statement
            .query_map(
                params![
                    guild_id,
                    player_id,
                    action_type_owned,
                    since_unix,
                    guild_id,
                    player_id,
                    player_id,
                    action_type,
                    since_unix,
                    i64::try_from(limit).unwrap_or(i64::MAX),
                ],
                map_action_row,
            )?
            .collect()
    }

    /// Return actor-or-target rows oldest-first for balance reconstruction.
    pub fn player_jc_events(
        &self,
        player_id: i64,
        guild_id: Option<i64>,
    ) -> rusqlite::Result<Vec<DigActionRow>> {
        let guild_id = guild_id.unwrap_or(0);
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT id, guild_id, actor_id, target_id, action_type, depth_before,
                    depth_after, COALESCE(jc_delta, 0), detail, created_at
             FROM (
                SELECT id, guild_id, actor_id, target_id, action_type, depth_before,
                       depth_after, jc_delta, detail, created_at
                FROM dig_actions
                WHERE guild_id = ?1 AND actor_id = ?2
                UNION ALL
                SELECT id, guild_id, actor_id, target_id, action_type, depth_before,
                       depth_after, jc_delta, detail, created_at
                FROM dig_actions
                WHERE guild_id = ?1 AND target_id = ?2 AND actor_id <> ?2
             ) AS player_actions
             ORDER BY created_at ASC",
        )?;
        statement
            .query_map(params![guild_id, player_id], map_action_row)?
            .collect()
    }

    /// Python's default is a 48-hour lookback and one global top-20 limit.
    pub fn recent_social_actions(
        &self,
        player_id: i64,
        guild_id: i64,
        now_unix: i64,
    ) -> rusqlite::Result<Vec<DigActionRow>> {
        let since_unix = now_unix.saturating_sub(48 * 3_600);
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT id, guild_id, actor_id, target_id, action_type, depth_before,
                    depth_after, COALESCE(jc_delta, 0), detail, created_at
             FROM (
                SELECT id, guild_id, actor_id, target_id, action_type, depth_before,
                       depth_after, jc_delta, detail, created_at
                FROM dig_actions
                WHERE guild_id = ?1 AND actor_id = ?2
                  AND action_type IN ('sabotage', 'help', 'cheer')
                  AND created_at >= ?3
                UNION ALL
                SELECT id, guild_id, actor_id, target_id, action_type, depth_before,
                       depth_after, jc_delta, detail, created_at
                FROM dig_actions
                WHERE guild_id = ?1 AND target_id = ?2 AND actor_id <> ?2
                  AND action_type IN ('sabotage', 'help', 'cheer')
                  AND created_at >= ?3
             ) AS player_actions
             ORDER BY created_at DESC
             LIMIT 20",
        )?;
        statement
            .query_map(params![guild_id, player_id, since_unix], map_action_row)?
            .collect()
    }

    /// The exact pair of history windows requested by `DigFlavorService`:
    /// 15 general rows and 20 social rows within 48 hours.
    pub fn flavor_history(
        &self,
        player_id: i64,
        guild_id: i64,
        now_unix: i64,
    ) -> rusqlite::Result<DigFlavorActionHistory> {
        let recent_actions = self.recent_actions(player_id, guild_id, 15, None, None)?;
        let social_actions = self.recent_social_actions(player_id, guild_id, now_unix)?;
        Ok(DigFlavorActionHistory {
            recent_actions: recent_actions
                .iter()
                .map(DigActionRow::project_for_flavor)
                .collect(),
            social_actions: social_actions
                .iter()
                .map(DigActionRow::project_for_flavor)
                .collect(),
        })
    }
}

fn map_action_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DigActionRow> {
    Ok(DigActionRow {
        id: row.get(0)?,
        guild_id: row.get(1)?,
        actor_id: row.get(2)?,
        target_id: row.get(3)?,
        action_type: row.get(4)?,
        depth_before: row.get(5)?,
        depth_after: row.get(6)?,
        jc_delta: row.get(7)?,
        detail: row.get(8)?,
        created_at: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{DigActionHistoryRepository, DigActionInsert};
    use crate::test_support::{FastTestDatabase, fast_migrated_database};

    const GUILD: i64 = 42;
    const OTHER_GUILD: i64 = 99;

    fn fixture() -> (FastTestDatabase, DigActionHistoryRepository) {
        let file = fast_migrated_database();
        let repository = DigActionHistoryRepository::new(file.path().to_path_buf());
        (file, repository)
    }

    fn insert(
        repository: &DigActionHistoryRepository,
        actor_id: i64,
        target_id: Option<i64>,
        action_type: &str,
        created_at: i64,
        guild_id: i64,
    ) -> i64 {
        repository
            .insert_action(DigActionInsert {
                guild_id,
                actor_id,
                target_id,
                action_type,
                depth_before: 0,
                depth_after: 0,
                jc_delta: 0,
                detail: None,
                created_at,
            })
            .expect("insert action")
    }

    #[test]
    fn test_player_histories_merge_actor_and_target_without_self_duplicates() {
        let (_file, repository) = fixture();
        let player_id = 101;
        let actor = insert(&repository, player_id, Some(202), "dig", 100, GUILD);
        let target = insert(&repository, 202, Some(player_id), "help", 200, GUILD);
        let self_row = insert(&repository, player_id, Some(player_id), "cheer", 300, GUILD);
        insert(&repository, 303, Some(404), "sabotage", 400, GUILD);
        insert(&repository, player_id, Some(202), "dig", 500, OTHER_GUILD);

        let all = repository
            .recent_actions(player_id, GUILD, 10, None, None)
            .expect("recent actions");
        assert_eq!(
            all.iter().map(|row| row.id).collect::<Vec<_>>(),
            [self_row, target, actor]
        );
        assert_eq!(
            all.iter().map(|row| row.id).collect::<BTreeSet<_>>().len(),
            3
        );

        let recent = repository
            .recent_actions(player_id, GUILD, 2, None, None)
            .expect("limited actions");
        assert_eq!(
            recent.iter().map(|row| row.id).collect::<Vec<_>>(),
            [self_row, target]
        );
        let filtered = repository
            .recent_actions(player_id, GUILD, 10, Some("help"), None)
            .expect("filtered actions");
        assert_eq!(
            filtered.iter().map(|row| row.id).collect::<Vec<_>>(),
            [target]
        );

        let jc_events = repository
            .player_jc_events(player_id, Some(GUILD))
            .expect("JC events");
        assert_eq!(
            jc_events
                .iter()
                .map(|row| row.created_at)
                .collect::<Vec<_>>(),
            [100, 200, 300]
        );
        assert_eq!(
            jc_events
                .iter()
                .filter(|row| row.actor_id == player_id && row.target_id == Some(player_id))
                .count(),
            1
        );
    }

    #[test]
    fn test_recent_social_actions_applies_one_global_order_and_limit() {
        let (_file, repository) = fixture();
        let player_id = 501;
        let now = 2_000_000_i64;
        let kinds = ["sabotage", "help", "cheer"];
        let mut expected = Vec::new();
        for sequence in 0..24_i64 {
            let created_at = now - 100 + sequence;
            let (actor, target) = if sequence % 2 == 0 {
                (player_id, Some(600 + sequence))
            } else {
                (600 + sequence, Some(player_id))
            };
            let id = insert(
                &repository,
                actor,
                target,
                kinds[usize::try_from(sequence % 3).expect("kind index")],
                created_at,
                GUILD,
            );
            expected.push((created_at, id));
        }
        let self_row = insert(&repository, player_id, Some(player_id), "cheer", now, GUILD);
        expected.push((now, self_row));

        insert(&repository, player_id, Some(700), "dig", now + 1, GUILD);
        insert(
            &repository,
            player_id,
            Some(701),
            "help",
            now - 49 * 3_600,
            GUILD,
        );
        insert(&repository, 702, Some(703), "help", now + 2, GUILD);
        insert(
            &repository,
            player_id,
            Some(704),
            "help",
            now + 3,
            OTHER_GUILD,
        );

        expected.sort_unstable_by(|left, right| right.cmp(left));
        let expected_ids = expected
            .into_iter()
            .take(20)
            .map(|(_, id)| id)
            .collect::<Vec<_>>();
        let actions = repository
            .recent_social_actions(player_id, GUILD, now)
            .expect("social actions");
        assert_eq!(
            actions.iter().map(|row| row.id).collect::<Vec<_>>(),
            expected_ids
        );
        assert_eq!(actions.len(), 20);
        assert_eq!(
            actions
                .iter()
                .map(|row| row.id)
                .collect::<BTreeSet<_>>()
                .len(),
            20
        );
    }
}
