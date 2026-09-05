//! Named curfew windows: block queueing and auto-remove from any lobby during
//! a player-chosen daily window. A player can have any number of these under
//! different names — the name and what it's for are entirely up to them.
//!
//! Kept pure here (no database/Discord dependency) so the window math —
//! including the overnight wraparound and DST-safe timezone conversion — is
//! unit-testable in isolation. Ports `utils/curfew.py`.

use std::str::FromStr;

use chrono::{DateTime, Datelike, LocalResult, TimeZone, Timelike, Utc, Weekday};
use chrono_tz::Tz;

pub const DEFAULT_TIMEZONE: &str = "America/New_York";

/// Local hour at which a staged strict-mode edit or delete takes effect on
/// the following day — see [`next_local_morning`].
pub const STAGED_CHANGE_APPLY_HOUR: u32 = 8;

/// How a window is enforced once it's active.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CurfewMode {
    /// Blocks joining and sweeps the player out of any lobby, immediately.
    #[default]
    Default,
    /// Same enforcement as `Default`, but an edit that *reduces* this window
    /// (see [`retains_coverage`]), switches it to a non-staging mode, or
    /// deletes it never takes effect the same calendar day it's made — it's
    /// staged and applied at the window's next local morning
    /// ([`STAGED_CHANGE_APPLY_HOUR`]) instead. Extending the window applies
    /// immediately. This closes the "loosen it right before it fires
    /// tonight" bypass: whatever was committed as of this morning is what
    /// still fires tonight.
    Strict,
    /// Never blocks or sweeps on its own. Joining while the window is active
    /// tells the player about the curfew and asks for a yes/no confirmation
    /// first; a yes covers them until their next completed (non-aborted)
    /// match, or until the day rolls over, whichever comes first.
    Informational,
}

impl CurfewMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Strict => "strict",
            Self::Informational => "informational",
        }
    }

    /// Whether a reducing edit or a delete of a window in this mode is
    /// staged to the next local morning instead of applying immediately.
    #[must_use]
    pub const fn stages_changes(self) -> bool {
        matches!(self, Self::Strict)
    }

    /// Whether an active window in this mode lets the player queue after an
    /// explicit confirmation (rather than blocking them outright).
    #[must_use]
    pub const fn asks_for_confirmation(self) -> bool {
        matches!(self, Self::Informational)
    }
}

/// Parse a `/player curfew` mode argument. Blank or absent input is the
/// caller's job to map to [`CurfewMode::default`] — this only validates text
/// that was actually given.
pub fn parse_mode(text: &str) -> Result<CurfewMode, String> {
    match text.trim().to_lowercase().as_str() {
        "default" => Ok(CurfewMode::Default),
        "strict" => Ok(CurfewMode::Strict),
        "informational" | "info" => Ok(CurfewMode::Informational),
        other => Err(format!(
            "Unknown curfew mode '{other}'. Use default, strict, or informational."
        )),
    }
}

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
    pub mode: CurfewMode,
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

/// The UTC instant of [`STAGED_CHANGE_APPLY_HOUR`] local time on the
/// calendar day after `now`, in `timezone`. Used to compute when a staged
/// strict-mode edit or delete takes effect: never today, always the next
/// morning, in whichever timezone the *currently enforced* version of the
/// window runs under. Always the next calendar day, even for a change made
/// before that hour today, so an early-morning edit can't loosen tonight's
/// window either.
#[must_use]
pub fn next_local_morning(timezone: &str, now: DateTime<Utc>) -> DateTime<Utc> {
    let tz = resolve_tz(timezone);
    let local_now = now.with_timezone(&tz);
    let next_date = local_now
        .date_naive()
        .succ_opt()
        .unwrap_or_else(|| local_now.date_naive());
    let morning = next_date
        .and_hms_opt(STAGED_CHANGE_APPLY_HOUR, 0, 0)
        .expect("the apply hour is a valid time of day");
    match tz.from_local_datetime(&morning) {
        LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => dt.with_timezone(&Utc),
        LocalResult::None => tz.from_utc_datetime(&morning).with_timezone(&Utc),
    }
}

/// The player's local calendar date for `now`, in `timezone` — used to key
/// informational-mode's per-day confirmation coverage.
#[must_use]
pub fn local_date_string(timezone: &str, now: DateTime<Utc>) -> String {
    let tz = resolve_tz(timezone);
    now.with_timezone(&tz).date_naive().to_string()
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
    is_active_at_local(
        window,
        moment.weekday(),
        moment.hour() * 60 + moment.minute(),
    )
}

/// The pure wall-clock core of [`is_within_window`]: whether `window` is
/// active at `minutes_now` (minutes since local midnight) on local weekday
/// `today`. Timezone-free, so it's also what [`retains_coverage`] compares.
fn is_active_at_local(window: &CurfewWindow, today: Weekday, minutes_now: u32) -> bool {
    let start_minutes = window.start_hour * 60 + window.start_minute;
    let end_minutes = window.end_hour * 60 + window.end_minute;

    if start_minutes == end_minutes {
        return false;
    }
    let day_selected = |day: Weekday| window.days.is_none_or(|mask| mask & weekday_bit(day) != 0);
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

/// How far ahead [`retains_coverage`] samples when the two windows run in
/// different timezones. Two weeks covers every weekday selection and the
/// DST transition nearest to `now`.
const COVERAGE_SAMPLE_DAYS: i64 = 14;

/// Whether every minute `old` would enforce is also enforced by `new`. This
/// is what decides whether an edit to a strict-mode window is an
/// *extension* (applies immediately) or a *reduction* (staged to the next
/// morning): a reduction is any edit that frees up a minute the committed
/// window would have curfewed — a later start, an earlier end, a dropped
/// day, or a timezone shift that moves the span.
///
/// When both windows run in the same timezone — every edit except a
/// timezone change — this is an exact wall-clock comparison over one week.
/// Only a timezone change needs the two-week UTC sampling, since that's the
/// one case where the same local minute means different instants.
#[must_use]
pub fn retains_coverage(
    new: &CurfewWindow,
    old: &CurfewWindow,
    general_timezone: Option<&str>,
    now: DateTime<Utc>,
) -> bool {
    if effective_timezone(new, general_timezone) == effective_timezone(old, general_timezone) {
        return WEEKDAY_ABBREVIATIONS.iter().all(|(day, _)| {
            (0..24 * 60).all(|minute| {
                !is_active_at_local(old, *day, minute) || is_active_at_local(new, *day, minute)
            })
        });
    }
    let start = now
        .with_second(0)
        .and_then(|moment| moment.with_nanosecond(0))
        .unwrap_or(now);
    let minutes = COVERAGE_SAMPLE_DAYS * 24 * 60;
    (0..minutes)
        .map(|offset| start + chrono::Duration::minutes(offset))
        .all(|moment| {
            !is_within_window(old, general_timezone, moment)
                || is_within_window(new, general_timezone, moment)
        })
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
        // For an overnight span the selected day is the day it *starts*, and
        // it runs into the following morning. Plain "on Sat" reads as "all day
        // Saturday", so name the rule where it actually applies.
        let start_minutes = window.start_hour * 60 + window.start_minute;
        let end_minutes = window.end_hour * 60 + window.end_minute;
        if start_minutes > end_minutes {
            rendered.push_str(&format!(" starting {days} (runs into the next morning)"));
        } else {
            rendered.push_str(&format!(" on {days}"));
        }
    }
    match window.mode {
        CurfewMode::Default => {}
        CurfewMode::Strict => {
            rendered.push_str(" [strict: edits/removal take effect the next morning]");
        }
        CurfewMode::Informational => {
            rendered.push_str(" [informational: asks before letting you queue]");
        }
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
