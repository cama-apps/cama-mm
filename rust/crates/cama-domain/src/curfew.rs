//! Named curfew windows: block queueing and auto-remove from any lobby during
//! a player-chosen daily window. A player can have any number of these under
//! different names — the name and what it's for are entirely up to them.
//!
//! Kept pure here (no database/Discord dependency) so the window math —
//! including the overnight wraparound and DST-safe timezone conversion — is
//! unit-testable in isolation. Ports `utils/curfew.py`.

use std::str::FromStr;

use chrono::{DateTime, Datelike, Timelike, Utc, Weekday};
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
    /// Which days of the week this window applies to, as a bitmask (bit 0 =
    /// Monday ... bit 6 = Sunday, see [`weekday_bit`]). `None` — the default,
    /// and what every window had before this field existed — means every
    /// day. For an overnight span the day picked is the *start* day: e.g.
    /// selecting Friday on a 22:00-06:00 window covers Friday night through
    /// Saturday morning, not Saturday night.
    pub days: Option<u8>,
}

/// The bitmask flag for a single weekday (Monday = bit 0 ... Sunday = bit 6).
#[must_use]
pub fn weekday_bit(day: Weekday) -> u8 {
    1 << day.num_days_from_monday()
}

const WEEKDAY_ABBREVIATIONS: [(Weekday, &str); 7] = [
    (Weekday::Mon, "Mon"),
    (Weekday::Tue, "Tue"),
    (Weekday::Wed, "Wed"),
    (Weekday::Thu, "Thu"),
    (Weekday::Fri, "Fri"),
    (Weekday::Sat, "Sat"),
    (Weekday::Sun, "Sun"),
];

/// Parse a comma/space-separated list of day tokens (case-insensitive) into a
/// weekday bitmask. Accepts the short forms M/T/W/Th/F/Sa/Su as well as
/// ordinary 3-letter and full weekday names. Returns an error naming the bad
/// token, or if no day was given at all.
pub fn parse_weekdays(text: &str) -> Result<u8, String> {
    let mut mask = 0u8;
    for token in text
        .split([',', ' '])
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        let day = match token.to_lowercase().as_str() {
            "m" | "mon" | "monday" => Weekday::Mon,
            "t" | "tu" | "tue" | "tues" | "tuesday" => Weekday::Tue,
            "w" | "wed" | "weds" | "wednesday" => Weekday::Wed,
            "th" | "thu" | "thur" | "thurs" | "thursday" => Weekday::Thu,
            "f" | "fri" | "friday" => Weekday::Fri,
            "sa" | "sat" | "saturday" => Weekday::Sat,
            "su" | "sun" | "sunday" => Weekday::Sun,
            other => {
                return Err(format!(
                    "Unknown day '{other}'. Use M, T, W, Th, F, Sa, Su (or full names)."
                ));
            }
        };
        mask |= weekday_bit(day);
    }
    if mask == 0 {
        return Err("Give at least one day, e.g. 'Sa,Su'.".to_owned());
    }
    Ok(mask)
}

/// Render a day-of-week bitmask for display, e.g. `"Mon, Wed, Fri"`. Returns
/// `None` for an unset (every-day) mask so callers can omit the clause.
#[must_use]
pub fn format_days(days: Option<u8>) -> Option<String> {
    let mask = days?;
    let names: Vec<&str> = WEEKDAY_ABBREVIATIONS
        .iter()
        .filter(|(day, _)| mask & weekday_bit(*day) != 0)
        .map(|(_, name)| *name)
        .collect();
    (!names.is_empty()).then(|| names.join(", "))
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
    let day_selected = |day: Weekday| window.days.is_none_or(|mask| mask & weekday_bit(day) != 0);
    let today = moment.weekday();
    if start_minutes < end_minutes {
        // Same-day span, e.g. 09:00-17:00.
        day_selected(today) && start_minutes <= minutes_now && minutes_now < end_minutes
    } else {
        // Overnight span, e.g. 22:00-06:00. The day picked is the *start*
        // day: it covers from start_minutes through midnight, and its
        // early-morning tail spills into the following calendar day.
        (day_selected(today) && minutes_now >= start_minutes)
            || (day_selected(today.pred()) && minutes_now < end_minutes)
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
    let mut rendered = format!("\"{}\": {start} - {end} {tz_name}", window.name);
    if let Some(days) = format_days(window.days) {
        rendered.push_str(&format!(" on {days}"));
    }
    rendered
}

/// Return whether `timezone` is a real IANA zone name.
#[must_use]
pub fn is_valid_timezone(timezone: &str) -> bool {
    Tz::from_str(timezone).is_ok()
}

#[cfg(test)]
#[path = "curfew/tests.rs"]
mod tests;
