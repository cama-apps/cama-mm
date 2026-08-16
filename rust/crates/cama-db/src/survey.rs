//! Existing-schema SQLite persistence for one-off surveys.
//!
//! The adapter mirrors the Python repository's transaction boundaries. It
//! never creates or migrates schema: startup initialization remains responsible for
//! proving that the four survey tables and their current columns exist.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

pub const MAX_SURVEY_QUESTIONS: usize = 10;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurveyStatus {
    Draft,
    Open,
    Closed,
}

impl SurveyStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }

    fn parse(value: &str) -> Result<Self, SurveyRepositoryError> {
        match value {
            "draft" => Ok(Self::Draft),
            "open" => Ok(Self::Open),
            "closed" => Ok(Self::Closed),
            _ => Err(SurveyRepositoryError::Corrupt(format!(
                "unknown survey status {value:?}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurveyTargetType {
    AllRegistered,
    Member,
    Role,
}

impl SurveyTargetType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AllRegistered => "all_registered",
            Self::Member => "member",
            Self::Role => "role",
        }
    }

    fn parse(value: &str) -> Result<Self, SurveyRepositoryError> {
        match value {
            "all_registered" => Ok(Self::AllRegistered),
            "member" => Ok(Self::Member),
            "role" => Ok(Self::Role),
            _ => Err(SurveyRepositoryError::Corrupt(format!(
                "unknown survey target type {value:?}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurveyQuestionType {
    Nps,
    Rating,
    Text,
}

impl SurveyQuestionType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nps => "nps",
            Self::Rating => "rating",
            Self::Text => "text",
        }
    }

    fn parse(value: &str) -> Result<Self, SurveyRepositoryError> {
        match value {
            "nps" => Ok(Self::Nps),
            "rating" => Ok(Self::Rating),
            "text" => Ok(Self::Text),
            _ => Err(SurveyRepositoryError::Corrupt(format!(
                "unknown survey question type {value:?}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurveyDeliveryStatus {
    Pending,
    Sending,
    Sent,
    Failed,
}

impl SurveyDeliveryStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sending => "sending",
            Self::Sent => "sent",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Result<Self, SurveyRepositoryError> {
        match value {
            "pending" => Ok(Self::Pending),
            "sending" => Ok(Self::Sending),
            "sent" => Ok(Self::Sent),
            "failed" => Ok(Self::Failed),
            _ => Err(SurveyRepositoryError::Corrupt(format!(
                "unknown survey delivery status {value:?}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Survey {
    pub survey_id: i64,
    pub guild_id: i64,
    pub title: String,
    pub status: SurveyStatus,
    pub target_type: Option<SurveyTargetType>,
    pub registered_only: bool,
    pub created_at: i64,
    pub opened_at: Option<i64>,
    pub closed_at: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurveyQuestion {
    pub question_id: i64,
    pub survey_id: i64,
    pub guild_id: i64,
    pub position: usize,
    pub prompt: String,
    pub question_type: SurveyQuestionType,
    pub required: bool,
    pub created_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurveyRecipient {
    pub recipient_id: i64,
    pub survey_id: i64,
    pub guild_id: i64,
    pub discord_id: i64,
    pub delivery_status: SurveyDeliveryStatus,
    pub attempt_count: u32,
    pub receipt_checked_attempt: u32,
    pub retry_requested: bool,
    pub claimed_at: Option<i64>,
    pub dm_channel_id: Option<i64>,
    pub dm_message_id: Option<i64>,
    pub ui_channel_id: Option<i64>,
    pub ui_message_id: Option<i64>,
    pub controls_finalized: bool,
    pub current_question_id: Option<i64>,
    pub in_review: bool,
    pub started_at: Option<i64>,
    pub submitted_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurveyDraftAnswer {
    pub question_id: i64,
    pub numeric_value: Option<i32>,
    pub text_value: Option<String>,
    pub skipped: bool,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SurveyAnswer {
    Numeric(i32),
    Text(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurveySession {
    pub survey: Survey,
    pub recipient: SurveyRecipient,
    pub questions: Vec<SurveyQuestion>,
    pub answers: Vec<SurveyDraftAnswer>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurveyQuestionResult {
    pub question_id: i64,
    pub position: usize,
    pub prompt: String,
    pub question_type: SurveyQuestionType,
    pub response_count: usize,
    pub skipped_count: usize,
    pub nps_score: Option<i32>,
    pub promoters: usize,
    pub passives: usize,
    pub detractors: usize,
    pub rating_average: Option<f64>,
    pub rating_distribution: [usize; 5],
    pub text_responses: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurveyResults {
    pub survey: Survey,
    pub recipient_count: usize,
    pub delivered_count: usize,
    pub started_count: usize,
    pub completed_count: usize,
    pub delivery_rate: f64,
    pub started_rate: f64,
    pub completion_rate: f64,
    pub response_rate: f64,
    pub questions: Vec<SurveyQuestionResult>,
}

#[derive(Debug, Error)]
pub enum SurveyRepositoryError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    State(String),
    #[error("{0}")]
    Response(String),
    #[error("survey database contains invalid state: {0}")]
    Corrupt(String),
    #[error("survey SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

type Clock = dyn Fn() -> i64 + Send + Sync;

#[derive(Clone)]
pub struct SurveyRepository {
    path: PathBuf,
    clock: Arc<Clock>,
}

impl fmt::Debug for SurveyRepository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SurveyRepository")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl SurveyRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self::with_clock(path, || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs() as i64)
        })
    }

    #[must_use]
    pub fn with_clock(
        path: impl AsRef<Path>,
        clock: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            clock: Arc::new(clock),
        }
    }

    fn now(&self) -> i64 {
        (self.clock)()
    }

    fn connection(&self) -> Result<Connection, SurveyRepositoryError> {
        Ok(open_runtime_connection(&self.path)?)
    }

    fn immediate(&self) -> Result<(Connection, i64), SurveyRepositoryError> {
        Ok((self.connection()?, self.now()))
    }

    /// Admit only the already-migrated Python survey schema. This deliberately
    /// performs no DDL; runtime startup must fail closed if any live column is
    /// missing.
    pub fn verify_schema(&self) -> Result<(), SurveyRepositoryError> {
        let connection = self.connection()?;
        connection.prepare(
            "SELECT survey_id,guild_id,title,status,target_type,registered_only,
                    created_at,opened_at,closed_at FROM surveys LIMIT 0",
        )?;
        connection.prepare(
            "SELECT question_id,survey_id,guild_id,position,prompt,question_type,
                    required,created_at FROM survey_questions LIMIT 0",
        )?;
        connection.prepare(
            "SELECT recipient_id,survey_id,guild_id,discord_id,delivery_status,
                    attempt_count,receipt_checked_attempt,retry_requested,claimed_at,
                    dm_channel_id,dm_message_id,ui_channel_id,ui_message_id,
                    controls_finalized,current_question_id,in_review,started_at,
                    submitted_at,last_error,created_at,updated_at
             FROM survey_recipients LIMIT 0",
        )?;
        connection.prepare(
            "SELECT survey_id,guild_id,recipient_id,question_id,numeric_value,
                    text_value,skipped,created_at,updated_at
             FROM survey_answers LIMIT 0",
        )?;
        connection.prepare("SELECT discord_id,guild_id FROM players LIMIT 0")?;
        Ok(())
    }

    /// Current Cama registrations for recipient targeting. Guild membership is
    /// intersected with the live Discord snapshot by the application policy.
    pub fn registered_discord_ids(&self, guild_id: i64) -> Result<Vec<i64>, SurveyRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id FROM players WHERE guild_id=?1 AND discord_id>0
             ORDER BY discord_id",
        )?;
        let rows = statement.query_map([guild_id], |row| row.get(0))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }

    pub fn create_survey(
        &self,
        guild_id: i64,
        title: &str,
    ) -> Result<Survey, SurveyRepositoryError> {
        let title = title.trim();
        if title.is_empty() || title.chars().count() > 100 {
            return Err(SurveyRepositoryError::Invalid(
                "Survey titles must contain 1 to 100 characters".to_owned(),
            ));
        }
        let (mut connection, now) = self.immediate()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO surveys (guild_id,title,status,created_at) VALUES (?1,?2,'draft',?3)",
            params![guild_id, title, now],
        )?;
        let survey_id = transaction.last_insert_rowid();
        let survey = query_survey(&transaction, guild_id, survey_id)?
            .ok_or_else(|| SurveyRepositoryError::Corrupt("new survey disappeared".to_owned()))?;
        transaction.commit()?;
        Ok(survey)
    }

    pub fn list_surveys(
        &self,
        guild_id: i64,
        status: Option<SurveyStatus>,
    ) -> Result<Vec<Survey>, SurveyRepositoryError> {
        let connection = self.connection()?;
        let mut surveys = Vec::new();
        if let Some(status) = status {
            let mut statement = connection.prepare(
                "SELECT * FROM surveys WHERE guild_id=?1 AND status=?2 ORDER BY survey_id DESC",
            )?;
            let rows = statement.query_map(params![guild_id, status.as_str()], survey_from_row)?;
            for row in rows {
                surveys.push(row??);
            }
        } else {
            let mut statement = connection
                .prepare("SELECT * FROM surveys WHERE guild_id=?1 ORDER BY survey_id DESC")?;
            let rows = statement.query_map([guild_id], survey_from_row)?;
            for row in rows {
                surveys.push(row??);
            }
        }
        Ok(surveys)
    }

    pub fn get_survey(
        &self,
        guild_id: i64,
        survey_id: i64,
    ) -> Result<Option<Survey>, SurveyRepositoryError> {
        query_survey(&self.connection()?, guild_id, survey_id)
    }

    pub fn delete_survey(
        &self,
        guild_id: i64,
        survey_id: i64,
    ) -> Result<bool, SurveyRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(survey) = query_survey(&transaction, guild_id, survey_id)? else {
            return Ok(false);
        };
        if survey.status != SurveyStatus::Draft {
            return Err(SurveyRepositoryError::State(
                "Only draft surveys can be deleted".to_owned(),
            ));
        }
        transaction.execute(
            "DELETE FROM survey_answers WHERE guild_id=?1 AND survey_id=?2",
            params![guild_id, survey_id],
        )?;
        transaction.execute(
            "DELETE FROM survey_recipients WHERE guild_id=?1 AND survey_id=?2",
            params![guild_id, survey_id],
        )?;
        transaction.execute(
            "DELETE FROM survey_questions WHERE guild_id=?1 AND survey_id=?2",
            params![guild_id, survey_id],
        )?;
        let deleted = transaction.execute(
            "DELETE FROM surveys WHERE guild_id=?1 AND survey_id=?2",
            params![guild_id, survey_id],
        )? == 1;
        transaction.commit()?;
        Ok(deleted)
    }

    pub fn get_questions(
        &self,
        guild_id: i64,
        survey_id: i64,
    ) -> Result<Vec<SurveyQuestion>, SurveyRepositoryError> {
        let connection = self.connection()?;
        require_survey(&connection, guild_id, survey_id)?;
        query_questions(&connection, guild_id, survey_id)
    }

    pub fn add_question(
        &self,
        guild_id: i64,
        survey_id: i64,
        prompt: &str,
        question_type: SurveyQuestionType,
        required: bool,
    ) -> Result<SurveyQuestion, SurveyRepositoryError> {
        let prompt = checked_prompt(prompt)?;
        let (mut connection, now) = self.immediate()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_draft(&transaction, guild_id, survey_id)?;
        let count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM survey_questions WHERE guild_id=?1 AND survey_id=?2",
            params![guild_id, survey_id],
            |row| row.get(0),
        )?;
        if count >= MAX_SURVEY_QUESTIONS as i64 {
            return Err(SurveyRepositoryError::State(format!(
                "A survey can contain at most {MAX_SURVEY_QUESTIONS} questions"
            )));
        }
        transaction.execute(
            "INSERT INTO survey_questions
             (survey_id,guild_id,position,prompt,question_type,required,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                survey_id,
                guild_id,
                count + 1,
                prompt,
                question_type.as_str(),
                required,
                now
            ],
        )?;
        let question_id = transaction.last_insert_rowid();
        let question = query_question(&transaction, guild_id, survey_id, question_id)?
            .ok_or_else(|| SurveyRepositoryError::Corrupt("new question disappeared".to_owned()))?;
        transaction.commit()?;
        Ok(question)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn edit_question(
        &self,
        guild_id: i64,
        survey_id: i64,
        question_id: i64,
        prompt: Option<&str>,
        question_type: Option<SurveyQuestionType>,
        required: Option<bool>,
    ) -> Result<SurveyQuestion, SurveyRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_draft(&transaction, guild_id, survey_id)?;
        let existing =
            query_question(&transaction, guild_id, survey_id, question_id)?.ok_or_else(|| {
                SurveyRepositoryError::NotFound("Survey question not found".to_owned())
            })?;
        let prompt = prompt
            .map(checked_prompt)
            .transpose()?
            .unwrap_or(existing.prompt);
        transaction.execute(
            "UPDATE survey_questions SET prompt=?1,question_type=?2,required=?3
             WHERE guild_id=?4 AND survey_id=?5 AND question_id=?6",
            params![
                prompt,
                question_type.unwrap_or(existing.question_type).as_str(),
                required.unwrap_or(existing.required),
                guild_id,
                survey_id,
                question_id
            ],
        )?;
        let updated = query_question(&transaction, guild_id, survey_id, question_id)?
            .ok_or_else(|| SurveyRepositoryError::Corrupt("question disappeared".to_owned()))?;
        transaction.commit()?;
        Ok(updated)
    }

    pub fn remove_question(
        &self,
        guild_id: i64,
        survey_id: i64,
        question_id: i64,
    ) -> Result<bool, SurveyRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_draft(&transaction, guild_id, survey_id)?;
        let questions = query_questions(&transaction, guild_id, survey_id)?;
        if !questions
            .iter()
            .any(|question| question.question_id == question_id)
        {
            return Ok(false);
        }
        replace_question_order(
            &transaction,
            &questions
                .into_iter()
                .filter(|question| question.question_id != question_id)
                .collect::<Vec<_>>(),
            guild_id,
            survey_id,
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn move_question(
        &self,
        guild_id: i64,
        survey_id: i64,
        question_id: i64,
        new_position: usize,
    ) -> Result<Vec<SurveyQuestion>, SurveyRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_draft(&transaction, guild_id, survey_id)?;
        let mut questions = query_questions(&transaction, guild_id, survey_id)?;
        let Some(index) = questions
            .iter()
            .position(|question| question.question_id == question_id)
        else {
            return Err(SurveyRepositoryError::NotFound(
                "Survey question not found".to_owned(),
            ));
        };
        if new_position == 0 || new_position > questions.len() {
            return Err(SurveyRepositoryError::Invalid(format!(
                "Question position must be between 1 and {}",
                questions.len()
            )));
        }
        let question = questions.remove(index);
        questions.insert(new_position - 1, question);
        replace_question_order(&transaction, &questions, guild_id, survey_id)?;
        let updated = query_questions(&transaction, guild_id, survey_id)?;
        transaction.commit()?;
        Ok(updated)
    }

    pub fn open_survey(
        &self,
        guild_id: i64,
        survey_id: i64,
        recipient_ids: &[i64],
        target_type: SurveyTargetType,
        registered_only: bool,
    ) -> Result<Survey, SurveyRepositoryError> {
        if target_type == SurveyTargetType::AllRegistered && registered_only {
            return Err(SurveyRepositoryError::Invalid(
                "all_registered targeting is already limited to registered users".to_owned(),
            ));
        }
        let mut recipients = Vec::new();
        for recipient_id in recipient_ids {
            if *recipient_id <= 0 {
                return Err(SurveyRepositoryError::Invalid(
                    "Survey recipients must be real Discord users".to_owned(),
                ));
            }
            if !recipients.contains(recipient_id) {
                recipients.push(*recipient_id);
            }
        }
        if target_type == SurveyTargetType::Member && recipients.len() != 1 {
            return Err(SurveyRepositoryError::Invalid(
                "member targeting requires exactly one recipient".to_owned(),
            ));
        }
        if recipients.is_empty() {
            return Err(SurveyRepositoryError::State(
                "The survey target has no eligible recipients".to_owned(),
            ));
        }
        let (mut connection, now) = self.immediate()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_draft(&transaction, guild_id, survey_id)?;
        let questions = query_questions(&transaction, guild_id, survey_id)?;
        if questions.is_empty() {
            return Err(SurveyRepositoryError::State(
                "Add at least one question before sending a survey".to_owned(),
            ));
        }
        let updated = transaction.execute(
            "UPDATE surveys SET status='open',target_type=?1,registered_only=?2,
             opened_at=?3,closed_at=NULL WHERE guild_id=?4 AND survey_id=?5 AND status='draft'",
            params![
                target_type.as_str(),
                registered_only,
                now,
                guild_id,
                survey_id
            ],
        )?;
        if updated != 1 {
            return Err(SurveyRepositoryError::State(
                "Survey has already been sent".to_owned(),
            ));
        }
        let first_question_id = questions[0].question_id;
        for recipient_id in recipients {
            transaction.execute(
                "INSERT INTO survey_recipients
                 (survey_id,guild_id,discord_id,delivery_status,current_question_id,created_at,updated_at)
                 VALUES (?1,?2,?3,'pending',?4,?5,?5)",
                params![survey_id, guild_id, recipient_id, first_question_id, now],
            )?;
        }
        let survey = require_survey(&transaction, guild_id, survey_id)?;
        transaction.commit()?;
        Ok(survey)
    }

    pub fn close_survey(
        &self,
        guild_id: i64,
        survey_id: i64,
    ) -> Result<Survey, SurveyRepositoryError> {
        let (mut connection, now) = self.immediate()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let survey = require_survey(&transaction, guild_id, survey_id)?;
        if survey.status == SurveyStatus::Draft {
            return Err(SurveyRepositoryError::State(
                "A draft survey cannot be closed".to_owned(),
            ));
        }
        if survey.status == SurveyStatus::Closed {
            return Ok(survey);
        }
        transaction.execute(
            "UPDATE surveys SET status='closed',closed_at=?1
             WHERE guild_id=?2 AND survey_id=?3 AND status='open'",
            params![now, guild_id, survey_id],
        )?;
        transaction.execute(
            "UPDATE survey_recipients
             SET delivery_status='failed',claimed_at=NULL,retry_requested=0,
                 last_error='Survey closed before delivery',updated_at=?1
             WHERE guild_id=?2 AND survey_id=?3 AND
               (delivery_status='pending' OR
                (delivery_status='sending' AND receipt_checked_attempt>=attempt_count))",
            params![now, guild_id, survey_id],
        )?;
        transaction.execute(
            "UPDATE survey_recipients SET retry_requested=0,updated_at=?1
             WHERE guild_id=?2 AND survey_id=?3 AND retry_requested=1",
            params![now, guild_id, survey_id],
        )?;
        let closed = require_survey(&transaction, guild_id, survey_id)?;
        transaction.commit()?;
        Ok(closed)
    }

    pub fn recover_stale_deliveries(
        &self,
        stale_after_seconds: i64,
    ) -> Result<usize, SurveyRepositoryError> {
        self.recover_stale_deliveries_for_guild(None, stale_after_seconds)
    }

    pub fn recover_stale_deliveries_for_guild(
        &self,
        guild_id: Option<i64>,
        stale_after_seconds: i64,
    ) -> Result<usize, SurveyRepositoryError> {
        if stale_after_seconds < 0 {
            return Err(SurveyRepositoryError::Invalid(
                "stale_after_seconds cannot be negative".to_owned(),
            ));
        }
        let (mut connection, now) = self.immediate()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count = recover_stale(&transaction, guild_id, stale_after_seconds, now)?;
        transaction.commit()?;
        Ok(count)
    }

    pub fn list_unconfirmed_deliveries(
        &self,
        scope: Option<(i64, i64)>,
        sending_stale_after_seconds: i64,
        failed_grace_seconds: i64,
    ) -> Result<Vec<SurveyRecipient>, SurveyRepositoryError> {
        if sending_stale_after_seconds < 0 || failed_grace_seconds < 0 {
            return Err(SurveyRepositoryError::Invalid(
                "delivery receipt ages cannot be negative".to_owned(),
            ));
        }
        let connection = self.connection()?;
        let now = self.now();
        let base = "SELECT recipients.* FROM survey_recipients recipients
             JOIN surveys ON surveys.survey_id=recipients.survey_id
                         AND surveys.guild_id=recipients.guild_id
             WHERE surveys.status IN ('open','closed')
               AND recipients.delivery_status IN ('pending','sending','failed')
               AND recipients.dm_message_id IS NULL
               AND recipients.attempt_count>recipients.receipt_checked_attempt
               AND ((recipients.delivery_status='sending'
                     AND COALESCE(recipients.claimed_at,recipients.updated_at)<=?1)
                 OR (recipients.delivery_status IN ('pending','failed')
                     AND recipients.updated_at<=?2))";
        let mut recipients = Vec::new();
        if let Some((guild_id, survey_id)) = scope {
            let sql = format!(
                "{base} AND recipients.guild_id=?3 AND recipients.survey_id=?4
                 ORDER BY recipients.recipient_id"
            );
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(
                params![
                    now - sending_stale_after_seconds,
                    now - failed_grace_seconds,
                    guild_id,
                    survey_id
                ],
                recipient_from_row,
            )?;
            for row in rows {
                recipients.push(row??);
            }
        } else {
            let sql = format!("{base} ORDER BY recipients.recipient_id");
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(
                params![
                    now - sending_stale_after_seconds,
                    now - failed_grace_seconds
                ],
                recipient_from_row,
            )?;
            for row in rows {
                recipients.push(row??);
            }
        }
        Ok(recipients)
    }

    pub fn list_unconfirmed_deliveries_for_guild(
        &self,
        guild_id: i64,
        sending_stale_after_seconds: i64,
        failed_grace_seconds: i64,
    ) -> Result<Vec<SurveyRecipient>, SurveyRepositoryError> {
        if sending_stale_after_seconds < 0 || failed_grace_seconds < 0 {
            return Err(SurveyRepositoryError::Invalid(
                "delivery receipt ages cannot be negative".to_owned(),
            ));
        }
        let connection = self.connection()?;
        let now = self.now();
        let mut statement = connection.prepare(
            "SELECT recipients.* FROM survey_recipients recipients
             JOIN surveys ON surveys.survey_id=recipients.survey_id
                         AND surveys.guild_id=recipients.guild_id
             WHERE surveys.status IN ('open','closed')
               AND recipients.guild_id=?3
               AND recipients.delivery_status IN ('pending','sending','failed')
               AND recipients.dm_message_id IS NULL
               AND recipients.attempt_count>recipients.receipt_checked_attempt
               AND ((recipients.delivery_status='sending'
                     AND COALESCE(recipients.claimed_at,recipients.updated_at)<=?1)
                 OR (recipients.delivery_status IN ('pending','failed')
                     AND recipients.updated_at<=?2))
             ORDER BY recipients.recipient_id",
        )?;
        let rows = statement.query_map(
            params![
                now - sending_stale_after_seconds,
                now - failed_grace_seconds,
                guild_id
            ],
            recipient_from_row,
        )?;
        let mut recipients = Vec::new();
        for row in rows {
            recipients.push(row??);
        }
        Ok(recipients)
    }

    pub fn retry_failed_deliveries(
        &self,
        guild_id: i64,
        survey_id: i64,
    ) -> Result<usize, SurveyRepositoryError> {
        let (mut connection, now) = self.immediate()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if require_survey(&transaction, guild_id, survey_id)?.status != SurveyStatus::Open {
            return Err(SurveyRepositoryError::State(
                "Only an open survey can retry failed DMs".to_owned(),
            ));
        }
        let requested = transaction.execute(
            "UPDATE survey_recipients SET retry_requested=1
             WHERE guild_id=?1 AND survey_id=?2 AND delivery_status='failed'",
            params![guild_id, survey_id],
        )?;
        transaction.execute(
            "UPDATE survey_recipients SET delivery_status='pending',claimed_at=NULL,
             retry_requested=0,last_error=NULL,updated_at=?1
             WHERE guild_id=?2 AND survey_id=?3 AND delivery_status='failed'
               AND retry_requested=1 AND receipt_checked_attempt>=attempt_count",
            params![now, guild_id, survey_id],
        )?;
        transaction.commit()?;
        Ok(requested)
    }

    pub fn queue_requested_delivery_retries(&self) -> Result<usize, SurveyRepositoryError> {
        self.queue_requested_delivery_retries_for_guild(None)
    }

    pub fn queue_requested_delivery_retries_for_guild(
        &self,
        guild_id: Option<i64>,
    ) -> Result<usize, SurveyRepositoryError> {
        let (mut connection, now) = self.immediate()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let base = "UPDATE survey_recipients SET delivery_status='pending',claimed_at=NULL,
             retry_requested=0,last_error=NULL,updated_at=?1
             WHERE delivery_status='failed' AND retry_requested=1
               AND receipt_checked_attempt>=attempt_count AND EXISTS (
                 SELECT 1 FROM surveys WHERE surveys.survey_id=survey_recipients.survey_id
                   AND surveys.guild_id=survey_recipients.guild_id AND surveys.status='open')";
        let changed = if let Some(guild_id) = guild_id {
            transaction.execute(&format!("{base} AND guild_id=?2"), params![now, guild_id])?
        } else {
            transaction.execute(base, [now])?
        };
        transaction.commit()?;
        Ok(changed)
    }

    pub fn claim_deliveries(
        &self,
        limit: usize,
        stale_after_seconds: i64,
    ) -> Result<Vec<SurveyRecipient>, SurveyRepositoryError> {
        self.claim_deliveries_for_guild(None, limit, stale_after_seconds)
    }

    pub fn claim_deliveries_for_guild(
        &self,
        guild_id: Option<i64>,
        limit: usize,
        stale_after_seconds: i64,
    ) -> Result<Vec<SurveyRecipient>, SurveyRepositoryError> {
        self.claim_deliveries_for_scope(guild_id, None, limit, stale_after_seconds)
    }

    pub fn claim_deliveries_for_survey(
        &self,
        guild_id: i64,
        survey_id: i64,
        limit: usize,
        stale_after_seconds: i64,
    ) -> Result<Vec<SurveyRecipient>, SurveyRepositoryError> {
        self.claim_deliveries_for_scope(Some(guild_id), Some(survey_id), limit, stale_after_seconds)
    }

    fn claim_deliveries_for_scope(
        &self,
        guild_id: Option<i64>,
        survey_id: Option<i64>,
        limit: usize,
        stale_after_seconds: i64,
    ) -> Result<Vec<SurveyRecipient>, SurveyRepositoryError> {
        if limit == 0 {
            return Err(SurveyRepositoryError::Invalid(
                "Delivery claim limit must be positive".to_owned(),
            ));
        }
        if stale_after_seconds < 0 {
            return Err(SurveyRepositoryError::Invalid(
                "stale_after_seconds cannot be negative".to_owned(),
            ));
        }
        let (mut connection, now) = self.immediate()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        recover_stale(&transaction, guild_id, stale_after_seconds, now)?;
        let base = "SELECT recipients.* FROM survey_recipients recipients
             JOIN surveys ON surveys.survey_id=recipients.survey_id
                         AND surveys.guild_id=recipients.guild_id
             WHERE recipients.delivery_status='pending'
               AND recipients.receipt_checked_attempt>=recipients.attempt_count
               AND surveys.status='open'";
        let (sql, query_parameters) = if let (Some(guild_id), Some(survey_id)) =
            (guild_id, survey_id)
        {
            (
                format!(
                    "{base} AND recipients.guild_id=?1 AND recipients.survey_id=?2 ORDER BY recipients.recipient_id LIMIT ?3"
                ),
                vec![guild_id, survey_id, limit as i64],
            )
        } else if let Some(guild_id) = guild_id {
            (
                format!(
                    "{base} AND recipients.guild_id=?1 ORDER BY recipients.recipient_id LIMIT ?2"
                ),
                vec![guild_id, limit as i64],
            )
        } else {
            (
                format!("{base} ORDER BY recipients.recipient_id LIMIT ?1"),
                vec![limit as i64],
            )
        };
        let mut statement = transaction.prepare(&sql)?;
        let rows = statement.query_map(
            rusqlite::params_from_iter(query_parameters),
            recipient_from_row,
        )?;
        let mut pending = Vec::new();
        for row in rows {
            pending.push(row??);
        }
        drop(statement);
        let mut claimed = Vec::new();
        for recipient in pending {
            let changed = transaction.execute(
                "UPDATE survey_recipients SET delivery_status='sending',
                 attempt_count=attempt_count+1,claimed_at=?1,retry_requested=0,
                 last_error=NULL,updated_at=?1
                 WHERE recipient_id=?2 AND delivery_status='pending'",
                params![now, recipient.recipient_id],
            )?;
            if changed == 1 {
                claimed.push(require_recipient(&transaction, recipient.recipient_id)?);
            }
        }
        transaction.commit()?;
        Ok(claimed)
    }

    pub fn renew_delivery_claim(
        &self,
        recipient_id: i64,
        expected_attempt_count: u32,
    ) -> Result<SurveyRecipient, SurveyRepositoryError> {
        let (mut connection, now) = self.immediate()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE survey_recipients SET claimed_at=?1,updated_at=?1
             WHERE recipient_id=?2 AND delivery_status='sending' AND attempt_count=?3
               AND receipt_checked_attempt<attempt_count AND EXISTS (
                 SELECT 1 FROM surveys WHERE surveys.survey_id=survey_recipients.survey_id
                 AND surveys.guild_id=survey_recipients.guild_id AND surveys.status='open')",
            params![now, recipient_id, expected_attempt_count],
        )?;
        if changed != 1 {
            return Err(SurveyRepositoryError::State(
                "Survey delivery claim is no longer active".to_owned(),
            ));
        }
        let recipient = require_recipient(&transaction, recipient_id)?;
        transaction.commit()?;
        Ok(recipient)
    }

    pub fn mark_delivery_sent(
        &self,
        recipient_id: i64,
        expected_attempt_count: u32,
        dm_channel_id: i64,
        dm_message_id: i64,
    ) -> Result<SurveyRecipient, SurveyRepositoryError> {
        self.complete_delivery(
            recipient_id,
            expected_attempt_count,
            dm_channel_id,
            dm_message_id,
            false,
        )
    }

    pub fn reconcile_delivery_sent(
        &self,
        recipient_id: i64,
        expected_attempt_count: u32,
        dm_channel_id: i64,
        dm_message_id: i64,
    ) -> Result<SurveyRecipient, SurveyRepositoryError> {
        self.complete_delivery(
            recipient_id,
            expected_attempt_count,
            dm_channel_id,
            dm_message_id,
            true,
        )
    }

    fn complete_delivery(
        &self,
        recipient_id: i64,
        expected_attempt_count: u32,
        dm_channel_id: i64,
        dm_message_id: i64,
        allow_closed: bool,
    ) -> Result<SurveyRecipient, SurveyRepositoryError> {
        let (mut connection, now) = self.immediate()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = require_recipient(&transaction, recipient_id)?;
        if existing.delivery_status == SurveyDeliveryStatus::Sent
            && existing.attempt_count == expected_attempt_count
        {
            return Ok(existing);
        }
        let allowed_status = if allow_closed {
            matches!(
                existing.delivery_status,
                SurveyDeliveryStatus::Pending
                    | SurveyDeliveryStatus::Sending
                    | SurveyDeliveryStatus::Failed
            )
        } else {
            existing.delivery_status == SurveyDeliveryStatus::Sending
        };
        if !allowed_status || existing.attempt_count != expected_attempt_count {
            return Err(SurveyRepositoryError::State(
                "Survey delivery claim is no longer active".to_owned(),
            ));
        }
        let survey = require_survey(&transaction, existing.guild_id, existing.survey_id)?;
        if survey.status == SurveyStatus::Draft
            || (!allow_closed && survey.status != SurveyStatus::Open)
        {
            return Err(SurveyRepositoryError::State(if allow_closed {
                "Survey delivery campaign is unavailable".to_owned()
            } else {
                "Cannot complete delivery for a closed survey".to_owned()
            }));
        }
        let statuses = if allow_closed {
            "('pending','sending','failed')"
        } else {
            "('sending')"
        };
        let sql = format!(
            "UPDATE survey_recipients SET delivery_status='sent',claimed_at=NULL,
             dm_channel_id=?1,dm_message_id=?2,ui_channel_id=?1,ui_message_id=?2,
             controls_finalized=0,receipt_checked_attempt=attempt_count,
             retry_requested=0,last_error=NULL,updated_at=?3
             WHERE recipient_id=?4 AND delivery_status IN {statuses} AND attempt_count=?5"
        );
        if transaction.execute(
            &sql,
            params![
                dm_channel_id,
                dm_message_id,
                now,
                recipient_id,
                expected_attempt_count
            ],
        )? != 1
        {
            return Err(SurveyRepositoryError::State(
                "Survey delivery claim is no longer active".to_owned(),
            ));
        }
        let recipient = require_recipient(&transaction, recipient_id)?;
        transaction.commit()?;
        Ok(recipient)
    }

    pub fn mark_delivery_failed(
        &self,
        recipient_id: i64,
        expected_attempt_count: u32,
        error: &str,
    ) -> Result<SurveyRecipient, SurveyRepositoryError> {
        let (mut connection, now) = self.immediate()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = require_recipient(&transaction, recipient_id)?;
        if matches!(
            existing.delivery_status,
            SurveyDeliveryStatus::Sent | SurveyDeliveryStatus::Failed
        ) && existing.attempt_count == expected_attempt_count
        {
            return Ok(existing);
        }
        if existing.delivery_status != SurveyDeliveryStatus::Sending
            || existing.attempt_count != expected_attempt_count
        {
            return Err(SurveyRepositoryError::State(
                "Survey delivery claim is no longer active".to_owned(),
            ));
        }
        let clean_error = {
            let trimmed = error.trim();
            if trimmed.is_empty() {
                "DM delivery failed".to_owned()
            } else {
                trimmed.chars().take(500).collect()
            }
        };
        transaction.execute(
            "UPDATE survey_recipients SET delivery_status='failed',last_error=?1,updated_at=?2
             WHERE recipient_id=?3 AND delivery_status='sending' AND attempt_count=?4",
            params![clean_error, now, recipient_id, expected_attempt_count],
        )?;
        let recipient = require_recipient(&transaction, recipient_id)?;
        transaction.commit()?;
        Ok(recipient)
    }

    pub fn mark_delivery_receipt_checked(
        &self,
        snapshot: &SurveyRecipient,
    ) -> Result<bool, SurveyRepositoryError> {
        let (mut connection, now) = self.immediate()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE survey_recipients
             SET receipt_checked_attempt=?1,
                 delivery_status=CASE
                   WHEN delivery_status IN ('pending','sending') AND EXISTS (
                     SELECT 1 FROM surveys WHERE surveys.survey_id=survey_recipients.survey_id
                       AND surveys.guild_id=survey_recipients.guild_id AND surveys.status='closed')
                   THEN 'failed' ELSE delivery_status END,
                 claimed_at=CASE
                   WHEN delivery_status IN ('pending','sending') AND EXISTS (
                     SELECT 1 FROM surveys WHERE surveys.survey_id=survey_recipients.survey_id
                       AND surveys.guild_id=survey_recipients.guild_id AND surveys.status='closed')
                   THEN NULL ELSE claimed_at END,
                 updated_at=?2
             WHERE recipient_id=?3 AND delivery_status IN ('pending','sending','failed')
               AND attempt_count=?1 AND receipt_checked_attempt<?1
               AND claimed_at IS ?4 AND delivery_status=?5 AND updated_at=?6",
            params![
                snapshot.attempt_count,
                now,
                snapshot.recipient_id,
                snapshot.claimed_at,
                snapshot.delivery_status.as_str(),
                snapshot.updated_at
            ],
        )? == 1;
        transaction.commit()?;
        Ok(changed)
    }

    pub fn get_response_session(
        &self,
        survey_id: i64,
        discord_id: i64,
    ) -> Result<Option<SurveySession>, SurveyRepositoryError> {
        let connection = self.connection()?;
        connection.execute_batch("BEGIN")?;
        query_session(&connection, survey_id, discord_id)
    }

    pub fn list_recoverable_response_sessions(
        &self,
    ) -> Result<Vec<SurveySession>, SurveyRepositoryError> {
        self.list_recoverable_response_sessions_for_guild(None)
    }

    pub fn list_recoverable_response_sessions_for_guild(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Vec<SurveySession>, SurveyRepositoryError> {
        let connection = self.connection()?;
        connection.execute_batch("BEGIN")?;
        let base = "SELECT recipients.survey_id,recipients.discord_id
             FROM survey_recipients recipients
             JOIN surveys ON surveys.survey_id=recipients.survey_id
                         AND surveys.guild_id=recipients.guild_id
             WHERE surveys.status IN ('open','closed')
               AND recipients.delivery_status='sent'
               AND recipients.controls_finalized=0
               AND COALESCE(recipients.ui_message_id,recipients.dm_message_id) IS NOT NULL";
        let (sql, query_parameters) = if let Some(guild_id) = guild_id {
            (
                format!("{base} AND recipients.guild_id=?1 ORDER BY recipients.recipient_id"),
                vec![guild_id],
            )
        } else {
            (
                format!("{base} ORDER BY recipients.recipient_id"),
                Vec::new(),
            )
        };
        let mut statement = connection.prepare(&sql)?;
        let keys = statement
            .query_map(rusqlite::params_from_iter(query_parameters), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut sessions = Vec::new();
        for (survey_id, discord_id) in keys {
            if let Some(session) = query_session(&connection, survey_id, discord_id)? {
                sessions.push(session);
            }
        }
        Ok(sessions)
    }

    pub fn save_answer(
        &self,
        survey_id: i64,
        discord_id: i64,
        question_id: i64,
        answer: SurveyAnswer,
    ) -> Result<SurveySession, SurveyRepositoryError> {
        let (mut connection, now) = self.immediate()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let recipient = require_open_recipient(&transaction, survey_id, discord_id)?;
        let question = query_question(&transaction, recipient.guild_id, survey_id, question_id)?
            .ok_or_else(|| {
                SurveyRepositoryError::Response(
                    "Question does not belong to this survey".to_owned(),
                )
            })?;
        let (numeric_value, text_value) = validate_answer(&question, answer)?;
        if recipient.current_question_id != Some(question_id) {
            let existing = transaction
                .query_row(
                    "SELECT numeric_value,text_value,skipped FROM survey_answers
                     WHERE guild_id=?1 AND survey_id=?2 AND recipient_id=?3 AND question_id=?4",
                    params![
                        recipient.guild_id,
                        survey_id,
                        recipient.recipient_id,
                        question_id
                    ],
                    |row| {
                        Ok((
                            row.get::<_, Option<i32>>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, bool>(2)?,
                        ))
                    },
                )
                .optional()?;
            if existing.as_ref().is_some_and(|(numeric, text, skipped)| {
                !skipped && *numeric == numeric_value && *text == text_value
            }) {
                return query_session(&transaction, survey_id, discord_id)?.ok_or_else(|| {
                    SurveyRepositoryError::Corrupt(
                        "survey session disappeared after duplicate answer".to_owned(),
                    )
                });
            }
            if recipient.in_review && recipient.current_question_id.is_none() {
                return Err(SurveyRepositoryError::Response(
                    "Select a question from the review summary first".to_owned(),
                ));
            }
            return Err(SurveyRepositoryError::Response(
                "This is not the current survey question".to_owned(),
            ));
        }
        transaction.execute(
            "INSERT INTO survey_answers
             (survey_id,guild_id,recipient_id,question_id,numeric_value,text_value,
              skipped,created_at,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,0,?7,?7)
             ON CONFLICT(recipient_id,question_id) DO UPDATE SET
               numeric_value=excluded.numeric_value,text_value=excluded.text_value,
               skipped=0,updated_at=excluded.updated_at",
            params![
                survey_id,
                recipient.guild_id,
                recipient.recipient_id,
                question_id,
                numeric_value,
                text_value,
                now
            ],
        )?;
        advance_recipient(&transaction, &recipient, now)?;
        let session = query_session(&transaction, survey_id, discord_id)?.ok_or_else(|| {
            SurveyRepositoryError::Corrupt(
                "survey session disappeared after saving an answer".to_owned(),
            )
        })?;
        transaction.commit()?;
        Ok(session)
    }

    pub fn skip_question(
        &self,
        survey_id: i64,
        discord_id: i64,
        question_id: i64,
    ) -> Result<SurveySession, SurveyRepositoryError> {
        let (mut connection, now) = self.immediate()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let recipient = require_open_recipient(&transaction, survey_id, discord_id)?;
        let question = query_question(&transaction, recipient.guild_id, survey_id, question_id)?
            .ok_or_else(|| {
                SurveyRepositoryError::Response(
                    "Question does not belong to this survey".to_owned(),
                )
            })?;
        if recipient.current_question_id != Some(question_id) {
            let already_skipped = transaction
                .query_row(
                    "SELECT skipped FROM survey_answers WHERE guild_id=?1 AND survey_id=?2
                     AND recipient_id=?3 AND question_id=?4",
                    params![
                        recipient.guild_id,
                        survey_id,
                        recipient.recipient_id,
                        question_id
                    ],
                    |row| row.get::<_, bool>(0),
                )
                .optional()?
                .unwrap_or(false);
            if already_skipped {
                return query_session(&transaction, survey_id, discord_id)?.ok_or_else(|| {
                    SurveyRepositoryError::Corrupt(
                        "survey session disappeared after duplicate skip".to_owned(),
                    )
                });
            }
            if recipient.in_review && recipient.current_question_id.is_none() {
                return Err(SurveyRepositoryError::Response(
                    "Select a question from the review summary first".to_owned(),
                ));
            }
            return Err(SurveyRepositoryError::Response(
                "This is not the current survey question".to_owned(),
            ));
        }
        if question.required {
            return Err(SurveyRepositoryError::Response(
                "This question is required".to_owned(),
            ));
        }
        transaction.execute(
            "INSERT INTO survey_answers
             (survey_id,guild_id,recipient_id,question_id,numeric_value,text_value,
              skipped,created_at,updated_at)
             VALUES (?1,?2,?3,?4,NULL,NULL,1,?5,?5)
             ON CONFLICT(recipient_id,question_id) DO UPDATE SET
               numeric_value=NULL,text_value=NULL,skipped=1,updated_at=excluded.updated_at",
            params![
                survey_id,
                recipient.guild_id,
                recipient.recipient_id,
                question_id,
                now
            ],
        )?;
        advance_recipient(&transaction, &recipient, now)?;
        let session = query_session(&transaction, survey_id, discord_id)?.ok_or_else(|| {
            SurveyRepositoryError::Corrupt(
                "survey session disappeared after skipping a question".to_owned(),
            )
        })?;
        transaction.commit()?;
        Ok(session)
    }

    pub fn begin_review(
        &self,
        survey_id: i64,
        discord_id: i64,
    ) -> Result<SurveySession, SurveyRepositoryError> {
        let (mut connection, now) = self.immediate()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let recipient = require_open_recipient(&transaction, survey_id, discord_id)?;
        assert_response_complete(&transaction, &recipient)?;
        transaction.execute(
            "UPDATE survey_recipients SET current_question_id=NULL,in_review=1,updated_at=?1
             WHERE recipient_id=?2 AND submitted_at IS NULL",
            params![now, recipient.recipient_id],
        )?;
        let session = query_session(&transaction, survey_id, discord_id)?.ok_or_else(|| {
            SurveyRepositoryError::Corrupt(
                "survey session disappeared while entering review".to_owned(),
            )
        })?;
        transaction.commit()?;
        Ok(session)
    }

    pub fn select_response_question(
        &self,
        survey_id: i64,
        discord_id: i64,
        question_id: i64,
    ) -> Result<SurveySession, SurveyRepositoryError> {
        let (mut connection, now) = self.immediate()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let recipient = require_open_recipient(&transaction, survey_id, discord_id)?;
        if !recipient.in_review {
            return Err(SurveyRepositoryError::Response(
                "Finish the survey before editing review answers".to_owned(),
            ));
        }
        if query_question(&transaction, recipient.guild_id, survey_id, question_id)?.is_none() {
            return Err(SurveyRepositoryError::Response(
                "Question does not belong to this survey".to_owned(),
            ));
        }
        transaction.execute(
            "UPDATE survey_recipients SET current_question_id=?1,updated_at=?2
             WHERE recipient_id=?3 AND submitted_at IS NULL AND in_review=1",
            params![question_id, now, recipient.recipient_id],
        )?;
        let session = query_session(&transaction, survey_id, discord_id)?.ok_or_else(|| {
            SurveyRepositoryError::Corrupt(
                "survey session disappeared while selecting a review answer".to_owned(),
            )
        })?;
        transaction.commit()?;
        Ok(session)
    }

    pub fn submit_response(
        &self,
        survey_id: i64,
        discord_id: i64,
    ) -> Result<bool, SurveyRepositoryError> {
        let (mut connection, now) = self.immediate()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let recipient = query_recipient_by_session(&transaction, survey_id, discord_id)?
            .ok_or_else(|| {
                SurveyRepositoryError::Response("This survey was not sent to you".to_owned())
            })?;
        if recipient.submitted_at.is_some() {
            return Ok(false);
        }
        let survey = require_survey(&transaction, recipient.guild_id, survey_id)?;
        if survey.status != SurveyStatus::Open {
            return Err(SurveyRepositoryError::Response(
                "This survey is closed".to_owned(),
            ));
        }
        if recipient.delivery_status != SurveyDeliveryStatus::Sent {
            return Err(SurveyRepositoryError::Response(
                "This survey has not been delivered".to_owned(),
            ));
        }
        if !recipient.in_review {
            return Err(SurveyRepositoryError::Response(
                "Review your answers before submitting".to_owned(),
            ));
        }
        if recipient.current_question_id.is_some() {
            return Err(SurveyRepositoryError::Response(
                "Return to the review summary before submitting".to_owned(),
            ));
        }
        assert_response_complete(&transaction, &recipient)?;
        let submitted = transaction.execute(
            "UPDATE survey_recipients SET submitted_at=?1,current_question_id=NULL,
             in_review=1,updated_at=?1 WHERE recipient_id=?2 AND submitted_at IS NULL",
            params![now, recipient.recipient_id],
        )? == 1;
        transaction.commit()?;
        Ok(submitted)
    }

    pub fn finalize_response_ui(
        &self,
        survey_id: i64,
        discord_id: i64,
        expected_message_id: i64,
    ) -> Result<bool, SurveyRepositoryError> {
        let (mut connection, now) = self.immediate()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE survey_recipients SET controls_finalized=1,updated_at=?1
             WHERE survey_id=?2 AND discord_id=?3 AND controls_finalized=0
               AND COALESCE(ui_message_id,dm_message_id)=?4
               AND (submitted_at IS NOT NULL OR EXISTS (
                 SELECT 1 FROM surveys WHERE surveys.survey_id=survey_recipients.survey_id
                   AND surveys.guild_id=survey_recipients.guild_id AND surveys.status='closed'))",
            params![now, survey_id, discord_id, expected_message_id],
        )? == 1;
        transaction.commit()?;
        Ok(changed)
    }

    pub fn get_results(
        &self,
        guild_id: i64,
        survey_id: i64,
    ) -> Result<SurveyResults, SurveyRepositoryError> {
        self.get_results_with_hook(guild_id, survey_id, || {})
    }

    fn get_results_with_hook(
        &self,
        guild_id: i64,
        survey_id: i64,
        after_question_snapshot: impl FnOnce(),
    ) -> Result<SurveyResults, SurveyRepositoryError> {
        let connection = self.connection()?;
        connection.execute_batch("BEGIN")?;
        let survey = require_survey(&connection, guild_id, survey_id)?;
        let metrics = connection.query_row(
            "SELECT COUNT(*),
             SUM(CASE WHEN delivery_status='sent' THEN 1 ELSE 0 END),
             SUM(CASE WHEN started_at IS NOT NULL THEN 1 ELSE 0 END),
             SUM(CASE WHEN submitted_at IS NOT NULL THEN 1 ELSE 0 END)
             FROM survey_recipients WHERE guild_id=?1 AND survey_id=?2",
            params![guild_id, survey_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                ))
            },
        )?;
        let questions = query_questions(&connection, guild_id, survey_id)?;
        after_question_snapshot();
        let mut results = Vec::new();
        for question in questions {
            let mut statement = connection.prepare(
                "SELECT answers.numeric_value,answers.text_value,answers.skipped
                 FROM survey_answers answers JOIN survey_recipients recipients
                   ON recipients.recipient_id=answers.recipient_id
                  AND recipients.survey_id=answers.survey_id
                  AND recipients.guild_id=answers.guild_id
                 WHERE answers.guild_id=?1 AND answers.survey_id=?2
                   AND answers.question_id=?3 AND recipients.submitted_at IS NOT NULL
                 ORDER BY answers.answer_id",
            )?;
            let answers = statement
                .query_map(params![guild_id, survey_id, question.question_id], |row| {
                    Ok((
                        row.get::<_, Option<i32>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            results.push(aggregate_question(question, &answers));
        }
        let (recipient_count, delivered_count, started_count, completed_count) = metrics;
        Ok(SurveyResults {
            survey,
            recipient_count: recipient_count as usize,
            delivered_count: delivered_count as usize,
            started_count: started_count as usize,
            completed_count: completed_count as usize,
            delivery_rate: rate(delivered_count, recipient_count),
            started_rate: rate(started_count, delivered_count),
            completion_rate: rate(completed_count, started_count),
            response_rate: rate(completed_count, delivered_count),
            questions: results,
        })
    }
}

fn checked_prompt(prompt: &str) -> Result<String, SurveyRepositoryError> {
    let prompt = prompt.trim();
    if prompt.is_empty() || prompt.chars().count() > 1_000 {
        Err(SurveyRepositoryError::Invalid(
            "Survey questions must contain 1 to 1000 characters".to_owned(),
        ))
    } else {
        Ok(prompt.to_owned())
    }
}

fn survey_from_row(row: &Row<'_>) -> rusqlite::Result<Result<Survey, SurveyRepositoryError>> {
    let status = row.get::<_, String>("status")?;
    let target = row.get::<_, Option<String>>("target_type")?;
    Ok((|| {
        Ok(Survey {
            survey_id: row.get("survey_id")?,
            guild_id: row.get("guild_id")?,
            title: row.get("title")?,
            status: SurveyStatus::parse(&status)?,
            target_type: target.as_deref().map(SurveyTargetType::parse).transpose()?,
            registered_only: row.get("registered_only")?,
            created_at: row.get("created_at")?,
            opened_at: row.get("opened_at")?,
            closed_at: row.get("closed_at")?,
        })
    })())
}

fn question_from_row(
    row: &Row<'_>,
) -> rusqlite::Result<Result<SurveyQuestion, SurveyRepositoryError>> {
    let question_type = row.get::<_, String>("question_type")?;
    Ok((|| {
        Ok(SurveyQuestion {
            question_id: row.get("question_id")?,
            survey_id: row.get("survey_id")?,
            guild_id: row.get("guild_id")?,
            position: usize::try_from(row.get::<_, i64>("position")?).map_err(|error| {
                SurveyRepositoryError::Corrupt(format!("invalid question position: {error}"))
            })?,
            prompt: row.get("prompt")?,
            question_type: SurveyQuestionType::parse(&question_type)?,
            required: row.get("required")?,
            created_at: row.get("created_at")?,
        })
    })())
}

fn recipient_from_row(
    row: &Row<'_>,
) -> rusqlite::Result<Result<SurveyRecipient, SurveyRepositoryError>> {
    let status = row.get::<_, String>("delivery_status")?;
    Ok((|| {
        Ok(SurveyRecipient {
            recipient_id: row.get("recipient_id")?,
            survey_id: row.get("survey_id")?,
            guild_id: row.get("guild_id")?,
            discord_id: row.get("discord_id")?,
            delivery_status: SurveyDeliveryStatus::parse(&status)?,
            attempt_count: row.get("attempt_count")?,
            receipt_checked_attempt: row.get("receipt_checked_attempt")?,
            retry_requested: row.get("retry_requested")?,
            claimed_at: row.get("claimed_at")?,
            dm_channel_id: row.get("dm_channel_id")?,
            dm_message_id: row.get("dm_message_id")?,
            ui_channel_id: row.get("ui_channel_id")?,
            ui_message_id: row.get("ui_message_id")?,
            controls_finalized: row.get("controls_finalized")?,
            current_question_id: row.get("current_question_id")?,
            in_review: row.get("in_review")?,
            started_at: row.get("started_at")?,
            submitted_at: row.get("submitted_at")?,
            last_error: row.get("last_error")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    })())
}

fn answer_from_row(
    row: &Row<'_>,
) -> rusqlite::Result<Result<SurveyDraftAnswer, SurveyRepositoryError>> {
    Ok(Ok(SurveyDraftAnswer {
        question_id: row.get("question_id")?,
        numeric_value: row.get("numeric_value")?,
        text_value: row.get("text_value")?,
        skipped: row.get("skipped")?,
        updated_at: row.get("updated_at")?,
    }))
}

fn query_survey(
    connection: &Connection,
    guild_id: i64,
    survey_id: i64,
) -> Result<Option<Survey>, SurveyRepositoryError> {
    connection
        .query_row(
            "SELECT * FROM surveys WHERE guild_id=?1 AND survey_id=?2",
            params![guild_id, survey_id],
            survey_from_row,
        )
        .optional()?
        .transpose()
}

fn require_survey(
    connection: &Connection,
    guild_id: i64,
    survey_id: i64,
) -> Result<Survey, SurveyRepositoryError> {
    query_survey(connection, guild_id, survey_id)?.ok_or_else(|| {
        SurveyRepositoryError::NotFound("Survey not found in this server".to_owned())
    })
}

fn require_draft(
    connection: &Connection,
    guild_id: i64,
    survey_id: i64,
) -> Result<Survey, SurveyRepositoryError> {
    let survey = require_survey(connection, guild_id, survey_id)?;
    if survey.status != SurveyStatus::Draft {
        return Err(SurveyRepositoryError::State(
            "Survey questions are immutable after the survey is sent".to_owned(),
        ));
    }
    Ok(survey)
}

fn query_questions(
    connection: &Connection,
    guild_id: i64,
    survey_id: i64,
) -> Result<Vec<SurveyQuestion>, SurveyRepositoryError> {
    let mut statement = connection.prepare(
        "SELECT * FROM survey_questions WHERE guild_id=?1 AND survey_id=?2
         ORDER BY position,question_id",
    )?;
    let rows = statement.query_map(params![guild_id, survey_id], question_from_row)?;
    let mut questions = Vec::new();
    for row in rows {
        questions.push(row??);
    }
    Ok(questions)
}

fn query_question(
    connection: &Connection,
    guild_id: i64,
    survey_id: i64,
    question_id: i64,
) -> Result<Option<SurveyQuestion>, SurveyRepositoryError> {
    connection
        .query_row(
            "SELECT * FROM survey_questions
             WHERE guild_id=?1 AND survey_id=?2 AND question_id=?3",
            params![guild_id, survey_id, question_id],
            question_from_row,
        )
        .optional()?
        .transpose()
}

fn replace_question_order(
    transaction: &Transaction<'_>,
    questions: &[SurveyQuestion],
    guild_id: i64,
    survey_id: i64,
) -> Result<(), SurveyRepositoryError> {
    transaction.execute(
        "DELETE FROM survey_questions WHERE guild_id=?1 AND survey_id=?2",
        params![guild_id, survey_id],
    )?;
    for (index, question) in questions.iter().enumerate() {
        transaction.execute(
            "INSERT INTO survey_questions
             (question_id,survey_id,guild_id,position,prompt,question_type,required,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                question.question_id,
                survey_id,
                guild_id,
                (index + 1) as i64,
                question.prompt,
                question.question_type.as_str(),
                question.required,
                question.created_at
            ],
        )?;
    }
    Ok(())
}

fn recover_stale(
    transaction: &Transaction<'_>,
    guild_id: Option<i64>,
    stale_after_seconds: i64,
    now: i64,
) -> Result<usize, SurveyRepositoryError> {
    let base =
        "UPDATE survey_recipients SET delivery_status='pending',claimed_at=NULL,updated_at=?1
         WHERE delivery_status='sending' AND claimed_at<=?2
           AND receipt_checked_attempt>=attempt_count AND EXISTS (
             SELECT 1 FROM surveys WHERE surveys.survey_id=survey_recipients.survey_id
               AND surveys.guild_id=survey_recipients.guild_id AND surveys.status='open')";
    Ok(if let Some(guild_id) = guild_id {
        transaction.execute(
            &format!("{base} AND guild_id=?3"),
            params![now, now - stale_after_seconds, guild_id],
        )?
    } else {
        transaction.execute(base, params![now, now - stale_after_seconds])?
    })
}

fn require_recipient(
    connection: &Connection,
    recipient_id: i64,
) -> Result<SurveyRecipient, SurveyRepositoryError> {
    connection
        .query_row(
            "SELECT * FROM survey_recipients WHERE recipient_id=?1",
            [recipient_id],
            recipient_from_row,
        )
        .optional()?
        .transpose()?
        .ok_or_else(|| SurveyRepositoryError::NotFound("Survey recipient not found".to_owned()))
}

fn query_recipient_by_session(
    connection: &Connection,
    survey_id: i64,
    discord_id: i64,
) -> Result<Option<SurveyRecipient>, SurveyRepositoryError> {
    connection
        .query_row(
            "SELECT recipients.* FROM survey_recipients recipients
             JOIN surveys ON surveys.survey_id=recipients.survey_id
                         AND surveys.guild_id=recipients.guild_id
             WHERE recipients.survey_id=?1 AND recipients.discord_id=?2",
            params![survey_id, discord_id],
            recipient_from_row,
        )
        .optional()?
        .transpose()
}

fn require_open_recipient(
    connection: &Connection,
    survey_id: i64,
    discord_id: i64,
) -> Result<SurveyRecipient, SurveyRepositoryError> {
    let recipient =
        query_recipient_by_session(connection, survey_id, discord_id)?.ok_or_else(|| {
            SurveyRepositoryError::Response("This survey was not sent to you".to_owned())
        })?;
    let survey = require_survey(connection, recipient.guild_id, survey_id)?;
    if survey.status != SurveyStatus::Open {
        return Err(SurveyRepositoryError::Response(
            "This survey is closed".to_owned(),
        ));
    }
    if recipient.delivery_status != SurveyDeliveryStatus::Sent {
        return Err(SurveyRepositoryError::Response(
            "This survey has not been delivered".to_owned(),
        ));
    }
    if recipient.submitted_at.is_some() {
        return Err(SurveyRepositoryError::Response(
            "This response has already been submitted".to_owned(),
        ));
    }
    Ok(recipient)
}

fn validate_answer(
    question: &SurveyQuestion,
    answer: SurveyAnswer,
) -> Result<(Option<i32>, Option<String>), SurveyRepositoryError> {
    match (question.question_type, answer) {
        (SurveyQuestionType::Nps, SurveyAnswer::Numeric(value)) if (0..=10).contains(&value) => {
            Ok((Some(value), None))
        }
        (SurveyQuestionType::Nps, _) => Err(SurveyRepositoryError::Response(
            "NPS answers must be a whole number from 0 to 10".to_owned(),
        )),
        (SurveyQuestionType::Rating, SurveyAnswer::Numeric(value)) if (1..=5).contains(&value) => {
            Ok((Some(value), None))
        }
        (SurveyQuestionType::Rating, _) => Err(SurveyRepositoryError::Response(
            "Rating answers must be a whole number from 1 to 5".to_owned(),
        )),
        (SurveyQuestionType::Text, SurveyAnswer::Text(value)) => {
            let value = value.trim();
            if value.is_empty() || value.chars().count() > 1_000 {
                Err(SurveyRepositoryError::Response(
                    "Text answers must contain 1 to 1000 characters".to_owned(),
                ))
            } else {
                Ok((None, Some(value.to_owned())))
            }
        }
        (SurveyQuestionType::Text, _) => Err(SurveyRepositoryError::Response(
            "Text questions require a text answer".to_owned(),
        )),
    }
}

fn advance_recipient(
    transaction: &Transaction<'_>,
    recipient: &SurveyRecipient,
    now: i64,
) -> Result<(), SurveyRepositoryError> {
    let next_question_id = transaction
        .query_row(
            "SELECT questions.question_id FROM survey_questions questions
             WHERE questions.guild_id=?1 AND questions.survey_id=?2 AND NOT EXISTS (
               SELECT 1 FROM survey_answers answers
               WHERE answers.guild_id=questions.guild_id
                 AND answers.survey_id=questions.survey_id
                 AND answers.question_id=questions.question_id
                 AND answers.recipient_id=?3)
             ORDER BY questions.position LIMIT 1",
            params![
                recipient.guild_id,
                recipient.survey_id,
                recipient.recipient_id
            ],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let in_review = recipient.in_review || next_question_id.is_none();
    transaction.execute(
        "UPDATE survey_recipients SET current_question_id=?1,in_review=?2,
         started_at=COALESCE(started_at,?3),updated_at=?3
         WHERE recipient_id=?4 AND submitted_at IS NULL",
        params![
            if in_review { None } else { next_question_id },
            in_review,
            now,
            recipient.recipient_id
        ],
    )?;
    Ok(())
}

fn assert_response_complete(
    connection: &Connection,
    recipient: &SurveyRecipient,
) -> Result<(), SurveyRepositoryError> {
    let (question_count, answer_count, missing_required) = connection.query_row(
        "SELECT COUNT(*),
         SUM(CASE WHEN answers.answer_id IS NOT NULL THEN 1 ELSE 0 END),
         SUM(CASE WHEN questions.required=1 AND
           (answers.answer_id IS NULL OR answers.skipped=1) THEN 1 ELSE 0 END)
         FROM survey_questions questions LEFT JOIN survey_answers answers
           ON answers.guild_id=questions.guild_id
          AND answers.survey_id=questions.survey_id
          AND answers.question_id=questions.question_id
          AND answers.recipient_id=?1
         WHERE questions.guild_id=?2 AND questions.survey_id=?3",
        params![
            recipient.recipient_id,
            recipient.guild_id,
            recipient.survey_id
        ],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                row.get::<_, Option<i64>>(2)?.unwrap_or(0),
            ))
        },
    )?;
    if question_count == 0 || answer_count != question_count || missing_required != 0 {
        return Err(SurveyRepositoryError::Response(
            "Answer or skip every question before reviewing".to_owned(),
        ));
    }
    Ok(())
}

fn rate(numerator: i64, denominator: i64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn round_ratio_to_nearest(numerator: i64, denominator: i64) -> i32 {
    debug_assert!(denominator > 0);
    let absolute = numerator.unsigned_abs() as i64;
    let mut quotient = absolute / denominator;
    if (absolute % denominator) * 2 >= denominator {
        quotient += 1;
    }
    if numerator < 0 {
        -quotient as i32
    } else {
        quotient as i32
    }
}

fn aggregate_question(
    question: SurveyQuestion,
    answers: &[(Option<i32>, Option<String>, bool)],
) -> SurveyQuestionResult {
    let skipped_count = answers.iter().filter(|(_, _, skipped)| *skipped).count();
    let answered = answers
        .iter()
        .filter(|(_, _, skipped)| !skipped)
        .collect::<Vec<_>>();
    let mut result = SurveyQuestionResult {
        question_id: question.question_id,
        position: question.position,
        prompt: question.prompt,
        question_type: question.question_type,
        response_count: answered.len(),
        skipped_count,
        nps_score: None,
        promoters: 0,
        passives: 0,
        detractors: 0,
        rating_average: None,
        rating_distribution: [0; 5],
        text_responses: Vec::new(),
    };
    match question.question_type {
        SurveyQuestionType::Nps => {
            let values = answered
                .iter()
                .filter_map(|(numeric, _, _)| numeric.filter(|value| (0..=10).contains(value)))
                .collect::<Vec<_>>();
            result.promoters = values.iter().filter(|value| **value >= 9).count();
            result.passives = values
                .iter()
                .filter(|value| (7..=8).contains(*value))
                .count();
            result.detractors = values.iter().filter(|value| **value <= 6).count();
            if !values.is_empty() {
                result.nps_score = Some(round_ratio_to_nearest(
                    100 * (result.promoters as i64 - result.detractors as i64),
                    values.len() as i64,
                ));
            }
        }
        SurveyQuestionType::Rating => {
            let values = answered
                .iter()
                .filter_map(|(numeric, _, _)| numeric.filter(|value| (1..=5).contains(value)))
                .collect::<Vec<_>>();
            if !values.is_empty() {
                result.rating_average = Some(
                    values.iter().map(|value| f64::from(*value)).sum::<f64>() / values.len() as f64,
                );
            }
            for value in values {
                result.rating_distribution[value as usize - 1] += 1;
            }
        }
        SurveyQuestionType::Text => {
            result.text_responses = answered
                .iter()
                .filter_map(|(_, text, _)| text.clone())
                .collect();
            result.text_responses.sort();
        }
    }
    result
}

fn query_session(
    connection: &Connection,
    survey_id: i64,
    discord_id: i64,
) -> Result<Option<SurveySession>, SurveyRepositoryError> {
    query_session_with_hook(connection, survey_id, discord_id, || {})
}

fn query_session_with_hook(
    connection: &Connection,
    survey_id: i64,
    discord_id: i64,
    after_question_snapshot: impl FnOnce(),
) -> Result<Option<SurveySession>, SurveyRepositoryError> {
    let recipient = query_recipient_by_session(connection, survey_id, discord_id)?;
    let Some(recipient) = recipient else {
        return Ok(None);
    };
    let Some(survey) = query_survey(connection, recipient.guild_id, survey_id)? else {
        return Ok(None);
    };
    let questions = query_questions(connection, recipient.guild_id, survey_id)?;
    after_question_snapshot();
    let mut statement = connection.prepare(
        "SELECT answers.* FROM survey_answers answers
         JOIN survey_questions questions ON questions.question_id=answers.question_id
           AND questions.survey_id=answers.survey_id AND questions.guild_id=answers.guild_id
         WHERE answers.guild_id=?1 AND answers.survey_id=?2 AND answers.recipient_id=?3
         ORDER BY questions.position",
    )?;
    let rows = statement.query_map(
        params![recipient.guild_id, survey_id, recipient.recipient_id],
        answer_from_row,
    )?;
    let mut answers = Vec::new();
    for row in rows {
        answers.push(row??);
    }
    Ok(Some(SurveySession {
        survey,
        recipient,
        questions,
        answers,
    }))
}

#[cfg(test)]
#[path = "survey/tests.rs"]
mod tests;
