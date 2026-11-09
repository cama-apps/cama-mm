//! Distinctiveness ranking for Wrapped: which of a player's year-long numbers
//! stand out against everyone else's, and who they shared the year with.
//!
//! Pure policy over rows the caller has already loaded. Unknown data is never
//! zero: a missing field drops the game from that metric, and a player who
//! lacks the metric's sample is absent from its population.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use chrono::{DateTime, Timelike, Utc};
use chrono_tz::Tz;

use crate::timezone::DEFAULT_TIMEZONE;
use crate::wrapped_activity::ServerRank;

use Direction::{Popoff, Roast};
use Theme::{Chaos, Clutch, Combat, Farm, Heroes, Items, Map, Time};

#[path = "wrapped_distinct/records.rs"]
mod records;

pub use records::{
    COUNT_RECORDS, HeroLine, HeroYear, PersonalRecord, RECORD_KEYS, hero_year, month_day,
    personal_records, record_twin, select_records,
};

/// Below this many eligible players a metric is too thin to rank.
pub const SMALL_POPULATION: usize = 12;

/// Items whose purchase rate can become a story.
pub const TRACKED_ITEMS: [&str; 8] = [
    "magic_wand",
    "black_king_bar",
    "blink",
    "hand_of_midas",
    "smoke_of_deceit",
    "dust",
    "bottle",
    "radiance",
];

/// Purchase counts per tracked item (plus rapiers) from OpenDota purchase-log
/// keys. Untracked keys are ignored; an empty log yields an empty map.
pub fn tracked_item_counts<'a>(keys: impl IntoIterator<Item = &'a str>) -> BTreeMap<String, i64> {
    let mut counts = BTreeMap::new();
    for key in keys {
        if key == "rapier" || TRACKED_ITEMS.contains(&key) {
            *counts.entry(key.to_owned()).or_insert(0) += 1;
        }
    }
    counts
}

/// Every metric key a story can carry. Item stories share the key `"item"`
/// and name the item in `DistinctStory::item`.
pub const METRIC_KEYS: &[&str] = &[
    "win_rate",
    "deaths_per_game",
    "kills_per_game",
    "best_game",
    "kill_loss",
    "gpm",
    "last_hits",
    "hero_damage",
    "healing",
    "stuns",
    "wards",
    "stacks",
    "runes",
    "couriers",
    "pings",
    "apm",
    "throw",
    "comeback",
    "rapiers",
    "upset_win",
    "upset_loss",
    "win_streak",
    "loss_streak",
    "rating_swing",
    "night_owl",
    "marathon_day",
    "hero_loyal",
    "hero_pool",
    "buybacks",
    "item",
];

const MIN_PAIR_GAMES: i64 = 5;
const MIN_PER_DIRECTION: usize = 2;
const LATE_HOUR_END: u32 = 6;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DistinctMatch {
    pub match_id: i64,
    pub discord_id: i64,
    /// `"radiant"` or `"dire"`.
    pub side: Option<String>,
    pub won: Option<bool>,
    /// Unix seconds, parsed from `matches.match_date` (UTC).
    pub finished_at: Option<i64>,
    pub duration_seconds: Option<i64>,
    pub hero_id: Option<i64>,
    pub kills: Option<i64>,
    pub deaths: Option<i64>,
    pub assists: Option<i64>,
    pub last_hits: Option<i64>,
    pub gpm: Option<i64>,
    pub hero_damage: Option<i64>,
    pub tower_damage: Option<i64>,
    pub hero_healing: Option<i64>,
    /// Observer plus sentry wards; `None` when both are null.
    pub wards: Option<i64>,
    pub camps_stacked: Option<i64>,
    pub rune_pickups: Option<i64>,
    pub stuns: Option<f64>,
    /// A `wrapped_enrichment_facts` row exists for this game.
    pub enriched: bool,
    pub courier_kills: Option<i64>,
    pub pings: Option<i64>,
    pub actions_per_min: Option<f64>,
    pub rapiers: Option<i64>,
    pub buybacks: Option<i64>,
    /// Purchase counts by OpenDota item key; empty when unknown.
    pub items: Vec<(String, i64)>,
    /// Match-wide team gold figures.
    pub comeback: Option<f64>,
    pub throw: Option<f64>,
    /// `rating_history.expected_team_win_prob` for this player.
    pub expected_win: Option<f64>,
    /// `rating - rating_before`.
    pub rating_delta: Option<f64>,
    /// Parsed games only; `None` when unknown.
    pub first_blood: Option<bool>,
    pub roshans: Option<i64>,
    pub towers: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DistinctPlayer {
    pub discord_id: i64,
    pub timezone: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Direction {
    Roast,
    Popoff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Theme {
    Heroes,
    Combat,
    Farm,
    Map,
    Chaos,
    Clutch,
    Time,
    Items,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    Extreme,
    Strong,
    Notable,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DistinctStory {
    pub metric: &'static str,
    pub theme: Theme,
    pub direction: Direction,
    pub tier: Tier,
    pub value: f64,
    /// Ready to show: `"23,400"`, `"4:12 AM"`, `"27 / 2 / 14"`, `"61"`.
    pub display: String,
    /// Counted from the story's own end of the table (best for a Popoff on
    /// the high side, lowest for a Roast on the low side). `None` for small
    /// populations and for ties.
    pub rank: Option<ServerRank>,
    pub hero_id: Option<i64>,
    pub match_id: Option<i64>,
    /// Item key for `"item"` stories.
    pub item: Option<String>,
    pub score: f64,
}

/// Each role needs at least five games and a record that fits it, so one
/// person can never be both halves of a contradiction.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PeopleYear {
    /// Most wins together, with more wins than losses together.
    pub duo: Option<PairStory>,
    /// Lowest win rate together, under 40%.
    pub cursed_duo: Option<PairStory>,
    /// Most losses against, with more losses than wins against them.
    pub nemesis: Option<PairStory>,
    /// Most wins against, with more wins than losses against them.
    pub punching_bag: Option<PairStory>,
}

/// `wins` is always the target's wins in the `games` shared with that player.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairStory {
    pub discord_id: i64,
    pub games: i64,
    pub wins: i64,
}

/// The directions stories under `key` can take; empty for an unknown key.
#[must_use]
pub fn metric_directions(key: &str) -> &'static [Direction] {
    match key {
        "win_rate" | "gpm" | "wards" | "rapiers" | "rating_swing" | "hero_loyal" | "hero_pool" => {
            &[Popoff, Roast]
        }
        "deaths_per_game" => &[Roast, Popoff],
        "kills_per_game" | "best_game" | "last_hits" | "hero_damage" | "healing" | "stuns"
        | "stacks" | "runes" | "couriers" | "apm" | "comeback" | "upset_win" | "win_streak" => {
            &[Popoff]
        }
        "kill_loss" | "pings" | "throw" | "upset_loss" | "loss_streak" | "night_owl"
        | "marathon_day" | "buybacks" | "item" => &[Roast],
        _ => &[],
    }
}

struct Measure {
    value: f64,
    display: String,
    hero_id: Option<i64>,
    match_id: Option<i64>,
    item: Option<String>,
    /// Replaces the row's high-side direction for this player.
    direction: Option<Direction>,
}

impl Measure {
    fn new(value: f64, display: String) -> Self {
        Self {
            value,
            display,
            hero_id: None,
            match_id: None,
            item: None,
            direction: None,
        }
    }

    fn game(mut self, game: &DistinctMatch) -> Self {
        self.hero_id = game.hero_id;
        self.match_id = Some(game.match_id);
        self
    }

    fn turning(mut self, direction: Option<Direction>) -> Self {
        self.direction = direction;
        self
    }
}

struct Player<'a> {
    games: Vec<&'a DistinctMatch>,
    zone: Tz,
}

type MeasureFn = fn(&Player<'_>, &'static str) -> Option<Measure>;

#[derive(Clone, Copy)]
struct Row {
    key: &'static str,
    theme: Theme,
    high: Direction,
    low: Option<Direction>,
    /// Single-game records still tell a story in a small population.
    record: bool,
    /// Which item, for `"item"` rows; empty otherwise.
    param: &'static str,
    measure: MeasureFn,
}

impl Row {
    const fn new(
        key: &'static str,
        theme: Theme,
        high: Direction,
        low: Option<Direction>,
        record: bool,
        measure: MeasureFn,
    ) -> Self {
        Self {
            key,
            theme,
            high,
            low,
            record,
            param: "",
            measure,
        }
    }
}

fn int(value: Option<i64>) -> Option<f64> {
    value.map(|value| value as f64)
}

fn commas(value: f64) -> String {
    let digits = (value.round() as i64).unsigned_abs().to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    if value.round() < 0.0 {
        grouped.insert(0, '-');
    }
    grouped
}

fn one_decimal(value: f64) -> String {
    format!("{value:.1}")
}

fn percent(value: f64) -> String {
    format!("{:.0}%", value * 100.0)
}

fn clock(time: &DateTime<Tz>) -> String {
    let hour = time.hour();
    let twelve = if hour.is_multiple_of(12) {
        12
    } else {
        hour % 12
    };
    let meridiem = if hour < 12 { "AM" } else { "PM" };
    format!("{twelve}:{:02} {meridiem}", time.minute())
}

impl Player<'_> {
    fn local(&self, finished_at: i64) -> Option<DateTime<Tz>> {
        Some(DateTime::<Utc>::from_timestamp(finished_at, 0)?.with_timezone(&self.zone))
    }

    /// Games that count: all of them, or only parsed ones.
    fn sample(&self, enriched_only: bool) -> impl Iterator<Item = &DistinctMatch> {
        self.games
            .iter()
            .copied()
            .filter(move |game| !enriched_only || game.enriched)
    }

    fn values(
        &self,
        enriched_only: bool,
        field: impl Fn(&DistinctMatch) -> Option<f64>,
    ) -> Vec<f64> {
        self.sample(enriched_only).filter_map(field).collect()
    }

    fn average(
        &self,
        minimum: usize,
        enriched_only: bool,
        show: fn(f64) -> String,
        field: impl Fn(&DistinctMatch) -> Option<f64>,
    ) -> Option<Measure> {
        let values = self.values(enriched_only, field);
        if values.len() < minimum {
            return None;
        }
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        Some(Measure::new(mean, show(mean)))
    }

    fn total(
        &self,
        minimum: usize,
        enriched_only: bool,
        field: impl Fn(&DistinctMatch) -> Option<f64>,
    ) -> Option<f64> {
        let values = self.values(enriched_only, field);
        (values.len() >= minimum).then(|| values.iter().sum())
    }

    /// Field total over duration minutes, across games that report both.
    fn per_minute(
        &self,
        show: fn(f64) -> String,
        field: impl Fn(&DistinctMatch) -> Option<i64>,
    ) -> Option<Measure> {
        let usable: Vec<(f64, f64)> = self
            .games
            .iter()
            .filter_map(|game| {
                let minutes = game.duration_seconds.filter(|seconds| *seconds > 0)? as f64 / 60.0;
                Some((field(game)? as f64, minutes))
            })
            .collect();
        if usable.len() < 5 {
            return None;
        }
        let total: f64 = usable.iter().map(|(amount, _)| amount).sum();
        let minutes: f64 = usable.iter().map(|(_, minutes)| minutes).sum();
        let rate = total / minutes;
        Some(Measure::new(rate, show(rate)))
    }

    /// The game with the highest `score`; the earliest wins ties.
    fn best(&self, score: impl Fn(&DistinctMatch) -> Option<f64>) -> Option<(f64, &DistinctMatch)> {
        self.games
            .iter()
            .copied()
            .filter_map(|game| Some((score(game)?, game)))
            .fold(None, |best, candidate| match best {
                Some((value, _)) if candidate.0 <= value => best,
                _ => Some(candidate),
            })
    }

    fn record(
        &self,
        score: impl Fn(&DistinctMatch) -> Option<f64>,
        show: fn(f64) -> String,
    ) -> Option<Measure> {
        let (value, game) = self.best(score)?;
        Some(Measure::new(value, show(value)).game(game))
    }

    fn streak(&self, won: bool) -> Option<Measure> {
        let (mut best, mut run, mut best_match) = (0, 0, None);
        for game in self.games.iter().filter(|game| game.finished_at.is_some()) {
            match game.won {
                Some(result) if result == won => {
                    run += 1;
                    if run > best {
                        best = run;
                        best_match = Some(game.match_id);
                    }
                }
                Some(_) => run = 0,
                None => {}
            }
        }
        (best >= 3).then(|| {
            let mut measure = Measure::new(best as f64, best.to_string());
            measure.match_id = best_match;
            measure
        })
    }

    fn night_owl(&self) -> Option<Measure> {
        let late: Vec<(&DistinctMatch, DateTime<Tz>)> = self
            .games
            .iter()
            .filter_map(|game| Some((*game, self.local(game.finished_at?)?)))
            .filter(|(_, time)| time.hour() < LATE_HOUR_END)
            .collect();
        if late.len() < 3 {
            return None;
        }
        let (game, latest) = late.iter().fold(
            None,
            |best: Option<&(&DistinctMatch, DateTime<Tz>)>, entry| match best {
                Some(held) if entry.1.time() <= held.1.time() => best,
                _ => Some(entry),
            },
        )?;
        Some(Measure::new(late.len() as f64, clock(latest)).game(game))
    }

    fn marathon_day(&self) -> Option<Measure> {
        let mut days = BTreeMap::new();
        for game in &self.games {
            if let Some(time) = game.finished_at.and_then(|at| self.local(at)) {
                *days.entry(time.date_naive()).or_insert(0_i64) += 1;
            }
        }
        let most = days.values().copied().max().filter(|most| *most >= 4)?;
        Some(Measure::new(most as f64, most.to_string()))
    }

    fn hero_loyal(&self) -> Option<Measure> {
        let with_hero: Vec<&DistinctMatch> = self
            .games
            .iter()
            .copied()
            .filter(|game| game.hero_id.is_some())
            .collect();
        if with_hero.len() < 10 {
            return None;
        }
        let mut counts: BTreeMap<i64, i64> = BTreeMap::new();
        for hero in with_hero.iter().filter_map(|game| game.hero_id) {
            *counts.entry(hero).or_insert(0) += 1;
        }
        let (hero, count) =
            counts.into_iter().fold(
                (0, 0),
                |best, entry| if entry.1 > best.1 { entry } else { best },
            );
        let share = count as f64 / with_hero.len() as f64;
        if share < 0.3 {
            return None;
        }
        let results: Vec<bool> = with_hero
            .iter()
            .filter(|game| game.hero_id == Some(hero))
            .filter_map(|game| game.won)
            .collect();
        let wins = results.iter().filter(|won| **won).count();
        let losing = !results.is_empty() && (wins as f64 / results.len() as f64) < 0.4;
        let mut measure =
            Measure::new(share, percent(share)).turning(losing.then_some(Direction::Roast));
        measure.hero_id = Some(hero);
        Some(measure)
    }

    fn hero_pool(&self) -> Option<Measure> {
        let heroes: Vec<i64> = self.games.iter().filter_map(|game| game.hero_id).collect();
        if heroes.len() < 10 {
            return None;
        }
        let distinct = heroes.iter().collect::<BTreeSet<_>>().len();
        Some(Measure::new(
            distinct as f64 / heroes.len() as f64,
            format!("{distinct} heroes"),
        ))
    }

    fn rapiers(&self) -> Option<Measure> {
        let total = self.total(1, false, |game| int(game.rapiers))?;
        if total < 1.0 {
            return None;
        }
        let lost_one = self
            .games
            .iter()
            .any(|game| game.rapiers.unwrap_or(0) > 0 && game.won == Some(false));
        Some(Measure::new(total, commas(total)).turning(lost_one.then_some(Direction::Roast)))
    }

    fn item_rate(&self, item: &'static str) -> Option<Measure> {
        let stocked: Vec<&DistinctMatch> = self
            .games
            .iter()
            .copied()
            .filter(|game| game.enriched && !game.items.is_empty())
            .collect();
        if stocked.len() < 5 {
            return None;
        }
        let bought: i64 = stocked
            .iter()
            .flat_map(|game| &game.items)
            .filter(|(key, _)| key == item)
            .map(|(_, count)| count)
            .sum();
        let rate = bought as f64 / stocked.len() as f64;
        let mut measure = Measure::new(rate, one_decimal(rate));
        measure.item = Some(item.to_string());
        Some(measure)
    }

    fn rating_swing(&self, gain: bool) -> Option<Measure> {
        self.record(
            |game| {
                let delta = game.rating_delta?;
                if gain {
                    (delta > 0.0).then_some(delta)
                } else {
                    (delta < 0.0).then_some(-delta)
                }
            },
            if gain {
                |value| format!("+{}", commas(value))
            } else {
                |value| format!("-{}", commas(value))
            },
        )
    }
}

fn won(game: &DistinctMatch) -> bool {
    game.won == Some(true)
}

fn lost(game: &DistinctMatch) -> bool {
    game.won == Some(false)
}

const CATALOG: &[Row] = &[
    Row::new("win_rate", Clutch, Popoff, Some(Roast), false, |p, _| {
        let known: Vec<bool> = p.games.iter().filter_map(|game| game.won).collect();
        if known.len() < 5 {
            return None;
        }
        let rate = known.iter().filter(|won| **won).count() as f64 / known.len() as f64;
        Some(Measure::new(rate, percent(rate)))
    }),
    Row::new(
        "deaths_per_game",
        Combat,
        Roast,
        Some(Popoff),
        false,
        |p, _| p.average(5, false, one_decimal, |game| int(game.deaths)),
    ),
    Row::new("kills_per_game", Combat, Popoff, None, false, |p, _| {
        p.average(5, false, one_decimal, |game| int(game.kills))
    }),
    Row::new("best_game", Combat, Popoff, None, true, |p, _| {
        let (value, game) =
            p.best(|game| Some((game.kills? + game.assists? - game.deaths?) as f64))?;
        let display = format!("{} / {} / {}", game.kills?, game.deaths?, game.assists?);
        Some(Measure::new(value, display).game(game))
    }),
    Row::new("kill_loss", Combat, Roast, None, true, |p, _| {
        p.record(
            |game| int(game.kills).filter(|kills| lost(game) && *kills >= 15.0),
            commas,
        )
    }),
    Row::new("gpm", Farm, Popoff, Some(Roast), false, |p, _| {
        p.average(5, false, commas, |game| int(game.gpm))
    }),
    Row::new("last_hits", Farm, Popoff, None, false, |p, _| {
        p.per_minute(one_decimal, |game| game.last_hits)
    }),
    Row::new("hero_damage", Farm, Popoff, None, false, |p, _| {
        p.per_minute(commas, |game| game.hero_damage)
    }),
    Row::new("healing", Farm, Popoff, None, false, |p, _| {
        let parsed = Player {
            games: p.sample(true).collect(),
            zone: p.zone,
        };
        parsed.per_minute(commas, |game| game.hero_healing)
    }),
    Row::new("stuns", Farm, Popoff, None, false, |p, _| {
        p.average(5, true, one_decimal, |game| game.stuns)
    }),
    Row::new("wards", Map, Popoff, Some(Roast), false, |p, _| {
        p.average(5, true, one_decimal, |game| int(game.wards))
    }),
    Row::new("stacks", Map, Popoff, None, false, |p, _| {
        p.average(5, true, one_decimal, |game| int(game.camps_stacked))
    }),
    Row::new("runes", Map, Popoff, None, false, |p, _| {
        p.average(5, true, one_decimal, |game| int(game.rune_pickups))
    }),
    Row::new("couriers", Map, Popoff, None, false, |p, _| {
        let total = p.total(3, true, |game| int(game.courier_kills))?;
        Some(Measure::new(total, commas(total)))
    }),
    Row::new("pings", Chaos, Roast, None, false, |p, _| {
        p.average(3, true, one_decimal, |game| int(game.pings))
    }),
    Row::new("apm", Chaos, Popoff, None, false, |p, _| {
        p.average(3, true, commas, |game| game.actions_per_min)
    }),
    Row::new("throw", Chaos, Roast, None, true, |p, _| {
        p.record(
            |game| game.throw.filter(|gold| lost(game) && *gold >= 8_000.0),
            commas,
        )
    }),
    Row::new("comeback", Chaos, Popoff, None, true, |p, _| {
        p.record(
            |game| game.comeback.filter(|gold| won(game) && *gold >= 5_000.0),
            commas,
        )
    }),
    Row::new("rapiers", Chaos, Popoff, None, true, |p, _| p.rapiers()),
    Row::new("upset_win", Clutch, Popoff, None, true, |p, _| {
        let (value, game) = p.best(|game| {
            game.expected_win
                .filter(|odds| won(game) && *odds <= 0.35)
                .map(|odds| 1.0 - odds)
        })?;
        Some(Measure::new(value, percent(1.0 - value)).game(game))
    }),
    Row::new("upset_loss", Clutch, Roast, None, true, |p, _| {
        p.record(
            |game| game.expected_win.filter(|odds| lost(game) && *odds >= 0.70),
            percent,
        )
    }),
    Row::new("win_streak", Clutch, Popoff, None, true, |p, _| {
        p.streak(true)
    }),
    Row::new("loss_streak", Clutch, Roast, None, true, |p, _| {
        p.streak(false)
    }),
    Row::new("rating_swing", Clutch, Popoff, None, false, |p, _| {
        p.rating_swing(true)
    }),
    Row::new("rating_swing", Clutch, Roast, None, false, |p, _| {
        p.rating_swing(false)
    }),
    Row::new("night_owl", Time, Roast, None, false, |p, _| p.night_owl()),
    Row::new("marathon_day", Time, Roast, None, false, |p, _| {
        p.marathon_day()
    }),
    Row::new("hero_loyal", Heroes, Popoff, None, false, |p, _| {
        p.hero_loyal()
    }),
    Row::new("hero_pool", Heroes, Popoff, Some(Roast), false, |p, _| {
        p.hero_pool()
    }),
    Row::new("buybacks", Items, Roast, None, false, |p, _| {
        p.average(5, true, one_decimal, |game| int(game.buybacks))
    }),
];

fn item_row(item: &'static str) -> Row {
    Row {
        param: item,
        ..Row::new("item", Items, Roast, None, false, |p, item| {
            p.item_rate(item)
        })
    }
}

fn theme_weight(theme: Theme) -> f64 {
    match theme {
        Clutch | Chaos => 0.15,
        Combat => 0.1,
        _ => 0.0,
    }
}

fn score(percentile: f64, tier: Tier, theme: Theme) -> f64 {
    (percentile - 50.0).abs() / 50.0
        + if tier == Tier::Extreme { 0.25 } else { 0.0 }
        + theme_weight(theme)
}

/// Mid-rank percentile of `query` among `values`.
fn percentile(query: f64, values: &[f64]) -> f64 {
    if values.is_empty() {
        return 50.0;
    }
    let below = values.iter().filter(|value| **value < query).count() as f64;
    let equal = values.iter().filter(|value| **value == query).count() as f64;
    ((below + equal * 0.5) / values.len() as f64) * 100.0
}

struct Placement {
    direction: Direction,
    tier: Tier,
    percentile: f64,
    rank: Option<ServerRank>,
}

/// Where `value` sits among `population`, if it stands out in a direction
/// the row can speak to.
fn place(row: &Row, high: Direction, value: f64, population: &[f64]) -> Option<Placement> {
    let n = population.len();
    let below = population.iter().filter(|other| **other < value).count();
    let equal = population.iter().filter(|other| **other == value).count();
    let above = n - below - equal;
    let percentile = percentile(value, population);

    let (direction, strength, beyond) = if percentile >= 50.0 {
        (high, percentile, above)
    } else {
        (row.low?, 100.0 - percentile, below)
    };
    let mut tier = if (beyond == 0 && equal == 1) || strength >= 95.0 {
        Tier::Extreme
    } else if strength >= 80.0 {
        Tier::Strong
    } else if strength >= 65.0 {
        Tier::Notable
    } else {
        return None;
    };
    let shared_extreme = beyond == 0 && equal > 2;
    if shared_extreme {
        tier = tier.max(Tier::Strong);
    }
    let rank = (equal == 1).then(|| ServerRank {
        rank: beyond as i64 + 1,
        of: n as i64,
    });
    Some(Placement {
        direction,
        tier,
        percentile,
        rank,
    })
}

fn zone_for(timezone: Option<&str>) -> Tz {
    timezone
        .and_then(|name| Tz::from_str(name).ok())
        .or_else(|| Tz::from_str(DEFAULT_TIMEZONE).ok())
        .unwrap_or(Tz::UTC)
}

fn compare_stories(a: &DistinctStory, b: &DistinctStory) -> Ordering {
    b.score
        .total_cmp(&a.score)
        .then_with(|| a.metric.cmp(b.metric))
        .then_with(|| a.direction.cmp(&b.direction))
        .then_with(|| a.item.cmp(&b.item))
}

/// Ranked stories for `target` against every player with at least
/// `minimum_games` known-result games, best first.
#[must_use]
pub fn distinct_stories(
    rows: &[DistinctMatch],
    players: &[DistinctPlayer],
    target: i64,
    minimum_games: usize,
) -> Vec<DistinctStory> {
    let mut by_player: BTreeMap<i64, Vec<&DistinctMatch>> = BTreeMap::new();
    for row in rows {
        by_player.entry(row.discord_id).or_default().push(row);
    }
    let mut eligible: BTreeMap<i64, Player<'_>> = BTreeMap::new();
    for player in players {
        let Some(mut games) = by_player.remove(&player.discord_id) else {
            continue;
        };
        if games.iter().filter(|game| game.won.is_some()).count() < minimum_games {
            continue;
        }
        games.sort_by_key(|game| (game.finished_at, game.match_id));
        eligible.insert(
            player.discord_id,
            Player {
                games,
                zone: zone_for(player.timezone.as_deref()),
            },
        );
    }
    if !eligible.contains_key(&target) {
        return Vec::new();
    }

    let mut stories = Vec::new();
    let mut best_item: Option<DistinctStory> = None;
    let rows = CATALOG
        .iter()
        .copied()
        .chain(TRACKED_ITEMS.iter().map(|item| item_row(item)));
    for row in rows {
        let measures: BTreeMap<i64, Measure> = eligible
            .iter()
            .filter_map(|(id, player)| Some((*id, (row.measure)(player, row.param)?)))
            .collect();
        let Some(mine) = measures.get(&target) else {
            continue;
        };
        let high = mine.direction.unwrap_or(row.high);
        let story = if measures.len() < SMALL_POPULATION {
            if !row.record {
                continue;
            }
            Placement {
                direction: high,
                tier: Tier::Notable,
                percentile: 65.0,
                rank: None,
            }
        } else {
            let population: Vec<f64> = measures.values().map(|measure| measure.value).collect();
            let Some(placement) = place(&row, high, mine.value, &population) else {
                continue;
            };
            placement
        };
        let built = DistinctStory {
            metric: row.key,
            theme: row.theme,
            direction: story.direction,
            tier: story.tier,
            value: mine.value,
            display: mine.display.clone(),
            rank: story.rank,
            hero_id: mine.hero_id,
            match_id: mine.match_id,
            item: mine.item.clone(),
            score: score(story.percentile, story.tier, row.theme),
        };
        if row.key == "item" {
            if best_item
                .as_ref()
                .is_none_or(|held| compare_stories(&built, held) == Ordering::Less)
            {
                best_item = Some(built);
            }
        } else {
            stories.push(built);
        }
    }
    stories.extend(best_item);
    stories.sort_by(compare_stories);
    stories
}

/// Pick up to `max` stories: best first, one per theme before any repeats,
/// and at least two of each direction when the candidates allow it.
#[must_use]
pub fn select_stories(candidates: &[DistinctStory], max: usize) -> Vec<DistinctStory> {
    let mut ranked: Vec<&DistinctStory> = candidates.iter().collect();
    ranked.sort_by(|a, b| compare_stories(a, b));

    let mut chosen: Vec<&DistinctStory> = Vec::new();
    for story in &ranked {
        if chosen.len() < max && !chosen.iter().any(|held| held.theme == story.theme) {
            chosen.push(story);
        }
    }
    for story in &ranked {
        if chosen.len() < max && !chosen.iter().any(|held| std::ptr::eq(*held, *story)) {
            chosen.push(story);
        }
    }

    for wanted in [Direction::Roast, Direction::Popoff] {
        while chosen.iter().filter(|s| s.direction == wanted).count() < MIN_PER_DIRECTION {
            let Some(incoming) = ranked.iter().find(|s| {
                s.direction == wanted && !chosen.iter().any(|held| std::ptr::eq(*held, **s))
            }) else {
                break;
            };
            let over_represented = chosen.iter().filter(|s| s.direction != wanted).count();
            if over_represented <= MIN_PER_DIRECTION {
                break;
            }
            let Some(weakest) = chosen
                .iter()
                .enumerate()
                .filter(|(_, s)| s.direction != wanted)
                .max_by(|a, b| compare_stories(a.1, b.1))
                .map(|(index, _)| index)
            else {
                break;
            };
            chosen[weakest] = incoming;
        }
    }

    chosen.sort_by(|a, b| compare_stories(a, b));
    chosen.into_iter().cloned().collect()
}

fn best_pair(
    tallies: &BTreeMap<i64, (i64, i64)>,
    eligible: impl Fn(&PairStory) -> bool,
    cmp: impl Fn(&PairStory, &PairStory) -> Ordering,
) -> Option<PairStory> {
    tallies
        .iter()
        .map(|(id, (games, wins))| PairStory {
            discord_id: *id,
            games: *games,
            wins: *wins,
        })
        .filter(|pair| pair.games >= MIN_PAIR_GAMES && eligible(pair))
        .fold(None, |best, pair| match best {
            Some(held) if cmp(&pair, &held) != Ordering::Greater => Some(held),
            _ => Some(pair),
        })
}

/// Who `target` won with, lost with, and was beaten or beat, from the rows
/// given. Lower player ids win ties.
#[must_use]
pub fn people_year(rows: &[DistinctMatch], target: i64) -> PeopleYear {
    let mine: BTreeMap<i64, (&str, bool)> = rows
        .iter()
        .filter(|row| row.discord_id == target)
        .filter_map(|row| Some((row.match_id, (row.side.as_deref()?, row.won?))))
        .collect();

    let mut together: BTreeMap<i64, (i64, i64)> = BTreeMap::new();
    let mut against: BTreeMap<i64, (i64, i64)> = BTreeMap::new();
    for row in rows.iter().filter(|row| row.discord_id != target) {
        let (Some((side, target_won)), Some(their_side)) =
            (mine.get(&row.match_id), row.side.as_deref())
        else {
            continue;
        };
        let tally = if their_side == *side {
            &mut together
        } else {
            &mut against
        }
        .entry(row.discord_id)
        .or_insert((0, 0));
        tally.0 += 1;
        tally.1 += i64::from(*target_won);
    }

    let winning = |pair: &PairStory| pair.wins * 2 > pair.games;
    let losing = |pair: &PairStory| pair.wins * 2 < pair.games;
    PeopleYear {
        duo: best_pair(&together, winning, |a, b| a.wins.cmp(&b.wins)),
        cursed_duo: best_pair(
            &together,
            |pair| pair.wins * 100 < pair.games * 40,
            |a, b| {
                (b.wins * a.games)
                    .cmp(&(a.wins * b.games))
                    .then(a.games.cmp(&b.games))
            },
        ),
        nemesis: best_pair(&against, losing, |a, b| {
            (a.games - a.wins).cmp(&(b.games - b.wins))
        }),
        punching_bag: best_pair(&against, winning, |a, b| a.wins.cmp(&b.wins)),
    }
}

#[cfg(test)]
#[path = "wrapped_distinct/tests.rs"]
mod tests;
