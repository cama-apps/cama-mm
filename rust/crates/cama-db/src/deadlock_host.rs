//! Durable dedicated-account host reservations and intent-first remote effects.
use crate::open_runtime_connection;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeadlockHostJob {
    pub match_id: i64,
    pub guild_id: i64,
    pub account_key: String,
    pub phase: String,
    pub party_id: Option<String>,
    pub join_code: Option<String>,
    pub external_match_id: Option<String>,
    pub error: Option<String>,
}
#[derive(Clone, Debug)]
pub struct DeadlockHostRepository {
    path: PathBuf,
}
fn invalid(message: &str) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(message.to_owned())
}
const SELECT: &str = "SELECT match_id,guild_id,account_key,phase,party_id,join_code,external_match_id,error FROM deadlock_host_jobs";
fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeadlockHostJob> {
    Ok(DeadlockHostJob {
        match_id: row.get(0)?,
        guild_id: row.get(1)?,
        account_key: row.get(2)?,
        phase: row.get(3)?,
        party_id: row.get(4)?,
        join_code: row.get(5)?,
        external_match_id: row.get(6)?,
        error: row.get(7)?,
    })
}
impl DeadlockHostRepository {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_owned(),
        }
    }
    /// Called once at match creation. `false` is a permanent manual fallback;
    /// the worker never scans and adopts unreserved matches later.
    pub fn reserve(
        &self,
        guild: i64,
        match_id: i64,
        account: u32,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let reserved = reserve_on(&transaction, guild, match_id, account, now)?;
        transaction.commit()?;
        Ok(reserved)
    }

    pub fn active(&self, account: u32) -> rusqlite::Result<Option<DeadlockHostJob>> {
        open_runtime_connection(&self.path)?
            .query_row(
                &format!("{SELECT} WHERE account_key=?1 AND phase!='released'"),
                [account.to_string()],
                read,
            )
            .optional()
    }
    pub fn for_match(
        &self,
        guild: i64,
        match_id: i64,
    ) -> rusqlite::Result<Option<DeadlockHostJob>> {
        open_runtime_connection(&self.path)?
            .query_row(
                &format!("{SELECT} WHERE guild_id=?1 AND match_id=?2"),
                params![guild, match_id],
                read,
            )
            .optional()
    }
    /// Compare-and-swap phases; only the winner may perform the remote action.
    pub fn transition(
        &self,
        job: &DeadlockHostJob,
        phase: &str,
        now: i64,
    ) -> rusqlite::Result<bool> {
        if job.phase == "released"
            || !matches!(
                (job.phase.as_str(), phase),
                ("reserved", "create_requested")
                    | ("create_requested", "gathering")
                    | ("gathering", "launch_requested")
                    | ("gathering", "running")
                    | ("launch_requested", "running")
                    | (_, "manual_review")
                    | (_, "released")
            )
        {
            return Err(invalid("invalid host phase transition"));
        }
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed=transaction.execute("UPDATE deadlock_host_jobs SET phase=?4,updated_at=?5 WHERE match_id=?1 AND guild_id=?2 AND phase=?3 AND (?4!='launch_requested' OR EXISTS(SELECT 1 FROM deadlock_matches m JOIN deadlock_betting_markets b ON b.guild_id=m.guild_id AND b.match_id=m.match_id WHERE m.guild_id=?2 AND m.match_id=?1 AND m.status='gathering' AND b.status='closed'))",params![job.match_id,job.guild_id,job.phase,phase,now])?==1;
        if changed && phase == "launch_requested" {
            // Survives a later manual_review phase after a lost ready/start
            // response; abort must not assume no remote match was launched.
            transaction.execute("INSERT INTO app_kv(guild_id,key,value) VALUES(?1,?2,?3) ON CONFLICT(guild_id,key) DO NOTHING",params![job.guild_id,format!("deadlock_launch_admitted:{}",job.match_id),now.to_string()])?;
        }
        transaction.commit()?;
        Ok(changed)
    }
    /// Readiness itself may auto-start. The overall launch intent precedes it;
    /// this separate one-shot marker fences any later explicit start request.
    pub fn claim_explicit_start(&self, job: &DeadlockHostJob, now: i64) -> rusqlite::Result<bool> {
        let mut connection = open_runtime_connection(&self.path)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let eligible:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM deadlock_host_jobs h JOIN deadlock_matches m ON m.guild_id=h.guild_id AND m.match_id=h.match_id JOIN deadlock_betting_markets b ON b.guild_id=h.guild_id AND b.match_id=h.match_id WHERE h.guild_id=?1 AND h.match_id=?2 AND h.phase='launch_requested' AND m.status='gathering' AND b.status='closed')",params![job.guild_id,job.match_id],|row|row.get(0))?;
        if !eligible {
            return Ok(false);
        }
        let inserted=tx.execute("INSERT INTO app_kv(guild_id,key,value) VALUES(?1,?2,?3) ON CONFLICT(guild_id,key) DO NOTHING",params![job.guild_id,format!("deadlock_explicit_start:{}",job.match_id),now.to_string()])?==1;
        tx.commit()?;
        Ok(inserted)
    }
    pub fn party(
        &self,
        job: &DeadlockHostJob,
        party_id: u64,
        join_code: Option<u64>,
        now: i64,
    ) -> rusqlite::Result<()> {
        if party_id == 0 {
            return Err(invalid("positive party ID required"));
        }
        let changed=open_runtime_connection(&self.path)?.execute("UPDATE deadlock_host_jobs SET party_id=?3,join_code=COALESCE(?4,join_code),updated_at=?5 WHERE match_id=?1 AND guild_id=?2 AND phase!='released' AND (party_id IS NULL OR party_id=?3)",params![job.match_id,job.guild_id,party_id.to_string(),join_code.map(|v|v.to_string()),now])?;
        if changed != 1 {
            return Err(invalid("host party changed unexpectedly"));
        }
        Ok(())
    }
    pub fn running(&self, job: &DeadlockHostJob, external: u64, now: i64) -> rusqlite::Result<()> {
        let external_i64 =
            i64::try_from(external).map_err(|_| invalid("match ID exceeds storage range"))?;
        if external_i64 <= 0 {
            return Err(invalid("positive external match ID required"));
        }
        let mut connection = open_runtime_connection(&self.path)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed=tx.execute("UPDATE deadlock_host_jobs SET external_match_id=?3,phase='running',updated_at=?4 WHERE guild_id=?1 AND match_id=?2 AND phase IN ('gathering','launch_requested','running') AND (external_match_id IS NULL OR external_match_id=?3)",params![job.guild_id,job.match_id,external.to_string(),now])?;
        if changed != 1 {
            return Err(invalid("external match identity changed"));
        }
        let changed = tx.execute("UPDATE deadlock_matches SET external_match_id=?3,status='running',updated_at=?4 WHERE guild_id=?1 AND match_id=?2 AND status IN ('gathering','running') AND (external_match_id IS NULL OR external_match_id=?3)",params![job.guild_id,job.match_id,external_i64,now])?;
        if changed != 1 {
            return Err(invalid(
                "local match ended or changed during host observation",
            ));
        }
        tx.commit()?;
        Ok(())
    }
    /// Evidence is written before result recording; retries preserve the first
    /// verified result, while the core recording transaction rejects conflicts.
    pub fn record_evidence(
        &self,
        job: &DeadlockHostJob,
        evidence: &str,
        _now: i64,
    ) -> rusqlite::Result<()> {
        let parsed: serde_json::Value =
            serde_json::from_str(evidence).map_err(|_| invalid("invalid result evidence"))?;
        if parsed.get("source").and_then(serde_json::Value::as_str) != Some("valve_gc_metadata") {
            return Err(invalid("invalid metadata evidence source"));
        }
        open_runtime_connection(&self.path)?.execute("INSERT INTO app_kv(guild_id,key,value) VALUES(?1,?2,?3) ON CONFLICT(guild_id,key) DO NOTHING",params![job.guild_id,format!("deadlock_result:{}",job.match_id),evidence])?;
        Ok(())
    }
    pub fn manual_review(
        &self,
        job: &DeadlockHostJob,
        reason: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        open_runtime_connection(&self.path)?.execute("UPDATE deadlock_host_jobs SET phase='manual_review',error=?3,updated_at=?4 WHERE guild_id=?1 AND match_id=?2 AND phase!='released'",params![job.guild_id,job.match_id,reason,now])?;
        Ok(())
    }
}

/// Reserve within the same writer transaction as immutable roster creation.
pub(crate) fn reserve_on(
    transaction: &rusqlite::Transaction<'_>,
    guild: i64,
    match_id: i64,
    account: u32,
    now: i64,
) -> rusqlite::Result<bool> {
    if guild <= 0 || match_id <= 0 || account == 0 {
        return Err(invalid(
            "positive guild, match and Steam account IDs required",
        ));
    }
    let existing: Option<(String, String)> = transaction
        .query_row(
            "SELECT account_key,phase FROM deadlock_host_jobs WHERE guild_id=?1 AND match_id=?2",
            params![guild, match_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((key, phase)) = existing {
        return Ok(key == account.to_string() && phase != "released");
    }
    let eligible:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM deadlock_matches WHERE guild_id=?1 AND match_id=?2 AND status IN ('economic_setup','gathering'))",params![guild,match_id],|r|r.get(0))?;
    if !eligible {
        return Err(invalid(
            "match is not eligible for initial host reservation",
        ));
    }
    let busy:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM deadlock_host_jobs WHERE account_key=?1 AND phase!='released')",[account.to_string()],|r|r.get(0))?;
    // Persist a released tombstone for manual fallback so retry cannot adopt it.
    transaction.execute("INSERT INTO deadlock_host_jobs(match_id,guild_id,account_key,phase,error,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?6)",params![match_id,guild,account.to_string(),if busy {"released"} else {"reserved"},if busy {Some("dedicated host busy; manual lobby")} else {None},now])?;
    Ok(!busy)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn games(path: &Path) {
        let connection = open_runtime_connection(path).unwrap();
        for (id, guild) in [(1, 10), (2, 20), (3, 10)] {
            connection.execute("INSERT INTO deadlock_matches(match_id,guild_id,format,status,roster_json,created_by,created_at,updated_at) VALUES(?1,?2,'street_brawl','gathering','{}',1,100,100)",params![id,guild]).unwrap();
        }
    }
    #[test]
    fn busy_fallback_is_permanent_and_account_claims_span_guilds() {
        let db = crate::test_support::fast_migrated_database();
        games(db.path());
        let repo = DeadlockHostRepository::new(db.path());
        assert!(repo.reserve(10, 1, 42, 100).unwrap());
        assert!(!repo.reserve(20, 2, 42, 100).unwrap());
        let first = repo.active(42).unwrap().unwrap();
        assert_eq!(first.guild_id, 10);
        assert!(repo.transition(&first, "released", 101).unwrap());
        assert!(!repo.reserve(20, 2, 42, 102).unwrap());
        assert!(repo.reserve(10, 3, 42, 102).unwrap());
    }
    #[test]
    fn intents_are_compare_and_swap_and_party_identity_is_immutable() {
        let db = crate::test_support::fast_migrated_database();
        games(db.path());
        let repo = DeadlockHostRepository::new(db.path());
        repo.reserve(10, 1, 42, 100).unwrap();
        let reserved = repo.active(42).unwrap().unwrap();
        assert!(repo.transition(&reserved, "create_requested", 101).unwrap());
        assert!(!repo.transition(&reserved, "create_requested", 102).unwrap());
        repo.party(&reserved, 999, None, 103).unwrap();
        repo.party(&reserved, 999, Some(123), 104).unwrap();
        assert!(repo.party(&reserved, 888, None, 105).is_err());
        assert!(repo.for_match(20, 1).unwrap().is_none());
        let current = repo.active(42).unwrap().unwrap();
        assert_eq!(current.join_code.as_deref(), Some("123"));
    }
    #[test]
    fn uncertain_remote_effect_keeps_the_account_reserved() {
        let db = crate::test_support::fast_migrated_database();
        games(db.path());
        let repo = DeadlockHostRepository::new(db.path());
        repo.reserve(10, 1, 42, 100).unwrap();
        let first = repo.active(42).unwrap().unwrap();
        repo.manual_review(&first, "lost create response", 101)
            .unwrap();
        assert!(!repo.reserve(20, 2, 42, 102).unwrap());
        assert_eq!(repo.active(42).unwrap().unwrap().phase, "manual_review");
    }
    #[test]
    fn launch_and_explicit_start_require_closed_live_market_and_one_shot_marker() {
        let db = crate::test_support::fast_migrated_database();
        games(db.path());
        let repo = DeadlockHostRepository::new(db.path());
        repo.reserve(10, 1, 42, 100).unwrap();
        let first = repo.active(42).unwrap().unwrap();
        repo.transition(&first, "create_requested", 100).unwrap();
        let created = repo.active(42).unwrap().unwrap();
        repo.transition(&created, "gathering", 100).unwrap();
        let gathered = repo.active(42).unwrap().unwrap();
        let connection = open_runtime_connection(db.path()).unwrap();
        connection.execute("INSERT INTO deadlock_betting_markets(market_id,guild_id,match_id,format,roster_json,terms_json,deadline,status,created_at) VALUES('deadlock:10:1',10,1,'street_brawl','{}','{}',101,'open',100)",[]).unwrap();
        assert!(!repo.transition(&gathered, "launch_requested", 102).unwrap());
        assert!(!repo.claim_explicit_start(&gathered, 102).unwrap());
        connection
            .execute(
                "UPDATE deadlock_betting_markets SET status='closed' WHERE match_id=1",
                [],
            )
            .unwrap();
        assert!(repo.transition(&gathered, "launch_requested", 103).unwrap());
        let launching = repo.active(42).unwrap().unwrap();
        assert!(connection.query_row("SELECT EXISTS(SELECT 1 FROM app_kv WHERE guild_id=10 AND key='deadlock_launch_admitted:1')",[],|row|row.get::<_,bool>(0)).unwrap());
        connection
            .execute(
                "UPDATE deadlock_matches SET status='aborted' WHERE match_id=1",
                [],
            )
            .unwrap();
        assert!(!repo.claim_explicit_start(&launching, 104).unwrap());
        connection
            .execute(
                "UPDATE deadlock_matches SET status='gathering' WHERE match_id=1",
                [],
            )
            .unwrap();
        assert!(repo.claim_explicit_start(&launching, 105).unwrap());
        assert!(!repo.claim_explicit_start(&launching, 106).unwrap());
    }
}
