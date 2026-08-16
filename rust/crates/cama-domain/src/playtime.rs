//! Personal dota play-time preference: purely informational, never enforced.
//!
//! Unlike curfew, this never blocks or removes anyone from a lobby — it just
//! lets players record which hours they'd like to play so `/player playtime
//! popular` can report the group's most desired hours. Each player's hours
//! are interpreted in their own timezone (their general timezone setting, or
//! the default) and converted into one reporting timezone before being
//! counted, so the aggregate is an honest cross-timezone comparison. Ports
//! `utils/playtime.py`.

use std::collections::BTreeSet;
use std::str::FromStr;

use chrono::{NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

use crate::timezone::DEFAULT_TIMEZONE;

/// Minimal view of a player needed to place them in the aggregate: their
/// informational play hours (in their own local time) and their general
/// timezone setting, if any.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PlayHoursPlayer {
    pub dota_play_hours: Option<Vec<u32>>,
    pub timezone: Option<String>,
}

/// Parse a comma-separated list of hours and/or hour ranges into a set of
/// hours (0-23).
///
/// Accepts single hours ("14"), ranges ("18-23"; wrapping past midnight is
/// fine, e.g. "22-2"), or any comma-separated mix ("14,18-23,2").
pub fn parse_hour_set(text: &str) -> Result<BTreeSet<u32>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Give at least one hour, e.g. '18-23' or '14,20,21'.".to_owned());
    }
    let mut hours = BTreeSet::new();
    for raw_token in text.split(',') {
        let token = raw_token.trim();
        let invalid = || {
            format!("Invalid hour or range '{token}'. Use hours 0-23, e.g. '18-23' or '14,20,21'.")
        };
        let is_digits = |segment: &str| {
            !segment.is_empty() && segment.len() <= 2 && segment.bytes().all(|b| b.is_ascii_digit())
        };
        let (start_text, end_text) = match token.split_once('-') {
            Some((start, end)) => (start, Some(end)),
            None => (token, None),
        };
        if !is_digits(start_text) {
            return Err(invalid());
        }
        let start: u32 = start_text.parse().map_err(|_| invalid())?;
        let end = match end_text {
            Some(end_text) => {
                if !is_digits(end_text) {
                    return Err(invalid());
                }
                end_text.parse::<u32>().map_err(|_| invalid())?
            }
            None => start,
        };
        if start > 23 || end > 23 {
            return Err(format!("Hours must be between 0 and 23 (got '{token}')."));
        }
        let mut hour = start;
        hours.insert(hour);
        while hour != end {
            hour = (hour + 1) % 24;
            hours.insert(hour);
        }
    }
    Ok(hours)
}

/// Render a set of hours as compact ascending ranges, e.g. `{18,19,20,23}` ->
/// `"18:00-20:00, 23:00"`.
#[must_use]
pub fn format_hour_ranges(hours: &BTreeSet<u32>) -> String {
    if hours.is_empty() {
        return "none".to_owned();
    }
    let ordered: Vec<u32> = hours.iter().copied().collect();
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    let mut start = ordered[0];
    let mut prev = ordered[0];
    for &hour in &ordered[1..] {
        if hour == prev + 1 {
            prev = hour;
            continue;
        }
        ranges.push((start, prev));
        start = hour;
        prev = hour;
    }
    ranges.push((start, prev));
    ranges
        .iter()
        .map(|&(s, e)| {
            if s == e {
                format!("{s:02}:00")
            } else {
                format!("{s:02}:00-{e:02}:00")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The timezone to interpret a player's play-time hours in: their general
/// setting, else the default.
#[must_use]
pub fn effective_timezone(player: &PlayHoursPlayer) -> &str {
    player.timezone.as_deref().unwrap_or(DEFAULT_TIMEZONE)
}

fn resolve_tz(name: &str) -> Tz {
    Tz::from_str(name).unwrap_or(chrono_tz::America::New_York)
}

fn resolve_local(tz: Tz, naive: chrono::NaiveDateTime) -> chrono::DateTime<Tz> {
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => dt,
        chrono::LocalResult::Ambiguous(earliest, _latest) => earliest,
        chrono::LocalResult::None => tz.from_utc_datetime(&naive),
    }
}

/// Count, per hour (0-23) in `reporting_timezone`, how many players want to
/// play then.
#[must_use]
pub fn aggregate_popular_hours(
    players: &[PlayHoursPlayer],
    reporting_timezone: &str,
    today: Option<NaiveDate>,
) -> [u32; 24] {
    let report_tz = resolve_tz(reporting_timezone);
    let reference_date = today.unwrap_or_else(|| Utc::now().with_timezone(&report_tz).date_naive());

    let mut counts = [0u32; 24];
    for player in players {
        let Some(hours) = &player.dota_play_hours else {
            continue;
        };
        if hours.is_empty() {
            continue;
        }
        let player_tz = resolve_tz(effective_timezone(player));
        for &hour in hours {
            let naive = reference_date.and_hms_opt(hour, 0, 0).unwrap_or_else(|| {
                reference_date
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight is valid")
            });
            let local = resolve_local(player_tz, naive);
            let converted = local.with_timezone(&report_tz);
            counts[converted.hour() as usize] += 1;
        }
    }
    counts
}

#[cfg(test)]
#[path = "playtime/tests.rs"]
mod tests;
