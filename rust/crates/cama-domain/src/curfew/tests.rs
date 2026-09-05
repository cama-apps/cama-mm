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
        mode: CurfewMode::Default,
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
        let mask =
            weekday_bit(Weekday::Sat) | weekday_bit(Weekday::Mon) | weekday_bit(Weekday::Wed);
        assert_eq!(format_days(Some(mask)), Some("Mon, Wed, Sat".to_owned()));
    }

    #[test]
    fn test_format_window_names_the_start_day_for_an_overnight_span() {
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
            "\"weekend\": 10:00 PM - 6:00 AM America/New_York starting Fri, Sat (runs into the next morning)"
        );
    }

    #[test]
    fn test_format_window_appends_plain_day_clause_for_a_same_day_span() {
        let w = window_on_days(
            "work",
            9,
            0,
            17,
            0,
            weekday_bit(Weekday::Mon) | weekday_bit(Weekday::Tue),
        );
        assert_eq!(
            format_window(&w, Some("America/New_York")),
            "\"work\": 9:00 AM - 5:00 PM America/New_York on Mon, Tue"
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

mod next_local_morning_tests {
    use super::*;

    #[test]
    fn test_noon_change_applies_at_eight_the_next_morning() {
        let now = ny(2026, 3, 10, 12, 0);
        assert_eq!(
            next_local_morning("America/New_York", now),
            ny(2026, 3, 11, 8, 0)
        );
    }

    #[test]
    fn test_change_before_eight_still_waits_for_the_next_calendar_day() {
        // An 07:00 edit must not apply at 08:00 the same day — that would
        // loosen tonight's window just like a noon edit would.
        let now = ny(2026, 3, 10, 7, 0);
        assert_eq!(
            next_local_morning("America/New_York", now),
            ny(2026, 3, 11, 8, 0)
        );
    }

    #[test]
    fn test_late_night_change_applies_the_following_morning() {
        let now = ny(2026, 3, 10, 23, 30);
        assert_eq!(
            next_local_morning("America/New_York", now),
            ny(2026, 3, 11, 8, 0)
        );
    }

    #[test]
    fn test_uses_the_windows_timezone() {
        let now = chrono_tz::Asia::Tokyo
            .with_ymd_and_hms(2026, 3, 10, 15, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let expected = chrono_tz::Asia::Tokyo
            .with_ymd_and_hms(2026, 3, 11, 8, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(next_local_morning("Asia/Tokyo", now), expected);
    }

    #[test]
    fn test_spring_forward_night_still_lands_on_eight_local() {
        // 2026-03-08 is the US spring-forward date; 08:00 exists on the 9th
        // regardless, and the UTC offset has changed underneath it.
        let now = ny(2026, 3, 8, 12, 0);
        assert_eq!(
            next_local_morning("America/New_York", now),
            ny(2026, 3, 9, 8, 0)
        );
    }
}

mod mode_tests {
    use super::*;

    #[test]
    fn test_parse_mode_accepts_every_mode() {
        assert_eq!(parse_mode("default"), Ok(CurfewMode::Default));
        assert_eq!(parse_mode("Strict"), Ok(CurfewMode::Strict));
        assert_eq!(parse_mode("informational"), Ok(CurfewMode::Informational));
        assert_eq!(parse_mode("info"), Ok(CurfewMode::Informational));
    }

    #[test]
    fn test_parse_mode_rejects_unknown_text() {
        let error = parse_mode("lenient").unwrap_err();
        assert!(error.contains("informational"), "{error}");
    }

    #[test]
    fn test_format_window_labels_each_mode() {
        let strict = CurfewWindow {
            mode: CurfewMode::Strict,
            ..window("w", 22, 0, 6, 0, None)
        };
        assert!(format_window(&strict, None).contains("next morning"));
        let informational = CurfewWindow {
            mode: CurfewMode::Informational,
            ..window("w", 22, 0, 6, 0, None)
        };
        assert!(format_window(&informational, None).contains("informational"));
        let plain = format_window(&window("w", 22, 0, 6, 0, None), None);
        assert!(!plain.contains('['), "{plain}");
    }
}

mod retains_coverage_tests {
    use super::*;

    fn strict(start_hour: u32, end_hour: u32) -> CurfewWindow {
        CurfewWindow {
            mode: CurfewMode::Strict,
            ..window("w", start_hour, 0, end_hour, 0, Some("America/New_York"))
        }
    }

    #[test]
    fn test_identical_window_retains_coverage() {
        let now = ny(2026, 3, 10, 12, 0);
        assert!(retains_coverage(&strict(22, 6), &strict(22, 6), None, now));
    }

    #[test]
    fn test_extension_retains_coverage_but_reduction_does_not() {
        let now = ny(2026, 3, 10, 12, 0);
        assert!(retains_coverage(&strict(21, 7), &strict(22, 6), None, now));
        assert!(!retains_coverage(&strict(23, 6), &strict(22, 6), None, now));
        assert!(!retains_coverage(&strict(22, 5), &strict(22, 6), None, now));
    }

    #[test]
    fn test_shifted_window_is_a_reduction() {
        // 22-06 -> 23-07 adds an hour at the end but frees 22-23.
        let now = ny(2026, 3, 10, 12, 0);
        assert!(!retains_coverage(&strict(23, 7), &strict(22, 6), None, now));
    }

    #[test]
    fn test_dropping_a_selected_day_is_a_reduction() {
        let now = ny(2026, 3, 10, 12, 0);
        let weekdays = CurfewWindow {
            days: Some(weekday_bit(Weekday::Mon) | weekday_bit(Weekday::Tue)),
            ..strict(22, 6)
        };
        let monday_only = CurfewWindow {
            days: Some(weekday_bit(Weekday::Mon)),
            ..strict(22, 6)
        };
        assert!(!retains_coverage(&monday_only, &weekdays, None, now));
        assert!(retains_coverage(&weekdays, &monday_only, None, now));
        let every_day = CurfewWindow {
            days: None,
            ..strict(22, 6)
        };
        assert!(retains_coverage(&every_day, &weekdays, None, now));
        assert!(!retains_coverage(&weekdays, &every_day, None, now));
    }

    #[test]
    fn test_timezone_move_that_shifts_the_span_is_a_reduction() {
        let now = ny(2026, 3, 10, 12, 0);
        let chicago = CurfewWindow {
            timezone: Some("America/Chicago".to_owned()),
            ..strict(22, 6)
        };
        // 22:00 Chicago is 23:00 New York, so 22:00-23:00 NY is freed.
        assert!(!retains_coverage(&chicago, &strict(22, 6), None, now));
        // ...while the reverse move only starts an hour earlier in NY terms
        // and ends an hour earlier too, freeing 05:00-06:00 NY.
        assert!(!retains_coverage(&strict(22, 6), &chicago, None, now));
    }
}

mod mode_helper_tests {
    use super::*;

    #[test]
    fn test_only_strict_stages_changes() {
        assert!(!CurfewMode::Default.stages_changes());
        assert!(CurfewMode::Strict.stages_changes());
        assert!(!CurfewMode::Informational.stages_changes());
    }

    #[test]
    fn test_only_informational_asks_for_confirmation() {
        assert!(!CurfewMode::Default.asks_for_confirmation());
        assert!(!CurfewMode::Strict.asks_for_confirmation());
        assert!(CurfewMode::Informational.asks_for_confirmation());
    }

    #[test]
    fn test_retired_tax_mode_is_rejected() {
        assert!(parse_mode("tax").is_err());
    }
}

mod retains_coverage_timezone_tests {
    use super::*;

    fn strict(start_hour: u32, end_hour: u32) -> CurfewWindow {
        CurfewWindow {
            mode: CurfewMode::Strict,
            ..window("w", start_hour, 0, end_hour, 0, Some("America/New_York"))
        }
    }

    #[test]
    fn test_mode_alone_does_not_affect_coverage() {
        let now = ny(2026, 3, 10, 12, 0);
        let informational = CurfewWindow {
            mode: CurfewMode::Informational,
            ..strict(22, 6)
        };
        assert!(retains_coverage(&informational, &strict(22, 6), None, now));
    }

    #[test]
    fn test_inherited_and_explicit_same_zone_take_the_exact_path() {
        // Same effective zone reached two ways still compares wall-clock
        // minutes exactly, and agrees with what sampling would say.
        let now = ny(2026, 3, 10, 12, 0);
        let explicit = strict(22, 6);
        let inherited = CurfewWindow {
            timezone: None,
            ..strict(23, 6)
        };
        assert!(!retains_coverage(
            &inherited,
            &explicit,
            Some("America/New_York"),
            now
        ));
        assert!(retains_coverage(
            &explicit,
            &inherited,
            Some("America/New_York"),
            now
        ));
    }

    #[test]
    fn test_moving_to_a_zone_with_the_same_offset_keeps_coverage() {
        // Detroit and New York share Eastern time, so the sampled fallback
        // finds no freed minute even though the zone names differ.
        let now = ny(2026, 3, 10, 12, 0);
        let detroit = CurfewWindow {
            timezone: Some("America/Detroit".to_owned()),
            ..strict(22, 6)
        };
        assert!(retains_coverage(&detroit, &strict(22, 6), None, now));
    }
}
