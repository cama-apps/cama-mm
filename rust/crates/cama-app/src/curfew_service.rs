//! Curfew-window management and enforcement.
//!
//! A player can register any number of named windows (e.g. "work", "night
//! shift" — the name and purpose are entirely up to them). Each window picks
//! one of four modes:
//!
//! - `default` — blocks joining and sweeps the player out of any lobby the
//!   moment the window starts.
//! - `strict` — same enforcement as `default`, but an edit that reduces the
//!   window (or deletes it) is staged rather than applied immediately: it
//!   never takes effect the same calendar day it's made, only at the
//!   window's next local morning. Extending the window applies right away.
//!   This closes the "loosen it right before it fires tonight" bypass.
//! - `informational` — never blocks outright. A join during the window is
//!   answered with a yes/no confirmation; a yes covers the player until
//!   their next completed (non-aborted) match or the day rolls over. A
//!   player already queued when the window starts is removed and offered
//!   the same confirmation to rejoin.
//!
//! Every join surface goes through [`CurfewService::join_gate`], and the
//! confirmation itself is expressed as [`CurfewConsent`] on that same call,
//! so there's exactly one place that decides whether a curfewed player gets
//! in.
//!
//! Ports `services/curfew_service.py`.

use std::collections::BTreeMap;

use cama_db::curfew::{CurfewRepository, PendingCurfewChange};
use cama_domain::curfew::{
    CurfewMode, CurfewWindow, effective_timezone, find_active_window, is_valid_timezone,
    local_date_string, next_local_morning, parse_mode, retains_coverage,
};
use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::dedicated_lobby_channel::{GuildId, LobbyScope, UserId};
use crate::embeds::LobbyKind;
use crate::lobby_service::{LobbyClock, LobbyPlayerPort, LobbyService, PendingMatchPort};

pub const MAX_WINDOW_NAME_LENGTH: usize = 40;

#[derive(Debug, Error)]
pub enum CurfewServiceError {
    #[error("Player not registered.")]
    PlayerNotRegistered,
    #[error("Give this window a name.")]
    EmptyName,
    #[error("Name must be {MAX_WINDOW_NAME_LENGTH} characters or fewer.")]
    NameTooLong,
    #[error("Hour must be between 0 and 23.")]
    InvalidHour,
    #[error("Minute must be between 0 and 59.")]
    InvalidMinute,
    #[error("Start and end time can't be the same.")]
    EqualStartAndEnd,
    #[error("Unknown timezone '{0}'. Use an IANA name like 'America/New_York'.")]
    InvalidTimezone(String),
    #[error("{0}")]
    InvalidDays(String),
    #[error("{0}")]
    InvalidMode(String),
    #[error("curfew SQLite operation failed: {0}")]
    Sqlite(String),
}

impl From<rusqlite::Error> for CurfewServiceError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error.to_string())
    }
}

/// A lobby member removed by the periodic sweep because their window
/// started while they were queued. `mode` tells the caller how to phrase
/// the notice: an informational-mode player can rejoin by saying yes, while
/// a default/strict-mode player just has to wait.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CurfewKick {
    pub discord_id: i64,
    pub guild_id: i64,
    pub lobby_kind: LobbyKind,
    pub window_name: String,
    pub mode: CurfewMode,
}

/// Whether the player has explicitly said yes to queuing through their
/// active informational-mode window on this join attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CurfewConsent {
    /// A plain join: an active informational window asks first.
    Withheld,
    /// The player answered the confirmation prompt with yes: record the
    /// acknowledgement and let them in.
    Given,
}

/// Outcome of `/player curfew add` — either the window took effect right
/// away, or (a reducing edit of a strict-mode window) it was staged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CurfewWindowChange {
    Applied(CurfewWindow),
    Staged {
        window: CurfewWindow,
        effective_at: DateTime<Utc>,
    },
}

/// Outcome of `/player curfew remove`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CurfewRemoveOutcome {
    Removed,
    Staged { effective_at: DateTime<Utc> },
    NotFound,
}

/// Outcome of checking curfew before letting a player into a lobby.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CurfewGateOutcome {
    /// No active window (or none that applies).
    Clear,
    /// Default/strict-mode window: refuse the join.
    Blocked(CurfewWindow),
    /// Informational-mode window and the player hasn't said yes yet: ask
    /// them.
    NeedsConfirmation { window: CurfewWindow },
    /// The player is already covered for today (said yes earlier) or just
    /// confirmed: let them in quietly.
    Covered { window: CurfewWindow },
}

#[derive(Clone)]
pub struct CurfewService {
    repository: CurfewRepository,
}

impl CurfewService {
    #[must_use]
    pub const fn new(repository: CurfewRepository) -> Self {
        Self { repository }
    }

    /// Create a window, or overwrite/stage-an-edit-to it if the player
    /// already has one by that name. An edit that reduces an existing
    /// `strict`-mode window never applies today — it's staged to
    /// take effect at that window's next local morning instead, so the
    /// currently-committed version keeps enforcing through the rest of
    /// today. Brand-new windows and edits that only extend coverage apply
    /// immediately.
    #[allow(clippy::too_many_arguments)]
    pub fn add_window(
        &self,
        discord_id: i64,
        guild_id: i64,
        name: &str,
        start_hour: u32,
        start_minute: u32,
        end_hour: u32,
        end_minute: u32,
        timezone: Option<&str>,
        days: Option<&str>,
        mode: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<CurfewWindowChange, CurfewServiceError> {
        if !self.repository.player_exists(discord_id, guild_id)? {
            return Err(CurfewServiceError::PlayerNotRegistered);
        }
        let name = name.trim();
        if name.is_empty() {
            return Err(CurfewServiceError::EmptyName);
        }
        if name.chars().count() > MAX_WINDOW_NAME_LENGTH {
            return Err(CurfewServiceError::NameTooLong);
        }
        for (hour, minute) in [(start_hour, start_minute), (end_hour, end_minute)] {
            if hour > 23 {
                return Err(CurfewServiceError::InvalidHour);
            }
            if minute > 59 {
                return Err(CurfewServiceError::InvalidMinute);
            }
        }
        if start_hour == end_hour && start_minute == end_minute {
            return Err(CurfewServiceError::EqualStartAndEnd);
        }
        if let Some(timezone) = timezone
            && !is_valid_timezone(timezone)
        {
            return Err(CurfewServiceError::InvalidTimezone(timezone.to_owned()));
        }
        let days = days
            .map(cama_domain::curfew::parse_weekdays)
            .transpose()
            .map_err(CurfewServiceError::InvalidDays)?;
        let mode = mode
            .map(parse_mode)
            .transpose()
            .map_err(CurfewServiceError::InvalidMode)?
            .unwrap_or_default();
        let window = CurfewWindow {
            discord_id,
            guild_id,
            name: name.to_owned(),
            start_hour,
            start_minute,
            end_hour,
            end_minute,
            timezone: timezone.map(str::to_owned),
            days,
            mode,
        };
        let existing = self.repository.get_window(discord_id, guild_id, name)?;
        if let Some(existing) = &existing
            && existing.mode.stages_changes()
        {
            let general_timezone = self.repository.general_timezone(discord_id, guild_id)?;
            // Extending the window (or moving it to another staging mode
            // without freeing any curfewed minute) tightens enforcement,
            // so it can land right away. Anything that frees up time the
            // committed window would have covered — or drops out of a
            // staging mode — waits for the next morning.
            let reduces = !window.mode.stages_changes()
                || !retains_coverage(&window, existing, general_timezone.as_deref(), now);
            if !reduces {
                self.repository.add_or_replace(&window)?;
                return Ok(CurfewWindowChange::Applied(window));
            }
            let tz = effective_timezone(existing, general_timezone.as_deref());
            let effective_at = next_local_morning(tz, now);
            self.repository
                .stage_pending_upsert(&window, effective_at)?;
            return Ok(CurfewWindowChange::Staged {
                window,
                effective_at,
            });
        }
        self.repository.add_or_replace(&window)?;
        Ok(CurfewWindowChange::Applied(window))
    }

    /// Delete a named window, or stage its removal if it's currently in
    /// strict mode (see [`Self::add_window`]).
    pub fn remove_window(
        &self,
        discord_id: i64,
        guild_id: i64,
        name: &str,
        now: DateTime<Utc>,
    ) -> Result<CurfewRemoveOutcome, CurfewServiceError> {
        let name = name.trim();
        let Some(existing) = self.repository.get_window(discord_id, guild_id, name)? else {
            return Ok(CurfewRemoveOutcome::NotFound);
        };
        if existing.mode.stages_changes() {
            let general_timezone = self.repository.general_timezone(discord_id, guild_id)?;
            let tz = effective_timezone(&existing, general_timezone.as_deref());
            let effective_at = next_local_morning(tz, now);
            self.repository
                .stage_pending_delete(discord_id, guild_id, name, effective_at)?;
            return Ok(CurfewRemoveOutcome::Staged { effective_at });
        }
        self.repository.remove(discord_id, guild_id, name)?;
        Ok(CurfewRemoveOutcome::Removed)
    }

    pub fn list_windows(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Vec<CurfewWindow>, CurfewServiceError> {
        Ok(self.repository.list_for_player(discord_id, guild_id)?)
    }

    /// Strict-mode edits/deletes staged for this player, keyed by window name.
    pub fn pending_changes(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<BTreeMap<String, PendingCurfewChange>, CurfewServiceError> {
        Ok(self
            .repository
            .pending_changes_for_player(discord_id, guild_id)?)
    }

    /// Commit every staged strict-mode change whose effective time has
    /// arrived. Meant to be called on the same cadence as [`Self::sweep`].
    pub fn apply_due_pending_changes(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<cama_db::curfew::AppliedPendingCurfewChange>, CurfewServiceError> {
        Ok(self.repository.apply_due_pending_changes(now)?)
    }

    /// The player's general `/player timezone` setting, for display purposes.
    pub fn general_timezone(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<String>, CurfewServiceError> {
        Ok(self.repository.general_timezone(discord_id, guild_id)?)
    }

    /// Return the player's currently-active window, if any.
    pub fn active_window(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: DateTime<Utc>,
    ) -> Result<Option<CurfewWindow>, CurfewServiceError> {
        let windows = self.repository.list_for_player(discord_id, guild_id)?;
        if windows.is_empty() {
            return Ok(None);
        }
        let general_timezone = self.repository.general_timezone(discord_id, guild_id)?;
        Ok(find_active_window(&windows, general_timezone.as_deref(), now).cloned())
    }

    /// Decide what happens when this player tries to join a lobby (or open
    /// one) right now: nothing, a hard block, a confirmation prompt, or —
    /// with `consent` given — the acknowledgement that lets them in. Shared
    /// by every join surface, and by the confirmation button itself, so a
    /// curfewed player can't slip through one path that forgot to check.
    ///
    /// Without consent this is read-only. With consent it records the
    /// player's yes for the rest of their local day.
    pub fn join_gate(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: DateTime<Utc>,
        consent: CurfewConsent,
    ) -> Result<CurfewGateOutcome, CurfewServiceError> {
        let windows = self.repository.list_for_player(discord_id, guild_id)?;
        if windows.is_empty() {
            return Ok(CurfewGateOutcome::Clear);
        }
        let general_timezone = self.repository.general_timezone(discord_id, guild_id)?;
        let Some(window) = find_active_window(&windows, general_timezone.as_deref(), now).cloned()
        else {
            return Ok(CurfewGateOutcome::Clear);
        };
        if !window.mode.asks_for_confirmation() {
            return Ok(CurfewGateOutcome::Blocked(window));
        }
        let tz = effective_timezone(&window, general_timezone.as_deref());
        let coverage_date = local_date_string(tz, now);
        if self
            .repository
            .is_covered(discord_id, guild_id, &window.name, &coverage_date)?
        {
            return Ok(CurfewGateOutcome::Covered { window });
        }
        match consent {
            CurfewConsent::Withheld => Ok(CurfewGateOutcome::NeedsConfirmation { window }),
            CurfewConsent::Given => {
                self.repository.record_coverage(
                    discord_id,
                    guild_id,
                    &window.name,
                    &coverage_date,
                )?;
                Ok(CurfewGateOutcome::Covered { window })
            }
        }
    }

    /// Clear standing confirmation coverage for these players — call after
    /// a completed (non-aborted) match so their next curfew join asks again.
    pub fn clear_coverage_for_match(
        &self,
        discord_ids: &[i64],
        guild_id: i64,
    ) -> Result<(), CurfewServiceError> {
        Ok(self.repository.clear_coverage(discord_ids, guild_id)?)
    }

    /// Remove any lobby member currently inside one of their windows who
    /// hasn't confirmed their way through it today. An informational-mode
    /// player who already said yes stays; one who hasn't is removed and —
    /// via the returned [`CurfewKick::mode`] — offered the same confirmation
    /// to rejoin, so the sweep never admits anyone without going through
    /// the join gate.
    ///
    /// Returns the players actually removed, for the caller to notify.
    #[must_use]
    pub fn sweep<P, M, C>(
        &self,
        lobby: &LobbyService<P, M, C>,
        guild_ids: &[i64],
        now: DateTime<Utc>,
    ) -> Vec<CurfewKick>
    where
        P: LobbyPlayerPort,
        M: PendingMatchPort,
        C: LobbyClock,
    {
        let mut kicks = Vec::new();
        for &guild_id in guild_ids {
            for kind in [LobbyKind::Open, LobbyKind::LowSkill] {
                kicks.extend(self.sweep_lobby(lobby, guild_id, kind, now));
            }
        }
        kicks
    }

    fn sweep_lobby<P, M, C>(
        &self,
        lobby: &LobbyService<P, M, C>,
        guild_id: i64,
        kind: LobbyKind,
        now: DateTime<Utc>,
    ) -> Vec<CurfewKick>
    where
        P: LobbyPlayerPort,
        M: PendingMatchPort,
        C: LobbyClock,
    {
        let scope = LobbyScope::new(GuildId(guild_id), kind);
        let Some(snapshot) = lobby.get_lobby(scope) else {
            return Vec::new();
        };
        if snapshot.players.is_empty() {
            return Vec::new();
        }
        let discord_ids: Vec<i64> = snapshot.players.iter().map(|player| player.0).collect();
        let Ok(windows_by_player) = self.repository.list_for_players(&discord_ids, guild_id) else {
            return Vec::new();
        };
        if windows_by_player.is_empty() {
            return Vec::new();
        }

        let mut due: BTreeMap<i64, CurfewWindow> = BTreeMap::new();
        for (discord_id, windows) in &windows_by_player {
            let general_timezone = self
                .repository
                .general_timezone(*discord_id, guild_id)
                .unwrap_or(None);
            let Some(window) = find_active_window(windows, general_timezone.as_deref(), now) else {
                continue;
            };
            if window.mode.asks_for_confirmation() {
                let tz = effective_timezone(window, general_timezone.as_deref());
                let coverage_date = local_date_string(tz, now);
                let covered = self
                    .repository
                    .is_covered(*discord_id, guild_id, &window.name, &coverage_date)
                    .unwrap_or(false);
                if covered {
                    continue;
                }
            }
            due.insert(*discord_id, window.clone());
        }
        if due.is_empty() {
            return Vec::new();
        }

        let player_ids = due.keys().map(|discord_id| UserId(*discord_id)).collect();
        let removed = lobby.remove_players_from_lobby(&player_ids, scope);
        removed
            .into_iter()
            .filter_map(|user_id| {
                due.get(&user_id.0).map(|window| CurfewKick {
                    discord_id: user_id.0,
                    guild_id,
                    lobby_kind: kind,
                    window_name: window.name.clone(),
                    mode: window.mode,
                })
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "curfew_service/tests.rs"]
mod tests;
