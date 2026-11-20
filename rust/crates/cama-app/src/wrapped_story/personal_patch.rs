//! Personal patch notes from recorded matches, without server rankings.

use cama_domain::wrapped_distinct::DistinctMatch;
use std::collections::BTreeSet;

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

#[must_use]
pub fn personal_patch(matches: &[DistinctMatch], player_id: i64) -> PersonalPatch {
    let games = matches
        .iter()
        .filter(|game| game.discord_id == player_id)
        .collect::<Vec<_>>();
    PersonalPatch {
        awards: awards(&games),
        changes: balance_changes(&games),
    }
}

fn nonnegative(value: Option<i64>) -> Option<i64> {
    value.filter(|value| *value >= 0)
}

fn finite(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value >= 0.0)
}

fn counted(
    games: &[&DistinctMatch],
    condition: impl Fn(&DistinctMatch) -> Option<bool>,
) -> (usize, usize) {
    games
        .iter()
        .filter_map(|game| condition(game))
        .fold((0, 0), |(hits, known), hit| {
            (hits + usize::from(hit), known + 1)
        })
}

fn awards(games: &[&DistinctMatch]) -> Vec<PatchAward> {
    let mut candidates = Vec::new();
    let mut add = |theme, score, key, title: &str, value, evidence| {
        candidates.push((
            theme,
            score,
            PatchAward {
                key,
                title: title.to_owned(),
                value,
                evidence,
            },
        ));
    };

    // Conversion rates describe results, never whether one action caused a win.
    for (key, title, event, minimum_wins, theme, condition) in [
        (
            "buyback_wins",
            "Buyback approved",
            "a buyback",
            2,
            0,
            (|game: &DistinctMatch| Some(nonnegative(game.buybacks)? > 0))
                as fn(&DistinctMatch) -> Option<bool>,
        ),
        (
            "first_blood_wins",
            "Started it. Finished it.",
            "your first blood",
            3,
            1,
            (|game: &DistinctMatch| game.first_blood) as fn(&DistinctMatch) -> Option<bool>,
        ),
        (
            "roshan_wins",
            "Roshan eviction notice",
            "a Roshan kill",
            3,
            2,
            (|game: &DistinctMatch| Some(nonnegative(game.roshans)? > 0))
                as fn(&DistinctMatch) -> Option<bool>,
        ),
    ] {
        let (wins, known) = counted(games, |game| {
            condition(game)?.then_some(())?;
            game.won
        });
        if known >= 3 && wins >= minimum_wins && wins as f64 / known as f64 >= 0.6 {
            add(
                theme,
                1.0 + wins as f64 / known as f64,
                key,
                title,
                format!("{wins} wins"),
                format!("Won {wins} of {known} recorded games with {event}."),
            );
        }
    }

    for (key, title, detail, theme, condition) in [
        (
            "support_infrastructure",
            "Support budget increased",
            "10+ wards and 3+ camps stacked",
            3,
            (|game: &DistinctMatch| {
                let (wards, stacks) = (nonnegative(game.wards)?, nonnegative(game.camps_stacked)?);
                Some(wards >= 10 && stacks >= 3)
            }) as fn(&DistinctMatch) -> Option<bool>,
        ),
        (
            "crowd_control",
            "Please remain seated",
            "60+ stun seconds and 20+ assists",
            3,
            (|game: &DistinctMatch| {
                let (stuns, assists) = (finite(game.stuns)?, nonnegative(game.assists)?);
                Some(stuns >= 60.0 && assists >= 20)
            }) as fn(&DistinctMatch) -> Option<bool>,
        ),
        (
            "rune_pace",
            "River patrol",
            "At least 2 rune pickups per 10 minutes",
            4,
            (|game: &DistinctMatch| {
                Some(
                    nonnegative(game.rune_pickups)? as f64 * 300.0
                        >= game.duration_seconds.filter(|duration| *duration > 0)? as f64,
                )
            }) as fn(&DistinctMatch) -> Option<bool>,
        ),
        (
            "team_shopping",
            "Packed for the team",
            "Bought both Smoke of Deceit and Dust",
            5,
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
            }) as fn(&DistinctMatch) -> Option<bool>,
        ),
    ] {
        let (hits, known) = counted(games, condition);
        if known >= 5 && hits >= 3 {
            add(
                theme,
                1.0 + hits as f64 / known as f64,
                key,
                title,
                format!("{hits} games"),
                format!("{detail} in {hits} of {known} games with stats."),
            );
        }
    }

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
        add(
            6,
            1.0 + radiant.min(dire) / 100.0,
            "both_sides",
            "Side selection disabled",
            format!("{radiant:.0}% / {dire:.0}%"),
            format!(
                "Won {radiant_wins}/{radiant_games} on Radiant and {dire_wins}/{dire_games} on Dire."
            ),
        );
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
    let mut dated = games
        .iter()
        .copied()
        .filter(|game| game.finished_at.is_some_and(|time| time > 0))
        .collect::<Vec<_>>();
    dated.sort_by_key(|game| (game.finished_at, game.match_id));
    let group_size = dated.len() / 2;
    if group_size < 5 {
        return Vec::new();
    }
    // Use one timeline for every metric. For odd counts the middle game is
    // omitted; equal boundary timestamps cannot establish an earlier period.
    let early = &dated[..group_size];
    let late = &dated[dated.len() - group_size..];
    if early[group_size - 1].finished_at >= late[0].finished_at {
        return Vec::new();
    }
    let mut changes = Vec::new();
    for (key, title, threshold, lower_is_better, measure) in [
        (
            "gpm",
            "Gold income",
            40.0,
            false,
            (|game: &DistinctMatch| nonnegative(game.gpm).map(|value| value as f64))
                as fn(&DistinctMatch) -> Option<f64>,
        ),
        (
            "deaths",
            "Deaths",
            1.0,
            true,
            (|game: &DistinctMatch| nonnegative(game.deaths).map(|value| value as f64))
                as fn(&DistinctMatch) -> Option<f64>,
        ),
        (
            "win_rate",
            "Win rate",
            15.0,
            false,
            (|game: &DistinctMatch| game.won.map(|won| if won { 100.0 } else { 0.0 }))
                as fn(&DistinctMatch) -> Option<f64>,
        ),
        (
            "wards",
            "Wards placed",
            3.0,
            false,
            (|game: &DistinctMatch| nonnegative(game.wards).map(|value| value as f64))
                as fn(&DistinctMatch) -> Option<f64>,
        ),
        (
            "stuns",
            "Stun duration",
            10.0,
            false,
            (|game: &DistinctMatch| finite(game.stuns)) as fn(&DistinctMatch) -> Option<f64>,
        ),
    ] {
        let averages = [early, late].map(|group| {
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
            _ => format!("{before:.1} to {after:.1} wards/game"),
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
        let report = personal_patch(&rows, 7);
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
        assert!(personal_patch(&rows, 7).awards.is_empty());
        for row in &mut rows[..3] {
            row.camps_stacked = Some(4);
        }
        let report = personal_patch(&rows, 7);
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
        assert!(personal_patch(&rows, 7).awards.is_empty());
    }

    #[test]
    fn first_blood_and_roshan_awards_require_wins_not_only_participation() {
        let mut rows = games(5);
        for row in &mut rows {
            row.first_blood = Some(true);
            row.roshans = Some(1);
            row.won = Some(false);
        }
        assert!(personal_patch(&rows, 7).awards.is_empty());
        for row in &mut rows[..3] {
            row.won = Some(true);
        }
        let report = personal_patch(&rows, 7);
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
        let report = personal_patch(&rows, 7);
        assert_eq!(report.awards[0].key, "team_shopping");
        assert!(report.awards[0].evidence.contains("3 of 5"));
        rows[4].items = None;
        assert!(personal_patch(&rows, 7).awards.is_empty());
    }

    #[test]
    fn rune_pace_rejects_missing_zero_and_negative_durations() {
        let mut rows = games(5);
        for row in &mut rows {
            row.duration_seconds = Some(1800);
            row.rune_pickups = Some(6);
        }
        let report = personal_patch(&rows, 7);
        assert_eq!(report.awards[0].key, "rune_pace");
        for invalid in [None, Some(0), Some(-1)] {
            rows[0].duration_seconds = invalid;
            assert!(personal_patch(&rows, 7).awards.is_empty());
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
        let report = personal_patch(&rows, 7);
        assert_eq!(report.awards[0].key, "crowd_control");
        assert!(report.awards[0].evidence.contains("3 of 5"));
        rows[3].stuns = Some(f64::NAN);
        assert!(personal_patch(&rows, 7).awards.is_empty());
    }

    #[test]
    fn win_rate_decreases_are_nerfs_and_flat_measurements_are_omitted() {
        let mut rows = games(10);
        for (index, row) in rows.iter_mut().enumerate() {
            row.won = Some(index < 5);
            row.gpm = Some(500);
        }
        let report = personal_patch(&rows, 7);
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
        let report = personal_patch(&rows, 7);
        assert_eq!(report.awards[0].key, "both_sides");
        assert!(report.awards[0].value.contains("60%"));
        rows[9].won = None;
        assert!(personal_patch(&rows, 7).awards.is_empty());
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
        let report = personal_patch(&rows, 7);
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
        let report = personal_patch(&rows, 7);
        assert_eq!(report.changes.len(), 1);
        assert_eq!(report.changes[0].value, "400 to 500 GPM");
        assert_eq!(
            (report.changes[0].early_games, report.changes[0].late_games),
            (5, 5)
        );
        assert!(report.changes[0].evidence.contains("First 6 vs last 6"));
        assert!(report.changes[0].evidence.contains("5/6"));
        rows[1].gpm = None;
        assert!(personal_patch(&rows, 7).changes.is_empty());
    }

    #[test]
    fn insufficient_or_ambiguous_chronology_never_creates_a_change() {
        let mut rows = games(10);
        for (index, row) in rows.iter_mut().enumerate() {
            row.gpm = Some(if index < 5 { 400 } else { 600 });
        }
        assert_eq!(personal_patch(&rows, 7).changes.len(), 1);
        for invalid in [None, Some(0), Some(-1)] {
            rows[0].finished_at = invalid;
            assert!(personal_patch(&rows, 7).changes.is_empty());
        }
        for row in &mut rows {
            row.finished_at = Some(1);
        }
        assert!(personal_patch(&rows, 7).changes.is_empty());
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
        assert_eq!(personal_patch(&rows, 7), PersonalPatch::default());
        rows[0].stuns = Some(f64::INFINITY);
        assert_eq!(personal_patch(&rows, 7), PersonalPatch::default());
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
        let report = personal_patch(&rows, 7);
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
        assert_eq!(personal_patch(&rows, 7), report);
        assert!(report.awards.iter().all(|award| !award.evidence.is_empty()));
    }
}
