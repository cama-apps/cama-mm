//! Existing-schema persistence for the daily guild player-trivia game.
//!
//! Question generation is owned by `cama-app`; this adapter exposes the
//! deterministic, privacy-filtered guild snapshot and freezes generated
//! questions before Discord displays them. Cooldown claims and answer
//! settlement use `BEGIN IMMEDIATE`, matching Python's cross-process writer
//! boundary. Repository construction never creates or migrates schema.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use cama_domain::rating::cap_glicko_rd;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::json;
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlayerTriviaPlayer {
    pub discord_id: i64,
    pub username: String,
    pub wins: i64,
    pub losses: i64,
    pub balance: i64,
    pub lowest_balance_ever: Option<i64>,
    pub glicko_rating: Option<f64>,
    pub glicko_rd: Option<f64>,
    pub openskill_mu: Option<f64>,
    pub openskill_sigma: Option<f64>,
    pub last_match_unix: Option<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TipRow {
    pub sender_id: i64,
    pub recipient_id: i64,
    pub amount: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WheelSpinRow {
    pub spin_id: i64,
    pub discord_id: i64,
    pub spin_time: i64,
    pub outcome_code: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParticipantRow {
    pub discord_id: i64,
    pub match_id: i64,
    pub match_time: i64,
    pub hero_id: i64,
    pub won: bool,
    pub kills: Option<f64>,
    pub assists: Option<f64>,
    pub gpm: Option<f64>,
    pub hero_damage: Option<f64>,
    pub tower_damage: Option<f64>,
    pub fantasy_points: Option<f64>,
    pub lane_role: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RatingRow {
    pub history_id: i64,
    pub discord_id: i64,
    pub match_id: i64,
    pub rating: Option<f64>,
    pub rating_before: Option<f64>,
    pub match_time: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PairingRow {
    pub player1_id: i64,
    pub player2_id: i64,
    pub games_together: i64,
    pub wins_together: i64,
    pub games_against: i64,
    pub player1_wins_against: i64,
    pub last_match_id: Option<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DoubleSpinRow {
    pub spin_id: i64,
    pub discord_id: i64,
    pub cost: i64,
    pub balance_before: i64,
    pub balance_after: i64,
    pub won: bool,
    pub spin_time: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TriviaHistoryRow {
    pub discord_id: i64,
    pub streak: i64,
    pub jc_earned: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TunnelRow {
    pub discord_id: i64,
    pub total_digs: i64,
    pub max_depth: i64,
    pub total_jc_earned: i64,
    pub prestige_level: i64,
    pub best_run_score: i64,
    pub pickaxe_tier: i64,
    pub stat_strength: i64,
    pub stat_smarts: i64,
    pub stat_stamina: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BankruptcyRow {
    pub discord_id: i64,
    pub bankruptcy_count: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DisbursementVoteRow {
    pub proposal_id: i64,
    pub discord_id: i64,
    pub vote_method: String,
    pub proposal_outcome: String,
    pub finalized_at: Option<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PredictionPositionRow {
    pub prediction_id: i64,
    pub discord_id: i64,
    pub status: String,
    pub outcome: String,
    pub yes_contracts: i64,
    pub yes_cost_basis_total: i64,
    pub no_contracts: i64,
    pub no_cost_basis_total: i64,
    pub bankruptcy_penalty: i64,
    pub vanity_tax: i64,
    pub low_priority_tax: i64,
    pub question: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PredictionTradeRow {
    pub trade_id: i64,
    pub prediction_id: i64,
    pub discord_id: i64,
    pub action: String,
    pub contracts: i64,
    pub jopacoins: i64,
    pub trade_time: i64,
    pub status: String,
    pub outcome: String,
    pub resolved_at: Option<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SettledBetRow {
    pub bet_id: i64,
    pub discord_id: i64,
    pub match_id: i64,
    pub team_bet_on: String,
    pub winning_team: i64,
    pub amount: i64,
    pub leverage: i64,
    pub effective_bet: Option<i64>,
    pub payout: i64,
    pub settlement_vanity_tax: i64,
    pub settlement_low_priority_tax: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MafiaGameRow {
    pub game_id: i64,
    pub phase: String,
    pub status: String,
    pub winner: Option<String>,
    pub mvp_id: Option<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MafiaPlayerRow {
    pub game_id: i64,
    pub discord_id: i64,
    pub role: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigArtifactRow {
    pub row_id: i64,
    pub discord_id: i64,
    pub artifact_id: String,
    pub found_at: i64,
    pub is_relic: bool,
    pub equipped: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigAchievementRow {
    pub discord_id: i64,
    pub achievement_id: String,
    pub unlocked_at: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigActionRow {
    pub row_id: i64,
    pub actor_id: i64,
    pub target_id: Option<i64>,
    pub action_type: String,
    pub depth_before: Option<i64>,
    pub depth_after: Option<i64>,
    pub jc_delta: i64,
    pub created_at: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MafiaActionRow {
    pub action_id: i64,
    pub game_id: i64,
    pub actor_id: i64,
    pub target_id: Option<i64>,
    pub action_type: String,
    pub phase: String,
    pub day_number: i64,
    pub created_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecalibrationRow {
    pub discord_id: i64,
    pub last_recalibration_at: Option<i64>,
    pub total_recalibrations: i64,
    pub rating_at_recalibration: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlayerTriviaSnapshot {
    pub players: Vec<PlayerTriviaPlayer>,
    pub participants: Vec<ParticipantRow>,
    pub ratings: Vec<RatingRow>,
    pub pairings: Vec<PairingRow>,
    pub tips: Vec<TipRow>,
    pub wheel_spins: Vec<WheelSpinRow>,
    pub double_spins: Vec<DoubleSpinRow>,
    pub trivia_sessions: Vec<TriviaHistoryRow>,
    pub tunnels: Vec<TunnelRow>,
    pub bankruptcies: Vec<BankruptcyRow>,
    pub disbursement_votes: Vec<DisbursementVoteRow>,
    pub prediction_positions: Vec<PredictionPositionRow>,
    pub prediction_trades: Vec<PredictionTradeRow>,
    pub bets: Vec<SettledBetRow>,
    pub dig_artifacts: Vec<DigArtifactRow>,
    pub dig_achievements: Vec<DigAchievementRow>,
    pub dig_actions: Vec<DigActionRow>,
    pub mafia_games: Vec<MafiaGameRow>,
    pub mafia_players: Vec<MafiaPlayerRow>,
    pub mafia_actions: Vec<MafiaActionRow>,
    pub recalibrations: Vec<RecalibrationRow>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerTriviaQuestionRecord {
    pub question_key: String,
    pub category: String,
    pub question_text: String,
    pub options: [String; 4],
    pub correct_index: usize,
    pub explanation: String,
    pub spicy: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerTriviaQuestion {
    pub key: String,
    pub category: String,
    pub text: String,
    pub options: [String; 4],
    pub correct_index: usize,
    pub explanation: String,
    pub spicy: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerTriviaSettlement {
    pub is_correct: bool,
    pub reward: i64,
    pub score: i64,
    pub jc_earned: i64,
    pub completed: bool,
    pub new_balance: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredPlayerTriviaQuestion {
    pub question_number: i64,
    pub question: PlayerTriviaQuestion,
    pub selected_index: Option<usize>,
    pub is_correct: Option<bool>,
    pub reward: i64,
    pub answered_at: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredPlayerTriviaSession {
    pub session_id: i64,
    pub guild_id: i64,
    pub discord_id: i64,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub status: String,
    pub question_count: i64,
    pub score: i64,
    pub jc_earned: i64,
    pub questions: Vec<StoredPlayerTriviaQuestion>,
}

impl StoredPlayerTriviaSession {
    #[must_use]
    pub fn next_question(&self) -> Option<&StoredPlayerTriviaQuestion> {
        self.questions
            .iter()
            .find(|question| question.selected_index.is_none())
    }

    #[must_use]
    pub fn last_activity_at(&self) -> i64 {
        self.questions
            .iter()
            .filter_map(|question| question.answered_at)
            .max()
            .unwrap_or(self.started_at)
    }
}

#[derive(Debug, Error)]
pub enum PlayerTriviaRepositoryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("a player-trivia session requires at least one question")]
    EmptyQuestions,
    #[error("player-trivia cooldown cannot be negative")]
    NegativeCooldown,
    #[error("player-trivia reward cannot be negative")]
    NegativeReward,
    #[error("player-trivia question requires exactly four distinct options")]
    InvalidOptions,
    #[error("player-trivia correct index is outside the four frozen options")]
    InvalidCorrectIndex,
    #[error("player-trivia question key, category, and text must be non-empty")]
    InvalidQuestionShape,
    #[error("selected player-trivia answer is outside the four frozen options")]
    InvalidSelectedIndex,
    #[error("player for trivia session no longer exists")]
    MissingPlayer,
    #[error("unsupported player-trivia finish status {0:?}")]
    InvalidFinishStatus(String),
    #[error("stored player-trivia option index is outside the supported range")]
    StoredIndexOverflow,
    #[error("system clock is before the Unix epoch")]
    Clock,
}

#[derive(Clone, Debug)]
pub struct PlayerTriviaRepository {
    path: PathBuf,
}

impl PlayerTriviaRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    fn connection(&self) -> Result<Connection, PlayerTriviaRepositoryError> {
        Ok(open_runtime_connection(&self.path)?)
    }

    pub fn player_exists(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<bool, PlayerTriviaRepositoryError> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT 1 FROM players WHERE guild_id = ?1 AND discord_id = ?2",
                params![guild_id, discord_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn get_last_session_started(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<i64>, PlayerTriviaRepositoryError> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT started_at FROM player_trivia_sessions
             WHERE guild_id = ?1 AND discord_id = ?2 AND status != 'cancelled'
             ORDER BY started_at DESC, session_id DESC LIMIT 1",
                params![guild_id, discord_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn active_session_for_player(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<StoredPlayerTriviaSession>, PlayerTriviaRepositoryError> {
        let id = self
            .connection()?
            .query_row(
                "SELECT session_id FROM player_trivia_sessions
             WHERE guild_id = ?1 AND discord_id = ?2 AND status = 'active'
             ORDER BY started_at DESC, session_id DESC LIMIT 1",
                params![guild_id, discord_id],
                |row| row.get(0),
            )
            .optional()?;
        id.map(|id| self.get_session(id))
            .transpose()
            .map(Option::flatten)
    }

    pub fn get_session(
        &self,
        session_id: i64,
    ) -> Result<Option<StoredPlayerTriviaSession>, PlayerTriviaRepositoryError> {
        let connection = self.connection()?;
        let session = connection
            .query_row(
                "SELECT session_id, guild_id, discord_id, started_at, completed_at,
                    status, question_count, score, jc_earned
             FROM player_trivia_sessions WHERE session_id = ?1",
                [session_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            session_id,
            guild_id,
            discord_id,
            started_at,
            completed_at,
            status,
            question_count,
            score,
            jc_earned,
        )) = session
        else {
            return Ok(None);
        };
        let mut statement = connection.prepare(
            "SELECT question_number, question_key, category, spicy, question_text,
                    options_json, correct_index, COALESCE(explanation, ''), selected_index,
                    is_correct, reward, answered_at
             FROM player_trivia_questions WHERE session_id = ?1
             ORDER BY question_number ASC",
        )?;
        let rows = statement.query_map([session_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<bool>>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, Option<i64>>(11)?,
            ))
        })?;
        let mut questions = Vec::new();
        for row in rows {
            let (
                number,
                key,
                category,
                spicy,
                text,
                options_json,
                correct_index,
                explanation,
                selected_index,
                is_correct,
                reward,
                answered_at,
            ) = row?;
            let options: [String; 4] = serde_json::from_str::<Vec<String>>(&options_json)?
                .try_into()
                .map_err(|_| PlayerTriviaRepositoryError::InvalidOptions)?;
            let correct_index = usize::try_from(correct_index)
                .map_err(|_| PlayerTriviaRepositoryError::StoredIndexOverflow)?;
            let distinct = options
                .iter()
                .map(|option| option.to_lowercase())
                .collect::<BTreeSet<_>>();
            if distinct.len() != 4 {
                return Err(PlayerTriviaRepositoryError::InvalidOptions);
            }
            if correct_index >= 4 {
                return Err(PlayerTriviaRepositoryError::InvalidCorrectIndex);
            }
            let question = PlayerTriviaQuestion {
                key,
                category,
                text,
                options,
                correct_index,
                explanation,
                spicy,
            };
            questions.push(StoredPlayerTriviaQuestion {
                question_number: number,
                question,
                selected_index: selected_index
                    .map(usize::try_from)
                    .transpose()
                    .map_err(|_| PlayerTriviaRepositoryError::StoredIndexOverflow)?,
                is_correct,
                reward,
                answered_at,
            });
        }
        Ok(Some(StoredPlayerTriviaSession {
            session_id,
            guild_id,
            discord_id,
            started_at,
            completed_at,
            status,
            question_count,
            score,
            jc_earned,
            questions,
        }))
    }

    pub fn cancel_session_if_unanswered_at(
        &self,
        session_id: i64,
        now: i64,
    ) -> Result<bool, PlayerTriviaRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE player_trivia_sessions SET status = 'cancelled', completed_at = ?1
             WHERE session_id = ?2 AND status = 'active' AND NOT EXISTS (
                 SELECT 1 FROM player_trivia_questions
                 WHERE session_id = ?2 AND selected_index IS NOT NULL
             )",
            params![now, session_id],
        )? == 1;
        transaction.commit()?;
        Ok(changed)
    }

    pub fn expire_stale_sessions(
        &self,
        now: i64,
        answer_timeout_seconds: i64,
    ) -> Result<usize, PlayerTriviaRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE player_trivia_sessions AS s SET status = 'timed_out', completed_at = ?1
             WHERE s.status = 'active' AND COALESCE((
                 SELECT MAX(q.answered_at) FROM player_trivia_questions q
                 WHERE q.session_id = s.session_id
             ), s.started_at) <= ?2",
            params![now, now.saturating_sub(answer_timeout_seconds.max(0))],
        )?;
        transaction.commit()?;
        Ok(changed)
    }

    /// Finish an active session only while the identified question is still
    /// its first unanswered round.
    ///
    /// Timeout tasks race with component callbacks in production. Keeping the
    /// question predicate in the same immediate transaction prevents an old
    /// timer from timing out the next round after an answer has committed.
    pub fn timeout_session_if_question_unanswered(
        &self,
        session_id: i64,
        question_number: i64,
        completed_at: i64,
    ) -> Result<bool, PlayerTriviaRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE player_trivia_sessions AS s
             SET status = 'timed_out', completed_at = ?1
             WHERE s.session_id = ?2 AND s.status = 'active'
               AND ?3 = (
                   SELECT MIN(q.question_number)
                   FROM player_trivia_questions q
                   WHERE q.session_id = s.session_id AND q.selected_index IS NULL
               )
               AND EXISTS (
                   SELECT 1 FROM player_trivia_questions q
                   WHERE q.session_id = s.session_id
                     AND q.question_number = ?3
                     AND q.selected_index IS NULL
               )",
            params![completed_at, session_id, question_number],
        )? == 1;
        transaction.commit()?;
        Ok(changed)
    }

    pub fn load_snapshot(
        &self,
        guild_id: i64,
    ) -> Result<PlayerTriviaSnapshot, PlayerTriviaRepositoryError> {
        let connection = self.connection()?;
        connection.execute_batch("BEGIN")?;
        let result = load_snapshot_from_connection(&connection, guild_id);
        connection.execute_batch("ROLLBACK")?;
        result
    }

    pub fn try_start_session(
        &self,
        discord_id: i64,
        guild_id: i64,
        questions: &[PlayerTriviaQuestionRecord],
        now: i64,
        cooldown_seconds: i64,
        bypass: bool,
    ) -> Result<Option<i64>, PlayerTriviaRepositoryError> {
        if questions.is_empty() {
            return Err(PlayerTriviaRepositoryError::EmptyQuestions);
        }
        if cooldown_seconds < 0 {
            return Err(PlayerTriviaRepositoryError::NegativeCooldown);
        }
        for question in questions {
            if question.question_key.trim().is_empty()
                || question.category.trim().is_empty()
                || question.question_text.trim().is_empty()
            {
                return Err(PlayerTriviaRepositoryError::InvalidQuestionShape);
            }
            let distinct = question
                .options
                .iter()
                .map(|value| value.to_lowercase())
                .collect::<BTreeSet<_>>();
            if distinct.len() != 4 {
                return Err(PlayerTriviaRepositoryError::InvalidOptions);
            }
            if question.correct_index >= 4 {
                return Err(PlayerTriviaRepositoryError::InvalidCorrectIndex);
            }
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !bypass {
            let claimed = transaction
                .query_row(
                    "SELECT 1 FROM player_trivia_sessions
                 WHERE guild_id = ?1 AND discord_id = ?2 AND status != 'cancelled'
                   AND started_at > ?3 LIMIT 1",
                    params![guild_id, discord_id, now.saturating_sub(cooldown_seconds)],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if claimed {
                transaction.commit()?;
                return Ok(None);
            }
        }
        transaction.execute(
            "INSERT INTO player_trivia_sessions
                 (guild_id, discord_id, started_at, status, question_count)
             VALUES (?1, ?2, ?3, 'active', ?4)",
            params![guild_id, discord_id, now, questions.len() as i64],
        )?;
        let session_id = transaction.last_insert_rowid();
        for (index, question) in questions.iter().enumerate() {
            transaction.execute(
                "INSERT INTO player_trivia_questions
                 (session_id, question_number, question_key, category, spicy,
                  question_text, options_json, correct_index, explanation)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    session_id,
                    index as i64 + 1,
                    question.question_key,
                    question.category,
                    question.spicy,
                    question.question_text,
                    serde_json::to_string(&question.options)?,
                    question.correct_index as i64,
                    question.explanation
                ],
            )?;
        }
        transaction.commit()?;
        Ok(Some(session_id))
    }

    pub fn settle_answer(
        &self,
        session_id: i64,
        question_number: i64,
        selected_index: usize,
        reward_if_correct: i64,
        answered_at: i64,
    ) -> Result<Option<PlayerTriviaSettlement>, PlayerTriviaRepositoryError> {
        if reward_if_correct < 0 {
            return Err(PlayerTriviaRepositoryError::NegativeReward);
        }
        if selected_index >= 4 {
            return Err(PlayerTriviaRepositoryError::InvalidSelectedIndex);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = transaction
            .query_row(
                "SELECT s.guild_id, s.discord_id, s.status, s.question_count, s.score,
                    s.jc_earned, q.question_key, q.category, q.correct_index, q.selected_index
             FROM player_trivia_sessions s JOIN player_trivia_questions q USING(session_id)
             WHERE s.session_id = ?1 AND q.question_number = ?2",
                params![session_id, question_number],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, Option<i64>>(9)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            guild_id,
            discord_id,
            status,
            question_count,
            old_score,
            old_jc,
            question_key,
            category,
            correct_index,
            prior_selection,
        )) = row
        else {
            transaction.commit()?;
            return Ok(None);
        };
        if status != "active" || prior_selection.is_some() {
            transaction.commit()?;
            return Ok(None);
        }
        let next: Option<i64> = transaction.query_row(
            "SELECT MIN(question_number) FROM player_trivia_questions
             WHERE session_id = ?1 AND selected_index IS NULL",
            [session_id],
            |row| row.get(0),
        )?;
        if next != Some(question_number) {
            transaction.commit()?;
            return Ok(None);
        }
        let is_correct = selected_index as i64 == correct_index;
        let reward = if is_correct { reward_if_correct } else { 0 };
        let changed = transaction.execute(
            "UPDATE player_trivia_questions SET selected_index = ?1, is_correct = ?2,
                    reward = ?3, answered_at = ?4
             WHERE session_id = ?5 AND question_number = ?6 AND selected_index IS NULL",
            params![
                selected_index as i64,
                is_correct,
                reward,
                answered_at,
                session_id,
                question_number
            ],
        )?;
        if changed != 1 {
            transaction.commit()?;
            return Ok(None);
        }
        let balance: Option<i64> = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance, 0) FROM players
             WHERE guild_id = ?1 AND discord_id = ?2",
                params![guild_id, discord_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(balance) = balance else {
            return Err(PlayerTriviaRepositoryError::MissingPlayer);
        };
        let new_balance = balance.saturating_add(reward);
        if reward > 0 {
            set_ledger_context(
                &transaction,
                discord_id,
                session_id,
                json!({
                    "session_id": session_id,
                    "question_number": question_number,
                    "question_key": question_key,
                    "category": category,
                    "selected_index": selected_index,
                    "correct_index": correct_index,
                    "reward": reward,
                    "answered_at": answered_at,
                })
                .to_string(),
            )?;
            transaction.execute(
                "UPDATE players SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
                 WHERE guild_id = ?2 AND discord_id = ?3",
                params![new_balance, guild_id, discord_id],
            )?;
            clear_ledger_context(&transaction)?;
        }
        let score = old_score + i64::from(is_correct);
        let jc_earned = old_jc.saturating_add(reward);
        let answered: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM player_trivia_questions
             WHERE session_id = ?1 AND selected_index IS NOT NULL",
            [session_id],
            |row| row.get(0),
        )?;
        let completed = answered >= question_count;
        transaction.execute(
            "UPDATE player_trivia_sessions SET score = ?1, jc_earned = ?2,
                 status = CASE WHEN ?3 THEN 'completed' ELSE status END,
                 completed_at = CASE WHEN ?3 THEN ?4 ELSE completed_at END
             WHERE session_id = ?5",
            params![score, jc_earned, completed, answered_at, session_id],
        )?;
        transaction.commit()?;
        Ok(Some(PlayerTriviaSettlement {
            is_correct,
            reward,
            score,
            jc_earned,
            completed,
            new_balance,
        }))
    }

    pub fn finish_session(
        &self,
        session_id: i64,
        status: &str,
        completed_at: i64,
    ) -> Result<bool, PlayerTriviaRepositoryError> {
        if !matches!(status, "completed" | "timed_out" | "error") {
            return Err(PlayerTriviaRepositoryError::InvalidFinishStatus(
                status.to_owned(),
            ));
        }
        Ok(self.connection()?.execute(
            "UPDATE player_trivia_sessions SET status = ?1, completed_at = ?2
             WHERE session_id = ?3 AND status = 'active'",
            params![status, completed_at, session_id],
        )? == 1)
    }

    pub fn recent_question_keys(
        &self,
        discord_id: i64,
        guild_id: i64,
        since: i64,
    ) -> Result<Vec<String>, PlayerTriviaRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT q.question_key FROM player_trivia_questions q
             JOIN player_trivia_sessions s USING(session_id)
             WHERE s.guild_id = ?1 AND s.discord_id = ?2 AND s.started_at >= ?3
               AND s.status != 'cancelled' ORDER BY q.question_key ASC",
        )?;
        statement
            .query_map(params![guild_id, discord_id, since], |row| row.get(0))
            .map_err(PlayerTriviaRepositoryError::from)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(PlayerTriviaRepositoryError::from)
    }

    pub fn cancel_session_if_unanswered(
        &self,
        session_id: i64,
    ) -> Result<bool, PlayerTriviaRepositoryError> {
        self.cancel_session_if_unanswered_at(session_id, unix_timestamp()?)
    }
}

fn load_snapshot_from_connection(
    connection: &Connection,
    guild_id: i64,
) -> Result<PlayerTriviaSnapshot, PlayerTriviaRepositoryError> {
    let players = query_rows(
        connection,
        "SELECT discord_id, discord_username, COALESCE(wins,0), COALESCE(losses,0),
                COALESCE(jopacoin_balance,0), lowest_balance_ever, glicko_rating, glicko_rd,
                os_mu, os_sigma,
                CASE
                    WHEN typeof(last_match_date) IN ('integer','real')
                        THEN CAST(last_match_date AS INTEGER)
                    ELSE CAST(strftime('%s', last_match_date) AS INTEGER)
                END
         FROM players WHERE guild_id = ?1 ORDER BY discord_id ASC",
        guild_id,
        |row| {
            Ok(PlayerTriviaPlayer {
                discord_id: row.get(0)?,
                username: row.get(1)?,
                wins: row.get(2)?,
                losses: row.get(3)?,
                balance: row.get(4)?,
                lowest_balance_ever: row.get(5)?,
                glicko_rating: row.get(6)?,
                glicko_rd: row.get::<_, Option<f64>>(7)?.map(cap_glicko_rd),
                openskill_mu: row.get(8)?,
                openskill_sigma: row.get(9)?,
                last_match_unix: row.get(10)?,
            })
        },
    )?;
    let participants = query_rows(
        connection,
        "SELECT mp.discord_id, mp.match_id,
                CASE
                    WHEN typeof(m.match_date) IN ('integer','real') THEN CAST(m.match_date AS INTEGER)
                    ELSE COALESCE(CAST(strftime('%s', m.match_date) AS INTEGER), 0)
                END,
                COALESCE(mp.hero_id,0), COALESCE(mp.won,0), mp.kills, mp.assists,
                mp.gpm, mp.hero_damage, mp.tower_damage, mp.fantasy_points, mp.lane_role
         FROM match_participants mp JOIN matches m
           ON m.guild_id=mp.guild_id AND m.match_id=mp.match_id
         WHERE mp.guild_id=?1 ORDER BY 3, mp.match_id, mp.discord_id",
        guild_id,
        |row| {
            Ok(ParticipantRow {
                discord_id: row.get(0)?,
                match_id: row.get(1)?,
                match_time: row.get(2)?,
                hero_id: row.get(3)?,
                won: row.get(4)?,
                kills: row.get(5)?,
                assists: row.get(6)?,
                gpm: row.get(7)?,
                hero_damage: row.get(8)?,
                tower_damage: row.get(9)?,
                fantasy_points: row.get(10)?,
                lane_role: row.get(11)?,
            })
        },
    )?;
    let ratings = query_rows(
        connection,
        "SELECT rh.id, rh.discord_id, rh.match_id, rh.rating, rh.rating_before,
                CASE
                    WHEN typeof(m.match_date) IN ('integer','real') THEN CAST(m.match_date AS INTEGER)
                    ELSE COALESCE(CAST(strftime('%s', m.match_date) AS INTEGER), 0)
                END
         FROM rating_history rh JOIN matches m
           ON m.guild_id=rh.guild_id AND m.match_id=rh.match_id
         WHERE rh.guild_id=?1 ORDER BY 6, rh.match_id, rh.id",
        guild_id,
        |row| {
            Ok(RatingRow {
                history_id: row.get(0)?,
                discord_id: row.get(1)?,
                match_id: row.get(2)?,
                rating: row.get(3)?,
                rating_before: row.get(4)?,
                match_time: row.get(5)?,
            })
        },
    )?;
    let pairings = query_rows(
        connection,
        "SELECT player1_id, player2_id, COALESCE(games_together,0),
                COALESCE(wins_together,0), COALESCE(games_against,0),
                COALESCE(player1_wins_against,0), last_match_id
         FROM player_pairings WHERE guild_id=?1 ORDER BY player1_id, player2_id",
        guild_id,
        |row| {
            Ok(PairingRow {
                player1_id: row.get(0)?,
                player2_id: row.get(1)?,
                games_together: row.get(2)?,
                wins_together: row.get(3)?,
                games_against: row.get(4)?,
                player1_wins_against: row.get(5)?,
                last_match_id: row.get(6)?,
            })
        },
    )?;
    let tips = query_rows(
        connection,
        "SELECT sender_id, recipient_id, amount FROM tip_transactions
         WHERE guild_id=?1 ORDER BY timestamp, id",
        guild_id,
        |row| {
            Ok(TipRow {
                sender_id: row.get(0)?,
                recipient_id: row.get(1)?,
                amount: row.get(2)?,
            })
        },
    )?;
    let wheel_spins = query_rows(
        connection,
        "SELECT spin_id, discord_id, spin_time, outcome_code FROM wheel_spins
         WHERE guild_id=?1 ORDER BY spin_time, spin_id",
        guild_id,
        |row| {
            Ok(WheelSpinRow {
                spin_id: row.get(0)?,
                discord_id: row.get(1)?,
                spin_time: row.get(2)?,
                outcome_code: row.get(3)?,
            })
        },
    )?;
    let double_spins = query_rows(
        connection,
        "SELECT spin_id, discord_id, cost, balance_before, balance_after,
                COALESCE(won,0), spin_time
         FROM double_or_nothing_spins WHERE guild_id=?1 ORDER BY spin_time, spin_id",
        guild_id,
        |row| {
            Ok(DoubleSpinRow {
                spin_id: row.get(0)?,
                discord_id: row.get(1)?,
                cost: row.get(2)?,
                balance_before: row.get(3)?,
                balance_after: row.get(4)?,
                won: row.get(5)?,
                spin_time: row.get(6)?,
            })
        },
    )?;
    let trivia_sessions = query_rows(
        connection,
        "SELECT discord_id, COALESCE(streak,0), COALESCE(jc_earned,0) FROM trivia_sessions
         WHERE guild_id=?1 ORDER BY played_at, id",
        guild_id,
        |row| {
            Ok(TriviaHistoryRow {
                discord_id: row.get(0)?,
                streak: row.get(1)?,
                jc_earned: row.get(2)?,
            })
        },
    )?;
    let tunnels = query_rows(
        connection,
        "SELECT discord_id, COALESCE(total_digs,0), COALESCE(max_depth,0),
                COALESCE(total_jc_earned,0), COALESCE(prestige_level,0), COALESCE(best_run_score,0),
                COALESCE(pickaxe_tier,0), COALESCE(stat_strength,0),
                COALESCE(stat_smarts,0), COALESCE(stat_stamina,0)
         FROM tunnels WHERE guild_id=?1 ORDER BY discord_id",
        guild_id,
        |row| {
            Ok(TunnelRow {
                discord_id: row.get(0)?,
                total_digs: row.get(1)?,
                max_depth: row.get(2)?,
                total_jc_earned: row.get(3)?,
                prestige_level: row.get(4)?,
                best_run_score: row.get(5)?,
                pickaxe_tier: row.get(6)?,
                stat_strength: row.get(7)?,
                stat_smarts: row.get(8)?,
                stat_stamina: row.get(9)?,
            })
        },
    )?;
    let bankruptcies = query_rows(
        connection,
        "SELECT discord_id, COALESCE(bankruptcy_count,0) FROM bankruptcy_state
         WHERE guild_id=?1 ORDER BY discord_id",
        guild_id,
        |row| {
            Ok(BankruptcyRow {
                discord_id: row.get(0)?,
                bankruptcy_count: row.get(1)?,
            })
        },
    )?;
    let disbursement_votes = query_rows(
        connection,
        "SELECT proposal_id, discord_id, COALESCE(vote_method,''),
                COALESCE(proposal_outcome,''), finalized_at FROM disburse_vote_history
         WHERE guild_id=?1 ORDER BY voted_at, proposal_id, discord_id",
        guild_id,
        |row| {
            Ok(DisbursementVoteRow {
                proposal_id: row.get(0)?,
                discord_id: row.get(1)?,
                vote_method: row.get(2)?,
                proposal_outcome: row.get(3)?,
                finalized_at: row.get(4)?,
            })
        },
    )?;
    let prediction_positions = query_rows(
        connection,
        "SELECT pp.prediction_id, pp.discord_id, p.status, COALESCE(p.outcome,''),
                COALESCE(pp.yes_contracts,0), COALESCE(pp.yes_cost_basis_total,0),
                COALESCE(pp.no_contracts,0), COALESCE(pp.no_cost_basis_total,0),
                COALESCE(pp.bankruptcy_penalty,0), COALESCE(pp.vanity_tax,0),
                COALESCE(pp.low_priority_tax,0),
                CASE WHEN p.status='resolved' THEN p.question END
         FROM prediction_positions pp JOIN predictions p USING(prediction_id)
         WHERE p.guild_id=?1 ORDER BY pp.prediction_id, pp.discord_id",
        guild_id,
        |row| {
            Ok(PredictionPositionRow {
                prediction_id: row.get(0)?,
                discord_id: row.get(1)?,
                status: row.get(2)?,
                outcome: row.get(3)?,
                yes_contracts: row.get(4)?,
                yes_cost_basis_total: row.get(5)?,
                no_contracts: row.get(6)?,
                no_cost_basis_total: row.get(7)?,
                bankruptcy_penalty: row.get(8)?,
                vanity_tax: row.get(9)?,
                low_priority_tax: row.get(10)?,
                question: row.get(11)?,
            })
        },
    )?;
    let prediction_trades = query_rows(
        connection,
        "SELECT pt.trade_id, pt.prediction_id, pt.discord_id, pt.action,
                pt.contracts, pt.jopacoins, pt.trade_time, p.status,
                COALESCE(p.outcome,''), p.resolved_at
         FROM prediction_trades pt JOIN predictions p USING(prediction_id)
         WHERE p.guild_id=?1 ORDER BY pt.trade_time, pt.trade_id",
        guild_id,
        |row| {
            Ok(PredictionTradeRow {
                trade_id: row.get(0)?,
                prediction_id: row.get(1)?,
                discord_id: row.get(2)?,
                action: row.get(3)?,
                contracts: row.get(4)?,
                jopacoins: row.get(5)?,
                trade_time: row.get(6)?,
                status: row.get(7)?,
                outcome: row.get(8)?,
                resolved_at: row.get(9)?,
            })
        },
    )?;
    let bets = query_rows(
        connection,
        "SELECT b.bet_id, b.discord_id, b.match_id, b.team_bet_on, m.winning_team,
                b.amount, COALESCE(b.leverage,1), b.amount*COALESCE(b.leverage,1),
                COALESCE(b.payout,0), COALESCE(bst.vanity_tax,0),
                COALESCE(bst.low_priority_tax,0)
         FROM bets b JOIN matches m ON m.guild_id=b.guild_id AND m.match_id=b.match_id
         LEFT JOIN bet_settlement_taxes bst ON bst.guild_id=b.guild_id
              AND bst.match_id=b.match_id AND bst.discord_id=b.discord_id
         WHERE b.guild_id=?1 AND m.winning_team IN (1,2) ORDER BY b.bet_time,b.bet_id",
        guild_id,
        |row| {
            Ok(SettledBetRow {
                bet_id: row.get(0)?,
                discord_id: row.get(1)?,
                match_id: row.get(2)?,
                team_bet_on: row.get(3)?,
                winning_team: row.get(4)?,
                amount: row.get(5)?,
                leverage: row.get(6)?,
                effective_bet: row.get(7)?,
                payout: row.get(8)?,
                settlement_vanity_tax: row.get(9)?,
                settlement_low_priority_tax: row.get(10)?,
            })
        },
    )?;
    let dig_artifacts = query_rows(
        connection,
        "SELECT id, discord_id, artifact_id, found_at, COALESCE(is_relic,0),
                COALESCE(equipped,0) FROM dig_artifacts
         WHERE guild_id=?1 ORDER BY found_at,id",
        guild_id,
        |row| {
            Ok(DigArtifactRow {
                row_id: row.get(0)?,
                discord_id: row.get(1)?,
                artifact_id: row.get(2)?,
                found_at: row.get(3)?,
                is_relic: row.get(4)?,
                equipped: row.get(5)?,
            })
        },
    )?;
    let dig_achievements = query_rows(
        connection,
        "SELECT discord_id, achievement_id, unlocked_at FROM dig_achievements
         WHERE guild_id=?1 ORDER BY unlocked_at,discord_id,achievement_id",
        guild_id,
        |row| {
            Ok(DigAchievementRow {
                discord_id: row.get(0)?,
                achievement_id: row.get(1)?,
                unlocked_at: row.get(2)?,
            })
        },
    )?;
    let dig_actions = query_rows(
        connection,
        "SELECT id, actor_id, target_id, action_type, depth_before, depth_after,
                COALESCE(jc_delta,0), created_at FROM dig_actions
         WHERE guild_id=?1 ORDER BY created_at,id",
        guild_id,
        |row| {
            Ok(DigActionRow {
                row_id: row.get(0)?,
                actor_id: row.get(1)?,
                target_id: row.get(2)?,
                action_type: row.get(3)?,
                depth_before: row.get(4)?,
                depth_after: row.get(5)?,
                jc_delta: row.get(6)?,
                created_at: row.get(7)?,
            })
        },
    )?;
    let mafia_games = query_rows(
        connection,
        "SELECT game_id, phase, status, winner, mvp_id FROM mafia_games
         WHERE guild_id=?1 ORDER BY started_at,game_id",
        guild_id,
        |row| {
            Ok(MafiaGameRow {
                game_id: row.get(0)?,
                phase: row.get(1)?,
                status: row.get(2)?,
                winner: row.get(3)?,
                mvp_id: row.get(4)?,
            })
        },
    )?;
    let mafia_players = query_rows(
        connection,
        "SELECT mp.game_id, mp.discord_id, mp.role FROM mafia_players mp
         JOIN mafia_games mg ON mg.guild_id=mp.guild_id AND mg.game_id=mp.game_id
         WHERE mp.guild_id=?1 AND mg.phase='RESOLVED' ORDER BY mp.game_id,mp.discord_id",
        guild_id,
        |row| {
            Ok(MafiaPlayerRow {
                game_id: row.get(0)?,
                discord_id: row.get(1)?,
                role: row.get(2)?,
            })
        },
    )?;
    let mafia_actions = query_rows(
        connection,
        "SELECT ma.action_id, ma.game_id, ma.actor_id, ma.target_id,
                ma.action_type, ma.phase, COALESCE(ma.day_number,0), ma.created_at
         FROM mafia_actions ma JOIN mafia_games mg
           ON mg.guild_id=ma.guild_id AND mg.game_id=ma.game_id
         WHERE ma.guild_id=?1 AND mg.phase='RESOLVED'
         ORDER BY ma.created_at,ma.action_id",
        guild_id,
        |row| {
            Ok(MafiaActionRow {
                action_id: row.get(0)?,
                game_id: row.get(1)?,
                actor_id: row.get(2)?,
                target_id: row.get(3)?,
                action_type: row.get(4)?,
                phase: row.get(5)?,
                day_number: row.get(6)?,
                created_at: row.get(7)?,
            })
        },
    )?;
    let recalibrations = query_rows(
        connection,
        "SELECT discord_id, last_recalibration_at, COALESCE(total_recalibrations,0),
                rating_at_recalibration FROM recalibration_state
         WHERE guild_id=?1 ORDER BY discord_id",
        guild_id,
        |row| {
            Ok(RecalibrationRow {
                discord_id: row.get(0)?,
                last_recalibration_at: row.get(1)?,
                total_recalibrations: row.get(2)?,
                rating_at_recalibration: row.get(3)?,
            })
        },
    )?;
    Ok(PlayerTriviaSnapshot {
        players,
        participants,
        ratings,
        pairings,
        tips,
        wheel_spins,
        double_spins,
        trivia_sessions,
        tunnels,
        bankruptcies,
        disbursement_votes,
        prediction_positions,
        prediction_trades,
        bets,
        dig_artifacts,
        dig_achievements,
        dig_actions,
        mafia_games,
        mafia_players,
        mafia_actions,
        recalibrations,
    })
}

fn query_rows<T, F>(
    connection: &Connection,
    sql: &str,
    guild_id: i64,
    map: F,
) -> Result<Vec<T>, rusqlite::Error>
where
    F: FnMut(&rusqlite::Row<'_>) -> Result<T, rusqlite::Error>,
{
    let mut statement = connection.prepare(sql)?;
    statement.query_map([guild_id], map)?.collect()
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    actor_id: i64,
    session_id: i64,
    metadata: String,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context
         (id, source, actor_id, related_type, related_id, reason, metadata)
         VALUES (1, 'player_trivia', ?1, 'player_trivia_session', ?2,
                 'player trivia correct answer', ?3)",
        params![actor_id, session_id.to_string(), metadata],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn unix_timestamp() -> Result<i64, PlayerTriviaRepositoryError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .map_err(|_| PlayerTriviaRepositoryError::Clock)
}

#[cfg(test)]
#[path = "player_trivia/tests.rs"]
mod tests;
