//! Personal records and hero stories: a player's year measured against their
//! own games, not the server's.
//!
//! Unknown data is never zero: a game that lacks a measure is left out of
//! that record and of the average it is compared with.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use chrono::DateTime;

use super::{Direction, DistinctMatch, commas, zone_for};
use crate::formatting::count_noun;

use Direction::{Popoff, Roast};

/// Every personal record and the way it reads.
pub const RECORD_KEYS: [(&str, Direction); 10] = [
    ("most_kills", Popoff),
    ("most_deaths", Roast),
    ("most_assists", Popoff),
    ("best_gpm", Popoff),
    ("most_damage", Popoff),
    ("most_wards", Popoff),
    ("longest_game", Roast),
    ("fastest_win", Popoff),
    ("first_bloods", Popoff),
    ("roshans", Popoff),
];

/// The distinct story that measures the same thing as a record, if any.
/// The distinct story that measures the same thing as a record, if any: a
/// deck that shows one should not show the other.
#[must_use]
pub fn record_twin(key: &str) -> Option<&'static str> {
    match key {
        "most_kills" => Some("kills_per_game"),
        "most_deaths" => Some("deaths_per_game"),
        "best_gpm" => Some("gpm"),
        "most_damage" => Some("hero_damage"),
        "most_wards" => Some("wards"),
        _ => None,
    }
}

/// Records counted across the year rather than read off one game.
pub const COUNT_RECORDS: [&str; 2] = ["first_bloods", "roshans"];

/// A record needs this many games that know its measure.
const MIN_RECORD_GAMES: usize = 3;
/// Games with known results a hero or a side needs before it tells a story.
const MIN_HERO_GAMES: i64 = 5;
/// Best hero at or above this win rate, the hero that hates you at or below
/// the other, in percent.
const BEST_HERO_RATE: i64 = 55;
const NEMESIS_HERO_RATE: i64 = 40;
/// Percentage points between Radiant and Dire that make a split.
const SIDE_SPLIT_POINTS: i64 = 20;

/// One personal record. `match_id`, `hero_id`, `finished_at` and `won`
/// describe the record game; for a count record (`COUNT_RECORDS`), the
/// latest game that counted.
#[derive(Clone, Debug, PartialEq)]
pub struct PersonalRecord {
    pub key: &'static str,
    pub direction: Direction,
    pub value: i64,
    /// Ready to show: `"27 kills"`, `"78:12"`, `"3 Roshans"`.
    pub display: String,
    pub hero_id: Option<i64>,
    pub match_id: i64,
    pub finished_at: Option<i64>,
    pub won: Option<bool>,
    /// How far the record sits from the player's own per-game average, in
    /// multiples of that average; a count per known game for counts.
    pub score: f64,
}

/// A hero's record for the year, counting games with a known result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeroLine {
    pub hero_id: i64,
    pub games: i64,
    pub wins: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeroYear {
    /// Highest win rate, at least 55% over five games or more.
    pub best: Option<HeroLine>,
    /// Lowest win rate, at most 40% over five games or more.
    pub nemesis_hero: Option<HeroLine>,
    /// `(games, wins)` on each side, counting known sides and results.
    pub radiant: (i64, i64),
    pub dire: (i64, i64),
}

impl HeroYear {
    /// The side the player wins more on, `"Radiant"` or `"Dire"`, when both
    /// sides have five games and their win rates are twenty points apart.
    #[must_use]
    pub fn side_split(&self) -> Option<&'static str> {
        let ((radiant_games, radiant_wins), (dire_games, dire_wins)) = (self.radiant, self.dire);
        if radiant_games < MIN_HERO_GAMES || dire_games < MIN_HERO_GAMES {
            return None;
        }
        // Cross-multiplied, so the comparison stays exact.
        let gap = radiant_wins * dire_games - dire_wins * radiant_games;
        if gap.abs() * 100 < SIDE_SPLIT_POINTS * radiant_games * dire_games {
            return None;
        }
        Some(if gap > 0 { "Radiant" } else { "Dire" })
    }
}

fn game_clock(seconds: i64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// The least a record must reach to be worth a card, or for the fastest win
/// the most: a two-death game can be a player's worst without being a story.
fn notable(key: &str, value: i64) -> bool {
    match key {
        "most_kills" | "most_deaths" => value >= 10,
        "most_assists" => value >= 15,
        "best_gpm" => value >= 400,
        "most_damage" => value >= 20_000,
        "most_wards" => value >= 10,
        "longest_game" => value >= 45 * 60,
        "fastest_win" => value <= 30 * 60,
        _ => value >= 2,
    }
}

/// The game with the most of `measure`, or the least when `lowest`, earliest
/// first on ties, scored against the average of every game that knows it.
fn peak(
    games: &[&DistinctMatch],
    (key, direction): (&'static str, Direction),
    lowest: bool,
    measure: impl Fn(&DistinctMatch) -> Option<i64>,
    show: impl Fn(i64) -> String,
) -> Option<PersonalRecord> {
    let known: Vec<(&DistinctMatch, i64)> = games
        .iter()
        .filter_map(|game| Some((*game, measure(game)?)))
        .collect();
    if known.len() < MIN_RECORD_GAMES {
        return None;
    }
    let mean = known.iter().map(|(_, value)| *value as f64).sum::<f64>() / known.len() as f64;
    let (game, value) = known.iter().copied().reduce(|best, next| {
        let better = if lowest {
            next.1 < best.1
        } else {
            next.1 > best.1
        };
        if better { next } else { best }
    })?;
    let score = (value as f64 - mean).abs() / mean;
    // A record no better than the average is no record.
    (mean > 0.0 && score > 0.0 && notable(key, value)).then(|| PersonalRecord {
        key,
        direction,
        value,
        display: show(value),
        hero_id: game.hero_id,
        match_id: game.match_id,
        finished_at: game.finished_at,
        won: game.won,
        score,
    })
}

/// A year's count of `counted`, scored by count per game that knows it.
fn count(
    games: &[&DistinctMatch],
    (key, direction): (&'static str, Direction),
    counted: impl Fn(&DistinctMatch) -> Option<i64>,
    show: impl Fn(i64) -> String,
) -> Option<PersonalRecord> {
    let known: Vec<(&DistinctMatch, i64)> = games
        .iter()
        .filter_map(|game| Some((*game, counted(game)?)))
        .collect();
    if known.len() < MIN_RECORD_GAMES {
        return None;
    }
    let total: i64 = known.iter().map(|(_, value)| value).sum();
    let (latest, _) = known.iter().rev().find(|(_, value)| *value > 0)?;
    if !notable(key, total) {
        return None;
    }
    Some(PersonalRecord {
        key,
        direction,
        value: total,
        display: show(total),
        hero_id: latest.hero_id,
        match_id: latest.match_id,
        finished_at: latest.finished_at,
        won: latest.won,
        score: total as f64 / known.len() as f64,
    })
}

/// Every record `target` set in their own games in `rows`, strongest first.
#[must_use]
pub fn personal_records(rows: &[DistinctMatch], target: i64) -> Vec<PersonalRecord> {
    let mut games: Vec<&DistinctMatch> =
        rows.iter().filter(|row| row.discord_id == target).collect();
    games.sort_by_key(|game| (game.finished_at, game.match_id));
    let played = |game: &DistinctMatch| game.duration_seconds.filter(|seconds| *seconds > 0);
    let mut records = Vec::new();
    for (key, direction) in RECORD_KEYS {
        let entry = (key, direction);
        let record = match key {
            "most_kills" => peak(
                &games,
                entry,
                false,
                |g| g.kills,
                |v| count_noun(v, "kill", "kills"),
            ),
            "most_deaths" => peak(
                &games,
                entry,
                false,
                |g| g.deaths,
                |v| count_noun(v, "death", "deaths"),
            ),
            "most_assists" => peak(
                &games,
                entry,
                false,
                |g| g.assists,
                |v| count_noun(v, "assist", "assists"),
            ),
            "best_gpm" => peak(
                &games,
                entry,
                false,
                |g| g.gpm,
                |v| format!("{} GPM", commas(v as f64)),
            ),
            "most_damage" => peak(
                &games,
                entry,
                false,
                |g| g.hero_damage,
                |v| format!("{} damage", commas(v as f64)),
            ),
            "most_wards" => peak(
                &games,
                entry,
                false,
                |g| g.wards,
                |v| count_noun(v, "ward", "wards"),
            ),
            "longest_game" => peak(&games, entry, false, played, game_clock),
            "fastest_win" => peak(
                &games,
                entry,
                true,
                |g| played(g).filter(|_| g.won == Some(true)),
                game_clock,
            ),
            "first_bloods" => count(
                &games,
                entry,
                |g| g.first_blood.map(i64::from),
                |v| count_noun(v, "first blood", "first bloods"),
            ),
            _ => count(
                &games,
                entry,
                |g| g.roshans,
                |v| count_noun(v, "Roshan", "Roshans"),
            ),
        };
        records.extend(record);
    }
    records.sort_by(compare_records);
    records
}

fn compare_records(a: &PersonalRecord, b: &PersonalRecord) -> Ordering {
    b.score.total_cmp(&a.score).then_with(|| a.key.cmp(b.key))
}

/// Up to `max` records, strongest first, with a roast and a popoff among
/// them whenever both kinds are available.
#[must_use]
pub fn select_records(records: &[PersonalRecord], max: usize) -> Vec<PersonalRecord> {
    let mut ranked: Vec<&PersonalRecord> = records.iter().collect();
    ranked.sort_by(|a, b| compare_records(a, b));
    let mut chosen: Vec<&PersonalRecord> = Vec::new();
    while chosen.len() < max {
        let unseen =
            |record: &&&PersonalRecord| !chosen.iter().any(|held| std::ptr::eq(*held, **record));
        let fresh = ranked
            .iter()
            .filter(unseen)
            .find(|record| chosen.iter().all(|held| held.direction != record.direction));
        let Some(next) = fresh.or_else(|| ranked.iter().find(unseen)) else {
            break;
        };
        chosen.push(*next);
    }
    chosen.into_iter().cloned().collect()
}

/// `target`'s best hero, the hero that hates them, and their record on each
/// side of the map.
#[must_use]
pub fn hero_year(rows: &[DistinctMatch], target: i64) -> HeroYear {
    let mut heroes: BTreeMap<i64, (i64, i64)> = BTreeMap::new();
    let mut year = HeroYear::default();
    for row in rows.iter().filter(|row| row.discord_id == target) {
        let Some(won) = row.won else {
            continue;
        };
        if let Some(hero) = row.hero_id {
            let tally = heroes.entry(hero).or_insert((0, 0));
            tally.0 += 1;
            tally.1 += i64::from(won);
        }
        let side = match row.side.as_deref() {
            Some("radiant") => &mut year.radiant,
            Some("dire") => &mut year.dire,
            _ => continue,
        };
        side.0 += 1;
        side.1 += i64::from(won);
    }
    let lines: Vec<HeroLine> = heroes
        .into_iter()
        .map(|(hero_id, (games, wins))| HeroLine {
            hero_id,
            games,
            wins,
        })
        .filter(|line| line.games >= MIN_HERO_GAMES)
        .collect();
    // Higher win rate first, then more games, then the lower hero id.
    let by_rate = |a: &&HeroLine, b: &&HeroLine| {
        (a.wins * b.games)
            .cmp(&(b.wins * a.games))
            .then(a.games.cmp(&b.games))
            .then(b.hero_id.cmp(&a.hero_id))
    };
    year.best = lines
        .iter()
        .filter(|line| line.wins * 100 >= line.games * BEST_HERO_RATE)
        .max_by(by_rate)
        .cloned();
    year.nemesis_hero = lines
        .iter()
        .filter(|line| line.wins * 100 <= line.games * NEMESIS_HERO_RATE)
        .max_by(|a, b| {
            (b.wins * a.games)
                .cmp(&(a.wins * b.games))
                .then(a.games.cmp(&b.games))
                .then(b.hero_id.cmp(&a.hero_id))
        })
        .cloned();
    year
}

/// `"Mar 3"`: the day a game finished, in the player's time zone.
#[must_use]
pub fn month_day(finished_at: i64, timezone: Option<&str>) -> Option<String> {
    let local = DateTime::from_timestamp(finished_at, 0)?.with_timezone(&zone_for(timezone));
    Some(local.format("%b %-d").to_string())
}
