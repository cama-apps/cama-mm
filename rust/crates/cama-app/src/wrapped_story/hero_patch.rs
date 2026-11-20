//! Hero Updates: the heroes a player's year buffed, nerfed, added, dropped,
//! or lost to opposing bans.
//!
//! Buffs and nerfs compare the same early and late halves of the year the
//! balance changes use. Lines never name the hero; the card header does.

use std::collections::{BTreeMap, BTreeSet};

use cama_domain::wrapped_distinct::DistinctMatch;

use super::format_flavor;
use super::patch_support::{counted, hero_of, nonnegative, own_games, percent, timeline, variant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HeroChangeTag {
    Buffed,
    Nerfed,
    New,
    Removed,
    Banned,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeroUpdate {
    pub hero_id: i64,
    pub tag: HeroChangeTag,
    /// One or two short lines.
    pub lines: Vec<String>,
    /// The player's games on the hero this year.
    pub games: usize,
}

pub const MAX_HERO_UPDATES: usize = 3;

/// Games a hero needs in a half, in the pool, or under a ban to be a story.
const MIN_GAMES: usize = 3;
/// Percentage points of win rate between the halves.
const WIN_RATE_SWING: usize = 20;
const KDA_SWING: f64 = 1.5;
const DEATHS_SWING: f64 = 2.0;

const NEW_LINES: &[&str] = &[
    "Added to the hero pool.",
    "Added to the rotation.",
    "Now enabled in drafts.",
];
const REMOVED_LINES: &[&str] = &[
    "Removed from the hero pool after {n} games.",
    "Removed from the rotation after {n} games.",
    "Disabled after {n} games.",
];
const REMOVED_LOSING: &[&str] = &[
    "Win rate was {p}%. Working as intended.",
    "Win rate was {p}%. Removal was overdue.",
    "Win rate was {p}%. No further comment.",
];
const REMOVED_WINNING: &[&str] = &[
    "Win rate was {p}%. Reason not given.",
    "Win rate was {p}%. Removed anyway.",
];
const BANNED_LINES: &[&str] = &[
    "Banned by opponents in {n} of your games.",
    "Opponents spent a ban on it in {n} of your games.",
];
const BANNED_WINNING: &[&str] = &[
    "{p}% wins in the {n} games it got through.",
    "{p}% wins in {n} games when left open. They had a point.",
];
const BANNED_LOSING: &[&str] = &[
    "{p}% wins in {n} games when left open.",
    "{p}% wins in {n} games. The bans were generous.",
];

/// Up to three hero updates for `player_id`: different tags first, the most
/// striking first, and never the same hero twice. `bans` lists, per hero, how
/// many of the player's games the opposing team banned it in.
#[must_use]
pub fn hero_updates(
    matches: &[DistinctMatch],
    player_id: i64,
    bans: &[(i64, i64)],
    seed: u64,
) -> Vec<HeroUpdate> {
    let games = own_games(matches, player_id);
    let mut by_hero: BTreeMap<i64, Vec<&DistinctMatch>> = BTreeMap::new();
    for game in &games {
        if let Some(hero) = hero_of(game) {
            by_hero.entry(hero).or_default().push(game);
        }
    }
    let line = |pool: &[&str], hero: i64, count: usize, rate: usize| {
        let template = variant(seed, &format!("{}:{hero}", pool[0]), pool);
        format_flavor(
            template,
            &[("n", &count.to_string()), ("p", &rate.to_string())],
        )
    };
    let mut candidates: Vec<(f64, HeroUpdate)> = Vec::new();

    if let Some(split) = timeline(&games) {
        let ids = |half: &[&DistinctMatch]| {
            half.iter()
                .map(|game| game.match_id)
                .collect::<BTreeSet<_>>()
        };
        let (early, late) = (ids(&split.early), ids(&split.late));
        // A game on an unknown hero could have been on any hero, so it blocks
        // every claim about when a hero entered or left the pool.
        let pool_known_outside = |half: &BTreeSet<i64>| {
            games
                .iter()
                .all(|game| half.contains(&game.match_id) || hero_of(game).is_some())
        };
        let (known_before_late, known_after_early) =
            (pool_known_outside(&late), pool_known_outside(&early));
        for (&hero, played) in &by_hero {
            let within = |half: &BTreeSet<i64>| {
                played
                    .iter()
                    .copied()
                    .filter(|game| half.contains(&game.match_id))
                    .collect::<Vec<_>>()
            };
            let (first, last) = (within(&early), within(&late));
            if first.len() >= MIN_GAMES && last.len() >= MIN_GAMES {
                let stats = shifts(&first, &last);
                if let Some((strength, buff, _)) = stats.first() {
                    candidates.push((
                        strength.min(3.0),
                        HeroUpdate {
                            hero_id: hero,
                            tag: if *buff {
                                HeroChangeTag::Buffed
                            } else {
                                HeroChangeTag::Nerfed
                            },
                            lines: stats
                                .iter()
                                .filter(|(_, direction, _)| direction == buff)
                                .take(2)
                                .map(|(_, _, line)| line.clone())
                                .collect(),
                            games: played.len(),
                        },
                    ));
                }
            }
            // Every game on the hero sits in one half, and every game
            // outside that half is known to be on another hero.
            let (only, tag) = if last.len() == played.len() && known_before_late {
                (&last, HeroChangeTag::New)
            } else if first.len() == played.len() && known_after_early {
                (&first, HeroChangeTag::Removed)
            } else {
                continue;
            };
            let count = only.len();
            if count < MIN_GAMES {
                continue;
            }
            let (wins, known) = counted(only, |game| game.won);
            let rate = (known == count).then(|| percent(wins, known));
            let lines = if tag == HeroChangeTag::New {
                vec![
                    line(NEW_LINES, hero, count, 0),
                    rate.map_or_else(
                        || format!("{count} games."),
                        |rate| format!("{count} games, {rate}% wins."),
                    ),
                ]
            } else {
                let mut lines = vec![line(REMOVED_LINES, hero, count, 0)];
                lines.extend(rate.map(|rate| {
                    let pool = if rate < 50 {
                        REMOVED_LOSING
                    } else {
                        REMOVED_WINNING
                    };
                    line(pool, hero, count, rate)
                }));
                lines
            };
            let lopsided = rate.map_or(0.0, |rate| rate.abs_diff(50) as f64 / 100.0);
            candidates.push((
                1.0 + (count as f64 / 10.0).min(1.0) + lopsided,
                HeroUpdate {
                    hero_id: hero,
                    tag,
                    lines,
                    games: count,
                },
            ));
        }
    }

    let mut banned: BTreeMap<i64, i64> = BTreeMap::new();
    for &(hero, count) in bans {
        let held = banned.entry(hero).or_insert(count);
        *held = (*held).max(count);
    }
    for (hero, count) in banned {
        let (Some(played), Ok(count)) = (by_hero.get(&hero), usize::try_from(count)) else {
            continue;
        };
        // More bans than games played is not this player's year.
        if count < MIN_GAMES || count > games.len() || played.len() < MIN_GAMES {
            continue;
        }
        let mut lines = vec![line(BANNED_LINES, hero, count, 0)];
        let (wins, known) = counted(played, |game| game.won);
        if known == played.len() {
            let rate = percent(wins, known);
            let pool = if rate >= 55 {
                BANNED_WINNING
            } else {
                BANNED_LOSING
            };
            lines.push(line(pool, hero, known, rate));
        }
        candidates.push((
            1.0 + (count as f64 / 5.0).min(2.0),
            HeroUpdate {
                hero_id: hero,
                tag: HeroChangeTag::Banned,
                lines,
                games: played.len(),
            },
        ));
    }

    candidates.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| a.1.hero_id.cmp(&b.1.hero_id))
            .then_with(|| a.1.tag.cmp(&b.1.tag))
    });
    let mut chosen: Vec<HeroUpdate> = Vec::new();
    for fresh_tags_only in [true, false] {
        for (_, update) in &candidates {
            let taken = chosen.iter().any(|held| {
                held.hero_id == update.hero_id || (fresh_tags_only && held.tag == update.tag)
            });
            if chosen.len() < MAX_HERO_UPDATES && !taken {
                chosen.push(update.clone());
            }
        }
    }
    chosen
}

fn moved(up: bool) -> &'static str {
    if up { "increased" } else { "reduced" }
}

/// Every stat that moved far enough between the halves, strongest first:
/// how far past its threshold, whether the move helps, and the line.
fn shifts(early: &[&DistinctMatch], late: &[&DistinctMatch]) -> Vec<(f64, bool, String)> {
    let mut shifts = Vec::new();

    let ((early_wins, early_known), (late_wins, late_known)) = (
        counted(early, |game| game.won),
        counted(late, |game| game.won),
    );
    if early_known >= MIN_GAMES && late_known >= MIN_GAMES {
        // Cross-multiplied, so the threshold comparison stays exact.
        let (before, after) = (early_wins * late_known, late_wins * early_known);
        let swing = before.abs_diff(after) * 100;
        let needed = WIN_RATE_SWING * early_known * late_known;
        if swing >= needed {
            shifts.push((
                swing as f64 / needed as f64,
                after > before,
                format!(
                    "Win rate {} from {}% to {}%.",
                    moved(after > before),
                    percent(early_wins, early_known),
                    percent(late_wins, late_known)
                ),
            ));
        }
    }

    let kda = |games: &[&DistinctMatch]| {
        let lines = games
            .iter()
            .filter_map(|game| {
                Some((
                    nonnegative(game.kills)? + nonnegative(game.assists)?,
                    nonnegative(game.deaths)?,
                ))
            })
            .collect::<Vec<_>>();
        (lines.len() >= MIN_GAMES).then(|| {
            let (takedowns, deaths) = lines
                .iter()
                .fold((0, 0), |sum, line| (sum.0 + line.0, sum.1 + line.1));
            takedowns as f64 / deaths.max(1) as f64
        })
    };
    if let (Some(before), Some(after)) = (kda(early), kda(late)) {
        let delta = after - before;
        if delta.abs() >= KDA_SWING {
            shifts.push((
                delta.abs() / KDA_SWING,
                delta > 0.0,
                format!("KDA {} from {before:.1} to {after:.1}.", moved(delta > 0.0)),
            ));
        }
    }

    let deaths = |games: &[&DistinctMatch]| {
        let known = games
            .iter()
            .filter_map(|game| nonnegative(game.deaths))
            .collect::<Vec<_>>();
        (known.len() >= MIN_GAMES).then(|| known.iter().sum::<i64>() as f64 / known.len() as f64)
    };
    if let (Some(before), Some(after)) = (deaths(early), deaths(late)) {
        let delta = after - before;
        if delta.abs() >= DEATHS_SWING {
            shifts.push((
                delta.abs() / DEATHS_SWING,
                delta < 0.0,
                format!(
                    "Deaths per game {} from {before:.1} to {after:.1}.",
                    moved(delta > 0.0)
                ),
            ));
        }
    }

    shifts.sort_by(|a, b| b.0.total_cmp(&a.0));
    shifts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Twelve dated games for player 7: ids 1-6 are the early half, 7-12 the
    /// late half. `heroes[i]` is the hero of game `i + 1`.
    fn year(heroes: [i64; 12]) -> Vec<DistinctMatch> {
        heroes
            .iter()
            .enumerate()
            .map(|(index, hero)| DistinctMatch {
                match_id: index as i64 + 1,
                discord_id: 7,
                finished_at: Some((index as i64 + 1) * 86_400),
                hero_id: Some(*hero),
                won: Some(true),
                ..Default::default()
            })
            .collect()
    }

    fn tags(updates: &[HeroUpdate]) -> Vec<(i64, HeroChangeTag)> {
        updates
            .iter()
            .map(|update| (update.hero_id, update.tag))
            .collect()
    }

    const SPLIT: [i64; 12] = [1, 1, 1, 2, 2, 2, 1, 1, 1, 2, 2, 2];

    #[test]
    fn win_rate_swing_tags_a_buff_and_its_mirror_a_nerf() {
        let mut rows = year(SPLIT);
        rows[0].won = Some(false);
        rows[1].won = Some(false);
        let updates = hero_updates(&rows, 7, &[], 0);
        assert_eq!(tags(&updates), [(1, HeroChangeTag::Buffed)]);
        assert_eq!(updates[0].lines, ["Win rate increased from 33% to 100%."]);
        assert_eq!(updates[0].games, 6);

        let mut rows = year(SPLIT);
        rows[6].won = Some(false);
        rows[7].won = Some(false);
        let updates = hero_updates(&rows, 7, &[], 0);
        assert_eq!(tags(&updates), [(1, HeroChangeTag::Nerfed)]);
        assert_eq!(updates[0].lines, ["Win rate reduced from 100% to 33%."]);
    }

    #[test]
    fn a_swing_needs_three_known_games_in_each_half_and_twenty_points() {
        // Two early games on hero 1 are not a sample.
        let mut rows = year([1, 1, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2]);
        rows[0].won = Some(false);
        rows[1].won = Some(false);
        assert!(hero_updates(&rows, 7, &[], 0).is_empty());

        // Three games, but one result unknown.
        let mut rows = year(SPLIT);
        rows[0].won = Some(false);
        rows[1].won = Some(false);
        rows[2].won = None;
        assert!(hero_updates(&rows, 7, &[], 0).is_empty());

        // Exactly twenty points counts; ten does not.
        let twenty = |early_wins: usize, late_wins: usize| {
            let mut rows = (1..=20_i64)
                .map(|id| DistinctMatch {
                    match_id: id,
                    discord_id: 7,
                    finished_at: Some(id * 86_400),
                    hero_id: Some(1),
                    won: Some(false),
                    ..Default::default()
                })
                .collect::<Vec<_>>();
            for row in rows.iter_mut().take(early_wins) {
                row.won = Some(true);
            }
            for row in rows.iter_mut().skip(10).take(late_wins) {
                row.won = Some(true);
            }
            hero_updates(&rows, 7, &[], 0)
        };
        assert!(twenty(4, 5).is_empty());
        let updates = twenty(4, 6);
        assert_eq!(updates[0].lines, ["Win rate increased from 40% to 60%."]);
    }

    #[test]
    fn deaths_and_kda_swings_read_as_patch_lines_and_ignore_unknown_values() {
        let mut rows = year(SPLIT);
        for (index, row) in rows.iter_mut().enumerate() {
            row.kills = Some(4);
            row.assists = Some(8);
            row.deaths = Some(if index < 6 { 8 } else { 5 });
        }
        let updates = hero_updates(&rows, 7, &[], 0);
        // Both heroes died less; the lower hero id wins the tie and the tag
        // is not repeated before every tag has had its turn.
        assert_eq!(
            tags(&updates),
            [(1, HeroChangeTag::Buffed), (2, HeroChangeTag::Buffed)]
        );
        assert_eq!(
            updates[0].lines,
            ["Deaths per game reduced from 8.0 to 5.0."]
        );

        for row in rows.iter_mut().skip(6) {
            row.deaths = Some(2);
        }
        let updates = hero_updates(&rows, 7, &[], 0);
        assert_eq!(
            updates[0].lines,
            [
                "KDA increased from 1.5 to 6.0.",
                "Deaths per game reduced from 8.0 to 2.0."
            ]
        );

        // Unknown or negative deaths leave too few games to compare.
        rows[0].deaths = None;
        rows[3].deaths = Some(-1);
        assert!(hero_updates(&rows, 7, &[], 0).is_empty());
    }

    #[test]
    fn a_nerf_never_borrows_a_line_that_points_the_other_way() {
        let mut rows = year(SPLIT);
        for (index, row) in rows.iter_mut().enumerate() {
            row.deaths = Some(if index < 6 { 8 } else { 3 });
        }
        for row in rows.iter_mut().skip(6).take(3) {
            row.won = Some(false);
        }
        let updates = hero_updates(&rows, 7, &[], 0);
        let hero = updates.iter().find(|update| update.hero_id == 1).unwrap();
        assert_eq!(hero.tag, HeroChangeTag::Nerfed);
        assert_eq!(hero.lines, ["Win rate reduced from 100% to 0%."]);
    }

    #[test]
    fn new_and_removed_heroes_need_three_games_and_a_fully_known_pool() {
        let mut rows = year([1, 1, 1, 2, 2, 2, 3, 3, 3, 2, 2, 2]);
        rows[6].won = Some(false);
        rows[0].won = Some(false);
        rows[1].won = Some(false);
        let updates = hero_updates(&rows, 7, &[], 0);
        assert_eq!(
            tags(&updates),
            [(1, HeroChangeTag::Removed), (3, HeroChangeTag::New)]
        );
        let removed = &updates[0];
        assert_eq!(removed.games, 3);
        assert!(removed.lines[0].contains("after 3 games"));
        assert!(removed.lines[1].starts_with("Win rate was 33%."));
        let new = &updates[1];
        assert!(NEW_LINES.contains(&new.lines[0].as_str()));
        assert_eq!(new.lines[1], "3 games, 67% wins.");

        // An unknown result keeps the count but drops the win rate.
        rows[7].won = None;
        let updates = hero_updates(&rows, 7, &[], 0);
        let new = updates
            .iter()
            .find(|update| update.tag == HeroChangeTag::New)
            .unwrap();
        assert_eq!(new.lines[1], "3 games.");

        // An early game on an unknown hero could have been hero 3.
        rows[3].hero_id = None;
        assert_eq!(
            tags(&hero_updates(&rows, 7, &[], 0)),
            [(1, HeroChangeTag::Removed)]
        );
        // A late one could have been hero 1.
        rows[3].hero_id = Some(2);
        rows[11].hero_id = Some(0);
        assert_eq!(
            tags(&hero_updates(&rows, 7, &[], 0)),
            [(3, HeroChangeTag::New)]
        );

        // Two games are not a pool change.
        let rows = year([1, 1, 2, 2, 2, 2, 3, 3, 2, 2, 2, 2]);
        assert!(hero_updates(&rows, 7, &[], 0).is_empty());
    }

    #[test]
    fn an_undated_game_on_the_hero_blocks_new_and_removed() {
        let mut rows = year([1, 1, 1, 2, 2, 2, 3, 3, 3, 2, 2, 2]);
        rows.push(DistinctMatch {
            match_id: 99,
            discord_id: 7,
            hero_id: Some(3),
            ..Default::default()
        });
        assert_eq!(
            tags(&hero_updates(&rows, 7, &[], 0)),
            [(1, HeroChangeTag::Removed)]
        );
        rows[12].hero_id = Some(1);
        assert_eq!(
            tags(&hero_updates(&rows, 7, &[], 0)),
            [(3, HeroChangeTag::New)]
        );
    }

    #[test]
    fn bans_need_three_games_banned_and_three_games_played() {
        let rows = year([2; 12]);
        let banned = |bans: &[(i64, i64)]| hero_updates(&rows, 7, bans, 0);
        assert!(banned(&[(2, 2)]).is_empty());
        assert!(banned(&[(5, 9)]).is_empty());
        assert!(banned(&[(2, -4)]).is_empty());
        // More bans than the player has games cannot be their year.
        assert!(banned(&[(2, 13)]).is_empty());
        let updates = banned(&[(2, 9)]);
        assert_eq!(tags(&updates), [(2, HeroChangeTag::Banned)]);
        assert!(updates[0].lines[0].contains("in 9 of your games"));
        assert!(updates[0].lines[1].starts_with("100% wins in"));
        assert_eq!(updates[0].games, 12);

        // Without dates there is no timeline, but bans still tell a story.
        let undated = rows
            .iter()
            .map(|row| DistinctMatch {
                finished_at: None,
                won: None,
                ..row.clone()
            })
            .collect::<Vec<_>>();
        let updates = hero_updates(&undated, 7, &[(2, 3)], 0);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].lines.len(), 1);
    }

    #[test]
    fn selection_prefers_new_tags_then_fills_without_repeating_a_hero() {
        // Hero 1 removed, heroes 3 and 4 new, hero 2 nerfed and banned.
        let mut rows = (1..=24_i64)
            .map(|id| DistinctMatch {
                match_id: id,
                discord_id: 7,
                finished_at: Some(id * 86_400),
                hero_id: Some(match id {
                    1..=4 => 1,
                    5..=12 => 2,
                    13..=16 => 3,
                    17..=19 => 4,
                    _ => 2,
                }),
                won: Some(true),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        for row in rows.iter_mut().skip(19) {
            row.won = Some(false);
        }
        let updates = hero_updates(&rows, 7, &[(2, 4)], 0);
        assert_eq!(updates.len(), MAX_HERO_UPDATES);
        let heroes = updates
            .iter()
            .map(|update| update.hero_id)
            .collect::<BTreeSet<_>>();
        assert_eq!(heroes.len(), 3);
        let distinct_tags = updates
            .iter()
            .map(|update| update.tag)
            .collect::<BTreeSet<_>>();
        assert_eq!(distinct_tags.len(), 3);
        assert!(
            updates
                .iter()
                .all(|update| (1..=2).contains(&update.lines.len()))
        );

        // With only pool changes on offer, a tag may repeat but a hero may not.
        let updates = hero_updates(&rows, 7, &[], 0);
        assert_eq!(updates.len(), MAX_HERO_UPDATES);
        assert_eq!(
            updates
                .iter()
                .map(|update| update.hero_id)
                .collect::<BTreeSet<_>>()
                .len(),
            3
        );
    }

    #[test]
    fn updates_ignore_other_players_and_row_order_and_vary_by_seed() {
        let mut rows = year([1, 1, 1, 2, 2, 2, 3, 3, 3, 2, 2, 2]);
        rows[0].won = Some(false);
        rows.extend(year([9; 12]).into_iter().map(|row| DistinctMatch {
            discord_id: 8,
            ..row
        }));
        let bans = [(2, 5), (3, 1)];
        let updates = hero_updates(&rows, 7, &bans, 11);
        assert_eq!(updates.len(), 3);
        assert!(updates.iter().all(|update| update.hero_id != 9));
        rows.reverse();
        assert_eq!(hero_updates(&rows, 7, &[(3, 1), (2, 5)], 11), updates);
        assert_eq!(hero_updates(&rows, 7, &bans, 11), updates);
        let wordings = (0..32)
            .map(|seed| format!("{:?}", hero_updates(&rows, 7, &bans, seed)))
            .collect::<BTreeSet<_>>()
            .len();
        assert!(wordings > 1);
        assert!(hero_updates(&rows, 6, &bans, 11).is_empty());
    }

    #[test]
    fn every_line_is_short_and_never_names_a_hero() {
        for pool in [
            NEW_LINES,
            REMOVED_LINES,
            REMOVED_LOSING,
            REMOVED_WINNING,
            BANNED_LINES,
            BANNED_WINNING,
            BANNED_LOSING,
        ] {
            assert!(pool.len() >= 2);
            for template in pool {
                let line = format_flavor(template, &[("n", "1234"), ("p", "100")]);
                assert!(!line.contains('{'), "{template}");
                assert!(line.chars().count() <= 60, "{line}");
                assert!(line.ends_with('.'), "{line}");
            }
        }
        let wide = |value: i64| {
            (0..6)
                .map(|_| DistinctMatch {
                    kills: Some(value),
                    deaths: Some(value),
                    assists: Some(value),
                    won: Some(true),
                    ..Default::default()
                })
                .collect::<Vec<_>>()
        };
        let (early, late) = (wide(999), wide(1));
        let mut early = early.iter().collect::<Vec<_>>();
        let late = late.iter().collect::<Vec<_>>();
        for (_, _, line) in shifts(&early, &late) {
            assert!(line.chars().count() <= 60, "{line}");
        }
        early.truncate(2);
        assert!(shifts(&early, &late).is_empty());
    }
}
