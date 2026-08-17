use super::*;
use chrono::TimeZone;
use std::str::FromStr;

fn window(
    name: &str,
    start_hour: u32,
    start_minute: u32,
    end_hour: u32,
    end_minute: u32,
    timezone: Option<&str>,
) -> CurfewWindow {
    CurfewWindow {
        discord_id: 1,
        guild_id: 0,
        name: name.to_owned(),
        start_hour,
        start_minute,
        end_hour,
        end_minute,
        timezone: timezone.map(str::to_owned),
        days: None,
    }
}

fn window_on_days(
    name: &str,
    start_hour: u32,
    start_minute: u32,
    end_hour: u32,
    end_minute: u32,
    days: u8,
) -> CurfewWindow {
    CurfewWindow {
        days: Some(days),
        ..window(name, start_hour, start_minute, end_hour, end_minute, None)
    }
}

fn ny(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    chrono_tz::America::New_York
        .with_ymd_and_hms(y, mo, d, h, mi, 0)
        .unwrap()
        .with_timezone(&Utc)
}

mod is_within_window {
    use super::*;

    #[test]
    fn test_overnight_window_blocks_after_start() {
        let w = window("w", 22, 0, 6, 0, None);
        assert!(is_within_window(&w, None, ny(2026, 1, 1, 23, 0)));
    }

    #[test]
    fn test_overnight_window_blocks_before_end() {
        let w = window("w", 22, 0, 6, 0, None);
        assert!(is_within_window(&w, None, ny(2026, 1, 2, 3, 0)));
    }

    #[test]
    fn test_overnight_window_allows_after_end() {
        let w = window("w", 22, 0, 6, 0, None);
        assert!(!is_within_window(&w, None, ny(2026, 1, 2, 7, 0)));
    }

    #[test]
    fn test_overnight_window_allows_before_start() {
        let w = window("w", 22, 0, 6, 0, None);
        assert!(!is_within_window(&w, None, ny(2026, 1, 1, 21, 0)));
    }

    #[test]
    fn test_same_day_window() {
        let w = window("w", 9, 0, 17, 0, None);
        assert!(is_within_window(&w, None, ny(2026, 1, 1, 12, 0)));
        assert!(!is_within_window(&w, None, ny(2026, 1, 1, 20, 0)));
    }

    #[test]
    fn test_equal_start_and_end_never_blocks() {
        let w = window("w", 22, 0, 22, 0, None);
        assert!(!is_within_window(&w, None, ny(2026, 1, 1, 22, 0)));
    }

    #[test]
    fn test_window_timezone_overrides_general_timezone() {
        let w = window("w", 22, 0, 6, 0, Some("America/New_York"));
        // 10:30pm EST (winter, no DST) is 3:30am UTC the next day.
        let utc_now = Utc.with_ymd_and_hms(2026, 1, 2, 3, 30, 0).unwrap();
        assert!(is_within_window(&w, Some("Asia/Tokyo"), utc_now));
    }

    #[test]
    fn test_falls_back_to_general_timezone_when_window_has_none() {
        let w = window("w", 22, 0, 6, 0, None);
        // 10:30pm JST is 1:30pm UTC.
        let utc_now = Utc.with_ymd_and_hms(2026, 1, 1, 13, 30, 0).unwrap();
        assert!(is_within_window(&w, Some("Asia/Tokyo"), utc_now));
    }

    #[test]
    fn test_falls_back_to_default_when_both_unset() {
        let w = window("w", 22, 0, 6, 0, None);
        let moment = chrono_tz::Tz::from_str(DEFAULT_TIMEZONE)
            .unwrap()
            .with_ymd_and_hms(2026, 1, 1, 22, 30, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert!(is_within_window(&w, None, moment));
    }

    #[test]
    fn test_unknown_timezone_falls_back_to_default() {
        let w = window("w", 22, 0, 6, 0, Some("Not/AZone"));
        let moment = chrono_tz::Tz::from_str(DEFAULT_TIMEZONE)
            .unwrap()
            .with_ymd_and_hms(2026, 1, 1, 22, 30, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert!(is_within_window(&w, None, moment));
    }
}

mod day_of_week {
    use super::*;

    // 2026-01-01 is a Thursday, 01-02 a Friday, 01-03 a Saturday, 01-04 a Sunday.

    #[test]
    fn test_same_day_span_active_on_selected_day() {
        let w = window_on_days("work", 9, 0, 17, 0, weekday_bit(Weekday::Fri));
        assert!(is_within_window(&w, None, ny(2026, 1, 2, 12, 0)));
    }

    #[test]
    fn test_same_day_span_inactive_on_unselected_day() {
        let w = window_on_days("work", 9, 0, 17, 0, weekday_bit(Weekday::Fri));
        assert!(!is_within_window(&w, None, ny(2026, 1, 3, 12, 0)));
    }

    #[test]
    fn test_overnight_span_active_from_start_day_evening() {
        // Friday 22:00-06:00, picking only Friday.
        let w = window_on_days("sleep", 22, 0, 6, 0, weekday_bit(Weekday::Fri));
        assert!(is_within_window(&w, None, ny(2026, 1, 2, 23, 0)));
    }

    #[test]
    fn test_overnight_span_active_into_next_calendar_day_morning() {
        // The Friday-picked window's early-morning tail lands on Saturday.
        let w = window_on_days("sleep", 22, 0, 6, 0, weekday_bit(Weekday::Fri));
        assert!(is_within_window(&w, None, ny(2026, 1, 3, 3, 0)));
    }

    #[test]
    fn test_overnight_span_inactive_the_evening_before_start_day() {
        let w = window_on_days("sleep", 22, 0, 6, 0, weekday_bit(Weekday::Fri));
        assert!(!is_within_window(&w, None, ny(2026, 1, 1, 23, 0)));
    }

    #[test]
    fn test_overnight_span_inactive_the_evening_after_the_tail_day() {
        // Saturday night is not covered by a window picked for Friday.
        let w = window_on_days("sleep", 22, 0, 6, 0, weekday_bit(Weekday::Fri));
        assert!(!is_within_window(&w, None, ny(2026, 1, 3, 23, 0)));
    }

    #[test]
    fn test_none_days_matches_every_day() {
        let w = window("sleep", 22, 0, 6, 0, None);
        assert!(is_within_window(&w, None, ny(2026, 1, 1, 23, 0)));
        assert!(is_within_window(&w, None, ny(2026, 1, 3, 23, 0)));
    }
}

mod parse_weekdays_tests {
    use super::*;

    #[test]
    fn test_parses_short_forms() {
        assert_eq!(
            parse_weekdays("M T W Th F Sa Su"),
            Ok(weekday_bit(Weekday::Mon)
                | weekday_bit(Weekday::Tue)
                | weekday_bit(Weekday::Wed)
                | weekday_bit(Weekday::Thu)
                | weekday_bit(Weekday::Fri)
                | weekday_bit(Weekday::Sat)
                | weekday_bit(Weekday::Sun))
        );
    }

    #[test]
    fn test_accepts_comma_separated_and_mixed_case() {
        assert_eq!(
            parse_weekdays("sa,SU"),
            Ok(weekday_bit(Weekday::Sat) | weekday_bit(Weekday::Sun))
        );
    }

    #[test]
    fn test_accepts_full_names() {
        assert_eq!(
            parse_weekdays("Monday, wednesday"),
            Ok(weekday_bit(Weekday::Mon) | weekday_bit(Weekday::Wed))
        );
    }

    #[test]
    fn test_duplicate_tokens_collapse() {
        assert_eq!(parse_weekdays("M M"), Ok(weekday_bit(Weekday::Mon)));
    }

    #[test]
    fn test_rejects_unknown_token() {
        assert!(parse_weekdays("Blursday").is_err());
    }

    #[test]
    fn test_rejects_empty_input() {
        assert!(parse_weekdays("").is_err());
        assert!(parse_weekdays("   ").is_err());
    }
}

mod format_days_tests {
    use super::*;

    #[test]
    fn test_none_renders_nothing() {
        assert_eq!(format_days(None), None);
    }

    #[test]
    fn test_renders_selected_days_in_week_order() {
        let mask = weekday_bit(Weekday::Sat) | weekday_bit(Weekday::Mon) | weekday_bit(Weekday::Wed);
        assert_eq!(format_days(Some(mask)), Some("Mon, Wed, Sat".to_owned()));
    }

    #[test]
    fn test_format_window_appends_day_clause() {
        let w = window_on_days(
            "weekend",
            22,
            0,
            6,
            0,
            weekday_bit(Weekday::Fri) | weekday_bit(Weekday::Sat),
        );
        assert_eq!(
            format_window(&w, Some("America/New_York")),
            "\"weekend\": 10:00 PM - 6:00 AM America/New_York on Fri, Sat"
        );
    }
}

mod find_active_window {
    use super::*;

    #[test]
    fn test_returns_none_when_no_windows_active() {
        let windows = vec![window("work", 9, 0, 17, 0, None)];
        assert!(find_active_window(&windows, None, ny(2026, 1, 1, 20, 0)).is_none());
    }

    #[test]
    fn test_returns_matching_window() {
        let windows = vec![
            window("work", 9, 0, 17, 0, None),
            window("sleep", 22, 0, 6, 0, None),
        ];
        let matched = find_active_window(&windows, None, ny(2026, 1, 1, 23, 0)).unwrap();
        assert_eq!(matched.name, "sleep");
    }

    #[test]
    fn test_returns_alphabetically_first_when_multiple_active() {
        let windows = vec![
            window("zzz", 0, 0, 23, 59, None),
            window("aaa", 0, 0, 23, 59, None),
        ];
        let matched = find_active_window(&windows, None, ny(2026, 1, 1, 12, 0)).unwrap();
        assert_eq!(matched.name, "aaa");
    }
}

mod parse_clock_tests {
    use super::*;

    #[test]
    fn test_valid_formats_22_00() {
        assert_eq!(parse_clock("22:00"), Ok((22, 0)));
    }

    #[test]
    fn test_valid_formats_6_00() {
        assert_eq!(parse_clock("6:00"), Ok((6, 0)));
    }

    #[test]
    fn test_valid_formats_06_00() {
        assert_eq!(parse_clock("06:00"), Ok((6, 0)));
    }

    #[test]
    fn test_valid_formats_0_00() {
        assert_eq!(parse_clock("0:00"), Ok((0, 0)));
    }

    #[test]
    fn test_valid_formats_23_59() {
        assert_eq!(parse_clock("23:59"), Ok((23, 59)));
    }

    #[test]
    fn test_invalid_formats_raise_24_00() {
        assert!(parse_clock("24:00").is_err());
    }

    #[test]
    fn test_invalid_formats_raise_10pm() {
        assert!(parse_clock("10pm").is_err());
    }

    #[test]
    fn test_invalid_formats_raise_22_60() {
        assert!(parse_clock("22:60").is_err());
    }

    #[test]
    fn test_invalid_formats_raise_neg1_00() {
        assert!(parse_clock("-1:00").is_err());
    }

    #[test]
    fn test_invalid_formats_raise_empty() {
        assert!(parse_clock("").is_err());
    }

    #[test]
    fn test_invalid_formats_raise_10_0() {
        assert!(parse_clock("10:0").is_err());
    }
}

mod format_window_tests {
    use super::*;

    #[test]
    fn test_formats_name_am_pm_and_timezone() {
        let w = window("work", 9, 0, 17, 30, Some("America/New_York"));
        assert_eq!(
            format_window(&w, None),
            "\"work\": 9:00 AM - 5:30 PM America/New_York"
        );
    }

    #[test]
    fn test_falls_back_to_general_timezone() {
        let w = window("sleep", 22, 0, 6, 0, None);
        assert_eq!(
            format_window(&w, Some("Asia/Tokyo")),
            "\"sleep\": 10:00 PM - 6:00 AM Asia/Tokyo"
        );
    }

    #[test]
    fn test_falls_back_to_default_timezone() {
        let w = window("sleep", 22, 0, 6, 0, None);
        assert_eq!(
            format_window(&w, None),
            format!("\"sleep\": 10:00 PM - 6:00 AM {DEFAULT_TIMEZONE}")
        );
    }
}

mod effective_timezone_tests {
    use super::*;

    #[test]
    fn test_window_timezone_wins() {
        let w = window("w", 22, 0, 6, 0, Some("America/New_York"));
        assert_eq!(
            effective_timezone(&w, Some("Asia/Tokyo")),
            "America/New_York"
        );
    }

    #[test]
    fn test_falls_back_to_general() {
        let w = window("w", 22, 0, 6, 0, None);
        assert_eq!(effective_timezone(&w, Some("Asia/Tokyo")), "Asia/Tokyo");
    }

    #[test]
    fn test_falls_back_to_default() {
        let w = window("w", 22, 0, 6, 0, None);
        assert_eq!(effective_timezone(&w, None), DEFAULT_TIMEZONE);
    }
}
