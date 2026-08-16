//! Named curfew windows: block queueing and auto-remove from any lobby during
//! a player-chosen daily window. A player can have any number of these under
//! different names — the name and what it's for are entirely up to them.
//!
//! Kept pure here (no database/Discord dependency) so the window math —
//! including the overnight wraparound and DST-safe timezone conversion — is
//! unit-testable in isolation. Ports `utils/curfew.py`.

use std::str::FromStr;

use chrono::{DateTime, Timelike, Utc};
use chrono_tz::Tz;

pub const DEFAULT_TIMEZONE: &str = "America/New_York";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CurfewWindow {
    pub discord_id: i64,
    pub guild_id: i64,
    pub name: String,
    pub start_hour: u32,
    pub start_minute: u32,
    pub end_hour: u32,
    pub end_minute: u32,
    /// Overrides the player's general timezone if set.
    pub timezone: Option<String>,
}

/// Parse a 24-hour "HH:MM" string into (hour, minute). Returns `Err` on bad format.
pub fn parse_clock(text: &str) -> Result<(u32, u32), String> {
    let text = text.trim();
    let (hour_text, minute_text) = text
        .split_once(':')
        .ok_or_else(|| format!("Invalid time '{text}'. Use 24-hour HH:MM, e.g. 22:00."))?;
    let invalid = || format!("Invalid time '{text}'. Use 24-hour HH:MM, e.g. 22:00.");
    if hour_text.is_empty() || hour_text.len() > 2 || !hour_text.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(invalid());
    }
    if minute_text.len() != 2 || !minute_text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let hour: u32 = hour_text.parse().map_err(|_| invalid())?;
    let minute: u32 = minute_text.parse().map_err(|_| invalid())?;
    if hour > 23 || minute > 59 {
        return Err(invalid());
    }
    Ok((hour, minute))
}

/// Resolve the timezone to interpret a window in.
///
/// Prefers the window's own timezone, falls back to the player's general
/// `/player timezone` setting, then to the hardcoded default.
#[must_use]
pub fn effective_timezone<'a>(
    window: &'a CurfewWindow,
    general_timezone: Option<&'a str>,
) -> &'a str {
    window
        .timezone
        .as_deref()
        .or(general_timezone)
        .unwrap_or(DEFAULT_TIMEZONE)
}

fn resolve_tz(name: &str) -> Tz {
    Tz::from_str(name).unwrap_or(chrono_tz::America::New_York)
}

/// Return whether `now` falls inside `window`'s daily start-end span.
///
/// A window where start == end is treated as zero-length (never active)
/// rather than "all day" — that's almost certainly a misconfiguration.
#[must_use]
pub fn is_within_window(
    window: &CurfewWindow,
    general_timezone: Option<&str>,
    now: DateTime<Utc>,
) -> bool {
    let tz = resolve_tz(effective_timezone(window, general_timezone));
    let moment = now.with_timezone(&tz);
    let minutes_now = moment.hour() * 60 + moment.minute();
    let start_minutes = window.start_hour * 60 + window.start_minute;
    let end_minutes = window.end_hour * 60 + window.end_minute;

    if start_minutes == end_minutes {
        return false;
    }
    if start_minutes < end_minutes {
        // Same-day span, e.g. 09:00-17:00.
        start_minutes <= minutes_now && minutes_now < end_minutes
    } else {
        // Overnight span, e.g. 22:00-06:00.
        minutes_now >= start_minutes || minutes_now < end_minutes
    }
}

/// Return the first (by name) of `windows` that's currently active, or `None`.
#[must_use]
pub fn find_active_window<'a>(
    windows: &'a [CurfewWindow],
    general_timezone: Option<&str>,
    now: DateTime<Utc>,
) -> Option<&'a CurfewWindow> {
    let mut ordered: Vec<&CurfewWindow> = windows.iter().collect();
    ordered.sort_by(|a, b| a.name.cmp(&b.name));
    ordered
        .into_iter()
        .find(|window| is_within_window(window, general_timezone, now))
}

fn format_clock(hour: u32, minute: u32) -> String {
    let period = if hour < 12 { "AM" } else { "PM" };
    let display_hour = match hour % 12 {
        0 => 12,
        other => other,
    };
    format!("{display_hour}:{minute:02} {period}")
}

/// Render a window for user-facing messages, e.g. `"work": 9:00 AM - 5:00 PM America/New_York`.
#[must_use]
pub fn format_window(window: &CurfewWindow, general_timezone: Option<&str>) -> String {
    let start = format_clock(window.start_hour, window.start_minute);
    let end = format_clock(window.end_hour, window.end_minute);
    let tz_name = effective_timezone(window, general_timezone);
    format!("\"{}\": {start} - {end} {tz_name}", window.name)
}

/// Return whether `timezone` is a real IANA zone name.
#[must_use]
pub fn is_valid_timezone(timezone: &str) -> bool {
    Tz::from_str(timezone).is_ok()
}

#[cfg(test)]
#[path = "curfew/tests.rs"]
mod tests;
