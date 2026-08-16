//! General per-player timezone preference.
//!
//! Kept separate from `curfew` so it's not curfew-specific: curfew falls back
//! to this when no window-specific override is set, and any other time-based
//! feature can read the same preference without depending on curfew. Ports
//! `utils/timezone.py`.

use std::str::FromStr;

use chrono_tz::Tz;

pub const DEFAULT_TIMEZONE: &str = "America/New_York";

/// A curated subset of the ~500 valid IANA names, for `/player timezone list`.
/// `/player timezone set` itself accepts any name `is_valid_timezone` allows,
/// not just these — this is just a friendlier menu than making players guess
/// exact IANA spelling.
pub const COMMON_TIMEZONES: &[(&str, &[(&str, &str)])] = &[
    (
        "US",
        &[
            ("America/New_York", "Eastern"),
            ("America/Chicago", "Central"),
            ("America/Denver", "Mountain"),
            ("America/Phoenix", "Mountain, no DST (Arizona)"),
            ("America/Los_Angeles", "Pacific"),
            ("America/Anchorage", "Alaska"),
            ("Pacific/Honolulu", "Hawaii"),
        ],
    ),
    (
        "Americas (other)",
        &[
            ("America/Toronto", "Canada East"),
            ("America/Vancouver", "Canada West"),
            ("America/Mexico_City", "Mexico"),
            ("America/Sao_Paulo", "Brazil"),
        ],
    ),
    (
        "Europe",
        &[
            ("Europe/London", "UK"),
            ("Europe/Paris", "Central Europe"),
            ("Europe/Berlin", "Central Europe"),
            ("Europe/Moscow", "Russia"),
        ],
    ),
    (
        "Asia / Pacific",
        &[
            ("Asia/Tokyo", "Japan"),
            ("Asia/Shanghai", "China"),
            ("Asia/Kolkata", "India"),
            ("Asia/Dubai", "UAE"),
            ("Australia/Sydney", "Australia East"),
        ],
    ),
];

/// Return whether `timezone` is a real IANA zone name.
#[must_use]
pub fn is_valid_timezone(timezone: &str) -> bool {
    Tz::from_str(timezone).is_ok()
}

/// Render `COMMON_TIMEZONES` as a Discord message for `/player timezone list`.
#[must_use]
pub fn format_common_timezones() -> String {
    let sections: Vec<String> = COMMON_TIMEZONES
        .iter()
        .map(|(region, zones)| {
            let lines: Vec<String> = zones
                .iter()
                .map(|(name, label)| format!("`{name}` — {label}"))
                .collect();
            format!("**{region}**\n{}", lines.join("\n"))
        })
        .collect();
    let body = sections.join("\n\n");
    format!(
        "{body}\n\nAny IANA timezone name works, not just these — use `/player timezone set <name>`."
    )
}

#[cfg(test)]
#[path = "timezone/tests.rs"]
mod tests;
