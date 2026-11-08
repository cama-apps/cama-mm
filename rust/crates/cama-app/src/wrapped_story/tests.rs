use std::collections::BTreeSet;

use super::*;

fn stats(
    discord_id: i64,
    games_played: i64,
    wins: i64,
    kills: i64,
    deaths: i64,
    assists: i64,
) -> MatchStatsRow {
    MatchStatsRow {
        discord_id,
        games_played,
        wins,
        total_kills: kills,
        total_deaths: deaths,
        total_assists: assists,
    }
}

fn hero(discord_id: i64, hero_id: i64, picks: i64, wins: i64) -> HeroRow {
    HeroRow {
        discord_id,
        hero_id,
        picks,
        wins,
        total_kills: 0,
        total_deaths: 0,
        total_assists: 0,
    }
}

fn year_match(
    won: Option<bool>,
    duration_seconds: Option<i64>,
    enrichment_present: bool,
    lane_role: Option<i32>,
) -> YearMatchRow {
    YearMatchRow {
        won,
        duration_seconds,
        enrichment_present,
        lane_role,
    }
}

fn populated_summary_input() -> PersonalSummaryInput {
    PersonalSummaryInput {
        discord_id: 111,
        player_name: Some("TestPlayer".to_owned()),
        match_details: Some(MatchDetails {
            games_played: 15,
            wins: 9,
            losses: 6,
        }),
        rating_change: Some(75),
        match_stats: vec![
            stats(111, 15, 9, 120, 60, 90),
            stats(222, 10, 5, 80, 40, 50),
            stats(333, 20, 8, 150, 80, 100),
        ],
        heroes: vec![
            hero(111, 1, 5, 3),
            hero(111, 2, 4, 2),
            hero(111, 3, 3, 2),
            hero(222, 1, 6, 3),
        ],
        matches: vec![
            year_match(Some(true), Some(2_400), true, Some(1)),
            year_match(Some(false), Some(1_800), true, Some(2)),
            year_match(Some(true), Some(3_000), true, Some(1)),
        ],
    }
}

#[test]
fn test_returns_string_from_pool() {
    let value = get_flavor("games_played_high", 0, &[]);
    assert!(
        flavor_pool("games_played_high")
            .unwrap()
            .contains(&value.as_str())
    );
}

#[test]
fn test_unknown_key_returns_empty() {
    assert_eq!(get_flavor("nonexistent_key_xyz", 0, &[]), "");
}

#[test]
fn package_deal_pool_is_gone() {
    assert!(flavor_pool("package_deal").is_none());
    assert!(
        FLAVOR_POOLS
            .iter()
            .flat_map(|(_, pool)| pool.iter())
            .all(|line| !line.to_lowercase().contains("package deal"))
    );
}

#[test]
fn test_template_formatting() {
    assert_eq!(
        format_flavor(
            "Hello {name}, you scored {score}",
            &[("name", "Alice"), ("score", "42")],
        ),
        "Hello Alice, you scored 42"
    );
}

#[test]
fn test_bad_template_returns_original() {
    assert_eq!(
        format_flavor("Hello {missing_var}", &[]),
        "Hello {missing_var}"
    );
}

#[test]
fn test_every_pool_has_non_empty_strings() {
    assert!(
        FLAVOR_POOLS
            .iter()
            .all(|(_, pool)| { !pool.is_empty() && pool.iter().all(|entry| !entry.is_empty()) })
    );
}

#[test]
fn test_compute_percentile_10_values0_50_0() {
    assert_eq!(compute_percentile(10.0, &[]), 50.0);
}

#[test]
fn test_compute_percentile_5_values1_50_0() {
    assert_eq!(compute_percentile(5.0, &[5.0]), 50.0);
}

#[test]
fn test_compute_percentile_10_values2_95_0() {
    let values: Vec<f64> = (1..=10).map(f64::from).collect();
    assert_eq!(compute_percentile(10.0, &values), 95.0);
}

#[test]
fn test_compute_percentile_1_values3_5_0() {
    let values: Vec<f64> = (1..=10).map(f64::from).collect();
    assert_eq!(compute_percentile(1.0, &values), 5.0);
}

#[test]
fn test_compute_percentile_5_values4_45_0() {
    let values: Vec<f64> = (1..=10).map(f64::from).collect();
    assert_eq!(compute_percentile(5.0, &values), 45.0);
}

#[test]
fn test_compute_percentile_5_values5_50_0() {
    assert_eq!(compute_percentile(5.0, &[5.0, 5.0, 5.0]), 50.0);
}

#[test]
fn test_compute_percentile_3_values6_50_0() {
    assert_eq!(compute_percentile(3.0, &[1.0, 3.0, 3.0, 5.0]), 50.0);
}

#[test]
fn test_personal_summary_wrapped() {
    let summary = personal_summary(&populated_summary_input()).unwrap();
    assert_eq!(summary.games_played, 15);
    assert_eq!(summary.win_rate, 0.6);
    assert!(summary.games_played_percentile.is_finite());
}

#[test]
fn test_no_arg_dataclass_defaults_hero_spotlight_wrapped() {
    let value = HeroSpotlightWrapped::default();
    assert!(value.top_hero_name.is_empty());
    assert!(value.top_3_heroes.is_empty());
}

#[test]
fn test_no_arg_dataclass_defaults_role_breakdown_wrapped() {
    let value = RoleBreakdownWrapped::default();
    assert!(value.lane_freq.is_empty());
    assert_eq!(value.total_games, 0);
}

#[test]
fn test_returns_none_when_player_not_found() {
    let mut input = populated_summary_input();
    input.player_name = None;
    assert!(personal_summary(&input).is_none());
}

#[test]
fn test_returns_none_when_no_matches() {
    let mut input = populated_summary_input();
    input.match_details = None;
    assert!(personal_summary(&input).is_none());
}

#[test]
fn test_returns_summary_with_correct_stats() {
    let summary = personal_summary(&populated_summary_input()).unwrap();
    assert_eq!(summary.discord_username, "TestPlayer");
    assert_eq!(
        (summary.games_played, summary.wins, summary.losses),
        (15, 9, 6)
    );
    assert_eq!(summary.rating_change, 75);
    assert_eq!(
        (
            summary.total_kills,
            summary.total_deaths,
            summary.total_assists
        ),
        (120, 60, 90)
    );
    assert_eq!(summary.unique_heroes, 3);
    assert_eq!(summary.avg_game_duration, 2_400);
    assert!(!summary.flavor_text.is_empty());
}

#[test]
fn test_returns_none_when_no_heroes() {
    assert!(hero_spotlight(111, &[], |_| None).is_none());
}

#[test]
fn test_returns_none_when_no_heroes_for_player() {
    assert!(hero_spotlight(111, &[hero(999, 1, 5, 3)], |_| None).is_none());
}

#[test]
fn test_returns_hero_spotlight() {
    let heroes = vec![
        hero(111, 1, 8, 5),
        hero(111, 2, 5, 4),
        hero(111, 3, 3, 1),
        hero(222, 1, 10, 6),
    ];
    let result = hero_spotlight(111, &heroes, |id| {
        Some(
            match id {
                1 => "Anti-Mage",
                2 => "Axe",
                _ => "Bane",
            }
            .to_owned(),
        )
    })
    .unwrap();
    assert_eq!(result.top_hero_name, "Anti-Mage");
    assert_eq!((result.top_hero_picks, result.top_hero_wins), (8, 5));
    assert_eq!(result.top_hero_win_rate, 5.0 / 8.0);
    assert_eq!(result.unique_heroes, 3);
    assert_eq!(
        result
            .top_3_heroes
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Anti-Mage", "Axe", "Bane"]
    );
}

#[test]
fn test_single_hero() {
    let result = hero_spotlight(111, &[hero(111, 5, 10, 7)], |_| {
        Some("Crystal Maiden".to_owned())
    })
    .unwrap();
    assert_eq!(result.top_hero_name, "Crystal Maiden");
    assert_eq!(result.unique_heroes, 1);
    assert_eq!(result.top_3_heroes.len(), 1);
}

#[test]
fn test_returns_none_when_no_matches_role_breakdown() {
    assert!(role_breakdown(&[]).is_none());
}

#[test]
fn test_returns_result_with_no_enrichment_data() {
    let result = role_breakdown(&[year_match(None, None, false, None)]).unwrap();
    assert!(result.lane_freq.is_empty());
    assert_eq!(result.total_games, 0);
}

#[test]
fn test_returns_role_freq_from_enrichment() {
    let result = role_breakdown(&[
        year_match(None, None, true, Some(1)),
        year_match(None, None, true, Some(2)),
        year_match(None, None, true, Some(1)),
    ])
    .unwrap();
    assert_eq!(result.lane_freq, BTreeMap::from([(1, 2), (2, 1)]));
    assert_eq!(result.total_games, 3);
}

#[test]
fn test_empty_freq_when_no_enrichment() {
    let result = role_breakdown(&[
        year_match(None, None, false, None),
        year_match(None, None, false, None),
    ])
    .unwrap();
    assert_eq!(result, RoleBreakdownWrapped::default());
}

#[test]
fn test_skips_lane_role_zero() {
    assert_eq!(
        role_breakdown(&[year_match(None, None, true, Some(0))])
            .unwrap()
            .total_games,
        0
    );
}

#[test]
fn test_skips_jungle_lane_role_4() {
    let result = role_breakdown(&[
        year_match(None, None, true, Some(4)),
        year_match(None, None, true, Some(2)),
    ])
    .unwrap();
    assert_eq!(result.lane_freq, BTreeMap::from([(2, 1)]));
    assert_eq!(result.total_games, 1);
}

#[test]
fn test_get_year_timestamps() {
    let (start, end) = year_timestamps(2026);
    assert_eq!(start, 1_767_225_600);
    assert_eq!(end, 1_798_761_600);
    assert_eq!(end - start, 365 * 86_400);
}

#[test]
fn test_generate_awards_empty_data() {
    assert!(generate_awards(&[], &[]).is_empty());
}

#[test]
fn test_generate_awards_with_data() {
    let match_stats: Vec<_> = (1..=12)
        .map(|id| WrappedAwardStats {
            discord_id: id,
            discord_username: format!("Player{id}"),
            games_played: 10 + id,
            avg_gpm: (400 + id * 20) as f64,
        })
        .collect();
    let rating_changes = [
        WrappedRatingChange {
            discord_id: 1,
            discord_username: "Player1".to_owned(),
            rating_change: 150.0,
        },
        WrappedRatingChange {
            discord_id: 2,
            discord_username: "Player2".to_owned(),
            rating_change: -200.0,
        },
    ];
    let titles: BTreeSet<_> = generate_awards(&match_stats, &rating_changes)
        .into_iter()
        .map(|award| award.title)
        .collect();
    assert!(
        [
            "Highest average GPM",
            "Largest rating gain",
            "Largest rating loss",
            "Most games played"
        ]
        .into_iter()
        .all(|title| titles.contains(title))
    );
}

#[test]
fn test_award_dataclass() {
    let award = WrappedAward {
        category: "performance".to_owned(),
        title: "Highest average GPM".to_owned(),
        stat_name: "Best GPM".to_owned(),
        stat_value: "847 avg".to_owned(),
        discord_id: 123,
        discord_username: "TestUser".to_owned(),
        emoji: "💰".to_owned(),
        flavor_text: "Farming simulator champion".to_owned(),
    };
    assert_eq!(award.category, "performance");
    assert_eq!(award.title, "Highest average GPM");
    assert_eq!(award.emoji, "💰");
}
