//! Discord-neutral orchestration for the classic `/trivia` command.
//!
//! Question generation reuses [`crate::trivia_questions`]. This module owns
//! streak rewards, cooldown/session lifecycle, and the transition barrier that
//! runs message cleanup, required edits, and next-question preparation in
//! parallel before sending the next question. Discord and image-cache handles
//! remain behind ports.

use std::collections::BTreeMap;
use std::thread;

use cama_db::trivia_commands_repository::TriviaCommandsRepository;
use thiserror::Error;

use crate::trivia_questions::TriviaQuestion;

pub type DiscordId = i64;
pub type GuildId = i64;
pub type MessageId = u64;

pub const DEFAULT_TRIVIA_REWARD: i64 = 1;
pub const DEFAULT_COOLDOWN_SECONDS: i64 = 21_600;

#[must_use]
pub const fn streak_bonus_for_streak(streak: i64) -> i64 {
    if streak == 10 {
        2
    } else if streak == 3 || streak == 6 || (streak > 10 && (streak - 10) % 4 == 0) {
        1
    } else {
        0
    }
}

#[must_use]
pub const fn jc_for_streak(streak: i64) -> i64 {
    DEFAULT_TRIVIA_REWARD + streak_bonus_for_streak(streak)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TriviaSession {
    pub user_id: DiscordId,
    pub guild_id: GuildId,
    pub streak: i64,
    pub total_jc: i64,
    pub recent_categories: Vec<&'static str>,
    pub current_message_id: Option<MessageId>,
    pub previous_message_id: Option<MessageId>,
    pub active: bool,
    pub answered: bool,
}

impl TriviaSession {
    #[must_use]
    pub const fn new(user_id: DiscordId, guild_id: GuildId) -> Self {
        Self {
            user_id,
            guild_id,
            streak: 0,
            total_jc: 0,
            recent_categories: Vec::new(),
            current_message_id: None,
            previous_message_id: None,
            active: true,
            answered: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TriviaAward {
    pub streak: i64,
    pub base_reward: i64,
    pub streak_bonus: i64,
    pub scaled_reward: i64,
    pub awarded_jc: i64,
}

/// Reward boundary. Daily-event adjustment returns `None` when no event
/// service exists and an error when an available service fails.
pub trait TriviaRewardPort: Sync {
    fn scale_minigame_reward(&self, amount: i64) -> i64;

    fn adjust_daily_event(&self, guild_id: GuildId, amount: i64) -> Result<Option<i64>, String>;

    fn credit_reward(
        &self,
        discord_id: DiscordId,
        guild_id: GuildId,
        amount: i64,
    ) -> Result<(), String>;
}

impl TriviaRewardPort for TriviaCommandsRepository {
    fn scale_minigame_reward(&self, amount: i64) -> i64 {
        amount
    }

    fn adjust_daily_event(&self, _guild_id: GuildId, _amount: i64) -> Result<Option<i64>, String> {
        Ok(None)
    }

    fn credit_reward(
        &self,
        discord_id: DiscordId,
        guild_id: GuildId,
        amount: i64,
    ) -> Result<(), String> {
        self.adjust_balance(discord_id, Some(guild_id), amount)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

/// Apply one correct answer. Scaling occurs exactly once, then the optional
/// daily event runs on a worker thread. Event failure falls back to the scaled
/// reward; credit failure reports zero awarded JC and never inflates the
/// session total.
pub fn award_correct_answer(
    port: &impl TriviaRewardPort,
    session: &mut TriviaSession,
    category: &'static str,
    base_reward: i64,
) -> TriviaAward {
    session.streak += 1;
    session.recent_categories.push(category);
    let streak_bonus = streak_bonus_for_streak(session.streak);
    let scaled_reward = port.scale_minigame_reward(base_reward + streak_bonus);
    let event_result = thread::scope(|scope| {
        scope
            .spawn(|| port.adjust_daily_event(session.guild_id, scaled_reward))
            .join()
    });
    let adjusted = match event_result {
        Ok(Ok(Some(adjusted))) => adjusted,
        Ok(Ok(None) | Err(_)) | Err(_) => scaled_reward,
    };
    let awarded_jc = if adjusted > 0
        && port
            .credit_reward(session.user_id, session.guild_id, adjusted)
            .is_ok()
    {
        adjusted
    } else {
        0
    };
    session.total_jc += awarded_jc;
    TriviaAward {
        streak: session.streak,
        base_reward,
        streak_bonus,
        scaled_reward,
        awarded_jc,
    }
}

pub trait TriviaSessionPort {
    fn try_claim(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
        now: i64,
        cooldown_seconds: i64,
    ) -> Result<bool, String>;

    fn reset_cooldown(&mut self, discord_id: DiscordId, guild_id: GuildId) -> Result<bool, String>;

    fn record_session(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
        streak: i64,
        jc_earned: i64,
        played_at: i64,
    ) -> Result<(), String>;
}

impl TriviaSessionPort for TriviaCommandsRepository {
    fn try_claim(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
        now: i64,
        cooldown_seconds: i64,
    ) -> Result<bool, String> {
        self.try_claim_trivia_session(discord_id, Some(guild_id), now, cooldown_seconds)
            .map_err(|error| error.to_string())
    }

    fn reset_cooldown(&mut self, discord_id: DiscordId, guild_id: GuildId) -> Result<bool, String> {
        self.reset_trivia_cooldown(discord_id, Some(guild_id))
            .map_err(|error| error.to_string())
    }

    fn record_session(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
        streak: i64,
        jc_earned: i64,
        played_at: i64,
    ) -> Result<(), String> {
        self.record_trivia_session(discord_id, Some(guild_id), streak, jc_earned, played_at)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum TriviaCommandError {
    #[error("a trivia session is already active")]
    ActiveSession,
    #[error("trivia is on cooldown")]
    Cooldown,
    #[error("interaction defer failed")]
    DeferFailed,
    #[error("trivia session does not exist")]
    MissingSession,
    #[error("trivia persistence failed: {0}")]
    Persistence(String),
}

#[derive(Clone, Debug)]
pub struct TriviaCommandService<P> {
    persistence: P,
    sessions: BTreeMap<(DiscordId, GuildId), TriviaSession>,
}

impl<P> TriviaCommandService<P> {
    #[must_use]
    pub fn new(persistence: P) -> Self {
        Self {
            persistence,
            sessions: BTreeMap::new(),
        }
    }

    #[must_use]
    pub const fn persistence(&self) -> &P {
        &self.persistence
    }

    #[must_use]
    pub const fn persistence_mut(&mut self) -> &mut P {
        &mut self.persistence
    }

    #[must_use]
    pub fn session(&self, discord_id: DiscordId, guild_id: GuildId) -> Option<&TriviaSession> {
        self.sessions.get(&(discord_id, guild_id))
    }

    pub fn session_mut(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> Option<&mut TriviaSession> {
        self.sessions.get_mut(&(discord_id, guild_id))
    }
}

impl<P: TriviaSessionPort> TriviaCommandService<P> {
    pub fn start_session_with_defer(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
        now: i64,
        cooldown_seconds: i64,
        is_admin: bool,
        defer: impl FnOnce() -> bool,
    ) -> Result<&TriviaSession, TriviaCommandError> {
        let key = (discord_id, guild_id);
        if self
            .sessions
            .get(&key)
            .is_some_and(|session| session.active)
        {
            return Err(TriviaCommandError::ActiveSession);
        }
        let claimed = if is_admin {
            false
        } else {
            self.persistence
                .try_claim(discord_id, guild_id, now, cooldown_seconds)
                .map_err(TriviaCommandError::Persistence)?
        };
        if !is_admin && !claimed {
            return Err(TriviaCommandError::Cooldown);
        }
        if !defer() {
            if claimed {
                self.persistence
                    .reset_cooldown(discord_id, guild_id)
                    .map_err(TriviaCommandError::Persistence)?;
            }
            return Err(TriviaCommandError::DeferFailed);
        }
        self.sessions
            .insert(key, TriviaSession::new(discord_id, guild_id));
        Ok(self.sessions.get(&key).expect("session was just inserted"))
    }

    pub fn end_session(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
        played_at: i64,
    ) -> Result<TriviaSession, TriviaCommandError> {
        let mut session = self
            .sessions
            .remove(&(discord_id, guild_id))
            .ok_or(TriviaCommandError::MissingSession)?;
        session.active = false;
        if session.streak > 0 {
            self.persistence
                .record_session(
                    discord_id,
                    guild_id,
                    session.streak,
                    session.total_jc,
                    played_at,
                )
                .map_err(TriviaCommandError::Persistence)?;
        }
        Ok(session)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedAsset {
    pub id: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedQuestion {
    pub question: TriviaQuestion,
    pub asset: Option<PreparedAsset>,
}

pub trait TriviaTransitionPort: Sync {
    fn delete_previous(&self, message_id: Option<MessageId>) -> Result<(), String>;

    fn edit_correct(&self, message_id: Option<MessageId>) -> Result<(), String>;

    fn edit_game_over(&self, message_id: Option<MessageId>, timed_out: bool) -> Result<(), String>;

    fn generate_next(
        &self,
        streak: i64,
        recent_categories: &[&'static str],
    ) -> Result<Option<TriviaQuestion>, String>;

    fn prepare_asset(&self, question: &TriviaQuestion) -> Result<Option<PreparedAsset>, String>;

    fn discard_asset(&self, asset: PreparedAsset);

    fn send_next(&self, prepared: PreparedQuestion) -> Result<MessageId, String>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorrectTransition {
    NextQuestion { message_id: MessageId },
    SessionEnded,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum TriviaTransitionError {
    #[error("required result edit failed: {0}")]
    RequiredEdit(String),
    #[error("next-question preparation failed: {0}")]
    Preparation(String),
    #[error("next-question send failed: {0}")]
    Send(String),
    #[error("trivia transition worker panicked")]
    WorkerPanic,
}

pub fn run_correct_transition(
    port: &impl TriviaTransitionPort,
    session: &mut TriviaSession,
) -> Result<CorrectTransition, TriviaTransitionError> {
    let previous_message = session.previous_message_id;
    let current_message = session.current_message_id;
    let streak = session.streak;
    let categories = session.recent_categories.clone();
    let (delete_result, edit_result, preparation_result) = thread::scope(|scope| {
        let delete = scope.spawn(|| port.delete_previous(previous_message));
        let edit = scope.spawn(|| port.edit_correct(current_message));
        let prepare = scope.spawn(|| {
            let Some(question) = port.generate_next(streak, &categories)? else {
                return Ok(None);
            };
            let asset = port.prepare_asset(&question)?;
            Ok::<_, String>(Some(PreparedQuestion { question, asset }))
        });
        (delete.join(), edit.join(), prepare.join())
    });

    let _ = delete_result.map_err(|_| TriviaTransitionError::WorkerPanic)?;
    let edit_result = edit_result.map_err(|_| TriviaTransitionError::WorkerPanic)?;
    let preparation_result = preparation_result.map_err(|_| TriviaTransitionError::WorkerPanic)?;
    if let Err(error) = edit_result {
        if let Ok(Some(prepared)) = preparation_result
            && let Some(asset) = prepared.asset
        {
            port.discard_asset(asset);
        }
        return Err(TriviaTransitionError::RequiredEdit(error));
    }
    session.previous_message_id = current_message;
    let prepared = preparation_result.map_err(TriviaTransitionError::Preparation)?;
    let Some(prepared) = prepared else {
        session.active = false;
        return Ok(CorrectTransition::SessionEnded);
    };
    let message_id = port
        .send_next(prepared)
        .map_err(TriviaTransitionError::Send)?;
    session.current_message_id = Some(message_id);
    Ok(CorrectTransition::NextQuestion { message_id })
}

pub fn run_wrong_transition(
    port: &impl TriviaTransitionPort,
    session: &mut TriviaSession,
) -> Result<(), TriviaTransitionError> {
    let previous_message = session.previous_message_id;
    let current_message = session.current_message_id;
    let (delete_result, edit_result) = thread::scope(|scope| {
        let delete = scope.spawn(|| port.delete_previous(previous_message));
        let edit = scope.spawn(|| port.edit_game_over(current_message, false));
        (delete.join(), edit.join())
    });
    let _ = delete_result.map_err(|_| TriviaTransitionError::WorkerPanic)?;
    edit_result
        .map_err(|_| TriviaTransitionError::WorkerPanic)?
        .map_err(TriviaTransitionError::RequiredEdit)?;
    session.active = false;
    Ok(())
}

pub fn run_timeout_transition(
    port: &impl TriviaTransitionPort,
    session: &mut TriviaSession,
) -> Result<(), TriviaTransitionError> {
    session.answered = true;
    let previous_message = session.previous_message_id;
    let current_message = session.current_message_id;
    let (delete_result, edit_result) = thread::scope(|scope| {
        let delete = scope.spawn(|| port.delete_previous(previous_message));
        let edit = scope.spawn(|| port.edit_game_over(current_message, true));
        (delete.join(), edit.join())
    });
    let _ = delete_result.map_err(|_| TriviaTransitionError::WorkerPanic)?;
    edit_result
        .map_err(|_| TriviaTransitionError::WorkerPanic)?
        .map_err(TriviaTransitionError::RequiredEdit)?;
    session.active = false;
    Ok(())
}

#[cfg(test)]
#[path = "trivia_commands/tests.rs"]
mod tests;
