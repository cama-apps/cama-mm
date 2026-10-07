//! Durable Deadlock queue, immutable match rosters and independent OpenSkill pools.
use crate::open_runtime_connection;
use cama_domain::deadlock::{DeadlockFormat, DeadlockPlayer, DeadlockTeams, balance_with_seed};
use cama_domain::openskill::{CamaOpenSkillSystem, Player, WinningTeam};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DeadlockError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
}
fn invalid(message: impl Into<String>) -> DeadlockError {
    DeadlockError::Invalid(message.into())
}
fn identity(guild: i64, user: i64) -> Result<(), DeadlockError> {
    if guild <= 0 || user <= 0 {
        Err(invalid("guild and player IDs must be positive"))
    } else {
        Ok(())
    }
}
fn parse_format(value: &str) -> Result<DeadlockFormat, DeadlockError> {
    match value {
        "street_brawl" => Ok(DeadlockFormat::StreetBrawl),
        "standard" => Ok(DeadlockFormat::Standard),
        _ => Err(invalid("unknown Deadlock format")),
    }
}

#[derive(Clone, Debug)]
pub struct DeadlockSeed {
    pub format: DeadlockFormat,
    pub mu: f64,
    pub sigma: f64,
    pub source: String,
    pub source_value: Option<f64>,
    pub provenance: Option<String>,
    pub source_at: Option<i64>,
}
#[derive(Clone, Debug)]
pub struct DeadlockRating {
    pub format: DeadlockFormat,
    pub mu: f64,
    pub sigma: f64,
    pub games: i64,
    pub revision: i64,
    pub seed_source: String,
    pub seed_value: Option<f64>,
    pub seed_provenance: Option<String>,
    pub seed_source_at: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeadlockEnrollment {
    pub discord_id: i64,
    pub steam_id: i64,
}
#[derive(Clone, Debug)]
pub struct DeadlockQueuePlayer {
    pub player: DeadlockPlayer,
    pub joined_at: i64,
    pub ready_until: Option<i64>,
    pub ready_format: Option<DeadlockFormat>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeadlockMatch {
    pub match_id: i64,
    pub guild_id: i64,
    pub format: DeadlockFormat,
    pub status: String,
    pub team1: Vec<DeadlockPlayer>,
    pub team2: Vec<DeadlockPlayer>,
    pub created_at: i64,
    pub winner: Option<i64>,
    pub external_match_id: Option<i64>,
    pub economy_terms_json: String,
    pub publication_channel_id: Option<i64>,
    pub publication_message_id: Option<i64>,
    pub publication_thread_id: Option<i64>,
    pub thread_message_id: Option<i64>,
}
#[derive(Clone, Debug)]
pub struct DeadlockRepository {
    path: PathBuf,
}
impl DeadlockRepository {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_owned(),
        }
    }
    pub fn enrolled(
        &self,
        guild: i64,
        user: i64,
    ) -> Result<Option<DeadlockEnrollment>, DeadlockError> {
        identity(guild, user)?;
        Ok(open_runtime_connection(&self.path)?.query_row("SELECT discord_id,steam_id FROM deadlock_players WHERE guild_id=?1 AND discord_id=?2",params![guild,user],|r|Ok(DeadlockEnrollment{discord_id:r.get(0)?,steam_id:r.get(1)?})).optional()?)
    }
    /// Existing global Steam ownership is authoritative; this never changes Dota ratings.
    pub fn enroll(
        &self,
        guild: i64,
        user: i64,
        steam: i64,
        display_name: &str,
        seeds: &[DeadlockSeed],
        now: i64,
    ) -> Result<(), DeadlockError> {
        identity(guild, user)?;
        if !(1..=i64::from(u32::MAX)).contains(&steam) {
            return Err(invalid("invalid Steam32 account ID"));
        }
        if seeds.len() != 2
            || !seeds
                .iter()
                .any(|s| s.format == DeadlockFormat::StreetBrawl)
            || !seeds.iter().any(|s| s.format == DeadlockFormat::Standard)
        {
            return Err(invalid("both mode seeds are required"));
        }
        for seed in seeds {
            if !seed.mu.is_finite()
                || !seed.sigma.is_finite()
                || seed.sigma <= 0.0
                || seed.source.trim().is_empty()
                || seed.source_value.is_some_and(|v| !v.is_finite())
            {
                return Err(invalid("invalid rating seed"));
            }
        }
        let mut conn = open_runtime_connection(&self.path)?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let owner: Option<i64> = transaction
            .query_row(
                "SELECT discord_id FROM (SELECT discord_id FROM player_steam_ids WHERE steam_id=?1 UNION SELECT discord_id FROM players WHERE steam_id=?1 UNION SELECT discord_id FROM deadlock_players WHERE steam_id=?1) WHERE discord_id<>?2 LIMIT 1",
                params![steam,user],
                |r| r.get(0),
            )
            .optional()?;
        if owner.is_some_and(|id| id != user) {
            return Err(invalid("Steam account belongs to another player"));
        }
        let previous: Option<i64> = transaction
            .query_row(
                "SELECT steam_id FROM deadlock_players WHERE guild_id=?1 AND discord_id=?2",
                params![guild, user],
                |r| r.get(0),
            )
            .optional()?;
        if previous.is_some_and(|id| id != steam) {
            return Err(invalid(
                "Deadlock account changes require an audited migration",
            ));
        }
        transaction.execute("INSERT OR IGNORE INTO players(discord_id,guild_id,discord_username,jopacoin_balance) VALUES(?1,?2,?3,3)",params![user,guild,display_name])?;
        transaction.execute("INSERT OR IGNORE INTO player_steam_ids(discord_id,steam_id,is_primary,added_at) VALUES(?1,?2,CASE WHEN EXISTS(SELECT 1 FROM player_steam_ids WHERE discord_id=?1) THEN 0 ELSE 1 END,?3)",params![user,steam,now])?;
        transaction.execute("INSERT OR IGNORE INTO deadlock_players(guild_id,discord_id,steam_id,created_at) VALUES(?1,?2,?3,?4)",params![guild,user,steam,now])?;
        for seed in seeds {
            transaction.execute("INSERT OR IGNORE INTO deadlock_ratings(guild_id,discord_id,format,mu,sigma,seed_source,seed_value,seeded_at,seed_provenance,seed_source_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![guild,user,seed.format.as_str(),seed.mu,seed.sigma,seed.source,seed.source_value,now,seed.provenance,seed.source_at])?;
        }
        transaction.commit()?;
        Ok(())
    }
    /// Retry an unavailable signup prior without resetting played ratings,
    /// changing an account link, or modifying a committed match's inputs.
    pub fn refresh_unplayed_provisional(
        &self,
        guild: i64,
        user: i64,
        steam: i64,
        seeds: &[DeadlockSeed],
        now: i64,
    ) -> Result<usize, DeadlockError> {
        identity(guild, user)?;
        for seed in seeds {
            if !seed.mu.is_finite()
                || !seed.sigma.is_finite()
                || seed.sigma <= 0.0
                || seed.source.trim().is_empty()
                || seed.source_value.is_some_and(|value| !value.is_finite())
            {
                return Err(invalid("invalid rating seed"));
            }
        }
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let linked: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM deadlock_players WHERE guild_id=?1 AND discord_id=?2 AND steam_id=?3)",
            params![guild, user, steam], |row| row.get(0),
        )?;
        if !linked {
            return Err(invalid("rating retry must use the linked Deadlock account"));
        }
        let committed: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM deadlock_participants p JOIN deadlock_matches m USING(match_id) WHERE p.guild_id=?1 AND p.discord_id=?2 AND m.status IN('economic_setup','gathering','running'))",
            params![guild, user], |row| row.get(0),
        )?;
        if committed {
            return Ok(0);
        }
        let mut updated = 0;
        for seed in seeds {
            if seed.source == "provisional-v1" || seed.source_value.is_none() {
                continue;
            }
            updated += transaction.execute(
                "UPDATE deadlock_ratings SET mu=?4,sigma=?5,seed_source=?6,seed_value=?7,seeded_at=?8,seed_provenance=?9,seed_source_at=?10 WHERE guild_id=?1 AND discord_id=?2 AND format=?3 AND games=0 AND revision=0 AND seed_source='provisional-v1'",
                params![guild,user,seed.format.as_str(),seed.mu,seed.sigma,seed.source,seed.source_value,now,seed.provenance,seed.source_at],
            )?;
        }
        if updated > 0 {
            transaction.execute("INSERT INTO deadlock_audit_events(guild_id,actor_id,kind,detail,created_at) VALUES(?1,?2,'rating_seed_retry',?3,?4)",params![guild,user,format!("updated_formats={updated}"),now])?;
        }
        transaction.commit()?;
        Ok(updated)
    }

    pub fn queue_join(&self, guild: i64, user: i64, now: i64) -> Result<(), DeadlockError> {
        identity(guild, user)?;
        let mut conn = open_runtime_connection(&self.path)?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let enrolled: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM deadlock_players WHERE guild_id=?1 AND discord_id=?2)",
            params![guild, user],
            |r| r.get(0),
        )?;
        if !enrolled {
            return Err(invalid("register for Deadlock first"));
        }
        ensure_available(&transaction, guild, user, now)?;
        transaction.execute(
            "INSERT OR IGNORE INTO deadlock_queue(guild_id,discord_id,joined_at) VALUES(?1,?2,?3)",
            params![guild, user, now],
        )?;
        transaction.commit()?;
        Ok(())
    }
    pub fn queue_leave(&self, guild: i64, user: i64) -> Result<bool, DeadlockError> {
        identity(guild, user)?;
        Ok(open_runtime_connection(&self.path)?.execute(
            "DELETE FROM deadlock_queue WHERE guild_id=?1 AND discord_id=?2",
            params![guild, user],
        )? > 0)
    }
    pub fn ready(
        &self,
        guild: i64,
        user: i64,
        format: DeadlockFormat,
        now: i64,
    ) -> Result<(), DeadlockError> {
        identity(guild, user)?;
        let until = now
            .checked_add(600)
            .ok_or_else(|| invalid("invalid time"))?;
        let changed=open_runtime_connection(&self.path)?.execute("UPDATE deadlock_queue SET ready_format=?3,ready_until=?4 WHERE guild_id=?1 AND discord_id=?2",params![guild,user,format.as_str(),until])?;
        if changed != 1 {
            return Err(invalid("join the Deadlock queue first"));
        }
        Ok(())
    }
    pub fn queue(
        &self,
        guild: i64,
        format: DeadlockFormat,
    ) -> Result<Vec<DeadlockQueuePlayer>, DeadlockError> {
        queue_on(&open_runtime_connection(&self.path)?, guild, format)
    }
    /// Ready checks are format-specific and expire; no partial rosters or automatic mode changes.
    pub fn shuffle(
        &self,
        guild: i64,
        format: DeadlockFormat,
        actor: i64,
        now: i64,
    ) -> Result<DeadlockMatch, DeadlockError> {
        self.shuffle_inner(guild, format, actor, now, None, "{}", None)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn shuffle_with_request(
        &self,
        guild: i64,
        format: DeadlockFormat,
        actor: i64,
        now: i64,
        request_key: &str,
        economy_terms_json: &str,
        host_account: Option<u32>,
    ) -> Result<DeadlockMatch, DeadlockError> {
        if request_key.trim().is_empty() {
            return Err(invalid("shuffle request key is required"));
        }
        self.shuffle_inner(
            guild,
            format,
            actor,
            now,
            Some(request_key),
            economy_terms_json,
            host_account,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn shuffle_inner(
        &self,
        guild: i64,
        format: DeadlockFormat,
        actor: i64,
        now: i64,
        request_key: Option<&str>,
        economy_terms_json: &str,
        host_account: Option<u32>,
    ) -> Result<DeadlockMatch, DeadlockError> {
        identity(guild, actor)?;
        let _: serde_json::Value = serde_json::from_str(economy_terms_json)?;
        let mut conn = open_runtime_connection(&self.path)?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(key) = request_key {
            let previous: Option<i64> = transaction
                .query_row(
                    "SELECT match_id FROM deadlock_matches WHERE guild_id=?1 AND request_key=?2",
                    params![guild, key],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(id) = previous {
                let existing = read_match(&transaction, guild, id)?
                    .ok_or_else(|| invalid("match disappeared"))?;
                let creator: i64 = transaction.query_row(
                    "SELECT created_by FROM deadlock_matches WHERE guild_id=?1 AND match_id=?2",
                    params![guild, id],
                    |row| row.get(0),
                )?;
                if existing.format != format || creator != actor {
                    return Err(invalid(
                        "shuffle request already used by another organizer or format",
                    ));
                }
                return Ok(existing);
            }
        }
        let selected: Vec<_> = queue_on(&transaction, guild, format)?
            .into_iter()
            .filter(|p| p.ready_format == Some(format) && p.ready_until.is_some_and(|t| t > now))
            .take(format.player_count())
            .collect();
        if selected.len() != format.player_count() {
            return Err(invalid(format!(
                "{} ready players required for {}",
                format.player_count(),
                format.label()
            )));
        }
        for entry in &selected {
            ensure_available(&transaction, guild, entry.player.discord_id, now)?;
        }
        let players: Vec<_> = selected.into_iter().map(|p| p.player).collect();
        let seed = request_key.map_or(now as u64, |key| {
            key.bytes().fold(14695981039346656037_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(1099511628211)
            })
        });
        let teams = balance_with_seed(format, &players, seed).map_err(invalid)?;
        transaction.execute("INSERT INTO deadlock_matches(guild_id,format,status,roster_json,created_by,created_at,updated_at,request_key,economy_terms_json) VALUES(?1,?2,'economic_setup',?3,?4,?5,?5,?6,?7)",params![guild,format.as_str(),serde_json::to_string(&teams)?,actor,now,request_key,economy_terms_json])?;
        let id = transaction.last_insert_rowid();
        if let Some(account) = host_account {
            crate::deadlock_host::reserve_on(&transaction, guild, id, account, now)
                .map_err(|e| invalid(e.to_string()))?;
        }
        for (side, team) in [(1, &teams.team1), (2, &teams.team2)] {
            for player in team {
                transaction.execute("INSERT INTO deadlock_participants(match_id,guild_id,discord_id,side,rating_revision) SELECT ?1,?2,?3,?4,revision FROM deadlock_ratings WHERE guild_id=?2 AND discord_id=?3 AND format=?5",params![id,guild,player.discord_id,side,format.as_str()])?;
                transaction.execute(
                    "DELETE FROM deadlock_queue WHERE guild_id=?1 AND discord_id=?2",
                    params![guild, player.discord_id],
                )?;
            }
        }
        audit(
            &transaction,
            guild,
            id,
            actor,
            "shuffle",
            format.as_str(),
            now,
        )?;
        transaction.commit()?;
        Ok(DeadlockMatch {
            match_id: id,
            guild_id: guild,
            format,
            status: "economic_setup".into(),
            team1: teams.team1,
            team2: teams.team2,
            created_at: now,
            winner: None,
            external_match_id: None,
            economy_terms_json: economy_terms_json.to_owned(),
            publication_channel_id: None,
            publication_message_id: None,
            publication_thread_id: None,
            thread_message_id: None,
        })
    }
    pub fn is_organizer(&self, guild: i64, id: i64, user: i64) -> Result<bool, DeadlockError> {
        identity(guild, user)?;
        Ok(open_runtime_connection(&self.path)?.query_row("SELECT EXISTS(SELECT 1 FROM deadlock_matches WHERE guild_id=?1 AND match_id=?2 AND created_by=?3)",params![guild,id,user],|r|r.get(0))?)
    }
    pub fn set_publication(
        &self,
        guild: i64,
        id: i64,
        channel: i64,
        message: i64,
    ) -> Result<(), DeadlockError> {
        identity(guild, channel)?;
        if message <= 0 {
            return Err(invalid("invalid message ID"));
        }
        let changed=open_runtime_connection(&self.path)?.execute("UPDATE deadlock_matches SET publication_channel_id=?3,publication_message_id=?4 WHERE guild_id=?1 AND match_id=?2 AND (publication_message_id IS NULL OR (publication_channel_id=?3 AND publication_message_id=?4))",params![guild,id,channel,message])?;
        if changed != 1 {
            return Err(invalid("Deadlock match not found"));
        }
        Ok(())
    }
    pub fn set_thread(
        &self,
        guild: i64,
        id: i64,
        thread: i64,
        message: i64,
    ) -> Result<(), DeadlockError> {
        identity(guild, thread)?;
        if message <= 0 {
            return Err(invalid("invalid thread message ID"));
        }
        let changed=open_runtime_connection(&self.path)?.execute("UPDATE deadlock_matches SET publication_thread_id=?3,thread_message_id=?4 WHERE guild_id=?1 AND match_id=?2 AND (publication_thread_id IS NULL OR (publication_thread_id=?3 AND thread_message_id=?4))",params![guild,id,thread,message])?;
        if changed != 1 {
            return Err(invalid(
                "match missing or thread publication already assigned",
            ));
        }
        Ok(())
    }
    pub fn match_by_request(
        &self,
        guild: i64,
        request_key: &str,
    ) -> Result<Option<DeadlockMatch>, DeadlockError> {
        let connection = open_runtime_connection(&self.path)?;
        let id: Option<i64> = connection
            .query_row(
                "SELECT match_id FROM deadlock_matches WHERE guild_id=?1 AND request_key=?2",
                params![guild, request_key],
                |row| row.get(0),
            )
            .optional()?;
        id.map(|id| read_match(&connection, guild, id))
            .transpose()
            .map(Option::flatten)
    }
    pub fn match_by_id(&self, guild: i64, id: i64) -> Result<Option<DeadlockMatch>, DeadlockError> {
        read_match(&open_runtime_connection(&self.path)?, guild, id)
    }
    pub fn ratings(&self, guild: i64, user: i64) -> Result<Vec<DeadlockRating>, DeadlockError> {
        identity(guild, user)?;
        let conn = open_runtime_connection(&self.path)?;
        let mut statement=conn.prepare("SELECT format,mu,sigma,games,revision,seed_source,seed_value,seed_provenance,seed_source_at FROM deadlock_ratings WHERE guild_id=?1 AND discord_id=?2 ORDER BY format")?;
        statement
            .query_map(params![guild, user], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<f64>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, Option<i64>>(8)?,
                ))
            })?
            .map(|r| {
                let (
                    format,
                    mu,
                    sigma,
                    games,
                    revision,
                    seed_source,
                    seed_value,
                    seed_provenance,
                    seed_source_at,
                ) = r?;
                Ok(DeadlockRating {
                    format: parse_format(&format)?,
                    mu,
                    sigma,
                    games,
                    revision,
                    seed_source,
                    seed_value,
                    seed_provenance,
                    seed_source_at,
                })
            })
            .collect()
    }
    pub fn recent_matches(
        &self,
        guild: i64,
        limit: usize,
    ) -> Result<Vec<DeadlockMatch>, DeadlockError> {
        let conn = open_runtime_connection(&self.path)?;
        let limit = limit.min(100) as i64;
        let ids=conn.prepare("SELECT match_id FROM deadlock_matches WHERE guild_id=?1 ORDER BY match_id DESC LIMIT ?2")?.query_map(params![guild,limit],|r|r.get::<_,i64>(0))?.collect::<Result<Vec<_>,_>>()?;
        ids.into_iter()
            .map(|id| read_match(&conn, guild, id)?.ok_or_else(|| invalid("match disappeared")))
            .collect()
    }
    /// Recovery never drops an active match or an incomplete publication because
    /// newer games exist. Recent completed matches remain refreshable as well.
    pub fn publication_candidates(&self, guild: i64) -> Result<Vec<DeadlockMatch>, DeadlockError> {
        let connection = open_runtime_connection(&self.path)?;
        let ids = connection
            .prepare(
                "SELECT match_id FROM deadlock_matches WHERE guild_id=?1 AND (
                status NOT IN ('settled','aborted')
                OR publication_channel_id IS NULL OR publication_message_id IS NULL
                OR publication_thread_id IS NULL OR thread_message_id IS NULL
                OR match_id IN (SELECT match_id FROM deadlock_matches
                    WHERE guild_id=?1 AND status IN ('settled','aborted')
                    ORDER BY match_id DESC LIMIT 25)
             ) ORDER BY match_id",
            )?
            .query_map([guild], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                read_match(&connection, guild, id)?.ok_or_else(|| invalid("match disappeared"))
            })
            .collect()
    }
    pub fn active_matches(&self, guild: i64) -> Result<Vec<DeadlockMatch>, DeadlockError> {
        let conn = open_runtime_connection(&self.path)?;
        let ids=conn.prepare("SELECT match_id FROM deadlock_matches WHERE guild_id=?1 AND status NOT IN('settled','aborted') ORDER BY match_id")?.query_map([guild],|r|r.get::<_,i64>(0))?.collect::<Result<Vec<_>,_>>()?;
        ids.into_iter()
            .map(|id| read_match(&conn, guild, id)?.ok_or_else(|| invalid("match disappeared")))
            .collect()
    }
    /// Durable result precedes recoverable economy settlement. Duplicate identical reports are safe.
    pub fn record_result(
        &self,
        guild: i64,
        id: i64,
        winner: i64,
        actor: i64,
        external_match_id: Option<i64>,
        now: i64,
    ) -> Result<DeadlockMatch, DeadlockError> {
        identity(guild, actor)?;
        if ![1, 2].contains(&winner) || external_match_id.is_some_and(|id| id <= 0) {
            return Err(invalid("invalid result"));
        }
        let mut conn = open_runtime_connection(&self.path)?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read_match(&transaction, guild, id)?
            .ok_or_else(|| invalid("Deadlock match not found"))?;
        if let (Some(known), Some(supplied)) = (current.external_match_id, external_match_id)
            && known != supplied
        {
            return Err(invalid("reported match ID conflicts with the hosted match"));
        }
        let external_match_id = external_match_id.or(current.external_match_id);
        if current.status == "recorded" || current.status == "settled" {
            if current.winner == Some(winner) && current.external_match_id == external_match_id {
                return Ok(current);
            }
            return Err(invalid("conflicting result requires an audited correction"));
        }
        if current.status != "running" && current.status != "gathering" {
            return Err(invalid("match is not ready for recording"));
        }
        let market_closed:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM deadlock_betting_markets WHERE guild_id=?1 AND match_id=?2 AND status='closed')",params![guild,id],|r|r.get(0))?;
        if !market_closed {
            return Err(invalid("close the betting market before recording"));
        }
        let to_player =
            |p: &DeadlockPlayer| Player::new(p.discord_id as u64, Some(p.mu), Some(p.sigma));
        let team1: Vec<_> = current.team1.iter().map(to_player).collect();
        let team2: Vec<_> = current.team2.iter().map(to_player).collect();
        let system = CamaOpenSkillSystem::new();
        let updates = system
            .update_ratings_equal_weight(
                &team1,
                &team2,
                if winner == 1 {
                    WinningTeam::Team1
                } else {
                    WinningTeam::Team2
                },
                &BTreeMap::new(),
                &BTreeMap::new(),
            )
            .map_err(|e| invalid(e.to_string()))?;
        for player in current.team1.iter().chain(&current.team2) {
            let next = updates
                .get(&(player.discord_id as u64))
                .ok_or_else(|| invalid("missing rating update"))?;
            let changed=transaction.execute("UPDATE deadlock_ratings SET mu=?4,sigma=?5,games=games+1,revision=revision+1 WHERE guild_id=?1 AND discord_id=?2 AND format=?3 AND revision=(SELECT rating_revision FROM deadlock_participants WHERE match_id=?6 AND discord_id=?2)",params![guild,player.discord_id,current.format.as_str(),next.mu,next.sigma,id])?;
            if changed != 1 {
                return Err(invalid(
                    "rating changed after shuffle; result needs reconciliation",
                ));
            }
            transaction.execute("INSERT INTO deadlock_rating_events(match_id,guild_id,discord_id,format,old_mu,old_sigma,new_mu,new_sigma,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![id,guild,player.discord_id,current.format.as_str(),player.mu,player.sigma,next.mu,next.sigma,now])?;
        }
        transaction.execute("UPDATE deadlock_matches SET status='recorded',winner=?3,external_match_id=?4,recorded_by=?5,updated_at=?6 WHERE guild_id=?1 AND match_id=?2",params![guild,id,winner,external_match_id,actor,now])?;
        audit(
            &transaction,
            guild,
            id,
            actor,
            "record",
            &format!("winner={winner};external={external_match_id:?}"),
            now,
        )?;
        let result =
            read_match(&transaction, guild, id)?.ok_or_else(|| invalid("match disappeared"))?;
        transaction.commit()?;
        Ok(result)
    }
    /// Refund recovery is driven by the durable aborted state. Recorded matches cannot be aborted.
    pub fn abort(
        &self,
        guild: i64,
        id: i64,
        actor: i64,
        reason: &str,
        now: i64,
    ) -> Result<(), DeadlockError> {
        identity(guild, actor)?;
        if reason.trim().is_empty() {
            return Err(invalid("an abort reason is required"));
        }
        let mut conn = open_runtime_connection(&self.path)?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read_match(&transaction, guild, id)?
            .ok_or_else(|| invalid("Deadlock match not found"))?;
        if current.status == "aborted" {
            return Ok(());
        }
        if current.status == "recorded" || current.status == "settled" {
            return Err(invalid("recorded matches require audited correction"));
        }
        // Ready/start is remote and its response may be lost. Admission is
        // recorded in the same write transaction as the host launch intent,
        // so abort and launch cannot both win. The marker survives reconnect
        // and manual_review; an uncertain played game is not a refundable
        // unplayed cancellation.
        let launch_admitted: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM app_kv WHERE guild_id=?1 AND key IN(?3,?4))
                OR EXISTS(SELECT 1 FROM deadlock_host_jobs WHERE guild_id=?1 AND match_id=?2
                    AND (phase IN('launch_requested','running') OR external_match_id IS NOT NULL))",
            params![
                guild,
                id,
                format!("deadlock_launch_admitted:{id}"),
                format!("deadlock_explicit_start:{id}")
            ],
            |row| row.get(0),
        )?;
        if launch_admitted || current.status == "running" || current.external_match_id.is_some() {
            return Err(invalid(
                "launch may have started; verify the whole-match result and record it, or use audited administrative reconciliation instead of refunding",
            ));
        }
        transaction.execute("UPDATE deadlock_matches SET status='aborted',abort_reason=?3,updated_at=?4 WHERE guild_id=?1 AND match_id=?2",params![guild,id,reason,now])?;
        audit(&transaction, guild, id, actor, "abort", reason, now)?;
        transaction.commit()?;
        Ok(())
    }
}
fn audit(
    transaction: &Transaction<'_>,
    guild: i64,
    id: i64,
    actor: i64,
    kind: &str,
    detail: &str,
    now: i64,
) -> Result<(), rusqlite::Error> {
    transaction.execute("INSERT INTO deadlock_audit_events(guild_id,match_id,actor_id,kind,detail,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![guild,id,actor,kind,detail,now])?;
    Ok(())
}
fn queue_on(
    conn: &Connection,
    guild: i64,
    format: DeadlockFormat,
) -> Result<Vec<DeadlockQueuePlayer>, DeadlockError> {
    let mut stmt=conn.prepare("SELECT q.discord_id,p.steam_id,r.mu,r.sigma,q.joined_at,q.ready_format,q.ready_until FROM deadlock_queue q JOIN deadlock_players p USING(guild_id,discord_id) JOIN deadlock_ratings r USING(guild_id,discord_id) WHERE q.guild_id=?1 AND r.format=?2 ORDER BY q.joined_at,q.discord_id")?;
    let rows = stmt.query_map(params![guild, format.as_str()], |r| {
        Ok((
            DeadlockPlayer {
                discord_id: r.get(0)?,
                steam_id: r.get(1)?,
                mu: r.get(2)?,
                sigma: r.get(3)?,
            },
            r.get::<_, i64>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<i64>>(6)?,
        ))
    })?;
    rows.map(|row| {
        let (player, joined_at, ready_format, ready_until) = row?;
        Ok(DeadlockQueuePlayer {
            player,
            joined_at,
            ready_until,
            ready_format: ready_format.as_deref().map(parse_format).transpose()?,
        })
    })
    .collect()
}
fn read_match(
    conn: &Connection,
    guild: i64,
    id: i64,
) -> Result<Option<DeadlockMatch>, DeadlockError> {
    let data=conn.query_row("SELECT format,status,roster_json,created_at,winner,external_match_id,economy_terms_json,publication_channel_id,publication_message_id,publication_thread_id,thread_message_id FROM deadlock_matches WHERE guild_id=?1 AND match_id=?2",params![guild,id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,Option<i64>>(4)?,r.get::<_,Option<i64>>(5)?,r.get::<_,String>(6)?,r.get::<_,Option<i64>>(7)?,r.get::<_,Option<i64>>(8)?,r.get::<_,Option<i64>>(9)?,r.get::<_,Option<i64>>(10)?))).optional()?;
    data.map(
        |(
            format,
            status,
            json,
            created_at,
            winner,
            external_match_id,
            economy_terms_json,
            publication_channel_id,
            publication_message_id,
            publication_thread_id,
            thread_message_id,
        )| {
            let roster: DeadlockTeams = serde_json::from_str(&json)?;
            Ok(DeadlockMatch {
                match_id: id,
                guild_id: guild,
                format: parse_format(&format)?,
                status,
                team1: roster.team1,
                team2: roster.team2,
                created_at,
                winner,
                external_match_id,
                economy_terms_json,
                publication_channel_id,
                publication_message_id,
                publication_thread_id,
                thread_message_id,
            })
        },
    )
    .transpose()
}
fn ensure_available(
    transaction: &Transaction<'_>,
    guild: i64,
    user: i64,
    now: i64,
) -> Result<(), DeadlockError> {
    let busy:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM deadlock_participants p JOIN deadlock_matches m USING(match_id) WHERE p.guild_id=?1 AND p.discord_id=?2 AND m.status IN('economic_setup','gathering','running')) OR EXISTS(SELECT 1 FROM pending_matches m WHERE m.guild_id=?1 AND (?2 IN(SELECT value FROM json_each(m.payload,'$.radiant_team_ids')) OR ?2 IN(SELECT value FROM json_each(m.payload,'$.dire_team_ids')))) OR EXISTS(SELECT 1 FROM app_kv WHERE guild_id=?1 AND key='draft:state' AND (COALESCE(json_extract(value,'$.active'),1)=1 OR COALESCE(json_extract(value,'$.finalizing'),0)=1) AND ?2 IN(SELECT value FROM json_each(app_kv.value,'$.state.player_pool_ids')))",params![guild,user],|r|r.get(0))?;
    if busy {
        return Err(invalid(format!(
            "player {user} is already reserved by an active match"
        )));
    }
    let suspended:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM lobby_suspensions WHERE guild_id=?1 AND discord_id=?2 AND active=1 AND scope='all' AND CASE completion WHEN 'time' THEN expires_at>?3 WHEN 'matches' THEN matches_remaining>0 WHEN 'either' THEN expires_at>?3 AND matches_remaining>0 WHEN 'both' THEN expires_at>?3 OR matches_remaining>0 ELSE 1 END)",params![guild,user,now],|r|r.get(0))?;
    if suspended {
        return Err(invalid("player has an active lobby suspension"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;
    fn fixture() -> (NamedTempFile, DeadlockRepository) {
        let file = NamedTempFile::new().unwrap();
        crate::schema_manager::initialize_or_migrate(file.path()).unwrap();
        let repo = DeadlockRepository::new(file.path());
        (file, repo)
    }
    fn enroll(repo: &DeadlockRepository, user: i64) {
        let seeds =
            [DeadlockFormat::StreetBrawl, DeadlockFormat::Standard].map(|format| DeadlockSeed {
                format,
                mu: 25.0,
                sigma: 8.333,
                source: "provisional-v1".into(),
                source_value: None,
                provenance: None,
                source_at: None,
            });
        repo.enroll(10, user, user, "Player", &seeds, 100).unwrap();
        repo.queue_join(10, user, 100 + user).unwrap();
        repo.ready(10, user, DeadlockFormat::StreetBrawl, 120)
            .unwrap();
    }
    #[test]
    fn neutral_import_retry_updates_once_without_granting_another_wallet_balance() {
        let (file, repo) = fixture();
        enroll(&repo, 1);
        let seeds = imported_seeds();
        assert_eq!(
            repo.refresh_unplayed_provisional(10, 1, 1, &seeds, 130)
                .unwrap(),
            2
        );
        assert_eq!(
            repo.refresh_unplayed_provisional(10, 1, 1, &seeds, 131)
                .unwrap(),
            0
        );
        let ratings = repo.ratings(10, 1).unwrap();
        assert!(ratings.iter().all(|rating| rating.seed_value == Some(84.0)
            && rating.games == 0
            && rating.revision == 0));
        assert!(
            repo.queue(10, DeadlockFormat::StreetBrawl).unwrap()[0]
                .player
                .mu
                > 25.0
        );
        let connection = open_runtime_connection(file.path()).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT jopacoin_balance FROM players WHERE guild_id=10 AND discord_id=1",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            3
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM deadlock_audit_events WHERE kind='rating_seed_retry'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn import_retry_preserves_played_ratings_and_rejects_a_different_account() {
        let (file, repo) = fixture();
        enroll(&repo, 1);
        let connection = open_runtime_connection(file.path()).unwrap();
        connection.execute("UPDATE deadlock_ratings SET games=1,revision=1,mu=27 WHERE guild_id=10 AND discord_id=1 AND format='street_brawl'", []).unwrap();
        assert!(
            repo.refresh_unplayed_provisional(10, 1, 2, &imported_seeds(), 130)
                .is_err()
        );
        assert_eq!(
            repo.refresh_unplayed_provisional(10, 1, 1, &imported_seeds(), 130)
                .unwrap(),
            1
        );
        let brawl = repo
            .ratings(10, 1)
            .unwrap()
            .into_iter()
            .find(|rating| rating.format == DeadlockFormat::StreetBrawl)
            .unwrap();
        assert_eq!((brawl.mu, brawl.games, brawl.revision), (27.0, 1, 1));
        assert!(
            repo.refresh_unplayed_provisional(20, 1, 1, &imported_seeds(), 130)
                .is_err()
        );
    }

    #[test]
    fn import_retry_cannot_change_a_committed_roster_or_accept_another_neutral_prior() {
        let (_file, repo) = fixture();
        for user in 1..=8 {
            enroll(&repo, user);
        }
        let mut missing = imported_seeds();
        for seed in &mut missing {
            seed.source = "provisional-v1".into();
            seed.source_value = None;
        }
        assert_eq!(
            repo.refresh_unplayed_provisional(10, 1, 1, &missing, 130)
                .unwrap(),
            0
        );
        let game = repo
            .shuffle(10, DeadlockFormat::StreetBrawl, 99, 130)
            .unwrap();
        assert_eq!(
            repo.refresh_unplayed_provisional(10, 1, 1, &imported_seeds(), 131)
                .unwrap(),
            0
        );
        assert!(
            repo.ratings(10, 1)
                .unwrap()
                .iter()
                .all(|rating| rating.mu == 25.0)
        );
        assert!(
            repo.match_by_id(10, game.match_id)
                .unwrap()
                .unwrap()
                .team1
                .iter()
                .chain(game.team2.iter())
                .all(|player| player.mu == 25.0)
        );
        repo.abort(10, game.match_id, 99, "cancelled", 132).unwrap();
        assert_eq!(
            repo.refresh_unplayed_provisional(10, 1, 1, &imported_seeds(), 133)
                .unwrap(),
            2
        );
    }

    fn imported_seeds() -> [DeadlockSeed; 2] {
        [DeadlockFormat::StreetBrawl, DeadlockFormat::Standard].map(|format| DeadlockSeed {
            format,
            mu: if format == DeadlockFormat::StreetBrawl {
                26.75
            } else {
                32.0
            },
            sigma: 8.333,
            source: if format == DeadlockFormat::StreetBrawl {
                "standard-weak-brawl-prior-v1"
            } else {
                "valve-rank-weak-prior-v1"
            }
            .into(),
            source_value: Some(84.0),
            provenance: Some("{}".into()),
            source_at: Some(125),
        })
    }

    #[test]
    fn enrollment_queue_and_frozen_fifo_roster() {
        let (file, repo) = fixture();
        for user in 1..=9 {
            enroll(&repo, user);
        }
        assert!(repo.shuffle(10, DeadlockFormat::Standard, 99, 130).is_err());
        assert_eq!(
            repo.queue(10, DeadlockFormat::StreetBrawl).unwrap().len(),
            9
        );
        let m = repo
            .shuffle(10, DeadlockFormat::StreetBrawl, 99, 130)
            .unwrap();
        assert_eq!(m.team1.len(), 4);
        assert_eq!(m.team2.len(), 4);
        assert_eq!(
            repo.queue(10, DeadlockFormat::StreetBrawl).unwrap()[0]
                .player
                .discord_id,
            9
        );
        assert!(repo.queue_join(10, 1, 140).is_err());
        assert!(repo.match_by_id(11, m.match_id).unwrap().is_none());
        let conn = Connection::open(file.path()).unwrap();
        let wallet: (i64, Option<i64>, Option<f64>) = conn
            .query_row(
                "SELECT jopacoin_balance,initial_mmr,os_mu FROM players WHERE discord_id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(wallet, (3, None, None));
        let payload = r#"{"radiant_team_ids":[1],"dire_team_ids":[]}"#;
        assert!(
            conn.execute(
                "INSERT INTO pending_matches(guild_id,payload) VALUES(10,?1)",
                [payload]
            )
            .is_err()
        );
        repo.abort(10, m.match_id, 99, "cancelled", 150).unwrap();
        assert!(repo.queue_join(10, 1, 151).is_ok());
    }
    #[test]
    fn result_is_atomic_idempotent_and_mode_isolated() {
        let (file, repo) = fixture();
        for user in 1..=8 {
            enroll(&repo, user);
        }
        let m = repo
            .shuffle(10, DeadlockFormat::StreetBrawl, 99, 130)
            .unwrap();
        let conn = Connection::open(file.path()).unwrap();
        conn.execute(
            "UPDATE deadlock_matches SET status='gathering' WHERE match_id=?1",
            [m.match_id],
        )
        .unwrap();
        assert!(
            repo.record_result(10, m.match_id, 1, 99, None, 150)
                .is_err()
        );
        conn.execute("INSERT INTO deadlock_betting_markets(market_id,guild_id,match_id,format,roster_json,terms_json,deadline,status,created_at) VALUES('test',10,?1,'street_brawl','{}','{}',140,'closed',130)",[m.match_id]).unwrap();
        repo.record_result(10, m.match_id, 1, 99, Some(123), 160)
            .unwrap();
        repo.record_result(10, m.match_id, 1, 99, Some(123), 160)
            .unwrap();
        assert!(
            repo.record_result(10, m.match_id, 2, 99, Some(123), 160)
                .is_err()
        );
        assert!(repo.abort(10, m.match_id, 99, "late", 160).is_err());
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM deadlock_rating_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            8
        );
        assert_eq!(
            conn.query_row(
                "SELECT SUM(games) FROM deadlock_ratings WHERE format='standard'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    #[test]
    fn expired_ready_check_and_dota_claim_prevent_shuffle() {
        let (file, repo) = fixture();
        for user in 1..=8 {
            enroll(&repo, user);
        }
        assert!(
            repo.shuffle(10, DeadlockFormat::StreetBrawl, 99, 800)
                .is_err()
        );
        Connection::open(file.path()).unwrap().execute("INSERT INTO pending_matches(guild_id,payload) VALUES(10,'{\"radiant_team_ids\":[1],\"dire_team_ids\":[]}')",[]).unwrap();
        assert!(
            repo.shuffle(10, DeadlockFormat::StreetBrawl, 99, 130)
                .is_err()
        );
        assert_eq!(
            repo.queue(10, DeadlockFormat::StreetBrawl).unwrap().len(),
            8
        );
    }
    #[test]
    fn draft_and_deadlock_claims_are_bidirectional_and_released_with_envelope() {
        let (file, repo) = fixture();
        for user in 1..=8 {
            enroll(&repo, user);
        }
        let conn = Connection::open(file.path()).unwrap();
        let draft = r#"{"active":true,"state":{"player_pool_ids":[1]}}"#;
        conn.execute(
            "INSERT INTO app_kv(guild_id,key,value) VALUES(10,'draft:state',?1)",
            [draft],
        )
        .unwrap();
        assert!(
            repo.shuffle(10, DeadlockFormat::StreetBrawl, 99, 130)
                .is_err()
        );
        conn.execute(
            "DELETE FROM app_kv WHERE guild_id=10 AND key='draft:state'",
            [],
        )
        .unwrap();
        let m = repo
            .shuffle(10, DeadlockFormat::StreetBrawl, 99, 130)
            .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO app_kv(guild_id,key,value) VALUES(10,'draft:state',?1)",
                [draft]
            )
            .is_err()
        );
        repo.abort(10, m.match_id, 99, "cancelled", 150).unwrap();
        conn.execute(
            "INSERT INTO app_kv(guild_id,key,value) VALUES(10,'draft:state',?1)",
            [draft],
        )
        .unwrap();
    }
    #[test]
    fn request_retry_preserves_roster_and_first_economic_snapshot() {
        let (_file, repo) = fixture();
        for user in 1..=8 {
            enroll(&repo, user);
        }
        let first = repo
            .shuffle_with_request(
                10,
                DeadlockFormat::StreetBrawl,
                99,
                130,
                "interaction-1",
                r#"{"seed":2}"#,
                None,
            )
            .unwrap();
        let retry = repo
            .shuffle_with_request(
                10,
                DeadlockFormat::StreetBrawl,
                99,
                999,
                "interaction-1",
                r#"{"seed":99}"#,
                None,
            )
            .unwrap();
        assert_eq!(first.match_id, retry.match_id);
        assert_eq!(first.economy_terms_json, retry.economy_terms_json);
        assert!(
            repo.shuffle_with_request(
                10,
                DeadlockFormat::Standard,
                99,
                999,
                "interaction-1",
                "{}",
                None
            )
            .is_err()
        );
    }
    #[test]
    fn existing_database_upgrade_is_retry_safe() {
        let (file, _) = fixture();
        let conn = Connection::open(file.path()).unwrap();
        conn.execute(
            "DELETE FROM schema_migrations WHERE name='create_deadlock_lobbies_and_betting'",
            [],
        )
        .unwrap();
        conn.execute("DROP TABLE deadlock_betting_liquidity", [])
            .unwrap();
        drop(conn);
        let first = crate::schema_manager::initialize_or_migrate(file.path()).unwrap();
        let second = crate::schema_manager::initialize_or_migrate(file.path()).unwrap();
        assert!(
            first
                .created_tables
                .contains(&"deadlock_betting_liquidity".to_owned())
        );
        assert!(second.created_tables.is_empty());
    }
    #[test]
    fn simultaneous_dota_and_deadlock_claims_have_one_winner() {
        let (file, repo) = fixture();
        for user in 1..=8 {
            enroll(&repo, user);
        }
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let path = file.path().to_owned();
        let other = barrier.clone();
        let dota = std::thread::spawn(move || {
            let connection = open_runtime_connection(&path).unwrap();
            other.wait();
            connection.execute("INSERT INTO pending_matches(guild_id,payload) VALUES(10,'{\"radiant_team_ids\":[1],\"dire_team_ids\":[]}')",[]).is_ok()
        });
        barrier.wait();
        let deadlock = repo
            .shuffle(10, DeadlockFormat::StreetBrawl, 99, 130)
            .is_ok();
        assert_ne!(deadlock, dota.join().unwrap());
    }
    #[test]
    fn signup_respects_legacy_steam_owner_and_does_not_reseed() {
        let (file, repo) = fixture();
        enroll(&repo, 1);
        let connection = open_runtime_connection(file.path()).unwrap();
        let seeds =
            [DeadlockFormat::StreetBrawl, DeadlockFormat::Standard].map(|format| DeadlockSeed {
                format,
                mu: 99.0,
                sigma: 1.0,
                source: "later-observation".into(),
                source_value: None,
                provenance: None,
                source_at: None,
            });
        repo.enroll(10, 1, 1, "Renamed", &seeds, 200).unwrap();
        assert_eq!(repo.ratings(10, 1).unwrap()[0].mu, 25.0);
        connection.execute("INSERT INTO players(discord_id,guild_id,discord_username,steam_id) VALUES(55,20,'Legacy',55)",[]).unwrap();
        assert!(repo.enroll(10, 2, 55, "Other", &seeds, 200).is_err());
        assert!(repo.enrolled(10, 2).unwrap().is_none());
    }
    #[test]
    fn recorded_result_preserves_host_match_identity_and_rejects_conflicts() {
        let (file, repo) = fixture();
        for user in 1..=8 {
            enroll(&repo, user);
        }
        let game = repo
            .shuffle(10, DeadlockFormat::StreetBrawl, 99, 130)
            .unwrap();
        let connection = open_runtime_connection(file.path()).unwrap();
        connection.execute("UPDATE deadlock_matches SET status='running',external_match_id=123 WHERE match_id=?1",[game.match_id]).unwrap();
        connection.execute("INSERT INTO deadlock_betting_markets(market_id,guild_id,match_id,format,roster_json,terms_json,deadline,status,created_at) VALUES('test',10,?1,'street_brawl','{}','{}',140,'closed',130)",[game.match_id]).unwrap();
        assert!(
            repo.record_result(10, game.match_id, 1, 99, Some(456), 160)
                .is_err()
        );
        assert_eq!(
            repo.ratings(10, 1)
                .unwrap()
                .iter()
                .map(|r| r.games)
                .sum::<i64>(),
            0
        );
        let recorded = repo
            .record_result(10, game.match_id, 1, 99, None, 160)
            .unwrap();
        assert_eq!(recorded.external_match_id, Some(123));
        assert!(
            repo.record_result(10, game.match_id, 1, 99, None, 170)
                .is_ok()
        );
    }
    #[test]
    fn host_reservation_and_roster_commit_atomically() {
        let (file, repo) = fixture();
        for user in 1..=8 {
            enroll(&repo, user);
        }
        assert!(
            repo.shuffle_with_request(
                10,
                DeadlockFormat::StreetBrawl,
                99,
                130,
                "invalid-host",
                "{}",
                Some(0)
            )
            .is_err()
        );
        assert_eq!(
            repo.queue(10, DeadlockFormat::StreetBrawl).unwrap().len(),
            8
        );
        assert!(repo.recent_matches(10, 10).unwrap().is_empty());
        let game = repo
            .shuffle_with_request(
                10,
                DeadlockFormat::StreetBrawl,
                99,
                130,
                "hosted",
                "{}",
                Some(123),
            )
            .unwrap();
        let host = crate::deadlock_host::DeadlockHostRepository::new(file.path())
            .for_match(10, game.match_id)
            .unwrap()
            .unwrap();
        assert_eq!(host.account_key, "123");
        assert_eq!(host.phase, "reserved");
        let retry = repo
            .shuffle_with_request(
                10,
                DeadlockFormat::StreetBrawl,
                99,
                130,
                "hosted",
                "{}",
                Some(123),
            )
            .unwrap();
        assert_eq!(retry.match_id, game.match_id);
    }
    #[test]
    fn publication_recovery_retains_old_active_and_incomplete_terminal_matches() {
        let (file, repo) = fixture();
        let connection = open_runtime_connection(file.path()).unwrap();
        for id in 1..=34 {
            connection.execute("INSERT INTO deadlock_matches(match_id,guild_id,format,status,roster_json,created_by,created_at,updated_at,publication_channel_id,publication_message_id,publication_thread_id,thread_message_id) VALUES(?1,10,'street_brawl',?2,'{\"team1\":[],\"team2\":[]}',99,130,130,100,200,300,400)",params![id,if id==1{"running"}else{"settled"}]).unwrap();
        }
        connection
            .execute(
                "UPDATE deadlock_matches SET thread_message_id=NULL WHERE match_id IN(2,34)",
                [],
            )
            .unwrap();
        connection.execute("INSERT INTO deadlock_matches(match_id,guild_id,format,status,roster_json,created_by,created_at,updated_at) VALUES(100,20,'street_brawl','running','{\"team1\":[],\"team2\":[]}',99,130,130)",[]).unwrap();
        let ids = repo
            .publication_candidates(10)
            .unwrap()
            .into_iter()
            .map(|game| game.match_id)
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 27);
        assert_eq!(&ids[..2], &[1, 2]);
        assert!(!ids.contains(&3));
        assert_eq!(ids.last(), Some(&34));
        assert_eq!(ids.iter().filter(|&&id| id == 34).count(), 1);
        assert!(!ids.contains(&100));
    }
    #[test]
    fn signup_protects_orphaned_global_deadlock_identity_across_guilds() {
        let (file, repo) = fixture();
        enroll(&repo, 1);
        let connection = open_runtime_connection(file.path()).unwrap();
        connection
            .execute("DELETE FROM player_steam_ids WHERE discord_id=1", [])
            .unwrap();
        let seeds =
            [DeadlockFormat::StreetBrawl, DeadlockFormat::Standard].map(|format| DeadlockSeed {
                format,
                mu: 25.0,
                sigma: 8.333,
                source: "test".into(),
                source_value: None,
                provenance: None,
                source_at: None,
            });
        assert!(repo.enroll(20, 2, 1, "Other", &seeds, 150).is_err());
        repo.enroll(20, 1, 1, "Owner", &seeds, 150).unwrap();
        assert_eq!(repo.enrolled(20, 1).unwrap().unwrap().steam_id, 1);
    }
    #[test]
    fn abort_rejects_durable_launch_uncertainty_even_after_manual_review() {
        for evidence in [
            "admitted",
            "explicit_start",
            "host_launch",
            "host_running",
            "host_external",
            "core_running",
            "core_external",
        ] {
            let (file, repo) = fixture();
            for user in 1..=8 {
                enroll(&repo, user);
            }
            let game = repo
                .shuffle(10, DeadlockFormat::StreetBrawl, 99, 130)
                .unwrap();
            let connection = open_runtime_connection(file.path()).unwrap();
            connection
                .execute(
                    "UPDATE deadlock_matches SET status='gathering' WHERE match_id=?1",
                    [game.match_id],
                )
                .unwrap();
            connection.execute("INSERT INTO deadlock_host_jobs(match_id,guild_id,account_key,phase,created_at,updated_at) VALUES(?1,10,'test-host','manual_review',130,130)", [game.match_id]).unwrap();
            match evidence {
                "admitted" | "explicit_start" => {
                    let prefix = if evidence == "admitted" {
                        "deadlock_launch_admitted"
                    } else {
                        "deadlock_explicit_start"
                    };
                    connection
                        .execute(
                            "INSERT INTO app_kv(guild_id,key,value) VALUES(10,?1,'140')",
                            [format!("{prefix}:{}", game.match_id)],
                        )
                        .unwrap();
                }
                "host_launch" | "host_running" => {
                    let phase = if evidence == "host_launch" {
                        "launch_requested"
                    } else {
                        "running"
                    };
                    connection
                        .execute(
                            "UPDATE deadlock_host_jobs SET phase=?1 WHERE match_id=?2",
                            params![phase, game.match_id],
                        )
                        .unwrap();
                }
                "host_external" => {
                    connection.execute("UPDATE deadlock_host_jobs SET external_match_id='123' WHERE match_id=?1", [game.match_id]).unwrap();
                }
                "core_running" => {
                    connection
                        .execute(
                            "UPDATE deadlock_matches SET status='running' WHERE match_id=?1",
                            [game.match_id],
                        )
                        .unwrap();
                }
                "core_external" => {
                    connection
                        .execute(
                            "UPDATE deadlock_matches SET external_match_id=123 WHERE match_id=?1",
                            [game.match_id],
                        )
                        .unwrap();
                }
                _ => unreachable!(),
            }
            let error = repo
                .abort(10, game.match_id, 99, "uncertain launch", 150)
                .unwrap_err();
            assert!(
                error.to_string().contains("launch may have started"),
                "{evidence}: {error}"
            );
            let current = repo.match_by_id(10, game.match_id).unwrap().unwrap();
            assert_ne!(current.status, "aborted", "{evidence}");
            assert!(
                repo.queue_join(10, 1, 151).is_err(),
                "{evidence}: participant claim released"
            );
            assert_eq!(connection.query_row("SELECT COUNT(*) FROM deadlock_audit_events WHERE match_id=?1 AND kind='abort'", [game.match_id], |row|row.get::<_,i64>(0)).unwrap(), 0);
        }
    }
    #[test]
    fn prelaunch_manual_review_can_abort_and_other_guild_marker_does_not_block_it() {
        let (file, repo) = fixture();
        for user in 1..=8 {
            enroll(&repo, user);
        }
        let game = repo
            .shuffle(10, DeadlockFormat::StreetBrawl, 99, 130)
            .unwrap();
        let connection = open_runtime_connection(file.path()).unwrap();
        connection.execute("INSERT INTO deadlock_host_jobs(match_id,guild_id,account_key,phase,created_at,updated_at) VALUES(?1,10,'test-host','manual_review',130,130)", [game.match_id]).unwrap();
        connection
            .execute(
                "INSERT INTO app_kv(guild_id,key,value) VALUES(20,?1,'140')",
                [format!("deadlock_launch_admitted:{}", game.match_id)],
            )
            .unwrap();
        repo.abort(10, game.match_id, 99, "failed before launch admission", 150)
            .unwrap();
        repo.abort(10, game.match_id, 99, "failed before launch admission", 151)
            .unwrap();
        assert_eq!(
            repo.match_by_id(10, game.match_id).unwrap().unwrap().status,
            "aborted"
        );
        assert!(repo.queue_join(10, 1, 152).is_ok());
    }
}
