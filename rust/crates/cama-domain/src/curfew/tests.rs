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
