use super::*;

mod is_valid_timezone_tests {
    use super::*;

    #[test]
    fn test_known_timezone() {
        assert!(is_valid_timezone("America/New_York"));
    }

    #[test]
    fn test_unknown_timezone() {
        assert!(!is_valid_timezone("Not/AZone"));
    }
}

mod common_timezones_tests {
    use super::*;

    #[test]
    fn test_every_curated_zone_is_a_real_iana_name() {
        for (_region, zones) in COMMON_TIMEZONES {
            for (name, _label) in *zones {
                assert!(
                    is_valid_timezone(name),
                    "{name} is not a valid IANA timezone"
                );
            }
        }
    }

    #[test]
    fn test_default_timezone_is_in_the_menu() {
        let (_, us_zones) = COMMON_TIMEZONES
            .iter()
            .find(|(region, _)| *region == "US")
            .expect("US region present");
        assert!(us_zones.iter().any(|(name, _)| *name == DEFAULT_TIMEZONE));
    }

    #[test]
    fn test_no_duplicate_zone_names_across_regions() {
        let mut all_names: Vec<&str> = COMMON_TIMEZONES
            .iter()
            .flat_map(|(_, zones)| zones.iter().map(|(name, _)| *name))
            .collect();
        let total = all_names.len();
        all_names.sort_unstable();
        all_names.dedup();
        assert_eq!(all_names.len(), total);
    }
}

mod format_common_timezones_tests {
    use super::*;

    #[test]
    fn test_includes_every_curated_zone() {
        let message = format_common_timezones();
        for (_region, zones) in COMMON_TIMEZONES {
            for (name, _label) in *zones {
                assert!(message.contains(&format!("`{name}`")));
            }
        }
    }

    #[test]
    fn test_includes_every_region_heading() {
        let message = format_common_timezones();
        for (region, _zones) in COMMON_TIMEZONES {
            assert!(message.contains(&format!("**{region}**")));
        }
    }

    #[test]
    fn test_notes_any_iana_name_is_accepted() {
        assert!(format_common_timezones().to_lowercase().contains("any"));
    }

    #[test]
    fn test_fits_within_discord_message_limit() {
        assert!(format_common_timezones().chars().count() <= 2000);
    }
}
