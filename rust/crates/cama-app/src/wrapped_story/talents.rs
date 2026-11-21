//! Talents: a Dota talent tree of four either/or tiers, each a way the
//! player's games split in two, with the half they won more of picked.
//!
//! Both options of a tier show a win rate, so the picked side is always the
//! one with the bigger number on the card. A pair needs enough known results
//! on both sides and two rates that read differently.

use chrono::{Datelike, Weekday};

use cama_domain::wrapped_distinct::{DistinctMatch, local_time};

use super::patch_support::{core, counted, finite, minutes, mix, own_games, percent};

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

/// Known results each side of a pair needs.
const MIN_SIDE_GAMES: usize = 5;
/// Win-rate points apart that count as a full-strength contrast.
const RATE_SCALE: f64 = 25.0;

/// The two options of a pair; `{}` is the win rate.
type Labels = (&'static str, &'static str);

const SIDES: Labels = ("{}% Radiant", "{}% Dire");
const ROLES: Labels = ("{}% as core", "{}% as support");
const LENGTH: Labels = ("{}% under 35m", "{}% over 45m");
const ODDS: Labels = ("{}% as favorite", "{}% as underdog");
const WEEK: Labels = ("{}% weekdays", "{}% weekends");
const HALVES: Labels = ("{}% Jan\u{2013}Jun", "{}% Jul\u{2013}Dec");

struct Pair {
    key: &'static str,
    left: String,
    right: String,
    picked: TalentSide,
    /// How far apart the sides are, on a scale shared by every pair.
    contrast: f64,
}

/// The player's talent tree: the four clearest contrasts in their year, or
/// nothing when fewer than four pairs have the data. `seed` orders pairs
/// whose contrasts tie.
#[must_use]
pub fn talent_tree(
    matches: &[DistinctMatch],
    player_id: i64,
    timezone: Option<&str>,
    seed: u64,
) -> Vec<TalentTier> {
    let games = own_games(matches, player_id);
    let mut pairs = [
        sides(&games),
        roles(&games),
        length(&games),
        odds(&games),
        week(&games, timezone),
        halves(&games),
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

/// A win-rate pair over the games `side` sorts to the left or the right. The
/// higher rate is picked, and rates that read the same are no talent.
fn rates(
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
    if left == right {
        return None;
    }
    Some(Pair {
        key,
        left: labels.0.replace("{}", &left.to_string()),
        right: labels.1.replace("{}", &right.to_string()),
        picked: if left > right {
            TalentSide::Left
        } else {
            TalentSide::Right
        },
        contrast: left.abs_diff(right) as f64 / scale,
    })
}

fn sides(games: &[&DistinctMatch]) -> Option<Pair> {
    rates("sides", SIDES, RATE_SCALE, games, |game| {
        match game.side.as_deref() {
            Some("radiant") => Some(TalentSide::Left),
            Some("dire") => Some(TalentSide::Right),
            _ => None,
        }
    })
}

fn roles(games: &[&DistinctMatch]) -> Option<Pair> {
    rates("roles", ROLES, RATE_SCALE, games, |game| {
        core(game).map(|core| {
            if core {
                TalentSide::Left
            } else {
                TalentSide::Right
            }
        })
    })
}

fn length(games: &[&DistinctMatch]) -> Option<Pair> {
    rates("length", LENGTH, RATE_SCALE, games, |game| {
        match minutes(game)? {
            played if played < 35.0 => Some(TalentSide::Left),
            played if played >= 45.0 => Some(TalentSide::Right),
            _ => None,
        }
    })
}

/// Favorites are expected to win more, so the gap has to be wider to count.
fn odds(games: &[&DistinctMatch]) -> Option<Pair> {
    rates("odds", ODDS, 40.0, games, |game| {
        match finite(game.expected_win).filter(|odds| *odds <= 1.0)? {
            odds if odds >= 0.55 => Some(TalentSide::Left),
            odds if odds <= 0.45 => Some(TalentSide::Right),
            _ => None,
        }
    })
}

fn week(games: &[&DistinctMatch], timezone: Option<&str>) -> Option<Pair> {
    rates("week", WEEK, RATE_SCALE, games, |game| {
        let finished = local_time(game.finished_at.filter(|time| *time > 0)?, timezone)?;
        Some(match finished.weekday() {
            Weekday::Sat | Weekday::Sun => TalentSide::Right,
            _ => TalentSide::Left,
        })
    })
}

/// The year is a UTC year, so its halves are UTC months: a game late on
/// New Year's Eve in the player's zone still opens the year.
fn halves(games: &[&DistinctMatch]) -> Option<Pair> {
    rates("halves", HALVES, RATE_SCALE, games, |game| {
        let finished =
            chrono::DateTime::from_timestamp(game.finished_at.filter(|time| *time > 0)?, 0)?;
        Some(if finished.month() <= 6 {
            TalentSide::Left
        } else {
            TalentSide::Right
        })
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

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

    /// The win rate an option leads with.
    fn number(text: &str) -> f64 {
        text.split('%').next().unwrap().parse().unwrap()
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
        let pair = sides(&refs(&rows)).expect("sides");
        assert_eq!(
            (pair.left.as_str(), pair.right.as_str()),
            ("60% Radiant", "40% Dire")
        );
        assert_eq!(pair.picked, TalentSide::Left);
        assert!((pair.contrast - 0.8).abs() < 1e-9);
        let pair = sides(&refs(&sided(2, 9))).expect("sides");
        assert_eq!(pair.picked, TalentSide::Right);
    }

    #[test]
    fn a_rate_pair_needs_five_known_results_a_side_and_two_different_numbers() {
        // Equal rates favor neither side.
        assert!(sides(&refs(&sided(5, 5))).is_none());
        // Unknown results and unknown sides are not games on a side.
        let mut rows = sided(6, 4);
        for row in rows.iter_mut().take(6) {
            row.won = None;
        }
        assert!(sides(&refs(&rows)).is_none());
        let mut rows = sided(6, 4);
        for row in rows.iter_mut().skip(14) {
            row.side = None;
        }
        assert!(sides(&refs(&rows)).is_none());
        rows[14].side = Some("dire".to_owned());
        assert!(sides(&refs(&rows)).is_some());
    }

    #[test]
    fn roles_compare_win_rates_and_need_a_sample_in_both() {
        let rows = |supports: i64| {
            (0..12)
                .map(|id| DistinctMatch {
                    position: Some(if id < supports { 5 } else { 1 }),
                    won: Some(id % 2 == 0 || id >= supports),
                    ..game(id)
                })
                .collect::<Vec<_>>()
        };
        let pair = roles(&refs(&rows(6))).expect("role rates");
        assert_eq!(
            (pair.left.as_str(), pair.right.as_str()),
            ("100% as core", "50% as support")
        );
        assert_eq!(pair.picked, TalentSide::Left);
        // Two support games are not a sample, and neither are positions
        // outside 1 to 5.
        assert!(roles(&refs(&rows(2))).is_none());
        let mut unknown = rows(6);
        unknown[0].position = Some(9);
        unknown[1].position = None;
        assert!(roles(&refs(&unknown)).is_none());
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
        let pair = length(&refs(&rows)).expect("length");
        assert_eq!((number(&pair.left), number(&pair.right)), (80.0, 20.0));
        assert_eq!(pair.picked, TalentSide::Left);
        let pair = odds(&refs(&rows)).expect("odds");
        assert_eq!((number(&pair.left), number(&pair.right)), (80.0, 20.0));
        assert!((pair.contrast - 1.5).abs() < 1e-9);

        let mut unknown = rows.clone();
        unknown[0].duration_seconds = Some(0);
        unknown[1].expected_win = Some(f64::NAN);
        assert!(length(&refs(&unknown)).is_none());
        assert!(odds(&refs(&unknown)).is_none());
        unknown[1].expected_win = Some(1.5);
        assert!(odds(&refs(&unknown)).is_none());
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
        assert!(week(&refs(&rows), None).is_none());
        let pair = week(&refs(&rows), Some("Europe/Berlin")).expect("week");
        assert_eq!((number(&pair.left), number(&pair.right)), (0.0, 80.0));
        assert_eq!(pair.picked, TalentSide::Right);

        // 2026-07-01 02:00 UTC is July in the year's own clock, whatever
        // the player's zone says.
        let july_utc = 1_782_871_200;
        let rows = (0..10)
            .map(|id| DistinctMatch {
                finished_at: Some(if id < 5 {
                    july_utc - 86_400 * (id + 1)
                } else {
                    july_utc + 86_400 * (id - 5)
                }),
                won: Some(id < 5),
                ..game(id)
            })
            .collect::<Vec<_>>();
        let pair = halves(&refs(&rows)).expect("halves");
        assert_eq!((number(&pair.left), number(&pair.right)), (100.0, 0.0));
        let mut undated = rows.clone();
        undated[0].finished_at = None;
        assert!(halves(&refs(&undated)).is_none());
    }

    /// A year in which five pairs have data: sides, roles, length, odds and
    /// the week. Every game is in the first half of the year.
    fn full_year() -> Vec<DistinctMatch> {
        let mut rows = (0..40)
            .map(|id| DistinctMatch {
                side: Some(if id % 2 == 0 { "radiant" } else { "dire" }.to_owned()),
                won: Some(id % 2 == 0 || id % 10 == 1),
                position: Some(if id % 4 == 0 { 2 } else { 5 }),
                duration_seconds: Some(if id % 5 < 2 { 30 * 60 } else { 50 * 60 }),
                expected_win: Some(if id % 3 == 0 { 0.6 } else { 0.4 }),
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
            row.position = None;
            row.duration_seconds = None;
        }
        // Sides, odds and the week are left.
        assert!(talent_tree(&rows, 7, None, 0).is_empty());
        for row in &mut rows {
            row.position = Some(if row.match_id % 4 == 0 { 2 } else { 5 });
        }
        assert_eq!(talent_tree(&rows, 7, None, 0).len(), 4);
    }

    #[test]
    fn the_tree_is_stable_under_reordering_and_the_seed_only_breaks_ties() {
        let mut rows = full_year();
        let tree = talent_tree(&rows, 7, Some("Europe/Berlin"), 4);
        rows.reverse();
        assert_eq!(talent_tree(&rows, 7, Some("Europe/Berlin"), 4), tree);
        // No two of this year's contrasts tie, so every seed gets this tree.
        let trees = (0..32)
            .map(|seed| format!("{:?}", talent_tree(&rows, 7, Some("Europe/Berlin"), seed)))
            .collect::<BTreeSet<_>>();
        assert_eq!(trees, BTreeSet::from([format!("{tree:?}")]));
    }

    #[test]
    fn every_option_fits_its_plate_at_a_full_win_rate() {
        for (left, right) in [SIDES, ROLES, LENGTH, ODDS, WEEK, HALVES] {
            for label in [left, right] {
                let text = label.replace("{}", "100");
                assert!(text.starts_with("100% "), "{label}");
                assert!(text.chars().count() <= MAX_TALENT_TEXT, "{text}");
            }
        }
    }
}
