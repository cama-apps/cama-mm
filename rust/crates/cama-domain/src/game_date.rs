//! Shared game-date policy.
//!
//! Game days roll over at 4 AM in a deliberately fixed UTC-8 offset. This is
//! not a daylight-saving-aware Pacific time zone: keeping every game day at
//! exactly 86,400 seconds prevents streak calculations from drifting.

use std::borrow::Borrow;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::SystemTime;

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, TimeDelta, TimeZone, Utc};

/// The fixed number of seconds east of UTC used by the game-date convention.
pub const PST_OFFSET_SECONDS: i32 = -8 * 60 * 60;

const GAME_DAY_ROLLOVER_HOURS: i64 = 4;
const DATE_FORMAT: &str = "%Y-%m-%d";

/// Errors raised while parsing a game date or converting a Unix timestamp.
#[derive(Debug)]
pub enum GameDateError {
    /// The supplied game-date string was not a valid `YYYY-MM-DD` date.
    InvalidDate(chrono::ParseError),
    /// The supplied timestamp is not finite or is outside Chrono's range.
    TimestampOutOfRange,
}

impl Display for GameDateError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDate(error) => write!(formatter, "invalid game date: {error}"),
            Self::TimestampOutOfRange => {
                formatter.write_str("timestamp is outside the supported range")
            }
        }
    }
}

impl Error for GameDateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidDate(error) => Some(error),
            Self::TimestampOutOfRange => None,
        }
    }
}

impl From<chrono::ParseError> for GameDateError {
    fn from(error: chrono::ParseError) -> Self {
        Self::InvalidDate(error)
    }
}

/// Return the fixed UTC-8 offset used by all game-date calculations.
#[must_use]
pub fn fixed_pst() -> FixedOffset {
    FixedOffset::east_opt(PST_OFFSET_SECONDS).expect("the fixed PST offset is valid")
}

/// A date-time value that can participate in game-date calculations.
///
/// Zoned values retain their represented instant. Naive values are treated as
/// UTC, matching the Python helper's behavior.
pub trait GameDateInput {
    /// Convert this value to the fixed UTC-8 game-date offset.
    fn into_fixed_pst(self) -> DateTime<FixedOffset>;
}

impl<Tz> GameDateInput for DateTime<Tz>
where
    Tz: TimeZone,
{
    fn into_fixed_pst(self) -> DateTime<FixedOffset> {
        self.with_timezone(&fixed_pst())
    }
}

impl GameDateInput for NaiveDateTime {
    fn into_fixed_pst(self) -> DateTime<FixedOffset> {
        self.and_utc().with_timezone(&fixed_pst())
    }
}

/// Convert a date-time into its `YYYY-MM-DD` game-date string.
///
/// The date changes at 4 AM in the fixed UTC-8 offset. Naive date-times are
/// interpreted as UTC.
#[must_use]
pub fn game_date_for(dt: impl GameDateInput) -> String {
    (dt.into_fixed_pst() - TimeDelta::hours(GAME_DAY_ROLLOVER_HOURS))
        .format(DATE_FORMAT)
        .to_string()
}

/// Return the current `YYYY-MM-DD` game date.
#[must_use]
pub fn get_game_date() -> String {
    game_date_for(DateTime::<Utc>::from(SystemTime::now()))
}

/// Given a `YYYY-MM-DD` game date, return the previous day's string.
pub fn yesterday_of(today: &str) -> Result<String, GameDateError> {
    let today = NaiveDate::parse_from_str(today, DATE_FORMAT)?;
    Ok((today - TimeDelta::days(1)).format(DATE_FORMAT).to_string())
}

/// Return the game-date string for a Unix timestamp in seconds.
pub fn game_date_for_timestamp(timestamp: f64) -> Result<String, GameDateError> {
    timestamp_to_utc(timestamp)
        .map(game_date_for)
        .ok_or(GameDateError::TimestampOutOfRange)
}

/// Return the Unix timestamp of 4 AM fixed-PST on `timestamp`'s game date.
pub fn game_day_start_ts(timestamp: i64) -> Result<i64, GameDateError> {
    let utc =
        DateTime::<Utc>::from_timestamp(timestamp, 0).ok_or(GameDateError::TimestampOutOfRange)?;
    let game_date =
        (utc.with_timezone(&fixed_pst()) - TimeDelta::hours(GAME_DAY_ROLLOVER_HOURS)).date_naive();
    let local_start = game_date
        .and_hms_opt(GAME_DAY_ROLLOVER_HOURS as u32, 0, 0)
        .expect("4 AM is a valid wall-clock time");
    let start = fixed_pst()
        .from_local_datetime(&local_start)
        .single()
        .expect("a fixed offset has one unambiguous local time");
    Ok(start.timestamp())
}

/// Return the weekday for a game-date string (`Monday = 0`, `Sunday = 6`).
pub fn weekday_of_game_date(date: &str) -> Result<u32, GameDateError> {
    Ok(NaiveDate::parse_from_str(date, DATE_FORMAT)?
        .weekday()
        .num_days_from_monday())
}

/// Look up a streak bonus, selecting the largest threshold at or below the streak.
#[must_use]
pub fn streak_bonus_for<I, K, V>(streak_days: i64, schedule: I) -> i64
where
    I: IntoIterator<Item = (K, V)>,
    K: Borrow<i64>,
    V: Borrow<i64>,
{
    schedule
        .into_iter()
        .filter_map(|(threshold, bonus)| {
            let threshold = *threshold.borrow();
            (streak_days >= threshold).then_some((threshold, *bonus.borrow()))
        })
        .max_by_key(|(threshold, _bonus)| *threshold)
        .map_or(0, |(_threshold, bonus)| bonus)
}

fn timestamp_to_utc(timestamp: f64) -> Option<DateTime<Utc>> {
    if !timestamp.is_finite() {
        return None;
    }

    let whole_seconds = timestamp.floor();
    if whole_seconds < i64::MIN as f64 || whole_seconds > i64::MAX as f64 {
        return None;
    }

    let mut seconds = whole_seconds as i64;
    let mut nanoseconds = ((timestamp - whole_seconds) * 1_000_000_000.0).round() as u32;
    if nanoseconds == 1_000_000_000 {
        seconds = seconds.checked_add(1)?;
        nanoseconds = 0;
    }

    DateTime::<Utc>::from_timestamp(seconds, nanoseconds)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::{fixed_pst, game_date_for};

    #[test]
    fn test_game_date_boundary_is_dst_stable_across_march_transition() {
        let pst = fixed_pst();
        let spring_forward = pst
            .with_ymd_and_hms(2026, 3, 8, 4, 0, 0)
            .single()
            .expect("test date is valid");
        let next_day = pst
            .with_ymd_and_hms(2026, 3, 9, 4, 0, 0)
            .single()
            .expect("test date is valid");

        assert_eq!(next_day.timestamp() - spring_forward.timestamp(), 86_400);
    }

    #[test]
    fn test_game_date_for_rolls_over_at_4am_pst() {
        let pst = fixed_pst();
        let before = pst
            .with_ymd_and_hms(2026, 5, 20, 3, 59, 0)
            .single()
            .expect("test date is valid");
        let after = pst
            .with_ymd_and_hms(2026, 5, 20, 4, 1, 0)
            .single()
            .expect("test date is valid");

        assert_eq!(game_date_for(before), "2026-05-19");
        assert_eq!(game_date_for(after), "2026-05-20");
    }
}
