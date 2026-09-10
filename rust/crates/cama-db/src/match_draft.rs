//! Optional draft estimates and captain identities for recorded matches.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MatchDraftAnalysis {
    pub match_id: i64,
    pub guild_id: i64,
    pub valve_match_id: i64,
    pub radiant_win_probability_bps: Option<i64>,
    /// 0 = split (inclusive 48–52%), 1 = Radiant, 2 = Dire.
    pub draft_winner: Option<i64>,
    pub prediction_provider: Option<String>,
    pub prediction_recorded_at: Option<i64>,
    pub draft_heroes_json: Option<String>,
    pub radiant_drafter_steam_id: Option<i64>,
    pub dire_drafter_steam_id: Option<i64>,
    pub radiant_drafter_discord_id: Option<i64>,
    pub dire_drafter_discord_id: Option<i64>,
    pub drafter_source: Option<String>,
}

#[derive(Debug, Error)]
pub enum MatchDraftRepositoryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("draft prediction requires a complete, valid prediction bundle")]
    InvalidPrediction,
}

#[derive(Clone, Debug)]
pub struct MatchDraftRepository {
    path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftBackfillCandidate {
    pub match_id: i64,
    pub valve_match_id: i64,
    pub enrichment_json: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DrafterStats {
    pub drafts_won: i64,
    pub drafts_lost: i64,
    pub drafts_split: i64,
    pub drafts_unknown: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftMatchEntry {
    pub match_id: i64,
    pub valve_match_id: Option<i64>,
    pub winning_team: Option<i64>,
    pub analysis: Option<MatchDraftAnalysis>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftHistoryPage {
    pub entries: Vec<DraftMatchEntry>,
    pub total: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrafterLeaderboardEntry {
    pub discord_id: i64,
    pub stats: DrafterStats,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrafterLeaderboardPage {
    pub entries: Vec<DrafterLeaderboardEntry>,
    pub total: usize,
}

impl MatchDraftRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn get(
        &self,
        match_id: i64,
        guild_id: i64,
    ) -> Result<Option<MatchDraftAnalysis>, MatchDraftRepositoryError> {
        Ok(read_current(
            &open_runtime_connection(&self.path)?,
            match_id,
            guild_id,
        )?)
    }

    /// Invalidate a prediction after a confirmed hero correction, without
    /// removing independently obtained captains. The old hero JSON is a compare
    /// guard so an in-flight correction cannot clear a newer saved prediction.
    pub fn clear_prediction(
        &self,
        match_id: i64,
        guild_id: i64,
        valve_match_id: i64,
        expected_heroes_json: &str,
    ) -> Result<bool, MatchDraftRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        Ok(connection.execute(
            "UPDATE match_draft_analysis SET radiant_win_probability_bps=NULL,draft_winner=NULL,
                prediction_provider=NULL,prediction_recorded_at=NULL,draft_heroes_json=NULL
             WHERE match_id=?1 AND guild_id=?2 AND valve_match_id=?3 AND draft_heroes_json=?4
               AND EXISTS(SELECT 1 FROM matches m
                 WHERE m.match_id=?1 AND m.guild_id=?2 AND m.valve_match_id=?3)",
            params![match_id, guild_id, valve_match_id, expected_heroes_json],
        )? > 0)
    }

    /// Page through linked matches missing estimates, or Captain's Mode matches
    /// missing captain identities. Advance the exclusive cursor after each page
    /// so matches with unavailable external data cannot starve older records.
    pub fn backfill_candidates(
        &self,
        guild_id: i64,
        limit: usize,
        before_match_id: Option<i64>,
    ) -> Result<Vec<DraftBackfillCandidate>, MatchDraftRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT m.match_id,m.valve_match_id,m.enrichment_data
             FROM matches m LEFT JOIN match_draft_analysis a
               ON a.match_id=m.match_id AND a.guild_id=m.guild_id
              AND a.valve_match_id=m.valve_match_id
             WHERE m.guild_id=?1 AND m.valve_match_id IS NOT NULL
               AND (?2 IS NULL OR m.match_id<?2)
               AND (a.radiant_win_probability_bps IS NULL
                 OR (m.game_mode=2 AND (a.radiant_drafter_steam_id IS NULL
                   OR a.dire_drafter_steam_id IS NULL
                   OR a.radiant_drafter_discord_id IS NULL
                   OR a.dire_drafter_discord_id IS NULL)))
             ORDER BY m.match_id DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![guild_id, before_match_id, limit.min(1000) as i64],
            |row| {
                Ok(DraftBackfillCandidate {
                    match_id: row.get(0)?,
                    valve_match_id: row.get(1)?,
                    enrichment_json: row.get(2)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Rank recorded draft estimates by distance from an even draft. Only the
    /// currently linked Valve match is eligible, independently of game outcome.
    pub fn most_imbalanced(
        &self,
        guild_id: i64,
        drafter_discord_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<MatchDraftAnalysis>, MatchDraftRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction()?;
        let ids = {
            let mut statement = transaction.prepare(
                "SELECT a.match_id FROM match_draft_analysis a JOIN matches m
                   ON m.match_id=a.match_id AND m.guild_id=a.guild_id
                  AND m.valve_match_id=a.valve_match_id
                 WHERE a.guild_id=?1 AND a.radiant_win_probability_bps IS NOT NULL
                   AND (?2 IS NULL OR a.radiant_drafter_discord_id=?2 OR a.dire_drafter_discord_id=?2)
                 ORDER BY ABS(a.radiant_win_probability_bps-5000) DESC,a.match_id DESC LIMIT ?3",
            )?;
            statement
                .query_map(
                    params![guild_id, drafter_discord_id, limit.clamp(1, 10) as i64],
                    |row| row.get::<_, i64>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut analyses = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(analysis) = read_current(&transaction, id, guild_id)? {
                analyses.push(analysis);
            }
        }
        transaction.commit()?;
        Ok(analyses)
    }

    /// Browse all recorded matches, including those awaiting a Valve link or a
    /// prediction. Explicit imbalance sorting selects current predictions only.
    /// Both the page and its count are read from the same SQLite snapshot.
    pub fn draft_history(
        &self,
        guild_id: i64,
        drafter_discord_id: Option<i64>,
        by_imbalance: bool,
        page: usize,
        page_size: usize,
    ) -> Result<DraftHistoryPage, MatchDraftRepositoryError> {
        const FILTER: &str = "FROM matches m LEFT JOIN match_draft_analysis a
            ON a.match_id=m.match_id AND a.guild_id=m.guild_id
           AND a.valve_match_id=m.valve_match_id
            WHERE m.guild_id=?1
              AND (?2 IS NULL OR a.radiant_drafter_discord_id=?2 OR a.dire_drafter_discord_id=?2)
              AND (?3=0 OR a.radiant_win_probability_bps IS NOT NULL)";
        let page_size = page_size.clamp(1, 10);
        let offset =
            i64::try_from(page.saturating_sub(1).saturating_mul(page_size)).unwrap_or(i64::MAX);
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction()?;
        let total: i64 = transaction.query_row(
            &format!("SELECT COUNT(*) {FILTER}"),
            params![guild_id, drafter_discord_id, by_imbalance],
            |row| row.get(0),
        )?;
        let order = if by_imbalance {
            "ABS(a.radiant_win_probability_bps-5000) DESC,m.match_id DESC"
        } else {
            "m.match_id DESC"
        };
        let mut entries = {
            let mut statement = transaction.prepare(&format!(
                "SELECT m.match_id,m.valve_match_id,
                    CASE WHEN (typeof(m.winning_team)='integer' AND m.winning_team IN (1,2))
                           OR (typeof(m.winning_team)='text' AND trim(m.winning_team) IN ('1','2'))
                         THEN CAST(m.winning_team AS INTEGER) ELSE NULL END
                 {FILTER} ORDER BY {order} LIMIT ?4 OFFSET ?5"
            ))?;
            statement
                .query_map(
                    params![
                        guild_id,
                        drafter_discord_id,
                        by_imbalance,
                        page_size as i64,
                        offset
                    ],
                    |row| {
                        Ok(DraftMatchEntry {
                            match_id: row.get(0)?,
                            valve_match_id: row.get(1)?,
                            winning_team: row.get(2)?,
                            analysis: None,
                        })
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        for entry in &mut entries {
            entry.analysis = read_current(&transaction, entry.match_id, guild_id)?;
        }
        transaction.commit()?;
        Ok(DraftHistoryPage {
            entries,
            total: usize::try_from(total).unwrap_or(usize::MAX),
        })
    }

    /// Count draft favorability from this drafter's side, excluding match results.
    pub fn drafter_stats(
        &self,
        guild_id: i64,
        discord_id: i64,
    ) -> Result<DrafterStats, MatchDraftRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        Ok(connection.query_row(
            "WITH drafts AS (
               SELECT a.draft_winner,CASE WHEN a.radiant_drafter_discord_id=?2 THEN 1 ELSE 2 END AS team
               FROM match_draft_analysis a JOIN matches m
                 ON m.match_id=a.match_id AND m.guild_id=a.guild_id
                AND m.valve_match_id=a.valve_match_id
               WHERE a.guild_id=?1 AND (a.radiant_drafter_discord_id=?2 OR a.dire_drafter_discord_id=?2)
             )
             SELECT COALESCE(SUM(draft_winner=team),0),
                    COALESCE(SUM(draft_winner IN (1,2) AND draft_winner<>team),0),
                    COALESCE(SUM(draft_winner=0),0),
                    COALESCE(SUM(draft_winner IS NULL),0)
             FROM drafts",params![guild_id,discord_id],
            |row| Ok(DrafterStats {drafts_won:row.get(0)?,drafts_lost:row.get(1)?,
                drafts_split:row.get(2)?,drafts_unknown:row.get(3)?}),
        )?)
    }

    /// Rank drafters by draft wins divided by decisive drafts. Splits and
    /// unavailable estimates remain visible counts but do not affect the rate.
    /// Drafters without any decisive draft are not assigned an invented rate.
    pub fn drafter_leaderboard(
        &self,
        guild_id: i64,
        drafter_discord_id: Option<i64>,
        least_successful: bool,
        page: usize,
        page_size: usize,
    ) -> Result<DrafterLeaderboardPage, MatchDraftRepositoryError> {
        const RANKING: &str = "WITH current_drafts AS (
            SELECT a.radiant_drafter_discord_id,a.dire_drafter_discord_id,a.draft_winner
            FROM match_draft_analysis a JOIN matches m
              ON m.match_id=a.match_id AND m.guild_id=a.guild_id
             AND m.valve_match_id=a.valve_match_id
            WHERE a.guild_id=?1
          ), sides AS (
            SELECT radiant_drafter_discord_id AS discord_id,draft_winner,1 AS team
            FROM current_drafts WHERE radiant_drafter_discord_id IS NOT NULL
            UNION ALL
            SELECT dire_drafter_discord_id AS discord_id,draft_winner,2 AS team
            FROM current_drafts WHERE dire_drafter_discord_id IS NOT NULL
              AND dire_drafter_discord_id IS NOT radiant_drafter_discord_id
          ), totals AS (
            SELECT discord_id,COALESCE(SUM(draft_winner=team),0) AS won,
              COALESCE(SUM(draft_winner IN (1,2) AND draft_winner<>team),0) AS lost,
              COALESCE(SUM(draft_winner=0),0) AS split,
              COALESCE(SUM(draft_winner IS NULL),0) AS unknown
            FROM sides WHERE (?2 IS NULL OR discord_id=?2) GROUP BY discord_id
          ), ranked AS (SELECT * FROM totals WHERE won+lost>0)";
        let page_size = page_size.clamp(1, 10);
        let offset =
            i64::try_from(page.saturating_sub(1).saturating_mul(page_size)).unwrap_or(i64::MAX);
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction()?;
        let total: i64 = transaction.query_row(
            &format!("{RANKING} SELECT COUNT(*) FROM ranked"),
            params![guild_id, drafter_discord_id],
            |row| row.get(0),
        )?;
        let direction = if least_successful { "ASC" } else { "DESC" };
        let entries = {
            let mut statement = transaction.prepare(&format!(
                "{RANKING} SELECT discord_id,won,lost,split,unknown FROM ranked
                 ORDER BY (1.0*won/(won+lost)) {direction},won+lost DESC,discord_id ASC
                 LIMIT ?3 OFFSET ?4"
            ))?;
            statement
                .query_map(
                    params![guild_id, drafter_discord_id, page_size as i64, offset],
                    |row| {
                        Ok(DrafterLeaderboardEntry {
                            discord_id: row.get(0)?,
                            stats: DrafterStats {
                                drafts_won: row.get(1)?,
                                drafts_lost: row.get(2)?,
                                drafts_split: row.get(3)?,
                                drafts_unknown: row.get(4)?,
                            },
                        })
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        transaction.commit()?;
        Ok(DrafterLeaderboardPage {
            entries,
            total: usize::try_from(total).unwrap_or(usize::MAX),
        })
    }

    /// Returns false if the match was deleted, belongs to another guild, or was
    /// relinked while an external prediction request was in flight. Missing
    /// optional data preserves previous successful data only for the same link.
    pub fn save(&self, analysis: &MatchDraftAnalysis) -> Result<bool, MatchDraftRepositoryError> {
        validate_prediction(analysis)?;
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let valid: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM matches WHERE match_id=?1 AND guild_id=?2 AND valve_match_id=?3)",
            params![analysis.match_id, analysis.guild_id, analysis.valve_match_id], |row| row.get(0),
        )?;
        if !valid {
            return Ok(false);
        }
        let mut merged = analysis.clone();
        if let Some(old) = read_current(&transaction, analysis.match_id, analysis.guild_id)? {
            if analysis.radiant_win_probability_bps.is_none() {
                merged.radiant_win_probability_bps = old.radiant_win_probability_bps;
                merged.draft_winner = old.draft_winner;
                merged.prediction_provider = old.prediction_provider;
                merged.prediction_recorded_at = old.prediction_recorded_at;
                merged.draft_heroes_json = old.draft_heroes_json;
            }
            merge_drafter(
                &mut merged.radiant_drafter_steam_id,
                &mut merged.radiant_drafter_discord_id,
                old.radiant_drafter_steam_id,
                old.radiant_drafter_discord_id,
            );
            merge_drafter(
                &mut merged.dire_drafter_steam_id,
                &mut merged.dire_drafter_discord_id,
                old.dire_drafter_steam_id,
                old.dire_drafter_discord_id,
            );
            merged.drafter_source = merged.drafter_source.or(old.drafter_source);
        }
        transaction.execute(
            "INSERT INTO match_draft_analysis(
                match_id,guild_id,valve_match_id,radiant_win_probability_bps,draft_winner,
                prediction_provider,prediction_recorded_at,draft_heroes_json,
                radiant_drafter_steam_id,dire_drafter_steam_id,
                radiant_drafter_discord_id,dire_drafter_discord_id,drafter_source
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
             ON CONFLICT(match_id) DO UPDATE SET
                guild_id=excluded.guild_id,valve_match_id=excluded.valve_match_id,
                radiant_win_probability_bps=excluded.radiant_win_probability_bps,
                draft_winner=excluded.draft_winner,prediction_provider=excluded.prediction_provider,
                prediction_recorded_at=excluded.prediction_recorded_at,draft_heroes_json=excluded.draft_heroes_json,
                radiant_drafter_steam_id=excluded.radiant_drafter_steam_id,
                dire_drafter_steam_id=excluded.dire_drafter_steam_id,
                radiant_drafter_discord_id=excluded.radiant_drafter_discord_id,
                dire_drafter_discord_id=excluded.dire_drafter_discord_id,drafter_source=excluded.drafter_source",
            params![merged.match_id,merged.guild_id,merged.valve_match_id,merged.radiant_win_probability_bps,
                merged.draft_winner,merged.prediction_provider,merged.prediction_recorded_at,merged.draft_heroes_json,
                merged.radiant_drafter_steam_id,merged.dire_drafter_steam_id,
                merged.radiant_drafter_discord_id,merged.dire_drafter_discord_id,merged.drafter_source],
        )?;
        transaction.commit()?;
        Ok(true)
    }
}

fn merge_drafter(
    steam: &mut Option<i64>,
    discord: &mut Option<i64>,
    old_steam: Option<i64>,
    old_discord: Option<i64>,
) {
    if steam.is_none() || *steam == old_steam {
        *discord = discord.or(old_discord);
    }
    *steam = steam.or(old_steam);
}

fn validate_prediction(analysis: &MatchDraftAnalysis) -> Result<(), MatchDraftRepositoryError> {
    let present = [
        analysis.radiant_win_probability_bps.is_some(),
        analysis.draft_winner.is_some(),
        analysis.prediction_provider.is_some(),
        analysis.prediction_recorded_at.is_some(),
        analysis.draft_heroes_json.is_some(),
    ];
    if present.iter().all(|value| !value) {
        return Ok(());
    }
    if !present.iter().all(|value| *value) {
        return Err(MatchDraftRepositoryError::InvalidPrediction);
    }
    let probability = analysis.radiant_win_probability_bps.unwrap_or(-1);
    let winner = cama_domain::draft_analysis::draft_winner(probability);
    if winner.is_none()
        || analysis.draft_winner != winner
        || analysis
            .prediction_provider
            .as_ref()
            .is_none_or(|value| value.trim().is_empty())
        || analysis
            .draft_heroes_json
            .as_ref()
            .is_none_or(|value| serde_json::from_str::<serde_json::Value>(value).is_err())
    {
        return Err(MatchDraftRepositoryError::InvalidPrediction);
    }
    Ok(())
}

fn read_current(
    connection: &Connection,
    match_id: i64,
    guild_id: i64,
) -> Result<Option<MatchDraftAnalysis>, rusqlite::Error> {
    connection.query_row(
        "SELECT a.match_id,a.guild_id,a.valve_match_id,a.radiant_win_probability_bps,a.draft_winner,
            a.prediction_provider,a.prediction_recorded_at,a.draft_heroes_json,
            a.radiant_drafter_steam_id,a.dire_drafter_steam_id,
            a.radiant_drafter_discord_id,a.dire_drafter_discord_id,a.drafter_source
         FROM match_draft_analysis a JOIN matches m ON m.match_id=a.match_id
            AND m.guild_id=a.guild_id AND m.valve_match_id=a.valve_match_id
         WHERE a.match_id=?1 AND a.guild_id=?2", params![match_id,guild_id],
        |row| Ok(MatchDraftAnalysis {
            match_id:row.get(0)?,guild_id:row.get(1)?,valve_match_id:row.get(2)?,
            radiant_win_probability_bps:row.get(3)?,draft_winner:row.get(4)?,prediction_provider:row.get(5)?,
            prediction_recorded_at:row.get(6)?,draft_heroes_json:row.get(7)?,radiant_drafter_steam_id:row.get(8)?,
            dire_drafter_steam_id:row.get(9)?,radiant_drafter_discord_id:row.get(10)?,
            dire_drafter_discord_id:row.get(11)?,drafter_source:row.get(12)?,
        }),
    ).optional()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (crate::test_support::FastTestDatabase, MatchDraftRepository) {
        let database = crate::test_support::fast_migrated_database();
        open_runtime_connection(database.path())
            .unwrap()
            .execute(
                "INSERT INTO matches(match_id,guild_id,valve_match_id,team1_players,team2_players)
             VALUES (1,10,100,'[]','[]')",
                [],
            )
            .unwrap();
        let repository = MatchDraftRepository::new(database.path());
        (database, repository)
    }

    fn empty() -> MatchDraftAnalysis {
        MatchDraftAnalysis {
            match_id: 1,
            guild_id: 10,
            valve_match_id: 100,
            ..Default::default()
        }
    }

    fn prediction() -> MatchDraftAnalysis {
        MatchDraftAnalysis {
            radiant_win_probability_bps: Some(6166),
            draft_winner: Some(1),
            prediction_provider: Some("batru".into()),
            prediction_recorded_at: Some(123),
            draft_heroes_json: Some(r#"{"radiant":[1,2,3,4,5],"dire":[6,7,8,9,10]}"#.into()),
            ..empty()
        }
    }

    #[test]
    fn match_draft_guild_and_stale_requests_fail_closed() {
        let (database, repository) = fixture();
        assert!(repository.save(&prediction()).unwrap());
        assert!(repository.get(1, 20).unwrap().is_none());
        for analysis in [
            MatchDraftAnalysis {
                guild_id: 20,
                ..prediction()
            },
            MatchDraftAnalysis {
                match_id: 2,
                ..prediction()
            },
            MatchDraftAnalysis {
                valve_match_id: 200,
                ..prediction()
            },
        ] {
            assert!(!repository.save(&analysis).unwrap());
        }
        open_runtime_connection(database.path())
            .unwrap()
            .execute("UPDATE matches SET valve_match_id=200 WHERE match_id=1", [])
            .unwrap();
        assert!(repository.get(1, 10).unwrap().is_none());
        assert!(!repository.save(&prediction()).unwrap());
        let changed = MatchDraftAnalysis {
            valve_match_id: 200,
            radiant_drafter_steam_id: Some(44),
            ..empty()
        };
        assert!(repository.save(&changed).unwrap());
        assert_eq!(repository.get(1, 10).unwrap(), Some(changed));
    }

    #[test]
    fn match_draft_partial_retries_preserve_prediction_and_drafters() {
        let (_database, repository) = fixture();
        let drafters = MatchDraftAnalysis {
            radiant_drafter_steam_id: Some(44),
            radiant_drafter_discord_id: Some(55),
            drafter_source: Some("stratz".into()),
            ..empty()
        };
        assert!(repository.save(&drafters).unwrap());
        assert!(repository.save(&prediction()).unwrap());
        let expected = repository.get(1, 10).unwrap().unwrap();
        assert_eq!(expected.radiant_drafter_steam_id, Some(44));
        assert_eq!(expected.radiant_win_probability_bps, Some(6166));
        assert!(repository.save(&drafters).unwrap());
        assert!(repository.save(&prediction()).unwrap());
        assert!(repository.save(&empty()).unwrap());
        assert_eq!(repository.get(1, 10).unwrap(), Some(expected));
        let corrected = MatchDraftAnalysis {
            radiant_drafter_steam_id: Some(66),
            ..empty()
        };
        assert!(repository.save(&corrected).unwrap());
        assert_eq!(
            repository
                .get(1, 10)
                .unwrap()
                .unwrap()
                .radiant_drafter_discord_id,
            None
        );
    }

    #[test]
    fn match_draft_rejects_partial_or_invalid_prediction_without_changing_saved_data() {
        let (_database, repository) = fixture();
        assert!(repository.save(&prediction()).unwrap());
        for bad in [
            MatchDraftAnalysis {
                prediction_provider: None,
                ..prediction()
            },
            MatchDraftAnalysis {
                radiant_win_probability_bps: Some(10001),
                ..prediction()
            },
            MatchDraftAnalysis {
                draft_winner: Some(2),
                ..prediction()
            },
        ] {
            assert!(matches!(
                repository.save(&bad),
                Err(MatchDraftRepositoryError::InvalidPrediction)
            ));
        }
        assert_eq!(repository.get(1, 10).unwrap(), Some(prediction()));
        for probability in [4800, 5000, 5200] {
            let split = MatchDraftAnalysis {
                radiant_win_probability_bps: Some(probability),
                draft_winner: Some(0),
                ..prediction()
            };
            assert!(repository.save(&split).unwrap());
            assert_eq!(repository.get(1, 10).unwrap(), Some(split));
        }
    }

    #[test]
    fn match_draft_backfill_scopes_missing_data_and_paginates_past_failures() {
        let (database, repository) = fixture();
        let connection = open_runtime_connection(database.path()).unwrap();
        connection
            .execute(
                "UPDATE matches SET game_mode=2,enrichment_data='{}' WHERE match_id=1",
                [],
            )
            .unwrap();
        for (match_id, guild_id, valve_match_id, game_mode) in [
            (2, 10, Some(102), 2),
            (3, 10, Some(103), 2),
            (4, 10, Some(104), 2),
            (5, 20, Some(105), 2),
            (6, 10, None, 2),
            (7, 10, Some(107), 22),
            (8, 10, Some(108), 2),
            (9, 10, Some(109), 2),
        ] {
            connection.execute(
                "INSERT INTO matches(match_id,guild_id,valve_match_id,game_mode,team1_players,team2_players)
                 VALUES(?1,?2,?3,?4,'[]','[]')",params![match_id,guild_id,valve_match_id,game_mode],
            ).unwrap();
        }
        let captains = MatchDraftAnalysis {
            radiant_drafter_steam_id: Some(11),
            dire_drafter_steam_id: Some(12),
            radiant_drafter_discord_id: Some(21),
            dire_drafter_discord_id: Some(22),
            drafter_source: Some("stratz".into()),
            ..empty()
        };
        // Prediction only, captains only, complete, and non-CM prediction only.
        repository
            .save(&MatchDraftAnalysis {
                match_id: 2,
                valve_match_id: 102,
                ..prediction()
            })
            .unwrap();
        repository
            .save(&MatchDraftAnalysis {
                match_id: 3,
                valve_match_id: 103,
                ..captains.clone()
            })
            .unwrap();
        repository
            .save(&MatchDraftAnalysis {
                match_id: 4,
                valve_match_id: 104,
                ..captains.clone()
            })
            .unwrap();
        repository
            .save(&MatchDraftAnalysis {
                match_id: 4,
                valve_match_id: 104,
                ..prediction()
            })
            .unwrap();
        repository
            .save(&MatchDraftAnalysis {
                match_id: 7,
                valve_match_id: 107,
                ..prediction()
            })
            .unwrap();
        // Complete stored row for an old Valve link must not exclude the new link.
        repository
            .save(&MatchDraftAnalysis {
                match_id: 8,
                valve_match_id: 108,
                ..captains.clone()
            })
            .unwrap();
        repository
            .save(&MatchDraftAnalysis {
                match_id: 8,
                valve_match_id: 108,
                ..prediction()
            })
            .unwrap();
        connection
            .execute("UPDATE matches SET valve_match_id=208 WHERE match_id=8", [])
            .unwrap();
        // Both Steam identities exist, but one Discord mapping is still missing.
        repository
            .save(&MatchDraftAnalysis {
                match_id: 9,
                valve_match_id: 109,
                dire_drafter_discord_id: None,
                ..captains
            })
            .unwrap();
        repository
            .save(&MatchDraftAnalysis {
                match_id: 9,
                valve_match_id: 109,
                ..prediction()
            })
            .unwrap();
        let all = repository.backfill_candidates(10, 100, None).unwrap();
        assert_eq!(
            all.iter().map(|row| row.match_id).collect::<Vec<_>>(),
            [9, 8, 3, 2, 1]
        );
        assert_eq!(all[1].valve_match_id, 208);
        assert_eq!(all[4].enrichment_json.as_deref(), Some("{}"));
        assert_eq!(
            repository.backfill_candidates(10, 2, None).unwrap(),
            all[..2]
        );
        assert_eq!(
            repository.backfill_candidates(10, 100, Some(8)).unwrap(),
            all[2..]
        );
        assert!(
            repository
                .backfill_candidates(10, 0, None)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            repository.backfill_candidates(20, 100, None).unwrap()[0].match_id,
            5
        );
    }

    #[test]
    fn match_draft_stats_and_rankings_use_drafter_side_and_current_guild_links() {
        let (database, repository) = fixture();
        let connection = open_runtime_connection(database.path()).unwrap();
        for (match_id, guild_id) in [(2, 10), (3, 10), (4, 10), (5, 10), (6, 20), (7, 10)] {
            connection.execute(
                "INSERT INTO matches(match_id,guild_id,valve_match_id,team1_players,team2_players)
                 VALUES(?1,?2,?3,'[]','[]')",params![match_id,guild_id,100+match_id],
            ).unwrap();
        }
        // Drafter21 is Radiant: win; Dire: win, loss, split, unknown.
        for (match_id, guild_id, valve_match_id, probability, radiant, dire) in [
            (1, 10, 100, Some(7000), Some(21), Some(22)),
            (2, 10, 102, Some(3000), Some(22), Some(21)),
            (3, 10, 103, Some(8000), Some(22), Some(21)),
            (4, 10, 104, Some(5100), Some(22), Some(21)),
            (5, 10, 105, None, Some(22), Some(21)),
            (6, 20, 106, Some(10000), Some(21), Some(22)),
            (7, 10, 107, Some(0), Some(22), Some(21)),
        ] {
            let mut analysis = if let Some(bps) = probability {
                MatchDraftAnalysis {
                    radiant_win_probability_bps: Some(bps),
                    draft_winner: cama_domain::draft_analysis::draft_winner(bps),
                    ..prediction()
                }
            } else {
                empty()
            };
            analysis.match_id = match_id;
            analysis.guild_id = guild_id;
            analysis.valve_match_id = valve_match_id;
            analysis.radiant_drafter_discord_id = radiant;
            analysis.dire_drafter_discord_id = dire;
            repository.save(&analysis).unwrap();
        }
        connection
            .execute("UPDATE matches SET valve_match_id=207 WHERE match_id=7", [])
            .unwrap();
        assert_eq!(
            repository.drafter_stats(10, 21).unwrap(),
            DrafterStats {
                drafts_won: 2,
                drafts_lost: 1,
                drafts_split: 1,
                drafts_unknown: 1,
            }
        );
        assert_eq!(
            repository.drafter_stats(10, 22).unwrap(),
            DrafterStats {
                drafts_won: 1,
                drafts_lost: 2,
                drafts_split: 1,
                drafts_unknown: 1,
            }
        );
        assert_eq!(
            repository.drafter_stats(20, 21).unwrap(),
            DrafterStats {
                drafts_won: 1,
                ..Default::default()
            }
        );
        assert_eq!(
            repository.drafter_stats(10, 99).unwrap(),
            DrafterStats::default()
        );
        let ranked = repository.most_imbalanced(10, None, 10).unwrap();
        assert_eq!(
            ranked.iter().map(|row| row.match_id).collect::<Vec<_>>(),
            [3, 2, 1, 4]
        );
        assert_eq!(
            repository.most_imbalanced(10, Some(21), 2).unwrap(),
            ranked[..2]
        );
        assert_eq!(
            repository.most_imbalanced(10, None, 0).unwrap(),
            ranked[..1]
        );
        assert!(
            repository
                .most_imbalanced(10, Some(99), 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            repository.most_imbalanced(20, None, 10).unwrap()[0].match_id,
            6
        );
    }

    #[test]
    fn match_draft_history_includes_incomplete_matches_and_preserves_pagination_filters() {
        let (database, repository) = fixture();
        let connection = open_runtime_connection(database.path()).unwrap();
        connection
            .execute("UPDATE matches SET winning_team=2 WHERE match_id=1", [])
            .unwrap();
        for (match_id, guild_id, valve_match_id, winner) in [
            (2, 10, Some(102), Some("1")),
            (3, 10, None, None),
            (4, 10, Some(104), Some("1invalid")),
            (5, 20, Some(105), Some("2")),
        ] {
            connection.execute(
                "INSERT INTO matches(match_id,guild_id,valve_match_id,winning_team,team1_players,team2_players)
                 VALUES(?1,?2,?3,?4,'[]','[]')",params![match_id,guild_id,valve_match_id,winner],
            ).unwrap();
        }
        repository
            .save(&MatchDraftAnalysis {
                radiant_drafter_discord_id: Some(21),
                ..prediction()
            })
            .unwrap();
        repository
            .save(&MatchDraftAnalysis {
                match_id: 2,
                valve_match_id: 102,
                radiant_win_probability_bps: Some(8000),
                dire_drafter_discord_id: Some(21),
                ..prediction()
            })
            .unwrap();
        repository
            .save(&MatchDraftAnalysis {
                match_id: 4,
                valve_match_id: 104,
                radiant_drafter_discord_id: Some(21),
                ..prediction()
            })
            .unwrap();
        connection
            .execute("UPDATE matches SET valve_match_id=204 WHERE match_id=4", [])
            .unwrap();
        let first = repository.draft_history(10, None, false, 1, 2).unwrap();
        assert_eq!(first.total, 4);
        assert_eq!(
            first
                .entries
                .iter()
                .map(|row| row.match_id)
                .collect::<Vec<_>>(),
            [4, 3]
        );
        assert_eq!(first.entries[0].valve_match_id, Some(204));
        assert_eq!(first.entries[1].valve_match_id, None);
        assert!(
            first
                .entries
                .iter()
                .all(|row| row.analysis.is_none() && row.winning_team.is_none())
        );
        let second = repository.draft_history(10, None, false, 2, 2).unwrap();
        assert_eq!(second.total, 4);
        assert_eq!(
            second
                .entries
                .iter()
                .map(|row| row.match_id)
                .collect::<Vec<_>>(),
            [2, 1]
        );
        assert_eq!(second.entries[0].winning_team, Some(1));
        assert_eq!(second.entries[1].winning_team, Some(2));
        assert!(second.entries.iter().all(|row| row.analysis.is_some()));
        let ranked = repository.draft_history(10, None, true, 1, 10).unwrap();
        assert_eq!(ranked.total, 2);
        assert_eq!(ranked.entries, second.entries);
        assert_eq!(
            repository
                .draft_history(10, Some(21), false, 1, 10)
                .unwrap(),
            ranked
        );
        assert_eq!(
            repository
                .draft_history(10, Some(99), false, 1, 10)
                .unwrap()
                .total,
            0
        );
        assert_eq!(
            repository
                .draft_history(20, None, false, 1, 10)
                .unwrap()
                .total,
            1
        );
        let beyond = repository
            .draft_history(10, None, false, usize::MAX, usize::MAX)
            .unwrap();
        assert_eq!(beyond.total, 4);
        assert!(beyond.entries.is_empty());
        assert_eq!(
            repository
                .draft_history(10, None, false, 0, 0)
                .unwrap()
                .entries,
            first.entries[..1]
        );
    }

    #[test]
    fn match_draft_clear_prediction_checks_current_link_and_heroes_preserving_captains() {
        let (database, repository) = fixture();
        let original = MatchDraftAnalysis {
            radiant_drafter_steam_id: Some(11),
            radiant_drafter_discord_id: Some(21),
            ..prediction()
        };
        repository.save(&original).unwrap();
        let json = original.draft_heroes_json.as_deref().unwrap();
        assert!(!repository.clear_prediction(1, 20, 100, json).unwrap());
        assert!(!repository.clear_prediction(1, 10, 200, json).unwrap());
        assert!(!repository.clear_prediction(1, 10, 100, "{}").unwrap());
        assert_eq!(repository.get(1, 10).unwrap(), Some(original.clone()));
        assert!(repository.clear_prediction(1, 10, 100, json).unwrap());
        assert!(!repository.clear_prediction(1, 10, 100, json).unwrap());
        assert_eq!(
            repository.get(1, 10).unwrap(),
            Some(MatchDraftAnalysis {
                radiant_drafter_steam_id: Some(11),
                radiant_drafter_discord_id: Some(21),
                ..empty()
            })
        );
        assert_eq!(
            repository.backfill_candidates(10, 10, None).unwrap().len(),
            1
        );
        repository.save(&original).unwrap();
        open_runtime_connection(database.path())
            .unwrap()
            .execute("UPDATE matches SET valve_match_id=200 WHERE match_id=1", [])
            .unwrap();
        assert!(!repository.clear_prediction(1, 10, 100, json).unwrap());
    }

    #[test]
    fn match_draft_leaderboard_ranks_decisive_rates_across_sides_and_guilds() {
        let (database, repository) = fixture();
        let connection = open_runtime_connection(database.path()).unwrap();
        for (id, guild, rad, dire, bps) in [
            (1, 10, Some(21), Some(22), Some(7000)),
            (2, 10, Some(22), Some(21), Some(3000)),
            (3, 10, Some(22), Some(21), Some(7000)),
            (4, 10, Some(21), Some(23), Some(5000)),
            (5, 10, Some(23), Some(21), None),
            (6, 10, Some(24), Some(25), Some(7000)),
            (7, 20, Some(22), Some(24), Some(7000)),
            (8, 10, Some(26), None, Some(7000)),
            (9, 10, Some(25), None, Some(7000)),
            (10, 10, None, Some(26), Some(3000)),
        ] {
            let valve = if id == 1 { 100 } else { 100 + id };
            if id != 1 {
                connection.execute("INSERT INTO matches(match_id,guild_id,valve_match_id,team1_players,team2_players) VALUES(?1,?2,?3,'[]','[]')",params![id,guild,valve]).unwrap();
            }
            let mut analysis = if let Some(probability) = bps {
                MatchDraftAnalysis {
                    radiant_win_probability_bps: Some(probability),
                    draft_winner: cama_domain::draft_analysis::draft_winner(probability),
                    ..prediction()
                }
            } else {
                empty()
            };
            analysis.match_id = id;
            analysis.guild_id = guild;
            analysis.valve_match_id = valve;
            analysis.radiant_drafter_discord_id = rad;
            analysis.dire_drafter_discord_id = dire;
            repository.save(&analysis).unwrap();
        }
        // A stale link cannot turn25's true0% rate into50%.
        connection
            .execute("UPDATE matches SET valve_match_id=209 WHERE match_id=9", [])
            .unwrap();
        let best = repository
            .drafter_leaderboard(10, None, false, 1, 10)
            .unwrap();
        assert_eq!(best.total, 5);
        assert_eq!(
            best.entries
                .iter()
                .map(|entry| entry.discord_id)
                .collect::<Vec<_>>(),
            [26, 24, 21, 22, 25]
        );
        assert_eq!(
            best.entries[2].stats,
            DrafterStats {
                drafts_won: 2,
                drafts_lost: 1,
                drafts_split: 1,
                drafts_unknown: 1
            }
        );
        assert_eq!(
            best.entries[3].stats,
            DrafterStats {
                drafts_won: 1,
                drafts_lost: 2,
                ..Default::default()
            }
        );
        assert_eq!(
            repository
                .drafter_leaderboard(10, None, true, 1, 10)
                .unwrap()
                .entries
                .iter()
                .map(|entry| entry.discord_id)
                .collect::<Vec<_>>(),
            [25, 22, 21, 26, 24]
        );
        let second = repository
            .drafter_leaderboard(10, None, false, 2, 2)
            .unwrap();
        assert_eq!(second.total, 5);
        assert_eq!(second.entries, best.entries[2..4]);
        assert_eq!(
            repository
                .drafter_leaderboard(10, Some(21), false, 1, 10)
                .unwrap()
                .entries,
            best.entries[2..3]
        );
        assert_eq!(
            repository
                .drafter_leaderboard(10, Some(23), false, 1, 10)
                .unwrap()
                .total,
            0
        );
        assert_eq!(
            repository
                .drafter_leaderboard(20, None, false, 1, 10)
                .unwrap()
                .entries[0]
                .discord_id,
            22
        );
        assert!(
            repository
                .drafter_leaderboard(10, None, false, usize::MAX, usize::MAX)
                .unwrap()
                .entries
                .is_empty()
        );
        assert_eq!(
            repository
                .drafter_leaderboard(10, None, false, 0, 0)
                .unwrap()
                .entries,
            best.entries[..1]
        );
    }
}
