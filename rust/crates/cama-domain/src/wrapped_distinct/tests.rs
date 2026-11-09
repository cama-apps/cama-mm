use chrono::{TimeZone, Utc};

use super::*;

#[test]
fn tracked_item_counts_keep_only_tracked_keys_and_rapiers() {
    let counts = tracked_item_counts([
        "magic_wand",
        "blink",
        "magic_wand",
        "rapier",
        "ward_observer",
    ]);
    assert_eq!(
        counts,
        BTreeMap::from([
            ("blink".to_owned(), 1),
            ("magic_wand".to_owned(), 2),
            ("rapier".to_owned(), 1),
        ])
    );
    assert!(tracked_item_counts(["ward_observer"]).is_empty());
}

const SINGLE_GAME_KEYS: &[&str] = &[
    "best_game",
    "kill_loss",
    "throw",
    "comeback",
    "rapiers",
    "upset_win",
    "upset_loss",
    "win_streak",
    "loss_streak",
];

fn game(id: i64, player: i64, side: &str, won: bool) -> DistinctMatch {
    DistinctMatch {
        match_id: id,
        discord_id: player,
        side: Some(side.to_string()),
        won: Some(won),
        finished_at: Some(1_700_000_000 + id * 3_600),
        ..DistinctMatch::default()
    }
}

fn players(count: i64) -> Vec<DistinctPlayer> {
    (1..=count)
        .map(|discord_id| DistinctPlayer {
            discord_id,
            timezone: None,
        })
        .collect()
}

/// `count` players with `games_each` alternating wins and losses; `shape`
/// edits each game given (player, game index).
fn population(
    count: i64,
    games_each: i64,
    shape: impl Fn(i64, i64, &mut DistinctMatch),
) -> Vec<DistinctMatch> {
    let mut rows = Vec::new();
    for player in 1..=count {
        for index in 0..games_each {
            let mut row = game(player * 100 + index, player, "radiant", index % 2 == 0);
            shape(player, index, &mut row);
            rows.push(row);
        }
    }
    rows
}

fn metrics(stories: &[DistinctStory]) -> Vec<&'static str> {
    stories.iter().map(|story| story.metric).collect()
}

fn story_for<'a>(stories: &'a [DistinctStory], key: &str) -> Option<&'a DistinctStory> {
    stories.iter().find(|story| story.metric == key)
}

fn contrast_rows() -> Vec<DistinctMatch> {
    population(20, 8, |player, _, row| {
        row.enriched = true;
        row.deaths = Some(if player == 1 { 12 } else { 4 + player % 4 });
        row.wards = Some(if player == 2 { 25 } else { 5 + player % 3 });
    })
}

#[test]
fn contrasting_players_get_different_top_stories() {
    let rows = contrast_rows();
    let roster = players(20);

    let feeder = select_stories(&distinct_stories(&rows, &roster, 1, 3), 6);
    let warder = select_stories(&distinct_stories(&rows, &roster, 2, 3), 6);

    let deaths = story_for(&feeder, "deaths_per_game").expect("feeder deaths story");
    assert_eq!(deaths.direction, Direction::Roast);
    assert_eq!(deaths.tier, Tier::Extreme);
    let wards = story_for(&warder, "wards").expect("warder wards story");
    assert_eq!(wards.direction, Direction::Popoff);
    assert_ne!(feeder[0].metric, warder[0].metric);
}

#[test]
fn unparsed_player_still_gets_stories() {
    let rows = population(20, 8, |player, index, row| {
        row.expected_win = Some(0.5);
        row.rating_delta = Some(5.0);
        if player == 1 {
            row.won = Some(index != 0);
            row.expected_win = Some(if index == 3 { 0.2 } else { 0.5 });
        }
    });
    let stories = distinct_stories(&rows, &players(20), 1, 3);

    assert!(stories.len() >= 2, "{stories:?}");
    assert!(story_for(&stories, "win_rate").is_some());
    assert!(story_for(&stories, "upset_win").is_some());
}

#[test]
fn nulls_are_excluded_not_zero() {
    let rows = population(20, 6, |player, _, row| {
        row.enriched = true;
        row.wards = if player == 1 {
            None
        } else {
            Some(if player == 2 { 25 } else { 5 + player % 3 })
        };
    });
    let roster = players(20);

    assert!(story_for(&distinct_stories(&rows, &roster, 1, 3), "wards").is_none());
    let top = distinct_stories(&rows, &roster, 2, 3);
    let wards = story_for(&top, "wards").expect("wards story");
    assert_eq!(wards.rank.map(|rank| rank.of), Some(19));
}

#[test]
fn ties_never_claim_first() {
    let rows = population(30, 5, |player, _, row| {
        row.enriched = true;
        row.courier_kills = Some(if player <= 25 { 0 } else { player - 25 });
    });
    let roster = players(30);

    for player in 1..=25 {
        let stories = distinct_stories(&rows, &roster, player, 3);
        assert!(
            story_for(&stories, "couriers").is_none(),
            "player {player}: {stories:?}"
        );
    }
    for player in 1..=30 {
        for story in distinct_stories(&rows, &roster, player, 3) {
            if story.metric == "couriers" && story.value == 0.0 {
                panic!("zero-courier story: {story:?}");
            }
        }
    }
}

#[test]
fn shared_extreme_never_gets_rank_one_or_extreme() {
    let rows = population(30, 8, |player, _, row| {
        row.enriched = true;
        row.deaths = Some(if player <= 3 { 15 } else { 4 + player % 4 });
    });
    let stories = distinct_stories(&rows, &players(30), 1, 3);
    let deaths = story_for(&stories, "deaths_per_game").expect("shared deaths story");

    assert_eq!(deaths.tier, Tier::Strong);
    assert!(deaths.rank.is_none());
}

#[test]
fn a_two_way_tie_at_the_top_is_not_extreme() {
    let rows = population(30, 8, |player, _, row| {
        row.enriched = true;
        row.deaths = Some(if player <= 2 { 15 } else { 4 + player % 4 });
    });
    for target in [1, 2] {
        let stories = distinct_stories(&rows, &players(30), target, 3);
        let deaths = story_for(&stories, "deaths_per_game").expect("tied deaths story");

        assert_eq!(deaths.tier, Tier::Strong, "player {target}");
        assert!(deaths.rank.is_none(), "player {target}");
    }
}

#[test]
fn stories_know_which_end_of_the_table_they_rank_from() {
    // Player 1 dies the most and wins the least; player 2 dies the least.
    let rows = population(30, 8, |player, index, row| {
        row.deaths = Some(match player {
            1 => 15,
            2 => 1,
            _ => 4 + player % 4,
        });
        if player == 1 {
            row.won = Some(index == 0);
        }
    });
    let roster = players(30);
    let feeder = distinct_stories(&rows, &roster, 1, 3);

    let deaths = story_for(&feeder, "deaths_per_game").expect("most deaths");
    assert_eq!(
        (deaths.direction, deaths.end),
        (Direction::Roast, TableEnd::Top)
    );
    assert_eq!(deaths.rank.map(|rank| rank.rank), Some(1));
    let losing = story_for(&feeder, "win_rate").expect("lowest win rate");
    assert_eq!(
        (losing.direction, losing.end),
        (Direction::Roast, TableEnd::Bottom)
    );
    assert_eq!(losing.rank.map(|rank| rank.rank), Some(1));
    let survivor = distinct_stories(&rows, &roster, 2, 3);
    let fewest = story_for(&survivor, "deaths_per_game").expect("fewest deaths");
    assert_eq!(
        (fewest.direction, fewest.end),
        (Direction::Popoff, TableEnd::Bottom)
    );
}

#[test]
fn non_finite_values_are_skipped_like_unknowns() {
    let rows = population(20, 6, |player, index, row| {
        row.enriched = true;
        row.stuns = Some((2 + player % 3) as f64);
        row.actions_per_min = Some((150 + player) as f64);
        if player == 1 {
            row.stuns = Some(if index == 5 { f64::NAN } else { 9.0 });
            row.actions_per_min = Some(if index == 5 { f64::INFINITY } else { 400.0 });
        }
    });
    let stories = distinct_stories(&rows, &players(20), 1, 3);

    let stuns = story_for(&stories, "stuns").expect("stuns story");
    assert_eq!((stuns.value, stuns.display.as_str()), (9.0, "9.0"));
    let apm = story_for(&stories, "apm").expect("APM story");
    assert_eq!((apm.value, apm.display.as_str()), (400.0, "400"));
}

#[test]
fn an_unknown_result_breaks_a_streak() {
    // Three wins, a game with no result, then two more wins.
    let results = [
        Some(true),
        Some(true),
        Some(true),
        None,
        Some(true),
        Some(true),
        Some(false),
    ];
    let mut rows = population(6, 6, |_, _, _| {});
    rows.retain(|row| row.discord_id != 1);
    for (index, won) in results.into_iter().enumerate() {
        let mut row = game(100 + index as i64, 1, "radiant", true);
        row.won = won;
        rows.push(row);
    }
    let stories = distinct_stories(&rows, &players(6), 1, 3);

    let streak = story_for(&stories, "win_streak").expect("win streak story");
    assert_eq!(streak.value, 3.0);
    assert_eq!(streak.match_id, Some(102));
}

#[test]
fn small_population_uses_records_only() {
    let rows = population(6, 8, |player, index, row| {
        row.enriched = true;
        row.kills = Some(if player == 1 { 20 } else { 5 });
        row.deaths = Some(2);
        row.assists = Some(10);
        row.wards = Some(8);
        row.hero_id = Some(index % 3);
        row.expected_win = Some(if player == 1 { 0.2 } else { 0.5 });
        row.throw = Some(9_000.0);
        row.comeback = Some(6_000.0);
        if player == 1 {
            row.won = Some(true);
        }
    });
    let stories = distinct_stories(&rows, &players(6), 1, 3);

    assert!(!stories.is_empty());
    for story in &stories {
        assert!(SINGLE_GAME_KEYS.contains(&story.metric), "{story:?}");
        assert!(story.rank.is_none(), "{story:?}");
    }
}

#[test]
fn below_minimum_games_returns_empty() {
    let rows = population(20, 6, |_, _, _| {});
    let mut rows = rows;
    rows.retain(|row| row.discord_id != 1 || row.match_id % 100 < 2);

    assert!(distinct_stories(&rows, &players(20), 1, 3).is_empty());
}

fn candidate(
    metric: &'static str,
    theme: Theme,
    direction: Direction,
    score: f64,
) -> DistinctStory {
    DistinctStory {
        metric,
        theme,
        direction,
        tier: Tier::Strong,
        value: 1.0,
        display: "1".to_string(),
        rank: None,
        end: TableEnd::Top,
        hero_id: None,
        match_id: None,
        item: None,
        score,
    }
}

#[test]
fn selection_balances_roasts_and_popoffs() {
    let candidates = vec![
        candidate("a", Theme::Combat, Direction::Roast, 1.5),
        candidate("b", Theme::Farm, Direction::Roast, 1.4),
        candidate("c", Theme::Map, Direction::Roast, 1.3),
        candidate("d", Theme::Chaos, Direction::Roast, 1.2),
        candidate("e", Theme::Clutch, Direction::Roast, 1.1),
        candidate("f", Theme::Time, Direction::Popoff, 0.5),
        candidate("g", Theme::Heroes, Direction::Popoff, 0.4),
        candidate("h", Theme::Items, Direction::Popoff, 0.3),
    ];
    let picked = select_stories(&candidates, 6);

    assert_eq!(picked.len(), 6);
    let roasts = picked
        .iter()
        .filter(|story| story.direction == Direction::Roast)
        .count();
    let popoffs = picked.len() - roasts;
    assert!(
        roasts >= 2 && popoffs >= 2,
        "{roasts} roasts, {popoffs} popoffs"
    );
    assert_eq!(picked[0].metric, "a");
}

#[test]
fn selection_prefers_one_story_per_theme() {
    let candidates = vec![
        candidate("a", Theme::Combat, Direction::Roast, 1.5),
        candidate("b", Theme::Combat, Direction::Popoff, 1.4),
        candidate("c", Theme::Farm, Direction::Popoff, 0.3),
    ];
    let picked = select_stories(&candidates, 2);

    assert_eq!(metrics(&picked), vec!["a", "c"]);
}

fn late_rows(timezone_hour_utc: u32, minute: u32, target_games: i64) -> Vec<DistinctMatch> {
    population(20, 8, |player, index, row| {
        let late_games = if player == 1 {
            target_games
        } else {
            3 + player % 3
        };
        let utc_hour = match (index < late_games, player) {
            (false, _) => 18,
            (true, 1) => timezone_hour_utc,
            (true, _) => 6,
        };
        let day = 1 + index as u32;
        row.finished_at = Some(
            Utc.with_ymd_and_hms(
                2026,
                1,
                day,
                utc_hour,
                if player == 1 { minute } else { 30 },
                0,
            )
            .unwrap()
            .timestamp(),
        );
    })
}

fn target_roster(timezone: &str) -> Vec<DistinctPlayer> {
    let mut roster = players(20);
    roster[0].timezone = Some(timezone.to_string());
    roster
}

#[test]
fn night_owl_uses_player_timezone() {
    let rows = late_rows(8, 30, 8);

    let pacific = distinct_stories(&rows, &target_roster("America/Los_Angeles"), 1, 3);
    let owl = story_for(&pacific, "night_owl").expect("late games in Pacific time");
    assert_eq!(owl.value, 8.0);
    assert_eq!(owl.display, "12:30 AM");

    let invalid = distinct_stories(&rows, &target_roster("Not/AZone"), 1, 3);
    let owl = story_for(&invalid, "night_owl").expect("default timezone is Eastern");
    assert_eq!(owl.display, "3:30 AM");
}

#[test]
fn night_owl_only_counts_local_late_hours() {
    // 11:00 UTC is 3:00 AM Pacific but 6:00 AM Eastern.
    let rows = late_rows(11, 0, 8);

    let pacific = distinct_stories(&rows, &target_roster("America/Los_Angeles"), 1, 3);
    assert_eq!(
        story_for(&pacific, "night_owl").map(|story| story.display.as_str()),
        Some("3:00 AM")
    );
    let eastern = distinct_stories(&rows, &target_roster("Not/AZone"), 1, 3);
    assert!(story_for(&eastern, "night_owl").is_none());
}

#[test]
fn people_year_scopes_to_rows() {
    let mut rows = Vec::new();
    for id in 1..=6 {
        rows.push(game(id, 1, "radiant", true));
        rows.push(game(id, 7, "radiant", true));
    }
    for id in 7..=12 {
        rows.push(game(id, 1, "dire", false));
        rows.push(game(id, 9, "radiant", true));
    }
    let year = people_year(&rows, 1);

    let duo = year.duo.expect("duo");
    assert_eq!((duo.discord_id, duo.games, duo.wins), (7, 6, 6));
    let nemesis = year.nemesis.expect("nemesis");
    assert_eq!((nemesis.discord_id, nemesis.games, nemesis.wins), (9, 6, 0));
    assert!(year.cursed_duo.is_none());
    assert!(year.punching_bag.is_none());
}

#[test]
fn people_year_needs_five_games_together() {
    let mut rows = Vec::new();
    for id in 1..=4 {
        rows.push(game(id, 1, "radiant", true));
        rows.push(game(id, 7, "radiant", true));
    }
    assert_eq!(people_year(&rows, 1), PeopleYear::default());
}

#[test]
fn people_year_finds_cursed_duo_and_punching_bag() {
    let mut rows = Vec::new();
    for id in 1..=6 {
        rows.push(game(id, 1, "radiant", id == 1));
        rows.push(game(id, 7, "radiant", id == 1));
    }
    for id in 7..=12 {
        rows.push(game(id, 1, "radiant", true));
        rows.push(game(id, 9, "dire", false));
    }
    let year = people_year(&rows, 1);

    assert_eq!(year.cursed_duo.map(|pair| pair.discord_id), Some(7));
    assert_eq!(year.punching_bag.map(|pair| pair.discord_id), Some(9));
}

#[test]
fn people_roles_follow_the_record_not_just_the_count() {
    let mut rows = Vec::new();
    // Player 8 sat across from the target ten times and lost seven of them.
    for id in 1..=10 {
        rows.push(game(id, 1, "radiant", id <= 7));
        rows.push(game(id, 8, "dire", id > 7));
    }
    // Player 9 met the target five times and won three.
    for id in 11..=15 {
        rows.push(game(id, 1, "radiant", id <= 12));
        rows.push(game(id, 9, "dire", id > 12));
    }
    // Player 7 shared a team six times and lost four of them.
    for id in 16..=21 {
        rows.push(game(id, 1, "radiant", id <= 17));
        rows.push(game(id, 7, "radiant", id <= 17));
    }
    let year = people_year(&rows, 1);

    // More losses to player 8 than to player 9, but a winning record.
    assert_eq!(year.nemesis.map(|pair| pair.discord_id), Some(9));
    assert_eq!(year.punching_bag.map(|pair| pair.discord_id), Some(8));
    // Two wins with player 7 are not a duo when four games were lost.
    assert!(year.duo.is_none());
    assert_eq!(year.cursed_duo.map(|pair| pair.discord_id), Some(7));

    // An even record fits neither side of a rivalry.
    let even = (1..=6)
        .flat_map(|id| [game(id, 1, "radiant", id <= 3), game(id, 8, "dire", id > 3)])
        .collect::<Vec<_>>();
    let year = people_year(&even, 1);
    assert!(year.nemesis.is_none() && year.punching_bag.is_none());
}

#[test]
fn deterministic_for_shuffled_rows() {
    let rows = contrast_rows();
    let mut reversed = rows.clone();
    reversed.reverse();
    let roster = players(20);

    for target in [1, 2, 5] {
        assert_eq!(
            distinct_stories(&rows, &roster, target, 3),
            distinct_stories(&reversed, &roster, target, 3)
        );
    }
}

#[test]
fn catalog_keys_match_the_published_list() {
    let mut keys: Vec<&str> = CATALOG.iter().map(|row| row.key).chain(["item"]).collect();
    keys.sort_unstable();
    keys.dedup();
    let mut published = METRIC_KEYS.to_vec();
    published.sort_unstable();
    assert_eq!(keys, published);
    assert_eq!(METRIC_KEYS.iter().filter(|key| **key == "item").count(), 1);
    for key in METRIC_KEYS {
        assert!(!metric_directions(key).is_empty(), "{key}");
    }
    assert!(metric_directions("nope").is_empty());
}

#[test]
fn emitted_directions_are_declared() {
    let rows = contrast_rows();
    for target in 1..=20 {
        for story in distinct_stories(&rows, &players(20), target, 3) {
            assert!(
                metric_directions(story.metric).contains(&story.direction),
                "{story:?}"
            );
        }
    }
}

#[test]
fn item_stories_name_the_item() {
    let rows = population(20, 6, |player, _, row| {
        row.enriched = true;
        let wands = if player == 1 { 4 } else { 1 + player % 3 };
        row.items = Some(vec![
            ("magic_wand".to_string(), wands),
            ("blink".to_string(), 1),
        ]);
    });
    let stories = distinct_stories(&rows, &players(20), 1, 3);
    let items: Vec<_> = stories
        .iter()
        .filter(|story| story.metric == "item")
        .collect();

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].item.as_deref(), Some("magic_wand"));
    assert_eq!(items[0].direction, Direction::Roast);
}

#[test]
fn parsed_games_without_tracked_items_lower_the_rate() {
    // Player 1 bought four wands in each of four games, none in a fifth that
    // was parsed, and has a sixth whose purchase log is unknown.
    let rows = population(20, 6, |player, index, row| {
        row.enriched = true;
        row.items = Some(vec![("magic_wand".to_string(), 1 + player % 3)]);
        if player == 1 {
            row.items = match index {
                4 => Some(Vec::new()),
                5 => None,
                _ => Some(vec![("magic_wand".to_string(), 4)]),
            };
        }
    });
    let stories = distinct_stories(&rows, &players(20), 1, 3);
    let wands = story_for(&stories, "item").expect("wand story");

    assert_eq!(wands.item.as_deref(), Some("magic_wand"));
    // Sixteen wands over five known logs; the unknown log is left out.
    assert_eq!(wands.value, 3.2);

    // The empty log is the fifth known game; without it the minimum fails.
    let mut short = rows.clone();
    short.retain(|row| row.discord_id != 1 || row.match_id % 100 != 4);
    assert!(story_for(&distinct_stories(&short, &players(20), 1, 3), "item").is_none());
}

/// One of player 7's games, finished on `day` of March 2026 at 20:00 UTC.
fn mine(id: i64, day: u32, hero: i64, won: Option<bool>) -> DistinctMatch {
    DistinctMatch {
        match_id: id,
        discord_id: 7,
        side: Some("radiant".to_owned()),
        won,
        hero_id: Some(hero),
        finished_at: Some(
            Utc.with_ymd_and_hms(2026, 3, day, 20, 0, 0)
                .unwrap()
                .timestamp(),
        ),
        ..DistinctMatch::default()
    }
}

fn record<'a>(records: &'a [PersonalRecord], key: &str) -> Option<&'a PersonalRecord> {
    records.iter().find(|record| record.key == key)
}

type RecordLine = (
    i64,
    i64,
    Option<bool>,
    Option<(i64, i64, i64, i64, i64)>,
    i64,
    Option<bool>,
    Option<i64>,
);

/// Five games of player 7, one of them a 27-kill Invoker loss that finished
/// at 03:30 UTC on March 4 (still March 3 in New York), plus a stranger's
/// bigger numbers that must never count.
fn record_rows() -> (Vec<DistinctMatch>, i64) {
    let late = Utc
        .with_ymd_and_hms(2026, 3, 4, 3, 30, 0)
        .unwrap()
        .timestamp();
    // Id, hero, result, (kills, deaths, assists, gpm, damage), duration,
    // first blood, Roshans.
    let lines: [RecordLine; 5] = [
        (
            1,
            1,
            Some(true),
            Some((6, 4, 10, 450, 12_000)),
            2_100,
            Some(false),
            Some(0),
        ),
        (
            2,
            74,
            Some(false),
            Some((27, 5, 8, 610, 41_500)),
            2_700,
            Some(true),
            Some(1),
        ),
        (
            3,
            1,
            Some(false),
            Some((5, 18, 12, 380, 14_000)),
            4_692,
            None,
            None,
        ),
        (
            4,
            1,
            Some(true),
            Some((8, 3, 9, 500, 15_000)),
            1_065,
            Some(true),
            Some(2),
        ),
        (5, 1, Some(true), None, 2_400, None, None),
    ];
    let mut rows = Vec::new();
    for (id, hero, won, combat, duration, first_blood, roshans) in lines {
        let mut row = mine(id, id as u32, hero, won);
        if let Some((kills, deaths, assists, gpm, damage)) = combat {
            row.kills = Some(kills);
            row.deaths = Some(deaths);
            row.assists = Some(assists);
            row.gpm = Some(gpm);
            row.hero_damage = Some(damage);
        }
        row.duration_seconds = Some(duration);
        row.first_blood = first_blood;
        row.roshans = roshans;
        rows.push(row);
    }
    rows[1].finished_at = Some(late);
    let mut stranger = game(9, 8, "dire", true);
    stranger.kills = Some(40);
    stranger.deaths = Some(30);
    stranger.duration_seconds = Some(6_000);
    rows.push(stranger);
    (rows, late)
}

#[test]
fn records_pick_personal_extremes_with_hero_and_date() {
    let (rows, late) = record_rows();
    let records = personal_records(&rows, 7);

    let kills = record(&records, "most_kills").expect("kill record");
    assert_eq!(
        (kills.value, kills.display.as_str(), kills.direction),
        (27, "27 kills", Direction::Popoff)
    );
    assert_eq!((kills.hero_id, kills.match_id), (Some(74), 2));
    assert_eq!((kills.finished_at, kills.won), (Some(late), Some(false)));
    // The unknown fifth game is left out of the mean, not counted as zero.
    assert!((kills.score - (27.0 - 11.5) / 11.5).abs() < 1e-9);

    let deaths = record(&records, "most_deaths").expect("death record");
    assert_eq!(
        (deaths.display.as_str(), deaths.direction),
        ("18 deaths", Direction::Roast)
    );
    assert_eq!((deaths.hero_id, deaths.match_id), (Some(1), 3));
    let long = record(&records, "longest_game").expect("longest game");
    assert_eq!((long.value, long.display.as_str()), (4_692, "78:12"));
    assert_eq!((long.direction, long.match_id), (Direction::Roast, 3));
    // The fastest of three known wins, scored below their mean.
    let fast = record(&records, "fastest_win").expect("fastest win");
    assert_eq!((fast.display.as_str(), fast.match_id), ("17:45", 4));
    assert_eq!(fast.won, Some(true));
    assert!((fast.score - (1_855.0 - 1_065.0) / 1_855.0).abs() < 1e-9);
    assert_eq!(
        record(&records, "most_damage").unwrap().display,
        "41,500 damage"
    );
    assert_eq!(record(&records, "best_gpm").unwrap().display, "610 GPM");
    // Counts score by count per known game and point at the latest game
    // that counted.
    let bloods = record(&records, "first_bloods").expect("first bloods");
    assert_eq!(
        (bloods.value, bloods.display.as_str()),
        (2, "2 first bloods")
    );
    assert_eq!(bloods.match_id, 4);
    assert!((bloods.score - 2.0 / 3.0).abs() < 1e-9);
    assert_eq!(record(&records, "roshans").unwrap().display, "3 Roshans");
    // No ward was ever recorded, so there is no ward record.
    assert!(record(&records, "most_wards").is_none());
    assert!(
        records
            .windows(2)
            .all(|pair| pair[0].score >= pair[1].score)
    );
    for key in records.iter().map(|record| record.key) {
        assert!(RECORD_KEYS.iter().any(|(known, _)| *known == key), "{key}");
    }

    // The date is the player's own: New York by default.
    assert_eq!(month_day(late, None).as_deref(), Some("Mar 3"));
    assert_eq!(
        month_day(late, Some("Europe/Berlin")).as_deref(),
        Some("Mar 4")
    );

    // The two strongest, one roast and one popoff.
    let keys = |picked: Vec<PersonalRecord>| picked.iter().map(|r| r.key).collect::<Vec<_>>();
    assert_eq!(
        keys(select_records(&records, 2)),
        ["most_deaths", "most_kills"]
    );
    let no_deaths = records
        .iter()
        .filter(|record| record.key != "most_deaths")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        keys(select_records(&no_deaths, 2)),
        ["most_kills", "longest_game"]
    );
    let popoffs = records
        .iter()
        .filter(|record| record.direction == Direction::Popoff)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        keys(select_records(&popoffs, 2)),
        ["most_kills", "most_damage"]
    );
}

#[test]
fn records_need_three_known_games() {
    let mut rows = Vec::new();
    for (id, kills, wards, won, first_blood) in [
        (1, 2, Some(3), Some(true), Some(true)),
        (2, 19, Some(12), Some(true), Some(true)),
        (3, 4, None, None, None),
    ] {
        let mut row = mine(id, id as u32, 1, won);
        row.kills = Some(kills);
        row.deaths = Some(5);
        row.wards = wards;
        row.first_blood = first_blood;
        row.duration_seconds = Some(1_800 + id * 300);
        rows.push(row);
    }
    let records = personal_records(&rows, 7);
    assert_eq!(record(&records, "most_kills").map(|r| r.value), Some(19));
    assert!(record(&records, "longest_game").is_some());
    // Two known values, two known wins, two known first-blood games.
    for key in ["most_wards", "fastest_win", "first_bloods"] {
        assert!(record(&records, key).is_none(), "{key}");
    }
    // Five deaths every game is the average, not a record.
    assert!(record(&records, "most_deaths").is_none());
    // Two games are too few for anything.
    assert!(personal_records(&rows[..2], 7).is_empty());
    assert!(personal_records(&rows, 8).is_empty());

    // A worst game of two deaths, or one first blood, is no story.
    let quiet = (1..=3)
        .map(|id| {
            let mut row = mine(id, id as u32, 1, Some(true));
            row.deaths = Some(i64::from(id == 3) * 2);
            row.first_blood = Some(id == 2);
            row
        })
        .collect::<Vec<_>>();
    assert!(personal_records(&quiet, 7).is_empty());
}

/// `wins` wins, `losses` losses and `unknown` unrecorded results on `hero`.
fn hero_games(
    rows: &mut Vec<DistinctMatch>,
    hero: i64,
    wins: usize,
    losses: usize,
    unknown: usize,
) {
    let results = std::iter::repeat_n(Some(true), wins)
        .chain(std::iter::repeat_n(Some(false), losses))
        .chain(std::iter::repeat_n(None, unknown));
    for result in results {
        let id = rows.len() as i64 + 1;
        rows.push(mine(id, 1 + (id % 28) as u32, hero, result));
    }
}

#[test]
fn hero_year_best_and_nemesis_thresholds() {
    let mut rows = Vec::new();
    hero_games(&mut rows, 1, 3, 2, 0); // 60% in five: the best
    hero_games(&mut rows, 2, 4, 0, 0); // perfect, but four games
    hero_games(&mut rows, 3, 11, 9, 0); // exactly 55%, a lower rate
    hero_games(&mut rows, 4, 2, 4, 0); // 33%: the hero that hates you
    hero_games(&mut rows, 5, 2, 3, 0); // exactly 40%, a higher rate
    hero_games(&mut rows, 6, 1, 3, 3); // 25%, but four known results
    let mut stranger = mine(500, 1, 9, Some(false));
    stranger.discord_id = 8;
    rows.extend(std::iter::repeat_n(stranger, 9));

    let line = |hero_id, games, wins| HeroLine {
        hero_id,
        games,
        wins,
    };
    let year = hero_year(&rows, 7);
    assert_eq!(year.best, Some(line(1, 5, 3)));
    assert_eq!(year.nemesis_hero, Some(line(4, 6, 2)));

    // Both thresholds are inclusive.
    let mut edges = Vec::new();
    hero_games(&mut edges, 3, 11, 9, 0);
    hero_games(&mut edges, 5, 2, 3, 0);
    let year = hero_year(&edges, 7);
    assert_eq!(year.best, Some(line(3, 20, 11)));
    assert_eq!(year.nemesis_hero, Some(line(5, 5, 2)));
    // Half the games won is not a best hero; 41% is not a nemesis.
    let mut middling = Vec::new();
    hero_games(&mut middling, 3, 10, 10, 0);
    hero_games(&mut middling, 5, 7, 10, 0);
    let year = hero_year(&middling, 7);
    assert!(year.best.is_none() && year.nemesis_hero.is_none());
}

#[test]
fn side_split_requires_gap_and_sample() {
    let sides = |radiant: (i64, i64), dire: (i64, i64)| {
        let mut rows = Vec::new();
        for (side, (games, wins)) in [("radiant", radiant), ("dire", dire)] {
            for index in 0..games {
                let id = rows.len() as i64 + 1;
                let mut row = mine(id, 1 + (id % 28) as u32, 1, Some(index < wins));
                row.side = Some(side.to_owned());
                rows.push(row);
            }
        }
        // Unknown sides and results never count.
        let mut sideless = mine(900, 2, 1, Some(true));
        sideless.side = None;
        rows.push(sideless);
        rows.push(mine(901, 2, 1, None));
        hero_year(&rows, 7)
    };
    let even_gap = sides((5, 4), (5, 3));
    assert_eq!((even_gap.radiant, even_gap.dire), ((5, 4), (5, 3)));
    // 80% against 60%: exactly twenty points.
    assert_eq!(even_gap.side_split(), Some("Radiant"));
    assert_eq!(sides((10, 5), (10, 7)).side_split(), Some("Dire"));
    // Too few games on one side, or too small a gap.
    assert_eq!(sides((5, 4), (4, 0)).side_split(), None);
    assert_eq!(sides((10, 7), (10, 6)).side_split(), None);
    assert_eq!(sides((0, 0), (0, 0)).side_split(), None);
}

#[test]
fn records_name_their_distinct_twins() {
    for (record, metric) in [
        ("most_kills", "kills_per_game"),
        ("most_deaths", "deaths_per_game"),
        ("best_gpm", "gpm"),
        ("most_damage", "hero_damage"),
        ("most_wards", "wards"),
    ] {
        assert_eq!(record_twin(record), Some(metric), "{record}");
        assert!(METRIC_KEYS.contains(&metric), "{metric}");
    }
    for key in [
        "most_assists",
        "longest_game",
        "fastest_win",
        "first_bloods",
        "roshans",
        "nope",
    ] {
        assert_eq!(record_twin(key), None, "{key}");
    }
}

#[test]
fn records_meet_their_floors() {
    // One standout game among two quiet ones, at a given ward and damage count.
    let standout = |wards: i64, damage: i64| {
        let rows = (1..=3)
            .map(|id| {
                let mut row = mine(id, id as u32, 1, Some(true));
                row.wards = Some(if id == 2 { wards } else { 2 });
                row.hero_damage = Some(if id == 2 { damage } else { 5_000 });
                row
            })
            .collect::<Vec<_>>();
        let records = personal_records(&rows, 7);
        (
            record(&records, "most_wards").is_some(),
            record(&records, "most_damage").is_some(),
        )
    };
    assert_eq!(standout(9, 19_999), (false, false));
    assert_eq!(standout(10, 20_000), (true, true));
}

#[test]
fn hero_year_ties_go_to_more_games_then_the_lower_id() {
    let mut rows = Vec::new();
    hero_games(&mut rows, 9, 6, 4, 0); // 60% over ten
    hero_games(&mut rows, 3, 3, 2, 0); // 60% over five
    hero_games(&mut rows, 8, 2, 8, 0); // 20% over ten
    hero_games(&mut rows, 4, 1, 4, 0); // 20% over five
    let year = hero_year(&rows, 7);
    assert_eq!(year.best.map(|line| line.hero_id), Some(9));
    assert_eq!(year.nemesis_hero.map(|line| line.hero_id), Some(8));

    let mut even = Vec::new();
    hero_games(&mut even, 6, 3, 2, 0);
    hero_games(&mut even, 2, 3, 2, 0);
    hero_games(&mut even, 7, 1, 4, 0);
    hero_games(&mut even, 5, 1, 4, 0);
    let year = hero_year(&even, 7);
    assert_eq!(year.best.map(|line| line.hero_id), Some(2));
    assert_eq!(year.nemesis_hero.map(|line| line.hero_id), Some(5));
}
