//! Discord-independent duel application service.
//!
//! Duel policy routing sits behind typed repository and clock ports so it
//! stays independent of the Discord transport.

use cama_db::duel_repository::{
    DuelChallenge, DuelChallengeRepository, DuelDueKind, DuelDueResult, DuelRepositoryError,
    DuelResolution, DuelStatus, DuelTrial,
};
use cama_db::profit_deductions::{ProfitDeductionPolicy, ProfitDeductions};
use cama_domain::pet_evolution::PetActivity;
use thiserror::Error;

pub const MAIN_CHANNEL_NAME: &str = "dota-mm";
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
        DuelRepositoryError::ChallengerCooldown { ready_at } => CommandError::Invalid(format!(
            "Your duel challenge cooldown has not elapsed. You can issue another challenge \
             <t:{ready_at}:R> (<t:{ready_at}:f>)."
        )),
        DuelRepositoryError::RecipientProtected { ready_at } => CommandError::Invalid(format!(
            "That player was recently challenged and has weekly protection until \
             <t:{ready_at}:f> (<t:{ready_at}:R>)."
        )),
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

pub trait FlavorPort {
    fn generate(&mut self, event: FlavorEvent, guild_id: i64, details: &FlavorDetails) -> String;
}

/// The `/duel resolve` announcement: the net pot for a winner, with every
/// non-zero withholding named, or the refund notice for a void. `winner_name`
/// is readable display text, never a mention.
#[must_use]
pub fn resolution_detail(resolution: &DuelResolution, winner_name: &str) -> String {
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
        "Challenge #{} is resolved: {winner_name} wins {} JC{}.",
        challenge.challenge_id,
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

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};

    use super::*;

    const GUILD_ID: i64 = 100;
    const CHANNEL_ID: i64 = 200;
    const CHALLENGER_ID: i64 = 1;
    const RECIPIENT_ID: i64 = 2;

    #[test]
    fn cooldown_errors_tell_the_player_when_they_can_try_again() {
        let challenger = map_duel_repository_error(DuelRepositoryError::ChallengerCooldown {
            ready_at: 1_700_000_000,
        });
        let CommandError::Invalid(message) = challenger else {
            panic!("expected an invalid-command error, got {challenger:?}");
        };
        assert!(message.starts_with("Your duel challenge cooldown has not elapsed."));
        assert!(
            message.contains("<t:1700000000:R>") && message.contains("<t:1700000000:f>"),
            "message must carry the ready time as Discord timestamps: {message}"
        );

        let recipient = map_duel_repository_error(DuelRepositoryError::RecipientProtected {
            ready_at: 1_700_000_000,
        });
        let CommandError::Invalid(message) = recipient else {
            panic!("expected an invalid-command error, got {recipient:?}");
        };
        assert!(message.starts_with("That player was recently challenged"));
        assert!(
            message.contains("<t:1700000000:f>") && message.contains("<t:1700000000:R>"),
            "message must carry the protection end as Discord timestamps: {message}"
        );
    }

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
