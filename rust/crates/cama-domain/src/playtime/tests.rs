use super::*;

fn player(hours: Option<&[u32]>, timezone: Option<&str>) -> PlayHoursPlayer {
    PlayHoursPlayer {
        dota_play_hours: hours.map(<[u32]>::to_vec),
        timezone: timezone.map(str::to_owned),
    }
}

fn set(hours: &[u32]) -> BTreeSet<u32> {
    hours.iter().copied().collect()
}

mod parse_hour_set_tests {
    use super::*;

    #[test]
    fn test_single_hours() {
        assert_eq!(parse_hour_set("14,20,21").unwrap(), set(&[14, 20, 21]));
    }

    #[test]
    fn test_simple_range() {
        assert_eq!(parse_hour_set("18-21").unwrap(), set(&[18, 19, 20, 21]));
    }

    #[test]
    fn test_wrapping_range() {
        assert_eq!(parse_hour_set("22-2").unwrap(), set(&[22, 23, 0, 1, 2]));
    }

    #[test]
    fn test_mixed_ranges_and_singles() {
        assert_eq!(
            parse_hour_set("14, 18-20, 23").unwrap(),
            set(&[14, 18, 19, 20, 23])
        );
    }

    #[test]
    fn test_single_hour_range_collapses() {
        assert_eq!(parse_hour_set("9-9").unwrap(), set(&[9]));
    }

    #[test]
    fn test_invalid_input_raises_empty() {
        assert!(parse_hour_set("").is_err());
    }

    #[test]
    fn test_invalid_input_raises_blank() {
        assert!(parse_hour_set("  ").is_err());
    }

    #[test]
    fn test_invalid_input_raises_24() {
        assert!(parse_hour_set("24").is_err());
    }

    #[test]
    fn test_invalid_input_raises_18_24() {
        assert!(parse_hour_set("18-24").is_err());
    }

    #[test]
    fn test_invalid_input_raises_abc() {
        assert!(parse_hour_set("abc").is_err());
    }

    #[test]
    fn test_invalid_input_raises_18_double_dash_20() {
        assert!(parse_hour_set("18--20").is_err());
    }

    #[test]
    fn test_invalid_input_raises_neg1() {
        assert!(parse_hour_set("-1").is_err());
    }
}

mod format_hour_ranges_tests {
    use super::*;

    #[test]
    fn test_empty_set() {
        assert_eq!(format_hour_ranges(&BTreeSet::new()), "none");
    }

    #[test]
    fn test_single_hour() {
        assert_eq!(format_hour_ranges(&set(&[14])), "14:00");
    }

    #[test]
    fn test_contiguous_range() {
        assert_eq!(format_hour_ranges(&set(&[18, 19, 20, 21])), "18:00-21:00");
    }

    #[test]
    fn test_multiple_disjoint_ranges() {
        assert_eq!(
            format_hour_ranges(&set(&[9, 14, 15, 16, 20])),
            "09:00, 14:00-16:00, 20:00"
        );
    }
}

mod effective_timezone_tests {
    use super::*;

    #[test]
    fn test_uses_player_timezone_when_set() {
        assert_eq!(
            effective_timezone(&player(Some(&[18]), Some("Asia/Tokyo"))),
            "Asia/Tokyo"
        );
    }

    #[test]
    fn test_falls_back_to_default_when_unset() {
        assert_eq!(
            effective_timezone(&player(Some(&[18]), None)),
            DEFAULT_TIMEZONE
        );
    }
}

mod aggregate_popular_hours_tests {
    use super::*;

    fn jan_1_2026() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date")
    }

    #[test]
    fn test_counts_hours_for_players_in_reporting_timezone() {
        let players = [
            player(Some(&[20, 21]), Some("America/New_York")),
            player(Some(&[20]), Some("America/New_York")),
        ];

        let counts = aggregate_popular_hours(&players, "America/New_York", Some(jan_1_2026()));

        assert_eq!(counts[20], 2);
        assert_eq!(counts[21], 1);
        assert_eq!(counts.iter().sum::<u32>(), 3);
    }

    #[test]
    fn test_converts_across_timezones() {
        // 8pm JST on 2026-01-01 is 6am EST the same calendar day (winter, UTC-5).
        let players = [player(Some(&[20]), Some("Asia/Tokyo"))];

        let counts = aggregate_popular_hours(&players, "America/New_York", Some(jan_1_2026()));

        assert_eq!(counts[6], 1);
        assert_eq!(counts[20], 0);
    }

    #[test]
    fn test_ignores_players_without_hours_set() {
        let players = [
            player(None, None),
            player(Some(&[]), None),
            player(Some(&[9]), None),
        ];

        let counts = aggregate_popular_hours(&players, "America/New_York", Some(jan_1_2026()));

        assert_eq!(counts.iter().sum::<u32>(), 1);
        assert_eq!(counts[9], 1);
    }

    #[test]
    fn test_unknown_reporting_timezone_falls_back_to_default() {
        let players = [player(Some(&[12]), Some("America/New_York"))];

        let counts = aggregate_popular_hours(&players, "Not/AZone", Some(jan_1_2026()));

        assert_eq!(counts[12], 1);
    }
}
