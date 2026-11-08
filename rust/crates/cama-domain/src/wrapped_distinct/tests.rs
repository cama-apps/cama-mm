use chrono::{TimeZone, Utc};

use super::*;

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
        row.items = vec![("magic_wand".to_string(), wands), ("blink".to_string(), 1)];
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
