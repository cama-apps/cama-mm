//! Personal patch notes from recorded matches, without server rankings.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Datelike, Weekday};

use cama_domain::formatting::format_grouped;
use cama_domain::wrapped_distinct::{DistinctMatch, Theme, local_time};

use super::patch_support::{core, counted, finite, minutes, nonnegative, timeline, variant};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PersonalPatch {
    pub awards: Vec<PatchAward>,
    pub changes: Vec<BalanceChange>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchAward {
    pub key: &'static str,
    pub title: String,
    pub value: String,
    pub evidence: String,
    /// The OpenDota key of a purchasable item whose icon suits the award.
    pub icon: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BalanceDirection {
    Buff,
    Nerf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalanceChange {
    pub key: &'static str,
    pub title: String,
    pub value: String,
    pub evidence: String,
    pub direction: BalanceDirection,
    /// Games with a valid measurement in each chronological group.
    pub early_games: usize,
    pub late_games: usize,
}

/// Every award: its key, its icon, and its two titles. `{day}` in a title
/// is the weekday the award was earned on.
pub const AWARDS: [(&str, &str, [&str; 2]); 22] = [
    (
        "buyback_wins",
        "refresher",
        ["Buyback approved", "Money well spent"],
    ),
    (
        "first_blood_wins",
        "dagon",
        ["Started it. Finished it.", "First blood, last laugh"],
    ),
    (
        "roshan_wins",
        "vladmir",
        ["Roshan eviction notice", "Pit inspection passed"],
    ),
    (
        "support_infrastructure",
        "ward_observer",
        ["Support budget increased", "Infrastructure week"],
    ),
    (
        "crowd_control",
        "basher",
        ["Please remain seated", "Movement disabled"],
    ),
    (
        "rune_pace",
        "bottle",
        ["River patrol", "Rune tax collected"],
    ),
    (
        "team_shopping",
        "smoke_of_deceit",
        ["Packed for the team", "Shared inventory"],
    ),
    (
        "both_sides",
        "travel_boots",
        ["Side selection disabled", "Map agnostic"],
    ),
    (
        "late_game",
        "heart",
        ["Late game insurance", "Scales with time"],
    ),
    (
        "closer",
        "phase_boots",
        ["Early checkout", "Done by 30 minutes"],
    ),
    ("underdog", "blade_mail", ["Odds ignored", "Upset special"]),
    (
        "lane_dominance",
        "quelling_blade",
        ["Lane closed for farming", "Creep wave secured"],
    ),
    (
        "always_fighting",
        "blink",
        ["Attendance: mandatory", "Never missed a fight"],
    ),
    (
        "building_damage",
        "desolator",
        ["Structural damage", "Demolition permit"],
    ),
    ("denies", "branches", ["Access denied", "Not your creep"]),
    ("healer", "mekansm", ["Health services", "Fountain on legs"]),
    (
        "clean_hands",
        "ghost",
        ["Clean hands", "Respawn timer unused"],
    ),
    (
        "deathless_wins",
        "aeon_disk",
        ["Zero deaths recorded", "Death timer: disabled"],
    ),
    (
        "comeback_wins",
        "satanic",
        ["Deficit denied", "Comeback mechanics"],
    ),
    (
        "top_net_worth",
        "hand_of_midas",
        ["Top of the scoreboard", "Net worth leader"],
    ),
    ("flex", "power_treads", ["Role queue: all", "Flex pick"]),
    (
        "weekday_specialist",
        "tpscroll",
        ["{day} specialist", "Scheduled for {day}s"],
    ),
];

/// Awards that tell the same kind of story; the first three shown differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum AwardTheme {
    Buyback,
    FirstBlood,
    Objectives,
    Support,
    Runes,
    Shopping,
    Sides,
    Tempo,
    Odds,
    Lane,
    Fighting,
    Survival,
    Wealth,
    Roles,
    Calendar,
}

/// The player's awards and balance changes. `timezone` places games on the
/// player's own weekdays; `seed` picks each award's title.
#[must_use]
pub fn personal_patch(
    matches: &[DistinctMatch],
    player_id: i64,
    timezone: Option<&str>,
    seed: u64,
) -> PersonalPatch {
    let games = matches
        .iter()
        .filter(|game| game.discord_id == player_id)
        .collect::<Vec<_>>();
    PersonalPatch {
        awards: awards(&games, matches, timezone, seed),
        changes: balance_changes(&games),
    }
}

/// A Dota-style name for the player's patch, from the theme of their year;
/// `None` without a theme.
#[must_use]
pub fn patch_name(theme: Option<Theme>, seed: u64) -> Option<&'static str> {
    Some(variant(seed, "patch_name", patch_names(theme?)))
}

fn patch_names(theme: Theme) -> &'static [&'static str] {
    match theme {
        Theme::Heroes => &[
            "The Hero Pool Update",
            "The One Trick Update",
            "The Last Pick Update",
            "The Comfort Pick Update",
        ],
        Theme::Combat => &[
            "The Bloodbath Update",
            "The Team Fight Update",
            "The Respawn Timer Update",
            "The Kill Feed Update",
        ],
        Theme::Farm => &[
            "The Jungle Economy Update",
            "The Last Hit Update",
            "The Net Worth Update",
            "The Free Farm Update",
        ],
        Theme::Map => &[
            "The Vision Update",
            "The Ward Budget Update",
            "The River Patrol Update",
            "The Smoke Break Update",
        ],
        Theme::Chaos => &[
            "The Buyback Update",
            "The Divine Rapier Update",
            "The Ping Wheel Update",
            "The Chaos Theory Update",
        ],
        Theme::Clutch => &[
            "The Comeback Update",
            "The Underdog Update",
            "The Mega Creeps Update",
            "The Base Race Update",
        ],
        Theme::Time => &[
            "The 4 AM Update",
            "The One More Game Update",
            "The Marathon Update",
            "The Late Night Update",
        ],
        Theme::Items => &[
            "The Shopping Spree Update",
            "The Secret Shop Update",
            "The Six Slot Update",
            "The Courier Update",
        ],
    }
}

fn award(seed: u64, key: &'static str, value: String, evidence: String) -> PatchAward {
    let (_, icon, titles) = AWARDS
        .iter()
        .find(|(held, _, _)| *held == key)
        .expect("every award is in the catalog");
    PatchAward {
        key,
        title: variant(seed, key, titles).to_owned(),
        value,
        evidence,
        icon,
    }
}

type Condition = fn(&DistinctMatch) -> Option<bool>;
type Measure = fn(&DistinctMatch) -> Option<f64>;

fn awards(
    games: &[&DistinctMatch],
    matches: &[DistinctMatch],
    timezone: Option<&str>,
    seed: u64,
) -> Vec<PatchAward> {
    let mut candidates: Vec<(AwardTheme, f64, PatchAward)> = Vec::new();

    // Conversion rates describe results, never whether one action caused a
    // win. Each needs `(known, wins, percent)` at least.
    for (key, event, theme, (minimum_known, minimum_wins, minimum_rate), condition) in [
        (
            "buyback_wins",
            "with a buyback",
            AwardTheme::Buyback,
            (3, 2, 60),
            (|game: &DistinctMatch| Some(nonnegative(game.buybacks)? > 0)) as Condition,
        ),
        (
            "first_blood_wins",
            "with your first blood",
            AwardTheme::FirstBlood,
            (3, 3, 60),
            (|game: &DistinctMatch| game.first_blood) as Condition,
        ),
        (
            "roshan_wins",
            "with a Roshan kill",
            AwardTheme::Objectives,
            (3, 3, 60),
            (|game: &DistinctMatch| Some(nonnegative(game.roshans)? > 0)) as Condition,
        ),
        (
            "late_game",
            "lasting 45+ minutes",
            AwardTheme::Tempo,
            (5, 3, 60),
            (|game: &DistinctMatch| Some(minutes(game)? >= 45.0)) as Condition,
        ),
        (
            "closer",
            "under 30 minutes",
            AwardTheme::Tempo,
            (5, 3, 60),
            (|game: &DistinctMatch| Some(minutes(game)? < 30.0)) as Condition,
        ),
        (
            "underdog",
            "at 40% odds or worse",
            AwardTheme::Odds,
            (3, 3, 50),
            (|game: &DistinctMatch| Some(finite(game.expected_win)? <= 0.40)) as Condition,
        ),
    ] {
        let (wins, known) = counted(games, |game| {
            condition(game)?.then_some(())?;
            game.won
        });
        if known >= minimum_known && wins >= minimum_wins && wins * 100 >= known * minimum_rate {
            candidates.push((
                theme,
                1.0 + wins as f64 / known as f64,
                award(
                    seed,
                    key,
                    format!("{wins} wins"),
                    format!("Won {wins} of {known} recorded games {event}."),
                ),
            ));
        }
    }

    // How often a game cleared a bar: at least three of five known games,
    // and `minimum_rate` percent of them. `weight` lifts rare feats.
    let mut tally = |key: &'static str,
                     detail: &str,
                     sample: &str,
                     theme: AwardTheme,
                     (weight, minimum_rate): (f64, usize),
                     (hits, known): (usize, usize)| {
        if known >= 5 && hits >= 3 && hits * 100 >= known * minimum_rate {
            candidates.push((
                theme,
                1.0 + weight * hits as f64 / known as f64,
                award(
                    seed,
                    key,
                    format!("{hits} games"),
                    format!("{detail} in {hits} of {known} {sample}."),
                ),
            ));
        }
    };
    for (key, detail, sample, theme, gate, condition) in [
        (
            "support_infrastructure",
            "10+ wards and 3+ camps stacked",
            "games with stats",
            AwardTheme::Support,
            (1.0, 0),
            (|game: &DistinctMatch| {
                let (wards, stacks) = (nonnegative(game.wards)?, nonnegative(game.camps_stacked)?);
                Some(wards >= 10 && stacks >= 3)
            }) as Condition,
        ),
        (
            "crowd_control",
            "60+ stun seconds and 20+ assists",
            "games with stats",
            AwardTheme::Support,
            (1.0, 0),
            (|game: &DistinctMatch| {
                let (stuns, assists) = (finite(game.stuns)?, nonnegative(game.assists)?);
                Some(stuns >= 60.0 && assists >= 20)
            }) as Condition,
        ),
        (
            "rune_pace",
            "At least 2 rune pickups per 10 minutes",
            "games with stats",
            AwardTheme::Runes,
            (1.0, 0),
            (|game: &DistinctMatch| {
                Some(
                    nonnegative(game.rune_pickups)? as f64 * 300.0
                        >= game.duration_seconds.filter(|duration| *duration > 0)? as f64,
                )
            }) as Condition,
        ),
        (
            "team_shopping",
            "Bought both Smoke of Deceit and Dust",
            "games with stats",
            AwardTheme::Shopping,
            (1.0, 0),
            (|game: &DistinctMatch| {
                let items = game.items.as_ref()?;
                if items.iter().any(|(_, count)| *count < 0) {
                    return None;
                }
                Some(
                    ["smoke_of_deceit", "dust"]
                        .iter()
                        .all(|item| items.iter().any(|(key, count)| key == item && *count > 0)),
                )
            }) as Condition,
        ),
        (
            "lane_dominance",
            "50+ last hits by 10:00",
            "core games with stats",
            AwardTheme::Lane,
            (1.0, 0),
            (|game: &DistinctMatch| {
                core(game)?.then_some(())?;
                Some(nonnegative(game.last_hits_at_10)? >= 50)
            }) as Condition,
        ),
        (
            "always_fighting",
            "Joined 80%+ of team fights",
            "games with stats",
            AwardTheme::Fighting,
            (1.0, 0),
            (|game: &DistinctMatch| {
                let share = finite(game.teamfight_participation).filter(|share| *share <= 1.0)?;
                Some(share >= 0.8)
            }) as Condition,
        ),
        (
            "building_damage",
            "8,000+ building damage",
            "games with stats",
            AwardTheme::Objectives,
            (1.0, 0),
            (|game: &DistinctMatch| Some(nonnegative(game.tower_damage)? >= 8_000)) as Condition,
        ),
        (
            "denies",
            "10+ denies",
            "games with stats",
            AwardTheme::Lane,
            (1.0, 0),
            (|game: &DistinctMatch| Some(nonnegative(game.denies)? >= 10)) as Condition,
        ),
        (
            "healer",
            "5,000+ hero healing",
            "games with stats",
            AwardTheme::Support,
            (1.0, 0),
            (|game: &DistinctMatch| Some(nonnegative(game.hero_healing)? >= 5_000)) as Condition,
        ),
        (
            "clean_hands",
            "3 or fewer deaths",
            "recorded games",
            AwardTheme::Survival,
            (1.0, 40),
            (|game: &DistinctMatch| Some(nonnegative(game.deaths)? <= 3)) as Condition,
        ),
        (
            "deathless_wins",
            "Won without dying",
            "recorded games",
            AwardTheme::Survival,
            (4.0, 0),
            (|game: &DistinctMatch| Some(nonnegative(game.deaths)? == 0 && game.won?)) as Condition,
        ),
        (
            "comeback_wins",
            "Won from 5,000+ gold behind",
            "games with stats",
            AwardTheme::Odds,
            (3.0, 0),
            // A loss is never a comeback; a win is one only when the
            // deficit is known.
            (|game: &DistinctMatch| Some(game.won? && finite(game.comeback)? >= 5_000.0))
                as Condition,
        ),
    ] {
        tally(key, detail, sample, theme, gate, counted(games, condition));
    }

    // The slice holds only registered players' rows, so a game counts when
    // at least eight of its ten net worths are known, and ties are not wins.
    let mut net_worths: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for row in matches {
        if let Some(net_worth) = nonnegative(row.net_worth) {
            net_worths.entry(row.match_id).or_default().push(net_worth);
        }
    }
    tally(
        "top_net_worth",
        "Top net worth among tracked players",
        "games",
        AwardTheme::Wealth,
        (1.5, 0),
        counted(games, |game| {
            let mine = nonnegative(game.net_worth)?;
            let known = net_worths.get(&game.match_id)?;
            (known.len() >= 8).then(|| known.iter().filter(|other| **other >= mine).count() == 1)
        }),
    );

    let side_results = ["radiant", "dire"].map(|side| {
        counted(games, |game| {
            (game.side.as_deref() == Some(side))
                .then_some(game.won)
                .flatten()
        })
    });
    if side_results
        .iter()
        .all(|(wins, known)| *known >= 5 && *wins as f64 / *known as f64 >= 0.6)
    {
        let [(radiant_wins, radiant_games), (dire_wins, dire_games)] = side_results;
        let radiant = 100.0 * radiant_wins as f64 / radiant_games as f64;
        let dire = 100.0 * dire_wins as f64 / dire_games as f64;
        candidates.push((
            AwardTheme::Sides,
            1.0 + radiant.min(dire) / 100.0,
            award(
                seed,
                "both_sides",
                format!("{radiant:.0}% / {dire:.0}%"),
                format!(
                    "Won {radiant_wins}/{radiant_games} on Radiant and {dire_wins}/{dire_games} on Dire."
                ),
            ),
        ));
    }

    let mut positions: BTreeMap<i64, usize> = BTreeMap::new();
    for position in games
        .iter()
        .filter_map(|game| game.position.filter(|position| (1..=5).contains(position)))
    {
        *positions.entry(position).or_default() += 1;
    }
    let played = positions
        .into_iter()
        .filter(|(_, count)| *count >= 3)
        .map(|(position, _)| position.to_string())
        .collect::<Vec<_>>();
    if let [rest @ .., last] = &played[..]
        && played.len() >= 3
    {
        candidates.push((
            AwardTheme::Roles,
            1.0 + played.len() as f64 / 5.0,
            award(
                seed,
                "flex",
                format!("{} roles", played.len()),
                format!(
                    "3+ games on each of positions {} and {last}.",
                    rest.join(", ")
                ),
            ),
        ));
    }

    // The best weekday on the player's own clock: at least 65% over five or
    // more known results. The earlier weekday keeps a full tie.
    let mut weekdays: BTreeMap<u32, (Weekday, usize, usize)> = BTreeMap::new();
    for game in games {
        let finished = game.finished_at.filter(|time| *time > 0);
        if let (Some(won), Some(day)) = (
            game.won,
            finished
                .and_then(|time| local_time(time, timezone))
                .map(|time| time.weekday()),
        ) {
            let record = weekdays
                .entry(day.num_days_from_monday())
                .or_insert((day, 0, 0));
            record.1 += usize::from(won);
            record.2 += 1;
        }
    }
    let best_day = weekdays
        .into_values()
        .filter(|(_, wins, known)| *known >= 5 && wins * 100 >= known * 65)
        .reduce(|best, next| {
            let (held, new) = (best.1 * next.2, next.1 * best.2);
            if new > held || (new == held && next.2 > best.2) {
                next
            } else {
                best
            }
        });
    if let Some((day, wins, known)) = best_day {
        let day = match day {
            Weekday::Mon => "Monday",
            Weekday::Tue => "Tuesday",
            Weekday::Wed => "Wednesday",
            Weekday::Thu => "Thursday",
            Weekday::Fri => "Friday",
            Weekday::Sat => "Saturday",
            Weekday::Sun => "Sunday",
        };
        let mut specialist = award(
            seed,
            "weekday_specialist",
            format!("{wins} wins"),
            format!("Won {wins} of {known} recorded games that ended on a {day}."),
        );
        specialist.title = specialist.title.replace("{day}", day);
        candidates.push((
            AwardTheme::Calendar,
            1.0 + wins as f64 / known as f64,
            specialist,
        ));
    }

    candidates.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.2.key.cmp(b.2.key)));
    let mut themes = BTreeSet::new();
    let mut selected = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, (theme, _, _))| themes.insert(*theme).then_some(index))
        .take(3)
        .collect::<Vec<_>>();
    for index in 0..candidates.len() {
        if selected.len() >= 3 {
            break;
        }
        if !selected.contains(&index) {
            selected.push(index);
        }
    }
    selected
        .into_iter()
        .map(|index| candidates[index].2.clone())
        .collect()
}

fn balance_changes(games: &[&DistinctMatch]) -> Vec<BalanceChange> {
    // One timeline for every metric: the same early and late halves.
    let Some(split) = timeline(games) else {
        return Vec::new();
    };
    let group_size = split.early.len();
    let grouped = |value: f64| format_grouped(value.round() as i64);
    let mut changes = Vec::new();
    for (key, title, threshold, lower_is_better, measure) in [
        (
            "gpm",
            "Gold income",
            40.0,
            false,
            (|game: &DistinctMatch| nonnegative(game.gpm).map(|value| value as f64)) as Measure,
        ),
        (
            "deaths",
            "Deaths",
            1.0,
            true,
            (|game: &DistinctMatch| nonnegative(game.deaths).map(|value| value as f64)) as Measure,
        ),
        (
            "win_rate",
            "Win rate",
            15.0,
            false,
            (|game: &DistinctMatch| game.won.map(|won| if won { 100.0 } else { 0.0 })) as Measure,
        ),
        (
            "wards",
            "Wards placed",
            3.0,
            false,
            (|game: &DistinctMatch| nonnegative(game.wards).map(|value| value as f64)) as Measure,
        ),
        (
            "stuns",
            "Stun duration",
            10.0,
            false,
            (|game: &DistinctMatch| finite(game.stuns)) as Measure,
        ),
        (
            "kills",
            "Kills",
            2.0,
            false,
            (|game: &DistinctMatch| nonnegative(game.kills).map(|value| value as f64)) as Measure,
        ),
        (
            "assists",
            "Assists",
            3.0,
            false,
            (|game: &DistinctMatch| nonnegative(game.assists).map(|value| value as f64)) as Measure,
        ),
        (
            "last_hits_at_10",
            "Lane last hits",
            8.0,
            false,
            (|game: &DistinctMatch| nonnegative(game.last_hits_at_10).map(|value| value as f64))
                as Measure,
        ),
        (
            "teamfight",
            "Team fights",
            8.0,
            false,
            // Percent of the team's fights joined.
            (|game: &DistinctMatch| {
                finite(game.teamfight_participation)
                    .filter(|share| *share <= 1.0)
                    .map(|share| share * 100.0)
            }) as Measure,
        ),
        (
            "hero_damage",
            "Hero damage",
            4_000.0,
            false,
            (|game: &DistinctMatch| nonnegative(game.hero_damage).map(|value| value as f64))
                as Measure,
        ),
        (
            "game_length",
            "Game length",
            4.0,
            // Longer games are filed as the nerf.
            true,
            minutes as Measure,
        ),
        (
            "tower_damage",
            "Building damage",
            1_500.0,
            false,
            (|game: &DistinctMatch| nonnegative(game.tower_damage).map(|value| value as f64))
                as Measure,
        ),
    ] {
        let averages = [&split.early, &split.late].map(|group| {
            let values = group
                .iter()
                .filter_map(|game| measure(game))
                .collect::<Vec<_>>();
            let count = values.len();
            // Divide before summing so even very large finite inputs cannot
            // overflow just because several observations were present.
            let average = values.iter().map(|value| value / count as f64).sum::<f64>();
            (average, count)
        });
        let [(before, early_games), (after, late_games)] = averages;
        if early_games < 5 || late_games < 5 || !before.is_finite() || !after.is_finite() {
            continue;
        }
        let delta = after - before;
        if delta.abs() < threshold {
            continue;
        }
        let value = match key {
            "gpm" => format!("{before:.0} to {after:.0} GPM"),
            "win_rate" => format!("{before:.0}% to {after:.0}%"),
            "stuns" => format!("{before:.1} to {after:.1} stun seconds/game"),
            "deaths" => format!("{before:.1} to {after:.1} deaths/game"),
            "wards" => format!("{before:.1} to {after:.1} wards/game"),
            "kills" => format!("{before:.1} to {after:.1} kills/game"),
            "assists" => format!("{before:.1} to {after:.1} assists/game"),
            "last_hits_at_10" => format!("{before:.0} to {after:.0} last hits at 10:00"),
            "teamfight" => format!("{before:.0}% to {after:.0}% of team fights"),
            "hero_damage" => format!("{} to {} hero damage", grouped(before), grouped(after)),
            "game_length" => format!("{before:.1} to {after:.1} minutes/game"),
            _ => format!("{} to {} building damage", grouped(before), grouped(after)),
        };
        changes.push((delta.abs() / threshold, BalanceChange {
            key,
            title: title.to_owned(),
            value,
            evidence: if early_games == group_size && late_games == group_size {
                format!("First {group_size} vs last {group_size} dated games.")
            } else {
                format!("First {group_size} vs last {group_size} dated games ({early_games}/{group_size} vs {late_games}/{group_size} with stats).")
            },
            direction: if (delta < 0.0) == lower_is_better { BalanceDirection::Buff } else { BalanceDirection::Nerf },
            early_games,
            late_games,
        }));
    }
    changes.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.key.cmp(b.1.key)));
    changes
        .into_iter()
        .take(3)
        .map(|(_, change)| change)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn games(count: i64) -> Vec<DistinctMatch> {
        (1..=count)
            .map(|id| DistinctMatch {
                match_id: id,
                discord_id: 7,
                finished_at: Some(id * 86_400),
                ..Default::default()
            })
            .collect()
    }

    fn patch(rows: &[DistinctMatch]) -> PersonalPatch {
        personal_patch(rows, 7, None, 0)
    }

    #[test]
    fn buyback_award_counts_known_outcomes_without_claiming_causation() {
        let mut rows = games(6);
        for row in &mut rows {
            row.buybacks = Some(1);
            row.won = Some(true);
        }
        rows[2].won = Some(false);
        rows[3].won = None;
        rows[4].buybacks = Some(-1);
        rows[5].discord_id = 8;
        let report = patch(&rows);
        assert_eq!(report.awards.len(), 1);
        assert_eq!(report.awards[0].key, "buyback_wins");
        assert_eq!(report.awards[0].value, "2 wins");
        assert!(report.awards[0].evidence.contains("2 of 3"));
        assert!(report.awards[0].evidence.contains("with a buyback"));
    }

    #[test]
    fn support_award_requires_wards_and_stacks_in_the_same_games() {
        let mut rows = games(6);
        for (index, row) in rows.iter_mut().enumerate() {
            row.wards = Some(if index < 3 { 12 } else { 0 });
            row.camps_stacked = Some(if index < 3 { 0 } else { 4 });
        }
        assert!(patch(&rows).awards.is_empty());
        for row in &mut rows[..3] {
            row.camps_stacked = Some(4);
        }
        let report = patch(&rows);
        assert_eq!(report.awards[0].key, "support_infrastructure");
        assert!(report.awards[0].evidence.contains("3 of 6"));
    }

    #[test]
    fn combined_awards_exclude_missing_data_even_when_one_threshold_is_missed() {
        let mut rows = games(5);
        for row in &mut rows[..3] {
            row.wards = Some(12);
            row.camps_stacked = Some(4);
            row.stuns = Some(90.0);
            row.assists = Some(25);
        }
        rows[3].wards = Some(0);
        rows[3].camps_stacked = Some(0);
        rows[3].stuns = Some(0.0);
        rows[3].assists = Some(0);
        rows[4].wards = Some(0);
        rows[4].stuns = Some(0.0);
        assert!(patch(&rows).awards.is_empty());
    }

    #[test]
    fn first_blood_and_roshan_awards_require_wins_not_only_participation() {
        let mut rows = games(5);
        for row in &mut rows {
            row.first_blood = Some(true);
            row.roshans = Some(1);
            row.won = Some(false);
        }
        assert!(patch(&rows).awards.is_empty());
        for row in &mut rows[..3] {
            row.won = Some(true);
        }
        let report = patch(&rows);
        assert_eq!(report.awards.len(), 2);
        assert!(report.awards.iter().all(|award| award.value == "3 wins"));
    }

    #[test]
    fn purchase_award_distinguishes_empty_logs_from_unknown_and_rejects_negative_counts() {
        let mut rows = games(8);
        for row in &mut rows[..3] {
            row.items = Some(vec![("smoke_of_deceit".into(), 1), ("dust".into(), 2)]);
        }
        rows[3].items = Some(Vec::new());
        rows[4].items = Some(Vec::new());
        rows[5].items = Some(vec![("smoke_of_deceit".into(), -1), ("dust".into(), 1)]);
        let report = patch(&rows);
        assert_eq!(report.awards[0].key, "team_shopping");
        assert!(report.awards[0].evidence.contains("3 of 5"));
        rows[4].items = None;
        assert!(patch(&rows).awards.is_empty());
    }

    #[test]
    fn rune_pace_rejects_missing_zero_and_negative_durations() {
        let mut rows = games(5);
        for row in &mut rows {
            row.duration_seconds = Some(1800);
            row.rune_pickups = Some(6);
        }
        let report = patch(&rows);
        assert_eq!(report.awards[0].key, "rune_pace");
        for invalid in [None, Some(0), Some(-1)] {
            rows[0].duration_seconds = invalid;
            assert!(patch(&rows).awards.is_empty());
        }
    }

    #[test]
    fn crowd_control_needs_both_stuns_and_assists_and_known_zero_is_valid() {
        let mut rows = games(5);
        for row in &mut rows {
            row.stuns = Some(90.0);
            row.assists = Some(25);
        }
        rows[3].stuns = Some(0.0);
        rows[4].assists = Some(0);
        let report = patch(&rows);
        assert_eq!(report.awards[0].key, "crowd_control");
        assert!(report.awards[0].evidence.contains("3 of 5"));
        rows[3].stuns = Some(f64::NAN);
        assert!(patch(&rows).awards.is_empty());
    }

    #[test]
    fn win_rate_decreases_are_nerfs_and_flat_measurements_are_omitted() {
        let mut rows = games(10);
        for (index, row) in rows.iter_mut().enumerate() {
            row.won = Some(index < 5);
            row.gpm = Some(500);
        }
        let report = patch(&rows);
        assert_eq!(report.changes.len(), 1);
        assert_eq!(report.changes[0].key, "win_rate");
        assert_eq!(report.changes[0].value, "100% to 0%");
        assert_eq!(report.changes[0].direction, BalanceDirection::Nerf);
    }

    #[test]
    fn both_sides_award_needs_five_known_results_per_side() {
        let mut rows = games(10);
        for (index, row) in rows.iter_mut().enumerate() {
            row.side = Some(if index < 5 { "radiant" } else { "dire" }.into());
            row.won = Some(index % 5 < 3);
        }
        let report = patch(&rows);
        assert_eq!(report.awards[0].key, "both_sides");
        assert!(report.awards[0].value.contains("60%"));
        rows[9].won = None;
        assert!(patch(&rows).awards.is_empty());
    }

    #[test]
    fn changes_sort_by_time_and_treat_fewer_deaths_as_a_buff() {
        let mut rows = games(11);
        for (index, row) in rows.iter_mut().enumerate() {
            row.gpm = Some(if index < 5 { 400 } else { 500 });
            row.deaths = Some(if index < 5 { 8 } else { 4 });
        }
        rows[5].gpm = Some(9000);
        rows[5].deaths = Some(90);
        rows.reverse();
        let report = patch(&rows);
        assert_eq!(report.changes.len(), 2);
        let gpm = report
            .changes
            .iter()
            .find(|change| change.key == "gpm")
            .unwrap();
        assert_eq!(gpm.value, "400 to 500 GPM");
        assert_eq!((gpm.early_games, gpm.late_games), (5, 5));
        assert_eq!(gpm.direction, BalanceDirection::Buff);
        let deaths = report
            .changes
            .iter()
            .find(|change| change.key == "deaths")
            .unwrap();
        assert_eq!(deaths.value, "8.0 to 4.0 deaths/game");
        assert_eq!(deaths.direction, BalanceDirection::Buff);
        assert!(deaths.evidence.contains("First 5 vs last 5"));
    }

    #[test]
    fn changes_keep_one_timeline_and_report_incomplete_metric_samples() {
        let mut rows = games(12);
        for (index, row) in rows.iter_mut().enumerate() {
            row.gpm = Some(if index < 6 { 400 } else { 500 });
        }
        rows[0].gpm = None;
        rows[11].gpm = Some(-1);
        let report = patch(&rows);
        assert_eq!(report.changes.len(), 1);
        assert_eq!(report.changes[0].value, "400 to 500 GPM");
        assert_eq!(
            (report.changes[0].early_games, report.changes[0].late_games),
            (5, 5)
        );
        assert!(report.changes[0].evidence.contains("First 6 vs last 6"));
        assert!(report.changes[0].evidence.contains("5/6"));
        rows[1].gpm = None;
        assert!(patch(&rows).changes.is_empty());
    }

    #[test]
    fn insufficient_or_ambiguous_chronology_never_creates_a_change() {
        let mut rows = games(10);
        for (index, row) in rows.iter_mut().enumerate() {
            row.gpm = Some(if index < 5 { 400 } else { 600 });
        }
        assert_eq!(patch(&rows).changes.len(), 1);
        for invalid in [None, Some(0), Some(-1)] {
            rows[0].finished_at = invalid;
            assert!(patch(&rows).changes.is_empty());
        }
        for row in &mut rows {
            row.finished_at = Some(1);
        }
        assert!(patch(&rows).changes.is_empty());
    }

    #[test]
    fn unknown_negative_and_nonfinite_data_do_not_earn_awards_or_changes() {
        let mut rows = games(20);
        for row in &mut rows {
            row.gpm = Some(-1);
            row.deaths = Some(-1);
            row.wards = Some(-1);
            row.camps_stacked = Some(-1);
            row.stuns = Some(f64::NAN);
            row.assists = Some(-1);
            row.rune_pickups = Some(-1);
            row.duration_seconds = Some(-1);
            row.buybacks = Some(-1);
            row.roshans = Some(-1);
        }
        assert_eq!(patch(&rows), PersonalPatch::default());
        rows[0].stuns = Some(f64::INFINITY);
        assert_eq!(patch(&rows), PersonalPatch::default());
    }

    #[test]
    fn awards_and_changes_are_bounded_varied_and_stable_under_reordering() {
        let mut rows = games(12);
        for (index, row) in rows.iter_mut().enumerate() {
            row.won = Some(true);
            row.first_blood = Some(true);
            row.roshans = Some(1);
            row.buybacks = Some(1);
            row.wards = Some(if index < 6 { 12 } else { 20 });
            row.camps_stacked = Some(4);
            row.stuns = Some(if index < 6 { 90.0 } else { 120.0 });
            row.assists = Some(25);
            row.gpm = Some(if index < 6 { 400 } else { 600 });
            row.deaths = Some(if index < 6 { 8 } else { 4 });
        }
        let report = patch(&rows);
        assert_eq!(report.awards.len(), 3);
        assert_eq!(report.changes.len(), 3);
        assert!(
            !report
                .awards
                .iter()
                .any(|award| award.key == "support_infrastructure")
                || !report
                    .awards
                    .iter()
                    .any(|award| award.key == "crowd_control")
        );
        rows.reverse();
        assert_eq!(patch(&rows), report);
        assert!(report.awards.iter().all(|award| !award.evidence.is_empty()));
    }

    fn award<'a>(report: &'a PersonalPatch, key: &str) -> Option<&'a PatchAward> {
        report.awards.iter().find(|award| award.key == key)
    }

    #[test]
    fn every_award_has_an_item_icon_and_two_short_titles() {
        assert!(AWARDS.len() >= 20);
        let keys = AWARDS
            .iter()
            .map(|(key, _, _)| *key)
            .collect::<BTreeSet<_>>();
        assert_eq!(keys.len(), AWARDS.len());
        for (key, icon, titles) in AWARDS {
            assert!(
                !icon.is_empty()
                    && icon
                        .chars()
                        .all(|letter| letter.is_ascii_lowercase() || letter == '_'),
                "{key}: {icon}"
            );
            // Not purchasable, so they have no shop icon.
            assert!(!["aegis", "cheese"].contains(&icon), "{key}");
            assert_ne!(titles[0], titles[1], "{key}");
            for title in titles {
                let widest = title.replace("{day}", "Wednesday");
                assert!(widest.chars().count() <= 26, "{widest}");
                assert!(!widest.contains('{'), "{title}");
            }
        }
    }

    #[test]
    fn award_titles_follow_the_seed_and_evidence_does_not() {
        let mut rows = games(6);
        for row in &mut rows {
            row.buybacks = Some(1);
            row.won = Some(true);
        }
        let reports = (0..16)
            .map(|seed| personal_patch(&rows, 7, None, seed))
            .collect::<Vec<_>>();
        let titles = reports
            .iter()
            .map(|report| report.awards[0].title.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            titles,
            BTreeSet::from(["Buyback approved", "Money well spent"])
        );
        for report in &reports {
            assert_eq!(report.awards[0].icon, "refresher");
            assert_eq!(report.awards[0].evidence, reports[0].awards[0].evidence);
            assert_eq!(report.awards[0].value, reports[0].awards[0].value);
        }
        assert_eq!(personal_patch(&rows, 7, None, 3), reports[3]);
    }

    #[test]
    fn tempo_awards_need_five_known_games_and_a_winning_record() {
        let mut rows = games(10);
        for (index, row) in rows.iter_mut().enumerate() {
            row.duration_seconds = Some(if index < 5 { 50 * 60 } else { 25 * 60 });
            row.won = Some(index % 5 != 0);
        }
        let report = patch(&rows);
        assert_eq!(
            award(&report, "late_game").unwrap().evidence,
            "Won 4 of 5 recorded games lasting 45+ minutes."
        );
        let closer = award(&report, "closer").unwrap();
        assert_eq!(
            closer.evidence,
            "Won 4 of 5 recorded games under 30 minutes."
        );
        assert_eq!(
            (closer.value.as_str(), closer.icon),
            ("4 wins", "phase_boots")
        );

        // 2 of 5 is a losing record, and four games are not a sample.
        for row in rows.iter_mut().take(3) {
            row.won = Some(false);
        }
        rows[9].duration_seconds = Some(0);
        assert!(patch(&rows).awards.is_empty());
        // A 30 to 45 minute game is neither.
        for row in &mut rows {
            row.duration_seconds = Some(40 * 60);
            row.won = Some(true);
        }
        assert!(patch(&rows).awards.is_empty());
    }

    #[test]
    fn underdog_award_needs_three_wins_and_half_of_the_long_shots() {
        let mut rows = games(8);
        for (index, row) in rows.iter_mut().enumerate() {
            row.expected_win = Some(if index < 6 { 0.38 } else { 0.41 });
            row.won = Some(!(3..6).contains(&index));
        }
        let report = patch(&rows);
        assert_eq!(report.awards.len(), 1);
        assert_eq!(
            report.awards[0].evidence,
            "Won 3 of 6 recorded games at 40% odds or worse."
        );
        rows[0].expected_win = Some(f64::NAN);
        assert!(patch(&rows).awards.is_empty());
        rows[0].expected_win = Some(0.2);
        rows[0].won = Some(false);
        assert!(patch(&rows).awards.is_empty());
    }

    #[test]
    fn stat_line_awards_count_games_over_the_bar_among_games_with_the_stat() {
        type Set = fn(&mut DistinctMatch, bool);
        let cases: [(&str, &str, Set); 6] = [
            (
                "lane_dominance",
                "50+ last hits by 10:00 in 3 of 5 core games with stats.",
                |row, hit| {
                    row.position = Some(1);
                    row.last_hits_at_10 = Some(if hit { 50 } else { 49 });
                },
            ),
            (
                "always_fighting",
                "Joined 80%+ of team fights in 3 of 5 games with stats.",
                |row, hit| row.teamfight_participation = Some(if hit { 0.8 } else { 0.79 }),
            ),
            (
                "building_damage",
                "8,000+ building damage in 3 of 5 games with stats.",
                |row, hit| row.tower_damage = Some(if hit { 8_000 } else { 7_999 }),
            ),
            (
                "denies",
                "10+ denies in 3 of 5 games with stats.",
                |row, hit| row.denies = Some(if hit { 10 } else { 9 }),
            ),
            (
                "healer",
                "5,000+ hero healing in 3 of 5 games with stats.",
                |row, hit| row.hero_healing = Some(if hit { 5_000 } else { 4_999 }),
            ),
            (
                "clean_hands",
                "3 or fewer deaths in 3 of 5 recorded games.",
                |row, hit| row.deaths = Some(if hit { 3 } else { 4 }),
            ),
        ];
        for (key, evidence, set) in cases {
            let mut rows = games(7);
            for (index, row) in rows.iter_mut().take(5).enumerate() {
                set(row, index < 3);
            }
            let report = patch(&rows);
            assert_eq!(report.awards.len(), 1, "{key}");
            let earned = &report.awards[0];
            assert_eq!((earned.key, earned.evidence.as_str()), (key, evidence));
            assert_eq!(earned.value, "3 games");
            // Two games over the bar are not an award.
            set(&mut rows[0], false);
            assert!(patch(&rows).awards.is_empty(), "{key}");
            // Nor are three of four known games.
            set(&mut rows[0], true);
            rows.remove(4);
            assert!(patch(&rows).awards.is_empty(), "{key}");
        }
    }

    #[test]
    fn stat_line_awards_ignore_supports_bad_shares_and_negative_values() {
        let mut rows = games(6);
        for row in &mut rows {
            row.position = Some(4);
            row.last_hits_at_10 = Some(80);
            row.teamfight_participation = Some(1.2);
            row.tower_damage = Some(-1);
            row.denies = Some(-1);
            row.hero_healing = Some(-1);
            row.deaths = Some(-1);
            row.net_worth = Some(-1);
        }
        assert!(patch(&rows).awards.is_empty());
        for row in &mut rows {
            row.position = None;
            row.teamfight_participation = Some(f64::NAN);
        }
        assert!(patch(&rows).awards.is_empty());
    }

    #[test]
    fn clean_hands_needs_forty_percent_and_deathless_wins_need_three() {
        let mut rows = games(8);
        for (index, row) in rows.iter_mut().enumerate() {
            row.deaths = Some(if index < 3 { 0 } else { 7 });
            row.won = Some(true);
        }
        // 3 of 8 games under four deaths is not a habit, but three of them
        // were deathless wins.
        let report = patch(&rows);
        assert_eq!(report.awards.len(), 1);
        assert_eq!(
            report.awards[0].evidence,
            "Won without dying in 3 of 8 recorded games."
        );
        assert_eq!(report.awards[0].icon, "aeon_disk");
        // A deathless loss is not a deathless win, and an unknown result is
        // not a game either way.
        rows[0].won = Some(false);
        assert!(patch(&rows).awards.is_empty());
        rows[0].won = None;
        assert!(patch(&rows).awards.is_empty());
    }

    #[test]
    fn comeback_wins_count_losses_but_not_wins_with_an_unknown_deficit() {
        let mut rows = games(7);
        for (index, row) in rows.iter_mut().enumerate() {
            row.won = Some(index != 6);
            row.comeback = match index {
                0..3 => Some(6_000.0),
                3..5 => Some(4_999.0),
                _ => None,
            };
        }
        let report = patch(&rows);
        assert_eq!(report.awards.len(), 1);
        assert_eq!(
            report.awards[0].evidence,
            "Won from 5,000+ gold behind in 3 of 6 games with stats."
        );
        rows[0].comeback = Some(f64::INFINITY);
        assert!(patch(&rows).awards.is_empty());
    }

    #[test]
    fn top_net_worth_needs_eight_known_rows_and_no_tie() {
        // Five matches of eight tracked players; player 7 is richest in the
        // first `tops` of them.
        let lobby = |tops: i64| {
            (1..=5_i64)
                .flat_map(|id| {
                    (0..8_i64).map(move |seat| DistinctMatch {
                        match_id: id,
                        discord_id: 7 + seat,
                        finished_at: Some(id * 86_400),
                        net_worth: Some(match seat {
                            0 if id <= tops => 30_000,
                            0 => 9_000,
                            _ => 10_000 + seat,
                        }),
                        ..Default::default()
                    })
                })
                .collect::<Vec<_>>()
        };
        let report = patch(&lobby(3));
        assert_eq!(report.awards.len(), 1);
        let earned = &report.awards[0];
        assert_eq!(
            (earned.key, earned.icon),
            ("top_net_worth", "hand_of_midas")
        );
        assert_eq!(
            earned.evidence,
            "Top net worth among tracked players in 3 of 5 games."
        );
        assert!(patch(&lobby(2)).awards.is_empty());

        // Sharing the top spot is not holding it.
        let mut tied = lobby(3);
        tied[1].net_worth = Some(30_000);
        assert!(patch(&tied).awards.is_empty());
        // With a seventh net worth unknown, the game cannot be judged.
        let mut thin = lobby(3);
        thin[1].net_worth = None;
        thin[2].net_worth = Some(-5);
        assert!(patch(&thin).awards.is_empty());
    }

    #[test]
    fn flex_needs_three_games_on_each_of_three_positions() {
        let rows = |positions: [i64; 10]| {
            let mut rows = games(10);
            for (row, position) in rows.iter_mut().zip(positions) {
                row.position = Some(position);
            }
            rows
        };
        let report = patch(&rows([1, 1, 1, 3, 3, 3, 5, 5, 5, 2]));
        assert_eq!(report.awards.len(), 1);
        let earned = &report.awards[0];
        assert_eq!((earned.key, earned.value.as_str()), ("flex", "3 roles"));
        assert_eq!(earned.evidence, "3+ games on each of positions 1, 3 and 5.");
        assert!(
            patch(&rows([1, 1, 1, 3, 3, 3, 5, 5, 2, 2]))
                .awards
                .is_empty()
        );
        // Positions outside 1 to 5 are unknown, not a sixth role.
        assert!(
            patch(&rows([1, 1, 1, 3, 3, 3, 9, 9, 9, 0]))
                .awards
                .is_empty()
        );
    }

    #[test]
    fn weekday_specialist_uses_the_players_clock() {
        // 2026-01-03 02:00 UTC: Friday evening in New York, the default
        // zone, and Saturday morning in Berlin. Then weekly.
        let rows = |wins: usize| {
            (0..5_i64)
                .map(|week| DistinctMatch {
                    match_id: week + 1,
                    discord_id: 7,
                    finished_at: Some(1_767_405_600 + week * 7 * 86_400),
                    won: Some((week as usize) < wins),
                    ..Default::default()
                })
                .collect::<Vec<_>>()
        };
        let home = personal_patch(&rows(4), 7, None, 0);
        assert_eq!(home.awards.len(), 1);
        assert_eq!(home.awards[0].key, "weekday_specialist");
        assert!(home.awards[0].title.contains("Friday"));
        assert_eq!(
            home.awards[0].evidence,
            "Won 4 of 5 recorded games that ended on a Friday."
        );
        let away = personal_patch(&rows(4), 7, Some("Europe/Berlin"), 0);
        assert!(away.awards[0].title.contains("Saturday"));
        assert!(away.awards[0].evidence.ends_with("on a Saturday."));
        // 60% is a good day, not a specialty; four games are not a sample.
        assert!(patch(&rows(3)).awards.is_empty());
        assert!(patch(&rows(4)[..4]).awards.is_empty());
        let mut undated = rows(4);
        undated[4].finished_at = Some(0);
        assert!(patch(&undated).awards.is_empty());
    }

    #[test]
    fn new_metrics_report_real_changes_and_ignore_small_ones() {
        type Set = fn(&mut DistinctMatch, bool);
        let cases: [(&str, &str, BalanceDirection, Set, Set); 7] = [
            (
                "kills",
                "4.0 to 6.0 kills/game",
                BalanceDirection::Buff,
                |row, late| row.kills = Some(if late { 6 } else { 4 }),
                |row, late| row.kills = Some(if late { 5 } else { 4 }),
            ),
            (
                "assists",
                "10.0 to 13.0 assists/game",
                BalanceDirection::Buff,
                |row, late| row.assists = Some(if late { 13 } else { 10 }),
                |row, late| row.assists = Some(if late { 12 } else { 10 }),
            ),
            (
                "last_hits_at_10",
                "40 to 48 last hits at 10:00",
                BalanceDirection::Buff,
                |row, late| row.last_hits_at_10 = Some(if late { 48 } else { 40 }),
                |row, late| row.last_hits_at_10 = Some(if late { 47 } else { 40 }),
            ),
            (
                "teamfight",
                "50% to 60% of team fights",
                BalanceDirection::Buff,
                |row, late| row.teamfight_participation = Some(if late { 0.6 } else { 0.5 }),
                |row, late| row.teamfight_participation = Some(if late { 0.55 } else { 0.5 }),
            ),
            (
                "hero_damage",
                "23,000 to 18,000 hero damage",
                BalanceDirection::Nerf,
                |row, late| row.hero_damage = Some(if late { 18_000 } else { 23_000 }),
                |row, late| row.hero_damage = Some(if late { 20_000 } else { 23_000 }),
            ),
            (
                "game_length",
                "40.0 to 45.0 minutes/game",
                BalanceDirection::Nerf,
                |row, late| row.duration_seconds = Some(if late { 2_700 } else { 2_400 }),
                |row, late| row.duration_seconds = Some(if late { 2_600 } else { 2_400 }),
            ),
            (
                "tower_damage",
                "2,000 to 4,000 building damage",
                BalanceDirection::Buff,
                |row, late| row.tower_damage = Some(if late { 4_000 } else { 2_000 }),
                |row, late| row.tower_damage = Some(if late { 3_000 } else { 2_000 }),
            ),
        ];
        for (key, value, direction, real, small) in cases {
            let mut rows = games(10);
            for (index, row) in rows.iter_mut().enumerate() {
                real(row, index >= 5);
            }
            let report = patch(&rows);
            assert_eq!(report.changes.len(), 1, "{key}");
            let change = &report.changes[0];
            assert_eq!(
                (change.key, change.value.as_str(), change.direction),
                (key, value, direction)
            );
            assert!(!change.title.is_empty() && change.title.len() <= 15);
            for (index, row) in rows.iter_mut().enumerate() {
                small(row, index >= 5);
            }
            assert!(patch(&rows).changes.is_empty(), "{key}");
        }
    }

    #[test]
    fn new_metrics_drop_unknown_values_and_still_cap_at_three() {
        let mut rows = games(10);
        for (index, row) in rows.iter_mut().enumerate() {
            let late = index >= 5;
            row.kills = Some(if late { 12 } else { 4 });
            row.assists = Some(if late { 22 } else { 10 });
            row.last_hits_at_10 = Some(if late { 70 } else { 40 });
            row.teamfight_participation = Some(if late { 0.9 } else { 0.5 });
            row.hero_damage = Some(if late { 40_000 } else { 18_000 });
            row.duration_seconds = Some(if late { 3_600 } else { 2_400 });
            row.tower_damage = Some(if late { 9_000 } else { 2_000 });
        }
        let report = patch(&rows);
        assert_eq!(
            report
                .changes
                .iter()
                .map(|change| change.key)
                .collect::<Vec<_>>(),
            ["hero_damage", "game_length", "teamfight"]
        );
        for row in &mut rows {
            row.kills = Some(-1);
            row.assists = None;
            row.last_hits_at_10 = Some(-1);
            row.teamfight_participation = Some(1.5);
            row.hero_damage = Some(-1);
            row.duration_seconds = Some(0);
            row.tower_damage = None;
        }
        assert!(patch(&rows).changes.is_empty());
    }

    #[test]
    fn patch_names_come_from_the_theme_and_the_seed() {
        assert_eq!(patch_name(None, 4), None);
        for theme in [
            Theme::Heroes,
            Theme::Combat,
            Theme::Farm,
            Theme::Map,
            Theme::Chaos,
            Theme::Clutch,
            Theme::Time,
            Theme::Items,
        ] {
            let pool = patch_names(theme);
            assert!(pool.len() >= 4, "{theme:?}");
            assert_eq!(pool.iter().collect::<BTreeSet<_>>().len(), pool.len());
            for name in pool {
                assert!(name.chars().count() <= 26, "{name}");
                assert!(name.starts_with("The ") && name.ends_with(" Update"));
                // Title case: every word opens with a capital or a digit.
                assert!(
                    name.split(' ')
                        .all(|word| word.chars().next().is_some_and(|first| first
                            .is_ascii_uppercase()
                            || first.is_ascii_digit())),
                    "{name}"
                );
            }
            let picked = (0..32)
                .map(|seed| patch_name(Some(theme), seed).unwrap())
                .collect::<BTreeSet<_>>();
            assert!(picked.len() > 1, "{theme:?}");
            assert!(picked.iter().all(|name| pool.contains(name)));
            assert_eq!(patch_name(Some(theme), 7), patch_name(Some(theme), 7));
        }
    }
}
