//! Admin low-priority command policy over the shared SQLite repository contract.

use cama_db::low_priority_repository::{
    LowPriorityRepository, LowPriorityState, PythonIntegerInput, SetLowPriorityInput,
};

pub const DEFAULT_REQUIRED_WINS: i64 = 3;
pub const MIN_REQUIRED_WINS: i64 = 1;
pub const MAX_REQUIRED_WINS: i64 = 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandMetadata {
    pub name: &'static str,
    pub manage_guild: bool,
}

pub const COMMANDS: [CommandMetadata; 4] = [
    CommandMetadata {
        name: "add",
        manage_guild: true,
    },
    CommandMetadata {
        name: "remove",
        manage_guild: true,
    },
    CommandMetadata {
        name: "status",
        manage_guild: true,
    },
    CommandMetadata {
        name: "list",
        manage_guild: true,
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WinsOption {
    pub default: i64,
    pub minimum: i64,
    pub maximum: i64,
}

pub const WINS_OPTION: WinsOption = WinsOption {
    default: DEFAULT_REQUIRED_WINS,
    minimum: MIN_REQUIRED_WINS,
    maximum: MAX_REQUIRED_WINS,
};

pub trait PlayerDirectory {
    fn player_exists(&self, discord_id: i64, guild_id: i64) -> Result<bool, String>;
}

pub trait LowPriorityCommandPort {
    fn get_state(&self, discord_id: i64, guild_id: i64)
    -> Result<Option<LowPriorityState>, String>;

    fn set_low_priority(&self, input: &SetLowPriorityInput) -> Result<LowPriorityState, String>;
}

impl LowPriorityCommandPort for LowPriorityRepository {
    fn get_state(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<LowPriorityState>, String> {
        LowPriorityRepository::get_state(self, discord_id, Some(guild_id))
            .map_err(|error| error.to_string())
    }

    fn set_low_priority(&self, input: &SetLowPriorityInput) -> Result<LowPriorityState, String> {
        LowPriorityRepository::set_low_priority(self, input).map_err(|error| error.to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LowPriorityAuditEvent {
    pub discord_id: i64,
    pub guild_id: i64,
    pub event_type: &'static str,
    pub wins_required: i64,
    pub wins_remaining: i64,
    pub pending_match_watermark: Option<i64>,
    pub actor_id: i64,
    pub reason: Option<String>,
    pub source: &'static str,
}

pub trait LowPriorityModerationPort {
    fn pending_match_watermark(&self, guild_id: i64) -> Result<Option<i64>, String>;
    fn record_event(&self, event: LowPriorityAuditEvent) -> Result<(), String>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandDelivery {
    pub response: String,
    pub ephemeral: bool,
    pub direct_message: Option<String>,
}

pub struct LowPriorityAdmin<'a, P, R, M> {
    players: &'a P,
    repository: &'a R,
    moderation: Option<&'a M>,
}

impl<'a, P, R, M> LowPriorityAdmin<'a, P, R, M>
where
    P: PlayerDirectory,
    R: LowPriorityCommandPort,
    M: LowPriorityModerationPort,
{
    #[must_use]
    pub const fn new(players: &'a P, repository: &'a R, moderation: Option<&'a M>) -> Self {
        Self {
            players,
            repository,
            moderation,
        }
    }

    pub fn add(
        &self,
        admin_allowed: bool,
        admin_id: i64,
        target_id: i64,
        guild_id: i64,
        reason: Option<&str>,
        wins: i64,
    ) -> Result<CommandDelivery, String> {
        if !admin_allowed {
            return Ok(CommandDelivery {
                response: "❌ Admin only! You need Administrator or Manage Server permissions."
                    .to_owned(),
                ephemeral: true,
                direct_message: None,
            });
        }
        if !self.players.player_exists(target_id, guild_id)? {
            return Ok(CommandDelivery {
                response: format!("⚠️ <@{target_id}> is not registered."),
                ephemeral: true,
                direct_message: None,
            });
        }

        let previous = self.repository.get_state(target_id, guild_id)?;
        let watermark = self
            .moderation
            .map(|moderation| moderation.pending_match_watermark(guild_id))
            .transpose()?
            .flatten();
        let mut input = SetLowPriorityInput::new(
            target_id,
            Some(guild_id),
            admin_id,
            reason.map(str::to_owned),
        );
        input.wins_required = PythonIntegerInput::Integer(wins);
        input.start_pending_match_id = watermark.map(PythonIntegerInput::Integer);
        let state = self.repository.set_low_priority(&input)?;

        if let Some(moderation) = self.moderation {
            let _ = moderation.record_event(LowPriorityAuditEvent {
                discord_id: target_id,
                guild_id,
                event_type: if previous.is_some_and(|state| state.active) {
                    "lowprio_replace"
                } else {
                    "lowprio_assign"
                },
                wins_required: state.wins_required,
                wins_remaining: state.wins_remaining,
                pending_match_watermark: state.start_pending_match_id.or(watermark),
                actor_id: admin_id,
                reason: reason.map(str::to_owned),
                source: "admin",
            });
        }

        let mut direct_message = format!(
            "You were placed in low priority for **{} wins**.\n\
             The matchmaker is asking for a small course correction.",
            state.wins_required
        );
        if let Some(reason) = reason {
            direct_message.push_str(&format!("\nReason: {reason}"));
        }
        direct_message.push_str(&format!(
            "\nWin {} recorded games to return to regular matchmaking.\n\
             Use `/player lobby status` to view your progress.",
            state.wins_required
        ));
        Ok(CommandDelivery {
            response: format!(
                "✅ Updated matchmaking state for <@{target_id}>. {} wins required.",
                state.wins_remaining
            ),
            ephemeral: true,
            direct_message: Some(direct_message),
        })
    }

    pub fn status(
        &self,
        admin_allowed: bool,
        target_id: i64,
        guild_id: i64,
    ) -> Result<CommandDelivery, String> {
        if !admin_allowed {
            return Ok(CommandDelivery {
                response: "❌ Admin only! You need Administrator or Manage Server permissions."
                    .to_owned(),
                ephemeral: true,
                direct_message: None,
            });
        }
        let state = self.repository.get_state(target_id, guild_id)?;
        let response = match state {
            Some(state) if state.active => format!(
                "<@{target_id}>: {}/{} wins completed ({} remaining).\nReason: {}\nIssued by: <@{}>\nApplied: {}",
                state.wins_required - state.wins_remaining,
                state.wins_required,
                state.wins_remaining,
                state.reason.as_deref().unwrap_or("Not provided"),
                state.set_by,
                state.created_at,
            ),
            _ => format!("<@{target_id}> does not have active restricted matchmaking state."),
        };
        Ok(CommandDelivery {
            response,
            ephemeral: true,
            direct_message: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct FakePlayers {
        calls: Mutex<Vec<(i64, i64)>>,
        exists: bool,
    }

    impl PlayerDirectory for FakePlayers {
        fn player_exists(&self, discord_id: i64, guild_id: i64) -> Result<bool, String> {
            self.calls.lock().unwrap().push((discord_id, guild_id));
            Ok(self.exists)
        }
    }

    struct FakeRepository {
        get_calls: Mutex<Vec<(i64, i64)>>,
        set_calls: Mutex<Vec<SetLowPriorityInput>>,
        get_result: Mutex<Option<LowPriorityState>>,
        set_result: Mutex<Option<LowPriorityState>>,
    }

    impl Default for FakeRepository {
        fn default() -> Self {
            Self {
                get_calls: Mutex::default(),
                set_calls: Mutex::default(),
                get_result: Mutex::new(None),
                set_result: Mutex::new(None),
            }
        }
    }

    impl LowPriorityCommandPort for FakeRepository {
        fn get_state(
            &self,
            discord_id: i64,
            guild_id: i64,
        ) -> Result<Option<LowPriorityState>, String> {
            self.get_calls.lock().unwrap().push((discord_id, guild_id));
            Ok(self.get_result.lock().unwrap().clone())
        }

        fn set_low_priority(
            &self,
            input: &SetLowPriorityInput,
        ) -> Result<LowPriorityState, String> {
            self.set_calls.lock().unwrap().push(input.clone());
            Ok(self.set_result.lock().unwrap().clone().unwrap())
        }
    }

    #[derive(Default)]
    struct FakeModeration {
        watermark_calls: Mutex<Vec<i64>>,
        events: Mutex<Vec<LowPriorityAuditEvent>>,
        watermark: Option<i64>,
    }

    impl LowPriorityModerationPort for FakeModeration {
        fn pending_match_watermark(&self, guild_id: i64) -> Result<Option<i64>, String> {
            self.watermark_calls.lock().unwrap().push(guild_id);
            Ok(self.watermark)
        }

        fn record_event(&self, event: LowPriorityAuditEvent) -> Result<(), String> {
            self.events.lock().unwrap().push(event);
            Ok(())
        }
    }

    fn state(wins_required: i64, wins_remaining: i64) -> LowPriorityState {
        LowPriorityState {
            discord_id: 42,
            guild_id: 77,
            wins_required,
            wins_remaining,
            start_pending_match_id: Some(314),
            active: true,
            reason: Some("internal".to_owned()),
            set_by: 900,
            removed_by: None,
            removed_reason: None,
            created_at: "2026-08-05 12:34:56".to_owned(),
            updated_at: "2026-08-05 12:34:56".to_owned(),
        }
    }

    #[test]
    fn test_low_priority_commands_require_manage_guild_by_default() {
        assert!(COMMANDS.iter().all(|command| command.manage_guild));
        assert_eq!(
            COMMANDS.map(|command| command.name),
            ["add", "remove", "status", "list"]
        );
    }

    #[test]
    fn test_lowprio_add_exposes_bounded_configurable_win_count() {
        assert_eq!(WINS_OPTION.default, 3);
        assert_eq!(WINS_OPTION.minimum, 1);
        assert_eq!(WINS_OPTION.maximum, 20);
    }

    #[test]
    fn test_lowprio_add_sets_configurable_wins_and_pending_match_watermark() {
        let players = FakePlayers {
            exists: true,
            ..FakePlayers::default()
        };
        let repository = FakeRepository::default();
        *repository.set_result.lock().unwrap() = Some(state(7, 7));
        let moderation = FakeModeration {
            watermark: Some(314),
            ..FakeModeration::default()
        };
        let delivery = LowPriorityAdmin::new(&players, &repository, Some(&moderation))
            .add(true, 900, 42, 77, Some("internal"), 7)
            .unwrap();

        assert_eq!(*players.calls.lock().unwrap(), [(42, 77)]);
        assert_eq!(*moderation.watermark_calls.lock().unwrap(), [77]);
        let inputs = repository.set_calls.lock().unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].wins_required, PythonIntegerInput::Integer(7));
        assert_eq!(
            inputs[0].start_pending_match_id,
            Some(PythonIntegerInput::Integer(314))
        );
        assert_eq!(inputs[0].set_by, 900);
        assert_eq!(inputs[0].reason.as_deref(), Some("internal"));
        assert_eq!(
            *moderation.events.lock().unwrap(),
            [LowPriorityAuditEvent {
                discord_id: 42,
                guild_id: 77,
                event_type: "lowprio_assign",
                wins_required: 7,
                wins_remaining: 7,
                pending_match_watermark: Some(314),
                actor_id: 900,
                reason: Some("internal".to_owned()),
                source: "admin",
            }]
        );
        assert!(delivery.response.contains("7 wins required"));
        let dm = delivery.direct_message.unwrap();
        assert!(dm.contains("7 wins"));
        assert!(dm.contains("internal"));
        assert!(delivery.ephemeral);
    }

    #[test]
    fn test_lowprio_runtime_admin_check_blocks_repository_access() {
        let players = FakePlayers::default();
        let repository = FakeRepository::default();
        let moderation = FakeModeration::default();
        let delivery = LowPriorityAdmin::new(&players, &repository, Some(&moderation))
            .add(false, 900, 42, 77, None, 3)
            .unwrap();
        assert!(players.calls.lock().unwrap().is_empty());
        assert!(repository.get_calls.lock().unwrap().is_empty());
        assert!(repository.set_calls.lock().unwrap().is_empty());
        assert!(delivery.ephemeral);
    }

    #[test]
    fn test_lowprio_status_reports_progress_ephemerally() {
        let players = FakePlayers::default();
        let repository = FakeRepository::default();
        let mut current = state(5, 2);
        current.reason = Some("repeat abandonment".to_owned());
        current.set_by = 901;
        *repository.get_result.lock().unwrap() = Some(current);
        let moderation = FakeModeration::default();
        let delivery = LowPriorityAdmin::new(&players, &repository, Some(&moderation))
            .status(true, 42, 77)
            .unwrap();
        assert!(
            delivery
                .response
                .contains("3/5 wins completed (2 remaining)")
        );
        assert!(delivery.response.contains("repeat abandonment"));
        assert!(delivery.response.contains("<@901>"));
        assert!(delivery.response.contains("2026-08-05 12:34:56"));
        assert!(delivery.ephemeral);
    }
}
