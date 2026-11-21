//! Bug Fixes: the year's worst games and habits, in the voice of a patch
//! that claims to have fixed them.
//!
//! Every number in a line is read off the player's own rows. A line about
//! one game may name its hero with the literal `{hero}`; the runtime fills
//! it in. Roasts are about play only.

use std::collections::BTreeMap;

use chrono::Timelike;

use cama_domain::formatting::format_grouped;
use cama_domain::wrapped_distinct::{DistinctMatch, local_time};

use super::format_flavor;
use super::patch_support::{
    core, counted, finite, hero_of, minutes, nonnegative, own_games, percent, timeline, variant,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BugFix {
    pub key: &'static str,
    /// May contain the literal `{hero}`.
    pub line: String,
    /// Set exactly when `line` contains `{hero}`.
    pub hero_id: Option<i64>,
    /// Phrased as still happening rather than fixed.
    pub known_issue: bool,
}

pub const MAX_BUG_FIXES: usize = 4;

/// Only one line per theme makes the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Theme {
    Deaths,
    Results,
    Gold,
    Kills,
    Items,
    Comms,
    Heroes,
    Length,
    Clock,
    Vision,
}

struct Bug {
    key: &'static str,
    theme: Theme,
    /// Wordings that claim the bug is fixed.
    fixed: &'static [&'static str],
    /// Wordings for a habit that is still going; empty for one-off games.
    ongoing: &'static [&'static str],
}

const BUGS: &[Bug] = &[
    Bug {
        key: "most_deaths",
        theme: Theme::Deaths,
        fixed: &[
            "Fixed {hero} dying {n} times in a single game.",
            "Fixed one game containing {n} deaths.",
        ],
        ongoing: &[],
    },
    Bug {
        key: "loss_streak",
        theme: Theme::Results,
        fixed: &[
            "Fixed a {n}-game losing streak.",
            "Losing {n} games in a row is no longer a strategy.",
        ],
        ongoing: &[],
    },
    Bug {
        key: "throw",
        theme: Theme::Gold,
        fixed: &[
            "Fixed a {k}k gold lead not resulting in a win.",
            "Fixed {hero}'s team losing from {k}k gold ahead.",
            "A {k}k gold lead no longer counts as a win.",
        ],
        ongoing: &[],
    },
    Bug {
        key: "kills_in_loss",
        theme: Theme::Kills,
        fixed: &[
            "Fixed {n} kills not being enough.",
            "Fixed {hero} getting {n} kills and losing anyway.",
        ],
        ongoing: &[],
    },
    Bug {
        key: "favorite_loss",
        theme: Theme::Results,
        fixed: &[
            "Fixed losing a game with a {p}% win chance.",
            "Fixed a {p}% favorite not winning.",
            "A {p}% win chance no longer counts as a win.",
        ],
        ongoing: &[],
    },
    Bug {
        key: "buyback_losses",
        theme: Theme::Items,
        fixed: &[
            "Fixed buybacks not preventing {l} losses.",
            "Fixed {l} games being lost after a buyback.",
        ],
        ongoing: &[
            "Buying back still loses games: {l} of {k} so far.",
            "Buybacks still end in defeat. {l} of {k} games.",
        ],
    },
    Bug {
        key: "rapier_loss",
        theme: Theme::Items,
        fixed: &[
            "Fixed a Divine Rapier not preventing a loss.",
            "Fixed {hero} buying a Divine Rapier and losing.",
            "Divine Rapier no longer guarantees a win.",
        ],
        ongoing: &[],
    },
    Bug {
        key: "rapier_losses",
        theme: Theme::Items,
        fixed: &[
            "Fixed Divine Rapier purchases losing {l} games.",
            "Fixed {l} games being lost with a Divine Rapier.",
        ],
        ongoing: &[
            "Divine Rapier games still get lost. {l} so far.",
            "Still losing with a Divine Rapier: {l} games.",
        ],
    },
    Bug {
        key: "pings",
        theme: Theme::Comms,
        fixed: &[
            "Fixed map pings averaging {n} per game.",
            "Reduced map pings from {n} per game.",
        ],
        ongoing: &[
            "Map pings still average {n} per game.",
            "The map is still pinged {n} times per game.",
        ],
    },
    Bug {
        key: "worst_hero",
        theme: Theme::Heroes,
        fixed: &[
            "{hero} win rate ({p}%) is now working as intended.",
            "Fixed {hero} winning only {p}% of {n} games.",
        ],
        ongoing: &[
            "{hero} win rate remains {p}% after {n} games.",
            "{hero} is still being picked. Win rate: {p}%.",
        ],
    },
    Bug {
        key: "longest_game",
        theme: Theme::Length,
        fixed: &[
            "Fixed a game lasting {m} minutes.",
            "Fixed {hero} being kept in a {m}-minute game.",
            "Games no longer last {m} minutes. Ideally.",
        ],
        ongoing: &[],
    },
    Bug {
        key: "late_nights",
        theme: Theme::Clock,
        fixed: &[
            "Fixed {n} games ending between midnight and 6 AM.",
            "Fixed {n} games running past midnight.",
        ],
        ongoing: &[
            "Matches still finish after midnight. {n} so far.",
            "{n} games still ended between midnight and 6 AM.",
        ],
    },
    Bug {
        key: "no_wards",
        theme: Theme::Vision,
        fixed: &[
            "Fixed {z} support games ending with 0 wards.",
            "Fixed 0 wards in {z} of {k} support games.",
        ],
        ongoing: &[
            "Still 0 wards in {z} of {k} support games.",
            "Support games still end with 0 wards: {z} of {k}.",
        ],
    },
    Bug {
        key: "lowest_gpm",
        theme: Theme::Gold,
        fixed: &[
            "Fixed {hero} finishing a game at {n} GPM.",
            "Fixed an entire game played at {n} GPM.",
            "Gold income no longer drops to {n} GPM. In theory.",
        ],
        ongoing: &[],
    },
    Bug {
        key: "no_first_blood",
        theme: Theme::Kills,
        fixed: &[
            "Fixed 0 first bloods across {n} games.",
            "Fixed first blood going elsewhere for {n} games.",
        ],
        ongoing: &[
            "Still 0 first bloods in {n} games.",
            "First blood still goes to someone else. {n} games.",
        ],
    },
    Bug {
        key: "deaths_rising",
        theme: Theme::Deaths,
        fixed: &[
            "Fixed deaths per game rising from {a} to {b}.",
            "Deaths per game are no longer {a}. Now {b}.",
        ],
        ongoing: &[
            "Deaths per game still rising: {a} to {b}.",
            "Deaths per game went {a} to {b}. Not yet fixed.",
        ],
    },
    Bug {
        key: "core_no_kills",
        theme: Theme::Kills,
        fixed: &[
            "Fixed {z} core games finishing with 0 kills.",
            "Fixed {z} games as a core ending without a kill.",
        ],
        ongoing: &[
            "Core games still end with 0 kills. {z} so far.",
            "{z} games as a core still ended without a kill.",
        ],
    },
    Bug {
        key: "fast_loss",
        theme: Theme::Length,
        fixed: &[
            "Fixed a loss taking only {m} minutes.",
            "Fixed {hero} losing the game in {m} minutes.",
            "Reduced time to defeat to {m} minutes in one game.",
        ],
        ongoing: &[],
    },
    Bug {
        key: "bad_side",
        theme: Theme::Results,
        fixed: &[
            "Fixed winning only {p}% of {side} games.",
            "Fixed a {p}% win rate across {n} {side} games.",
        ],
        ongoing: &[
            "{side} win rate remains {p}% across {n} games.",
            "{side} is still a {p}% win rate. No fix planned.",
        ],
    },
];

fn bug(key: &str) -> &'static Bug {
    BUGS.iter()
        .find(|bug| bug.key == key)
        .expect("every reported bug is in the catalog")
}

/// One bug the player's rows prove.
struct Found {
    key: &'static str,
    /// 1.0 at the gate, about 2.0 for a dramatic case.
    severity: f64,
    /// The single game behind the line, if there is one.
    match_id: Option<i64>,
    hero_id: Option<i64>,
    variables: Vec<(&'static str, String)>,
}

impl Found {
    fn new(key: &'static str, severity: f64) -> Self {
        Self {
            key,
            severity,
            match_id: None,
            hero_id: None,
            variables: Vec::new(),
        }
    }

    fn game(mut self, game: &DistinctMatch) -> Self {
        self.match_id = Some(game.match_id);
        self.hero_id = hero_of(game);
        self
    }

    fn with(mut self, name: &'static str, value: impl ToString) -> Self {
        self.variables.push((name, value.to_string()));
        self
    }
}

fn lost(game: &DistinctMatch) -> bool {
    game.won == Some(false)
}

/// The game with the highest `score`; the earliest wins a tie.
fn worst<'a>(
    games: &[&'a DistinctMatch],
    score: impl Fn(&DistinctMatch) -> Option<f64>,
) -> Option<(f64, &'a DistinctMatch)> {
    games
        .iter()
        .filter_map(|game| Some((score(game)?, *game)))
        .reduce(|best, next| if next.0 > best.0 { next } else { best })
}

/// Up to four bug fixes for `player_id`, the most severe first, with at
/// most one line per theme and per game. The most severe ongoing habit is
/// reported as a known issue instead, last in the list.
#[must_use]
pub fn bug_fixes(
    matches: &[DistinctMatch],
    player_id: i64,
    timezone: Option<&str>,
    seed: u64,
) -> Vec<BugFix> {
    let games = own_games(matches, player_id);
    let mut found = [
        most_deaths(&games),
        loss_streak(&games),
        throw(&games),
        kills_in_loss(&games),
        favorite_loss(&games),
        buyback_losses(&games),
        rapier_losses(&games),
        pings(&games),
        worst_hero(&games),
        longest_game(&games),
        late_nights(&games, timezone),
        no_wards(&games),
        lowest_gpm(&games),
        no_first_blood(&games),
        deaths_rising(&games),
        core_no_kills(&games),
        fast_loss(&games),
        bad_side(&games),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    found.sort_by(|a, b| {
        b.severity
            .total_cmp(&a.severity)
            .then_with(|| a.key.cmp(b.key))
    });

    let same_theme = |a: &Found, b: &Found| bug(a.key).theme == bug(b.key).theme;
    let mut chosen: Vec<&Found> = Vec::new();
    for next in &found {
        let clash = chosen.iter().any(|held| {
            same_theme(held, next) || (next.match_id.is_some() && held.match_id == next.match_id)
        });
        if chosen.len() < MAX_BUG_FIXES && !clash {
            chosen.push(next);
        }
    }
    // A habit among the chosen becomes the known issue. When there is none,
    // the most severe habit that fits beside the top three takes the last
    // slot.
    let habit = |found: &&Found| !bug(found.key).ongoing.is_empty();
    let known_issue = chosen.iter().position(&habit).or_else(|| {
        let kept = chosen.len().min(MAX_BUG_FIXES - 1);
        let extra = found
            .iter()
            .filter(habit)
            .find(|extra| chosen[..kept].iter().all(|held| !same_theme(held, extra)))?;
        chosen.truncate(kept);
        chosen.push(extra);
        Some(kept)
    });
    if let Some(index) = known_issue {
        let issue = chosen.remove(index);
        chosen.push(issue);
    }

    chosen
        .iter()
        .enumerate()
        .map(|(index, found)| {
            let known_issue = known_issue.is_some() && index + 1 == chosen.len();
            let entry = bug(found.key);
            let pool = if known_issue {
                entry.ongoing
            } else {
                entry.fixed
            };
            // Without a hero only the wordings that do not need one remain.
            let usable = pool
                .iter()
                .copied()
                .filter(|template| found.hero_id.is_some() || !template.contains("{hero}"))
                .collect::<Vec<_>>();
            let mut variables = vec![("hero", "{hero}")];
            variables.extend(
                found
                    .variables
                    .iter()
                    .map(|(name, value)| (*name, value.as_str())),
            );
            let line = format_flavor(variant(seed, found.key, &usable), &variables);
            BugFix {
                key: found.key,
                hero_id: found.hero_id.filter(|_| line.contains("{hero}")),
                line,
                known_issue,
            }
        })
        .collect()
}

fn most_deaths(games: &[&DistinctMatch]) -> Option<Found> {
    let (deaths, game) = worst(games, |game| {
        nonnegative(game.deaths)
            .filter(|deaths| *deaths >= 12)
            .map(|deaths| deaths as f64)
    })?;
    Some(
        Found::new("most_deaths", deaths / 12.0)
            .game(game)
            .with("n", deaths),
    )
}

/// The longest run of dated losses; an unknown result ends a run.
fn loss_streak(games: &[&DistinctMatch]) -> Option<Found> {
    let (mut best, mut run) = (0_usize, 0_usize);
    for game in games
        .iter()
        .filter(|game| game.finished_at.is_some_and(|time| time > 0))
    {
        run = if lost(game) { run + 1 } else { 0 };
        best = best.max(run);
    }
    (best >= 4).then(|| Found::new("loss_streak", (best as f64 / 4.0).min(2.5)).with("n", best))
}

fn throw(games: &[&DistinctMatch]) -> Option<Found> {
    let (gold, game) = worst(games, |game| {
        finite(game.throw).filter(|gold| lost(game) && *gold >= 8_000.0)
    })?;
    Some(
        Found::new("throw", (gold / 8_000.0).min(3.0))
            .game(game)
            // Rounded down, so the lead was at least this big.
            .with("k", (gold / 1_000.0).floor()),
    )
}

fn kills_in_loss(games: &[&DistinctMatch]) -> Option<Found> {
    let (kills, game) = worst(games, |game| {
        nonnegative(game.kills)
            .filter(|kills| lost(game) && *kills >= 15)
            .map(|kills| kills as f64)
    })?;
    Some(
        Found::new("kills_in_loss", kills / 15.0)
            .game(game)
            .with("n", kills),
    )
}

fn favorite_loss(games: &[&DistinctMatch]) -> Option<Found> {
    let (odds, game) = worst(games, |game| {
        finite(game.expected_win).filter(|odds| lost(game) && (0.70..=1.0).contains(odds))
    })?;
    Some(
        Found::new("favorite_loss", 1.0 + (odds - 0.70) / 0.15)
            .game(game)
            // Rounded down, so the chance was at least this good.
            .with("p", (odds * 100.0 + 1e-9).floor()),
    )
}

fn buyback_losses(games: &[&DistinctMatch]) -> Option<Found> {
    let (losses, known) = counted(games, |game| {
        (nonnegative(game.buybacks)? > 0)
            .then_some(game.won.map(|won| !won))
            .flatten()
    });
    (losses >= 3).then(|| {
        Found::new("buyback_losses", (1.0 + losses as f64 / 5.0).min(2.2))
            .with("l", losses)
            .with("k", known)
    })
}

/// Lost games with a Divine Rapier, from the Rapier count or the purchase
/// log: one game is a story about that game, more are a habit.
fn rapier_losses(games: &[&DistinctMatch]) -> Option<Found> {
    let lost_with = games
        .iter()
        .copied()
        .filter(|game| {
            let counted = nonnegative(game.rapiers).is_some_and(|rapiers| rapiers > 0);
            let logged = game.items.as_ref().is_some_and(|items| {
                items
                    .iter()
                    .any(|(key, count)| key == "rapier" && *count > 0)
            });
            lost(game) && (counted || logged)
        })
        .collect::<Vec<_>>();
    match lost_with[..] {
        [] => None,
        [game] => Some(Found::new("rapier_loss", 1.6).game(game)),
        _ => Some(
            Found::new(
                "rapier_losses",
                (1.6 + 0.3 * (lost_with.len() - 1) as f64).min(2.5),
            )
            .with("l", lost_with.len()),
        ),
    }
}

fn pings(games: &[&DistinctMatch]) -> Option<Found> {
    let known = games
        .iter()
        .filter_map(|game| nonnegative(game.pings))
        .collect::<Vec<_>>();
    if known.len() < 5 {
        return None;
    }
    let average = known.iter().sum::<i64>() as f64 / known.len() as f64;
    (average >= 30.0).then(|| {
        Found::new("pings", (average / 30.0).min(2.5))
            .with("n", format_grouped(average.round() as i64))
    })
}

/// The hero with the lowest win rate over five or more known results, at
/// 40% or worse.
fn worst_hero(games: &[&DistinctMatch]) -> Option<Found> {
    let mut heroes: BTreeMap<i64, (usize, usize)> = BTreeMap::new();
    for game in games {
        if let (Some(hero), Some(won)) = (hero_of(game), game.won) {
            let tally = heroes.entry(hero).or_insert((0, 0));
            tally.0 += usize::from(won);
            tally.1 += 1;
        }
    }
    let (hero, (wins, played)) = heroes
        .into_iter()
        .filter(|(_, (wins, played))| *played >= 5 && wins * 100 <= played * 40)
        // Lowest rate, then more games; the lower hero id keeps a full tie.
        .reduce(|best, next| {
            let (held, new) = (best.1.0 * next.1.1, next.1.0 * best.1.1);
            if new < held || (new == held && next.1.1 > best.1.1) {
                next
            } else {
                best
            }
        })?;
    let rate = percent(wins, played);
    let mut found = Found::new(
        "worst_hero",
        1.0 + (40.0 - rate as f64) / 20.0 + played as f64 / 50.0,
    )
    .with("p", rate)
    .with("n", played);
    found.hero_id = Some(hero);
    Some(found)
}

fn longest_game(games: &[&DistinctMatch]) -> Option<Found> {
    let (length, game) = worst(games, |game| minutes(game).filter(|length| *length >= 60.0))?;
    Some(
        Found::new("longest_game", 1.0 + (length - 60.0) / 20.0)
            .game(game)
            .with("m", length.floor()),
    )
}

fn late_nights(games: &[&DistinctMatch], timezone: Option<&str>) -> Option<Found> {
    let late = games
        .iter()
        .filter_map(|game| local_time(game.finished_at.filter(|time| *time > 0)?, timezone))
        .filter(|finished| finished.hour() < 6)
        .count();
    (late >= 3)
        .then(|| Found::new("late_nights", (1.0 + late as f64 / 10.0).min(2.5)).with("n", late))
}

fn no_wards(games: &[&DistinctMatch]) -> Option<Found> {
    let (wardless, known) = counted(games, |game| {
        (!core(game)?)
            .then_some(nonnegative(game.wards).map(|wards| wards == 0))
            .flatten()
    });
    (known >= 5 && wardless >= 3).then(|| {
        Found::new("no_wards", (1.0 + wardless as f64 / 5.0).min(2.5))
            .with("z", wardless)
            .with("k", known)
    })
}

/// The game furthest under a floor of 300 GPM as position 1 or 2, or 200
/// in any other role.
fn lowest_gpm(games: &[&DistinctMatch]) -> Option<Found> {
    let known = games
        .iter()
        .filter(|game| nonnegative(game.gpm).is_some())
        .count();
    if known < 5 {
        return None;
    }
    let floor = |game: &DistinctMatch| {
        if matches!(game.position, Some(1 | 2)) {
            300
        } else {
            200
        }
    };
    let (short, game) = worst(games, |game| {
        let gpm = nonnegative(game.gpm).filter(|gpm| *gpm > 0)?;
        (gpm < floor(game)).then(|| (floor(game) - gpm) as f64)
    })?;
    Some(
        Found::new("lowest_gpm", 1.0 + short / 100.0)
            .game(game)
            .with("n", floor(game) - short as i64),
    )
}

fn no_first_blood(games: &[&DistinctMatch]) -> Option<Found> {
    let (claimed, known) = counted(games, |game| game.first_blood);
    (known >= 15 && claimed == 0).then(|| {
        Found::new("no_first_blood", (1.0 + known as f64 / 50.0).min(1.8)).with("n", known)
    })
}

/// Deaths per game across the same early and late halves the balance
/// changes compare.
fn deaths_rising(games: &[&DistinctMatch]) -> Option<Found> {
    let split = timeline(games)?;
    let average = |half: &[&DistinctMatch]| {
        let known = half
            .iter()
            .filter_map(|game| nonnegative(game.deaths))
            .collect::<Vec<_>>();
        (known.len() >= 5).then(|| known.iter().sum::<i64>() as f64 / known.len() as f64)
    };
    let (before, after) = (average(&split.early)?, average(&split.late)?);
    (after - before >= 1.5).then(|| {
        Found::new("deaths_rising", ((after - before) / 1.5).min(2.5))
            .with("a", format!("{before:.1}"))
            .with("b", format!("{after:.1}"))
    })
}

fn core_no_kills(games: &[&DistinctMatch]) -> Option<Found> {
    let (killless, known) = counted(games, |game| {
        core(game)?
            .then_some(nonnegative(game.kills).map(|kills| kills == 0))
            .flatten()
    });
    (known >= 5 && killless >= 3).then(|| {
        Found::new("core_no_kills", (1.0 + killless as f64 / 5.0).min(2.2)).with("z", killless)
    })
}

/// The quickest loss of at least ten minutes, at twenty or under.
fn fast_loss(games: &[&DistinctMatch]) -> Option<Found> {
    let (early, game) = worst(games, |game| {
        let length = minutes(game).filter(|length| lost(game) && (10.0..=20.0).contains(length))?;
        Some(20.0 - length)
    })?;
    Some(
        Found::new("fast_loss", 1.0 + early / 8.0)
            .game(game)
            // Rounded up, so the loss took no longer than this.
            .with("m", (20.0 - early).ceil()),
    )
}

/// A side won 35% of the time or less over eight or more known results.
fn bad_side(games: &[&DistinctMatch]) -> Option<Found> {
    [("radiant", "Radiant"), ("dire", "Dire")]
        .into_iter()
        .filter_map(|(side, name)| {
            let (wins, known) = counted(games, |game| {
                (game.side.as_deref() == Some(side))
                    .then_some(game.won)
                    .flatten()
            });
            (known >= 8 && wins * 100 <= known * 35).then_some((name, wins, known))
        })
        // The lower rate; Radiant keeps a tie.
        .reduce(|best, next| {
            if next.1 * best.2 < best.1 * next.2 {
                next
            } else {
                best
            }
        })
        .map(|(name, wins, known)| {
            let rate = percent(wins, known);
            Found::new(
                "bad_side",
                1.0 + (35.0 - rate as f64) / 15.0 + known as f64 / 60.0,
            )
            .with("side", name)
            .with("p", rate)
            .with("n", known)
        })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// 2026-01-03 02:00 UTC: nine in the evening in New York, the default
    /// zone, and three in the morning in Berlin.
    const EVENING: i64 = 1_767_405_600;

    /// `count` quiet games for player 7, a day apart, each won on a hero
    /// of its own.
    fn year(count: i64) -> Vec<DistinctMatch> {
        (1..=count)
            .map(|id| DistinctMatch {
                match_id: id,
                discord_id: 7,
                finished_at: Some(EVENING + id * 86_400),
                hero_id: Some(id),
                won: Some(true),
                ..Default::default()
            })
            .collect()
    }

    fn fixes(rows: &[DistinctMatch]) -> Vec<BugFix> {
        bug_fixes(rows, 7, None, 0)
    }

    fn keys(rows: &[DistinctMatch]) -> Vec<&'static str> {
        fixes(rows).iter().map(|fix| fix.key).collect()
    }

    /// The only line, with its hero filled in.
    fn only(rows: &[DistinctMatch]) -> String {
        let fixes = fixes(rows);
        assert_eq!(fixes.len(), 1, "{fixes:?}");
        fixes[0].line.replace("{hero}", "Pudge")
    }

    #[test]
    fn a_quiet_year_has_no_bugs() {
        assert!(fixes(&year(40)).is_empty());
        assert!(fixes(&[]).is_empty());
        assert!(bug_fixes(&year(40), 8, None, 0).is_empty());
    }

    #[test]
    fn single_game_bugs_need_their_threshold_and_carry_the_number() {
        let mut rows = year(6);
        rows[2].deaths = Some(11);
        assert!(fixes(&rows).is_empty());
        rows[2].deaths = Some(19);
        assert_eq!(keys(&rows), ["most_deaths"]);
        assert!(only(&rows).contains("19"));

        let mut rows = year(6);
        rows[1].kills = Some(22);
        assert!(fixes(&rows).is_empty());
        rows[1].won = Some(false);
        assert_eq!(keys(&rows), ["kills_in_loss"]);
        assert!(only(&rows).contains("22 kills"));
        rows[1].kills = Some(14);
        assert!(fixes(&rows).is_empty());

        let mut rows = year(6);
        rows[3].throw = Some(14_900.0);
        assert!(fixes(&rows).is_empty());
        rows[3].won = Some(false);
        assert_eq!(keys(&rows), ["throw"]);
        assert!(only(&rows).contains("14k gold"));
        rows[3].throw = Some(7_999.0);
        assert!(fixes(&rows).is_empty());

        let mut rows = year(6);
        rows[0].expected_win = Some(0.789);
        rows[0].won = Some(false);
        assert_eq!(keys(&rows), ["favorite_loss"]);
        assert!(only(&rows).contains("78%"));
        rows[0].expected_win = Some(0.69);
        assert!(fixes(&rows).is_empty());

        let mut rows = year(6);
        rows[4].duration_seconds = Some(74 * 60 + 40);
        assert_eq!(keys(&rows), ["longest_game"]);
        assert!(only(&rows).contains("74"));
        rows[4].duration_seconds = Some(59 * 60 + 59);
        assert!(fixes(&rows).is_empty());

        let mut rows = year(6);
        rows[5].duration_seconds = Some(17 * 60 + 30);
        assert!(fixes(&rows).is_empty());
        rows[5].won = Some(false);
        assert_eq!(keys(&rows), ["fast_loss"]);
        assert!(only(&rows).contains("18 minutes"));
        rows[5].duration_seconds = Some(9 * 60);
        assert!(fixes(&rows).is_empty());
        rows[5].duration_seconds = Some(21 * 60);
        assert!(fixes(&rows).is_empty());
    }

    #[test]
    fn lowest_gpm_has_a_floor_per_role_and_needs_a_sample() {
        let mut rows = year(6);
        for row in &mut rows {
            row.gpm = Some(450);
        }
        rows[2].gpm = Some(250);
        assert!(fixes(&rows).is_empty());
        rows[2].position = Some(1);
        assert_eq!(keys(&rows), ["lowest_gpm"]);
        assert!(only(&rows).contains("250 GPM"));
        rows[2].position = Some(5);
        rows[2].gpm = Some(142);
        assert!(only(&rows).contains("142 GPM"));
        // A zero is a broken row, not a game.
        rows[2].gpm = Some(0);
        assert!(fixes(&rows).is_empty());
        rows[2].gpm = Some(142);
        for row in rows.iter_mut().skip(4) {
            row.gpm = None;
        }
        assert!(fixes(&rows).is_empty());
    }

    #[test]
    fn loss_streaks_follow_the_calendar_and_stop_at_an_unknown_result() {
        let mut rows = year(10);
        for row in rows.iter_mut().skip(2).take(6) {
            row.won = Some(false);
        }
        rows.reverse();
        assert_eq!(keys(&rows), ["loss_streak"]);
        assert!(only(&rows).contains('6'));
        // An unknown result in the middle leaves two runs of at most three.
        rows[4].won = None;
        assert!(fixes(&rows).is_empty());
        // Undated losses cannot be placed in a run.
        let mut rows = year(10);
        for row in rows.iter_mut().take(4) {
            row.won = Some(false);
        }
        assert_eq!(keys(&rows), ["loss_streak"]);
        rows[1].finished_at = None;
        assert!(fixes(&rows).is_empty());
    }

    #[test]
    fn habits_need_their_counts_and_read_as_known_issues() {
        let mut rows = year(8);
        for row in rows.iter_mut().take(3) {
            row.buybacks = Some(2);
            row.won = Some(false);
        }
        rows[3].buybacks = Some(1);
        rows[4].buybacks = Some(-1);
        rows[4].won = Some(false);
        let found = fixes(&rows);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "buyback_losses");
        assert!(found[0].known_issue);
        assert!(found[0].line.contains("3 of 4"));
        rows[0].won = None;
        assert!(fixes(&rows).is_empty());

        let mut rows = year(8);
        for row in &mut rows {
            row.pings = Some(62);
        }
        assert_eq!(keys(&rows), ["pings"]);
        assert!(only(&rows).contains("62"));
        for row in &mut rows {
            row.pings = Some(29);
        }
        assert!(fixes(&rows).is_empty());
        for row in rows.iter_mut().take(4) {
            row.pings = Some(500);
        }
        for row in rows.iter_mut().skip(4) {
            row.pings = None;
        }
        assert!(fixes(&rows).is_empty());

        let mut rows = year(20);
        for row in &mut rows {
            row.first_blood = Some(false);
        }
        assert_eq!(keys(&rows), ["no_first_blood"]);
        assert!(only(&rows).contains("20"));
        rows[7].first_blood = Some(true);
        assert!(fixes(&rows).is_empty());
        rows[7].first_blood = None;
        for row in rows.iter_mut().skip(14) {
            row.first_blood = None;
        }
        assert!(fixes(&rows).is_empty());
    }

    #[test]
    fn role_habits_count_only_games_in_that_role_with_known_stats() {
        let mut rows = year(8);
        for (index, row) in rows.iter_mut().enumerate() {
            row.position = Some(5);
            row.wards = Some(if index < 3 { 0 } else { 12 });
            row.kills = Some(0);
        }
        assert_eq!(keys(&rows), ["no_wards"]);
        assert!(only(&rows).contains("3 of 8"));
        rows[0].wards = None;
        assert!(fixes(&rows).is_empty());
        rows[0].wards = Some(0);
        rows[1].position = Some(2);
        // The wardless core game is not a support game, and one core game
        // without a kill is not a habit.
        assert!(fixes(&rows).is_empty());

        for row in rows.iter_mut().take(5) {
            row.position = Some(2);
        }
        assert_eq!(keys(&rows), ["core_no_kills"]);
        assert!(only(&rows).contains('5'));
        rows[0].kills = Some(1);
        rows[1].kills = None;
        rows[2].position = None;
        assert!(fixes(&rows).is_empty());
    }

    #[test]
    fn worst_hero_is_the_lowest_known_win_rate_over_five_games() {
        let mut rows = year(16);
        for (index, row) in rows.iter_mut().enumerate() {
            row.hero_id = Some(if index < 8 { 14 } else { 22 });
            // Hero 14 wins 2 of 8, hero 22 wins 3 of 8.
            row.won = Some(matches!(index, 0 | 1 | 8 | 9 | 10));
        }
        let found = fixes(&rows);
        let hero = found.iter().find(|fix| fix.key == "worst_hero").unwrap();
        assert_eq!(hero.hero_id, Some(14));
        assert!(hero.line.contains("{hero}"));
        assert!(hero.line.contains("25%"));
        // 3 of 7 is over the line, and four known games are not a sample.
        let worst = |rows: &[DistinctMatch]| fixes(rows).iter().any(|fix| fix.key == "worst_hero");
        let mut rows = year(7);
        for (index, row) in rows.iter_mut().enumerate() {
            row.hero_id = Some(14);
            row.won = Some(index < 3);
        }
        assert!(!worst(&rows));
        rows[2].won = Some(false);
        assert!(worst(&rows));
        let mut rows = year(5);
        for row in &mut rows {
            row.hero_id = Some(14);
            row.won = Some(false);
        }
        assert!(worst(&rows));
        rows[0].won = None;
        assert!(!worst(&rows));
        rows[0].won = Some(false);
        rows[1].hero_id = Some(0);
        assert!(!worst(&rows));
    }

    #[test]
    fn bad_side_needs_eight_known_results_at_thirty_five_percent_or_less() {
        let rows = |wins: usize, games: usize| {
            let mut rows = year(games as i64 + 8);
            for (index, row) in rows.iter_mut().enumerate() {
                row.hero_id = Some(index as i64 + 1);
                row.side = Some(if index < games { "dire" } else { "radiant" }.to_owned());
                row.won = Some(index < wins || index >= games);
            }
            rows
        };
        let found = fixes(&rows(2, 8));
        let side = found.iter().find(|fix| fix.key == "bad_side").unwrap();
        assert!(side.line.contains("Dire"));
        assert!(side.line.contains("25%"));
        assert!(fixes(&rows(3, 8)).iter().all(|fix| fix.key != "bad_side"));
        assert!(fixes(&rows(1, 7)).iter().all(|fix| fix.key != "bad_side"));
    }

    #[test]
    fn late_nights_use_the_players_own_clock() {
        let rows = year(5);
        assert!(bug_fixes(&rows, 7, None, 0).is_empty());
        let berlin = bug_fixes(&rows, 7, Some("Europe/Berlin"), 0);
        assert_eq!(berlin.len(), 1);
        assert_eq!(berlin[0].key, "late_nights");
        assert!(berlin[0].known_issue);
        assert!(berlin[0].line.contains('5'));
        assert!(bug_fixes(&rows[..2], 7, Some("Europe/Berlin"), 0).is_empty());
        let mut undated = rows.clone();
        for row in undated.iter_mut().skip(2) {
            row.finished_at = Some(0);
        }
        assert!(bug_fixes(&undated, 7, Some("Europe/Berlin"), 0).is_empty());
    }

    #[test]
    fn deaths_rising_compares_the_halves_of_the_year() {
        let mut rows = year(12);
        for (index, row) in rows.iter_mut().enumerate() {
            row.deaths = Some(if index < 6 { 5 } else { 8 });
        }
        let found = fixes(&rows);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "deaths_rising");
        assert!(found[0].line.contains("5.0") && found[0].line.contains("8.0"));
        for row in rows.iter_mut().skip(6) {
            row.deaths = Some(6);
        }
        assert!(fixes(&rows).is_empty());
        // Falling deaths are not a bug.
        rows.reverse();
        for (index, row) in rows.iter_mut().enumerate() {
            row.deaths = Some(if index < 6 { 5 } else { 8 });
        }
        assert!(fixes(&rows).is_empty());
    }

    #[test]
    fn rapier_losses_read_the_count_or_the_purchase_log() {
        let mut rows = year(6);
        rows[0].rapiers = Some(1);
        assert!(fixes(&rows).is_empty());
        rows[0].won = Some(false);
        assert_eq!(keys(&rows), ["rapier_loss"]);
        assert!(!fixes(&rows)[0].known_issue);
        rows[0].rapiers = Some(-1);
        assert!(fixes(&rows).is_empty());
        rows[0].items = Some(vec![("rapier".to_owned(), 1)]);
        assert_eq!(keys(&rows), ["rapier_loss"]);
        rows[3].rapiers = Some(2);
        rows[3].won = Some(false);
        let found = fixes(&rows);
        assert_eq!(found[0].key, "rapier_losses");
        assert!(found[0].known_issue);
        assert!(found[0].line.contains('2'));
    }

    #[test]
    fn unknown_negative_and_nonfinite_values_are_never_bugs() {
        let mut rows = year(20);
        for row in &mut rows {
            row.won = Some(false);
            row.finished_at = None;
            row.hero_id = None;
            row.deaths = Some(-19);
            row.kills = Some(-22);
            row.throw = Some(f64::NAN);
            row.expected_win = Some(f64::INFINITY);
            row.buybacks = Some(-1);
            row.rapiers = Some(-1);
            row.pings = Some(-80);
            row.duration_seconds = Some(-1);
            row.wards = Some(-1);
            row.gpm = Some(-1);
            row.position = Some(0);
        }
        assert!(fixes(&rows).is_empty());
        for row in &mut rows {
            row.won = None;
            row.deaths = Some(30);
            row.kills = Some(30);
            row.throw = Some(20_000.0);
            row.expected_win = Some(0.9);
            row.buybacks = Some(3);
            row.rapiers = Some(1);
            row.duration_seconds = Some(15 * 60);
        }
        // Without a result, nothing above was a loss.
        assert_eq!(keys(&rows), ["most_deaths"]);
    }

    /// A year with a bug in every theme.
    fn cursed_year() -> Vec<DistinctMatch> {
        let mut rows = year(24);
        for (index, row) in rows.iter_mut().enumerate() {
            row.hero_id = Some(index as i64 % 3 + 1);
            row.side = Some(if index % 2 == 0 { "radiant" } else { "dire" }.to_owned());
            row.won = Some(index % 2 == 1 && index % 3 != 0);
            row.deaths = Some(if index < 12 { 4 } else { 9 });
            row.kills = Some(0);
            row.pings = Some(90);
            row.first_blood = Some(false);
            row.buybacks = Some(1);
            row.position = Some(if index < 12 { 5 } else { 2 });
            row.wards = Some(0);
            row.gpm = Some(180);
        }
        // One game holds the death, kill, throw, odds and length records.
        rows[20].deaths = Some(21);
        rows[20].kills = Some(25);
        rows[20].throw = Some(18_000.0);
        rows[20].expected_win = Some(0.85);
        rows[20].duration_seconds = Some(80 * 60);
        rows[20].rapiers = Some(1);
        rows
    }

    #[test]
    fn the_list_keeps_one_line_per_theme_and_per_game_with_one_known_issue_last() {
        let rows = cursed_year();
        for (zone, seed) in [(None, 0), (Some("Europe/Berlin"), 1), (None, 2)] {
            let found = bug_fixes(&rows, 7, zone, seed);
            assert_eq!(found.len(), MAX_BUG_FIXES);
            let themes = found
                .iter()
                .map(|fix| format!("{:?}", bug(fix.key).theme))
                .collect::<BTreeSet<_>>();
            assert_eq!(themes.len(), MAX_BUG_FIXES);
            // Game 21 has five records; only one of them makes the list.
            let single_game = [
                "most_deaths",
                "kills_in_loss",
                "throw",
                "favorite_loss",
                "longest_game",
                "rapier_loss",
            ];
            assert!(
                found
                    .iter()
                    .filter(|fix| single_game.contains(&fix.key))
                    .count()
                    <= 1
            );
            let issues = found
                .iter()
                .filter(|fix| fix.known_issue)
                .collect::<Vec<_>>();
            assert_eq!(issues.len(), 1);
            assert!(found.last().unwrap().known_issue);
            assert!(!bug(issues[0].key).ongoing.is_empty());
        }
    }

    #[test]
    fn a_habit_takes_the_last_slot_when_the_top_four_are_single_games() {
        let mut rows = year(30);
        rows[0].deaths = Some(30);
        rows[1].kills = Some(40);
        rows[1].won = Some(false);
        rows[2].throw = Some(30_000.0);
        rows[2].won = Some(false);
        rows[3].duration_seconds = Some(120 * 60);
        // A mild habit that would not make the top four on severity.
        for row in &mut rows {
            row.pings = Some(30);
        }
        let found = fixes(&rows);
        assert_eq!(
            found.iter().map(|fix| fix.key).collect::<Vec<_>>(),
            ["longest_game", "throw", "kills_in_loss", "pings"]
        );
        assert!(found[3].known_issue);
        assert!(found[..3].iter().all(|fix| !fix.known_issue));

        // A habit that shares a theme with the top three stays out.
        for row in &mut rows {
            row.pings = None;
            row.first_blood = Some(false);
        }
        let found = fixes(&rows);
        assert_eq!(
            found.iter().map(|fix| fix.key).collect::<Vec<_>>(),
            ["longest_game", "throw", "kills_in_loss", "most_deaths"]
        );
        assert!(found.iter().all(|fix| !fix.known_issue));
    }

    #[test]
    fn hero_placeholders_appear_exactly_when_a_hero_is_named() {
        let mut rows = cursed_year();
        let mut with_hero = 0;
        for seed in 0..64 {
            for fix in bug_fixes(&rows, 7, None, seed) {
                assert_eq!(
                    fix.line.contains("{hero}"),
                    fix.hero_id.is_some(),
                    "{fix:?}"
                );
                with_hero += usize::from(fix.hero_id.is_some());
            }
        }
        assert!(with_hero > 0);
        // With no hero on record, no line asks for one.
        for row in &mut rows {
            row.hero_id = None;
        }
        for seed in 0..64 {
            let found = bug_fixes(&rows, 7, None, seed);
            assert!(!found.is_empty());
            for fix in found {
                assert!(!fix.line.contains('{'), "{fix:?}");
                assert_eq!(fix.hero_id, None);
            }
        }
    }

    #[test]
    fn the_list_is_stable_under_reordering_and_worded_by_seed() {
        let mut rows = cursed_year();
        let found = bug_fixes(&rows, 7, None, 9);
        rows.reverse();
        assert_eq!(bug_fixes(&rows, 7, None, 9), found);
        rows.extend(year(30).into_iter().map(|row| DistinctMatch {
            discord_id: 8,
            deaths: Some(40),
            ..row
        }));
        assert_eq!(bug_fixes(&rows, 7, None, 9), found);
        let wordings = (0..32)
            .map(|seed| format!("{:?}", bug_fixes(&rows, 7, None, seed)))
            .collect::<BTreeSet<_>>();
        assert!(wordings.len() > 1);
        // The seed words the list; it does not choose the bugs.
        for seed in 0..32 {
            let other = bug_fixes(&rows, 7, None, seed);
            assert_eq!(
                other.iter().map(|fix| fix.key).collect::<Vec<_>>(),
                found.iter().map(|fix| fix.key).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn the_catalog_is_varied_in_voice_and_every_wording_is_fifty_characters_or_fewer() {
        assert!(BUGS.len() >= 14);
        assert_eq!(
            BUGS.iter()
                .map(|bug| bug.key)
                .collect::<BTreeSet<_>>()
                .len(),
            BUGS.len()
        );
        let wide = [
            ("hero", "Keeper of the Light"),
            ("n", "1,234"),
            ("k", "123"),
            ("p", "100"),
            ("l", "123"),
            ("z", "123"),
            ("m", "120"),
            ("a", "12.3"),
            ("b", "12.3"),
            ("side", "Radiant"),
        ];
        for bug in BUGS {
            assert!((2..=3).contains(&bug.fixed.len()), "{}", bug.key);
            assert!(bug.ongoing.is_empty() || bug.ongoing.len() >= 2);
            // A line that needs a hero always has a sibling that does not,
            // except for the bug that is about a hero.
            for pool in [bug.fixed, bug.ongoing] {
                assert!(
                    pool.is_empty()
                        || bug.key == "worst_hero"
                        || pool.iter().any(|line| !line.contains("{hero}")),
                    "{}",
                    bug.key
                );
            }
            for template in bug.fixed {
                assert!(
                    template.starts_with("Fixed ")
                        || template.starts_with("Reduced ")
                        || template.contains(" no longer ")
                        || template.contains("working as intended"),
                    "{template}"
                );
            }
            for template in bug.ongoing {
                assert!(!template.starts_with("Fixed "), "{template}");
                assert!(
                    ["still", "Still", "remains", "Not yet fixed"]
                        .iter()
                        .any(|word| template.contains(word)),
                    "{template}"
                );
            }
            for template in bug.fixed.iter().chain(bug.ongoing) {
                // Measured as written, before the hero is named.
                assert!(template.chars().count() <= 50, "{template}");
                let line = format_flavor(template, &wide);
                assert!(!line.contains('{'), "{template}");
                assert!(line.chars().count() <= 80, "{line}");
                assert!(
                    template.contains("{hero}") || line.chars().count() <= 60,
                    "{line}"
                );
                assert!(line.ends_with('.'), "{line}");
                assert!(line.is_ascii(), "{line}");
            }
        }
    }
}
