//! Discord-independent duel command orchestration.
//!
//! The Python command cog mixes application decisions with `discord.py`
//! transport objects. This module preserves the decisions behind explicit
//! interaction, message, member, and scheduler ports. A live Discord adapter
//! is intentionally out of scope during side-by-side parity; tests use the
//! same typed requests, responses, embeds, views, and channel rules.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use cama_db::duel_repository::{
    DuelChallenge, DuelChallengeRepository, DuelDueKind, DuelDueResult, DuelRepositoryError,
    DuelResolution, DuelStatus, DuelTrial,
};
use cama_db::profit_deductions::{ProfitDeductionPolicy, ProfitDeductions};
use cama_domain::pet_evolution::PetActivity;
use thiserror::Error;

pub const MAIN_CHANNEL_NAME: &str = "dota-mm";
pub const RECOVERY_EDIT_CONCURRENCY: usize = 5;
pub const CONFIGURED_RESCUE_RETRY_SECONDS: i64 = 600;
pub const CONFIGURED_RESCUE_CACHE_SECONDS: i64 = 300;
pub const RESPONSE_SECONDS: i64 = 7 * 86_400;
pub const CHALLENGER_COOLDOWN_SECONDS: i64 = 15 * 86_400;
pub const RECIPIENT_COOLDOWN_SECONDS: i64 = 7 * 86_400;
pub const REMINDER_RETRY_BACKOFF_SECONDS: i64 = 600;

pub const TRIAL_BY_COMBAT_RULES: &str = "Mirror matchups: both duelists play the same hero each game.\n\
• Game 1 hero: recipient's pick\n\
• Game 2 hero: challenger's pick\n\
• Tiebreaker: Shadow Fiend, mid\n\
• Hero pool: all heroes, no bans\n\
• Victory: tower destruction, two kills, or opponent surrender\n\
• If neither player has won by 15:00, the higher score wins: creep score + (kills × 35)\n\
• Prohibited: farming in the jungle, destroying observer wards, visiting other lanes, blocking the first wave of creeps, collecting and using runes, Bottle, and Infused Raindrops\n\
Players can agree to additional game rules that do not conflict with existing regulations.";

pub const COMMAND_GROUP: CommandGroup = CommandGroup {
    name: "duel",
    description: "Challenges of honor",
    subcommands: &["issue", "respond", "list", "resolve"],
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandGroup {
    pub name: &'static str,
    pub description: &'static str,
    pub subcommands: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Visibility {
    #[default]
    Public,
    Ephemeral,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AllowedMentions {
    pub everyone: bool,
    pub roles: bool,
    pub users: Vec<i64>,
    pub replied_user: bool,
}

impl AllowedMentions {
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn users(users: impl IntoIterator<Item = i64>) -> Self {
        Self {
            users: users.into_iter().collect(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbedColor {
    Gold,
    Blue,
    Red,
    DarkGrey,
    Green,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Embed {
    pub title: String,
    pub description: String,
    pub color: EmbedColor,
    pub fields: Vec<EmbedField>,
    pub footer: Option<String>,
}

impl Embed {
    fn field(&mut self, name: impl Into<String>, value: impl Into<String>, inline: bool) {
        self.fields.push(EmbedField {
            name: name.into(),
            value: value.into(),
            inline,
        });
    }

    #[must_use]
    pub fn field_value(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|field| field.name == name)
            .map(|field| field.value.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonStyle {
    Danger,
    Primary,
    Success,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Button {
    pub label: String,
    pub custom_id: String,
    pub choice: String,
    pub style: ButtonStyle,
    pub emoji: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChallengeView {
    pub timeout_seconds: Option<u64>,
    pub challenge_id: i64,
    pub buttons: Vec<Button>,
}

impl ChallengeView {
    #[must_use]
    pub fn new(challenge_id: i64) -> Self {
        Self {
            timeout_seconds: None,
            challenge_id,
            buttons: vec![
                Button {
                    label: "Decline in Cowardice".to_owned(),
                    custom_id: format!("duel:{challenge_id}:decline"),
                    choice: "decline".to_owned(),
                    style: ButtonStyle::Danger,
                    emoji: "🏳️".to_owned(),
                },
                Button {
                    label: "Trial by Combat".to_owned(),
                    custom_id: format!("duel:{challenge_id}:trial_by_combat"),
                    choice: "trial_by_combat".to_owned(),
                    style: ButtonStyle::Primary,
                    emoji: "⚔️".to_owned(),
                },
                Button {
                    label: "Trial by Five".to_owned(),
                    custom_id: format!("duel:{challenge_id}:trial_of_five"),
                    choice: "trial_of_five".to_owned(),
                    style: ButtonStyle::Success,
                    emoji: "🛡️".to_owned(),
                },
            ],
        }
    }

    #[must_use]
    pub fn dispatch(&self, custom_id: &str) -> Option<(i64, &str)> {
        self.buttons
            .iter()
            .find(|button| button.custom_id == custom_id)
            .map(|button| (self.challenge_id, button.choice.as_str()))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResponsePayload {
    pub content: Option<String>,
    pub embed: Option<Embed>,
    pub view: Option<ChallengeView>,
    /// Distinguishes an omitted view from Discord's explicit `view=None`
    /// component removal on an edit.
    pub clear_view: bool,
    pub visibility: Visibility,
    pub allowed_mentions: AllowedMentions,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundControlEdit {
    pub challenge_id: i64,
    pub channel_id: i64,
    pub message_id: i64,
    pub view: ChallengeView,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Member {
    pub id: i64,
    pub bot: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelKind {
    Text,
    Thread,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Channel {
    pub id: i64,
    pub guild_id: Option<i64>,
    pub name: String,
    pub kind: ChannelKind,
    pub can_send: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Guild {
    pub id: i64,
    pub bot_member_present: bool,
    pub text_channel_ids: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InteractionContext {
    pub guild_id: Option<i64>,
    pub channel_id: Option<i64>,
    pub user_id: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlavorEvent {
    Issued,
    Reminder,
    AcceptedCombat,
    AcceptedFive,
    Declined,
    Expired,
    Resolved,
    Voided,
    Unresolved,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlavorDetails {
    pub challenge_id: i64,
    pub challenger: &'static str,
    pub recipient: &'static str,
    pub wager: i64,
    pub issuance_fee: i64,
    pub status: &'static str,
    pub trial: Option<&'static str>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CommandError {
    #[error("{0}")]
    Invalid(String),
    #[error("The challenged player cannot cover the duel wager.")]
    RecipientFunding { recipient_id: i64, wager: i64 },
    #[error("{0}")]
    Internal(String),
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct TransportError {
    pub message: String,
}

impl TransportError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub trait InteractionPort {
    fn defer(&mut self) -> bool;
    fn send_immediate(&mut self, payload: ResponsePayload) -> Result<(), TransportError>;
    fn send_followup(&mut self, payload: ResponsePayload) -> Result<Option<i64>, TransportError>;
}

pub trait MessagePort {
    fn guild(&self, guild_id: i64) -> Option<Guild>;
    fn cached_channel(&self, channel_id: i64) -> Option<Channel>;
    fn fetch_channel(&mut self, channel_id: i64) -> Result<Option<Channel>, TransportError>;
    fn send_channel(
        &mut self,
        channel_id: i64,
        payload: ResponsePayload,
    ) -> Result<Option<i64>, TransportError>;
    fn edit_message(
        &mut self,
        channel_id: i64,
        message_id: i64,
        payload: ResponsePayload,
    ) -> Result<(), TransportError>;
    fn delete_message(&mut self, channel_id: i64, message_id: i64) -> Result<(), TransportError>;
    fn register_view(&mut self, message_id: i64, view: ChallengeView);
    /// Restore edits behind a transport-owned concurrency limiter.
    fn restore_controls_bounded(
        &mut self,
        edits: Vec<BoundControlEdit>,
        max_concurrency: usize,
    ) -> Vec<(i64, Result<(), TransportError>)>;
}

pub trait MemberPort {
    fn member(&self, guild_id: i64, member_id: i64) -> Option<Member>;
    fn is_admin(&self, guild_id: i64, member_id: i64) -> bool;
}

pub trait SchedulerPort {
    fn run<T, F: FnOnce() -> T>(&mut self, operation: &'static str, work: F) -> T;
}

pub trait DuelServicePort {
    #[allow(clippy::too_many_arguments)]
    fn issue(
        &mut self,
        guild_id: i64,
        channel_id: i64,
        challenger_id: i64,
        recipient_id: i64,
        wager: i64,
        recipient_is_bot: bool,
    ) -> Result<DuelChallenge, CommandError>;
    fn bind_message(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        message_id: i64,
    ) -> Result<DuelChallenge, CommandError>;
    fn mark_delivery_failed(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        actor_id: i64,
    ) -> Result<DuelChallenge, CommandError>;
    fn get_challenge(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
    ) -> Result<Option<DuelChallenge>, CommandError>;
    fn respond(
        &mut self,
        guild_id: i64,
        recipient_id: i64,
        choice: &str,
    ) -> Result<DuelChallenge, CommandError>;
    fn list_outstanding(&mut self, guild_id: i64) -> Result<Vec<DuelChallenge>, CommandError>;
    fn list_pending_all(&mut self) -> Result<Vec<DuelChallenge>, CommandError>;
    fn resolve(
        &mut self,
        guild_id: i64,
        actor_id: i64,
        challenge_id: i64,
        outcome: &str,
        policy: &ProfitDeductionPolicy<'_>,
    ) -> Result<DuelResolution, CommandError>;
    fn process_due(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        now: i64,
        claim_reminders: bool,
    ) -> Result<Option<DuelDueResult>, CommandError>;
    fn rearm_reminder(&mut self, result: &DuelDueResult) -> Result<bool, CommandError>;
    fn confirm_expiry_announced(&mut self, result: &DuelDueResult) -> Result<bool, CommandError>;
}

/// Atomic persistence operations consumed by the duel application facade.
///
/// The production implementation below is a thin adapter around
/// `cama_db::duel_repository::DuelChallengeRepository`. Keeping this port at
/// the application boundary makes policy routing testable without weakening
/// the repository's transaction ownership.
pub trait DuelRepositoryPort {
    #[allow(clippy::too_many_arguments)]
    fn create_challenge_atomic(
        &mut self,
        guild_id: i64,
        channel_id: i64,
        challenger_id: i64,
        recipient_id: i64,
        wager: i64,
        now: i64,
        challenger_cooldown_seconds: i64,
        recipient_cooldown_seconds: i64,
        response_seconds: i64,
        actor_id: i64,
    ) -> Result<DuelChallenge, CommandError>;
    fn get_pending_for_recipient(
        &mut self,
        recipient_id: i64,
        guild_id: i64,
    ) -> Result<Option<DuelChallenge>, CommandError>;
    fn accept_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        recipient_id: i64,
        trial: DuelTrial,
        now: i64,
        actor_id: i64,
    ) -> Result<DuelChallenge, CommandError>;
    fn decline_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        recipient_id: i64,
        now: i64,
        actor_id: i64,
    ) -> Result<DuelChallenge, CommandError>;
    fn get_challenge(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
    ) -> Result<Option<DuelChallenge>, CommandError>;
    fn resolve_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        winner_id: Option<i64>,
        now: i64,
        actor_id: i64,
        policy: &ProfitDeductionPolicy<'_>,
    ) -> Result<DuelResolution, CommandError>;
    fn bind_message(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        message_id: i64,
    ) -> Result<DuelChallenge, CommandError>;
    fn mark_delivery_failed_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        now: i64,
        actor_id: i64,
    ) -> Result<DuelChallenge, CommandError>;
    fn list_outstanding(&mut self, guild_id: i64) -> Result<Vec<DuelChallenge>, CommandError>;
    fn list_pending_all(&mut self) -> Result<Vec<DuelChallenge>, CommandError>;
    fn get_due_challenge_ids(&mut self, now: i64) -> Result<Vec<(i64, i64)>, CommandError>;
    fn claim_reminder_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<Option<DuelDueResult>, CommandError>;
    fn claim_unresolved_reminder_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<Option<DuelDueResult>, CommandError>;
    fn claim_expired_announcement_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        now: i64,
        retry_at: i64,
    ) -> Result<Option<DuelDueResult>, CommandError>;
    fn expire_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<DuelChallenge, CommandError>;
    fn clear_expired_announcement_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        expected_next_reminder_at: i64,
    ) -> Result<bool, CommandError>;
    fn rearm_reminder_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        expected_status: DuelStatus,
        expected_next_reminder_at: i64,
        retry_at: i64,
    ) -> Result<bool, CommandError>;
}

fn map_duel_repository_error(error: DuelRepositoryError) -> CommandError {
    match error {
        DuelRepositoryError::Invalid(message) => CommandError::Invalid(message.to_owned()),
        DuelRepositoryError::RecipientFunding {
            recipient_id,
            wager,
        } => CommandError::RecipientFunding {
            recipient_id,
            wager,
        },
        DuelRepositoryError::Sqlite(error) => CommandError::Internal(error.to_string()),
    }
}

impl DuelRepositoryPort for DuelChallengeRepository {
    fn create_challenge_atomic(
        &mut self,
        guild_id: i64,
        channel_id: i64,
        challenger_id: i64,
        recipient_id: i64,
        wager: i64,
        now: i64,
        challenger_cooldown_seconds: i64,
        recipient_cooldown_seconds: i64,
        response_seconds: i64,
        actor_id: i64,
    ) -> Result<DuelChallenge, CommandError> {
        DuelChallengeRepository::create_challenge_atomic(
            self,
            Some(guild_id),
            channel_id,
            challenger_id,
            recipient_id,
            wager,
            now,
            challenger_cooldown_seconds,
            recipient_cooldown_seconds,
            response_seconds,
            actor_id,
        )
        .map_err(map_duel_repository_error)
    }

    fn get_pending_for_recipient(
        &mut self,
        recipient_id: i64,
        guild_id: i64,
    ) -> Result<Option<DuelChallenge>, CommandError> {
        DuelChallengeRepository::get_pending_for_recipient(self, recipient_id, Some(guild_id))
            .map_err(map_duel_repository_error)
    }

    fn accept_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        recipient_id: i64,
        trial: DuelTrial,
        now: i64,
        actor_id: i64,
    ) -> Result<DuelChallenge, CommandError> {
        DuelChallengeRepository::accept_atomic(
            self,
            challenge_id,
            Some(guild_id),
            recipient_id,
            trial,
            now,
            actor_id,
        )
        .map_err(map_duel_repository_error)
    }

    fn decline_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        recipient_id: i64,
        now: i64,
        actor_id: i64,
    ) -> Result<DuelChallenge, CommandError> {
        DuelChallengeRepository::decline_atomic(
            self,
            challenge_id,
            Some(guild_id),
            recipient_id,
            now,
            actor_id,
        )
        .map_err(map_duel_repository_error)
    }

    fn get_challenge(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
    ) -> Result<Option<DuelChallenge>, CommandError> {
        DuelChallengeRepository::get_challenge(self, challenge_id, Some(guild_id))
            .map_err(map_duel_repository_error)
    }

    fn resolve_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        winner_id: Option<i64>,
        now: i64,
        actor_id: i64,
        policy: &ProfitDeductionPolicy<'_>,
    ) -> Result<DuelResolution, CommandError> {
        DuelChallengeRepository::resolve_atomic(
            self,
            challenge_id,
            Some(guild_id),
            winner_id,
            now,
            actor_id,
            policy,
        )
        .map_err(map_duel_repository_error)
    }

    fn bind_message(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        message_id: i64,
    ) -> Result<DuelChallenge, CommandError> {
        DuelChallengeRepository::bind_message(self, challenge_id, Some(guild_id), message_id)
            .map_err(map_duel_repository_error)
    }

    fn mark_delivery_failed_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        now: i64,
        actor_id: i64,
    ) -> Result<DuelChallenge, CommandError> {
        DuelChallengeRepository::mark_delivery_failed_atomic(
            self,
            challenge_id,
            Some(guild_id),
            now,
            actor_id,
        )
        .map_err(map_duel_repository_error)
    }

    fn list_outstanding(&mut self, guild_id: i64) -> Result<Vec<DuelChallenge>, CommandError> {
        DuelChallengeRepository::list_outstanding(self, Some(guild_id))
            .map_err(map_duel_repository_error)
    }

    fn list_pending_all(&mut self) -> Result<Vec<DuelChallenge>, CommandError> {
        DuelChallengeRepository::list_pending_all(self).map_err(map_duel_repository_error)
    }

    fn get_due_challenge_ids(&mut self, now: i64) -> Result<Vec<(i64, i64)>, CommandError> {
        DuelChallengeRepository::get_due_challenge_ids(self, now).map_err(map_duel_repository_error)
    }

    fn claim_reminder_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<Option<DuelDueResult>, CommandError> {
        DuelChallengeRepository::claim_reminder_atomic(self, challenge_id, Some(guild_id), now)
            .map_err(map_duel_repository_error)
    }

    fn claim_unresolved_reminder_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<Option<DuelDueResult>, CommandError> {
        DuelChallengeRepository::claim_unresolved_reminder_atomic(
            self,
            challenge_id,
            Some(guild_id),
            now,
        )
        .map_err(map_duel_repository_error)
    }

    fn claim_expired_announcement_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        now: i64,
        retry_at: i64,
    ) -> Result<Option<DuelDueResult>, CommandError> {
        DuelChallengeRepository::claim_expired_announcement_atomic(
            self,
            challenge_id,
            Some(guild_id),
            now,
            retry_at,
        )
        .map_err(map_duel_repository_error)
    }

    fn expire_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<DuelChallenge, CommandError> {
        DuelChallengeRepository::expire_atomic(self, challenge_id, Some(guild_id), now)
            .map_err(map_duel_repository_error)
    }

    fn clear_expired_announcement_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        expected_next_reminder_at: i64,
    ) -> Result<bool, CommandError> {
        DuelChallengeRepository::clear_expired_announcement_atomic(
            self,
            challenge_id,
            Some(guild_id),
            expected_next_reminder_at,
        )
        .map_err(map_duel_repository_error)
    }

    fn rearm_reminder_atomic(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        expected_status: DuelStatus,
        expected_next_reminder_at: i64,
        retry_at: i64,
    ) -> Result<bool, CommandError> {
        DuelChallengeRepository::rearm_reminder_atomic(
            self,
            challenge_id,
            Some(guild_id),
            expected_status,
            expected_next_reminder_at,
            retry_at,
        )
        .map_err(map_duel_repository_error)
    }
}

pub trait DuelClockPort {
    fn now(&mut self) -> f64;
}

impl<F> DuelClockPort for F
where
    F: FnMut() -> f64,
{
    fn now(&mut self) -> f64 {
        self()
    }
}

pub trait DuelEvolutionPort {
    fn record_activity(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        activity: PetActivity,
        source_key: &str,
        occurred_at: i64,
    );
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopDuelEvolution;

impl DuelEvolutionPort for NoopDuelEvolution {
    fn record_activity(
        &mut self,
        _discord_id: i64,
        _guild_id: i64,
        _activity: PetActivity,
        _source_key: &str,
        _occurred_at: i64,
    ) {
    }
}

/// Python-compatible duel use-case facade over the atomic repository.
pub struct DuelApplicationService<R, C, E = NoopDuelEvolution> {
    pub repository: R,
    pub clock: C,
    pub evolution: Option<E>,
}

impl<R, C> DuelApplicationService<R, C, NoopDuelEvolution> {
    #[must_use]
    pub const fn new(repository: R, clock: C) -> Self {
        Self {
            repository,
            clock,
            evolution: None,
        }
    }
}

impl<R, C, E> DuelApplicationService<R, C, E> {
    #[must_use]
    pub const fn with_evolution(repository: R, clock: C, evolution: E) -> Self {
        Self {
            repository,
            clock,
            evolution: Some(evolution),
        }
    }
}

impl<R, C, E> DuelApplicationService<R, C, E>
where
    R: DuelRepositoryPort,
    C: DuelClockPort,
    E: DuelEvolutionPort,
{
    fn integer_now(&mut self) -> i64 {
        self.clock.now() as i64
    }

    #[allow(clippy::too_many_arguments)]
    pub fn issue(
        &mut self,
        guild_id: i64,
        channel_id: i64,
        challenger_id: i64,
        recipient_id: i64,
        wager: i64,
        recipient_is_bot: bool,
    ) -> Result<DuelChallenge, CommandError> {
        if recipient_is_bot {
            return Err(CommandError::Invalid(
                "Bots cannot answer a challenge of honor.".to_owned(),
            ));
        }
        let now = self.integer_now();
        self.repository.create_challenge_atomic(
            guild_id,
            channel_id,
            challenger_id,
            recipient_id,
            wager,
            now,
            CHALLENGER_COOLDOWN_SECONDS,
            RECIPIENT_COOLDOWN_SECONDS,
            RESPONSE_SECONDS,
            challenger_id,
        )
    }

    pub fn respond(
        &mut self,
        guild_id: i64,
        recipient_id: i64,
        choice: &str,
    ) -> Result<DuelChallenge, CommandError> {
        self.respond_to(guild_id, recipient_id, choice, None)
    }

    /// Respond to a specific challenge when the caller knows which one.
    ///
    /// A button carries the challenge it was rendered for. Resolving "whatever
    /// is pending" instead would answer a newer challenge when one arrives
    /// between the render and the click, so the recipient accepts a duel they
    /// never saw.
    pub fn respond_to(
        &mut self,
        guild_id: i64,
        recipient_id: i64,
        choice: &str,
        challenge_id: Option<i64>,
    ) -> Result<DuelChallenge, CommandError> {
        let challenge = match challenge_id {
            Some(challenge_id) => self
                .repository
                .get_challenge(challenge_id, guild_id)?
                .filter(|challenge| challenge.recipient_id == recipient_id),
            None => self
                .repository
                .get_pending_for_recipient(recipient_id, guild_id)?,
        }
        .ok_or_else(|| CommandError::Invalid("You have no pending duel challenge.".to_owned()))?;
        let now = self.integer_now();
        if choice == "decline" {
            return self.repository.decline_atomic(
                challenge.challenge_id,
                guild_id,
                recipient_id,
                now,
                recipient_id,
            );
        }
        let trial = match choice {
            "trial_by_combat" => DuelTrial::TrialByCombat,
            "trial_of_five" => DuelTrial::TrialOfFive,
            _ => {
                return Err(CommandError::Invalid(format!(
                    "'{choice}' is not a valid DuelTrial"
                )));
            }
        };
        self.repository.accept_atomic(
            challenge.challenge_id,
            guild_id,
            recipient_id,
            trial,
            now,
            recipient_id,
        )
    }

    pub fn resolve(
        &mut self,
        guild_id: i64,
        actor_id: i64,
        challenge_id: i64,
        outcome: &str,
        policy: &ProfitDeductionPolicy<'_>,
    ) -> Result<DuelResolution, CommandError> {
        let challenge = self
            .repository
            .get_challenge(challenge_id, guild_id)?
            .ok_or_else(|| CommandError::Invalid("Challenge not found.".to_owned()))?;
        let winner_id = match outcome {
            "challenger_victory" => Some(challenge.challenger_id),
            "recipient_victory" => Some(challenge.recipient_id),
            "void" => None,
            _ => {
                return Err(CommandError::Invalid(format!(
                    "'{outcome}' is not a valid DuelResolution"
                )));
            }
        };
        let now = self.integer_now();
        let resolved = self.repository.resolve_atomic(
            challenge_id,
            guild_id,
            winner_id,
            now,
            actor_id,
            policy,
        )?;
        if outcome != "void"
            && let Some(evolution) = self.evolution.as_mut()
        {
            let source_key = format!("duel:{}", challenge.challenge_id);
            for participant_id in [challenge.challenger_id, challenge.recipient_id] {
                evolution.record_activity(
                    participant_id,
                    guild_id,
                    PetActivity::DuelCompleted,
                    &source_key,
                    now,
                );
            }
        }
        Ok(resolved)
    }

    pub fn bind_message(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        message_id: i64,
    ) -> Result<DuelChallenge, CommandError> {
        self.repository
            .bind_message(challenge_id, guild_id, message_id)
    }

    pub fn get_challenge(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
    ) -> Result<Option<DuelChallenge>, CommandError> {
        self.repository.get_challenge(challenge_id, guild_id)
    }

    pub fn mark_delivery_failed(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        actor_id: i64,
    ) -> Result<DuelChallenge, CommandError> {
        let now = self.integer_now();
        self.repository
            .mark_delivery_failed_atomic(challenge_id, guild_id, now, actor_id)
    }

    pub fn list_outstanding(&mut self, guild_id: i64) -> Result<Vec<DuelChallenge>, CommandError> {
        self.repository.list_outstanding(guild_id)
    }

    pub fn list_pending_all(&mut self) -> Result<Vec<DuelChallenge>, CommandError> {
        self.repository.list_pending_all()
    }

    pub fn get_due_challenge_ids(&mut self, now: i64) -> Result<Vec<(i64, i64)>, CommandError> {
        self.repository.get_due_challenge_ids(now)
    }

    pub fn process_due(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        now: i64,
        claim_reminders: bool,
    ) -> Result<Option<DuelDueResult>, CommandError> {
        if claim_reminders {
            if let Some(reminder) =
                self.repository
                    .claim_reminder_atomic(challenge_id, guild_id, now)?
            {
                return Ok(Some(reminder));
            }
            if let Some(unresolved) =
                self.repository
                    .claim_unresolved_reminder_atomic(challenge_id, guild_id, now)?
            {
                return Ok(Some(unresolved));
            }
        }
        if let Some(announcement) = self.repository.claim_expired_announcement_atomic(
            challenge_id,
            guild_id,
            now,
            now + REMINDER_RETRY_BACKOFF_SECONDS,
        )? {
            return Ok(Some(announcement));
        }
        match self.repository.expire_atomic(challenge_id, guild_id, now) {
            Ok(expired) => Ok(Some(DuelDueResult {
                kind: DuelDueKind::Expired,
                challenge: expired,
                remaining_seconds: 0,
                ping_recipient: false,
                claimed_reminder_at: None,
                is_redelivery: false,
            })),
            Err(CommandError::Invalid(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn confirm_expiry_announced(
        &mut self,
        result: &DuelDueResult,
    ) -> Result<bool, CommandError> {
        if result.kind != DuelDueKind::Expired {
            return Ok(false);
        }
        let Some(next_reminder_at) = result.challenge.next_reminder_at else {
            return Ok(false);
        };
        self.repository.clear_expired_announcement_atomic(
            result.challenge.challenge_id,
            result.challenge.guild_id,
            next_reminder_at,
        )
    }

    pub fn rearm_reminder(&mut self, result: &DuelDueResult) -> Result<bool, CommandError> {
        let expected_status = match result.kind {
            DuelDueKind::Reminder => DuelStatus::Pending,
            DuelDueKind::Unresolved => DuelStatus::Accepted,
            DuelDueKind::Expired => return Ok(false),
        };
        if result.claimed_reminder_at.is_none() {
            return Ok(false);
        }
        let Some(next_reminder_at) = result.challenge.next_reminder_at else {
            return Ok(false);
        };
        let retry_at = next_reminder_at.min(self.integer_now() + REMINDER_RETRY_BACKOFF_SECONDS);
        self.repository.rearm_reminder_atomic(
            result.challenge.challenge_id,
            result.challenge.guild_id,
            expected_status,
            next_reminder_at,
            retry_at,
        )
    }
}

impl<R, C, E> DuelServicePort for DuelApplicationService<R, C, E>
where
    R: DuelRepositoryPort,
    C: DuelClockPort,
    E: DuelEvolutionPort,
{
    fn issue(
        &mut self,
        guild_id: i64,
        channel_id: i64,
        challenger_id: i64,
        recipient_id: i64,
        wager: i64,
        recipient_is_bot: bool,
    ) -> Result<DuelChallenge, CommandError> {
        DuelApplicationService::issue(
            self,
            guild_id,
            channel_id,
            challenger_id,
            recipient_id,
            wager,
            recipient_is_bot,
        )
    }

    fn bind_message(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        message_id: i64,
    ) -> Result<DuelChallenge, CommandError> {
        DuelApplicationService::bind_message(self, challenge_id, guild_id, message_id)
    }

    fn mark_delivery_failed(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        actor_id: i64,
    ) -> Result<DuelChallenge, CommandError> {
        DuelApplicationService::mark_delivery_failed(self, challenge_id, guild_id, actor_id)
    }

    fn get_challenge(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
    ) -> Result<Option<DuelChallenge>, CommandError> {
        DuelApplicationService::get_challenge(self, challenge_id, guild_id)
    }

    fn respond(
        &mut self,
        guild_id: i64,
        recipient_id: i64,
        choice: &str,
    ) -> Result<DuelChallenge, CommandError> {
        DuelApplicationService::respond(self, guild_id, recipient_id, choice)
    }

    fn list_outstanding(&mut self, guild_id: i64) -> Result<Vec<DuelChallenge>, CommandError> {
        DuelApplicationService::list_outstanding(self, guild_id)
    }

    fn list_pending_all(&mut self) -> Result<Vec<DuelChallenge>, CommandError> {
        DuelApplicationService::list_pending_all(self)
    }

    fn resolve(
        &mut self,
        guild_id: i64,
        actor_id: i64,
        challenge_id: i64,
        outcome: &str,
        policy: &ProfitDeductionPolicy<'_>,
    ) -> Result<DuelResolution, CommandError> {
        DuelApplicationService::resolve(self, guild_id, actor_id, challenge_id, outcome, policy)
    }

    fn process_due(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        now: i64,
        claim_reminders: bool,
    ) -> Result<Option<DuelDueResult>, CommandError> {
        DuelApplicationService::process_due(self, challenge_id, guild_id, now, claim_reminders)
    }

    fn rearm_reminder(&mut self, result: &DuelDueResult) -> Result<bool, CommandError> {
        DuelApplicationService::rearm_reminder(self, result)
    }

    fn confirm_expiry_announced(&mut self, result: &DuelDueResult) -> Result<bool, CommandError> {
        DuelApplicationService::confirm_expiry_announced(self, result)
    }
}

pub trait FlavorPort {
    fn generate(&mut self, event: FlavorEvent, guild_id: i64, details: &FlavorDetails) -> String;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub level: LogLevel,
    pub message: String,
}

/// Observable recovery accounting for the transport adapter.
///
/// The orchestration is synchronous in this Discord-independent model, but
/// `peak_bound_edits` records the bounded batch width a live async adapter is
/// allowed to schedule. This makes the Python semaphore contract explicit
/// without coupling this crate to an async runtime.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    pub registered_views: usize,
    pub attempted_bound_edits: usize,
    pub peak_bound_edits: usize,
    pub distinct_channel_resolutions: usize,
    pub restored_unbound: usize,
}

/// The `/duel resolve` announcement: the net pot for a winner, with every
/// non-zero withholding named, or the refund notice for a void.
#[must_use]
pub fn resolution_detail(resolution: &DuelResolution) -> String {
    let challenge = &resolution.challenge;
    if challenge.status == DuelStatus::Voided {
        return format!(
            "Challenge #{} was voided. The {} JC stakes were refunded to each player; \
             the issuance fee was not refunded.",
            challenge.challenge_id, challenge.wager
        );
    }
    let deductions = resolution.deductions;
    format!(
        "Challenge #{} is resolved: <@{}> wins {} JC{}.",
        challenge.challenge_id,
        challenge.winner_id.unwrap_or_default(),
        challenge.wager * 2 - deductions.total(),
        withholding_note(deductions)
    )
}

fn withholding_note(deductions: ProfitDeductions) -> String {
    let parts = [
        (deductions.bankruptcy_penalty, "bankruptcy penalty"),
        (deductions.vanity_tax, "vanity tax"),
        (deductions.low_priority_tax, "low priority tax"),
    ]
    .into_iter()
    .filter(|(amount, _)| *amount > 0)
    .map(|(amount, label)| format!("\u{2212}{amount} JC {label}"))
    .collect::<Vec<_>>();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

pub struct DuelCommands<S, F, M, N, X> {
    pub service: S,
    pub flavor: F,
    pub messages: M,
    pub members: N,
    pub scheduler: X,
    pub configured_channel_id: Option<i64>,
    pub monotonic_seconds: i64,
    pub logs: Vec<LogEntry>,
    configured_rescue_failed_at: HashMap<i64, i64>,
    configured_rescue_channel: HashMap<i64, (i64, Channel)>,
    deferred_warned_at: HashMap<i64, i64>,
}

impl<S, F, M, N, X> DuelCommands<S, F, M, N, X>
where
    S: DuelServicePort,
    F: FlavorPort,
    M: MessagePort,
    N: MemberPort,
    X: SchedulerPort,
{
    #[must_use]
    pub fn new(service: S, flavor: F, messages: M, members: N, scheduler: X) -> Self {
        Self {
            service,
            flavor,
            messages,
            members,
            scheduler,
            configured_channel_id: None,
            monotonic_seconds: 0,
            logs: Vec::new(),
            configured_rescue_failed_at: HashMap::new(),
            configured_rescue_channel: HashMap::new(),
            deferred_warned_at: HashMap::new(),
        }
    }

    fn log(&mut self, level: LogLevel, message: impl Into<String>) {
        self.logs.push(LogEntry {
            level,
            message: message.into(),
        });
    }

    #[must_use]
    pub fn flavor_details(challenge: &DuelChallenge) -> FlavorDetails {
        FlavorDetails {
            challenge_id: challenge.challenge_id,
            challenger: "the challenger",
            recipient: "the recipient",
            wager: challenge.wager,
            issuance_fee: challenge.issuance_fee,
            status: challenge.status.as_str(),
            trial: challenge.trial_type.map(DuelTrial::as_str),
        }
    }

    #[must_use]
    pub fn build_challenge_embed(challenge: &DuelChallenge, flavor: &str) -> Embed {
        let color = match challenge.status {
            DuelStatus::Pending => EmbedColor::Gold,
            DuelStatus::Accepted => EmbedColor::Blue,
            DuelStatus::Declined => EmbedColor::Red,
            DuelStatus::Expired | DuelStatus::Voided | DuelStatus::DeliveryFailed => {
                EmbedColor::DarkGrey
            }
            DuelStatus::Resolved => EmbedColor::Green,
        };
        let mut embed = Embed {
            title: "Challenge of Honor".to_owned(),
            description: flavor.to_owned(),
            color,
            fields: Vec::new(),
            footer: None,
        };
        embed.field("Challenge", format!("#{}", challenge.challenge_id), true);
        embed.field("Status", status_label(challenge.status), true);
        embed.field("Wager", format!("{} JC", challenge.wager), true);
        embed.field(
            "Issuance Fee",
            format!(
                "{} JC — nonrefundable after delivery",
                challenge.issuance_fee
            ),
            true,
        );
        embed.field(
            "Challenger",
            format!("<@{}>", challenge.challenger_id),
            true,
        );
        embed.field(
            "Challenger Rating",
            format!(
                "{:.0} ± {:.0}",
                challenge.challenger_glicko, challenge.challenger_rd
            ),
            true,
        );
        embed.field("Recipient", format!("<@{}>", challenge.recipient_id), true);
        embed.field(
            "Recipient Rating",
            format!(
                "{:.0} ± {:.0}",
                challenge.recipient_glicko, challenge.recipient_rd
            ),
            true,
        );
        embed.field(
            "Decline Penalty",
            format!("{} JC", challenge.decline_penalty()),
            true,
        );
        if challenge.status == DuelStatus::Pending {
            embed.field(
                "Response Deadline",
                format!("<t:{}:R>", challenge.expires_at),
                true,
            );
            embed.field(
                "Trials",
                "Trial by Combat — best-of-three, one-versus-one Dota mid\n\
                 Trial of Five — a lobby using Immortal Draft",
                false,
            );
        }
        if let Some(trial) = challenge.trial_type {
            embed.field("Trial", trial_label(trial), true);
            if challenge.status == DuelStatus::Accepted && trial == DuelTrial::TrialByCombat {
                embed.field("Rules", TRIAL_BY_COMBAT_RULES, false);
            }
        }
        if challenge.status == DuelStatus::Resolved {
            embed.field(
                "Winner",
                format!("<@{}>", challenge.winner_id.unwrap_or_default()),
                true,
            );
        }
        if challenge.status == DuelStatus::Voided {
            let refund = if challenge.trial_type.is_none() {
                embed.field(
                    "Funding Failure",
                    format!(
                        "The recipient could not fund the {} JC wager.",
                        challenge.wager
                    ),
                    false,
                );
                format!(
                    "{} JC challenger stake refunded; {} JC issuance fee remains \
                     nonrefundable after delivery",
                    challenge.wager, challenge.issuance_fee
                )
            } else {
                format!(
                    "{} JC stake to each player; issuance fee remains nonrefundable",
                    challenge.wager
                )
            };
            embed.field("Refund", refund, true);
        }
        embed
    }
}

fn status_label(status: DuelStatus) -> &'static str {
    match status {
        DuelStatus::Pending => "Pending",
        DuelStatus::Accepted => "Accepted",
        DuelStatus::Declined => "Declined",
        DuelStatus::Expired => "Expired",
        DuelStatus::Resolved => "Resolved",
        DuelStatus::Voided => "Voided",
        DuelStatus::DeliveryFailed => "Delivery Failed",
    }
}

fn trial_label(trial: DuelTrial) -> &'static str {
    match trial {
        DuelTrial::TrialByCombat => "Trial by Combat",
        DuelTrial::TrialOfFive => "Trial of Five",
    }
}

impl<S, F, M, N, X> DuelCommands<S, F, M, N, X>
where
    S: DuelServicePort,
    F: FlavorPort,
    M: MessagePort,
    N: MemberPort,
    X: SchedulerPort,
{
    pub fn issue<I: InteractionPort>(
        &mut self,
        interaction: &InteractionContext,
        io: &mut I,
        recipient_id: i64,
        wager: i64,
    ) {
        let (Some(guild_id), Some(channel_id)) = (interaction.guild_id, interaction.channel_id)
        else {
            self.send_immediate_error(io, "This command must be used in a server.");
            return;
        };
        if !io.defer() {
            return;
        }
        let recipient = self
            .members
            .member(guild_id, recipient_id)
            .unwrap_or(Member {
                id: recipient_id,
                bot: false,
            });
        let actor_id = interaction.user_id;
        let service = &mut self.service;
        let issued = self.scheduler.run("duel.issue", || {
            service.issue(
                guild_id,
                channel_id,
                actor_id,
                recipient_id,
                wager,
                recipient.bot,
            )
        });
        let challenge = match issued {
            Ok(challenge) => challenge,
            Err(CommandError::RecipientFunding { wager, .. }) => {
                let _ = io.send_followup(ResponsePayload {
                    content: Some(format!(
                        "The duel from <@{actor_id}> to <@{recipient_id}> failed because \
                         the challenged player cannot cover the {wager} JC wager."
                    )),
                    allowed_mentions: AllowedMentions::none(),
                    ..ResponsePayload::default()
                });
                return;
            }
            Err(error) => {
                self.send_deferred_error(io, &error.to_string());
                return;
            }
        };

        let flavor = self.flavor.generate(
            FlavorEvent::Issued,
            guild_id,
            &Self::flavor_details(&challenge),
        );
        let initial = ResponsePayload {
            content: Some(format!("<@{recipient_id}>")),
            embed: Some(Self::build_challenge_embed(&challenge, &flavor)),
            allowed_mentions: AllowedMentions::users([recipient_id]),
            ..ResponsePayload::default()
        };
        let message_id = match io.send_followup(initial) {
            Ok(Some(message_id)) => message_id,
            Ok(None) | Err(_) => {
                self.refund_failed_delivery(io, &challenge, guild_id, actor_id);
                return;
            }
        };

        let service = &mut self.service;
        let bind = self.scheduler.run("duel.bind_message", || {
            service.bind_message(challenge.challenge_id, guild_id, message_id)
        });
        match bind {
            Ok(_) => {}
            Err(CommandError::Invalid(_)) => {
                self.log(
                    LogLevel::Info,
                    format!(
                        "Duel challenge {} changed before initial message binding",
                        challenge.challenge_id
                    ),
                );
                self.delete_unbound_message(channel_id, message_id, &challenge);
                return;
            }
            Err(_) => {
                self.log(
                    LogLevel::Error,
                    "Unable to bind delivered duel challenge message",
                );
                self.delete_unbound_message(channel_id, message_id, &challenge);
                self.refund_failed_delivery(io, &challenge, guild_id, actor_id);
                return;
            }
        }

        if self
            .messages
            .edit_message(
                channel_id,
                message_id,
                ResponsePayload {
                    view: Some(ChallengeView::new(challenge.challenge_id)),
                    allowed_mentions: AllowedMentions::users([recipient_id]),
                    ..ResponsePayload::default()
                },
            )
            .is_err()
        {
            self.log(
                LogLevel::Error,
                "Unable to attach duel challenge response controls",
            );
        }
    }

    pub fn handle_button<I: InteractionPort>(
        &mut self,
        interaction: &InteractionContext,
        io: &mut I,
        custom_id: &str,
    ) {
        let parts = custom_id.split(':').collect::<Vec<_>>();
        if parts.len() != 3 || parts[0] != "duel" {
            return;
        }
        let Ok(challenge_id) = parts[1].parse::<i64>() else {
            return;
        };
        self.handle_response(interaction, io, Some(challenge_id), parts[2]);
    }

    pub fn respond<I: InteractionPort>(
        &mut self,
        interaction: &InteractionContext,
        io: &mut I,
        choice: &str,
    ) {
        self.handle_response(interaction, io, None, choice);
    }

    pub fn handle_response<I: InteractionPort>(
        &mut self,
        interaction: &InteractionContext,
        io: &mut I,
        challenge_id: Option<i64>,
        choice: &str,
    ) {
        let Some(guild_id) = interaction.guild_id else {
            self.send_immediate_error(io, "This command must be used in a server.");
            return;
        };
        if let Some(challenge_id) = challenge_id {
            let service = &mut self.service;
            let pending = self.scheduler.run("duel.get_challenge", || {
                service.get_challenge(challenge_id, guild_id)
            });
            let valid = matches!(
                pending,
                Ok(Some(ref challenge))
                    if challenge.challenge_id == challenge_id
                        && challenge.guild_id == guild_id
                        && challenge.status == DuelStatus::Pending
                        && challenge.message_id.is_some()
                        && challenge.recipient_id == interaction.user_id
            );
            if !valid {
                self.send_immediate_error(
                    io,
                    "This duel challenge is unavailable or is not yours to answer.",
                );
                return;
            }
        }
        if !io.defer() {
            return;
        }
        let recipient_id = interaction.user_id;
        let service = &mut self.service;
        let response = self.scheduler.run("duel.respond", || {
            service.respond(guild_id, recipient_id, choice)
        });
        let challenge = match response {
            Ok(challenge) => challenge,
            Err(error) => {
                self.send_deferred_error(io, &error.to_string());
                return;
            }
        };
        let event = response_event(&challenge);
        let flavor = self
            .flavor
            .generate(event, guild_id, &Self::flavor_details(&challenge));
        self.edit_original(&challenge, &flavor);
        let detail = if challenge.status == DuelStatus::Declined {
            format!(
                "Challenge #{} was declined. The {} JC penalty was paid to the challenger.",
                challenge.challenge_id,
                challenge.decline_penalty()
            )
        } else if challenge.status == DuelStatus::Voided {
            format!(
                "Challenge #{} was voided because the recipient could not fund the {} JC wager. \
                 The challenger's {} JC stake was refunded; the {} JC issuance fee remains \
                 nonrefundable after delivery.",
                challenge.challenge_id, challenge.wager, challenge.wager, challenge.issuance_fee
            )
        } else {
            let trial = challenge
                .trial_type
                .map(trial_detail)
                .unwrap_or("A valid duel trial is required.");
            let mut detail = format!(
                "Challenge #{}: <@{}> vs <@{}>. Both {} JC stakes are locked. {trial}",
                challenge.challenge_id,
                challenge.challenger_id,
                challenge.recipient_id,
                challenge.wager
            );
            if challenge.trial_type == Some(DuelTrial::TrialByCombat) {
                detail.push('\n');
                detail.push_str(TRIAL_BY_COMBAT_RULES);
            }
            detail
        };
        let _ = io.send_followup(ResponsePayload {
            content: Some(format!("{flavor}\n{detail}")),
            allowed_mentions: AllowedMentions::none(),
            ..ResponsePayload::default()
        });
    }

    pub fn list<I: InteractionPort>(&mut self, interaction: &InteractionContext, io: &mut I) {
        let Some(guild_id) = interaction.guild_id else {
            self.send_immediate_error(io, "This command must be used in a server.");
            return;
        };
        if !io.defer() {
            return;
        }
        let service = &mut self.service;
        let challenges = match self.scheduler.run("duel.list_outstanding", || {
            service.list_outstanding(guild_id)
        }) {
            Ok(challenges) => challenges,
            Err(error) => {
                self.send_deferred_error(io, &error.to_string());
                return;
            }
        };
        if challenges.is_empty() {
            let _ = io.send_followup(ResponsePayload {
                content: Some("No pending or accepted duel challenges were found.".to_owned()),
                visibility: Visibility::Ephemeral,
                allowed_mentions: AllowedMentions::none(),
                ..ResponsePayload::default()
            });
            return;
        }
        let page_count = challenges.len().div_ceil(25);
        for (page_index, page) in challenges.chunks(25).enumerate() {
            let mut embed = Embed {
                title: "Outstanding Duel Challenges".to_owned(),
                description: String::new(),
                color: EmbedColor::Gold,
                fields: Vec::new(),
                footer: (page_count > 1)
                    .then(|| format!("Page {} of {page_count}", page_index + 1)),
            };
            for challenge in page {
                let trial = challenge.trial_type.map_or_else(
                    || format!(" • responds <t:{}:R>", challenge.expires_at),
                    |trial| format!(" • {}", trial_label(trial)),
                );
                embed.field(
                    format!(
                        "#{} • {}",
                        challenge.challenge_id,
                        status_label(challenge.status)
                    ),
                    format!(
                        "<@{}> vs <@{}> • {} JC{trial}",
                        challenge.challenger_id, challenge.recipient_id, challenge.wager
                    ),
                    false,
                );
            }
            let _ = io.send_followup(ResponsePayload {
                embed: Some(embed),
                allowed_mentions: AllowedMentions::none(),
                ..ResponsePayload::default()
            });
        }
    }

    pub fn resolve<I: InteractionPort>(
        &mut self,
        interaction: &InteractionContext,
        io: &mut I,
        challenge_id: i64,
        outcome: &str,
        policy: &ProfitDeductionPolicy<'_>,
    ) {
        let Some(guild_id) = interaction.guild_id else {
            self.send_immediate_error(io, "This command must be used in a server.");
            return;
        };
        if !self.members.is_admin(guild_id, interaction.user_id) {
            self.send_immediate_error(io, "You do not have permission to resolve duel challenges.");
            return;
        }
        if !io.defer() {
            return;
        }
        let actor_id = interaction.user_id;
        let service = &mut self.service;
        let resolved = self.scheduler.run("duel.resolve", || {
            service.resolve(guild_id, actor_id, challenge_id, outcome, policy)
        });
        let resolution = match resolved {
            Ok(resolution) => resolution,
            Err(error) => {
                self.send_deferred_error(io, &error.to_string());
                return;
            }
        };
        let challenge = &resolution.challenge;
        let event = if challenge.status == DuelStatus::Voided {
            FlavorEvent::Voided
        } else {
            FlavorEvent::Resolved
        };
        let flavor = self
            .flavor
            .generate(event, guild_id, &Self::flavor_details(challenge));
        self.edit_original(challenge, &flavor);
        let detail = resolution_detail(&resolution);
        let _ = io.send_followup(ResponsePayload {
            content: Some(format!("{flavor}\n{detail}")),
            allowed_mentions: AllowedMentions::none(),
            ..ResponsePayload::default()
        });
    }

    fn send_immediate_error<I: InteractionPort>(&mut self, io: &mut I, message: &str) {
        let _ = io.send_immediate(ResponsePayload {
            content: Some(message.to_owned()),
            visibility: Visibility::Ephemeral,
            allowed_mentions: AllowedMentions::none(),
            ..ResponsePayload::default()
        });
    }

    fn send_deferred_error<I: InteractionPort>(&mut self, io: &mut I, message: &str) {
        let _ = io.send_followup(ResponsePayload {
            content: Some(message.to_owned()),
            visibility: Visibility::Ephemeral,
            allowed_mentions: AllowedMentions::none(),
            ..ResponsePayload::default()
        });
    }

    fn refund_failed_delivery<I: InteractionPort>(
        &mut self,
        io: &mut I,
        challenge: &DuelChallenge,
        guild_id: i64,
        actor_id: i64,
    ) {
        let service = &mut self.service;
        let result = self.scheduler.run("duel.mark_delivery_failed", || {
            service.mark_delivery_failed(challenge.challenge_id, guild_id, actor_id)
        });
        if matches!(result, Err(CommandError::Invalid(_))) {
            self.log(
                LogLevel::Info,
                format!(
                    "Duel challenge {} changed before initial delivery refund",
                    challenge.challenge_id
                ),
            );
            return;
        }
        if result.is_err() {
            return;
        }
        let _ = io.send_followup(ResponsePayload {
            content: Some(format!(
                "The challenge could not be delivered; the wager and {} JC issuance fee were refunded.",
                challenge.issuance_fee
            )),
            visibility: Visibility::Ephemeral,
            allowed_mentions: AllowedMentions::none(),
            ..ResponsePayload::default()
        });
    }

    fn delete_unbound_message(
        &mut self,
        channel_id: i64,
        message_id: i64,
        challenge: &DuelChallenge,
    ) {
        if self
            .messages
            .delete_message(channel_id, message_id)
            .is_err()
        {
            self.log(
                LogLevel::Error,
                format!(
                    "Unable to delete unbound replacement for duel challenge {}",
                    challenge.challenge_id
                ),
            );
        }
    }

    fn edit_original(&mut self, challenge: &DuelChallenge, flavor: &str) {
        let Some(message_id) = challenge.message_id else {
            return;
        };
        let Some(channel) = self.get_channel(challenge.channel_id) else {
            return;
        };
        if channel.guild_id != Some(challenge.guild_id) {
            return;
        }
        if self
            .messages
            .edit_message(
                channel.id,
                message_id,
                ResponsePayload {
                    embed: Some(Self::build_challenge_embed(challenge, flavor)),
                    view: None,
                    clear_view: true,
                    allowed_mentions: AllowedMentions::none(),
                    ..ResponsePayload::default()
                },
            )
            .is_err()
        {
            self.log(LogLevel::Error, "Unable to update duel challenge message");
        }
    }

    fn get_channel(&mut self, channel_id: i64) -> Option<Channel> {
        if let Some(channel) = self.messages.cached_channel(channel_id) {
            return Some(channel);
        }
        match self.messages.fetch_channel(channel_id) {
            Ok(channel) => channel,
            Err(_) => {
                self.log(LogLevel::Error, "Unable to fetch duel challenge channel");
                None
            }
        }
    }

    /// Resolve the configured duel target, or the unique sendable text
    /// channel whose name contains `dota-mm`.
    pub fn get_main_channel(&mut self, guild_id: i64) -> Option<Channel> {
        let Some(guild) = self.messages.guild(guild_id) else {
            self.log(
                LogLevel::Warning,
                format!("Duel reminder guild unavailable; guild={guild_id}"),
            );
            return None;
        };

        if let Some(configured_id) = self.configured_channel_id {
            match self.messages.cached_channel(configured_id) {
                Some(channel) if channel.guild_id == Some(guild_id) => {
                    if self.can_send_reminder(&channel, guild_id) {
                        return Some(channel);
                    }
                    self.log(
                        LogLevel::Warning,
                        format!(
                            "Configured duel channel is not sendable; \
                             guild={guild_id} channel={configured_id}"
                        ),
                    );
                }
                Some(_) => self.log(
                    LogLevel::Debug,
                    format!(
                        "Configured duel channel belongs to another guild; \
                         guild={guild_id} channel={configured_id}"
                    ),
                ),
                None => self.log(
                    LogLevel::Debug,
                    format!(
                        "Configured duel channel not in cache; \
                         guild={guild_id} channel={configured_id}"
                    ),
                ),
            }
        }

        let mut matches = guild
            .text_channel_ids
            .iter()
            .filter_map(|channel_id| self.messages.cached_channel(*channel_id))
            .filter(|channel| {
                channel.kind == ChannelKind::Text && channel.name.contains(MAIN_CHANNEL_NAME)
            })
            .collect::<Vec<_>>();
        let exact = matches
            .iter()
            .filter(|channel| channel.name == MAIN_CHANNEL_NAME)
            .cloned()
            .collect::<Vec<_>>();
        if exact.len() == 1 {
            matches = exact;
        }
        if matches.len() != 1 {
            let level = if self.configured_channel_id.is_some() {
                LogLevel::Debug
            } else {
                LogLevel::Warning
            };
            self.log(
                level,
                format!(
                    "Expected one #{MAIN_CHANNEL_NAME} text channel; \
                     guild={guild_id} matches={}",
                    matches.len()
                ),
            );
            return None;
        }
        let channel = matches.remove(0);
        if !self.can_send_reminder(&channel, guild_id) {
            self.log(
                LogLevel::Warning,
                format!(
                    "Cannot send duel reminders in #{MAIN_CHANNEL_NAME}; \
                     guild={guild_id} channel={}",
                    channel.id
                ),
            );
            return None;
        }
        Some(channel)
    }

    #[must_use]
    pub fn can_send_reminder(&self, channel: &Channel, guild_id: i64) -> bool {
        matches!(channel.kind, ChannelKind::Text | ChannelKind::Thread)
            && channel.guild_id == Some(guild_id)
            && channel.can_send
            && self
                .messages
                .guild(guild_id)
                .is_some_and(|guild| guild.id == guild_id && guild.bot_member_present)
    }

    /// Resolve unique same-guild delivery targets in main-then-origin order.
    pub fn get_lifecycle_channels(&mut self, challenge: &DuelChallenge) -> Vec<Channel> {
        let mut main_channel = self.get_main_channel(challenge.guild_id);
        if let Some(configured_id) = self.configured_channel_id
            && main_channel.as_ref().map(|channel| channel.id) != Some(configured_id)
            && self.messages.cached_channel(configured_id).is_none()
        {
            let cached = self
                .configured_rescue_channel
                .get(&challenge.guild_id)
                .filter(|(at, channel)| {
                    self.monotonic_seconds - *at < CONFIGURED_RESCUE_CACHE_SECONDS
                        && channel.id == configured_id
                })
                .map(|(_, channel)| channel.clone());
            if let Some(channel) = cached {
                main_channel = Some(channel);
            } else {
                let can_retry = self
                    .configured_rescue_failed_at
                    .get(&challenge.guild_id)
                    .is_none_or(|last| {
                        self.monotonic_seconds - *last >= CONFIGURED_RESCUE_RETRY_SECONDS
                    });
                if can_retry {
                    match self.messages.fetch_channel(configured_id) {
                        Ok(Some(channel))
                            if self.can_send_reminder(&channel, challenge.guild_id) =>
                        {
                            main_channel = Some(channel.clone());
                            self.configured_rescue_channel
                                .insert(challenge.guild_id, (self.monotonic_seconds, channel));
                        }
                        Ok(Some(_)) => {
                            self.configured_rescue_failed_at
                                .insert(challenge.guild_id, self.monotonic_seconds);
                        }
                        Ok(None) | Err(_) => {
                            self.configured_rescue_failed_at
                                .insert(challenge.guild_id, self.monotonic_seconds);
                            self.log(
                                LogLevel::Debug,
                                format!(
                                    "Configured duel channel fetch rescue failed; \
                                     guild={} channel={configured_id}",
                                    challenge.guild_id
                                ),
                            );
                        }
                    }
                }
            }
        }

        let origin_channel = self.get_channel(challenge.channel_id);
        let mut channels = Vec::new();
        let mut channel_ids = BTreeSet::new();
        for channel in [main_channel, origin_channel].into_iter().flatten() {
            if channel_ids.insert(channel.id)
                && self.can_send_reminder(&channel, challenge.guild_id)
            {
                channels.push(channel);
            }
        }
        channels
    }

    /// Deliver one already-claimed reminder, unresolved notice, or expiry.
    pub fn deliver_due_result(&mut self, result: &DuelDueResult) -> Result<bool, CommandError> {
        let challenge = &result.challenge;
        let channels = self.get_lifecycle_channels(challenge);
        if channels.is_empty() {
            self.log(
                LogLevel::Warning,
                format!(
                    "duel lifecycle channel unavailable challenge={} guild={} channel={}",
                    challenge.challenge_id, challenge.guild_id, challenge.channel_id
                ),
            );
            return Ok(false);
        }

        let expected_status = match result.kind {
            DuelDueKind::Reminder => Some(DuelStatus::Pending),
            DuelDueKind::Unresolved => Some(DuelStatus::Accepted),
            DuelDueKind::Expired => None,
        };
        if let Some(status) = expected_status {
            let service = &mut self.service;
            let current = self.scheduler.run("duel.get_challenge", || {
                service.get_challenge(challenge.challenge_id, challenge.guild_id)
            })?;
            if current.as_ref().map(|current| current.status) != Some(status) {
                return Ok(false);
            }
        }

        let event = match result.kind {
            DuelDueKind::Reminder => FlavorEvent::Reminder,
            DuelDueKind::Expired => FlavorEvent::Expired,
            DuelDueKind::Unresolved => FlavorEvent::Unresolved,
        };
        let flavor =
            self.flavor
                .generate(event, challenge.guild_id, &Self::flavor_details(challenge));
        let (content, allowed_mentions) = match result.kind {
            DuelDueKind::Expired => {
                self.edit_original(challenge, &flavor);
                (
                    format!(
                        "{flavor}\nChallenge #{} was declined in cowardice by silence. \
                         The {} JC penalty was paid to the challenger.",
                        challenge.challenge_id,
                        challenge.decline_penalty()
                    ),
                    AllowedMentions::none(),
                )
            }
            DuelDueKind::Unresolved => {
                let trial = challenge.trial_type.map_or("The trial", trial_label);
                (
                    format!(
                        "<@{}> <@{}>\n{flavor}\nChallenge #{} awaits its verdict: \
                         {trial}, {} JC a side. Settle it, then have an admin record the \
                         outcome with /duel resolve.",
                        challenge.challenger_id,
                        challenge.recipient_id,
                        challenge.challenge_id,
                        challenge.wager
                    ),
                    AllowedMentions::users([challenge.challenger_id, challenge.recipient_id]),
                )
            }
            DuelDueKind::Reminder => {
                let mention = if result.ping_recipient {
                    format!("<@{}>\n", challenge.recipient_id)
                } else {
                    String::new()
                };
                (
                    format!(
                        "{mention}{flavor}\nChallenge #{} expires <t:{}:R>.",
                        challenge.challenge_id, challenge.expires_at
                    ),
                    if result.ping_recipient {
                        AllowedMentions::users([challenge.recipient_id])
                    } else {
                        AllowedMentions::none()
                    },
                )
            }
        };

        for channel in channels {
            if let Some(status) = expected_status {
                let service = &mut self.service;
                let current = self.scheduler.run("duel.get_challenge", || {
                    service.get_challenge(challenge.challenge_id, challenge.guild_id)
                })?;
                if current.as_ref().map(|current| current.status) != Some(status) {
                    return Ok(false);
                }
            }
            let payload = ResponsePayload {
                content: Some(content.clone()),
                allowed_mentions: allowed_mentions.clone(),
                ..ResponsePayload::default()
            };
            if self.messages.send_channel(channel.id, payload).is_ok() {
                return Ok(true);
            }
            self.log(
                LogLevel::Error,
                format!(
                    "Unable to deliver duel lifecycle message challenge={} guild={} channel={}",
                    challenge.challenge_id, challenge.guild_id, channel.id
                ),
            );
        }
        Ok(false)
    }

    /// Atomically claim one due challenge and reconcile delivery markers.
    pub fn process_due_challenge(
        &mut self,
        challenge_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<(), CommandError> {
        let service = &mut self.service;
        let Some(challenge) = self.scheduler.run("duel.get_challenge", || {
            service.get_challenge(challenge_id, guild_id)
        })?
        else {
            return Ok(());
        };
        let deferred = self.get_lifecycle_channels(&challenge).is_empty();
        if deferred {
            let should_warn = self.deferred_warned_at.get(&guild_id).is_none_or(|last| {
                self.monotonic_seconds - *last >= CONFIGURED_RESCUE_RETRY_SECONDS
            });
            if should_warn {
                self.deferred_warned_at
                    .insert(guild_id, self.monotonic_seconds);
                self.log(
                    LogLevel::Warning,
                    format!(
                        "No sendable channel for due duel work; deferring reminder claims. \
                         guild={guild_id} challenge={challenge_id}"
                    ),
                );
            }
        }
        let service = &mut self.service;
        let result = self.scheduler.run("duel.process_due", || {
            service.process_due(challenge_id, guild_id, now, !deferred)
        })?;
        let Some(result) = result else {
            return Ok(());
        };
        let delivered = self.deliver_due_result(&result)?;
        match result.kind {
            DuelDueKind::Expired if delivered => {
                let service = &mut self.service;
                let _ = self.scheduler.run("duel.confirm_expiry_announced", || {
                    service.confirm_expiry_announced(&result)
                })?;
            }
            DuelDueKind::Expired if result.is_redelivery => self.log(
                LogLevel::Warning,
                format!(
                    "Duel expiry announcement still undelivered, retrying with backoff: \
                     challenge={} guild={}",
                    result.challenge.challenge_id, result.challenge.guild_id
                ),
            ),
            DuelDueKind::Expired => self.log(
                LogLevel::Error,
                format!(
                    "Duel expiry settled but its announcement was not delivered: \
                     challenge={} guild={} challenger={} was refunded the {} JC escrow and \
                     received the {} JC penalty debited from recipient={}",
                    result.challenge.challenge_id,
                    result.challenge.guild_id,
                    result.challenge.challenger_id,
                    result.challenge.wager,
                    result.challenge.decline_penalty(),
                    result.challenge.recipient_id
                ),
            ),
            _ if !delivered => {
                let service = &mut self.service;
                let _ = self
                    .scheduler
                    .run("duel.rearm_reminder", || service.rearm_reminder(&result))?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Restore persistent controls and interrupted unbound announcements.
    pub fn recover_pending(&mut self) -> Result<RecoveryReport, CommandError> {
        let service = &mut self.service;
        let pending = self
            .scheduler
            .run("duel.list_pending_all", || service.list_pending_all())?;
        let mut bound = Vec::new();
        let mut unbound = Vec::new();
        let mut report = RecoveryReport::default();

        for challenge in pending {
            if let Some(message_id) = challenge.message_id {
                let view = ChallengeView::new(challenge.challenge_id);
                self.messages.register_view(message_id, view.clone());
                report.registered_views += 1;
                bound.push((challenge, message_id, view));
            } else {
                unbound.push(challenge);
            }
        }

        let channel_ids = bound
            .iter()
            .map(|(challenge, _, _)| challenge.channel_id)
            .collect::<BTreeSet<_>>();
        let mut channels = BTreeMap::new();
        for channel_id in channel_ids {
            channels.insert(channel_id, self.get_channel(channel_id));
            report.distinct_channel_resolutions += 1;
        }
        let edits = bound
            .into_iter()
            .filter_map(|(challenge, message_id, view)| {
                let channel = channels.get(&challenge.channel_id)?.as_ref()?;
                Some(BoundControlEdit {
                    challenge_id: challenge.challenge_id,
                    channel_id: channel.id,
                    message_id,
                    view,
                })
            })
            .collect::<Vec<_>>();
        report.attempted_bound_edits = edits.len();
        report.peak_bound_edits = edits.len().min(RECOVERY_EDIT_CONCURRENCY);
        for (challenge_id, result) in self
            .messages
            .restore_controls_bounded(edits, RECOVERY_EDIT_CONCURRENCY)
        {
            if result.is_err() {
                self.log(
                    LogLevel::Error,
                    format!("Unable to restore controls for duel challenge {challenge_id}"),
                );
            }
        }

        for challenge in unbound {
            if self.restore_unbound_challenge(&challenge)? {
                report.restored_unbound += 1;
            }
        }
        Ok(report)
    }

    pub fn restore_unbound_challenge(
        &mut self,
        challenge: &DuelChallenge,
    ) -> Result<bool, CommandError> {
        let flavor = self.flavor.generate(
            FlavorEvent::Issued,
            challenge.guild_id,
            &Self::flavor_details(challenge),
        );
        let Some(channel) = self.get_channel(challenge.channel_id) else {
            self.mark_recovery_delivery_failed(challenge)?;
            return Ok(false);
        };
        let allowed_mentions = AllowedMentions::users([challenge.recipient_id]);
        let message_id = match self.messages.send_channel(
            channel.id,
            ResponsePayload {
                content: Some(format!("<@{}>", challenge.recipient_id)),
                embed: Some(Self::build_challenge_embed(challenge, &flavor)),
                allowed_mentions: allowed_mentions.clone(),
                ..ResponsePayload::default()
            },
        ) {
            Ok(Some(message_id)) => message_id,
            Ok(None) | Err(_) => {
                self.log(
                    LogLevel::Error,
                    format!(
                        "Unable to deliver replacement for duel challenge {} in guild {} \
                         channel {}",
                        challenge.challenge_id, challenge.guild_id, challenge.channel_id
                    ),
                );
                self.mark_recovery_delivery_failed(challenge)?;
                return Ok(false);
            }
        };

        let service = &mut self.service;
        let bind = self.scheduler.run("duel.bind_message", || {
            service.bind_message(challenge.challenge_id, challenge.guild_id, message_id)
        });
        match bind {
            Ok(_) => {}
            Err(CommandError::Invalid(_)) => {
                self.log(
                    LogLevel::Info,
                    format!(
                        "Duel challenge {} changed before replacement message binding",
                        challenge.challenge_id
                    ),
                );
                self.delete_unbound_message(channel.id, message_id, challenge);
                return Ok(false);
            }
            Err(_) => {
                self.log(
                    LogLevel::Error,
                    format!(
                        "Unable to bind replacement for duel challenge {} in guild {}",
                        challenge.challenge_id, challenge.guild_id
                    ),
                );
                self.delete_unbound_message(channel.id, message_id, challenge);
                self.mark_recovery_delivery_failed(challenge)?;
                return Ok(false);
            }
        }

        let view = ChallengeView::new(challenge.challenge_id);
        self.messages.register_view(message_id, view.clone());
        if self
            .messages
            .edit_message(
                channel.id,
                message_id,
                ResponsePayload {
                    view: Some(view),
                    allowed_mentions,
                    ..ResponsePayload::default()
                },
            )
            .is_err()
        {
            self.log(
                LogLevel::Error,
                format!(
                    "Unable to attach controls to replacement for duel challenge {}",
                    challenge.challenge_id
                ),
            );
        }
        Ok(true)
    }

    fn mark_recovery_delivery_failed(
        &mut self,
        challenge: &DuelChallenge,
    ) -> Result<(), CommandError> {
        let service = &mut self.service;
        match self.scheduler.run("duel.mark_delivery_failed", || {
            service.mark_delivery_failed(
                challenge.challenge_id,
                challenge.guild_id,
                challenge.challenger_id,
            )
        }) {
            Ok(_) => Ok(()),
            Err(CommandError::Invalid(_)) => {
                self.log(
                    LogLevel::Info,
                    format!(
                        "Duel challenge {} changed before recovery delivery refund",
                        challenge.challenge_id
                    ),
                );
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

/// Match the Python setup guard before constructing the command adapter.
pub fn validate_setup_dependencies(
    has_duel_service: bool,
    has_flavor_service: bool,
) -> Result<(), &'static str> {
    if !has_duel_service {
        return Err("duel_service must be initialized before commands.duel");
    }
    if !has_flavor_service {
        return Err("duel_flavor_service must be initialized before commands.duel");
    }
    Ok(())
}

fn response_event(challenge: &DuelChallenge) -> FlavorEvent {
    if challenge.status == DuelStatus::Declined {
        FlavorEvent::Declined
    } else if challenge.status == DuelStatus::Voided {
        FlavorEvent::Voided
    } else if challenge.trial_type == Some(DuelTrial::TrialByCombat) {
        FlavorEvent::AcceptedCombat
    } else {
        FlavorEvent::AcceptedFive
    }
}

fn trial_detail(trial: DuelTrial) -> &'static str {
    match trial {
        DuelTrial::TrialByCombat => "Trial by Combat: best-of-three, one-versus-one Dota mid.",
        DuelTrial::TrialOfFive => "Trial of Five: a lobby using Immortal Draft.",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet, VecDeque};

    use super::*;

    const GUILD_ID: i64 = 100;
    const CHANNEL_ID: i64 = 200;
    const CHALLENGER_ID: i64 = 1;
    const RECIPIENT_ID: i64 = 2;

    fn challenge_with(
        status: DuelStatus,
        trial_type: Option<DuelTrial>,
        message_id: Option<i64>,
    ) -> DuelChallenge {
        DuelChallenge {
            challenge_id: 7,
            guild_id: GUILD_ID,
            channel_id: CHANNEL_ID,
            message_id,
            challenger_id: CHALLENGER_ID,
            recipient_id: RECIPIENT_ID,
            wager: 500,
            issuance_fee: 50,
            status,
            trial_type,
            challenger_glicko: 1_400.0,
            challenger_rd: 75.0,
            recipient_glicko: 1_550.0,
            recipient_rd: 65.0,
            created_at: 1_700_000_000,
            expires_at: 1_700_604_800,
            next_reminder_at: Some(1_700_432_000),
            responded_at: None,
            resolved_at: None,
            winner_id: None,
            resolution_actor_id: None,
        }
    }

    fn pending() -> DuelChallenge {
        challenge_with(DuelStatus::Pending, None, Some(300))
    }

    fn due(kind: DuelDueKind, challenge: DuelChallenge) -> DuelDueResult {
        DuelDueResult {
            kind,
            challenge,
            remaining_seconds: 48 * 3_600,
            ping_recipient: true,
            claimed_reminder_at: None,
            is_redelivery: false,
        }
    }

    #[derive(Default)]
    struct FakeInteraction {
        defer_result: bool,
        defer_calls: usize,
        immediate: Vec<ResponsePayload>,
        followups: Vec<ResponsePayload>,
        followup_results: VecDeque<Result<Option<i64>, TransportError>>,
        next_message_id: i64,
    }

    impl FakeInteraction {
        fn available() -> Self {
            Self {
                defer_result: true,
                next_message_id: 300,
                ..Self::default()
            }
        }
    }

    impl InteractionPort for FakeInteraction {
        fn defer(&mut self) -> bool {
            self.defer_calls += 1;
            self.defer_result
        }

        fn send_immediate(&mut self, payload: ResponsePayload) -> Result<(), TransportError> {
            self.immediate.push(payload);
            Ok(())
        }

        fn send_followup(
            &mut self,
            payload: ResponsePayload,
        ) -> Result<Option<i64>, TransportError> {
            self.followups.push(payload);
            self.followup_results.pop_front().unwrap_or_else(|| {
                let message_id = self.next_message_id;
                self.next_message_id += 1;
                Ok(Some(message_id))
            })
        }
    }

    struct FakeMessages {
        guilds: HashMap<i64, Guild>,
        cached: HashMap<i64, Channel>,
        fetched: HashMap<i64, Channel>,
        fetch_failures: HashSet<i64>,
        fetch_calls: Vec<i64>,
        sent: Vec<(i64, ResponsePayload)>,
        send_results: HashMap<i64, VecDeque<Result<Option<i64>, TransportError>>>,
        edits: Vec<(i64, i64, ResponsePayload)>,
        edit_failures: HashSet<i64>,
        deletes: Vec<(i64, i64)>,
        registered: Vec<(i64, ChallengeView)>,
        bounded_limits: Vec<usize>,
        events: Vec<String>,
        next_message_id: i64,
    }

    impl Default for FakeMessages {
        fn default() -> Self {
            let guild = Guild {
                id: GUILD_ID,
                bot_member_present: true,
                text_channel_ids: vec![CHANNEL_ID],
            };
            let channel = Channel {
                id: CHANNEL_ID,
                guild_id: Some(GUILD_ID),
                name: MAIN_CHANNEL_NAME.to_owned(),
                kind: ChannelKind::Text,
                can_send: true,
            };
            Self {
                guilds: HashMap::from([(GUILD_ID, guild)]),
                cached: HashMap::from([(CHANNEL_ID, channel)]),
                fetched: HashMap::new(),
                fetch_failures: HashSet::new(),
                fetch_calls: Vec::new(),
                sent: Vec::new(),
                send_results: HashMap::new(),
                edits: Vec::new(),
                edit_failures: HashSet::new(),
                deletes: Vec::new(),
                registered: Vec::new(),
                bounded_limits: Vec::new(),
                events: Vec::new(),
                next_message_id: 900,
            }
        }
    }

    impl FakeMessages {
        fn channel(id: i64, name: &str) -> Channel {
            Channel {
                id,
                guild_id: Some(GUILD_ID),
                name: name.to_owned(),
                kind: ChannelKind::Text,
                can_send: true,
            }
        }

        fn set_main_and_origin(&mut self) {
            self.cached.get_mut(&CHANNEL_ID).expect("origin").name = "gamba".to_owned();
            self.cached
                .insert(999, Self::channel(999, MAIN_CHANNEL_NAME));
            self.guilds
                .get_mut(&GUILD_ID)
                .expect("guild")
                .text_channel_ids = vec![CHANNEL_ID, 999];
        }

        fn fail_send(&mut self, channel_id: i64) {
            self.send_results
                .entry(channel_id)
                .or_default()
                .push_back(Err(TransportError::new("forbidden")));
        }
    }

    impl MessagePort for FakeMessages {
        fn guild(&self, guild_id: i64) -> Option<Guild> {
            self.guilds.get(&guild_id).cloned()
        }

        fn cached_channel(&self, channel_id: i64) -> Option<Channel> {
            self.cached.get(&channel_id).cloned()
        }

        fn fetch_channel(&mut self, channel_id: i64) -> Result<Option<Channel>, TransportError> {
            self.fetch_calls.push(channel_id);
            if self.fetch_failures.contains(&channel_id) {
                Err(TransportError::new("fetch failed"))
            } else {
                Ok(self.fetched.get(&channel_id).cloned())
            }
        }

        fn send_channel(
            &mut self,
            channel_id: i64,
            payload: ResponsePayload,
        ) -> Result<Option<i64>, TransportError> {
            self.events.push(format!("send:{channel_id}"));
            self.sent.push((channel_id, payload));
            if let Some(result) = self
                .send_results
                .get_mut(&channel_id)
                .and_then(VecDeque::pop_front)
            {
                result
            } else {
                let message_id = self.next_message_id;
                self.next_message_id += 1;
                Ok(Some(message_id))
            }
        }

        fn edit_message(
            &mut self,
            channel_id: i64,
            message_id: i64,
            payload: ResponsePayload,
        ) -> Result<(), TransportError> {
            self.events.push(format!("edit:{message_id}"));
            self.edits.push((channel_id, message_id, payload));
            if self.edit_failures.contains(&message_id) {
                Err(TransportError::new("edit failed"))
            } else {
                Ok(())
            }
        }

        fn delete_message(
            &mut self,
            channel_id: i64,
            message_id: i64,
        ) -> Result<(), TransportError> {
            self.events.push(format!("delete:{message_id}"));
            self.deletes.push((channel_id, message_id));
            Ok(())
        }

        fn register_view(&mut self, message_id: i64, view: ChallengeView) {
            self.events.push(format!("register:{message_id}"));
            self.registered.push((message_id, view));
        }

        fn restore_controls_bounded(
            &mut self,
            edits: Vec<BoundControlEdit>,
            max_concurrency: usize,
        ) -> Vec<(i64, Result<(), TransportError>)> {
            self.bounded_limits.push(max_concurrency);
            edits
                .into_iter()
                .map(|edit| {
                    let challenge_id = edit.challenge_id;
                    let result = self.edit_message(
                        edit.channel_id,
                        edit.message_id,
                        ResponsePayload {
                            view: Some(edit.view),
                            allowed_mentions: AllowedMentions::none(),
                            ..ResponsePayload::default()
                        },
                    );
                    (challenge_id, result)
                })
                .collect()
        }
    }

    #[derive(Default)]
    struct FakeMembers {
        members: HashMap<(i64, i64), Member>,
        admins: HashSet<(i64, i64)>,
    }

    impl MemberPort for FakeMembers {
        fn member(&self, guild_id: i64, member_id: i64) -> Option<Member> {
            self.members.get(&(guild_id, member_id)).copied()
        }

        fn is_admin(&self, guild_id: i64, member_id: i64) -> bool {
            self.admins.contains(&(guild_id, member_id))
        }
    }

    #[derive(Default)]
    struct FakeScheduler {
        operations: Vec<&'static str>,
    }

    impl SchedulerPort for FakeScheduler {
        fn run<T, F: FnOnce() -> T>(&mut self, operation: &'static str, work: F) -> T {
            self.operations.push(operation);
            work()
        }
    }

    #[derive(Default)]
    struct FakeFlavor {
        calls: Vec<(FlavorEvent, i64, FlavorDetails)>,
    }

    impl FlavorPort for FakeFlavor {
        fn generate(
            &mut self,
            event: FlavorEvent,
            guild_id: i64,
            details: &FlavorDetails,
        ) -> String {
            self.calls.push((event, guild_id, details.clone()));
            "The herald speaks.".to_owned()
        }
    }

    struct FakeService {
        issue_results: VecDeque<Result<DuelChallenge, CommandError>>,
        bind_results: VecDeque<Result<DuelChallenge, CommandError>>,
        mark_results: VecDeque<Result<DuelChallenge, CommandError>>,
        get_results: VecDeque<Result<Option<DuelChallenge>, CommandError>>,
        get_default: Option<DuelChallenge>,
        respond_results: VecDeque<Result<DuelChallenge, CommandError>>,
        list_results: VecDeque<Result<Vec<DuelChallenge>, CommandError>>,
        pending_results: VecDeque<Result<Vec<DuelChallenge>, CommandError>>,
        resolve_results: VecDeque<Result<DuelChallenge, CommandError>>,
        resolve_deductions: ProfitDeductions,
        due_results: VecDeque<Result<Option<DuelDueResult>, CommandError>>,
        rearm_results: VecDeque<Result<bool, CommandError>>,
        confirm_results: VecDeque<Result<bool, CommandError>>,
        issue_calls: Vec<(i64, i64, i64, i64, i64, bool)>,
        bind_calls: Vec<(i64, i64, i64)>,
        mark_calls: Vec<(i64, i64, i64)>,
        get_calls: Vec<(i64, i64)>,
        respond_calls: Vec<(i64, i64, String)>,
        list_calls: Vec<i64>,
        pending_calls: usize,
        resolve_calls: Vec<(i64, i64, i64, String)>,
        due_calls: Vec<(i64, i64, i64, bool)>,
        rearm_calls: Vec<i64>,
        confirm_calls: Vec<i64>,
    }

    impl Default for FakeService {
        fn default() -> Self {
            let accepted = challenge_with(
                DuelStatus::Accepted,
                Some(DuelTrial::TrialByCombat),
                Some(300),
            );
            Self {
                issue_results: VecDeque::new(),
                bind_results: VecDeque::new(),
                mark_results: VecDeque::new(),
                get_results: VecDeque::new(),
                get_default: Some(pending()),
                respond_results: VecDeque::from([Ok(accepted)]),
                list_results: VecDeque::from([Ok(vec![pending()])]),
                pending_results: VecDeque::new(),
                resolve_results: VecDeque::new(),
                resolve_deductions: ProfitDeductions::default(),
                due_results: VecDeque::new(),
                rearm_results: VecDeque::new(),
                confirm_results: VecDeque::new(),
                issue_calls: Vec::new(),
                bind_calls: Vec::new(),
                mark_calls: Vec::new(),
                get_calls: Vec::new(),
                respond_calls: Vec::new(),
                list_calls: Vec::new(),
                pending_calls: 0,
                resolve_calls: Vec::new(),
                due_calls: Vec::new(),
                rearm_calls: Vec::new(),
                confirm_calls: Vec::new(),
            }
        }
    }

    impl DuelServicePort for FakeService {
        fn issue(
            &mut self,
            guild_id: i64,
            channel_id: i64,
            challenger_id: i64,
            recipient_id: i64,
            wager: i64,
            recipient_is_bot: bool,
        ) -> Result<DuelChallenge, CommandError> {
            self.issue_calls.push((
                guild_id,
                channel_id,
                challenger_id,
                recipient_id,
                wager,
                recipient_is_bot,
            ));
            self.issue_results
                .pop_front()
                .unwrap_or_else(|| Ok(pending()))
        }

        fn bind_message(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            message_id: i64,
        ) -> Result<DuelChallenge, CommandError> {
            self.bind_calls.push((challenge_id, guild_id, message_id));
            self.bind_results.pop_front().unwrap_or_else(|| {
                let mut challenge = pending();
                challenge.message_id = Some(message_id);
                Ok(challenge)
            })
        }

        fn mark_delivery_failed(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            actor_id: i64,
        ) -> Result<DuelChallenge, CommandError> {
            self.mark_calls.push((challenge_id, guild_id, actor_id));
            self.mark_results
                .pop_front()
                .unwrap_or_else(|| Ok(challenge_with(DuelStatus::DeliveryFailed, None, None)))
        }

        fn get_challenge(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
        ) -> Result<Option<DuelChallenge>, CommandError> {
            self.get_calls.push((challenge_id, guild_id));
            self.get_results
                .pop_front()
                .unwrap_or_else(|| Ok(self.get_default.clone()))
        }

        fn respond(
            &mut self,
            guild_id: i64,
            recipient_id: i64,
            choice: &str,
        ) -> Result<DuelChallenge, CommandError> {
            self.respond_calls
                .push((guild_id, recipient_id, choice.to_owned()));
            self.respond_results
                .pop_front()
                .unwrap_or_else(|| Ok(pending()))
        }

        fn list_outstanding(&mut self, guild_id: i64) -> Result<Vec<DuelChallenge>, CommandError> {
            self.list_calls.push(guild_id);
            self.list_results
                .pop_front()
                .unwrap_or_else(|| Ok(Vec::new()))
        }

        fn list_pending_all(&mut self) -> Result<Vec<DuelChallenge>, CommandError> {
            self.pending_calls += 1;
            self.pending_results
                .pop_front()
                .unwrap_or_else(|| Ok(Vec::new()))
        }

        fn resolve(
            &mut self,
            guild_id: i64,
            actor_id: i64,
            challenge_id: i64,
            outcome: &str,
            _policy: &ProfitDeductionPolicy<'_>,
        ) -> Result<DuelResolution, CommandError> {
            self.resolve_calls
                .push((guild_id, actor_id, challenge_id, outcome.to_owned()));
            self.resolve_results
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(challenge_with(
                        DuelStatus::Voided,
                        Some(DuelTrial::TrialByCombat),
                        Some(300),
                    ))
                })
                .map(|challenge| DuelResolution {
                    challenge,
                    deductions: self.resolve_deductions,
                })
        }

        fn process_due(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            now: i64,
            claim_reminders: bool,
        ) -> Result<Option<DuelDueResult>, CommandError> {
            self.due_calls
                .push((challenge_id, guild_id, now, claim_reminders));
            self.due_results.pop_front().unwrap_or(Ok(None))
        }

        fn rearm_reminder(&mut self, result: &DuelDueResult) -> Result<bool, CommandError> {
            self.rearm_calls.push(result.challenge.challenge_id);
            self.rearm_results.pop_front().unwrap_or(Ok(true))
        }

        fn confirm_expiry_announced(
            &mut self,
            result: &DuelDueResult,
        ) -> Result<bool, CommandError> {
            self.confirm_calls.push(result.challenge.challenge_id);
            self.confirm_results.pop_front().unwrap_or(Ok(true))
        }
    }

    type Harness = DuelCommands<FakeService, FakeFlavor, FakeMessages, FakeMembers, FakeScheduler>;

    fn harness() -> Harness {
        DuelCommands::new(
            FakeService::default(),
            FakeFlavor::default(),
            FakeMessages::default(),
            FakeMembers::default(),
            FakeScheduler::default(),
        )
    }

    fn interaction() -> InteractionContext {
        InteractionContext {
            guild_id: Some(GUILD_ID),
            channel_id: Some(CHANNEL_ID),
            user_id: RECIPIENT_ID,
        }
    }

    fn content(payload: &ResponsePayload) -> &str {
        payload.content.as_deref().expect("content")
    }

    fn assert_mentions_disabled(mentions: &AllowedMentions) {
        assert!(!mentions.everyone);
        assert!(!mentions.roles);
        assert!(!mentions.replied_user);
        assert!(mentions.users.is_empty());
    }

    #[test]
    fn test_trial_by_combat_rules_match_agreed_format() {
        assert_eq!(
            TRIAL_BY_COMBAT_RULES,
            "Mirror matchups: both duelists play the same hero each game.\n\
             • Game 1 hero: recipient's pick\n\
             • Game 2 hero: challenger's pick\n\
             • Tiebreaker: Shadow Fiend, mid\n\
             • Hero pool: all heroes, no bans\n\
             • Victory: tower destruction, two kills, or opponent surrender\n\
             • If neither player has won by 15:00, the higher score wins: creep score + (kills × 35)\n\
             • Prohibited: farming in the jungle, destroying observer wards, visiting other lanes, blocking the first wave of creeps, collecting and using runes, Bottle, and Infused Raindrops\n\
             Players can agree to additional game rules that do not conflict with existing regulations."
        );
    }

    #[test]
    fn test_flavor_details_do_not_expose_discord_ids() {
        let mut challenge = pending();
        challenge.challenger_id = 111_222_333_444_555_666;
        challenge.recipient_id = 777_888_999_000_111_222;
        let details = Harness::flavor_details(&challenge);
        assert_eq!(details.challenger, "the challenger");
        assert_eq!(details.recipient, "the recipient");
        let rendered = format!("{details:?}");
        assert!(!rendered.contains("111222333444555666"));
        assert!(!rendered.contains("777888999000111222"));
    }

    #[test]
    fn test_duel_is_one_top_level_group_with_approved_subcommands() {
        assert_eq!(COMMAND_GROUP.name, "duel");
        assert_eq!(COMMAND_GROUP.description, "Challenges of honor");
        assert_eq!(
            COMMAND_GROUP
                .subcommands
                .iter()
                .copied()
                .collect::<HashSet<_>>(),
            HashSet::from(["issue", "respond", "list", "resolve"])
        );
    }

    #[test]
    fn test_duel_service_calls_are_offloaded_from_the_event_loop() {
        let mut command = harness();
        let mut io = FakeInteraction::available();
        command.issue(&interaction(), &mut io, RECIPIENT_ID, 500);
        assert_eq!(
            command.scheduler.operations,
            ["duel.issue", "duel.bind_message"]
        );

        let mut command = harness();
        let mut io = FakeInteraction::available();
        command.handle_response(&interaction(), &mut io, Some(7), "trial_by_combat");
        assert!(
            command
                .scheduler
                .operations
                .starts_with(&["duel.get_challenge", "duel.respond"])
        );

        let mut command = harness();
        let mut io = FakeInteraction::available();
        command.list(&interaction(), &mut io);
        command.members.admins.insert((GUILD_ID, RECIPIENT_ID));
        command.resolve(
            &interaction(),
            &mut io,
            7,
            "void",
            &ProfitDeductionPolicy::none(),
        );
        command.recover_pending().expect("recovery");
        assert!(
            command
                .scheduler
                .operations
                .contains(&"duel.list_outstanding")
        );
        assert!(command.scheduler.operations.contains(&"duel.resolve"));
        assert!(
            command
                .scheduler
                .operations
                .contains(&"duel.list_pending_all")
        );

        let mut command = harness();
        let mut expired = pending();
        expired.status = DuelStatus::Expired;
        command
            .service
            .due_results
            .push_back(Ok(Some(due(DuelDueKind::Expired, expired))));
        command
            .process_due_challenge(7, GUILD_ID, 1_700_432_000)
            .expect("due");
        assert!(command.scheduler.operations.contains(&"duel.process_due"));
        assert!(
            command
                .scheduler
                .operations
                .contains(&"duel.confirm_expiry_announced")
        );
    }

    #[test]
    fn test_response_buttons_have_durable_challenge_specific_ids() {
        let view = ChallengeView::new(42);
        assert_eq!(view.timeout_seconds, None);
        assert_eq!(
            view.buttons
                .iter()
                .map(|button| (button.label.as_str(), button.custom_id.as_str()))
                .collect::<Vec<_>>(),
            [
                ("Decline in Cowardice", "duel:42:decline"),
                ("Trial by Combat", "duel:42:trial_by_combat"),
                ("Trial by Five", "duel:42:trial_of_five"),
            ]
        );
    }

    #[test]
    fn test_button_calls_shared_response_handler() {
        let mut command = harness();
        let mut io = FakeInteraction::available();
        command.handle_button(&interaction(), &mut io, "duel:7:trial_by_combat");
        assert_eq!(
            command.service.respond_calls,
            [(GUILD_ID, RECIPIENT_ID, "trial_by_combat".to_owned())]
        );
        assert_eq!(io.defer_calls, 1);
    }

    #[test]
    fn test_issue_posts_scoped_ping_and_binds_message() {
        let mut command = harness();
        let mut io = FakeInteraction::available();
        command.issue(&interaction(), &mut io, RECIPIENT_ID, 500);

        assert_eq!(io.defer_calls, 1);
        assert_eq!(
            command.service.issue_calls,
            [(GUILD_ID, CHANNEL_ID, RECIPIENT_ID, RECIPIENT_ID, 500, false)]
        );
        assert_eq!(command.flavor.calls.len(), 1);
        assert_eq!(command.flavor.calls[0].2.issuance_fee, 50);
        assert_eq!(content(&io.followups[0]), "<@2>");
        assert!(io.followups[0].view.is_none());
        assert!(!io.followups[0].clear_view);
        assert_eq!(io.followups[0].allowed_mentions.users, [RECIPIENT_ID]);
        assert_eq!(command.service.bind_calls, [(7, GUILD_ID, 300)]);
        assert_eq!(command.messages.edits.len(), 1);
        assert_eq!(
            command.messages.edits[0].2.view,
            Some(ChallengeView::new(7))
        );
    }

    #[test]
    fn test_issue_rejects_dms_ephemerally() {
        let mut command = harness();
        let mut io = FakeInteraction::available();
        let mut context = interaction();
        context.guild_id = None;
        command.issue(&context, &mut io, RECIPIENT_ID, 500);
        assert!(command.service.issue_calls.is_empty());
        assert_eq!(io.immediate.len(), 1);
        assert_eq!(io.immediate[0].visibility, Visibility::Ephemeral);
        assert_eq!(
            content(&io.immediate[0]),
            "This command must be used in a server."
        );
    }

    #[test]
    fn test_issue_reports_recipient_funding_failure_publicly_without_creating_post() {
        let mut command = harness();
        command
            .service
            .issue_results
            .push_back(Err(CommandError::RecipientFunding {
                recipient_id: RECIPIENT_ID,
                wager: 500,
            }));
        let mut io = FakeInteraction::available();
        let mut context = interaction();
        context.user_id = CHALLENGER_ID;
        command.issue(&context, &mut io, RECIPIENT_ID, 500);
        assert_eq!(
            content(&io.followups[0]),
            "The duel from <@1> to <@2> failed because the challenged player cannot cover the 500 JC wager."
        );
        assert_eq!(io.followups[0].visibility, Visibility::Public);
        assert_mentions_disabled(&io.followups[0].allowed_mentions);
        assert!(command.flavor.calls.is_empty());
        assert!(command.service.bind_calls.is_empty());
    }

    fn assert_issue_rule_error(recipient_id: i64, bot: bool, expected: &str) {
        let mut command = harness();
        command.members.members.insert(
            (GUILD_ID, recipient_id),
            Member {
                id: recipient_id,
                bot,
            },
        );
        command
            .service
            .issue_results
            .push_back(Err(CommandError::Invalid(expected.to_owned())));
        let mut io = FakeInteraction::available();
        command.issue(&interaction(), &mut io, recipient_id, 500);
        assert_eq!(content(&io.followups[0]), expected);
        assert_eq!(io.followups[0].visibility, Visibility::Ephemeral);
        assert_mentions_disabled(&io.followups[0].allowed_mentions);
        assert_eq!(command.service.issue_calls[0].5, bot);
    }

    #[test]
    fn test_issue_surfaces_self_and_bot_rule_errors_self() {
        assert_issue_rule_error(RECIPIENT_ID, false, "You cannot challenge yourself.");
    }

    #[test]
    fn test_issue_surfaces_self_and_bot_rule_errors_bot() {
        assert_issue_rule_error(3, true, "Bots cannot answer a challenge of honor.");
    }

    #[test]
    fn test_issue_refunds_escrow_when_initial_delivery_fails() {
        let mut command = harness();
        let mut io = FakeInteraction::available();
        io.followup_results
            .push_back(Err(TransportError::new("delivery failed")));
        command.issue(&interaction(), &mut io, RECIPIENT_ID, 500);
        assert_eq!(command.service.mark_calls, [(7, GUILD_ID, RECIPIENT_ID)]);
        assert!(command.service.bind_calls.is_empty());
        assert!(io.followups.iter().any(|payload| {
            content(payload).contains("wager and 50 JC issuance fee were refunded")
        }));
    }

    #[test]
    fn test_issue_bind_race_discards_duplicate_without_cancelling_challenge() {
        let mut command = harness();
        command
            .service
            .bind_results
            .push_back(Err(CommandError::Invalid("bind race".to_owned())));
        let mut io = FakeInteraction::available();
        command.issue(&interaction(), &mut io, RECIPIENT_ID, 500);
        assert!(io.followups[0].view.is_none());
        assert_eq!(command.messages.deletes, [(CHANNEL_ID, 300)]);
        assert!(command.service.mark_calls.is_empty());
        assert!(command.messages.edits.is_empty());
        assert_eq!(io.followups.len(), 1);
    }

    #[test]
    fn test_issue_bind_error_deletes_inert_post_before_refund() {
        let mut command = harness();
        command
            .service
            .bind_results
            .push_back(Err(CommandError::Internal(
                "database unavailable".to_owned(),
            )));
        let mut io = FakeInteraction::available();
        command.issue(&interaction(), &mut io, RECIPIENT_ID, 500);
        assert_eq!(command.messages.deletes, [(CHANNEL_ID, 300)]);
        assert_eq!(command.service.mark_calls, [(7, GUILD_ID, RECIPIENT_ID)]);
        assert!(
            content(io.followups.last().expect("feedback"))
                .contains("wager and 50 JC issuance fee were refunded")
        );
    }

    #[test]
    fn test_issue_delivery_failure_does_not_refund_a_concurrently_bound_challenge() {
        let mut command = harness();
        command
            .service
            .mark_results
            .push_back(Err(CommandError::Invalid("already bound".to_owned())));
        let mut io = FakeInteraction::available();
        io.followup_results
            .push_back(Err(TransportError::new("delivery failed")));
        command.issue(&interaction(), &mut io, RECIPIENT_ID, 500);
        assert_eq!(command.service.mark_calls, [(7, GUILD_ID, RECIPIENT_ID)]);
        assert!(!io.followups.iter().any(|payload| {
            payload
                .content
                .as_deref()
                .is_some_and(|text| text.contains("were refunded"))
        }));
    }

    #[test]
    fn test_issue_view_edit_failure_does_not_refund_bound_challenge() {
        let mut command = harness();
        command.messages.edit_failures.insert(300);
        let mut io = FakeInteraction::available();
        command.issue(&interaction(), &mut io, RECIPIENT_ID, 500);
        assert_eq!(command.service.bind_calls, [(7, GUILD_ID, 300)]);
        assert_eq!(command.messages.edits.len(), 1);
        assert!(command.service.mark_calls.is_empty());
    }

    #[test]
    fn test_recipient_button_and_command_share_handler() {
        let mut command = harness();
        command.service.respond_results.push_back(Ok(challenge_with(
            DuelStatus::Accepted,
            Some(DuelTrial::TrialByCombat),
            Some(300),
        )));
        let mut io = FakeInteraction::available();
        command.handle_response(&interaction(), &mut io, Some(7), "trial_by_combat");
        command.respond(&interaction(), &mut io, "trial_by_combat");
        assert_eq!(
            command.service.respond_calls,
            [
                (GUILD_ID, RECIPIENT_ID, "trial_by_combat".to_owned()),
                (GUILD_ID, RECIPIENT_ID, "trial_by_combat".to_owned()),
            ]
        );
    }

    fn assert_button_rejected(candidate: Option<DuelChallenge>) {
        let mut command = harness();
        command.service.get_default = candidate;
        let mut io = FakeInteraction::available();
        command.handle_response(&interaction(), &mut io, Some(7), "decline");
        assert_eq!(command.service.get_calls, [(7, GUILD_ID)]);
        assert!(command.service.respond_calls.is_empty());
        assert_eq!(io.defer_calls, 0);
        assert_eq!(io.immediate.len(), 1);
        assert_eq!(io.immediate[0].visibility, Visibility::Ephemeral);
        assert_eq!(
            content(&io.immediate[0]),
            "This duel challenge is unavailable or is not yours to answer."
        );
    }

    #[test]
    fn test_button_rejects_stale_unauthorized_or_cross_guild_challenge_none() {
        assert_button_rejected(None);
    }

    #[test]
    fn test_button_rejects_stale_unauthorized_or_cross_guild_challenge_wrong_recipient() {
        let mut candidate = pending();
        candidate.recipient_id = 99;
        assert_button_rejected(Some(candidate));
    }

    #[test]
    fn test_button_rejects_stale_unauthorized_or_cross_guild_challenge_wrong_id() {
        let mut candidate = pending();
        candidate.challenge_id = 8;
        assert_button_rejected(Some(candidate));
    }

    #[test]
    fn test_button_rejects_stale_unauthorized_or_cross_guild_challenge_wrong_guild() {
        let mut candidate = pending();
        candidate.guild_id = 999;
        assert_button_rejected(Some(candidate));
    }

    #[test]
    fn test_button_rejects_stale_unauthorized_or_cross_guild_challenge_not_pending() {
        let mut candidate = pending();
        candidate.status = DuelStatus::Accepted;
        assert_button_rejected(Some(candidate));
    }

    #[test]
    fn test_button_rejects_stale_unauthorized_or_cross_guild_challenge_unbound() {
        let mut candidate = pending();
        candidate.message_id = None;
        assert_button_rejected(Some(candidate));
    }

    fn assert_acceptance(trial: DuelTrial, expected_detail: &str) {
        let mut command = harness();
        let accepted = challenge_with(DuelStatus::Accepted, Some(trial), Some(300));
        command.service.respond_results.clear();
        command
            .service
            .respond_results
            .push_back(Ok(accepted.clone()));
        let mut io = FakeInteraction::available();
        command.handle_response(&interaction(), &mut io, Some(7), trial.as_str());

        assert_eq!(command.messages.edits.len(), 1);
        let edited = &command.messages.edits[0].2;
        assert!(edited.view.is_none());
        assert!(edited.clear_view);
        let embed = edited.embed.as_ref().expect("embed");
        assert_eq!(embed.field_value("Status"), Some("Accepted"));
        assert_eq!(
            command.flavor.calls[0].0,
            if trial == DuelTrial::TrialByCombat {
                FlavorEvent::AcceptedCombat
            } else {
                FlavorEvent::AcceptedFive
            }
        );
        let announcement = io.followups.last().expect("announcement");
        let announcement_text = content(announcement);
        assert!(announcement_text.contains(expected_detail));
        assert!(announcement_text.contains("Challenge #7"));
        assert!(announcement_text.contains("<@1> vs <@2>"));
        assert!(announcement_text.contains("500 JC"));
        if trial == DuelTrial::TrialByCombat {
            assert!(announcement_text.contains(TRIAL_BY_COMBAT_RULES));
            assert_eq!(embed.field_value("Rules"), Some(TRIAL_BY_COMBAT_RULES));
        } else {
            assert_eq!(embed.field_value("Rules"), None);
            assert!(!announcement_text.contains("Shadow Fiend"));
        }
        assert_mentions_disabled(&announcement.allowed_mentions);
    }

    #[test]
    fn test_acceptance_edits_original_and_posts_approved_trial_detail_trial_by_combat() {
        assert_acceptance(
            DuelTrial::TrialByCombat,
            "best-of-three, one-versus-one Dota mid",
        );
    }

    #[test]
    fn test_acceptance_edits_original_and_posts_approved_trial_detail_trial_of_five() {
        assert_acceptance(DuelTrial::TrialOfFive, "a lobby using Immortal Draft");
    }

    #[test]
    fn test_decline_edits_original_disables_buttons_and_posts_detail() {
        let mut command = harness();
        command.service.respond_results.clear();
        command.service.respond_results.push_back(Ok(challenge_with(
            DuelStatus::Declined,
            None,
            Some(300),
        )));
        let mut io = FakeInteraction::available();
        command.handle_response(&interaction(), &mut io, Some(7), "decline");
        let edited = &command.messages.edits[0].2;
        assert!(edited.view.is_none());
        assert_eq!(
            edited.embed.as_ref().expect("embed").field_value("Status"),
            Some("Declined")
        );
        let sent = io.followups.last().expect("announcement");
        assert!(content(sent).contains("250 JC"));
        assert_mentions_disabled(&sent.allowed_mentions);
    }

    #[test]
    fn test_acceptance_funding_failure_voids_post_and_explains_refund() {
        let mut command = harness();
        command.service.respond_results.clear();
        command.service.respond_results.push_back(Ok(challenge_with(
            DuelStatus::Voided,
            None,
            Some(300),
        )));
        let mut io = FakeInteraction::available();
        command.handle_response(&interaction(), &mut io, Some(7), "trial_by_combat");
        let edited = &command.messages.edits[0].2;
        assert!(edited.view.is_none());
        let embed = edited.embed.as_ref().expect("embed");
        assert_eq!(embed.field_value("Status"), Some("Voided"));
        assert_eq!(
            embed.field_value("Funding Failure"),
            Some("The recipient could not fund the 500 JC wager.")
        );
        assert_eq!(
            embed.field_value("Refund"),
            Some(
                "500 JC challenger stake refunded; 50 JC issuance fee remains nonrefundable after delivery"
            )
        );
        assert_eq!(command.flavor.calls[0].0, FlavorEvent::Voided);
        let sent = io.followups.last().expect("announcement");
        assert_eq!(sent.visibility, Visibility::Public);
        assert!(content(sent).contains("recipient could not fund the 500 JC wager"));
        assert!(content(sent).contains("challenger's 500 JC stake was refunded"));
        assert!(content(sent).contains("50 JC issuance fee remains nonrefundable after delivery"));
        assert_mentions_disabled(&sent.allowed_mentions);
    }

    #[test]
    fn test_acceptance_funding_failure_still_announces_when_original_was_deleted() {
        let mut command = harness();
        command.service.respond_results.clear();
        command.service.respond_results.push_back(Ok(challenge_with(
            DuelStatus::Voided,
            None,
            Some(300),
        )));
        command.messages.edit_failures.insert(300);
        let mut io = FakeInteraction::available();
        command.handle_response(&interaction(), &mut io, Some(7), "trial_by_combat");
        assert_eq!(command.flavor.calls[0].0, FlavorEvent::Voided);
        let sent = io.followups.last().expect("announcement");
        assert!(content(sent).contains("recipient could not fund the 500 JC wager"));
        assert_eq!(sent.visibility, Visibility::Public);
        assert_eq!(command.messages.edits.len(), 1);
    }

    fn assert_final_due_reminder(remaining_seconds: i64) {
        let mut command = harness();
        let mut result = due(DuelDueKind::Reminder, pending());
        result.remaining_seconds = remaining_seconds;
        assert!(command.deliver_due_result(&result).expect("delivery"));
        assert_eq!(command.flavor.calls[0].0, FlavorEvent::Reminder);
        let (_, payload) = command.messages.sent.last().expect("send");
        assert!(content(payload).starts_with("<@2>"));
        assert_eq!(payload.allowed_mentions.users, [RECIPIENT_ID]);
        assert!(!payload.allowed_mentions.everyone);
        assert!(!payload.allowed_mentions.roles);
        assert!(!payload.allowed_mentions.replied_user);
    }

    #[test]
    fn test_final_due_reminders_post_only_the_recipient_ping_172800() {
        assert_final_due_reminder(172_800);
    }

    #[test]
    fn test_final_due_reminders_post_only_the_recipient_ping_86400() {
        assert_final_due_reminder(86_400);
    }

    #[test]
    fn test_unresolved_daily_reminder_pings_both_participants() {
        let mut command = harness();
        let accepted = challenge_with(
            DuelStatus::Accepted,
            Some(DuelTrial::TrialByCombat),
            Some(300),
        );
        command.service.get_default = Some(accepted.clone());
        assert!(
            command
                .deliver_due_result(&due(DuelDueKind::Unresolved, accepted))
                .expect("delivery")
        );
        assert_eq!(command.flavor.calls[0].0, FlavorEvent::Unresolved);
        let (_, payload) = command.messages.sent.last().expect("send");
        assert!(content(payload).starts_with("<@1> <@2>"));
        assert!(content(payload).contains("Challenge #7"));
        assert!(content(payload).contains("Trial by Combat"));
        assert!(content(payload).contains("/duel resolve"));
        assert_eq!(
            payload
                .allowed_mentions
                .users
                .iter()
                .copied()
                .collect::<HashSet<_>>(),
            HashSet::from([CHALLENGER_ID, RECIPIENT_ID])
        );
    }

    fn assert_due_posts_in_main(kind: DuelDueKind, status: DuelStatus) {
        let mut command = harness();
        command.messages.set_main_and_origin();
        let trial = (status == DuelStatus::Accepted).then_some(DuelTrial::TrialByCombat);
        let challenge = challenge_with(status, trial, Some(300));
        command.service.get_default = Some(challenge.clone());
        assert!(
            command
                .deliver_due_result(&due(kind, challenge))
                .expect("delivery")
        );
        assert_eq!(command.messages.sent.len(), 1);
        assert_eq!(command.messages.sent[0].0, 999);
    }

    #[test]
    fn test_due_reminders_post_in_main_channel_not_origin_reminder_pending() {
        assert_due_posts_in_main(DuelDueKind::Reminder, DuelStatus::Pending);
    }

    #[test]
    fn test_due_reminders_post_in_main_channel_not_origin_unresolved_accepted() {
        assert_due_posts_in_main(DuelDueKind::Unresolved, DuelStatus::Accepted);
    }

    #[test]
    fn test_due_reminder_resolves_decorated_dota_mm_by_name() {
        let mut command = harness();
        command.messages.set_main_and_origin();
        command.messages.cached.get_mut(&999).expect("main").name = "⚔️dota-mm⚔️".to_owned();
        assert!(
            command
                .deliver_due_result(&due(DuelDueKind::Reminder, pending()))
                .expect("delivery")
        );
        assert_eq!(command.messages.sent[0].0, 999);
    }

    #[test]
    fn test_due_reminder_falls_back_to_origin_when_dota_mm_is_unavailable() {
        let mut command = harness();
        command
            .messages
            .cached
            .get_mut(&CHANNEL_ID)
            .expect("origin")
            .name = "gamba".to_owned();
        assert!(
            command
                .deliver_due_result(&due(DuelDueKind::Reminder, pending()))
                .expect("delivery")
        );
        assert_eq!(command.messages.sent[0].0, CHANNEL_ID);
    }

    fn assert_stale_claim_not_delivered(kind: DuelDueKind, stale_status: DuelStatus) {
        let mut command = harness();
        let claimed_status = if kind == DuelDueKind::Unresolved {
            DuelStatus::Accepted
        } else {
            DuelStatus::Pending
        };
        let claimed = challenge_with(claimed_status, Some(DuelTrial::TrialByCombat), Some(300));
        command.service.get_default = Some(challenge_with(stale_status, None, Some(300)));
        assert!(
            !command
                .deliver_due_result(&due(kind, claimed))
                .expect("stale")
        );
        assert!(command.flavor.calls.is_empty());
        assert!(command.messages.sent.is_empty());
    }

    #[test]
    fn test_stale_claimed_reminder_is_not_delivered_unresolved_resolved() {
        assert_stale_claim_not_delivered(DuelDueKind::Unresolved, DuelStatus::Resolved);
    }

    #[test]
    fn test_stale_claimed_reminder_is_not_delivered_reminder_accepted() {
        assert_stale_claim_not_delivered(DuelDueKind::Reminder, DuelStatus::Accepted);
    }

    #[test]
    fn test_missing_dota_mm_channel_uses_origin_for_reminder_claims() {
        let mut command = harness();
        command
            .messages
            .cached
            .get_mut(&CHANNEL_ID)
            .expect("origin")
            .name = "gamba".to_owned();
        command
            .process_due_challenge(7, GUILD_ID, 1_700_432_000)
            .expect("due");
        assert_eq!(
            command.service.due_calls,
            [(7, GUILD_ID, 1_700_432_000, true)]
        );
    }

    #[test]
    fn test_duplicate_dota_mm_channels_use_origin_for_reminder_claims() {
        let mut command = harness();
        let mut duplicate = FakeMessages::channel(999, MAIN_CHANNEL_NAME);
        duplicate.can_send = false;
        command.messages.cached.insert(999, duplicate);
        command
            .messages
            .guilds
            .get_mut(&GUILD_ID)
            .expect("guild")
            .text_channel_ids
            .push(999);
        command
            .process_due_challenge(7, GUILD_ID, 1_700_432_000)
            .expect("due");
        assert!(command.service.due_calls[0].3);
    }

    #[test]
    fn test_dota_mm_channel_allows_reminder_claims_without_configuration() {
        let mut command = harness();
        command
            .process_due_challenge(7, GUILD_ID, 1_700_432_000)
            .expect("due");
        assert!(command.service.due_calls[0].3);
    }

    #[test]
    fn test_main_channel_from_different_guild_uses_origin_for_reminder_claims() {
        let mut command = harness();
        command
            .messages
            .cached
            .get_mut(&CHANNEL_ID)
            .expect("origin")
            .name = "gamba".to_owned();
        let mut foreign = FakeMessages::channel(999, MAIN_CHANNEL_NAME);
        foreign.guild_id = Some(GUILD_ID + 1);
        command.messages.cached.insert(999, foreign);
        command
            .messages
            .guilds
            .get_mut(&GUILD_ID)
            .expect("guild")
            .text_channel_ids = vec![999];
        command
            .process_due_challenge(7, GUILD_ID, 1_700_432_000)
            .expect("due");
        assert!(command.service.due_calls[0].3);
    }

    #[test]
    fn test_due_reminder_is_not_delivered_to_another_guild() {
        let mut command = harness();
        command
            .messages
            .cached
            .get_mut(&CHANNEL_ID)
            .expect("origin")
            .name = "gamba".to_owned();
        let mut foreign = FakeMessages::channel(999, MAIN_CHANNEL_NAME);
        foreign.guild_id = Some(GUILD_ID + 1);
        command.messages.cached.insert(999, foreign);
        command
            .messages
            .guilds
            .get_mut(&GUILD_ID)
            .expect("guild")
            .text_channel_ids = vec![999];
        assert!(
            command
                .deliver_due_result(&due(DuelDueKind::Reminder, pending()))
                .expect("delivery")
        );
        assert_eq!(command.messages.sent[0].0, CHANNEL_ID);
        assert!(!command.messages.sent.iter().any(|(id, _)| *id == 999));
    }

    fn assert_invalid_main_uses_origin(kind: ChannelKind, guild_id: Option<i64>, can_send: bool) {
        let mut command = harness();
        command
            .messages
            .cached
            .get_mut(&CHANNEL_ID)
            .expect("origin")
            .name = "gamba".to_owned();
        let mut invalid = FakeMessages::channel(999, MAIN_CHANNEL_NAME);
        invalid.kind = kind;
        invalid.guild_id = guild_id;
        invalid.can_send = can_send;
        command.messages.cached.insert(999, invalid);
        command
            .messages
            .guilds
            .get_mut(&GUILD_ID)
            .expect("guild")
            .text_channel_ids = vec![999];
        command
            .process_due_challenge(7, GUILD_ID, 1_700_432_000)
            .expect("due");
        assert!(command.service.due_calls[0].3);
    }

    #[test]
    fn test_invalid_main_channel_uses_origin_for_reminder_claims_guildless() {
        assert_invalid_main_uses_origin(ChannelKind::Text, None, true);
    }

    #[test]
    fn test_invalid_main_channel_uses_origin_for_reminder_claims_non_text() {
        assert_invalid_main_uses_origin(ChannelKind::Other, Some(GUILD_ID), true);
    }

    #[test]
    fn test_invalid_main_channel_uses_origin_for_reminder_claims_no_send() {
        assert_invalid_main_uses_origin(ChannelKind::Text, Some(GUILD_ID), false);
    }

    #[test]
    fn test_due_reminder_uses_origin_without_main_send_permission() {
        let mut command = harness();
        command
            .messages
            .cached
            .get_mut(&CHANNEL_ID)
            .expect("origin")
            .name = "gamba".to_owned();
        let mut main = FakeMessages::channel(999, MAIN_CHANNEL_NAME);
        main.can_send = false;
        command.messages.cached.insert(999, main);
        command
            .messages
            .guilds
            .get_mut(&GUILD_ID)
            .expect("guild")
            .text_channel_ids = vec![999];
        assert!(
            command
                .deliver_due_result(&due(DuelDueKind::Reminder, pending()))
                .expect("delivery")
        );
        assert_eq!(command.messages.sent[0].0, CHANNEL_ID);
    }

    #[test]
    fn test_unsendable_origin_channel_defers_reminder_claims() {
        let mut command = harness();
        command
            .messages
            .cached
            .get_mut(&CHANNEL_ID)
            .expect("origin")
            .can_send = false;
        command
            .process_due_challenge(7, GUILD_ID, 1_700_432_000)
            .expect("due");
        assert!(!command.service.due_calls[0].3);
        assert!(command.flavor.calls.is_empty());
        assert!(command.messages.sent.is_empty());
    }

    #[test]
    fn test_deliver_skips_unsendable_origin_channel() {
        let mut command = harness();
        command
            .messages
            .cached
            .get_mut(&CHANNEL_ID)
            .expect("origin")
            .can_send = false;
        assert!(
            !command
                .deliver_due_result(&due(DuelDueKind::Reminder, pending()))
                .expect("delivery")
        );
        assert!(command.messages.sent.is_empty());
    }

    #[test]
    fn test_main_channel_prefers_exact_name_over_substring_matches() {
        let mut command = harness();
        command
            .messages
            .cached
            .insert(999, FakeMessages::channel(999, "dota-mm-clips"));
        command
            .messages
            .guilds
            .get_mut(&GUILD_ID)
            .expect("guild")
            .text_channel_ids = vec![999, CHANNEL_ID];
        assert_eq!(
            command.get_main_channel(GUILD_ID).map(|channel| channel.id),
            Some(CHANNEL_ID)
        );
    }

    #[test]
    fn test_main_channel_keeps_unique_substring_match() {
        let mut command = harness();
        command
            .messages
            .cached
            .get_mut(&CHANNEL_ID)
            .expect("channel")
            .name = "the-dota-mm-hub".to_owned();
        assert_eq!(
            command.get_main_channel(GUILD_ID).map(|channel| channel.id),
            Some(CHANNEL_ID)
        );
    }

    #[test]
    fn test_main_channel_prefers_configured_duel_channel() {
        let mut command = harness();
        let mut configured = FakeMessages::channel(4_242, "honor-duels");
        configured.kind = ChannelKind::Text;
        command.messages.cached.insert(4_242, configured);
        command.configured_channel_id = Some(4_242);
        assert_eq!(
            command.get_main_channel(GUILD_ID).map(|channel| channel.id),
            Some(4_242)
        );
    }

    #[test]
    fn test_main_channel_resolves_configured_thread() {
        let mut command = harness();
        let mut thread = FakeMessages::channel(4_242, "honor-duels");
        thread.kind = ChannelKind::Thread;
        command.messages.cached.insert(4_242, thread);
        command.configured_channel_id = Some(4_242);
        assert_eq!(
            command.get_main_channel(GUILD_ID).map(|channel| channel.id),
            Some(4_242)
        );
    }

    #[test]
    fn test_lifecycle_channels_fetch_configured_thread_evicted_from_cache() {
        let mut command = harness();
        let mut thread = FakeMessages::channel(4_242, "honor-duels");
        thread.kind = ChannelKind::Thread;
        command.messages.fetched.insert(4_242, thread);
        command.configured_channel_id = Some(4_242);
        let channels = command.get_lifecycle_channels(&pending());
        assert_eq!(channels.first().map(|channel| channel.id), Some(4_242));
        assert!(command.messages.fetch_calls.contains(&4_242));
    }

    #[test]
    fn test_main_channel_logs_debug_when_configured_is_in_another_guild() {
        let mut command = harness();
        let mut foreign = FakeMessages::channel(4_242, "honor-duels");
        foreign.guild_id = Some(GUILD_ID + 1);
        command.messages.cached.insert(4_242, foreign);
        command.configured_channel_id = Some(4_242);
        assert_eq!(
            command.get_main_channel(GUILD_ID).map(|channel| channel.id),
            Some(CHANNEL_ID)
        );
        assert!(
            command
                .logs
                .iter()
                .all(|entry| !matches!(entry.level, LogLevel::Warning | LogLevel::Error))
        );
        assert!(
            command
                .logs
                .iter()
                .any(|entry| entry.message.contains("belongs to another guild"))
        );
    }

    #[test]
    fn test_main_channel_scans_by_name_when_unconfigured() {
        let mut command = harness();
        assert_eq!(
            command.get_main_channel(GUILD_ID).map(|channel| channel.id),
            Some(CHANNEL_ID)
        );
        assert!(command.logs.is_empty());
    }

    fn assert_configured_unusable_falls_back(configured: Option<Channel>) {
        let mut command = harness();
        if let Some(configured) = configured {
            command.messages.cached.insert(4_242, configured);
        }
        command.configured_channel_id = Some(4_242);
        assert_eq!(
            command.get_main_channel(GUILD_ID).map(|channel| channel.id),
            Some(CHANNEL_ID)
        );
    }

    #[test]
    fn test_main_channel_falls_back_to_name_scan_when_configured_is_unusable_missing() {
        assert_configured_unusable_falls_back(None);
    }

    #[test]
    fn test_main_channel_falls_back_to_name_scan_when_configured_is_unusable_unsendable() {
        let mut configured = FakeMessages::channel(4_242, "honor-duels");
        configured.can_send = false;
        assert_configured_unusable_falls_back(Some(configured));
    }

    #[test]
    fn test_earlier_daily_reminder_suppresses_recipient_ping() {
        let mut command = harness();
        let mut result = due(DuelDueKind::Reminder, pending());
        result.remaining_seconds = 72 * 3_600;
        result.ping_recipient = false;
        assert!(command.deliver_due_result(&result).expect("delivery"));
        let (_, payload) = command.messages.sent.last().expect("send");
        assert_mentions_disabled(&payload.allowed_mentions);
        assert!(!content(payload).starts_with("<@2>"));
    }

    #[test]
    fn test_due_expiry_fetches_origin_channel_after_cache_miss() {
        let mut command = harness();
        let origin = command.messages.cached.remove(&CHANNEL_ID).expect("origin");
        command.messages.fetched.insert(CHANNEL_ID, origin);
        let expired = challenge_with(DuelStatus::Expired, None, Some(300));
        assert!(
            command
                .deliver_due_result(&due(DuelDueKind::Expired, expired))
                .expect("delivery")
        );
        assert_eq!(command.messages.fetch_calls, [CHANNEL_ID, CHANNEL_ID]);
        assert_eq!(command.messages.sent[0].0, CHANNEL_ID);
    }

    #[test]
    fn test_due_expiry_channel_fetch_failure_logs_challenge_guild_and_channel() {
        let mut command = harness();
        command.messages.cached.remove(&CHANNEL_ID);
        command
            .messages
            .guilds
            .get_mut(&GUILD_ID)
            .expect("guild")
            .text_channel_ids
            .clear();
        command.messages.fetch_failures.insert(CHANNEL_ID);
        let expired = challenge_with(DuelStatus::Expired, None, Some(300));
        assert!(
            !command
                .deliver_due_result(&due(DuelDueKind::Expired, expired))
                .expect("delivery")
        );
        let logs = command
            .logs
            .iter()
            .map(|entry| entry.message.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(logs.contains("challenge=7"));
        assert!(logs.contains("guild=100"));
        assert!(logs.contains("channel=200"));
    }

    #[test]
    fn test_due_channel_send_failure_logs_challenge_guild_and_channel() {
        let mut command = harness();
        command.messages.fail_send(CHANNEL_ID);
        assert!(
            !command
                .deliver_due_result(&due(DuelDueKind::Reminder, pending()))
                .expect("delivery")
        );
        let error = command
            .logs
            .iter()
            .find(|entry| entry.level == LogLevel::Error)
            .expect("error log");
        assert!(error.message.contains("challenge=7"));
        assert!(error.message.contains("guild=100"));
        assert!(error.message.contains("channel=200"));
    }

    #[test]
    fn test_due_reminder_falls_back_to_origin_after_main_send_failure() {
        let mut command = harness();
        command.messages.set_main_and_origin();
        command.messages.fail_send(999);
        assert!(
            command
                .deliver_due_result(&due(DuelDueKind::Reminder, pending()))
                .expect("delivery")
        );
        assert_eq!(
            command
                .messages
                .sent
                .iter()
                .map(|(channel_id, _)| *channel_id)
                .collect::<Vec<_>>(),
            [999, CHANNEL_ID]
        );
    }

    #[test]
    fn test_due_reminder_stops_fallback_when_status_changes_after_send_failure() {
        let mut command = harness();
        command.messages.set_main_and_origin();
        command.messages.fail_send(999);
        command.service.get_results.extend([
            Ok(Some(pending())),
            Ok(Some(pending())),
            Ok(Some(challenge_with(
                DuelStatus::Accepted,
                Some(DuelTrial::TrialByCombat),
                Some(300),
            ))),
        ]);
        assert!(
            !command
                .deliver_due_result(&due(DuelDueKind::Reminder, pending()))
                .expect("delivery")
        );
        assert_eq!(command.messages.sent.len(), 1);
        assert_eq!(command.messages.sent[0].0, 999);
    }

    #[test]
    fn test_process_due_challenge_claims_off_loop_and_delivers() {
        let mut command = harness();
        command
            .service
            .due_results
            .push_back(Ok(Some(due(DuelDueKind::Reminder, pending()))));
        command
            .process_due_challenge(7, GUILD_ID, 1_700_432_000)
            .expect("due");
        assert_eq!(
            command.service.due_calls,
            [(7, GUILD_ID, 1_700_432_000, true)]
        );
        assert_eq!(command.messages.sent.len(), 1);
        assert!(command.scheduler.operations.contains(&"duel.process_due"));
    }

    #[test]
    fn test_process_due_challenge_rearms_reminder_after_send_failure() {
        let mut command = harness();
        command.messages.fail_send(CHANNEL_ID);
        let mut result = due(DuelDueKind::Reminder, pending());
        result.claimed_reminder_at = Some(1_700_432_000);
        command.service.due_results.push_back(Ok(Some(result)));
        command
            .process_due_challenge(7, GUILD_ID, 1_700_432_000)
            .expect("due");
        assert_eq!(command.service.rearm_calls, [7]);
    }

    #[test]
    fn test_process_due_challenge_does_not_rearm_after_origin_fallback() {
        let mut command = harness();
        command.messages.set_main_and_origin();
        command.messages.fail_send(999);
        let mut result = due(DuelDueKind::Reminder, pending());
        result.claimed_reminder_at = Some(1_700_432_000);
        command.service.due_results.push_back(Ok(Some(result)));
        command
            .process_due_challenge(7, GUILD_ID, 1_700_432_000)
            .expect("due");
        assert_eq!(
            command.messages.sent.last().expect("fallback").0,
            CHANNEL_ID
        );
        assert!(command.service.rearm_calls.is_empty());
    }

    #[test]
    fn test_undelivered_expiry_settlement_logs_amounts() {
        let mut command = harness();
        command.messages.cached.remove(&CHANNEL_ID);
        command
            .messages
            .guilds
            .get_mut(&GUILD_ID)
            .expect("guild")
            .text_channel_ids
            .clear();
        command.messages.fetch_failures.insert(CHANNEL_ID);
        let expired = challenge_with(DuelStatus::Expired, None, Some(300));
        command
            .service
            .due_results
            .push_back(Ok(Some(due(DuelDueKind::Expired, expired))));
        command
            .process_due_challenge(7, GUILD_ID, 1_700_432_000)
            .expect("due");
        assert!(command.service.rearm_calls.is_empty());
        let errors = command
            .logs
            .iter()
            .filter(|entry| entry.level == LogLevel::Error)
            .map(|entry| entry.message.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(errors.contains("challenge=7"));
        assert!(errors.contains("guild=100"));
        assert!(errors.contains("challenger=1"));
        assert!(errors.contains("recipient=2"));
        assert!(errors.contains("500 JC escrow"));
        assert!(errors.contains("250 JC penalty"));
    }

    #[test]
    fn test_delivered_expiry_announcement_is_confirmed() {
        let mut command = harness();
        let expired = challenge_with(DuelStatus::Expired, None, Some(300));
        command
            .service
            .due_results
            .push_back(Ok(Some(due(DuelDueKind::Expired, expired))));
        command
            .process_due_challenge(7, GUILD_ID, 1_700_604_800)
            .expect("due");
        assert_eq!(command.service.confirm_calls, [7]);
        assert!(command.service.rearm_calls.is_empty());
    }

    #[test]
    fn test_failed_expiry_announcement_is_not_confirmed() {
        let mut command = harness();
        command.messages.fail_send(CHANNEL_ID);
        let expired = challenge_with(DuelStatus::Expired, None, Some(300));
        command
            .service
            .due_results
            .push_back(Ok(Some(due(DuelDueKind::Expired, expired))));
        command
            .process_due_challenge(7, GUILD_ID, 1_700_604_800)
            .expect("due");
        assert!(command.service.confirm_calls.is_empty());
        assert!(command.service.rearm_calls.is_empty());
    }

    #[test]
    fn test_process_due_challenge_ignores_lost_claim() {
        let mut command = harness();
        command
            .process_due_challenge(7, GUILD_ID, 1_700_432_000)
            .expect("due");
        assert!(command.messages.sent.is_empty());
        assert!(command.flavor.calls.is_empty());
        assert!(command.service.rearm_calls.is_empty());
        assert!(command.service.confirm_calls.is_empty());
    }

    #[test]
    fn test_due_expiry_edits_original_and_posts_without_mentions() {
        let mut command = harness();
        let expired = challenge_with(DuelStatus::Expired, None, Some(300));
        assert!(
            command
                .deliver_due_result(&due(DuelDueKind::Expired, expired))
                .expect("delivery")
        );
        assert_eq!(command.flavor.calls[0].0, FlavorEvent::Expired);
        let edit = &command.messages.edits[0].2;
        assert_eq!(
            edit.embed.as_ref().expect("embed").field_value("Status"),
            Some("Expired")
        );
        assert!(edit.view.is_none());
        let (_, sent) = command.messages.sent.last().expect("send");
        assert!(content(sent).contains("declined in cowardice by silence"));
        assert_mentions_disabled(&sent.allowed_mentions);
    }

    #[test]
    fn test_due_expiry_announcement_prefers_main_channel() {
        let mut command = harness();
        command.messages.set_main_and_origin();
        let expired = challenge_with(DuelStatus::Expired, None, Some(300));
        assert!(
            command
                .deliver_due_result(&due(DuelDueKind::Expired, expired))
                .expect("delivery")
        );
        assert_eq!(command.messages.sent[0].0, 999);
        assert_eq!(command.messages.edits[0].0, CHANNEL_ID);
    }

    #[test]
    fn test_due_expiry_does_not_edit_origin_channel_from_another_guild() {
        let mut command = harness();
        command.messages.set_main_and_origin();
        command
            .messages
            .cached
            .get_mut(&CHANNEL_ID)
            .expect("origin")
            .guild_id = Some(GUILD_ID + 1);
        let expired = challenge_with(DuelStatus::Expired, None, Some(300));
        assert!(
            command
                .deliver_due_result(&due(DuelDueKind::Expired, expired))
                .expect("delivery")
        );
        assert_eq!(command.messages.sent[0].0, 999);
        assert!(command.messages.edits.is_empty());
    }

    #[test]
    fn test_due_expiry_announcement_falls_back_to_origin_channel() {
        let mut command = harness();
        command
            .messages
            .cached
            .get_mut(&CHANNEL_ID)
            .expect("origin")
            .name = "gamba".to_owned();
        let expired = challenge_with(DuelStatus::Expired, None, Some(300));
        assert!(
            command
                .deliver_due_result(&due(DuelDueKind::Expired, expired))
                .expect("delivery")
        );
        assert_eq!(command.messages.sent[0].0, CHANNEL_ID);
        assert_eq!(command.messages.edits[0].1, 300);
    }

    #[test]
    fn test_due_expiry_announcement_falls_back_after_main_send_failure() {
        let mut command = harness();
        command.messages.set_main_and_origin();
        command.messages.fail_send(999);
        let expired = challenge_with(DuelStatus::Expired, None, Some(300));
        assert!(
            command
                .deliver_due_result(&due(DuelDueKind::Expired, expired))
                .expect("delivery")
        );
        assert_eq!(
            command
                .messages
                .sent
                .iter()
                .map(|(channel_id, _)| *channel_id)
                .collect::<Vec<_>>(),
            [999, CHANNEL_ID]
        );
    }

    fn assert_embed_case(
        challenge: &DuelChallenge,
        expected_status: &str,
        extra_field: &str,
        extra_value: &str,
    ) {
        let embed = Harness::build_challenge_embed(challenge, "The herald speaks.");
        assert_eq!(embed.field_value("Challenge"), Some("#7"));
        assert_eq!(embed.field_value("Challenger"), Some("<@1>"));
        assert_eq!(embed.field_value("Recipient"), Some("<@2>"));
        assert_eq!(embed.field_value("Wager"), Some("500 JC"));
        assert_eq!(
            embed.field_value("Issuance Fee"),
            Some("50 JC — nonrefundable after delivery")
        );
        assert_eq!(embed.field_value("Status"), Some(expected_status));
        assert_eq!(embed.field_value(extra_field), Some(extra_value));
    }

    #[test]
    fn test_challenge_embed_has_lifecycle_fields_pending() {
        assert_embed_case(
            &pending(),
            "Pending",
            "Response Deadline",
            "<t:1700604800:R>",
        );
    }

    #[test]
    fn test_challenge_embed_has_lifecycle_fields_accepted() {
        assert_embed_case(
            &challenge_with(
                DuelStatus::Accepted,
                Some(DuelTrial::TrialByCombat),
                Some(300),
            ),
            "Accepted",
            "Trial",
            "Trial by Combat",
        );
    }

    #[test]
    fn test_challenge_embed_has_lifecycle_fields_declined() {
        assert_embed_case(
            &challenge_with(DuelStatus::Declined, None, Some(300)),
            "Declined",
            "Decline Penalty",
            "250 JC",
        );
    }

    #[test]
    fn test_challenge_embed_has_lifecycle_fields_expired() {
        assert_embed_case(
            &challenge_with(DuelStatus::Expired, None, Some(300)),
            "Expired",
            "Decline Penalty",
            "250 JC",
        );
    }

    #[test]
    fn test_challenge_embed_has_lifecycle_fields_resolved() {
        let mut resolved = challenge_with(
            DuelStatus::Resolved,
            Some(DuelTrial::TrialOfFive),
            Some(300),
        );
        resolved.winner_id = Some(CHALLENGER_ID);
        assert_embed_case(&resolved, "Resolved", "Winner", "<@1>");
    }

    #[test]
    fn test_challenge_embed_has_lifecycle_fields_voided() {
        assert_embed_case(
            &challenge_with(DuelStatus::Voided, Some(DuelTrial::TrialOfFive), Some(300)),
            "Voided",
            "Refund",
            "500 JC stake to each player; issuance fee remains nonrefundable",
        );
    }

    #[test]
    fn test_void_copy_says_only_stakes_are_refunded() {
        let mut command = harness();
        command.members.admins.insert((GUILD_ID, RECIPIENT_ID));
        command.service.resolve_results.push_back(Ok(challenge_with(
            DuelStatus::Voided,
            Some(DuelTrial::TrialByCombat),
            Some(300),
        )));
        let mut io = FakeInteraction::available();
        command.resolve(
            &interaction(),
            &mut io,
            7,
            "void",
            &ProfitDeductionPolicy::none(),
        );
        let detail = content(io.followups.last().expect("announcement")).to_lowercase();
        assert!(detail.contains("stakes"));
        assert!(detail.contains("issuance fee was not refunded"));
    }

    #[test]
    fn test_resolve_copy_shows_net_pot_and_names_withholdings() {
        let mut command = harness();
        command.members.admins.insert((GUILD_ID, RECIPIENT_ID));
        let mut resolved = challenge_with(
            DuelStatus::Resolved,
            Some(DuelTrial::TrialByCombat),
            Some(300),
        );
        resolved.wager = 600;
        resolved.winner_id = Some(RECIPIENT_ID);
        command.service.resolve_results.push_back(Ok(resolved));
        command.service.resolve_deductions = ProfitDeductions {
            profit: 600,
            bankruptcy_penalty: 50,
            vanity_tax: 60,
            low_priority_tax: 0,
        };
        let mut io = FakeInteraction::available();
        command.resolve(
            &interaction(),
            &mut io,
            7,
            "recipient_victory",
            &ProfitDeductionPolicy::none(),
        );
        let detail = content(io.followups.last().expect("announcement"));
        assert!(
            detail.ends_with(&format!(
                "<@{RECIPIENT_ID}> wins 1090 JC (\u{2212}50 JC bankruptcy penalty, \u{2212}60 JC vanity tax)."
            )),
            "{detail}"
        );

        let mut command = harness();
        command.members.admins.insert((GUILD_ID, RECIPIENT_ID));
        let mut resolved = challenge_with(
            DuelStatus::Resolved,
            Some(DuelTrial::TrialByCombat),
            Some(300),
        );
        resolved.wager = 600;
        resolved.winner_id = Some(RECIPIENT_ID);
        command.service.resolve_results.push_back(Ok(resolved));
        let mut io = FakeInteraction::available();
        command.resolve(
            &interaction(),
            &mut io,
            7,
            "recipient_victory",
            &ProfitDeductionPolicy::none(),
        );
        let detail = content(io.followups.last().expect("announcement"));
        assert!(
            detail.ends_with(&format!("<@{RECIPIENT_ID}> wins 1200 JC.")),
            "{detail}"
        );
    }

    #[test]
    fn test_list_splits_more_than_twenty_five_outstanding_challenges() {
        let mut command = harness();
        command.service.list_results.clear();
        let challenges = (1..28)
            .map(|challenge_id| {
                let mut challenge = pending();
                challenge.challenge_id = challenge_id;
                challenge
            })
            .collect();
        command.service.list_results.push_back(Ok(challenges));
        let mut io = FakeInteraction::available();
        command.list(&interaction(), &mut io);
        assert_eq!(io.followups.len(), 2);
        assert_eq!(
            io.followups
                .iter()
                .map(|payload| payload.embed.as_ref().expect("embed").fields.len())
                .collect::<Vec<_>>(),
            [25, 2]
        );
        assert_eq!(
            io.followups
                .iter()
                .flat_map(|payload| payload.embed.as_ref().expect("embed").fields.iter())
                .filter(|field| field.name.starts_with('#'))
                .count(),
            27
        );
        for payload in &io.followups {
            assert_mentions_disabled(&payload.allowed_mentions);
        }
    }

    #[test]
    fn test_list_has_clear_empty_state() {
        let mut command = harness();
        command.service.list_results.clear();
        command.service.list_results.push_back(Ok(Vec::new()));
        let mut io = FakeInteraction::available();
        command.list(&interaction(), &mut io);
        assert!(content(&io.followups[0]).contains("No pending or accepted duel challenges"));
        assert_eq!(io.followups[0].visibility, Visibility::Ephemeral);
    }

    #[test]
    fn test_resolve_denies_non_admin() {
        let mut command = harness();
        let mut io = FakeInteraction::available();
        command.resolve(
            &interaction(),
            &mut io,
            7,
            "void",
            &ProfitDeductionPolicy::none(),
        );
        assert!(command.service.resolve_calls.is_empty());
        assert_eq!(io.immediate.len(), 1);
        assert_eq!(io.immediate[0].visibility, Visibility::Ephemeral);
    }

    fn assert_admin_resolution(outcome: &str, status: DuelStatus, winner_id: Option<i64>) {
        let mut command = harness();
        command.members.admins.insert((GUILD_ID, RECIPIENT_ID));
        let mut resolved = challenge_with(status, Some(DuelTrial::TrialByCombat), Some(300));
        resolved.winner_id = winner_id;
        command.service.resolve_results.push_back(Ok(resolved));
        let mut io = FakeInteraction::available();
        command.resolve(
            &interaction(),
            &mut io,
            7,
            outcome,
            &ProfitDeductionPolicy::none(),
        );
        assert_eq!(
            command.service.resolve_calls,
            [(GUILD_ID, RECIPIENT_ID, 7, outcome.to_owned())]
        );
        let edit = &command.messages.edits[0].2;
        assert!(edit.view.is_none());
        assert_eq!(
            edit.embed.as_ref().expect("embed").field_value("Status"),
            Some(status_label(status))
        );
    }

    #[test]
    fn test_admin_resolve_supports_two_winners_and_void_challenger_victory() {
        assert_admin_resolution(
            "challenger_victory",
            DuelStatus::Resolved,
            Some(CHALLENGER_ID),
        );
    }

    #[test]
    fn test_admin_resolve_supports_two_winners_and_void_recipient_victory() {
        assert_admin_resolution(
            "recipient_victory",
            DuelStatus::Resolved,
            Some(RECIPIENT_ID),
        );
    }

    #[test]
    fn test_admin_resolve_supports_two_winners_and_void_void() {
        assert_admin_resolution("void", DuelStatus::Voided, None);
    }

    #[test]
    fn test_setup_restores_one_view_for_every_pending_challenge() {
        let mut command = harness();
        command
            .service
            .pending_results
            .push_back(Ok(vec![pending()]));
        let report = command.recover_pending().expect("recovery");
        assert_eq!(command.service.pending_calls, 1);
        assert_eq!(report.registered_views, 1);
        assert_eq!(command.messages.registered, [(300, ChallengeView::new(7))]);
        assert_eq!(command.messages.edits.len(), 1);
        let edit = &command.messages.edits[0];
        assert_eq!((edit.0, edit.1), (CHANNEL_ID, 300));
        assert_eq!(edit.2.view, Some(ChallengeView::new(7)));
        assert_mentions_disabled(&edit.2.allowed_mentions);
    }

    #[test]
    fn test_setup_caps_bound_recovery_and_registers_all_views_first() {
        let mut command = harness();
        let challenges = (0..12)
            .map(|index| {
                let mut challenge = pending();
                challenge.challenge_id = index;
                challenge.message_id = Some(300 + index);
                challenge
            })
            .collect();
        command.service.pending_results.push_back(Ok(challenges));
        let report = command.recover_pending().expect("recovery");
        assert_eq!(report.registered_views, 12);
        assert_eq!(report.attempted_bound_edits, 12);
        assert_eq!(report.peak_bound_edits, RECOVERY_EDIT_CONCURRENCY);
        assert_eq!(command.messages.bounded_limits, [RECOVERY_EDIT_CONCURRENCY]);
        assert_eq!(command.messages.registered.len(), 12);
        assert_eq!(command.messages.edits.len(), 12);
        assert!(
            command.messages.events[..12]
                .iter()
                .all(|event| event.starts_with("register:"))
        );
        assert!(
            command.messages.events[12..]
                .iter()
                .all(|event| event.starts_with("edit:"))
        );
    }

    #[test]
    fn test_setup_deduplicates_cold_channel_resolution() {
        let mut command = harness();
        let challenges = (0..3)
            .map(|index| {
                let mut challenge = pending();
                challenge.challenge_id = index;
                challenge.message_id = Some(300 + index);
                challenge
            })
            .collect();
        command.service.pending_results.push_back(Ok(challenges));
        let origin = command.messages.cached.remove(&CHANNEL_ID).expect("origin");
        command.messages.fetched.insert(CHANNEL_ID, origin);
        let report = command.recover_pending().expect("recovery");
        assert_eq!(report.distinct_channel_resolutions, 1);
        assert_eq!(command.messages.fetch_calls, [CHANNEL_ID]);
        assert_eq!(command.messages.edits.len(), 3);
    }

    #[test]
    fn test_setup_isolates_bound_control_recovery_failures() {
        let mut command = harness();
        let challenges = (0..3)
            .map(|index| {
                let mut challenge = pending();
                challenge.challenge_id = index;
                challenge.message_id = Some(300 + index);
                challenge
            })
            .collect();
        command.service.pending_results.push_back(Ok(challenges));
        command.messages.edit_failures.insert(301);
        let report = command.recover_pending().expect("recovery");
        assert_eq!(report.attempted_bound_edits, 3);
        assert_eq!(command.messages.edits.len(), 3);
        assert_eq!(command.messages.registered.len(), 3);
        assert!(
            command
                .logs
                .iter()
                .any(|entry| entry.message.contains("challenge 1"))
        );
    }

    fn unbound_challenge() -> DuelChallenge {
        let mut challenge = challenge_with(DuelStatus::Pending, None, None);
        challenge.challenge_id = 8;
        challenge
    }

    #[test]
    fn test_setup_posts_and_binds_replacement_for_unbound_challenge() {
        let mut command = harness();
        command.messages.next_message_id = 301;
        command
            .service
            .pending_results
            .push_back(Ok(vec![unbound_challenge()]));
        let report = command.recover_pending().expect("recovery");
        assert_eq!(report.restored_unbound, 1);
        assert_eq!(command.flavor.calls[0].0, FlavorEvent::Issued);
        let (channel_id, sent) = &command.messages.sent[0];
        assert_eq!(*channel_id, CHANNEL_ID);
        assert_eq!(content(sent), "<@2>");
        assert!(sent.view.is_none());
        assert_eq!(sent.allowed_mentions.users, [RECIPIENT_ID]);
        assert_eq!(command.service.bind_calls, [(8, GUILD_ID, 301)]);
        assert!(command.service.mark_calls.is_empty());
        assert_eq!(command.messages.registered, [(301, ChallengeView::new(8))]);
        assert_eq!(command.messages.edits.len(), 1);
        assert_eq!(
            command.messages.edits[0].2.view,
            Some(ChallengeView::new(8))
        );
    }

    #[test]
    fn test_setup_refunds_when_replacement_delivery_fails() {
        let mut command = harness();
        command
            .service
            .pending_results
            .push_back(Ok(vec![unbound_challenge()]));
        command.messages.fail_send(CHANNEL_ID);
        let report = command.recover_pending().expect("recovery");
        assert_eq!(report.restored_unbound, 0);
        assert_eq!(command.service.mark_calls, [(8, GUILD_ID, CHALLENGER_ID)]);
        assert!(command.service.bind_calls.is_empty());
        assert!(command.messages.registered.is_empty());
    }

    #[test]
    fn test_setup_discards_replacement_when_another_process_binds_first() {
        let mut command = harness();
        command.messages.next_message_id = 301;
        command
            .service
            .pending_results
            .push_back(Ok(vec![unbound_challenge()]));
        command
            .service
            .bind_results
            .push_back(Err(CommandError::Invalid("already bound".to_owned())));
        command.recover_pending().expect("recovery");
        assert_eq!(command.messages.deletes, [(CHANNEL_ID, 301)]);
        assert!(command.service.mark_calls.is_empty());
        assert!(command.messages.registered.is_empty());
    }

    #[test]
    fn test_setup_bind_error_deletes_replacement_and_refunds_unbound_challenge() {
        let mut command = harness();
        command.messages.next_message_id = 301;
        command
            .service
            .pending_results
            .push_back(Ok(vec![unbound_challenge()]));
        command
            .service
            .bind_results
            .push_back(Err(CommandError::Internal(
                "database unavailable".to_owned(),
            )));
        command.recover_pending().expect("recovery");
        assert_eq!(command.messages.deletes, [(CHANNEL_ID, 301)]);
        assert_eq!(command.service.mark_calls, [(8, GUILD_ID, CHALLENGER_ID)]);
        assert!(command.messages.registered.is_empty());
    }

    #[test]
    fn test_setup_requires_both_duel_services() {
        assert_eq!(
            validate_setup_dependencies(false, true),
            Err("duel_service must be initialized before commands.duel")
        );
        assert_eq!(
            validate_setup_dependencies(true, false),
            Err("duel_flavor_service must be initialized before commands.duel")
        );
        assert_eq!(validate_setup_dependencies(true, true), Ok(()));
    }

    #[test]
    fn invariant_configured_rescue_success_is_cached_per_guild() {
        let mut command = harness();
        command.configured_channel_id = Some(4_242);
        let mut thread = FakeMessages::channel(4_242, "honor-duels");
        thread.kind = ChannelKind::Thread;
        command.messages.fetched.insert(4_242, thread);
        command.get_lifecycle_channels(&pending());
        command.monotonic_seconds = 299;
        command.get_lifecycle_channels(&pending());
        assert_eq!(
            command
                .messages
                .fetch_calls
                .iter()
                .filter(|channel_id| **channel_id == 4_242)
                .count(),
            1
        );
    }

    #[test]
    fn invariant_configured_rescue_failure_is_throttled_per_guild() {
        let mut command = harness();
        command.configured_channel_id = Some(4_242);
        command.messages.fetch_failures.insert(4_242);
        command.get_lifecycle_channels(&pending());
        command.monotonic_seconds = 599;
        command.get_lifecycle_channels(&pending());
        command.monotonic_seconds = 600;
        command.get_lifecycle_channels(&pending());
        assert_eq!(
            command
                .messages
                .fetch_calls
                .iter()
                .filter(|channel_id| **channel_id == 4_242)
                .count(),
            2
        );
    }

    #[test]
    fn invariant_malformed_button_ids_never_reach_the_service() {
        let mut command = harness();
        let mut io = FakeInteraction::available();
        for malformed in ["duel", "duel:not-an-id:decline", "other:7:decline"] {
            command.handle_button(&interaction(), &mut io, malformed);
        }
        assert!(command.service.get_calls.is_empty());
        assert!(command.service.respond_calls.is_empty());
        assert_eq!(io.defer_calls, 0);
    }

    const SERVICE_GUILD_ID: i64 = 123;
    const SERVICE_NOW: i64 = 1_000_000;
    const SERVICE_DAY: i64 = 86_400;

    fn service_challenge() -> DuelChallenge {
        let mut challenge = pending();
        challenge.guild_id = SERVICE_GUILD_ID;
        challenge.channel_id = 20;
        challenge
    }

    #[derive(Clone, Debug)]
    struct RecordingClock {
        value: f64,
        calls: usize,
    }

    impl RecordingClock {
        const fn new(value: f64) -> Self {
            Self { value, calls: 0 }
        }
    }

    impl DuelClockPort for RecordingClock {
        fn now(&mut self) -> f64 {
            self.calls += 1;
            self.value
        }
    }

    type CreateChallengeCall = (i64, i64, i64, i64, i64, i64, i64, i64, i64, i64);

    struct RecordingDuelRepository {
        pending: Result<Option<DuelChallenge>, CommandError>,
        challenge: Result<Option<DuelChallenge>, CommandError>,
        create_result: Result<DuelChallenge, CommandError>,
        accept_result: Result<DuelChallenge, CommandError>,
        decline_result: Result<DuelChallenge, CommandError>,
        resolve_result: Result<DuelChallenge, CommandError>,
        bind_result: Result<DuelChallenge, CommandError>,
        mark_result: Result<DuelChallenge, CommandError>,
        outstanding_result: Result<Vec<DuelChallenge>, CommandError>,
        pending_all_result: Result<Vec<DuelChallenge>, CommandError>,
        due_ids_result: Result<Vec<(i64, i64)>, CommandError>,
        reminder_results: VecDeque<Result<Option<DuelDueResult>, CommandError>>,
        unresolved_results: VecDeque<Result<Option<DuelDueResult>, CommandError>>,
        announcement_results: VecDeque<Result<Option<DuelDueResult>, CommandError>>,
        expire_results: VecDeque<Result<DuelChallenge, CommandError>>,
        clear_result: Result<bool, CommandError>,
        rearm_result: Result<bool, CommandError>,
        operations: Vec<&'static str>,
        create_calls: Vec<CreateChallengeCall>,
        pending_calls: Vec<(i64, i64)>,
        accept_calls: Vec<(i64, i64, i64, DuelTrial, i64, i64)>,
        decline_calls: Vec<(i64, i64, i64, i64, i64)>,
        get_calls: Vec<(i64, i64)>,
        resolve_calls: Vec<(i64, i64, Option<i64>, i64, i64)>,
        bind_calls: Vec<(i64, i64, i64)>,
        mark_calls: Vec<(i64, i64, i64, i64)>,
        outstanding_calls: Vec<i64>,
        pending_all_calls: usize,
        due_ids_calls: Vec<i64>,
        reminder_calls: Vec<(i64, i64, i64)>,
        unresolved_calls: Vec<(i64, i64, i64)>,
        announcement_calls: Vec<(i64, i64, i64, i64)>,
        expire_calls: Vec<(i64, i64, i64)>,
        clear_calls: Vec<(i64, i64, i64)>,
        rearm_calls: Vec<(i64, i64, DuelStatus, i64, i64)>,
    }

    impl Default for RecordingDuelRepository {
        fn default() -> Self {
            let challenge = service_challenge();
            Self {
                pending: Ok(Some(challenge.clone())),
                challenge: Ok(Some(challenge.clone())),
                create_result: Ok(challenge.clone()),
                accept_result: Ok(challenge.clone()),
                decline_result: Ok(challenge.clone()),
                resolve_result: Ok(challenge.clone()),
                bind_result: Ok(challenge.clone()),
                mark_result: Ok(challenge.clone()),
                outstanding_result: Ok(vec![challenge.clone()]),
                pending_all_result: Ok(vec![challenge]),
                due_ids_result: Ok(vec![(7, SERVICE_GUILD_ID)]),
                reminder_results: VecDeque::new(),
                unresolved_results: VecDeque::new(),
                announcement_results: VecDeque::new(),
                expire_results: VecDeque::new(),
                clear_result: Ok(true),
                rearm_result: Ok(true),
                operations: Vec::new(),
                create_calls: Vec::new(),
                pending_calls: Vec::new(),
                accept_calls: Vec::new(),
                decline_calls: Vec::new(),
                get_calls: Vec::new(),
                resolve_calls: Vec::new(),
                bind_calls: Vec::new(),
                mark_calls: Vec::new(),
                outstanding_calls: Vec::new(),
                pending_all_calls: 0,
                due_ids_calls: Vec::new(),
                reminder_calls: Vec::new(),
                unresolved_calls: Vec::new(),
                announcement_calls: Vec::new(),
                expire_calls: Vec::new(),
                clear_calls: Vec::new(),
                rearm_calls: Vec::new(),
            }
        }
    }

    impl DuelRepositoryPort for RecordingDuelRepository {
        fn create_challenge_atomic(
            &mut self,
            guild_id: i64,
            channel_id: i64,
            challenger_id: i64,
            recipient_id: i64,
            wager: i64,
            now: i64,
            challenger_cooldown_seconds: i64,
            recipient_cooldown_seconds: i64,
            response_seconds: i64,
            actor_id: i64,
        ) -> Result<DuelChallenge, CommandError> {
            self.operations.push("create");
            self.create_calls.push((
                guild_id,
                channel_id,
                challenger_id,
                recipient_id,
                wager,
                now,
                challenger_cooldown_seconds,
                recipient_cooldown_seconds,
                response_seconds,
                actor_id,
            ));
            self.create_result.clone()
        }

        fn get_pending_for_recipient(
            &mut self,
            recipient_id: i64,
            guild_id: i64,
        ) -> Result<Option<DuelChallenge>, CommandError> {
            self.operations.push("get_pending");
            self.pending_calls.push((recipient_id, guild_id));
            self.pending.clone()
        }

        fn accept_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            recipient_id: i64,
            trial: DuelTrial,
            now: i64,
            actor_id: i64,
        ) -> Result<DuelChallenge, CommandError> {
            self.operations.push("accept");
            self.accept_calls
                .push((challenge_id, guild_id, recipient_id, trial, now, actor_id));
            self.accept_result.clone()
        }

        fn decline_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            recipient_id: i64,
            now: i64,
            actor_id: i64,
        ) -> Result<DuelChallenge, CommandError> {
            self.operations.push("decline");
            self.decline_calls
                .push((challenge_id, guild_id, recipient_id, now, actor_id));
            self.decline_result.clone()
        }

        fn get_challenge(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
        ) -> Result<Option<DuelChallenge>, CommandError> {
            self.operations.push("get");
            self.get_calls.push((challenge_id, guild_id));
            self.challenge.clone()
        }

        fn resolve_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            winner_id: Option<i64>,
            now: i64,
            actor_id: i64,
            _policy: &ProfitDeductionPolicy<'_>,
        ) -> Result<DuelResolution, CommandError> {
            self.operations.push("resolve");
            self.resolve_calls
                .push((challenge_id, guild_id, winner_id, now, actor_id));
            self.resolve_result.clone().map(|challenge| DuelResolution {
                challenge,
                deductions: ProfitDeductions::default(),
            })
        }

        fn bind_message(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            message_id: i64,
        ) -> Result<DuelChallenge, CommandError> {
            self.operations.push("bind");
            self.bind_calls.push((challenge_id, guild_id, message_id));
            self.bind_result.clone()
        }

        fn mark_delivery_failed_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            now: i64,
            actor_id: i64,
        ) -> Result<DuelChallenge, CommandError> {
            self.operations.push("mark");
            self.mark_calls
                .push((challenge_id, guild_id, now, actor_id));
            self.mark_result.clone()
        }

        fn list_outstanding(&mut self, guild_id: i64) -> Result<Vec<DuelChallenge>, CommandError> {
            self.operations.push("list");
            self.outstanding_calls.push(guild_id);
            self.outstanding_result.clone()
        }

        fn list_pending_all(&mut self) -> Result<Vec<DuelChallenge>, CommandError> {
            self.operations.push("list_pending");
            self.pending_all_calls += 1;
            self.pending_all_result.clone()
        }

        fn get_due_challenge_ids(&mut self, now: i64) -> Result<Vec<(i64, i64)>, CommandError> {
            self.operations.push("due_ids");
            self.due_ids_calls.push(now);
            self.due_ids_result.clone()
        }

        fn claim_reminder_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            now: i64,
        ) -> Result<Option<DuelDueResult>, CommandError> {
            self.operations.push("claim_reminder");
            self.reminder_calls.push((challenge_id, guild_id, now));
            self.reminder_results.pop_front().unwrap_or(Ok(None))
        }

        fn claim_unresolved_reminder_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            now: i64,
        ) -> Result<Option<DuelDueResult>, CommandError> {
            self.operations.push("claim_unresolved");
            self.unresolved_calls.push((challenge_id, guild_id, now));
            self.unresolved_results.pop_front().unwrap_or(Ok(None))
        }

        fn claim_expired_announcement_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            now: i64,
            retry_at: i64,
        ) -> Result<Option<DuelDueResult>, CommandError> {
            self.operations.push("claim_announcement");
            self.announcement_calls
                .push((challenge_id, guild_id, now, retry_at));
            self.announcement_results.pop_front().unwrap_or(Ok(None))
        }

        fn expire_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            now: i64,
        ) -> Result<DuelChallenge, CommandError> {
            self.operations.push("expire");
            self.expire_calls.push((challenge_id, guild_id, now));
            self.expire_results
                .pop_front()
                .unwrap_or_else(|| Err(CommandError::Invalid("not due".to_owned())))
        }

        fn clear_expired_announcement_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            expected_next_reminder_at: i64,
        ) -> Result<bool, CommandError> {
            self.operations.push("clear");
            self.clear_calls
                .push((challenge_id, guild_id, expected_next_reminder_at));
            self.clear_result.clone()
        }

        fn rearm_reminder_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            expected_status: DuelStatus,
            expected_next_reminder_at: i64,
            retry_at: i64,
        ) -> Result<bool, CommandError> {
            self.operations.push("rearm");
            self.rearm_calls.push((
                challenge_id,
                guild_id,
                expected_status,
                expected_next_reminder_at,
                retry_at,
            ));
            self.rearm_result.clone()
        }
    }

    type RecordingDuelService =
        DuelApplicationService<RecordingDuelRepository, RecordingClock, NoopDuelEvolution>;

    fn recording_duel_service(clock: f64) -> RecordingDuelService {
        DuelApplicationService::new(
            RecordingDuelRepository::default(),
            RecordingClock::new(clock),
        )
    }

    fn service_due_result(kind: DuelDueKind, status: DuelStatus) -> DuelDueResult {
        let mut challenge = service_challenge();
        challenge.status = status;
        challenge.next_reminder_at = Some(SERVICE_NOW + 2 * SERVICE_DAY);
        DuelDueResult {
            kind,
            challenge,
            remaining_seconds: 0,
            ping_recipient: false,
            claimed_reminder_at: Some(SERVICE_NOW - SERVICE_DAY),
            is_redelivery: false,
        }
    }

    #[test]
    fn test_service_issue_routes_policy_and_integer_clock() {
        let mut service = recording_duel_service(1_000_000.75);
        service
            .issue(SERVICE_GUILD_ID, 20, 1, 2, 500, false)
            .expect("issue");
        assert_eq!(
            service.repository.create_calls,
            [(
                SERVICE_GUILD_ID,
                20,
                1,
                2,
                500,
                SERVICE_NOW,
                15 * SERVICE_DAY,
                RECIPIENT_COOLDOWN_SECONDS,
                RESPONSE_SECONDS,
                1,
            )]
        );
        assert_eq!(service.clock.calls, 1);
    }

    #[test]
    fn test_service_rejects_bot_before_repository_issue() {
        let mut service = recording_duel_service(1_000_000.0);
        assert_eq!(
            service.issue(SERVICE_GUILD_ID, 20, 1, 2, 500, true),
            Err(CommandError::Invalid(
                "Bots cannot answer a challenge of honor.".to_owned()
            ))
        );
        assert_eq!(service.clock.calls, 0);
        assert!(service.repository.create_calls.is_empty());
    }

    #[test]
    fn test_service_routes_response_choices() {
        let mut service = recording_duel_service(SERVICE_NOW as f64);
        service
            .respond(SERVICE_GUILD_ID, 2, "trial_by_combat")
            .expect("accept");
        assert_eq!(service.repository.pending_calls, [(2, SERVICE_GUILD_ID)]);
        assert_eq!(
            service.repository.accept_calls,
            [(
                7,
                SERVICE_GUILD_ID,
                2,
                DuelTrial::TrialByCombat,
                SERVICE_NOW,
                2,
            )]
        );
    }

    #[test]
    fn test_service_responds_to_the_clicked_challenge() {
        // A button carries the challenge it was rendered for. Resolving
        // "whatever is pending" would answer a newer challenge that arrived
        // between the render and the click.
        let mut service = recording_duel_service(SERVICE_NOW as f64);
        let mut clicked = service_challenge();
        clicked.challenge_id = 11;
        service.repository.challenge = Ok(Some(clicked));

        service
            .respond_to(SERVICE_GUILD_ID, 2, "trial_by_combat", Some(11))
            .expect("accept the clicked challenge");

        assert_eq!(service.repository.get_calls, [(11, SERVICE_GUILD_ID)]);
        assert!(
            service.repository.pending_calls.is_empty(),
            "the pending lookup must not decide which challenge is answered"
        );
        assert_eq!(service.repository.accept_calls[0].0, 11);
    }

    #[test]
    fn test_service_rejects_a_clicked_challenge_owned_by_someone_else() {
        let mut service = recording_duel_service(SERVICE_NOW as f64);
        let mut clicked = service_challenge();
        clicked.challenge_id = 11;
        clicked.recipient_id = 999;
        service.repository.challenge = Ok(Some(clicked));

        assert_eq!(
            service.respond_to(SERVICE_GUILD_ID, 2, "decline", Some(11)),
            Err(CommandError::Invalid(
                "You have no pending duel challenge.".to_owned()
            ))
        );
        assert!(service.repository.decline_calls.is_empty());
    }

    #[test]
    fn test_service_routes_decline_choice() {
        let mut service = recording_duel_service(SERVICE_NOW as f64);
        service
            .respond(SERVICE_GUILD_ID, 2, "decline")
            .expect("decline");
        assert_eq!(
            service.repository.decline_calls,
            [(7, SERVICE_GUILD_ID, 2, SERVICE_NOW, 2)]
        );
        assert!(service.repository.accept_calls.is_empty());
    }

    #[test]
    fn test_service_rejects_response_without_pending_challenge() {
        let mut service = recording_duel_service(SERVICE_NOW as f64);
        service.repository.pending = Ok(None);
        assert_eq!(
            service.respond(SERVICE_GUILD_ID, 2, "trial_of_five"),
            Err(CommandError::Invalid(
                "You have no pending duel challenge.".to_owned()
            ))
        );
        assert!(service.repository.accept_calls.is_empty());
        assert!(service.repository.decline_calls.is_empty());
        assert_eq!(service.clock.calls, 0);
    }

    fn assert_service_resolution(outcome: &str, winner_id: Option<i64>) {
        let mut service = recording_duel_service(SERVICE_NOW as f64);
        service
            .resolve(
                SERVICE_GUILD_ID,
                99,
                7,
                outcome,
                &ProfitDeductionPolicy::none(),
            )
            .expect("resolve");
        assert_eq!(
            service.repository.resolve_calls,
            [(7, SERVICE_GUILD_ID, winner_id, SERVICE_NOW, 99)]
        );
    }

    #[test]
    fn test_service_resolve_maps_victory_to_participant_challenger_victory_1() {
        assert_service_resolution("challenger_victory", Some(1));
    }

    #[test]
    fn test_service_resolve_maps_victory_to_participant_recipient_victory_2() {
        assert_service_resolution("recipient_victory", Some(2));
    }

    #[test]
    fn test_service_resolve_maps_void_to_none() {
        assert_service_resolution("void", None);
    }

    #[test]
    fn test_service_rejects_resolution_for_missing_challenge() {
        let mut service = recording_duel_service(SERVICE_NOW as f64);
        service.repository.challenge = Ok(None);
        assert_eq!(
            service.resolve(
                SERVICE_GUILD_ID,
                99,
                7,
                "void",
                &ProfitDeductionPolicy::none()
            ),
            Err(CommandError::Invalid("Challenge not found.".to_owned()))
        );
        assert!(service.repository.resolve_calls.is_empty());
        assert_eq!(service.clock.calls, 0);
    }

    #[test]
    fn test_service_bind_message_is_thin_wrapper() {
        let mut service = recording_duel_service(0.0);
        let expected = service_challenge();
        service.repository.bind_result = Ok(expected.clone());
        assert_eq!(service.bind_message(7, SERVICE_GUILD_ID, 44), Ok(expected));
        assert_eq!(service.repository.bind_calls, [(7, SERVICE_GUILD_ID, 44)]);
        assert_eq!(service.clock.calls, 0);
    }

    #[test]
    fn test_service_get_challenge_is_thin_wrapper() {
        let mut service = recording_duel_service(0.0);
        let expected = service_challenge();
        assert_eq!(
            service.get_challenge(7, SERVICE_GUILD_ID),
            Ok(Some(expected))
        );
        assert_eq!(service.repository.get_calls, [(7, SERVICE_GUILD_ID)]);
        assert_eq!(service.clock.calls, 0);
    }

    #[test]
    fn test_service_mark_delivery_failed_uses_integer_clock() {
        let mut service = recording_duel_service(1_000_000.75);
        let expected = service_challenge();
        assert_eq!(
            service.mark_delivery_failed(7, SERVICE_GUILD_ID, 1),
            Ok(expected)
        );
        assert_eq!(
            service.repository.mark_calls,
            [(7, SERVICE_GUILD_ID, SERVICE_NOW, 1)]
        );
    }

    #[test]
    fn test_service_list_outstanding_is_thin_wrapper() {
        let mut service = recording_duel_service(0.0);
        let expected = vec![service_challenge()];
        assert_eq!(service.list_outstanding(SERVICE_GUILD_ID), Ok(expected));
        assert_eq!(service.repository.outstanding_calls, [SERVICE_GUILD_ID]);
    }

    #[test]
    fn test_service_list_pending_all_is_thin_wrapper() {
        let mut service = recording_duel_service(0.0);
        let expected = vec![service_challenge()];
        assert_eq!(service.list_pending_all(), Ok(expected));
        assert_eq!(service.repository.pending_all_calls, 1);
    }

    #[test]
    fn test_service_get_due_challenge_ids_is_thin_wrapper() {
        let mut service = recording_duel_service(0.0);
        assert_eq!(
            service.get_due_challenge_ids(SERVICE_NOW),
            Ok(vec![(7, SERVICE_GUILD_ID)])
        );
        assert_eq!(service.repository.due_ids_calls, [SERVICE_NOW]);
    }

    #[test]
    fn test_service_process_due_returns_expiry_after_reminder_declines() {
        let mut service = recording_duel_service(0.0);
        let mut expired = service_challenge();
        expired.status = DuelStatus::Expired;
        service
            .repository
            .expire_results
            .push_back(Ok(expired.clone()));
        let result = service
            .process_due(7, SERVICE_GUILD_ID, SERVICE_NOW, true)
            .expect("process")
            .expect("expiry");
        assert_eq!(result.kind, DuelDueKind::Expired);
        assert_eq!(result.challenge, expired);
        assert_eq!(
            service.repository.operations,
            [
                "claim_reminder",
                "claim_unresolved",
                "claim_announcement",
                "expire",
            ]
        );
        assert_eq!(
            service.repository.announcement_calls,
            [(
                7,
                SERVICE_GUILD_ID,
                SERVICE_NOW,
                SERVICE_NOW + REMINDER_RETRY_BACKOFF_SECONDS,
            )]
        );
    }

    #[test]
    fn test_service_process_due_returns_claimed_reminder() {
        let mut service = recording_duel_service(0.0);
        let reminder = service_due_result(DuelDueKind::Reminder, DuelStatus::Pending);
        service
            .repository
            .reminder_results
            .push_back(Ok(Some(reminder.clone())));
        assert_eq!(
            service.process_due(7, SERVICE_GUILD_ID, SERVICE_NOW, true),
            Ok(Some(reminder))
        );
        assert_eq!(
            service.repository.reminder_calls,
            [(7, SERVICE_GUILD_ID, SERVICE_NOW)]
        );
        assert!(service.repository.unresolved_calls.is_empty());
        assert!(service.repository.expire_calls.is_empty());
    }

    #[test]
    fn test_service_process_due_returns_claimed_unresolved_reminder() {
        let mut service = recording_duel_service(0.0);
        let unresolved = service_due_result(DuelDueKind::Unresolved, DuelStatus::Accepted);
        service
            .repository
            .unresolved_results
            .push_back(Ok(Some(unresolved.clone())));
        assert_eq!(
            service.process_due(7, SERVICE_GUILD_ID, SERVICE_NOW, true),
            Ok(Some(unresolved))
        );
        assert_eq!(
            service.repository.unresolved_calls,
            [(7, SERVICE_GUILD_ID, SERVICE_NOW)]
        );
        assert!(service.repository.expire_calls.is_empty());
    }

    #[test]
    fn test_service_process_due_can_skip_reminder_claims() {
        let mut service = recording_duel_service(0.0);
        assert_eq!(
            service.process_due(7, SERVICE_GUILD_ID, SERVICE_NOW, false),
            Ok(None)
        );
        assert!(service.repository.reminder_calls.is_empty());
        assert!(service.repository.unresolved_calls.is_empty());
        assert_eq!(
            service.repository.expire_calls,
            [(7, SERVICE_GUILD_ID, SERVICE_NOW)]
        );
    }

    #[test]
    fn test_service_process_due_deferred_still_claims_expired_announcement() {
        let mut service = recording_duel_service(0.0);
        let announcement = service_due_result(DuelDueKind::Expired, DuelStatus::Expired);
        service
            .repository
            .announcement_results
            .push_back(Ok(Some(announcement.clone())));
        assert_eq!(
            service.process_due(7, SERVICE_GUILD_ID, SERVICE_NOW, false),
            Ok(Some(announcement))
        );
        assert!(service.repository.reminder_calls.is_empty());
        assert!(service.repository.unresolved_calls.is_empty());
        assert_eq!(
            service.repository.announcement_calls,
            [(
                7,
                SERVICE_GUILD_ID,
                SERVICE_NOW,
                SERVICE_NOW + REMINDER_RETRY_BACKOFF_SECONDS,
            )]
        );
        assert!(service.repository.expire_calls.is_empty());
    }

    fn assert_service_rearm(kind: DuelDueKind, status: DuelStatus) {
        let mut service = recording_duel_service(SERVICE_NOW as f64);
        let result = service_due_result(kind, status);
        assert_eq!(service.rearm_reminder(&result), Ok(true));
        assert_eq!(
            service.repository.rearm_calls,
            [(
                7,
                SERVICE_GUILD_ID,
                status,
                SERVICE_NOW + 2 * SERVICE_DAY,
                SERVICE_NOW + REMINDER_RETRY_BACKOFF_SECONDS,
            )]
        );
    }

    #[test]
    fn test_service_rearms_failed_reminder_claim_with_backoff_reminder_pending() {
        assert_service_rearm(DuelDueKind::Reminder, DuelStatus::Pending);
    }

    #[test]
    fn test_service_rearms_failed_reminder_claim_with_backoff_unresolved_accepted() {
        assert_service_rearm(DuelDueKind::Unresolved, DuelStatus::Accepted);
    }

    #[test]
    fn test_service_rearm_retry_never_delays_past_scheduled_reminder() {
        let mut service = recording_duel_service(SERVICE_NOW as f64);
        let mut result = service_due_result(DuelDueKind::Reminder, DuelStatus::Pending);
        result.challenge.next_reminder_at = Some(SERVICE_NOW + 60);
        assert_eq!(service.rearm_reminder(&result), Ok(true));
        assert_eq!(
            service.repository.rearm_calls,
            [(
                7,
                SERVICE_GUILD_ID,
                DuelStatus::Pending,
                SERVICE_NOW + 60,
                SERVICE_NOW + 60,
            )]
        );
    }

    #[test]
    fn test_service_process_due_returns_none_for_stale_challenge() {
        let mut service = recording_duel_service(0.0);
        assert_eq!(
            service.process_due(7, SERVICE_GUILD_ID, SERVICE_NOW, true),
            Ok(None)
        );
        assert_eq!(
            service.repository.operations,
            [
                "claim_reminder",
                "claim_unresolved",
                "claim_announcement",
                "expire",
            ]
        );
    }

    #[test]
    fn test_service_process_due_returns_claimed_expired_announcement() {
        let mut service = recording_duel_service(0.0);
        let announcement = service_due_result(DuelDueKind::Expired, DuelStatus::Expired);
        service
            .repository
            .announcement_results
            .push_back(Ok(Some(announcement.clone())));
        assert_eq!(
            service.process_due(7, SERVICE_GUILD_ID, SERVICE_NOW, true),
            Ok(Some(announcement))
        );
        assert_eq!(
            service.repository.announcement_calls,
            [(
                7,
                SERVICE_GUILD_ID,
                SERVICE_NOW,
                SERVICE_NOW + REMINDER_RETRY_BACKOFF_SECONDS,
            )]
        );
        assert!(service.repository.expire_calls.is_empty());
    }

    #[test]
    fn test_service_confirm_expiry_announced_clears_the_claimed_marker() {
        let mut service = recording_duel_service(SERVICE_NOW as f64);
        let mut result = service_due_result(DuelDueKind::Expired, DuelStatus::Expired);
        result.challenge.next_reminder_at = Some(SERVICE_NOW + REMINDER_RETRY_BACKOFF_SECONDS);
        assert_eq!(service.confirm_expiry_announced(&result), Ok(true));
        assert_eq!(
            service.repository.clear_calls,
            [(
                7,
                SERVICE_GUILD_ID,
                SERVICE_NOW + REMINDER_RETRY_BACKOFF_SECONDS,
            )]
        );
    }

    #[test]
    fn test_service_confirm_expiry_announced_ignores_non_expiry_results() {
        let mut service = recording_duel_service(SERVICE_NOW as f64);
        let reminder = service_due_result(DuelDueKind::Reminder, DuelStatus::Pending);
        let mut unmarked = service_due_result(DuelDueKind::Expired, DuelStatus::Expired);
        unmarked.challenge.next_reminder_at = None;
        assert_eq!(service.confirm_expiry_announced(&reminder), Ok(false));
        assert_eq!(service.confirm_expiry_announced(&unmarked), Ok(false));
        assert!(service.repository.clear_calls.is_empty());
        assert_eq!(service.clock.calls, 0);
    }

    struct StatefulDuelRepository {
        challenge: Option<DuelChallenge>,
        balances: HashMap<i64, i64>,
    }

    impl StatefulDuelRepository {
        fn new() -> Self {
            Self {
                challenge: None,
                balances: HashMap::from([(1, 550), (2, 500)]),
            }
        }

        fn stored(&self) -> &DuelChallenge {
            self.challenge.as_ref().expect("stored challenge")
        }

        fn update_challenge(&mut self, update: impl FnOnce(&mut DuelChallenge)) -> DuelChallenge {
            let challenge = self.challenge.as_mut().expect("stored challenge");
            update(challenge);
            challenge.clone()
        }

        fn balance(&self, player_id: i64) -> i64 {
            *self.balances.get(&player_id).expect("player balance")
        }
    }

    impl DuelRepositoryPort for StatefulDuelRepository {
        fn create_challenge_atomic(
            &mut self,
            guild_id: i64,
            channel_id: i64,
            challenger_id: i64,
            recipient_id: i64,
            wager: i64,
            now: i64,
            _challenger_cooldown_seconds: i64,
            _recipient_cooldown_seconds: i64,
            response_seconds: i64,
            _actor_id: i64,
        ) -> Result<DuelChallenge, CommandError> {
            if self.challenge.is_some() {
                return Err(CommandError::Invalid("unresolved duel".to_owned()));
            }
            let challenger_balance = self
                .balances
                .get_mut(&challenger_id)
                .ok_or_else(|| CommandError::Invalid("missing challenger".to_owned()))?;
            *challenger_balance -= wager + 50;
            let challenge = DuelChallenge {
                challenge_id: 7,
                guild_id,
                channel_id,
                message_id: None,
                challenger_id,
                recipient_id,
                wager,
                issuance_fee: 50,
                status: DuelStatus::Pending,
                trial_type: None,
                challenger_glicko: 1_400.0,
                challenger_rd: 80.0,
                recipient_glicko: 1_500.0,
                recipient_rd: 80.0,
                created_at: now,
                expires_at: now + response_seconds,
                next_reminder_at: Some(now + SERVICE_DAY),
                responded_at: None,
                resolved_at: None,
                winner_id: None,
                resolution_actor_id: None,
            };
            self.challenge = Some(challenge.clone());
            Ok(challenge)
        }

        fn get_pending_for_recipient(
            &mut self,
            recipient_id: i64,
            guild_id: i64,
        ) -> Result<Option<DuelChallenge>, CommandError> {
            Ok(self
                .challenge
                .as_ref()
                .filter(|challenge| {
                    challenge.guild_id == guild_id
                        && challenge.recipient_id == recipient_id
                        && challenge.status == DuelStatus::Pending
                        && challenge.message_id.is_some()
                })
                .cloned())
        }

        fn accept_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            recipient_id: i64,
            trial: DuelTrial,
            now: i64,
            _actor_id: i64,
        ) -> Result<DuelChallenge, CommandError> {
            let challenge = self.stored();
            if challenge.challenge_id != challenge_id
                || challenge.guild_id != guild_id
                || challenge.recipient_id != recipient_id
                || challenge.status != DuelStatus::Pending
            {
                return Err(CommandError::Invalid("not pending".to_owned()));
            }
            *self.balances.get_mut(&recipient_id).expect("recipient") -= challenge.wager;
            Ok(self.update_challenge(|challenge| {
                challenge.status = DuelStatus::Accepted;
                challenge.trial_type = Some(trial);
                challenge.responded_at = Some(now);
                challenge.next_reminder_at = Some(now + SERVICE_DAY);
            }))
        }

        fn decline_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            recipient_id: i64,
            now: i64,
            actor_id: i64,
        ) -> Result<DuelChallenge, CommandError> {
            let challenge = self.stored().clone();
            if challenge.challenge_id != challenge_id
                || challenge.guild_id != guild_id
                || challenge.recipient_id != recipient_id
                || challenge.status != DuelStatus::Pending
            {
                return Err(CommandError::Invalid("not pending".to_owned()));
            }
            let penalty = challenge.decline_penalty();
            *self
                .balances
                .get_mut(&challenge.challenger_id)
                .expect("challenger") += challenge.wager + penalty;
            *self
                .balances
                .get_mut(&challenge.recipient_id)
                .expect("recipient") -= penalty;
            Ok(self.update_challenge(|challenge| {
                challenge.status = DuelStatus::Declined;
                challenge.responded_at = Some(now);
                challenge.resolved_at = Some(now);
                challenge.resolution_actor_id = Some(actor_id);
                challenge.next_reminder_at = None;
            }))
        }

        fn get_challenge(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
        ) -> Result<Option<DuelChallenge>, CommandError> {
            Ok(self
                .challenge
                .as_ref()
                .filter(|challenge| {
                    challenge.challenge_id == challenge_id && challenge.guild_id == guild_id
                })
                .cloned())
        }

        fn resolve_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            winner_id: Option<i64>,
            now: i64,
            actor_id: i64,
            _policy: &ProfitDeductionPolicy<'_>,
        ) -> Result<DuelResolution, CommandError> {
            let challenge = self.stored();
            if challenge.challenge_id != challenge_id
                || challenge.guild_id != guild_id
                || challenge.status != DuelStatus::Accepted
            {
                return Err(CommandError::Invalid("not accepted".to_owned()));
            }
            Ok(DuelResolution {
                challenge: self.update_challenge(|challenge| {
                    challenge.status = if winner_id.is_some() {
                        DuelStatus::Resolved
                    } else {
                        DuelStatus::Voided
                    };
                    challenge.winner_id = winner_id;
                    challenge.resolved_at = Some(now);
                    challenge.resolution_actor_id = Some(actor_id);
                    challenge.next_reminder_at = None;
                }),
                deductions: ProfitDeductions::default(),
            })
        }

        fn bind_message(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            message_id: i64,
        ) -> Result<DuelChallenge, CommandError> {
            let challenge = self.stored();
            if challenge.challenge_id != challenge_id
                || challenge.guild_id != guild_id
                || challenge.status != DuelStatus::Pending
                || challenge.message_id.is_some()
            {
                return Err(CommandError::Invalid("cannot bind".to_owned()));
            }
            Ok(self.update_challenge(|challenge| {
                challenge.message_id = Some(message_id);
            }))
        }

        fn mark_delivery_failed_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            now: i64,
            actor_id: i64,
        ) -> Result<DuelChallenge, CommandError> {
            let challenge = self.stored().clone();
            if challenge.challenge_id != challenge_id
                || challenge.guild_id != guild_id
                || challenge.status != DuelStatus::Pending
                || challenge.message_id.is_some()
            {
                return Err(CommandError::Invalid("cannot fail delivery".to_owned()));
            }
            *self
                .balances
                .get_mut(&challenge.challenger_id)
                .expect("challenger") += challenge.wager + challenge.issuance_fee;
            Ok(self.update_challenge(|challenge| {
                challenge.status = DuelStatus::DeliveryFailed;
                challenge.resolved_at = Some(now);
                challenge.resolution_actor_id = Some(actor_id);
                challenge.next_reminder_at = None;
            }))
        }

        fn list_outstanding(&mut self, guild_id: i64) -> Result<Vec<DuelChallenge>, CommandError> {
            Ok(self
                .challenge
                .as_ref()
                .filter(|challenge| {
                    challenge.guild_id == guild_id
                        && matches!(challenge.status, DuelStatus::Pending | DuelStatus::Accepted)
                })
                .cloned()
                .into_iter()
                .collect())
        }

        fn list_pending_all(&mut self) -> Result<Vec<DuelChallenge>, CommandError> {
            Ok(self
                .challenge
                .as_ref()
                .filter(|challenge| challenge.status == DuelStatus::Pending)
                .cloned()
                .into_iter()
                .collect())
        }

        fn get_due_challenge_ids(&mut self, now: i64) -> Result<Vec<(i64, i64)>, CommandError> {
            let due = self
                .challenge
                .as_ref()
                .is_some_and(|challenge| match challenge.status {
                    DuelStatus::Pending => {
                        challenge.message_id.is_some()
                            && (challenge.expires_at <= now
                                || challenge.next_reminder_at.is_some_and(|due| due <= now))
                    }
                    DuelStatus::Accepted | DuelStatus::Expired => {
                        challenge.next_reminder_at.is_some_and(|due| due <= now)
                    }
                    _ => false,
                });
            Ok(if due {
                let challenge = self.stored();
                vec![(challenge.challenge_id, challenge.guild_id)]
            } else {
                Vec::new()
            })
        }

        fn claim_reminder_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            now: i64,
        ) -> Result<Option<DuelDueResult>, CommandError> {
            let challenge = self.stored();
            let Some(claimed_at) = challenge.next_reminder_at else {
                return Ok(None);
            };
            if challenge.challenge_id != challenge_id
                || challenge.guild_id != guild_id
                || challenge.status != DuelStatus::Pending
                || challenge.message_id.is_none()
                || challenge.expires_at <= now
                || claimed_at > now
            {
                return Ok(None);
            }
            let created_at = challenge.created_at;
            let expires_at = challenge.expires_at;
            let mut next = created_at + ((now - created_at) / SERVICE_DAY + 1) * SERVICE_DAY;
            if next - now < 600 {
                next += SERVICE_DAY;
            }
            next = next.min(expires_at);
            let claimed = self.update_challenge(|challenge| {
                challenge.next_reminder_at = Some(next);
            });
            Ok(Some(DuelDueResult {
                kind: DuelDueKind::Reminder,
                remaining_seconds: expires_at - now,
                ping_recipient: expires_at - now <= 48 * 3_600,
                challenge: claimed,
                claimed_reminder_at: Some(claimed_at),
                is_redelivery: false,
            }))
        }

        fn claim_unresolved_reminder_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            now: i64,
        ) -> Result<Option<DuelDueResult>, CommandError> {
            let challenge = self.stored();
            let Some(claimed_at) = challenge.next_reminder_at else {
                return Ok(None);
            };
            if challenge.challenge_id != challenge_id
                || challenge.guild_id != guild_id
                || challenge.status != DuelStatus::Accepted
                || claimed_at > now
            {
                return Ok(None);
            }
            let anchor = challenge.responded_at.unwrap_or(claimed_at);
            let mut next = anchor + ((now - anchor) / SERVICE_DAY + 1) * SERVICE_DAY;
            if next - now < 600 {
                next += SERVICE_DAY;
            }
            let claimed = self.update_challenge(|challenge| {
                challenge.next_reminder_at = Some(next);
            });
            Ok(Some(DuelDueResult {
                kind: DuelDueKind::Unresolved,
                challenge: claimed,
                remaining_seconds: 0,
                ping_recipient: false,
                claimed_reminder_at: Some(claimed_at),
                is_redelivery: false,
            }))
        }

        fn claim_expired_announcement_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            now: i64,
            retry_at: i64,
        ) -> Result<Option<DuelDueResult>, CommandError> {
            let challenge = self.stored();
            let Some(claimed_at) = challenge.next_reminder_at else {
                return Ok(None);
            };
            if challenge.challenge_id != challenge_id
                || challenge.guild_id != guild_id
                || challenge.status != DuelStatus::Expired
                || claimed_at > now
            {
                return Ok(None);
            }
            let claimed = self.update_challenge(|challenge| {
                challenge.next_reminder_at = Some(retry_at);
            });
            Ok(Some(DuelDueResult {
                kind: DuelDueKind::Expired,
                challenge: claimed,
                remaining_seconds: 0,
                ping_recipient: false,
                claimed_reminder_at: Some(claimed_at),
                is_redelivery: true,
            }))
        }

        fn expire_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            now: i64,
        ) -> Result<DuelChallenge, CommandError> {
            let challenge = self.stored().clone();
            if challenge.challenge_id != challenge_id
                || challenge.guild_id != guild_id
                || challenge.status != DuelStatus::Pending
                || challenge.message_id.is_none()
                || challenge.expires_at > now
            {
                return Err(CommandError::Invalid("not due".to_owned()));
            }
            let penalty = challenge.decline_penalty();
            *self
                .balances
                .get_mut(&challenge.challenger_id)
                .expect("challenger") += challenge.wager + penalty;
            *self
                .balances
                .get_mut(&challenge.recipient_id)
                .expect("recipient") -= penalty;
            Ok(self.update_challenge(|challenge| {
                challenge.status = DuelStatus::Expired;
                challenge.resolved_at = Some(now);
                challenge.next_reminder_at = Some(now);
            }))
        }

        fn clear_expired_announcement_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            expected_next_reminder_at: i64,
        ) -> Result<bool, CommandError> {
            let challenge = self.stored();
            if challenge.challenge_id != challenge_id
                || challenge.guild_id != guild_id
                || challenge.status != DuelStatus::Expired
                || challenge.next_reminder_at != Some(expected_next_reminder_at)
            {
                return Ok(false);
            }
            self.update_challenge(|challenge| {
                challenge.next_reminder_at = None;
            });
            Ok(true)
        }

        fn rearm_reminder_atomic(
            &mut self,
            challenge_id: i64,
            guild_id: i64,
            expected_status: DuelStatus,
            expected_next_reminder_at: i64,
            retry_at: i64,
        ) -> Result<bool, CommandError> {
            let challenge = self.stored();
            if challenge.challenge_id != challenge_id
                || challenge.guild_id != guild_id
                || challenge.status != expected_status
                || challenge.next_reminder_at != Some(expected_next_reminder_at)
            {
                return Ok(false);
            }
            self.update_challenge(|challenge| {
                challenge.next_reminder_at = Some(retry_at);
            });
            Ok(true)
        }
    }

    type StatefulDuelService =
        DuelApplicationService<StatefulDuelRepository, RecordingClock, NoopDuelEvolution>;

    fn stateful_duel_service(bind_message: bool) -> (StatefulDuelService, DuelChallenge) {
        let mut service = DuelApplicationService::new(
            StatefulDuelRepository::new(),
            RecordingClock::new(SERVICE_NOW as f64),
        );
        let mut challenge = service
            .issue(SERVICE_GUILD_ID, 77, 1, 2, 500, false)
            .expect("issue");
        if bind_message {
            challenge = service
                .bind_message(challenge.challenge_id, SERVICE_GUILD_ID, 1_234)
                .expect("bind");
        }
        (service, challenge)
    }

    #[test]
    fn test_expiry_announcement_failure_is_retried_without_double_pay() {
        let (mut service, challenge) = stateful_duel_service(true);
        let now = SERVICE_NOW + RESPONSE_SECONDS;
        let settled = service
            .process_due(challenge.challenge_id, SERVICE_GUILD_ID, now, true)
            .expect("process")
            .expect("settled expiry");
        assert_eq!(settled.kind, DuelDueKind::Expired);
        let balances = (service.repository.balance(1), service.repository.balance(2));
        assert_eq!(balances, (750, 250));

        assert_eq!(
            service.get_due_challenge_ids(now),
            Ok(vec![(challenge.challenge_id, SERVICE_GUILD_ID)])
        );
        let retry = service
            .process_due(challenge.challenge_id, SERVICE_GUILD_ID, now + 60, true)
            .expect("retry")
            .expect("announcement");
        assert_eq!(retry.kind, DuelDueKind::Expired);
        assert_eq!(retry.challenge.status, DuelStatus::Expired);
        assert_eq!(
            (service.repository.balance(1), service.repository.balance(2),),
            balances
        );
        assert_eq!(service.get_due_challenge_ids(now + 61), Ok(Vec::new()));
        assert_eq!(
            service.get_due_challenge_ids(now + 60 + REMINDER_RETRY_BACKOFF_SECONDS),
            Ok(vec![(challenge.challenge_id, SERVICE_GUILD_ID)])
        );
        assert_eq!(service.confirm_expiry_announced(&retry), Ok(true));
        assert_eq!(
            service.get_due_challenge_ids(now + 60 + REMINDER_RETRY_BACKOFF_SECONDS),
            Ok(Vec::new())
        );
        assert_eq!(
            service.process_due(
                challenge.challenge_id,
                SERVICE_GUILD_ID,
                now + 60 + REMINDER_RETRY_BACKOFF_SECONDS,
                true,
            ),
            Ok(None)
        );
    }

    #[test]
    fn test_deferred_wake_advances_expired_announcement_marker() {
        let (mut service, challenge) = stateful_duel_service(true);
        let now = SERVICE_NOW + RESPONSE_SECONDS;
        let settled = service
            .process_due(challenge.challenge_id, SERVICE_GUILD_ID, now, false)
            .expect("process")
            .expect("settled expiry");
        assert_eq!(settled.kind, DuelDueKind::Expired);
        let retry = service
            .process_due(challenge.challenge_id, SERVICE_GUILD_ID, now + 60, false)
            .expect("retry")
            .expect("announcement");
        assert_eq!(retry.kind, DuelDueKind::Expired);
        assert_eq!(service.get_due_challenge_ids(now + 61), Ok(Vec::new()));
        assert_eq!(
            service.get_due_challenge_ids(now + 60 + REMINDER_RETRY_BACKOFF_SECONDS),
            Ok(vec![(challenge.challenge_id, SERVICE_GUILD_ID)])
        );
    }

    #[test]
    fn test_unresolved_daily_ping_returns_to_grid_after_backoff() {
        let (mut service, challenge) = stateful_duel_service(true);
        let accepted = service
            .respond(SERVICE_GUILD_ID, 2, "trial_by_combat")
            .expect("accept");
        assert_eq!(accepted.next_reminder_at, Some(SERVICE_NOW + SERVICE_DAY));

        service.clock.value = (SERVICE_NOW + SERVICE_DAY + 30) as f64;
        let claimed = service
            .process_due(
                challenge.challenge_id,
                SERVICE_GUILD_ID,
                SERVICE_NOW + SERVICE_DAY + 30,
                true,
            )
            .expect("claim")
            .expect("unresolved");
        assert_eq!(claimed.kind, DuelDueKind::Unresolved);
        assert_eq!(service.rearm_reminder(&claimed), Ok(true));
        let stored = service
            .get_challenge(challenge.challenge_id, SERVICE_GUILD_ID)
            .expect("get")
            .expect("stored");
        assert_eq!(
            stored.next_reminder_at,
            Some(SERVICE_NOW + SERVICE_DAY + 30 + REMINDER_RETRY_BACKOFF_SECONDS)
        );

        service.clock.value = stored.next_reminder_at.expect("retry") as f64;
        let retried = service
            .process_due(
                challenge.challenge_id,
                SERVICE_GUILD_ID,
                stored.next_reminder_at.expect("retry"),
                true,
            )
            .expect("process")
            .expect("retried unresolved");
        assert_eq!(retried.kind, DuelDueKind::Unresolved);
        assert_eq!(
            retried.challenge.next_reminder_at,
            Some(SERVICE_NOW + 2 * SERVICE_DAY)
        );
    }

    #[test]
    fn test_service_process_due_real_repository_returns_reminder() {
        let (mut service, challenge) = stateful_duel_service(true);
        let result = service
            .process_due(
                challenge.challenge_id,
                SERVICE_GUILD_ID,
                SERVICE_NOW + SERVICE_DAY,
                true,
            )
            .expect("process")
            .expect("reminder");
        assert_eq!(result.kind, DuelDueKind::Reminder);
        assert_eq!(result.challenge.challenge_id, challenge.challenge_id);
        assert_eq!(
            result.challenge.next_reminder_at,
            Some(SERVICE_NOW + 2 * SERVICE_DAY)
        );
    }

    #[test]
    fn test_service_process_due_real_repository_returns_expiry() {
        let (mut service, challenge) = stateful_duel_service(true);
        let result = service
            .process_due(
                challenge.challenge_id,
                SERVICE_GUILD_ID,
                SERVICE_NOW + RESPONSE_SECONDS,
                true,
            )
            .expect("process")
            .expect("expiry");
        assert_eq!(result.kind, DuelDueKind::Expired);
        assert_eq!(result.challenge.challenge_id, challenge.challenge_id);
    }

    #[test]
    fn test_service_process_due_skips_unbound_expiry_without_moving_balances() {
        let (mut service, challenge) = stateful_duel_service(false);
        let balances_before = (service.repository.balance(1), service.repository.balance(2));
        assert_eq!(
            service.process_due(
                challenge.challenge_id,
                SERVICE_GUILD_ID,
                challenge.expires_at,
                true,
            ),
            Ok(None)
        );
        assert_eq!(
            (service.repository.balance(1), service.repository.balance(2),),
            balances_before
        );
        let stored = service.repository.stored();
        assert_eq!(stored.status, DuelStatus::Pending);
        assert_eq!(stored.message_id, None);
    }

    #[test]
    fn test_service_process_due_real_repository_returns_none_when_not_due() {
        let (mut service, challenge) = stateful_duel_service(true);
        assert_eq!(
            service.process_due(
                challenge.challenge_id,
                SERVICE_GUILD_ID,
                SERVICE_NOW + SERVICE_DAY - 1,
                true,
            ),
            Ok(None)
        );
        assert_eq!(service.repository.stored().status, DuelStatus::Pending);
    }

    #[test]
    fn test_service_process_due_real_repository_cycles_unresolved_until_resolution() {
        let (mut service, challenge) = stateful_duel_service(true);
        let accepted = service
            .respond(SERVICE_GUILD_ID, 2, "trial_by_combat")
            .expect("accept");
        assert_eq!(accepted.status, DuelStatus::Accepted);
        assert_eq!(accepted.next_reminder_at, Some(SERVICE_NOW + SERVICE_DAY));
        let result = service
            .process_due(
                challenge.challenge_id,
                SERVICE_GUILD_ID,
                SERVICE_NOW + SERVICE_DAY,
                true,
            )
            .expect("process")
            .expect("unresolved");
        assert_eq!(result.kind, DuelDueKind::Unresolved);
        assert_eq!(result.challenge.challenge_id, challenge.challenge_id);
        assert_eq!(
            result.challenge.next_reminder_at,
            Some(SERVICE_NOW + 2 * SERVICE_DAY)
        );
        service
            .resolve(
                SERVICE_GUILD_ID,
                99,
                challenge.challenge_id,
                "challenger_victory",
                &ProfitDeductionPolicy::none(),
            )
            .expect("resolve");
        assert_eq!(
            service.process_due(
                challenge.challenge_id,
                SERVICE_GUILD_ID,
                SERVICE_NOW + 2 * SERVICE_DAY,
                true,
            ),
            Ok(None)
        );
    }
}
