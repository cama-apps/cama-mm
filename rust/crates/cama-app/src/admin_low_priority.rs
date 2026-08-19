//! Admin low-priority command policy over the shared SQLite repository contract.

use cama_db::low_priority_repository::{
    LowPriorityRepository, LowPriorityState, PythonIntegerInput, SetLowPriorityInput,
};
use cama_domain::formatting::escape_discord_text;

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
        manage_guild: false,
    },
    CommandMetadata {
        name: "remove",
        manage_guild: false,
    },
    CommandMetadata {
        name: "status",
        manage_guild: false,
    },
    CommandMetadata {
        name: "list",
        manage_guild: false,
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

    fn clear_low_priority(
        &self,
        discord_id: i64,
        guild_id: i64,
        removed_by: i64,
        reason: Option<&str>,
    ) -> Result<bool, String>;
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

    fn clear_low_priority(
        &self,
        discord_id: i64,
        guild_id: i64,
        removed_by: i64,
        reason: Option<&str>,
    ) -> Result<bool, String> {
        LowPriorityRepository::clear_low_priority(
            self,
            discord_id,
            Some(guild_id),
            removed_by,
            reason,
        )
        .map_err(|error| error.to_string())
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

/// Longest admin-authored reason rendered into player-facing low-priority copy.
///
/// Matches the bound `/admin lowprio add` puts on the option. Rows written
/// before that bound existed are clipped here rather than trusted, so a legacy
/// reason cannot push a DM or a status reply past Discord's 2000-character
/// message limit and get it silently rejected.
pub const REASON_DISPLAY_LIMIT: usize = 300;

/// Prepare an admin-authored low-priority reason for player-facing copy.
///
/// Blank reasons become `None` so no dangling `Reason:` line is rendered. The
/// surviving text is clipped to [`REASON_DISPLAY_LIMIT`] and then escaped, which
/// matters for more than markdown: `escape_discord_text` breaks up `@`, so a
/// reason an admin wrote as `reported by <@901>` reaches the player as inert
/// text instead of resolving into the reporter's name. Escaping cannot stop an
/// admin from typing a name in plain prose — the option description carries that
/// warning — but it does stop the mention syntax from doing it for them.
#[must_use]
pub fn player_visible_reason(reason: Option<&str>) -> Option<String> {
    let reason = reason.map(str::trim).filter(|reason| !reason.is_empty())?;
    let mut clipped: String = reason.chars().take(REASON_DISPLAY_LIMIT).collect();
    if clipped.chars().count() < reason.chars().count() {
        clipped.push('…');
    }
    Some(escape_discord_text(&clipped))
}

/// Polite notice sent to a player when an admin places them in low priority.
///
/// The notice carries the admin-authored reason so the player learns what the
/// correction is for, but never the issuing admin: `set_by` stays admin-facing
/// on `/admin lowprio status`. `low_priority_state` has no reporter column, so
/// the reason is the only field that could name whoever raised the behaviour;
/// it goes through [`player_visible_reason`] first.
#[must_use]
pub fn assignment_direct_message(wins_required: i64, reason: Option<&str>) -> String {
    let wins = if wins_required == 1 { "win" } else { "wins" };
    let games = if wins_required == 1 { "game" } else { "games" };
    let reason = player_visible_reason(reason)
        .map_or_else(String::new, |reason| format!("\nReason: {reason}"));
    format!(
        "You were placed in low priority for **{wins_required} {wins}**.\n\
         The matchmaker is asking for a small course correction.\
         {reason}\n\
         Win {wins_required} recorded {games} to return to regular matchmaking.\n\
         Use `/player lobby status` in the server to view your progress."
    )
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

        Ok(CommandDelivery {
            response: format!(
                "✅ Updated matchmaking state for <@{target_id}>. {} wins required.",
                state.wins_remaining
            ),
            ephemeral: true,
            direct_message: Some(assignment_direct_message(state.wins_required, reason)),
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

    pub fn remove(
        &self,
        admin_allowed: bool,
        admin_id: i64,
        target_id: i64,
        guild_id: i64,
        reason: Option<&str>,
    ) -> Result<CommandDelivery, String> {
        if !admin_allowed {
            return Ok(CommandDelivery {
                response: "❌ Admin only! You need Administrator or Manage Server permissions."
                    .to_owned(),
                ephemeral: true,
                direct_message: None,
            });
        }
        let previous = self.repository.get_state(target_id, guild_id)?;
        let removed = self
            .repository
            .clear_low_priority(target_id, guild_id, admin_id, reason)?;
        if removed && let Some(moderation) = self.moderation {
            let previous = previous.as_ref();
            let _ = moderation.record_event(LowPriorityAuditEvent {
                discord_id: target_id,
                guild_id,
                event_type: "lowprio_clear",
                wins_required: previous.map_or(DEFAULT_REQUIRED_WINS, |state| state.wins_required),
                wins_remaining: 0,
                pending_match_watermark: previous.and_then(|state| state.start_pending_match_id),
                actor_id: admin_id,
                reason: reason.map(str::to_owned),
                source: "admin",
            });
        }
        Ok(CommandDelivery {
            response: if removed {
                format!("✅ Cleared matchmaking state for <@{target_id}>.")
            } else {
                format!("<@{target_id}> does not have active restricted matchmaking state.")
            },
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

        fn clear_low_priority(
            &self,
            _discord_id: i64,
            _guild_id: i64,
            _removed_by: i64,
            _reason: Option<&str>,
        ) -> Result<bool, String> {
            Ok(true)
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
    fn test_low_priority_commands_have_no_static_permission_gate() {
        assert!(COMMANDS.iter().all(|command| !command.manage_guild));
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
        assert_eq!(
            delivery.direct_message.as_deref(),
            Some(assignment_direct_message(7, Some("internal")).as_str())
        );
        assert!(delivery.ephemeral);
    }

    #[test]
    fn test_lowprio_assignment_dm_is_polite_and_points_at_the_player_status_command() {
        assert_eq!(
            assignment_direct_message(3, Some("repeated abandons")),
            "You were placed in low priority for **3 wins**.\n\
             The matchmaker is asking for a small course correction.\n\
             Reason: repeated abandons\n\
             Win 3 recorded games to return to regular matchmaking.\n\
             Use `/player lobby status` in the server to view your progress."
        );
        assert_eq!(
            assignment_direct_message(1, None),
            "You were placed in low priority for **1 win**.\n\
             The matchmaker is asking for a small course correction.\n\
             Win 1 recorded game to return to regular matchmaking.\n\
             Use `/player lobby status` in the server to view your progress."
        );
    }

    #[test]
    fn test_lowprio_reason_defuses_a_mention_so_it_cannot_resolve_to_a_reporter() {
        // Suppressed mentions do not stop Discord rendering `<@id>` as the
        // named user, so the syntax itself has to be broken up.
        let rendered = player_visible_reason(Some("reported by <@901> for griefing"))
            .expect("reason survives");
        assert!(!rendered.contains("<@901>"));
        assert!(rendered.contains('\u{200b}'));
        assert!(
            rendered.contains("901"),
            "the text is defused, not censored"
        );

        let notice = assignment_direct_message(3, Some("reported by <@901> for griefing"));
        assert!(!notice.contains("<@901>"));
        // `@` gains a zero-width space and `>` is escaped, so Discord's mention
        // parser no longer sees a user reference to resolve.
        assert!(notice.contains("Reason: reported by <@\u{200b}901\\>"));
    }

    #[test]
    fn test_lowprio_reason_is_clipped_so_a_legacy_row_cannot_break_delivery() {
        let legacy = "g".repeat(4_000);
        let rendered = player_visible_reason(Some(&legacy)).expect("reason survives");
        assert_eq!(rendered.chars().count(), REASON_DISPLAY_LIMIT + 1);
        assert!(rendered.ends_with('…'));

        let notice = assignment_direct_message(3, Some(&legacy));
        assert!(
            notice.chars().count() < 2_000,
            "the notice must stay inside Discord's message limit, got {}",
            notice.chars().count()
        );
    }

    #[test]
    fn test_lowprio_reason_at_the_limit_is_not_marked_as_clipped() {
        let exact = "g".repeat(REASON_DISPLAY_LIMIT);
        let rendered = player_visible_reason(Some(&exact)).expect("reason survives");
        assert_eq!(rendered, exact);
        assert!(!rendered.ends_with('…'));
    }

    #[test]
    fn test_lowprio_assignment_dm_omits_a_blank_reason_rather_than_an_empty_line() {
        for blank in ["", "   ", "\n\t "] {
            let notice = assignment_direct_message(2, Some(blank));
            assert!(
                !notice.contains("Reason"),
                "blank reason {blank:?} rendered"
            );
            assert_eq!(notice, assignment_direct_message(2, None));
        }
    }

    #[test]
    fn test_lowprio_assignment_dm_carries_the_reason_but_never_the_issuing_admin() {
        let players = FakePlayers {
            exists: true,
            ..FakePlayers::default()
        };
        let repository = FakeRepository::default();
        *repository.set_result.lock().unwrap() = Some(state(4, 4));
        let moderation = FakeModeration::default();
        let delivery = LowPriorityAdmin::new(&players, &repository, Some(&moderation))
            .add(true, 900, 42, 77, Some("repeated abandons"), 4)
            .unwrap();

        let notice = delivery.direct_message.expect("assignment notice");
        assert!(notice.contains("**4 wins**"));
        assert!(notice.contains("Reason: repeated abandons"));
        // `set_by` is the issuing admin and stays on the admin-facing status
        // command; the player is told what, never who.
        assert!(!notice.contains("900"));
        assert!(!notice.contains("<@"));
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

    #[test]
    fn test_lowprio_remove_clears_state_and_records_audit_without_dm() {
        let players = FakePlayers {
            exists: true,
            ..FakePlayers::default()
        };
        let repository = FakeRepository::default();
        *repository.get_result.lock().unwrap() = Some(state(7, 3));
        let moderation = FakeModeration::default();
        let delivery = LowPriorityAdmin::new(&players, &repository, Some(&moderation))
            .remove(true, 900, 42, 77, Some("internal"))
            .unwrap();

        assert!(delivery.direct_message.is_none());
        assert!(delivery.response.contains("Cleared matchmaking state"));
        assert_eq!(
            *moderation.events.lock().unwrap(),
            [LowPriorityAuditEvent {
                discord_id: 42,
                guild_id: 77,
                event_type: "lowprio_clear",
                wins_required: 7,
                wins_remaining: 0,
                pending_match_watermark: Some(314),
                actor_id: 900,
                reason: Some("internal".to_owned()),
                source: "admin",
            }]
        );
    }
}
