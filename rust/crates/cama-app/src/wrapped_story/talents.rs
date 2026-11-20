//! Talents: a Dota talent tree of four either/or tiers, each a real contrast
//! in the player's year, with the side their own numbers favor picked.
//!
//! Both options of a tier show comparable numbers, so the picked side is
//! always the one with the bigger number on the card. A pair needs enough
//! known data on both sides and two numbers that read differently.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Datelike, Weekday};

use cama_domain::wrapped_distinct::{DistinctMatch, local_time};

use super::patch_support::{
    core, counted, finite, hero_of, minutes, mix, nonnegative, own_games, percent,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TalentSide {
    Left,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TalentTier {
    /// 25, 20, 15, then 10: the clearest contrast sits at 25.
    pub level: u8,
    pub left: String,
    pub right: String,
    pub picked: TalentSide,
}

pub const TALENT_LEVELS: [u8; 4] = [25, 20, 15, 10];
/// Longest option text, in characters.
pub const MAX_TALENT_TEXT: usize = 18;

/// Known games each side of a pair needs.
const MIN_SIDE_GAMES: usize = 5;
/// Games with a known position or hero before their split is a talent.
const MIN_SPLIT_GAMES: usize = 10;
/// Players with the stat before the guild has a median.
const MIN_GUILD_PLAYERS: usize = 5;
/// Win-rate points apart that count as a full-strength contrast.
const RATE_SCALE: f64 = 25.0;

/// Option templates per pair, the shortest wording first; `{}` is the value.
type Labels = &'static [(&'static str, &'static str)];

const KILLS_ASSISTS: Labels = &[
    ("+{} Kills", "+{} Assists"),
    ("+{} Kills/Game", "+{} Assists/Game"),
];
const SIDES: Labels = &[
    ("{}% Radiant", "{}% Dire"),
    ("{}% On Radiant", "{}% On Dire"),
];
const ROLE_RATES: Labels = &[
    ("{}% Core", "{}% Support"),
    ("{}% As Core", "{}% As Support"),
];
const ROLE_GAMES: Labels = &[
    ("+{} Core Games", "+{} Support Games"),
    ("+{} Games: Core", "+{} Games: Support"),
];
const LENGTH: Labels = &[
    ("{}% Sub-35 Min", "{}% 45+ Min"),
    ("{}% Under 35 Min", "{}% Over 45 Min"),
];
const ODDS: Labels = &[
    ("{}% Favored", "{}% Underdog"),
    ("{}% As Favorite", "{}% As Underdog"),
];
const WEEK: Labels = &[
    ("{}% Mon-Fri", "{}% Sat-Sun"),
    ("{}% Weekdays", "{}% Weekends"),
];
const FARM_FIGHT: Labels = &[("{}% Farm", "{}% Fight"), ("{}% GPM", "{}% Team Fights")];
const HERO_POOL: Labels = &[
    ("+{} On Top Hero", "+{} Heroes Played"),
    ("+{} Top Hero Games", "+{} Unique Heroes"),
];
const HALVES: Labels = &[
    ("{}% Jan-Jun", "{}% Jul-Dec"),
    ("{}% First Half", "{}% Second Half"),
];

struct Pair {
    key: &'static str,
    left: String,
    right: String,
    picked: TalentSide,
    /// How far apart the sides are, on a scale shared by every pair.
    contrast: f64,
}

/// The player's talent tree: the four clearest contrasts in their year, or
/// nothing when fewer than four pairs have the data.
#[must_use]
pub fn talent_tree(
    matches: &[DistinctMatch],
    player_id: i64,
    timezone: Option<&str>,
    seed: u64,
) -> Vec<TalentTier> {
    let games = own_games(matches, player_id);
    let mut pairs = [
        kills_assists(&games, seed),
        sides(&games, seed),
        roles(&games, seed),
        length(&games, seed),
        odds(&games, seed),
        week(&games, timezone, seed),
        farm_fight(matches, player_id, seed),
        hero_pool(&games, seed),
        halves(&games, timezone, seed),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if pairs.len() < TALENT_LEVELS.len() {
        return Vec::new();
    }
    pairs.sort_by(|a, b| {
        b.contrast
            .total_cmp(&a.contrast)
            .then_with(|| mix(seed, a.key).cmp(&mix(seed, b.key)))
    });
    TALENT_LEVELS
        .into_iter()
        .zip(pairs)
        .map(|(level, pair)| TalentTier {
            level,
            left: pair.left,
            right: pair.right,
            picked: pair.picked,
        })
        .collect()
}

/// A pair from two numbers shown as `left` and `right`: the bigger one is
/// picked, and numbers that read the same are no talent. The seed chooses
/// the wording; the shortest wording stands in when the chosen one is too
/// long.
fn pair(
    seed: u64,
    key: &'static str,
    labels: Labels,
    left: (f64, String),
    right: (f64, String),
    contrast: f64,
) -> Option<Pair> {
    if left.1 == right.1 {
        return None;
    }
    let written = |(left_label, right_label): (&str, &str)| {
        let texts = (
            left_label.replace("{}", &left.1),
            right_label.replace("{}", &right.1),
        );
        let fits = |text: &str| text.chars().count() <= MAX_TALENT_TEXT;
        (fits(&texts.0) && fits(&texts.1)).then_some(texts)
    };
    let chosen = labels[(mix(seed, key) % labels.len() as u64) as usize];
    let (left_text, right_text) = written(chosen).or_else(|| written(labels[0]))?;
    Some(Pair {
        key,
        left: left_text,
        right: right_text,
        picked: if left.0 > right.0 {
            TalentSide::Left
        } else {
            TalentSide::Right
        },
        contrast,
    })
}

/// A win-rate pair over the games `side` sorts to the left or the right.
fn rates(
    seed: u64,
    key: &'static str,
    labels: Labels,
    scale: f64,
    games: &[&DistinctMatch],
    side: impl Fn(&DistinctMatch) -> Option<TalentSide>,
) -> Option<Pair> {
    let rate = |wanted: TalentSide| {
        let (wins, known) = counted(games, |game| {
            (side(game)? == wanted).then_some(game.won).flatten()
        });
        (known >= MIN_SIDE_GAMES).then(|| percent(wins, known))
    };
    let (left, right) = (rate(TalentSide::Left)?, rate(TalentSide::Right)?);
    pair(
        seed,
        key,
        labels,
        (left as f64, left.to_string()),
        (right as f64, right.to_string()),
        left.abs_diff(right) as f64 / scale,
    )
}

fn kills_assists(games: &[&DistinctMatch], seed: u64) -> Option<Pair> {
    let average = |field: fn(&DistinctMatch) -> Option<i64>| {
        let known = games
            .iter()
            .filter_map(|game| nonnegative(field(game)))
            .collect::<Vec<_>>();
        (known.len() >= MIN_SIDE_GAMES)
            .then(|| known.iter().sum::<i64>() as f64 / known.len() as f64)
    };
    let (kills, assists) = (average(|game| game.kills)?, average(|game| game.assists)?);
    pair(
        seed,
        "kills_assists",
        KILLS_ASSISTS,
        (kills, format!("{kills:.1}")),
        (assists, format!("{assists:.1}")),
        (kills - assists).abs() / kills.max(assists).max(1.0) / 0.8,
    )
}

fn sides(games: &[&DistinctMatch], seed: u64) -> Option<Pair> {
    rates(seed, "sides", SIDES, RATE_SCALE, games, |game| {
        match game.side.as_deref() {
            Some("radiant") => Some(TalentSide::Left),
            Some("dire") => Some(TalentSide::Right),
            _ => None,
        }
    })
}

/// Core against support: by win rate when both roles have a sample, else by
/// how many games went to each.
fn roles(games: &[&DistinctMatch], seed: u64) -> Option<Pair> {
    let role = |game: &DistinctMatch| {
        core(game).map(|core| {
            if core {
                TalentSide::Left
            } else {
                TalentSide::Right
            }
        })
    };
    rates(seed, "roles", ROLE_RATES, RATE_SCALE, games, role).or_else(|| {
        let (cores, known) = counted(games, core);
        let supports = known - cores;
        if known < MIN_SPLIT_GAMES {
            return None;
        }
        pair(
            seed,
            "roles",
            ROLE_GAMES,
            (cores as f64, cores.to_string()),
            (supports as f64, supports.to_string()),
            cores.abs_diff(supports) as f64 / known as f64 / 1.25,
        )
    })
}

fn length(games: &[&DistinctMatch], seed: u64) -> Option<Pair> {
    rates(
        seed,
        "length",
        LENGTH,
        RATE_SCALE,
        games,
        |game| match minutes(game)? {
            played if played < 35.0 => Some(TalentSide::Left),
            played if played > 45.0 => Some(TalentSide::Right),
            _ => None,
        },
    )
}

/// Favorites are expected to win more, so the gap has to be wider to count.
fn odds(games: &[&DistinctMatch], seed: u64) -> Option<Pair> {
    rates(seed, "odds", ODDS, 40.0, games, |game| {
        match finite(game.expected_win).filter(|odds| *odds <= 1.0)? {
            odds if odds >= 0.55 => Some(TalentSide::Left),
            odds if odds <= 0.45 => Some(TalentSide::Right),
            _ => None,
        }
    })
}

fn week(games: &[&DistinctMatch], timezone: Option<&str>, seed: u64) -> Option<Pair> {
    rates(seed, "week", WEEK, RATE_SCALE, games, |game| {
        let finished = local_time(game.finished_at.filter(|time| *time > 0)?, timezone)?;
        Some(match finished.weekday() {
            Weekday::Sat | Weekday::Sun => TalentSide::Right,
            _ => TalentSide::Left,
        })
    })
}

fn halves(games: &[&DistinctMatch], timezone: Option<&str>, seed: u64) -> Option<Pair> {
    rates(seed, "halves", HALVES, RATE_SCALE, games, |game| {
        let finished = local_time(game.finished_at.filter(|time| *time > 0)?, timezone)?;
        Some(if finished.month() <= 6 {
            TalentSide::Left
        } else {
            TalentSide::Right
        })
    })
}

/// Gold income against team-fight presence, each as the player's average
/// over or under the guild median.
fn farm_fight(matches: &[DistinctMatch], player_id: i64, seed: u64) -> Option<Pair> {
    let against_guild = |field: fn(&DistinctMatch) -> Option<f64>| {
        let mut known: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
        for game in matches {
            if let Some(value) = field(game) {
                known.entry(game.discord_id).or_default().push(value);
            }
        }
        let averages = known
            .into_iter()
            .filter(|(_, values)| values.len() >= MIN_SIDE_GAMES)
            .map(|(player, values)| (player, values.iter().sum::<f64>() / values.len() as f64))
            .collect::<BTreeMap<_, _>>();
        let mine = *averages.get(&player_id)?;
        let mut all = averages.into_values().collect::<Vec<_>>();
        if all.len() < MIN_GUILD_PLAYERS {
            return None;
        }
        all.sort_by(f64::total_cmp);
        let median = (all[(all.len() - 1) / 2] + all[all.len() / 2]) / 2.0;
        // Adding zero turns a rounded negative zero into a plain one.
        (median > 0.0).then(|| ((mine / median - 1.0) * 100.0).round() + 0.0)
    };
    let farm = against_guild(|game| nonnegative(game.gpm).map(|gpm| gpm as f64))?;
    let fight =
        against_guild(|game| finite(game.teamfight_participation).filter(|share| *share <= 1.0))?;
    pair(
        seed,
        "farm_fight",
        FARM_FIGHT,
        (farm, format!("{farm:+.0}")),
        (fight, format!("{fight:+.0}")),
        (farm - fight).abs() / 30.0,
    )
}

/// Games on the most-played hero against the number of heroes played.
fn hero_pool(games: &[&DistinctMatch], seed: u64) -> Option<Pair> {
    let heroes = games
        .iter()
        .filter_map(|game| hero_of(game))
        .collect::<Vec<_>>();
    if heroes.len() < MIN_SPLIT_GAMES {
        return None;
    }
    let distinct = heroes.iter().collect::<BTreeSet<_>>();
    let top = distinct
        .iter()
        .map(|hero| heroes.iter().filter(|played| played == hero).count())
        .max()?;
    let distinct = distinct.len();
    pair(
        seed,
        "hero_pool",
        HERO_POOL,
        (top as f64, top.to_string()),
        (distinct as f64, distinct.to_string()),
        top.abs_diff(distinct) as f64 / top.max(distinct) as f64 / 1.25,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(id: i64) -> DistinctMatch {
        DistinctMatch {
            match_id: id,
            discord_id: 7,
            ..Default::default()
        }
    }

    fn refs(rows: &[DistinctMatch]) -> Vec<&DistinctMatch> {
        rows.iter().collect()
    }

    /// The number an option leads with, sign included.
    fn number(text: &str) -> f64 {
        text.trim_start_matches('+')
            .split(['%', ' '])
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }

    /// Ten games per side: `radiant_wins` and `dire_wins` of them won.
    fn sided(radiant_wins: i64, dire_wins: i64) -> Vec<DistinctMatch> {
        (0..20)
            .map(|id| DistinctMatch {
                side: Some(if id < 10 { "radiant" } else { "dire" }.to_owned()),
                won: Some(if id < 10 {
                    id < radiant_wins
                } else {
                    id - 10 < dire_wins
                }),
                ..game(id)
            })
            .collect()
    }

    #[test]
    fn side_pair_shows_both_rates_and_picks_the_higher() {
        let rows = sided(6, 4);
        let pair = sides(&refs(&rows), 0).expect("sides");
        assert!(pair.left.starts_with("60% "));
        assert!(pair.right.starts_with("40% "));
        assert_eq!(pair.picked, TalentSide::Left);
        assert!((pair.contrast - 0.8).abs() < 1e-9);
        let pair = sides(&refs(&sided(2, 9)), 0).expect("sides");
        assert_eq!(pair.picked, TalentSide::Right);
    }

    #[test]
    fn a_rate_pair_needs_five_known_results_a_side_and_two_different_numbers() {
        // Equal rates favor neither side.
        assert!(sides(&refs(&sided(5, 5)), 0).is_none());
        // Unknown results and unknown sides are not games on a side.
        let mut rows = sided(6, 4);
        for row in rows.iter_mut().take(6) {
            row.won = None;
        }
        assert!(sides(&refs(&rows), 0).is_none());
        let mut rows = sided(6, 4);
        for row in rows.iter_mut().skip(14) {
            row.side = None;
        }
        assert!(sides(&refs(&rows), 0).is_none());
        rows[14].side = Some("dire".to_owned());
        assert!(sides(&refs(&rows), 0).is_some());
    }

    #[test]
    fn kills_and_assists_compare_per_game_averages_of_known_values() {
        let mut rows = (0..6)
            .map(|id| DistinctMatch {
                kills: Some(9),
                assists: Some(14),
                ..game(id)
            })
            .collect::<Vec<_>>();
        rows[0].kills = Some(10);
        rows[1].kills = None;
        rows[2].assists = Some(-1);
        let pair = kills_assists(&refs(&rows), 0).expect("kills and assists");
        assert!(pair.left.starts_with("+9.2 Kills"));
        assert!(pair.right.starts_with("+14.0 Assists"));
        assert_eq!(pair.picked, TalentSide::Right);
        rows[3].kills = None;
        assert!(kills_assists(&refs(&rows), 0).is_none());
        // 9.0 against 9.0 is not a choice.
        let even = (0..6)
            .map(|id| DistinctMatch {
                kills: Some(9),
                assists: Some(9),
                ..game(id)
            })
            .collect::<Vec<_>>();
        assert!(kills_assists(&refs(&even), 0).is_none());
    }

    #[test]
    fn roles_use_win_rates_when_both_have_a_sample_and_game_counts_otherwise() {
        let rows = |supports: i64| {
            (0..12)
                .map(|id| DistinctMatch {
                    position: Some(if id < supports { 5 } else { 1 }),
                    won: Some(id % 2 == 0 || id >= supports),
                    ..game(id)
                })
                .collect::<Vec<_>>()
        };
        let pair = roles(&refs(&rows(6)), 0).expect("role rates");
        assert!(pair.left.starts_with("100% "));
        assert!(pair.right.starts_with("50% "));
        assert_eq!(pair.picked, TalentSide::Left);

        let pair = roles(&refs(&rows(2)), 0).expect("role games");
        assert!(pair.left.starts_with("+10 "));
        assert!(pair.right.starts_with("+2 "));
        assert_eq!(pair.picked, TalentSide::Left);
        // An even split of games picks nothing.
        let mut even = rows(5);
        even.truncate(10);
        for row in &mut even {
            row.won = None;
        }
        assert!(roles(&refs(&even), 0).is_none());
        even[0].position = Some(1);
        assert!(roles(&refs(&even), 0).is_some());
        // Nine games with a known position are too few to split.
        let mut thin = rows(2);
        for row in thin.iter_mut().skip(9) {
            row.position = None;
        }
        assert!(roles(&refs(&thin), 0).is_none());
        thin[9].position = Some(9);
        assert!(roles(&refs(&thin), 0).is_none());
    }

    #[test]
    fn length_and_odds_pairs_leave_out_the_middle_and_the_unknown() {
        let rows = (0..15)
            .map(|id| DistinctMatch {
                duration_seconds: Some(match id {
                    0..5 => 30 * 60,
                    5..10 => 50 * 60,
                    _ => 40 * 60,
                }),
                expected_win: Some(match id {
                    0..5 => 0.6,
                    5..10 => 0.4,
                    _ => 0.5,
                }),
                won: Some(id < 4 || id == 5 || id >= 10),
                ..game(id)
            })
            .collect::<Vec<_>>();
        let pair = length(&refs(&rows), 0).expect("length");
        assert_eq!((number(&pair.left), number(&pair.right)), (80.0, 20.0));
        assert_eq!(pair.picked, TalentSide::Left);
        let pair = odds(&refs(&rows), 0).expect("odds");
        assert_eq!((number(&pair.left), number(&pair.right)), (80.0, 20.0));
        assert!((pair.contrast - 1.5).abs() < 1e-9);

        let mut unknown = rows.clone();
        unknown[0].duration_seconds = Some(0);
        unknown[1].expected_win = Some(f64::NAN);
        assert!(length(&refs(&unknown), 0).is_none());
        assert!(odds(&refs(&unknown), 0).is_none());
        unknown[1].expected_win = Some(1.5);
        assert!(odds(&refs(&unknown), 0).is_none());
    }

    #[test]
    fn week_and_half_year_pairs_follow_the_players_clock() {
        // 2026-01-03 02:00 UTC is a Saturday in Berlin and still Friday in
        // New York, the default zone.
        let saturday_utc = 1_767_405_600;
        let rows = (0..10)
            .map(|id| DistinctMatch {
                // Five games a week apart from that moment, five on Tuesdays.
                finished_at: Some(if id < 5 {
                    saturday_utc + id * 7 * 86_400
                } else {
                    saturday_utc + 3 * 86_400 + id * 7 * 86_400
                }),
                won: Some(id < 4),
                ..game(id)
            })
            .collect::<Vec<_>>();
        assert!(week(&refs(&rows), None, 0).is_none());
        let pair = week(&refs(&rows), Some("Europe/Berlin"), 0).expect("week");
        assert_eq!((number(&pair.left), number(&pair.right)), (0.0, 80.0));
        assert_eq!(pair.picked, TalentSide::Right);

        // 2026-07-01 02:00 UTC is still June in New York.
        let july_utc = 1_782_871_200;
        let rows = (0..10)
            .map(|id| DistinctMatch {
                finished_at: Some(if id < 5 {
                    july_utc
                } else {
                    july_utc + 86_400 * (id + 1)
                }),
                won: Some(id < 5),
                ..game(id)
            })
            .collect::<Vec<_>>();
        let pair = halves(&refs(&rows), None, 0).expect("halves");
        assert_eq!((number(&pair.left), number(&pair.right)), (100.0, 0.0));
        assert!(halves(&refs(&rows), Some("Europe/Berlin"), 0).is_none());
        let mut undated = rows.clone();
        undated[0].finished_at = None;
        assert!(halves(&refs(&undated), None, 0).is_none());
    }

    /// Five players with six games each; player 7 has `gpm` and `fights`.
    fn guild(gpm: i64, fights: f64) -> Vec<DistinctMatch> {
        (0..30)
            .map(|id| {
                let player = 7 + id % 5;
                DistinctMatch {
                    discord_id: player,
                    gpm: Some(if player == 7 { gpm } else { 400 }),
                    teamfight_participation: Some(if player == 7 { fights } else { 0.5 }),
                    ..game(id)
                }
            })
            .collect()
    }

    #[test]
    fn farm_and_fight_are_measured_against_the_guild_median() {
        let rows = guild(500, 0.45);
        let pair = farm_fight(&rows, 7, 0).expect("farm and fight");
        assert!(pair.left.starts_with("+25% "));
        assert!(pair.right.starts_with("-10% "));
        assert_eq!(pair.picked, TalentSide::Left);
        assert!((pair.contrast - 35.0 / 30.0).abs() < 1e-9);

        // Level with the guild on both is not a choice.
        assert!(farm_fight(&guild(400, 0.5), 7, 0).is_none());
        // Four players are not a guild median.
        let small = rows
            .iter()
            .filter(|row| row.discord_id != 11)
            .cloned()
            .collect::<Vec<_>>();
        assert!(farm_fight(&small, 7, 0).is_none());
        // A share above one is not a share.
        let mut broken = rows.clone();
        for row in broken.iter_mut().filter(|row| row.discord_id == 7).take(2) {
            row.teamfight_participation = Some(1.5);
        }
        assert!(farm_fight(&broken, 7, 0).is_none());
        assert!(farm_fight(&rows, 99, 0).is_none());
    }

    #[test]
    fn hero_pool_weighs_the_top_hero_against_the_number_of_heroes() {
        let rows = |heroes: [i64; 10]| {
            heroes
                .iter()
                .enumerate()
                .map(|(id, hero)| DistinctMatch {
                    hero_id: Some(*hero),
                    ..game(id as i64)
                })
                .collect::<Vec<_>>()
        };
        let pair = hero_pool(&refs(&rows([1, 1, 1, 1, 1, 1, 1, 2, 3, 4])), 0).expect("one hero");
        assert_eq!((number(&pair.left), number(&pair.right)), (7.0, 4.0));
        assert_eq!(pair.picked, TalentSide::Left);
        let pair = hero_pool(&refs(&rows([1, 1, 2, 3, 4, 5, 6, 7, 8, 9])), 0).expect("variety");
        assert_eq!((number(&pair.left), number(&pair.right)), (2.0, 9.0));
        assert_eq!(pair.picked, TalentSide::Right);
        assert!(hero_pool(&refs(&rows([1, 1, 1, 2, 2, 2, 3, 3, 3, 0])), 0).is_none());
        assert!(hero_pool(&refs(&rows([1, 1, 1, 1, 2, 2, 3, 3, 4, 4])), 0).is_none());
    }

    /// A year in which six pairs have data.
    fn full_year() -> Vec<DistinctMatch> {
        let mut rows = (0..40)
            .map(|id| DistinctMatch {
                side: Some(if id % 2 == 0 { "radiant" } else { "dire" }.to_owned()),
                won: Some(id % 2 == 0 || id % 10 == 1),
                kills: Some(4),
                assists: Some(16),
                position: Some(if id < 30 { 5 } else { 2 }),
                duration_seconds: Some(if id % 4 < 2 { 30 * 60 } else { 50 * 60 }),
                expected_win: Some(if id % 3 == 0 { 0.6 } else { 0.4 }),
                hero_id: Some(1 + id % 8),
                finished_at: Some(1_767_405_600 + id * 86_400),
                ..game(id)
            })
            .collect::<Vec<_>>();
        rows.extend((0..40).map(|id| DistinctMatch {
            discord_id: 8,
            ..game(id)
        }));
        rows
    }

    #[test]
    fn the_tree_has_four_tiers_from_the_clearest_contrast_down() {
        let rows = full_year();
        let tree = talent_tree(&rows, 7, None, 0);
        assert_eq!(
            tree.iter().map(|tier| tier.level).collect::<Vec<_>>(),
            TALENT_LEVELS
        );
        // Radiant 100% against Dire 20% is the widest gap on offer.
        assert_eq!(
            (number(&tree[0].left), number(&tree[0].right)),
            (100.0, 20.0)
        );
        for tier in &tree {
            for text in [&tier.left, &tier.right] {
                assert!(text.chars().count() <= MAX_TALENT_TEXT, "{text}");
            }
            // The picked side is always the bigger number on the card.
            let (left, right) = (number(&tier.left), number(&tier.right));
            assert_ne!(left, right);
            assert_eq!(tier.picked == TalentSide::Left, left > right);
        }
        assert!(talent_tree(&rows, 8, None, 0).is_empty());
        assert!(talent_tree(&rows, 99, None, 0).is_empty());
    }

    #[test]
    fn fewer_than_four_pairs_is_no_tree() {
        let mut rows = full_year();
        for row in &mut rows {
            row.kills = None;
            row.position = None;
            row.duration_seconds = None;
            row.hero_id = None;
        }
        // Sides, odds and the week are left.
        assert!(talent_tree(&rows, 7, None, 0).is_empty());
        for row in &mut rows {
            row.hero_id = Some(1 + row.match_id % 8);
        }
        assert_eq!(talent_tree(&rows, 7, None, 0).len(), 4);
    }

    #[test]
    fn the_tree_is_stable_under_reordering_and_worded_by_seed() {
        let mut rows = full_year();
        let tree = talent_tree(&rows, 7, Some("Europe/Berlin"), 4);
        rows.reverse();
        assert_eq!(talent_tree(&rows, 7, Some("Europe/Berlin"), 4), tree);
        let trees = (0..32)
            .map(|seed| format!("{:?}", talent_tree(&rows, 7, None, seed)))
            .collect::<BTreeSet<_>>();
        assert!(trees.len() > 1);
        // The seed changes wording and tie order, never the numbers.
        for seed in 0..32 {
            let other = talent_tree(&rows, 7, Some("Europe/Berlin"), seed);
            assert_eq!(number(&other[0].left), number(&tree[0].left));
            assert_eq!(other[0].picked, tree[0].picked);
        }
    }

    #[test]
    fn every_wording_fits_and_the_shortest_stands_in_for_wide_numbers() {
        for (labels, widest) in [
            (KILLS_ASSISTS, "99.9"),
            (SIDES, "100"),
            (ROLE_RATES, "100"),
            (ROLE_GAMES, "999"),
            (LENGTH, "100"),
            (ODDS, "100"),
            (WEEK, "100"),
            (FARM_FIGHT, "+100"),
            (HERO_POOL, "999"),
            (HALVES, "100"),
        ] {
            assert!(labels.len() >= 2);
            for (index, (left, right)) in labels.iter().enumerate() {
                for label in [left, right] {
                    let text = label.replace("{}", widest);
                    assert!(text.contains(widest), "{label}");
                    assert!(
                        index > 0 || text.chars().count() <= MAX_TALENT_TEXT,
                        "{text}"
                    );
                    assert!(text.is_ascii());
                }
            }
        }
        // Every seed gets a wording that fits, even when its first choice
        // would not.
        for seed in 0..16 {
            let wide = pair(
                seed,
                "hero_pool",
                HERO_POOL,
                (1234.0, "1234".to_owned()),
                (999.0, "999".to_owned()),
                1.0,
            )
            .expect("the short wording fits");
            assert_eq!(wide.left, "+1234 On Top Hero");
        }
        let fits_nowhere = pair(
            0,
            "hero_pool",
            HERO_POOL,
            (1.0, "1".to_owned()),
            (2.0, "123456789".to_owned()),
            1.0,
        );
        assert!(fits_nowhere.is_none());
    }
}
