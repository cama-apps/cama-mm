use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use super::{AdminRepository, increment_openskill_revision, open_runtime_connection};

#[path = "account_transfer/relational.rs"]
mod relational;
#[path = "account_transfer/structured.rs"]
mod structured;

#[derive(Debug, Error)]
pub enum TransferError {
    #[error("{0}")]
    Conflict(String),
    #[error("SQLite account transfer failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

impl AdminRepository {
    /// Recover an existing player's bot-wide identity onto an unused Discord account.
    pub fn transfer_account(
        &self,
        guild_id: i64,
        old: i64,
        new: i64,
        destination_name: &str,
    ) -> Result<(), TransferError> {
        if old <= 0 || new <= 0 || old == new || guild_id <= 0 {
            return Err(TransferError::Conflict(
                "Choose two distinct Discord accounts in this server.".into(),
            ));
        }
        let mut conn = open_runtime_connection(&self.path)?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let guilds = transaction
            .prepare("SELECT guild_id FROM players WHERE discord_id=?1")?
            .query_map([old], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if !guilds.contains(&guild_id) {
            return Err(TransferError::Conflict(
                "The old Discord ID has no player profile in this server.".into(),
            ));
        }
        if transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM players WHERE discord_id=?1)",
            [new],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(TransferError::Conflict(
                "The replacement account already has a bot profile.".into(),
            ));
        }

        ensure_sessions_finished(&transaction, old, new)?;
        // The question buttons belong to the old Discord account. Close the session
        // with its answers and earned rewards intact, including after a bot restart.
        transaction.execute(
            "UPDATE player_trivia_sessions SET status='cancelled', completed_at=unixepoch()
             WHERE discord_id=?1 AND status='active'",
            [old],
        )?;
        structured::transfer(&transaction, old, new)?;
        relational::transfer(&transaction, old, new)?;
        transaction.execute(
            "UPDATE players SET discord_username=?1 WHERE discord_id=?2 AND guild_id=?3",
            params![destination_name, new, guild_id],
        )?;
        for guild in guilds {
            increment_openskill_revision(&transaction, guild)?;
        }
        transaction.commit()?;
        Ok(())
    }
}

// These sessions can have outstanding interactions or workers bound to the old identity.
fn ensure_sessions_finished(
    transaction: &Transaction<'_>,
    old: i64,
    new: i64,
) -> Result<(), TransferError> {
    let busy: Option<String> = transaction.query_row(
        "SELECT 'Dota lobby' FROM lobby_state WHERE created_by IN (?1,?2) AND lower(status)='open'
         UNION ALL SELECT 'Dota wager' FROM bets b JOIN pending_matches p ON p.guild_id=b.guild_id
           AND (b.pending_match_id=p.pending_match_id OR (b.pending_match_id IS NULL
                AND b.bet_time >= coalesce(json_extract(p.payload,'$.shuffle_timestamp'),0)))
           WHERE b.match_id IS NULL AND b.discord_id IN (?1,?2)
         UNION ALL SELECT 'match correction' FROM match_correction_claims c JOIN match_participants p USING(match_id)
           WHERE p.discord_id IN (?1,?2)
         UNION ALL SELECT 'Deadlock queue' FROM deadlock_queue WHERE discord_id IN (?1,?2)
         UNION ALL SELECT 'Deadlock match' FROM deadlock_participants p JOIN deadlock_matches m USING(match_id)
           WHERE p.discord_id IN (?1,?2) AND m.status NOT IN ('settled','aborted')
         UNION ALL SELECT 'Deadlock wager' FROM deadlock_wagers w JOIN deadlock_betting_markets m USING(market_id)
           WHERE w.discord_id IN (?1,?2) AND m.status IN ('open','closed')
         UNION ALL SELECT 'Mafia game' FROM mafia_players p JOIN mafia_games g USING(game_id)
           WHERE p.discord_id IN (?1,?2) AND g.phase != 'RESOLVED'
         UNION ALL SELECT 'duel' FROM duel_challenges WHERE status IN ('pending','accepted')
           AND (challenger_id IN (?1,?2) OR recipient_id IN (?1,?2))
         UNION ALL SELECT 'pet brawl' FROM pet_brawls WHERE status IN ('pending','active')
           AND (challenger_id IN (?1,?2) OR recipient_id IN (?1,?2))
         LIMIT 1",
        [old, new], |r| r.get(0),
    ).optional()?;
    if let Some(activity) = busy {
        return Err(TransferError::Conflict(format!(
            "Finish or clear the active {activity} before transferring this account."
        )));
    }
    Ok(())
}
