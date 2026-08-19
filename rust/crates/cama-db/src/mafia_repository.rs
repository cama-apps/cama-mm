//! Existing-schema SQLite persistence for the Daily Mafia subgame.
//!
//! Python remains the sole schema-migration and live-runtime authority. Every
//! entry point opens an already-existing database through
//! [`crate::open_runtime_connection`]; production code contains no DDL. Game
//! startup, opt-out cancellation, bounty stakes, and bounty resolution use
//! `BEGIN IMMEDIATE` so their multi-table effects commit as one unit.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, Value, ValueRef};
use rusqlite::{
    Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params, params_from_iter,
};
use thiserror::Error;

use crate::open_runtime_connection;

const SIGNUP_BUCKET: &str = "pending";

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub enum $name { $($variant),+ }

        impl $name {
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $value),+ }
            }
        }

        impl FromSql for $name {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                match value.as_str()? {
                    $($value => Ok(Self::$variant),)+
                    other => Err(FromSqlError::Other(
                        format!("invalid {} value: {other}", stringify!($name)).into(),
                    )),
                }
            }
        }
    };
}

string_enum!(MafiaPhase {
    Setup => "SETUP",
    Night => "NIGHT",
    Day => "DAY",
    Resolved => "RESOLVED",
});
string_enum!(MafiaRole {
    Mafia => "MAFIA",
    Doctor => "DOCTOR",
    Detective => "DETECTIVE",
    Vigilante => "VIGILANTE",
    Townie => "TOWNIE",
    Jester => "JESTER",
    Bookie => "BOOKIE",
});
string_enum!(MafiaActionType {
    Kill => "KILL",
    Save => "SAVE",
    Investigate => "INVESTIGATE",
    VigilanteKill => "VIG_KILL",
    Vote => "VOTE",
    Wager => "WAGER",
});
string_enum!(MafiaTwist {
    BloodMoon => "BLOOD_MOON",
    TownHall => "TOWN_HALL",
    MemoryFog => "MEMORY_FOG",
    Plague => "PLAGUE",
    Resurrection => "RESURRECTION",
});
string_enum!(MafiaWinner {
    Town => "TOWN",
    Mafia => "MAFIA",
    Jester => "JESTER",
    None => "NONE",
});

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MafiaGame {
    pub game_id: i64,
    pub guild_id: i64,
    pub game_date: String,
    pub phase: MafiaPhase,
    pub started_at: i64,
    pub roster_size: i64,
    pub twist_event: Option<MafiaTwist>,
    pub night_ended_at: Option<i64>,
    pub day_ended_at: Option<i64>,
    pub winner: Option<MafiaWinner>,
    pub entry_fee: i64,
    pub payout_per_winner: i64,
    pub mvp_id: Option<i64>,
    pub mafia_thread_id: Option<i64>,
    pub discussion_thread_id: Option<i64>,
    pub setup_message_id: Option<i64>,
    pub day_number: i64,
    pub phase_started_at: Option<i64>,
    pub standings_message_id: Option<i64>,
    pub graveyard_thread_id: Option<i64>,
    pub mafia_intro_message_id: Option<i64>,
    pub graveyard_intro_message_id: Option<i64>,
    pub resolution_message_id: Option<i64>,
    pub resolution_summary: Option<String>,
    pub status: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MafiaPlayer {
    pub game_id: i64,
    pub discord_id: i64,
    pub guild_id: i64,
    pub role: MafiaRole,
    pub is_godfather: bool,
    pub hero_name: Option<String>,
    pub is_alive: bool,
    pub eliminated_phase: Option<MafiaPhase>,
    pub eliminated_at: Option<i64>,
    pub acted: bool,
}

impl MafiaPlayer {
    #[must_use]
    pub fn new(game_id: i64, discord_id: i64, guild_id: i64, role: MafiaRole) -> Self {
        Self {
            game_id,
            discord_id,
            guild_id,
            role,
            is_godfather: false,
            hero_name: None,
            is_alive: true,
            eliminated_phase: None,
            eliminated_at: None,
            acted: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MafiaAction {
    pub game_id: i64,
    pub guild_id: i64,
    pub actor_id: i64,
    pub target_id: Option<i64>,
    pub action_type: MafiaActionType,
    pub phase: MafiaPhase,
    pub day_number: i64,
    pub result: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MafiaLeaderboardRow {
    pub discord_id: i64,
    pub games_played: i64,
    pub wins: i64,
    pub mafia_wins: i64,
    pub town_wins: i64,
    pub jester_wins: i64,
    pub mvp_count: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MafiaPlayerStats {
    pub games_played: i64,
    pub wins: i64,
    pub mafia_wins: i64,
    pub town_wins: i64,
    pub jester_wins: i64,
    pub mafia_kills: i64,
    pub doctor_saves: i64,
    pub correct_reads: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MafiaHistoryRow {
    pub game_id: i64,
    pub game_date: String,
    pub winner: Option<MafiaWinner>,
    pub twist_event: Option<MafiaTwist>,
    pub payout_per_winner: i64,
    pub mvp_id: Option<i64>,
    pub role: MafiaRole,
    pub is_godfather: bool,
    pub hero_name: Option<String>,
    pub is_alive: bool,
    pub eliminated_phase: Option<MafiaPhase>,
    pub acted: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddBountyResult {
    Added,
    AlreadyStaked,
    InsufficientFunds,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BountyResolution {
    pub paid: BTreeMap<i64, i64>,
    pub reward: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MafiaPhaseCommit {
    pub applied: bool,
    pub reason: Option<&'static str>,
    pub bounty: BountyResolution,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MafiaCycleAdvance {
    pub game_id: i64,
    pub ended_at: i64,
    pub guild_id: Option<i64>,
    pub day_number: i64,
    pub lynched_id: Option<i64>,
    pub lynched_was_mafia: bool,
    pub alive_count: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MafiaCancelResult {
    pub applied: bool,
    pub reason: Option<&'static str>,
    pub refunded: BTreeMap<i64, i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MafiaFinalizeResult {
    pub applied: bool,
    pub reason: Option<&'static str>,
    pub bounty: BountyResolution,
    pub bankruptcy_penalties: BTreeMap<i64, i64>,
    pub vanity_taxes: BTreeMap<i64, i64>,
    pub low_priority_taxes: BTreeMap<i64, i64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ThreadIds {
    pub mafia_thread_id: Option<i64>,
    pub discussion_thread_id: Option<i64>,
    pub setup_message_id: Option<i64>,
    pub graveyard_thread_id: Option<i64>,
    pub mafia_intro_message_id: Option<i64>,
    pub graveyard_intro_message_id: Option<i64>,
    pub standings_message_id: Option<i64>,
    pub resolution_message_id: Option<i64>,
}

#[derive(Clone, Copy, Debug)]
pub struct MafiaGameStart<'a> {
    pub guild_id: Option<i64>,
    pub game_date: &'a str,
    pub phase: MafiaPhase,
    pub started_at: i64,
    pub roster_size: i64,
    pub twist_event: Option<MafiaTwist>,
    pub entry_fee: i64,
    pub players: &'a [MafiaPlayer],
    pub max_debt: i64,
}

#[derive(Debug, Error)]
pub enum MafiaRepositoryError {
    #[error("Mafia SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}")]
    Invalid(&'static str),
}

#[derive(Clone, Debug)]
pub struct MafiaRepository {
    path: PathBuf,
}

impl MafiaRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    #[must_use]
    pub const fn normalize_guild_id(guild_id: Option<i64>) -> i64 {
        match guild_id {
            Some(guild_id) => guild_id,
            None => 0,
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    pub fn create_game(
        &self,
        guild_id: Option<i64>,
        game_date: &str,
        phase: MafiaPhase,
        started_at: i64,
        roster_size: i64,
        twist_event: Option<MafiaTwist>,
    ) -> Result<i64, MafiaRepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO mafia_games (
                 guild_id, game_date, phase, started_at, roster_size,
                 twist_event, phase_started_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?4)",
            params![
                Self::normalize_guild_id(guild_id),
                game_date,
                phase.as_str(),
                started_at,
                roster_size,
                twist_event.map(MafiaTwist::as_str),
            ],
        )?;
        Ok(connection.last_insert_rowid())
    }

    pub fn get_game_by_id(&self, game_id: i64) -> Result<Option<MafiaGame>, MafiaRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT * FROM mafia_games WHERE game_id = ?1",
                [game_id],
                row_to_game,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_game_for_date(
        &self,
        guild_id: Option<i64>,
        game_date: &str,
    ) -> Result<Option<MafiaGame>, MafiaRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT * FROM mafia_games WHERE guild_id = ?1 AND game_date = ?2
                 ORDER BY game_id LIMIT 1",
                params![Self::normalize_guild_id(guild_id), game_date],
                row_to_game,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_active_game(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Option<MafiaGame>, MafiaRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT * FROM mafia_games
                 WHERE guild_id = ?1 AND phase != 'RESOLVED'
                 ORDER BY game_id DESC LIMIT 1",
                [Self::normalize_guild_id(guild_id)],
                row_to_game,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Resolved games whose public resolution receipt was not durably stored.
    /// READY recovery uses this to finish a Discord send that was interrupted
    /// after the SQLite settlement transaction committed.
    pub fn get_unannounced_resolved_games(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Vec<MafiaGame>, MafiaRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT * FROM mafia_games
             WHERE guild_id=?1 AND phase='RESOLVED' AND status != 'CANCELLED'
               AND resolution_message_id IS NULL
               AND resolution_summary IS NOT NULL
             ORDER BY game_id",
        )?;
        let rows = statement.query_map([Self::normalize_guild_id(guild_id)], row_to_game)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn set_resolution_message_id(
        &self,
        game_id: i64,
        message_id: i64,
    ) -> Result<bool, MafiaRepositoryError> {
        let changed = self.connection()?.execute(
            "UPDATE mafia_games
             SET resolution_message_id=?1
             WHERE game_id=?2 AND resolution_message_id IS NULL",
            params![message_id, game_id],
        )?;
        Ok(changed == 1)
    }

    /// Persist the complete terminal-resolution payload before attempting the
    /// public Discord send. If the process dies after settlement but before
    /// the message receipt is stored, READY can replay exact copy and payout
    /// fields rather than reconstructing a sparse summary from wallets.
    pub fn set_resolution_summary(
        &self,
        game_id: i64,
        summary: &str,
    ) -> Result<bool, MafiaRepositoryError> {
        let changed = self.connection()?.execute(
            "UPDATE mafia_games
             SET resolution_summary=?1
             WHERE game_id=?2 AND resolution_summary IS NULL",
            params![summary, game_id],
        )?;
        Ok(changed == 1)
    }

    /// Claim selected pending signups, create their game, and debit all entry
    /// fees atomically. Any roster, balance, or delete mismatch rolls back.
    pub fn create_game_with_players_and_entry_fees(
        &self,
        request: MafiaGameStart<'_>,
    ) -> Result<i64, MafiaRepositoryError> {
        let MafiaGameStart {
            guild_id,
            game_date,
            phase,
            started_at,
            roster_size,
            twist_event,
            entry_fee,
            players,
            max_debt,
        } = request;
        if players.is_empty() {
            return Err(MafiaRepositoryError::Invalid("no_players"));
        }
        let unique = players
            .iter()
            .map(|player| player.discord_id)
            .collect::<BTreeSet<_>>();
        if usize::try_from(roster_size).ok() != Some(players.len()) || unique.len() != players.len()
        {
            return Err(MafiaRepositoryError::Invalid("invalid_roster"));
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active = transaction
            .query_row(
                "SELECT game_id FROM mafia_games
                 WHERE guild_id = ?1 AND phase != 'RESOLVED'
                 ORDER BY game_id DESC LIMIT 1",
                [guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if active.is_some() {
            return Err(MafiaRepositoryError::Invalid("game_already_active"));
        }
        for player in players {
            let eligible = transaction
                .query_row(
                    "SELECT 1
                     FROM mafia_signups s
                     JOIN players p ON p.discord_id=s.discord_id AND p.guild_id=s.guild_id
                     LEFT JOIN mafia_optout o
                       ON o.discord_id=s.discord_id AND o.guild_id=s.guild_id
                     WHERE s.guild_id=?1 AND s.week_start=?2 AND s.discord_id=?3
                       AND o.discord_id IS NULL
                       AND COALESCE(p.jopacoin_balance,0)-?4 >= ?5",
                    params![
                        guild_id,
                        SIGNUP_BUCKET,
                        player.discord_id,
                        entry_fee,
                        -max_debt
                    ],
                    |_| Ok(()),
                )
                .optional()?;
            if eligible.is_none() {
                return Err(MafiaRepositoryError::Invalid("signup_claim_failed"));
            }
        }
        transaction.execute(
            "INSERT INTO mafia_games (
                 guild_id, game_date, phase, started_at, roster_size,
                 twist_event, entry_fee, phase_started_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?4)",
            params![
                guild_id,
                game_date,
                phase.as_str(),
                started_at,
                roster_size,
                twist_event.map(MafiaTwist::as_str),
                entry_fee,
            ],
        )?;
        let game_id = transaction.last_insert_rowid();
        for player in players {
            insert_player(&transaction, game_id, guild_id, player)?;
        }
        set_ledger_context(
            &transaction,
            "mafia_entry_fee",
            None,
            game_id,
            "mafia entry fee",
        )?;
        for player in players {
            let changed = transaction.execute(
                "UPDATE players
                 SET jopacoin_balance=COALESCE(jopacoin_balance,0)-?1,
                     updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2 AND guild_id=?3
                   AND COALESCE(jopacoin_balance,0)-?1 >= ?4",
                params![entry_fee, player.discord_id, guild_id, -max_debt],
            )?;
            if changed != 1 {
                return Err(MafiaRepositoryError::Invalid("entry_fee_debit_failed"));
            }
            transaction.execute(
                "UPDATE players SET lowest_balance_ever=jopacoin_balance
                 WHERE discord_id=?1 AND guild_id=?2
                   AND (lowest_balance_ever IS NULL OR jopacoin_balance<lowest_balance_ever)",
                params![player.discord_id, guild_id],
            )?;
        }
        clear_ledger_context(&transaction)?;
        for player in players {
            let removed = transaction.execute(
                "DELETE FROM mafia_signups
                 WHERE guild_id=?1 AND week_start=?2 AND discord_id=?3",
                params![guild_id, SIGNUP_BUCKET, player.discord_id],
            )?;
            if removed != 1 {
                return Err(MafiaRepositoryError::Invalid("signup_claim_failed"));
            }
        }
        transaction.commit()?;
        Ok(game_id)
    }

    pub fn claim_phase_reminder(
        &self,
        guild_id: Option<i64>,
        game_id: i64,
        day_number: i64,
        phase: MafiaPhase,
    ) -> Result<bool, MafiaRepositoryError> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "INSERT OR IGNORE INTO mafia_phase_reminders
             (guild_id,game_id,day_number,phase,claimed_at) VALUES (?1,?2,?3,?4,?5)",
            params![
                Self::normalize_guild_id(guild_id),
                game_id,
                day_number,
                phase.as_str(),
                now_seconds(),
            ],
        )? == 1)
    }

    pub fn release_phase_reminder(
        &self,
        guild_id: Option<i64>,
        game_id: i64,
        day_number: i64,
        phase: MafiaPhase,
    ) -> Result<(), MafiaRepositoryError> {
        self.connection()?.execute(
            "DELETE FROM mafia_phase_reminders
             WHERE guild_id=?1 AND game_id=?2 AND day_number=?3 AND phase=?4",
            params![
                Self::normalize_guild_id(guild_id),
                game_id,
                day_number,
                phase.as_str()
            ],
        )?;
        Ok(())
    }

    pub fn set_phase(
        &self,
        game_id: i64,
        phase: MafiaPhase,
        night_ended_at: Option<i64>,
        day_ended_at: Option<i64>,
    ) -> Result<(), MafiaRepositoryError> {
        self.connection()?.execute(
            "UPDATE mafia_games SET phase=?1,
                 night_ended_at=COALESCE(?2,night_ended_at),
                 day_ended_at=COALESCE(?3,day_ended_at)
             WHERE game_id=?4",
            params![phase.as_str(), night_ended_at, day_ended_at, game_id],
        )?;
        Ok(())
    }

    /// Claim an active game for an administrator-forced terminal resolution.
    ///
    /// The claim is deliberately conditional so two concurrent `/mafia admin
    /// stop` requests cannot both settle the same game. The force-finalize
    /// resolver preserves the current cycle number because Python's admin
    /// standing tally is not a normal day-14 transition.
    pub fn try_reopen_for_finalize(&self, game_id: i64) -> Result<bool, MafiaRepositoryError> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "UPDATE mafia_games
             SET phase='DAY', phase_started_at=?1
             WHERE game_id=?2 AND phase!='RESOLVED'",
            params![now_seconds(), game_id],
        )? == 1)
    }

    pub fn set_twist_event(
        &self,
        game_id: i64,
        twist: Option<MafiaTwist>,
    ) -> Result<(), MafiaRepositoryError> {
        self.connection()?.execute(
            "UPDATE mafia_games SET twist_event=?1 WHERE game_id=?2",
            params![twist.map(MafiaTwist::as_str), game_id],
        )?;
        Ok(())
    }

    /// Atomically apply the NIGHT deaths and claim NIGHT -> DAY. A losing
    /// resolver observes `applied=false` and must not write detective or
    /// announcement side effects for a phase it did not own.
    pub fn apply_night_resolution(
        &self,
        game_id: i64,
        killed_ids: &[i64],
        ended_at: i64,
    ) -> Result<bool, MafiaRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE mafia_games
             SET phase='DAY', night_ended_at=?1, phase_started_at=?1
             WHERE game_id=?2 AND phase='NIGHT'",
            params![ended_at, game_id],
        )?;
        if changed != 1 {
            return Ok(false);
        }
        for discord_id in killed_ids {
            transaction.execute(
                "UPDATE mafia_players
                 SET is_alive=0, eliminated_phase='NIGHT', eliminated_at=?1
                 WHERE game_id=?2 AND discord_id=?3 AND is_alive=1",
                params![ended_at, game_id, discord_id],
            )?;
        }
        transaction.commit()?;
        Ok(true)
    }

    /// Atomically apply a non-terminal day and advance DAY -> next NIGHT.
    pub fn advance_to_next_cycle(
        &self,
        request: MafiaCycleAdvance,
    ) -> Result<MafiaPhaseCommit, MafiaRepositoryError> {
        let MafiaCycleAdvance {
            game_id,
            ended_at,
            guild_id,
            day_number,
            lynched_id,
            lynched_was_mafia,
            alive_count,
        } = request;
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE mafia_games
             SET phase='NIGHT', day_ended_at=?1, phase_started_at=?1,
                 day_number=day_number+1
             WHERE game_id=?2 AND phase='DAY'",
            params![ended_at, game_id],
        )?;
        if changed != 1 {
            return Ok(MafiaPhaseCommit {
                applied: false,
                reason: Some("day_already_resolved"),
                ..MafiaPhaseCommit::default()
            });
        }
        if let Some(lynched_id) = lynched_id {
            transaction.execute(
                "UPDATE mafia_players
                 SET is_alive=0, eliminated_phase='DAY', eliminated_at=?1
                 WHERE game_id=?2 AND discord_id=?3 AND is_alive=1",
                params![ended_at, game_id, lynched_id],
            )?;
        }
        mark_bookie_wager_hit(&transaction, game_id, day_number, lynched_id)?;
        let bounty = resolve_bounties(
            &transaction,
            game_id,
            guild_id,
            day_number,
            lynched_id,
            lynched_was_mafia,
            alive_count,
        )?;
        transaction.commit()?;
        Ok(MafiaPhaseCommit {
            applied: true,
            reason: None,
            bounty,
        })
    }

    pub fn revive_player(&self, game_id: i64, discord_id: i64) -> Result<(), MafiaRepositoryError> {
        self.connection()?.execute(
            "UPDATE mafia_players
             SET is_alive=1, eliminated_phase=NULL, eliminated_at=NULL
             WHERE game_id=?1 AND discord_id=?2",
            params![game_id, discord_id],
        )?;
        Ok(())
    }

    pub fn set_thread_ids(&self, game_id: i64, ids: ThreadIds) -> Result<(), MafiaRepositoryError> {
        self.connection()?.execute(
            "UPDATE mafia_games SET
                 mafia_thread_id=COALESCE(?1,mafia_thread_id),
                 discussion_thread_id=COALESCE(?2,discussion_thread_id),
                 setup_message_id=COALESCE(?3,setup_message_id),
                 graveyard_thread_id=COALESCE(?4,graveyard_thread_id),
                 mafia_intro_message_id=COALESCE(?5,mafia_intro_message_id),
                 graveyard_intro_message_id=COALESCE(?6,graveyard_intro_message_id),
                 standings_message_id=COALESCE(?7,standings_message_id),
                 resolution_message_id=COALESCE(?8,resolution_message_id)
             WHERE game_id=?9",
            params![
                ids.mafia_thread_id,
                ids.discussion_thread_id,
                ids.setup_message_id,
                ids.graveyard_thread_id,
                ids.mafia_intro_message_id,
                ids.graveyard_intro_message_id,
                ids.standings_message_id,
                ids.resolution_message_id,
                game_id,
            ],
        )?;
        Ok(())
    }

    pub fn add_players(
        &self,
        game_id: i64,
        players: &[MafiaPlayer],
    ) -> Result<(), MafiaRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for player in players {
            insert_player(&transaction, game_id, player.guild_id, player)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn get_players(&self, game_id: i64) -> Result<Vec<MafiaPlayer>, MafiaRepositoryError> {
        query_players(
            &self.connection()?,
            "SELECT * FROM mafia_players WHERE game_id=?1 ORDER BY discord_id",
            params![game_id],
        )
    }

    pub fn get_player(
        &self,
        game_id: i64,
        discord_id: i64,
    ) -> Result<Option<MafiaPlayer>, MafiaRepositoryError> {
        self.connection()?
            .query_row(
                "SELECT * FROM mafia_players WHERE game_id=?1 AND discord_id=?2",
                params![game_id, discord_id],
                row_to_player,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_alive_players(
        &self,
        game_id: i64,
        role: Option<MafiaRole>,
    ) -> Result<Vec<MafiaPlayer>, MafiaRepositoryError> {
        let connection = self.connection()?;
        if let Some(role) = role {
            query_players(
                &connection,
                "SELECT * FROM mafia_players
                 WHERE game_id=?1 AND is_alive=1 AND role=?2 ORDER BY discord_id",
                params![game_id, role.as_str()],
            )
        } else {
            query_players(
                &connection,
                "SELECT * FROM mafia_players
                 WHERE game_id=?1 AND is_alive=1 ORDER BY discord_id",
                params![game_id],
            )
        }
    }

    pub fn set_player_alive(
        &self,
        game_id: i64,
        discord_id: i64,
        alive: bool,
        eliminated_phase: Option<MafiaPhase>,
    ) -> Result<(), MafiaRepositoryError> {
        self.connection()?.execute(
            "UPDATE mafia_players SET is_alive=?1, eliminated_phase=?2, eliminated_at=?3
             WHERE game_id=?4 AND discord_id=?5",
            params![
                i64::from(alive),
                eliminated_phase.map(MafiaPhase::as_str),
                (!alive).then(now_seconds),
                game_id,
                discord_id,
            ],
        )?;
        Ok(())
    }

    pub fn record_action(&self, action: &MafiaAction) -> Result<(), MafiaRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO mafia_actions (
                 game_id,guild_id,actor_id,target_id,action_type,phase,
                 day_number,created_at,result
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(game_id,actor_id,action_type,phase,day_number) DO UPDATE SET
                 target_id=excluded.target_id, created_at=excluded.created_at,
                 result=excluded.result",
            params![
                action.game_id,
                action.guild_id,
                action.actor_id,
                action.target_id,
                action.action_type.as_str(),
                action.phase.as_str(),
                action.day_number,
                now_seconds(),
                action.result,
            ],
        )?;
        transaction.execute(
            "UPDATE mafia_players SET acted=1 WHERE game_id=?1 AND discord_id=?2",
            params![action.game_id, action.actor_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn get_actions(
        &self,
        game_id: i64,
        action_type: Option<MafiaActionType>,
        phase: Option<MafiaPhase>,
    ) -> Result<Vec<MafiaAction>, MafiaRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT game_id,guild_id,actor_id,target_id,action_type,phase,day_number,result
             FROM mafia_actions
             WHERE game_id=?1 AND (?2 IS NULL OR action_type=?2)
               AND (?3 IS NULL OR phase=?3)
             ORDER BY created_at, action_id",
        )?;
        let rows = statement.query_map(
            params![
                game_id,
                action_type.map(MafiaActionType::as_str),
                phase.map(MafiaPhase::as_str),
            ],
            |row| {
                Ok(MafiaAction {
                    game_id: row.get(0)?,
                    guild_id: row.get(1)?,
                    actor_id: row.get(2)?,
                    target_id: row.get(3)?,
                    action_type: row.get(4)?,
                    phase: row.get(5)?,
                    day_number: row.get(6)?,
                    result: row.get(7)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_action_for_actor(
        &self,
        game_id: i64,
        actor_id: i64,
        action_type: MafiaActionType,
        day_number: Option<i64>,
    ) -> Result<Option<MafiaAction>, MafiaRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT game_id,guild_id,actor_id,target_id,action_type,phase,
                        day_number,result
                 FROM mafia_actions
                 WHERE game_id=?1 AND actor_id=?2 AND action_type=?3
                   AND (?4 IS NULL OR day_number=?4)
                 ORDER BY created_at DESC, action_id DESC LIMIT 1",
                params![game_id, actor_id, action_type.as_str(), day_number],
                |row| {
                    Ok(MafiaAction {
                        game_id: row.get(0)?,
                        guild_id: row.get(1)?,
                        actor_id: row.get(2)?,
                        target_id: row.get(3)?,
                        action_type: row.get(4)?,
                        phase: row.get(5)?,
                        day_number: row.get(6)?,
                        result: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Toggle opt-out and cancel a pending signup in one immediate transaction.
    pub fn set_optout(
        &self,
        guild_id: Option<i64>,
        discord_id: i64,
        opted_out: bool,
    ) -> Result<(), MafiaRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if opted_out {
            transaction.execute(
                "INSERT OR IGNORE INTO mafia_optout(discord_id,guild_id) VALUES (?1,?2)",
                params![discord_id, guild_id],
            )?;
            transaction.execute(
                "DELETE FROM mafia_signups
                 WHERE guild_id=?1 AND week_start=?2 AND discord_id=?3",
                params![guild_id, SIGNUP_BUCKET, discord_id],
            )?;
        } else {
            transaction.execute(
                "DELETE FROM mafia_optout WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, guild_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn is_opted_out(
        &self,
        guild_id: Option<i64>,
        discord_id: i64,
    ) -> Result<bool, MafiaRepositoryError> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT 1 FROM mafia_optout WHERE discord_id=?1 AND guild_id=?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn get_opted_out_player_ids(
        &self,
        guild_id: Option<i64>,
    ) -> Result<BTreeSet<i64>, MafiaRepositoryError> {
        let connection = self.connection()?;
        let mut statement =
            connection.prepare("SELECT discord_id FROM mafia_optout WHERE guild_id=?1")?;
        Ok(statement
            .query_map([Self::normalize_guild_id(guild_id)], |row| row.get(0))?
            .collect::<Result<_, _>>()?)
    }

    pub fn add_signup(
        &self,
        guild_id: Option<i64>,
        discord_id: i64,
    ) -> Result<(), MafiaRepositoryError> {
        self.connection()?.execute(
            "INSERT OR IGNORE INTO mafia_signups(guild_id,week_start,discord_id,created_at)
             VALUES (?1,?2,?3,?4)",
            params![
                Self::normalize_guild_id(guild_id),
                SIGNUP_BUCKET,
                discord_id,
                now_seconds(),
            ],
        )?;
        Ok(())
    }

    pub fn get_signups(&self, guild_id: Option<i64>) -> Result<Vec<i64>, MafiaRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id FROM mafia_signups
             WHERE guild_id=?1 AND week_start=?2 ORDER BY created_at,discord_id",
        )?;
        Ok(statement
            .query_map(
                params![Self::normalize_guild_id(guild_id), SIGNUP_BUCKET],
                |row| row.get(0),
            )?
            .collect::<Result<_, _>>()?)
    }

    pub fn get_eligible_signups(
        &self,
        guild_id: Option<i64>,
        entry_fee: i64,
        max_debt: Option<i64>,
    ) -> Result<Vec<i64>, MafiaRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT s.discord_id FROM mafia_signups s
             JOIN players p ON p.discord_id=s.discord_id AND p.guild_id=s.guild_id
             LEFT JOIN mafia_optout o
               ON o.discord_id=s.discord_id AND o.guild_id=s.guild_id
             WHERE s.guild_id=?1 AND s.week_start=?2 AND o.discord_id IS NULL
               AND (?3 IS NULL OR COALESCE(p.jopacoin_balance,0)-?4 >= -?3)
             ORDER BY s.created_at,s.discord_id",
        )?;
        Ok(statement
            .query_map(
                params![
                    Self::normalize_guild_id(guild_id),
                    SIGNUP_BUCKET,
                    max_debt,
                    entry_fee,
                ],
                |row| row.get(0),
            )?
            .collect::<Result<_, _>>()?)
    }

    pub fn remove_signups(
        &self,
        guild_id: Option<i64>,
        discord_ids: &[i64],
    ) -> Result<(), MafiaRepositoryError> {
        let unique = discord_ids.iter().copied().collect::<BTreeSet<_>>();
        if unique.is_empty() {
            return Ok(());
        }
        let placeholders = (1..=unique.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "DELETE FROM mafia_signups WHERE guild_id=? AND week_start=?
             AND discord_id IN ({placeholders})"
        );
        let mut bindings = vec![
            Value::Integer(Self::normalize_guild_id(guild_id)),
            Value::Text(SIGNUP_BUCKET.to_owned()),
        ];
        bindings.extend(unique.into_iter().map(Value::Integer));
        self.connection()?
            .execute(&sql, params_from_iter(bindings))?;
        Ok(())
    }

    pub fn finalize_game(
        &self,
        game_id: i64,
        winner: MafiaWinner,
        payout_per_winner: i64,
        mvp_id: Option<i64>,
    ) -> Result<(), MafiaRepositoryError> {
        self.connection()?.execute(
            "UPDATE mafia_games SET phase='RESOLVED',winner=?1,payout_per_winner=?2,
                 mvp_id=?3,day_ended_at=?4 WHERE game_id=?5",
            params![
                winner.as_str(),
                payout_per_winner,
                mvp_id,
                now_seconds(),
                game_id
            ],
        )?;
        Ok(())
    }

    pub fn cancel_game(
        &self,
        game_id: i64,
        refund: bool,
        ended_at: i64,
    ) -> Result<MafiaCancelResult, MafiaRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some((guild_id, entry_fee, phase)) = transaction
            .query_row(
                "SELECT guild_id,entry_fee,phase FROM mafia_games WHERE game_id=?1",
                [game_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, MafiaPhase>(2)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(MafiaCancelResult {
                reason: Some("game_not_found"),
                ..MafiaCancelResult::default()
            });
        };
        if phase == MafiaPhase::Resolved {
            return Ok(MafiaCancelResult {
                reason: Some("already_resolved"),
                ..MafiaCancelResult::default()
            });
        }
        let mut refunded = BTreeMap::new();
        if refund && entry_fee > 0 {
            let ids = transaction
                .prepare("SELECT discord_id FROM mafia_players WHERE game_id=?1")?
                .query_map([game_id], |row| row.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            set_ledger_context(
                &transaction,
                "mafia_abort_refund",
                None,
                game_id,
                "mafia game aborted - entry fee refunded",
            )?;
            for discord_id in ids {
                if transaction.execute(
                    "UPDATE players SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                         updated_at=CURRENT_TIMESTAMP WHERE discord_id=?2 AND guild_id=?3",
                    params![entry_fee, discord_id, guild_id],
                )? == 1
                {
                    refunded.insert(discord_id, entry_fee);
                }
            }
            clear_ledger_context(&transaction)?;
        }
        transaction.execute(
            "UPDATE mafia_games SET phase='RESOLVED',status='CANCELLED',winner='NONE',day_ended_at=?1
             WHERE game_id=?2 AND phase!='RESOLVED'",
            params![ended_at, game_id],
        )?;
        transaction.commit()?;
        Ok(MafiaCancelResult {
            applied: true,
            reason: None,
            refunded,
        })
    }

    /// Atomically settle a terminal DAY. The caller computes winner/payout
    /// policy; this method owns the phase claim, deaths, Bookie/bounty effects,
    /// wallet writes, overflow, and optional economy penalties.
    #[allow(clippy::too_many_arguments)]
    pub fn finalize_day_resolution(
        &self,
        game_id: i64,
        winner: MafiaWinner,
        payout_per_winner: i64,
        mvp_id: Option<i64>,
        lynched_id: Option<i64>,
        payout_deltas: &BTreeMap<i64, i64>,
        entry_fee: i64,
        nonprofit_overflow: i64,
        ended_at: i64,
        day_number: Option<i64>,
        lynched_was_mafia: bool,
        alive_count: i64,
        bankruptcy_penalty_rate: Option<f64>,
        vanity_tax_rate: f64,
        vanity_taxable_ids: &BTreeSet<i64>,
        low_priority_tax_rate: f64,
        low_priority_taxable_ids: &BTreeSet<i64>,
    ) -> Result<MafiaFinalizeResult, MafiaRepositoryError> {
        self.finalize_day_resolution_inner(
            game_id,
            winner,
            payout_per_winner,
            mvp_id,
            lynched_id,
            payout_deltas,
            entry_fee,
            nonprofit_overflow,
            ended_at,
            day_number,
            lynched_was_mafia,
            alive_count,
            bankruptcy_penalty_rate,
            vanity_tax_rate,
            vanity_taxable_ids,
            low_priority_tax_rate,
            low_priority_taxable_ids,
            None::<fn(&MafiaFinalizeResult) -> Result<String, MafiaRepositoryError>>,
        )
    }

    /// Atomically settle a terminal DAY and write its exact resolution receipt
    /// before committing the same SQLite transaction. The factory runs after
    /// all payout, bounty, bankruptcy, and tax amounts are known, but
    /// before `COMMIT`, so a crash cannot leave a settled game without the
    /// copy needed for deterministic READY replay.
    #[allow(clippy::too_many_arguments)]
    pub fn finalize_day_resolution_with_receipt<F>(
        &self,
        game_id: i64,
        winner: MafiaWinner,
        payout_per_winner: i64,
        mvp_id: Option<i64>,
        lynched_id: Option<i64>,
        payout_deltas: &BTreeMap<i64, i64>,
        entry_fee: i64,
        nonprofit_overflow: i64,
        ended_at: i64,
        day_number: Option<i64>,
        lynched_was_mafia: bool,
        alive_count: i64,
        bankruptcy_penalty_rate: Option<f64>,
        vanity_tax_rate: f64,
        vanity_taxable_ids: &BTreeSet<i64>,
        low_priority_tax_rate: f64,
        low_priority_taxable_ids: &BTreeSet<i64>,
        receipt_factory: F,
    ) -> Result<MafiaFinalizeResult, MafiaRepositoryError>
    where
        F: FnOnce(&MafiaFinalizeResult) -> Result<String, MafiaRepositoryError>,
    {
        self.finalize_day_resolution_inner(
            game_id,
            winner,
            payout_per_winner,
            mvp_id,
            lynched_id,
            payout_deltas,
            entry_fee,
            nonprofit_overflow,
            ended_at,
            day_number,
            lynched_was_mafia,
            alive_count,
            bankruptcy_penalty_rate,
            vanity_tax_rate,
            vanity_taxable_ids,
            low_priority_tax_rate,
            low_priority_taxable_ids,
            Some(receipt_factory),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finalize_day_resolution_inner<F>(
        &self,
        game_id: i64,
        winner: MafiaWinner,
        payout_per_winner: i64,
        mvp_id: Option<i64>,
        lynched_id: Option<i64>,
        payout_deltas: &BTreeMap<i64, i64>,
        entry_fee: i64,
        nonprofit_overflow: i64,
        ended_at: i64,
        day_number: Option<i64>,
        lynched_was_mafia: bool,
        alive_count: i64,
        bankruptcy_penalty_rate: Option<f64>,
        vanity_tax_rate: f64,
        vanity_taxable_ids: &BTreeSet<i64>,
        low_priority_tax_rate: f64,
        low_priority_taxable_ids: &BTreeSet<i64>,
        receipt_factory: Option<F>,
    ) -> Result<MafiaFinalizeResult, MafiaRepositoryError>
    where
        F: FnOnce(&MafiaFinalizeResult) -> Result<String, MafiaRepositoryError>,
    {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(guild_id) = transaction
            .query_row(
                "SELECT guild_id FROM mafia_games WHERE game_id=?1",
                [game_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        else {
            return Ok(MafiaFinalizeResult {
                reason: Some("game_not_found"),
                ..MafiaFinalizeResult::default()
            });
        };
        let changed = transaction.execute(
            "UPDATE mafia_games SET phase='RESOLVED',winner=?1,payout_per_winner=?2,
                 mvp_id=?3,day_ended_at=?4
             WHERE game_id=?5 AND phase='DAY'",
            params![
                winner.as_str(),
                payout_per_winner,
                mvp_id,
                ended_at,
                game_id
            ],
        )?;
        if changed != 1 {
            return Ok(MafiaFinalizeResult {
                reason: Some("already_resolved"),
                ..MafiaFinalizeResult::default()
            });
        }
        if let Some(lynched_id) = lynched_id {
            transaction.execute(
                "UPDATE mafia_players SET is_alive=0,eliminated_phase='DAY',eliminated_at=?1
                 WHERE game_id=?2 AND discord_id=?3 AND is_alive=1",
                params![ended_at, game_id, lynched_id],
            )?;
        }
        let bounty = if let Some(day_number) = day_number {
            mark_bookie_wager_hit(&transaction, game_id, day_number, lynched_id)?;
            resolve_bounties(
                &transaction,
                game_id,
                guild_id,
                day_number,
                lynched_id,
                lynched_was_mafia,
                alive_count,
            )?
        } else {
            BountyResolution::default()
        };
        let mut penalties = BTreeMap::new();
        let mut taxes = BTreeMap::new();
        let mut low_priority_taxes = BTreeMap::new();
        if !payout_deltas.is_empty() {
            set_ledger_context_with_metadata(
                &transaction,
                "mafia_payout",
                None,
                game_id,
                "mafia pot payout",
                &format!(
                    "{{\"winner\":\"{}\",\"entry_fee\":{},\"payout_per_winner\":{}}}",
                    winner.as_str(),
                    entry_fee,
                    payout_per_winner
                ),
            )?;
            let payout_result: Result<(), MafiaRepositoryError> = (|| {
                for (&discord_id, &amount) in payout_deltas {
                    if amount <= 0 {
                        continue;
                    }
                    if transaction.execute(
                        "UPDATE players SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                             updated_at=CURRENT_TIMESTAMP WHERE discord_id=?2 AND guild_id=?3",
                        params![amount, discord_id, guild_id],
                    )? != 1
                    {
                        return Err(MafiaRepositoryError::Invalid("payout_player_missing"));
                    }
                }
                Ok(())
            })();
            clear_ledger_context(&transaction)?;
            payout_result?;
        }
        if nonprofit_overflow > 0 {
            transaction.execute(
                "INSERT INTO nonprofit_fund(guild_id,total_collected,updated_at)
                 VALUES (?1,?2,CURRENT_TIMESTAMP)
                 ON CONFLICT(guild_id) DO UPDATE SET
                   total_collected=total_collected+excluded.total_collected,
                   updated_at=CURRENT_TIMESTAMP",
                params![guild_id, nonprofit_overflow],
            )?;
        }
        // Python's penalty uses the configured kept-rate and durable
        // bankruptcy_state. Keep the same fail-soft behavior when that table
        // has no active penalty row.
        if let Some(kept_rate) = bankruptcy_penalty_rate {
            let kept_rate = kept_rate.clamp(0.0, 1.0);
            set_ledger_context_with_metadata(
                &transaction,
                "mafia_bankruptcy_penalty",
                None,
                game_id,
                "mafia bankruptcy penalty",
                &format!(
                    "{{\"winner\":\"{}\",\"entry_fee\":{}}}",
                    winner.as_str(),
                    entry_fee
                ),
            )?;
            let penalty_result: Result<(), MafiaRepositoryError> = (|| {
                for (&discord_id, &amount) in payout_deltas {
                    let profit = (amount - entry_fee).max(0);
                    if profit == 0 {
                        continue;
                    }
                    let active = transaction
                        .query_row(
                            "SELECT COALESCE(penalty_games_remaining,0) FROM bankruptcy_state
                             WHERE discord_id=?1 AND guild_id=?2",
                            params![discord_id, guild_id],
                            |row| row.get::<_, i64>(0),
                        )
                        .optional()?
                        .unwrap_or(0)
                        > 0;
                    if !active {
                        continue;
                    }
                    let penalty = ((profit as f64) * (1.0 - kept_rate)) as i64;
                    if penalty <= 0 {
                        continue;
                    }
                    let changed = transaction.execute(
                        "UPDATE players SET jopacoin_balance=COALESCE(jopacoin_balance,0)-?1,
                             updated_at=CURRENT_TIMESTAMP WHERE discord_id=?2 AND guild_id=?3",
                        params![penalty, discord_id, guild_id],
                    )?;
                    if changed != 1 {
                        return Err(MafiaRepositoryError::Invalid("penalty_player_missing"));
                    }
                    penalties.insert(discord_id, penalty);
                }
                Ok(())
            })();
            clear_ledger_context(&transaction)?;
            penalty_result?;
        }
        if vanity_tax_rate > 0.0 {
            let rate = vanity_tax_rate.clamp(0.0, 1.0);
            set_ledger_context_with_metadata(
                &transaction,
                "vanity_tax",
                None,
                game_id,
                "vanity tax on JC profit",
                &format!(
                    "{{\"winner\":\"{}\",\"entry_fee\":{}}}",
                    winner.as_str(),
                    entry_fee
                ),
            )?;
            let tax_result: Result<(), MafiaRepositoryError> = (|| {
                for (&discord_id, &amount) in payout_deltas {
                    if !vanity_taxable_ids.contains(&discord_id) {
                        continue;
                    }
                    let profit = (amount - entry_fee).max(0);
                    let tax = ((profit as f64) * rate) as i64;
                    if tax <= 0 {
                        continue;
                    }
                    let changed = transaction.execute(
                        "UPDATE players SET jopacoin_balance=COALESCE(jopacoin_balance,0)-?1,
                             updated_at=CURRENT_TIMESTAMP WHERE discord_id=?2 AND guild_id=?3",
                        params![tax, discord_id, guild_id],
                    )?;
                    if changed != 1 {
                        return Err(MafiaRepositoryError::Invalid("vanity_tax_player_missing"));
                    }
                    taxes.insert(discord_id, tax);
                }
                Ok(())
            })();
            clear_ledger_context(&transaction)?;
            tax_result?;
        }
        if low_priority_tax_rate > 0.0 {
            let rate = low_priority_tax_rate.clamp(0.0, 1.0);
            set_ledger_context_with_metadata(
                &transaction,
                "low_priority_tax",
                None,
                game_id,
                "low priority tax on JC profit",
                &format!(
                    "{{\"winner\":\"{}\",\"entry_fee\":{}}}",
                    winner.as_str(),
                    entry_fee
                ),
            )?;
            let tax_result: Result<(), MafiaRepositoryError> = (|| {
                for (&discord_id, &amount) in payout_deltas {
                    if !low_priority_taxable_ids.contains(&discord_id) {
                        continue;
                    }
                    let profit = (amount - entry_fee).max(0);
                    // The penalty and the vanity tax are independent rates on
                    // this same profit, so cap the combined withholding at the
                    // profit instead of letting the payout go net negative.
                    let withheld = penalties.get(&discord_id).copied().unwrap_or(0)
                        + taxes.get(&discord_id).copied().unwrap_or(0);
                    let tax = (((profit as f64) * rate) as i64).min(profit - withheld);
                    if tax <= 0 {
                        continue;
                    }
                    let changed = transaction.execute(
                        "UPDATE players SET jopacoin_balance=COALESCE(jopacoin_balance,0)-?1,
                             updated_at=CURRENT_TIMESTAMP WHERE discord_id=?2 AND guild_id=?3",
                        params![tax, discord_id, guild_id],
                    )?;
                    if changed != 1 {
                        return Err(MafiaRepositoryError::Invalid(
                            "low_priority_tax_player_missing",
                        ));
                    }
                    low_priority_taxes.insert(discord_id, tax);
                }
                Ok(())
            })();
            clear_ledger_context(&transaction)?;
            tax_result?;
        }
        let result = MafiaFinalizeResult {
            applied: true,
            reason: None,
            bounty,
            bankruptcy_penalties: penalties,
            vanity_taxes: taxes,
            low_priority_taxes,
        };
        if let Some(receipt_factory) = receipt_factory {
            let payload = receipt_factory(&result)?;
            transaction.execute(
                "UPDATE mafia_games SET resolution_summary=?1
                 WHERE game_id=?2 AND resolution_summary IS NULL",
                params![payload, game_id],
            )?;
        }
        transaction.commit()?;
        Ok(result)
    }

    pub fn get_leaderboard(
        &self,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<MafiaLeaderboardRow>, MafiaRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(LEADERBOARD_SQL)?;
        Ok(statement
            .query_map(params![Self::normalize_guild_id(guild_id), limit], |row| {
                Ok(MafiaLeaderboardRow {
                    discord_id: row.get(0)?,
                    games_played: row.get(1)?,
                    wins: row.get(2)?,
                    mafia_wins: row.get(3)?,
                    town_wins: row.get(4)?,
                    jester_wins: row.get(5)?,
                    mvp_count: row.get(6)?,
                })
            })?
            .collect::<Result<_, _>>()?)
    }

    pub fn compute_player_stats(
        &self,
        guild_id: Option<i64>,
        discord_id: i64,
    ) -> Result<MafiaPlayerStats, MafiaRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut stats =
            connection.query_row(PLAYER_STATS_SQL, params![discord_id, guild_id], |row| {
                Ok(MafiaPlayerStats {
                    games_played: row.get(0)?,
                    wins: row.get(1)?,
                    mafia_wins: row.get(2)?,
                    town_wins: row.get(3)?,
                    jester_wins: row.get(4)?,
                    ..MafiaPlayerStats::default()
                })
            })?;
        stats.mafia_kills = count_stat(&connection, MAFIA_KILLS_SQL, discord_id, guild_id)?;
        stats.doctor_saves = count_stat(&connection, DOCTOR_SAVES_SQL, discord_id, guild_id)?;
        stats.correct_reads = count_stat(&connection, CORRECT_READS_SQL, discord_id, guild_id)?;
        Ok(stats)
    }

    pub fn get_player_history(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<MafiaHistoryRow>, MafiaRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT g.game_id,g.game_date,g.winner,g.twist_event,
                    g.payout_per_winner,g.mvp_id,p.role,p.is_godfather,
                    p.hero_name,p.is_alive,p.eliminated_phase,p.acted
             FROM mafia_players p JOIN mafia_games g ON g.game_id=p.game_id
             WHERE p.discord_id=?1 AND p.guild_id=?2 AND g.phase='RESOLVED'
             ORDER BY g.game_id DESC LIMIT ?3",
        )?;
        Ok(statement
            .query_map(
                params![discord_id, Self::normalize_guild_id(guild_id), limit],
                |row| {
                    Ok(MafiaHistoryRow {
                        game_id: row.get(0)?,
                        game_date: row.get(1)?,
                        winner: row.get(2)?,
                        twist_event: row.get(3)?,
                        payout_per_winner: row.get(4)?,
                        mvp_id: row.get(5)?,
                        role: row.get(6)?,
                        is_godfather: row.get::<_, i64>(7)? != 0,
                        hero_name: row.get(8)?,
                        is_alive: row.get::<_, i64>(9)? != 0,
                        eliminated_phase: row.get(10)?,
                        acted: row.get::<_, i64>(11)? != 0,
                    })
                },
            )?
            .collect::<Result<_, _>>()?)
    }

    /// Debit one coin, park it in the guild fund, and insert the unique stake
    /// under one immediate transaction.
    pub fn add_bounty(
        &self,
        game_id: i64,
        guild_id: Option<i64>,
        day_number: i64,
        target_id: i64,
        contributor_id: i64,
        max_debt: i64,
    ) -> Result<AddBountyResult, MafiaRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let duplicate = transaction
            .query_row(
                "SELECT 1 FROM mafia_bounties
                 WHERE game_id=?1 AND day_number=?2 AND target_id=?3 AND contributor_id=?4",
                params![game_id, day_number, target_id, contributor_id],
                |_| Ok(()),
            )
            .optional()?;
        if duplicate.is_some() {
            return Ok(AddBountyResult::AlreadyStaked);
        }
        set_ledger_context(
            &transaction,
            "mafia_bounty_stake",
            Some(contributor_id),
            game_id,
            "mafia town bounty stake",
        )?;
        let debited = transaction.execute(
            "UPDATE players SET jopacoin_balance=COALESCE(jopacoin_balance,0)-1,
                 updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?1 AND guild_id=?2
               AND COALESCE(jopacoin_balance,0)-1 >= ?3",
            params![contributor_id, guild_id, -max_debt],
        )?;
        clear_ledger_context(&transaction)?;
        if debited != 1 {
            return Ok(AddBountyResult::InsufficientFunds);
        }
        transaction.execute(
            "INSERT INTO nonprofit_fund(guild_id,total_collected,updated_at)
             VALUES (?1,1,CURRENT_TIMESTAMP)
             ON CONFLICT(guild_id) DO UPDATE SET
                 total_collected=total_collected+1,updated_at=CURRENT_TIMESTAMP",
            [guild_id],
        )?;
        transaction.execute(
            "INSERT INTO mafia_bounties
             (game_id,guild_id,day_number,target_id,contributor_id,amount,created_at)
             VALUES (?1,?2,?3,?4,?5,1,?6)",
            params![
                game_id,
                guild_id,
                day_number,
                target_id,
                contributor_id,
                now_seconds()
            ],
        )?;
        transaction.commit()?;
        Ok(AddBountyResult::Added)
    }

    pub fn bounty_count(&self, game_id: i64, day_number: i64) -> Result<i64, MafiaRepositoryError> {
        Ok(self.connection()?.query_row(
            "SELECT COUNT(*) FROM mafia_bounties WHERE game_id=?1 AND day_number=?2",
            params![game_id, day_number],
            |row| row.get(0),
        )?)
    }

    /// Resolve a correct-read bounty without allowing its parked stake to
    /// consume unrelated nonprofit revenue.
    pub fn resolve_day_bounties(
        &self,
        game_id: i64,
        guild_id: Option<i64>,
        day_number: i64,
        lynched_id: Option<i64>,
        lynched_was_mafia: bool,
        alive_count: i64,
    ) -> Result<BountyResolution, MafiaRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = resolve_bounties(
            &transaction,
            game_id,
            Self::normalize_guild_id(guild_id),
            day_number,
            lynched_id,
            lynched_was_mafia,
            alive_count,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn player_balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, MafiaRepositoryError> {
        Ok(self.connection()?.query_row(
            "SELECT COALESCE(jopacoin_balance,0) FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, Self::normalize_guild_id(guild_id)],
            |row| row.get(0),
        )?)
    }

    /// Charge the one-jopacoin wrong-channel penalty used by the Python gate.
    ///
    /// The penalty intentionally has no balance floor: `adjust_balance(-1)` in
    /// the Python player service records the charge even for a zero-balance
    /// player.  A missing player is a no-op and returns `false`.
    pub fn charge_channel_penalty(
        &self,
        guild_id: Option<i64>,
        discord_id: i64,
    ) -> Result<bool, MafiaRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        set_ledger_context(
            &transaction,
            "mafia_channel_penalty",
            Some(discord_id),
            0,
            "mafia wrong-channel penalty",
        )?;
        let changed = transaction.execute(
            "UPDATE players
             SET jopacoin_balance=COALESCE(jopacoin_balance,0)-1,
                 updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
        )?;
        clear_ledger_context(&transaction)?;
        transaction.commit()?;
        Ok(changed == 1)
    }

    pub fn nonprofit_fund(&self, guild_id: Option<i64>) -> Result<i64, MafiaRepositoryError> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id=?1",
                [Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }
}

fn row_to_game(row: &Row<'_>) -> Result<MafiaGame, rusqlite::Error> {
    Ok(MafiaGame {
        game_id: row.get("game_id")?,
        guild_id: row.get("guild_id")?,
        game_date: row.get("game_date")?,
        phase: row.get("phase")?,
        started_at: row.get("started_at")?,
        roster_size: row.get("roster_size")?,
        twist_event: row.get("twist_event")?,
        night_ended_at: row.get("night_ended_at")?,
        day_ended_at: row.get("day_ended_at")?,
        winner: row.get("winner")?,
        entry_fee: row.get("entry_fee")?,
        payout_per_winner: row.get("payout_per_winner")?,
        mvp_id: row.get("mvp_id")?,
        mafia_thread_id: row.get("mafia_thread_id")?,
        discussion_thread_id: row.get("discussion_thread_id")?,
        setup_message_id: row.get("setup_message_id")?,
        day_number: row.get("day_number")?,
        phase_started_at: row.get("phase_started_at")?,
        standings_message_id: row.get("standings_message_id")?,
        graveyard_thread_id: row.get("graveyard_thread_id")?,
        mafia_intro_message_id: row.get("mafia_intro_message_id")?,
        graveyard_intro_message_id: row.get("graveyard_intro_message_id")?,
        resolution_message_id: row.get("resolution_message_id")?,
        resolution_summary: row.get("resolution_summary")?,
        status: row.get("status")?,
    })
}

fn row_to_player(row: &Row<'_>) -> Result<MafiaPlayer, rusqlite::Error> {
    Ok(MafiaPlayer {
        game_id: row.get("game_id")?,
        discord_id: row.get("discord_id")?,
        guild_id: row.get("guild_id")?,
        role: row.get("role")?,
        is_godfather: row.get::<_, i64>("is_godfather")? != 0,
        hero_name: row.get("hero_name")?,
        is_alive: row.get::<_, i64>("is_alive")? != 0,
        eliminated_phase: row.get("eliminated_phase")?,
        eliminated_at: row.get("eliminated_at")?,
        acted: row.get::<_, i64>("acted")? != 0,
    })
}

fn insert_player(
    transaction: &Transaction<'_>,
    game_id: i64,
    guild_id: i64,
    player: &MafiaPlayer,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO mafia_players (
             game_id,discord_id,guild_id,role,is_godfather,hero_name,
             is_alive,eliminated_phase,eliminated_at,acted
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            game_id,
            player.discord_id,
            guild_id,
            player.role.as_str(),
            i64::from(player.is_godfather),
            player.hero_name,
            i64::from(player.is_alive),
            player.eliminated_phase.map(MafiaPhase::as_str),
            player.eliminated_at,
            i64::from(player.acted),
        ],
    )?;
    Ok(())
}

fn query_players<P>(
    connection: &Connection,
    sql: &str,
    parameters: P,
) -> Result<Vec<MafiaPlayer>, MafiaRepositoryError>
where
    P: rusqlite::Params,
{
    let mut statement = connection.prepare(sql)?;
    Ok(statement
        .query_map(parameters, row_to_player)?
        .collect::<Result<_, _>>()?)
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    source: &str,
    actor_id: Option<i64>,
    game_id: i64,
    reason: &str,
) -> Result<(), rusqlite::Error> {
    set_ledger_context_with_metadata(transaction, source, actor_id, game_id, reason, "{}")
}

fn set_ledger_context_with_metadata(
    transaction: &Transaction<'_>,
    source: &str,
    actor_id: Option<i64>,
    game_id: i64,
    reason: &str,
    metadata: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context
         (id,source,actor_id,related_type,related_id,reason,metadata)
         VALUES (1,?1,?2,'mafia_game',?3,?4,?5)",
        params![source, actor_id, game_id.to_string(), reason, metadata],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn mark_bookie_wager_hit(
    transaction: &Transaction<'_>,
    game_id: i64,
    day_number: i64,
    lynched_id: Option<i64>,
) -> Result<(), rusqlite::Error> {
    let Some(lynched_id) = lynched_id else {
        return Ok(());
    };
    transaction.execute(
        "UPDATE mafia_actions SET result='HIT'
         WHERE game_id=?1 AND day_number=?2 AND action_type='WAGER'
           AND phase='NIGHT' AND target_id=?3 AND COALESCE(result,'')!='HIT'",
        params![game_id, day_number, lynched_id],
    )?;
    Ok(())
}

fn resolve_bounties(
    transaction: &Transaction<'_>,
    game_id: i64,
    guild_id: i64,
    day_number: i64,
    lynched_id: Option<i64>,
    lynched_was_mafia: bool,
    alive_count: i64,
) -> Result<BountyResolution, rusqlite::Error> {
    let Some(lynched_id) = lynched_id.filter(|_| lynched_was_mafia) else {
        return Ok(BountyResolution::default());
    };
    let mut statement = transaction.prepare(
        "SELECT contributor_id,amount FROM mafia_bounties
         WHERE game_id=?1 AND day_number=?2 AND target_id=?3 ORDER BY rowid",
    )?;
    let rows = statement
        .query_map(params![game_id, day_number, lynched_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    if rows.is_empty() {
        return Ok(BountyResolution::default());
    }
    let parked = rows.iter().map(|(_, amount)| amount).sum::<i64>();
    let available = transaction
        .query_row(
            "SELECT total_collected FROM nonprofit_fund WHERE guild_id=?1",
            [guild_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0);
    let reward = alive_count.min(parked).min(available).max(0);
    if reward == 0 {
        return Ok(BountyResolution::default());
    }
    transaction.execute(
        "UPDATE nonprofit_fund SET total_collected=total_collected-?1,
             updated_at=CURRENT_TIMESTAMP WHERE guild_id=?2",
        params![reward, guild_id],
    )?;
    let base = reward / i64::try_from(rows.len()).unwrap_or(1);
    let dust = reward - base * i64::try_from(rows.len()).unwrap_or(1);
    set_ledger_context(
        transaction,
        "mafia_bounty_payout",
        None,
        game_id,
        "mafia town bounty payout",
    )?;
    let mut paid = BTreeMap::new();
    for (index, (contributor_id, _)) in rows.into_iter().enumerate() {
        let amount = base + if index == 0 { dust } else { 0 };
        if amount > 0
            && transaction.execute(
                "UPDATE players SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                     updated_at=CURRENT_TIMESTAMP WHERE discord_id=?2 AND guild_id=?3",
                params![amount, contributor_id, guild_id],
            )? == 1
        {
            paid.insert(contributor_id, amount);
        }
    }
    clear_ledger_context(transaction)?;
    Ok(BountyResolution { paid, reward })
}

fn count_stat(
    connection: &Connection,
    sql: &str,
    discord_id: i64,
    guild_id: i64,
) -> Result<i64, rusqlite::Error> {
    connection.query_row(sql, params![discord_id, guild_id], |row| row.get(0))
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

const LEADERBOARD_SQL: &str = "
SELECT p.discord_id,COUNT(*) AS games_played,
 SUM(CASE WHEN (g.winner='TOWN' AND p.role IN ('TOWNIE','DOCTOR','DETECTIVE','VIGILANTE'))
           OR (g.winner='MAFIA' AND p.role='MAFIA')
           OR (g.winner='JESTER' AND p.role='JESTER') THEN 1 ELSE 0 END) AS wins,
 SUM(CASE WHEN g.winner='MAFIA' AND p.role='MAFIA' THEN 1 ELSE 0 END),
 SUM(CASE WHEN g.winner='TOWN' AND p.role IN ('TOWNIE','DOCTOR','DETECTIVE','VIGILANTE') THEN 1 ELSE 0 END),
 SUM(CASE WHEN g.winner='JESTER' AND p.role='JESTER' THEN 1 ELSE 0 END),
 SUM(CASE WHEN g.mvp_id=p.discord_id THEN 1 ELSE 0 END)
FROM mafia_players p JOIN mafia_games g ON g.game_id=p.game_id
WHERE p.guild_id=?1 AND g.phase='RESOLVED'
GROUP BY p.discord_id ORDER BY wins DESC,games_played DESC LIMIT ?2";

const PLAYER_STATS_SQL: &str = "
SELECT COUNT(*),
 COALESCE(SUM(CASE WHEN (g.winner='TOWN' AND p.role IN ('TOWNIE','DOCTOR','DETECTIVE','VIGILANTE'))
           OR (g.winner='MAFIA' AND p.role='MAFIA')
           OR (g.winner='JESTER' AND p.role='JESTER') THEN 1 ELSE 0 END),0),
 COALESCE(SUM(CASE WHEN g.winner='MAFIA' AND p.role='MAFIA' THEN 1 ELSE 0 END),0),
 COALESCE(SUM(CASE WHEN g.winner='TOWN' AND p.role IN ('TOWNIE','DOCTOR','DETECTIVE','VIGILANTE') THEN 1 ELSE 0 END),0),
 COALESCE(SUM(CASE WHEN g.winner='JESTER' AND p.role='JESTER' THEN 1 ELSE 0 END),0)
FROM mafia_players p JOIN mafia_games g ON g.game_id=p.game_id
WHERE p.discord_id=?1 AND p.guild_id=?2 AND g.phase='RESOLVED'";

const MAFIA_KILLS_SQL: &str = "SELECT COUNT(*) FROM mafia_actions a
 JOIN mafia_players victim ON victim.game_id=a.game_id AND victim.discord_id=a.target_id
 JOIN mafia_games g ON g.game_id=a.game_id
 WHERE a.actor_id=?1 AND g.guild_id=?2 AND a.action_type='KILL'
   AND victim.eliminated_phase='NIGHT' AND g.phase='RESOLVED'";
const DOCTOR_SAVES_SQL: &str = "SELECT COUNT(*) FROM mafia_actions s
 JOIN mafia_players target ON target.game_id=s.game_id AND target.discord_id=s.target_id
 JOIN mafia_games g ON g.game_id=s.game_id
 WHERE s.actor_id=?1 AND g.guild_id=?2 AND s.action_type='SAVE'
   AND target.eliminated_phase IS NULL AND EXISTS (
     SELECT 1 FROM mafia_actions k WHERE k.game_id=s.game_id
       AND k.action_type='KILL' AND k.target_id=s.target_id)
   AND g.phase='RESOLVED'";
const CORRECT_READS_SQL: &str = "SELECT COUNT(*) FROM mafia_actions i
 JOIN mafia_players target ON target.game_id=i.game_id AND target.discord_id=i.target_id
 JOIN mafia_games g ON g.game_id=i.game_id
 WHERE i.actor_id=?1 AND g.guild_id=?2 AND i.action_type='INVESTIGATE'
   AND target.role='MAFIA' AND target.is_godfather=0
   AND target.eliminated_phase='DAY' AND g.phase='RESOLVED'";

#[cfg(test)]
#[path = "mafia_repository/tests.rs"]
mod tests;
