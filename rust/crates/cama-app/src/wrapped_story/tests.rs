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

fn pair(peer_id: i64, games: i64, wins: i64, win_rate: f64) -> PairwiseRaw {
    PairwiseRaw {
        peer_id,
        games,
        wins,
        win_rate,
    }
}

fn award(discord_id: i64, title: impl Into<String>) -> Award {
    Award {
        discord_id,
        title: title.into(),
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
fn test_pairwise_entry() {
    let entry = PairwiseEntry {
        discord_id: 1,
        username: "A".to_owned(),
        games: 10,
        wins: 7,
        win_rate: 0.7,
    };
    assert_eq!(entry.win_rate, 0.7);
}

#[test]
fn test_no_arg_dataclass_defaults_pairwise_wrapped() {
    let value = PairwiseWrapped::default();
    assert!(value.best_teammates.is_empty());
    assert!(value.nemesis.is_none());
}

#[test]
fn test_no_arg_dataclass_defaults_package_deal_wrapped() {
    assert_eq!(PackageDealWrapped::default(), PackageDealWrapped::default());
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
fn test_wrapped_story_snapshot_reuses_queries_and_compact_enrichment() {
    let input = populated_summary_input();
    let mut audit = QueryAudit::default();
    let snapshot = build_story_snapshot(
        111,
        2026,
        Some(0),
        input.match_stats.clone(),
        input.heroes.clone(),
        input.matches.clone(),
        &mut audit,
    );
    let details = MatchDetails {
        games_played: snapshot.matches.len() as i64,
        wins: snapshot
            .matches
            .iter()
            .filter(|row| row.won == Some(true))
            .count() as i64,
        losses: snapshot
            .matches
            .iter()
            .filter(|row| row.won == Some(false))
            .count() as i64,
    };
    let summary = personal_summary(&PersonalSummaryInput {
        discord_id: 111,
        player_name: Some("TestPlayer".to_owned()),
        match_details: Some(details),
        rating_change: Some(0),
        match_stats: snapshot.match_stats.clone(),
        heroes: snapshot.heroes.clone(),
        matches: snapshot.matches.clone(),
    });
    assert!(summary.is_some());
    assert!(hero_spotlight(111, &snapshot.heroes, |id| Some(format!("Hero {id}"))).is_some());
    assert_eq!(
        role_breakdown(&snapshot.matches).unwrap().lane_freq,
        BTreeMap::from([(1, 2), (2, 1)])
    );
    assert_eq!(audit.match_stats_reads, 1);
    assert_eq!(audit.hero_reads, 1);
    assert_eq!(audit.year_match_reads, 1);
    assert_eq!(audit.match_detail_reads, 0);
    assert_eq!(audit.steam_id_reads, 0);
}

#[test]
fn test_player_wrapped_uses_player_scoped_rating_change() {
    let mut audit = QueryAudit::default();
    assert_eq!(player_scoped_rating_change(Some(42), &mut audit), 42);
    assert_eq!(audit.player_rating_reads, 1);
    assert_eq!(audit.aggregate_rating_reads, 0);
}

#[test]
fn test_returns_none_without_pairings_repo() {
    assert!(pairwise_wrapped(None, &BTreeMap::new()).is_none());
}

#[test]
fn test_returns_none_when_no_pairwise_data() {
    assert!(pairwise_wrapped(Some(&PairwiseInput::default()), &BTreeMap::new()).is_none());
}

fn full_pairwise_input() -> PairwiseInput {
    PairwiseInput {
        best_teammates: vec![pair(222, 10, 8, 0.8)],
        most_played_with: vec![pair(333, 15, 7, 0.47)],
        worst_matchups: vec![pair(444, 8, 2, 0.25)],
        best_matchups: vec![pair(555, 6, 5, 0.83)],
        most_played_against: vec![pair(666, 12, 6, 0.5)],
    }
}

#[test]
fn test_returns_pairwise_data_with_resolved_names() {
    let input = full_pairwise_input();
    let names = BTreeMap::from([
        (222, "Alice".to_owned()),
        (333, "Bob".to_owned()),
        (444, "Charlie".to_owned()),
        (555, "Diana".to_owned()),
        (666, "Eve".to_owned()),
    ]);
    let result = pairwise_wrapped(Some(&input), &names).unwrap();
    assert_eq!(result.best_teammates[0].username, "Alice");
    assert_eq!(
        (
            result.best_teammates[0].games,
            result.best_teammates[0].wins
        ),
        (10, 8)
    );
    assert_eq!(result.most_played_with[0].username, "Bob");
    assert_eq!(result.nemesis.unwrap().username, "Charlie");
    assert_eq!(result.punching_bag.unwrap().username, "Diana");
    assert_eq!(result.most_played_against[0].username, "Eve");
    assert_eq!(pairwise_player_ids(&input), vec![222, 333, 444, 555, 666]);
}

#[test]
fn test_pairwise_name_lookup_deduplicates_overlapping_players() {
    let input = PairwiseInput {
        best_teammates: vec![pair(222, 10, 8, 0.8)],
        most_played_with: vec![pair(222, 12, 9, 0.75)],
        most_played_against: vec![pair(222, 6, 3, 0.5)],
        ..PairwiseInput::default()
    };
    assert_eq!(pairwise_player_ids(&input), vec![222]);
    let result =
        pairwise_wrapped(Some(&input), &BTreeMap::from([(222, "Alice".to_owned())])).unwrap();
    assert!(
        result
            .best_teammates
            .iter()
            .chain(&result.most_played_with)
            .chain(&result.most_played_against)
            .all(|entry| entry.username == "Alice")
    );
}

#[test]
fn test_guild_id_none_normalizes_to_zero() {
    assert_eq!(pairwise_query_scope(None), None);
    assert_eq!(pairwise_query_scope(Some(0)), Some(0));
}

#[test]
fn test_returns_none_without_service() {
    assert!(package_deal_from_source(None, 111, 2026, 0).is_none());
}

#[test]
fn test_returns_none_when_no_deals() {
    assert!(package_deal_from_source(Some(&PackageDealLedger::default()), 111, 2026, 0).is_none());
}

#[test]
fn test_returns_deal_data() {
    let mut ledger = PackageDealLedger::default();
    ledger.purchase(0, 111, 222, 5, 50, 2026);
    ledger.purchase(0, 333, 111, 3, 30, 2026);
    let result = ledger.wrapped(111, 2026, 0).unwrap();
    assert_eq!(result.times_bought, 1);
    assert_eq!(result.times_bought_on_you, 1);
    assert_eq!(result.unique_buyers, 1);
    assert_eq!((result.jc_spent, result.jc_spent_on_you), (50, 30));
    assert_eq!(result.total_games_committed, 8);
}

#[test]
fn test_consumed_and_deleted_deals_still_counted() {
    let mut ledger = PackageDealLedger::default();
    let first = ledger.purchase(0, 111, 222, 10, 50, 2026);
    let second = ledger.purchase(0, 111, 333, 10, 70, 2026);
    for _ in 0..10 {
        ledger.decrement(0, &[first, second]);
    }
    assert!(ledger.active_involving(0, 111).is_empty());
    let result = ledger.wrapped(111, 2026, 0).unwrap();
    assert_eq!(result.times_bought, 2);
    assert_eq!(result.jc_spent, 120);
    assert_eq!(result.total_games_committed, 20);
}

#[test]
fn test_partner_purchases_counted_after_deletion() {
    let mut ledger = PackageDealLedger::default();
    let deal = ledger.purchase(0, 999, 111, 10, 80, 2026);
    for _ in 0..10 {
        ledger.decrement(0, &[deal]);
    }
    let result = ledger.wrapped(111, 2026, 0).unwrap();
    assert_eq!(
        (
            result.times_bought_on_you,
            result.unique_buyers,
            result.jc_spent_on_you
        ),
        (1, 1, 80)
    );
}

#[test]
fn test_purchases_outside_year_excluded() {
    let mut ledger = PackageDealLedger::default();
    ledger.purchase(0, 111, 222, 10, 50, 2026);
    assert!(ledger.wrapped(111, 2021, 0).is_none());
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
fn test_returns_deals_as_buyer() {
    let mut ledger = PackageDealLedger::default();
    ledger.purchase(0, 111, 222, 5, 50, 2026);
    let deals = ledger.active_involving(0, 111);
    assert_eq!(deals.len(), 1);
    assert_eq!(
        (
            deals[0].buyer_discord_id,
            deals[0].partner_discord_id,
            deals[0].games_remaining
        ),
        (111, 222, 5)
    );
}

#[test]
fn test_returns_deals_as_partner() {
    let mut ledger = PackageDealLedger::default();
    ledger.purchase(0, 333, 111, 3, 30, 2026);
    assert_eq!(ledger.active_involving(0, 111)[0].buyer_discord_id, 333);
}

#[test]
fn test_returns_both_buyer_and_partner_deals() {
    let mut ledger = PackageDealLedger::default();
    ledger.purchase(0, 111, 222, 5, 50, 2026);
    ledger.purchase(0, 333, 111, 3, 30, 2026);
    assert_eq!(ledger.active_involving(0, 111).len(), 2);
}

#[test]
fn test_excludes_expired_deals() {
    let mut ledger = PackageDealLedger::default();
    let id = ledger.purchase(0, 111, 222, 1, 10, 2026);
    ledger.decrement(0, &[id]);
    assert!(ledger.active_involving(0, 111).is_empty());
}

#[test]
fn test_empty_when_no_deals() {
    assert!(
        PackageDealLedger::default()
            .active_involving(0, 111)
            .is_empty()
    );
}

#[test]
fn test_guild_isolation() {
    let mut ledger = PackageDealLedger::default();
    ledger.purchase(100, 111, 222, 5, 50, 2026);
    ledger.purchase(200, 111, 333, 3, 30, 2026);
    assert_eq!(ledger.active_involving(100, 111).len(), 1);
    assert_eq!(ledger.active_involving(200, 111).len(), 1);
    assert!(ledger.active_involving(999, 111).is_empty());
}

#[test]
fn test_draw_story_slide_basic_render() {
    assert_eq!(
        slide_spec("story"),
        SlideSpec {
            width: 800,
            height: 600,
            kind: "story"
        }
    );
}

#[test]
fn test_draw_summary_stats_slide_basic_render() {
    assert_eq!(
        (slide_spec("summary").width, slide_spec("summary").height),
        (800, 600)
    );
}

#[test]
fn test_draw_pairwise_slide_teammates_slide() {
    assert_eq!(
        (slide_spec("pairwise").width, slide_spec("pairwise").height),
        (800, 600)
    );
}

#[test]
fn test_draw_hero_spotlight_slide_basic_render() {
    assert_eq!(
        (slide_spec("hero").width, slide_spec("hero").height),
        (800, 600)
    );
}

#[test]
fn test_draw_lane_breakdown_slide_with_lane_data() {
    assert_eq!(
        (slide_spec("lanes").width, slide_spec("lanes").height),
        (800, 600)
    );
}

#[test]
fn test_word_wrap_empty_string() {
    assert!(word_wrap("", 200).is_empty());
}

#[test]
fn test_word_wrap_single_word() {
    assert_eq!(word_wrap("hello", 200), ["hello"]);
}

#[test]
fn test_word_wrap_fits_one_line() {
    assert_eq!(word_wrap("short text", 500), ["short text"]);
}

#[test]
fn test_word_wrap_wraps_long_text() {
    let text = "this is a longer piece of text that should wrap";
    let result = word_wrap(text, 80);
    assert!(result.len() > 1);
    assert_eq!(result.join(" "), text);
}

#[test]
fn test_word_wrap_truncates_oversized_word() {
    let result = word_wrap("Supercalifragilisticexpialidocious", 30);
    assert_eq!(result.len(), 1);
    assert!(result[0].ends_with(".."));
}

#[test]
fn test_draw_package_deal_slide_basic_render() {
    assert_eq!(
        (
            slide_spec("package_deal").width,
            slide_spec("package_deal").height
        ),
        (800, 600)
    );
}

#[test]
fn test_wrap_chart_in_slide_wraps_chart_image() {
    assert_eq!(
        (slide_spec("chart").width, slide_spec("chart").height),
        (800, 600)
    );
}

#[test]
fn test_viewer_awards_always_included() {
    let awards: Vec<_> = (1..=10).map(|id| award(id, format!("Award{id}"))).collect();
    let result = select_awards_for_viewer(&awards, 10, MAX_AWARDS);
    assert_eq!(
        result.iter().filter(|item| item.discord_id == 10).count(),
        1
    );
}

#[test]
fn test_caps_at_max_awards() {
    let awards: Vec<_> = (1..=20).map(|id| award(id, format!("Award{id}"))).collect();
    assert_eq!(select_awards_for_viewer(&awards, 1, MAX_AWARDS).len(), 6);
}

#[test]
fn test_viewer_with_more_than_max_awards() {
    let awards: Vec<_> = (0..8).map(|id| award(1, format!("Award{id}"))).collect();
    let result = select_awards_for_viewer(&awards, 1, MAX_AWARDS);
    assert_eq!(result.len(), 6);
    assert!(result.iter().all(|item| item.discord_id == 1));
}

#[test]
fn test_fills_remaining_with_others() {
    let mut awards = vec![award(1, "Viewer Award")];
    awards.extend((2..10).map(|id| award(id, format!("Other{id}"))));
    let result = select_awards_for_viewer(&awards, 1, MAX_AWARDS);
    assert_eq!(result.len(), 6);
    assert_eq!(result[0].discord_id, 1);
}

#[test]
fn test_empty_awards() {
    assert!(select_awards_for_viewer(&[], 1, MAX_AWARDS).is_empty());
}

#[test]
fn test_no_viewer_awards() {
    let awards: Vec<_> = (2..10).map(|id| award(id, format!("Award{id}"))).collect();
    let result = select_awards_for_viewer(&awards, 1, MAX_AWARDS);
    assert_eq!(result.len(), 6);
    assert!(result.iter().all(|item| item.discord_id != 1));
}

#[test]
fn test_reuses_degen_score_from_player_stats() {
    let stats = GamblingStats {
        total_bets: 12,
        win_rate: 0.5,
        net_pnl: -40,
        roi: -0.25,
        degen_score: DegenScore {
            total: 73,
            title: "Degenerate".to_owned(),
            emoji: "🎰".to_owned(),
        },
    };
    let data = wrapped_gambling_data(vec![(1, -10), (2, -40)], stats.clone());
    assert_eq!(data.stats, stats);
    assert_eq!(data.stats.degen_score.total, 73);
    assert!(!data.recalculated_degen_score);
    assert_eq!(data.snapshot_consumer_count, 5);
}

#[test]
fn test_draw_awards_grid_basic_render() {
    let awards = [award(1, "Award1"), award(2, "Award2"), award(3, "Award3")];
    let spec = slide_spec(if awards.is_empty() { "empty" } else { "awards" });
    assert!(spec.width > 0);
    assert!(spec.height > 0);
}

fn record_row(match_id: i64, kills: i64, deaths: i64, assists: i64) -> WrappedRecordRow {
    WrappedRecordRow {
        match_id,
        guild_id: 42,
        discord_id: 111,
        hero_id: Some(match_id),
        kills,
        deaths,
        assists,
        gpm: 400 + match_id * 50,
        xpm: 500 + match_id * 40,
        won: Some(match_id % 2 == 1),
        enrichment: None,
    }
}

fn sample_record_rows(count: usize) -> Vec<WrappedRecordRow> {
    (0..count)
        .map(|index| {
            record_row(
                index as i64 + 1,
                5 + index as i64 * 3,
                2 + index as i64,
                10 + index as i64 * 2,
            )
        })
        .collect()
}

#[test]
fn test_basic_records_populated() {
    let result = player_records(Some("TestPlayer"), &sample_record_rows(6), 3).unwrap();
    assert_eq!(result.discord_username, "TestPlayer");
    assert_eq!(result.games_played, 6);
    let kills = result
        .records
        .iter()
        .find(|record| record.stat_key == "kills_best")
        .unwrap();
    assert_eq!(kills.value, Some(20.0));
}

#[test]
fn test_below_min_games_returns_none() {
    assert!(player_records(Some("TestPlayer"), &sample_record_rows(2), 3).is_none());
}

#[test]
fn test_worst_records_included() {
    let result = player_records(Some("TestPlayer"), &sample_record_rows(6), 3).unwrap();
    let keys: BTreeSet<_> = result
        .records
        .iter()
        .filter(|record| record.is_worst)
        .map(|record| record.stat_key)
        .collect();
    assert!(
        ["kda_worst", "gpm_worst", "xpm_worst", "deaths_worst"]
            .into_iter()
            .all(|key| keys.contains(key))
    );
}

#[test]
fn test_wwlwwwlll() {
    let matches = [
        (Some(true), Some(1)),
        (Some(true), Some(2)),
        (Some(false), Some(3)),
        (Some(true), Some(4)),
        (Some(true), Some(5)),
        (Some(true), Some(6)),
        (Some(false), Some(7)),
        (Some(false), Some(8)),
        (Some(false), Some(9)),
    ];
    assert_eq!(compute_streaks(&matches), (3, 3, Some(7), None));
}

#[test]
fn test_all_wins() {
    assert_eq!(
        compute_streaks(&[(Some(true), None); 5]),
        (5, 0, None, None)
    );
}

#[test]
fn test_all_losses() {
    assert_eq!(
        compute_streaks(&[(Some(false), None); 4]),
        (0, 4, None, None)
    );
}

#[test]
fn test_alternating() {
    let matches = [
        (Some(true), Some(1)),
        (Some(false), Some(2)),
        (Some(true), Some(3)),
        (Some(false), Some(4)),
    ];
    let (wins, losses, _, _) = compute_streaks(&matches);
    assert_eq!((wins, losses), (1, 1));
}

#[test]
fn test_empty_streaks() {
    assert_eq!(compute_streaks(&[]), (0, 0, None, None));
}

#[test]
fn test_none_won_resets_streaks() {
    let matches = [(Some(true), None), (None, None), (Some(true), None)];
    assert_eq!(compute_streaks(&matches).0, 1);
}

#[test]
fn test_breaker_hero_returned() {
    let matches = [
        (Some(true), Some(10)),
        (Some(true), Some(11)),
        (Some(false), Some(99)),
        (Some(true), Some(12)),
    ];
    let result = compute_streaks(&matches);
    assert_eq!(result.0, 2);
    assert_eq!(result.2, Some(99));
}

#[test]
fn test_deaths_zero_uses_max_1() {
    let rows: Vec<_> = (1..=5).map(|id| record_row(id, 10, 0, 5)).collect();
    let result = player_records(Some("TestPlayer"), &rows, 3).unwrap();
    assert_eq!(
        result
            .records
            .iter()
            .find(|record| record.stat_key == "kda_best")
            .unwrap()
            .value,
        Some(15.0)
    );
}

fn enriched_rows() -> Vec<WrappedRecordRow> {
    let facts = WrappedEnrichmentFacts {
        actions_per_min: Some(200),
        courier_kills: Some(3),
        pings: Some(100),
        rapier_count: Some(2),
        comeback: Some(15_000),
        throw: Some(8_000),
    };
    (1..=5)
        .map(|id| {
            let mut row = record_row(id, 10, 3, 5);
            row.enrichment = Some(facts.clone());
            row
        })
        .collect()
}

#[test]
fn test_enrichment_stats_extracted() {
    let result = player_records(Some("TestPlayer"), &enriched_rows(), 3).unwrap();
    let values: BTreeMap<_, _> = result
        .records
        .iter()
        .map(|record| (record.stat_key, record.value))
        .collect();
    assert_eq!(values.get("apm_best"), Some(&Some(200.0)));
    assert_eq!(values.get("rapiers_best"), Some(&Some(2.0)));
    assert!(
        [
            "courier_kills_best",
            "pings_worst",
            "comeback_best",
            "throw_worst"
        ]
        .into_iter()
        .all(|key| values.contains_key(key))
    );
}

#[test]
fn test_enrichment_data_missing_graceful() {
    let result = player_records(Some("TestPlayer"), &sample_record_rows(5), 3).unwrap();
    for key in [
        "apm_best",
        "courier_kills_best",
        "pings_worst",
        "rapiers_best",
        "comeback_best",
        "throw_worst",
    ] {
        let record = result
            .records
            .iter()
            .find(|record| record.stat_key == key)
            .unwrap();
        assert_eq!(record.value, None);
        assert_eq!(record.display_value, "N/A");
    }
}

#[test]
fn test_get_slides_returns_5_slides() {
    let result = player_records(Some("TestPlayer"), &enriched_rows(), 3).unwrap();
    let slides = result.get_slides();
    assert_eq!(slides.len(), 5);
    assert_eq!(
        slides.iter().map(|(title, _)| *title).collect::<Vec<_>>(),
        RECORD_SLIDE_TITLES
    );
}

#[test]
fn test_returns_matches_in_date_range() {
    let mut store = WrappedMatchStore::default();
    for (offset, kills) in [10, 11, 12].into_iter().enumerate() {
        let mut row = record_row(offset as i64 + 1, kills, 3, 5);
        row.discord_id = 100;
        row.guild_id = 42;
        store.insert(row);
    }
    let rows = store.player_year_matches(100, 42);
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows.iter().map(|row| row.kills).collect::<BTreeSet<_>>(),
        BTreeSet::from([10, 11, 12])
    );
    assert!(store.player_year_matches(100, 99_999).is_empty());
}

#[test]
fn test_basic_render_dimensions() {
    let records = player_records(Some("TestPlayer"), &sample_record_rows(5), 3)
        .unwrap()
        .records;
    let spec = records_slide_spec(&records);
    assert_eq!((spec.width, spec.height, spec.mode), (800, 600, "RGBA"));
}

#[test]
fn test_worst_records_render() {
    let records = player_records(Some("TestPlayer"), &sample_record_rows(5), 3)
        .unwrap()
        .records;
    let spec = records_slide_spec(&records);
    assert!(spec.worst_records > 0);
    assert_eq!((spec.width, spec.height), (800, 600));
}

#[test]
fn test_na_values_render() {
    let result = player_records(Some("TestPlayer"), &sample_record_rows(5), 3).unwrap();
    let spec = records_slide_spec(&result.records);
    assert!(spec.unavailable_records >= 2);
    assert_eq!((spec.width, spec.height), (800, 600));
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
        ["Gold Goblin", "Elo Inflation", "The Cliff", "No Life"]
            .into_iter()
            .all(|title| titles.contains(title))
    );
}

#[test]
fn test_award_dataclass() {
    let award = WrappedAward {
        category: "performance".to_owned(),
        title: "Gold Goblin".to_owned(),
        stat_name: "Best GPM".to_owned(),
        stat_value: "847 avg".to_owned(),
        discord_id: 123,
        discord_username: "TestUser".to_owned(),
        emoji: "💰".to_owned(),
        flavor_text: "Farming simulator champion".to_owned(),
    };
    assert_eq!(award.category, "performance");
    assert_eq!(award.title, "Gold Goblin");
    assert_eq!(award.emoji, "💰");
}

#[test]
fn test_get_server_wrapped_no_data() {
    assert_eq!(server_wrapped(false), None);
}

#[test]
fn test_get_player_wrapped_not_registered() {
    assert_eq!(player_wrapped_for_registration(None), None);
}
