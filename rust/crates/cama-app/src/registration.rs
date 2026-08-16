//! Discord-independent registration command policy.
//!
//! Typed ports model reminder persistence, DM preflight, and role storage. The
//! production Discord callback and OpenDota transport remain Python-owned
//! during additive parity, while ordering and user-visible outcomes are fully
//! deterministic here.

use std::fmt::Display;

use cama_db::registration_repository::{RegistrationRepository, SetRolesOutcome};
use thiserror::Error;

pub const STEAM_ACCOUNT_ID_UPPER_BOUND: i64 = 1_i64 << 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistrationCommands<B, P> {
    pub bot: B,
    pub player_service: P,
}

impl<B, P> RegistrationCommands<B, P> {
    #[must_use]
    pub const fn new(bot: B, player_service: P) -> Self {
        Self {
            bot,
            player_service,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LobbyTarget {
    pub discord_id: i64,
    pub display_name: String,
    pub is_bot: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LobbyAutoNotifyRequest {
    pub subscriber_id: i64,
    pub guild_id: Option<i64>,
    pub enabled: Option<bool>,
    pub target: Option<LobbyTarget>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReminderPortError {
    Unavailable,
}

pub trait LobbyReminderPort {
    fn set_preference(
        &mut self,
        subscriber_id: i64,
        guild_id: i64,
        enabled: bool,
    ) -> Result<(), ReminderPortError>;

    fn has_player_subscription(
        &mut self,
        subscriber_id: i64,
        target_id: i64,
        guild_id: i64,
    ) -> Result<bool, ReminderPortError>;

    fn add_player_subscription(
        &mut self,
        subscriber_id: i64,
        target_id: i64,
        guild_id: i64,
    ) -> Result<bool, ReminderPortError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmPortError {
    Forbidden,
    Unavailable,
}

pub trait LobbyDmPort {
    type Message;

    fn send_preflight(&mut self, target_name: &str) -> Result<Self::Message, DmPortError>;
    fn edit_message(&mut self, message: &Self::Message, content: &str) -> Result<(), DmPortError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LobbyAutoNotifyOutcome {
    pub content: String,
    pub ephemeral: bool,
    pub preference_persisted: bool,
    pub subscription_persisted: bool,
    pub preflight_dm_sent: bool,
    pub preflight_dm_edited: bool,
}

impl LobbyAutoNotifyOutcome {
    fn ephemeral(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            ephemeral: true,
            preference_persisted: false,
            subscription_persisted: false,
            preflight_dm_sent: false,
            preflight_dm_edited: false,
        }
    }
}

pub fn execute_lobby_autonotify<R, D>(
    request: LobbyAutoNotifyRequest,
    reminder: Option<&mut R>,
    dm: &mut D,
) -> LobbyAutoNotifyOutcome
where
    R: LobbyReminderPort,
    D: LobbyDmPort,
{
    let Some(guild_id) = request.guild_id else {
        return LobbyAutoNotifyOutcome::ephemeral("This command can only be used in a server.");
    };
    let Some(reminder) = reminder else {
        return LobbyAutoNotifyOutcome::ephemeral(
            "Lobby notification preferences are currently unavailable.",
        );
    };

    if let Some(target) = request.target {
        return execute_target_subscription(request.subscriber_id, guild_id, target, reminder, dm);
    }

    let enabled = request.enabled.unwrap_or(true);
    if reminder
        .set_preference(request.subscriber_id, guild_id, enabled)
        .is_err()
    {
        return LobbyAutoNotifyOutcome::ephemeral(
            "Couldn't update your lobby notification preference. Try again later.",
        );
    }
    let mut outcome = LobbyAutoNotifyOutcome::ephemeral(if enabled {
        "Lobby auto-notify is now **ON**."
    } else {
        "Lobby auto-notify is now **OFF**."
    });
    outcome.preference_persisted = true;
    outcome
}

fn execute_target_subscription<R, D>(
    subscriber_id: i64,
    guild_id: i64,
    target: LobbyTarget,
    reminder: &mut R,
    dm: &mut D,
) -> LobbyAutoNotifyOutcome
where
    R: LobbyReminderPort,
    D: LobbyDmPort,
{
    if target.discord_id == subscriber_id {
        return LobbyAutoNotifyOutcome::ephemeral("You can't subscribe to your own lobby signup.");
    }
    if target.is_bot {
        return LobbyAutoNotifyOutcome::ephemeral("Bot accounts can't sign up for player lobbies.");
    }
    let duplicate = reminder
        .has_player_subscription(subscriber_id, target.discord_id, guild_id)
        .unwrap_or(false);
    let duplicate_message = format!(
        "You already have a one-time lobby alert set for **{}**.",
        target.display_name
    );
    if duplicate {
        return LobbyAutoNotifyOutcome::ephemeral(duplicate_message);
    }

    let dm_message = match dm.send_preflight(&target.display_name) {
        Ok(message) => message,
        Err(DmPortError::Forbidden) => {
            return LobbyAutoNotifyOutcome::ephemeral(
                "I can't DM you. Enable direct messages, then try again.",
            );
        }
        Err(DmPortError::Unavailable) => {
            return LobbyAutoNotifyOutcome::ephemeral(
                "I couldn't verify that I can DM you. Try again in a moment.",
            );
        }
    };

    let created = match reminder.add_player_subscription(subscriber_id, target.discord_id, guild_id)
    {
        Ok(created) => created,
        Err(ReminderPortError::Unavailable) => {
            let content = "Your DM check passed, but I couldn't save the alert. Try again later.";
            let edited = dm.edit_message(&dm_message, content).is_ok();
            let mut outcome = LobbyAutoNotifyOutcome::ephemeral(content);
            outcome.preflight_dm_sent = true;
            outcome.preflight_dm_edited = edited;
            return outcome;
        }
    };
    let content = if created {
        format!(
            "One-time lobby alert set for **{}**. I'll DM you when they join.",
            target.display_name
        )
    } else {
        duplicate_message
    };
    let edited = dm.edit_message(&dm_message, &content).is_ok();
    let mut outcome = LobbyAutoNotifyOutcome::ephemeral(content);
    outcome.subscription_persisted = created;
    outcome.preflight_dm_sent = true;
    outcome.preflight_dm_edited = edited;
    outcome
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MmrInteraction {
    pub modal_presented: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MmrButton {
    pub disabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MmrPromptView {
    pub attempts_left: u8,
}

impl MmrPromptView {
    /// The typed order mirrors discord.py: interaction first, then button.
    pub fn enter_mmr(&mut self, interaction: &mut MmrInteraction, button: &mut MmrButton) {
        if self.attempts_left == 0 {
            button.disabled = true;
            return;
        }
        interaction.modal_presented = true;
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SteamIdValidationError {
    #[error("Invalid Steam ID. Must be Steam32 (positive, 32-bit).")]
    OutOfRange,
}

pub fn validate_steam_id(steam_id: i64) -> Result<(), SteamIdValidationError> {
    if steam_id <= 0 || steam_id >= STEAM_ACCOUNT_ID_UPPER_BOUND {
        Err(SteamIdValidationError::OutOfRange)
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RoleInputError {
    #[error("Invalid role: {0}. Roles must be 1-5.")]
    Invalid(char),
    #[error("Please provide at least one role.")]
    Empty,
}

pub fn parse_roles(input: &str) -> Result<Vec<String>, RoleInputError> {
    let mut seen = [false; 5];
    let mut roles = Vec::new();
    for role in input
        .chars()
        .filter(|character| *character != ',' && *character != ' ')
    {
        if !(('1'..='5').contains(&role)) {
            return Err(RoleInputError::Invalid(role));
        }
        let index = usize::from(role as u8 - b'1');
        if !seen[index] {
            seen[index] = true;
            roles.push(role.to_string());
        }
    }
    if roles.is_empty() {
        Err(RoleInputError::Empty)
    } else {
        Ok(roles)
    }
}

pub trait RegistrationRolePort {
    type Error: Display;

    fn set_roles(
        &self,
        discord_id: i64,
        guild_id: i64,
        roles: &[String],
    ) -> Result<bool, Self::Error>;
}

impl RegistrationRolePort for RegistrationRepository {
    type Error = cama_db::registration_repository::RegistrationRepositoryError;

    fn set_roles(
        &self,
        discord_id: i64,
        guild_id: i64,
        roles: &[String],
    ) -> Result<bool, Self::Error> {
        self.set_roles_atomic(discord_id, Some(guild_id), roles)
            .map(|outcome| matches!(outcome, SetRolesOutcome::Updated(_)))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetRolesCommandOutcome {
    pub content: String,
    pub ephemeral: bool,
    pub persisted_roles: Option<Vec<String>>,
}

pub fn execute_set_roles(
    port: &impl RegistrationRolePort,
    discord_id: i64,
    guild_id: i64,
    input: &str,
) -> SetRolesCommandOutcome {
    let roles = match parse_roles(input) {
        Ok(roles) => roles,
        Err(error) => {
            return SetRolesCommandOutcome {
                content: error.to_string(),
                ephemeral: true,
                persisted_roles: None,
            };
        }
    };
    match port.set_roles(discord_id, guild_id, &roles) {
        Ok(true) => SetRolesCommandOutcome {
            content: format!("Set your preferred roles to: {}", roles.join(", ")),
            ephemeral: false,
            persisted_roles: Some(roles),
        },
        Ok(false) => SetRolesCommandOutcome {
            content: "Player not registered.".to_owned(),
            ephemeral: true,
            persisted_roles: None,
        },
        Err(error) => SetRolesCommandOutcome {
            content: format!("Unexpected error setting roles: {error}"),
            ephemeral: true,
            persisted_roles: None,
        },
    }
}

#[cfg(test)]
#[path = "registration/tests.rs"]
mod tests;
