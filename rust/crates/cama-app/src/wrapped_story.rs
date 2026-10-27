//! Adapter-neutral Cama Wrapped story policies.
//!
//! These pure aggregation, selection, immutable purchase-history, and layout
//! contracts are shared by the native live runtime and adapter-neutral tests.

use std::collections::{BTreeMap, BTreeSet};

include!("wrapped_story/flavor_catalog.rs");

pub fn flavor_pool(key: &str) -> Option<&'static [&'static str]> {
    FLAVOR_POOLS
        .iter()
        .find_map(|(candidate, pool)| (*candidate == key).then_some(*pool))
}

pub fn format_flavor(template: &str, variables: &[(&str, &str)]) -> String {
    let mut output = template.to_owned();
    let mut cursor = 0;
    while let Some(relative_start) = output[cursor..].find('{') {
        let start = cursor + relative_start;
        let Some(relative_end) = output[start + 1..].find('}') else {
            return template.to_owned();
        };
        let end = start + 1 + relative_end;
        let key = &output[start + 1..end];
        let Some((_, value)) = variables.iter().find(|(name, _)| *name == key) else {
            return template.to_owned();
        };
        output.replace_range(start..=end, value);
        cursor = start + value.len();
    }
    output
}

pub fn get_flavor(key: &str, choice_index: usize, variables: &[(&str, &str)]) -> String {
    flavor_pool(key)
        .and_then(|pool| pool.get(choice_index % pool.len()))
        .map_or_else(String::new, |template| format_flavor(template, variables))
}

pub fn compute_percentile(query: f64, values: &[f64]) -> f64 {
    if values.is_empty() {
        return 50.0;
    }
    let below = values.iter().filter(|value| **value < query).count() as f64;
    let equal = values.iter().filter(|value| **value == query).count() as f64;
    ((below + equal * 0.5) / values.len() as f64) * 100.0
}

#[derive(Clone, Debug, PartialEq)]
pub struct PersonalSummaryWrapped {
    pub discord_id: i64,
    pub discord_username: String,
    pub games_played: i64,
    pub wins: i64,
    pub losses: i64,
    pub win_rate: f64,
    pub rating_change: i64,
    pub total_kills: i64,
    pub total_deaths: i64,
    pub total_assists: i64,
    pub avg_game_duration: i64,
    pub unique_heroes: usize,
    pub games_played_percentile: f64,
    pub win_rate_percentile: f64,
    pub kda_percentile: f64,
    pub unique_heroes_percentile: f64,
    pub total_kda_percentile: f64,
    pub flavor_text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PairwiseEntry {
    pub discord_id: i64,
    pub username: String,
    pub games: i64,
    pub wins: i64,
    pub win_rate: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PairwiseWrapped {
    pub best_teammates: Vec<PairwiseEntry>,
    pub most_played_with: Vec<PairwiseEntry>,
    pub nemesis: Option<PairwiseEntry>,
    pub punching_bag: Option<PairwiseEntry>,
    pub most_played_against: Vec<PairwiseEntry>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageDealWrapped {
    pub times_bought: usize,
    pub times_bought_on_you: usize,
    pub unique_buyers: usize,
    pub jc_spent: i64,
    pub jc_spent_on_you: i64,
    pub total_games_committed: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeroSpotlightWrapped {
    pub top_hero_name: String,
    pub top_hero_picks: i64,
    pub top_hero_wins: i64,
    pub top_hero_win_rate: f64,
    pub top_3_heroes: Vec<HeroSpotlightEntry>,
    pub unique_heroes: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HeroSpotlightEntry {
    pub name: String,
    pub picks: i64,
    pub wins: i64,
    pub win_rate: f64,
    pub kda: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoleBreakdownWrapped {
    pub lane_freq: BTreeMap<i32, usize>,
    pub total_games: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchDetails {
    pub games_played: i64,
    pub wins: i64,
    pub losses: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchStatsRow {
    pub discord_id: i64,
    pub games_played: i64,
    pub wins: i64,
    pub total_kills: i64,
    pub total_deaths: i64,
    pub total_assists: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeroRow {
    pub discord_id: i64,
    pub hero_id: i64,
    pub picks: i64,
    pub wins: i64,
    pub total_kills: i64,
    pub total_deaths: i64,
    pub total_assists: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YearMatchRow {
    pub won: Option<bool>,
    pub duration_seconds: Option<i64>,
    pub enrichment_present: bool,
    pub lane_role: Option<i32>,
}

#[derive(Clone, Debug, Default)]
pub struct PersonalSummaryInput {
    pub discord_id: i64,
    pub player_name: Option<String>,
    pub match_details: Option<MatchDetails>,
    pub rating_change: Option<i64>,
    pub match_stats: Vec<MatchStatsRow>,
    pub heroes: Vec<HeroRow>,
    pub matches: Vec<YearMatchRow>,
}

pub fn personal_summary(input: &PersonalSummaryInput) -> Option<PersonalSummaryWrapped> {
    let player_name = input.player_name.clone()?;
    let details = input.match_details.as_ref()?;
    let own_stats = input
        .match_stats
        .iter()
        .find(|row| row.discord_id == input.discord_id);
    let total_kills = own_stats.map_or(0, |row| row.total_kills);
    let total_deaths = own_stats.map_or(0, |row| row.total_deaths);
    let total_assists = own_stats.map_or(0, |row| row.total_assists);
    let unique_heroes = input
        .heroes
        .iter()
        .filter(|row| row.discord_id == input.discord_id)
        .count();
    let durations: Vec<i64> = input
        .matches
        .iter()
        .filter_map(|row| row.duration_seconds.filter(|duration| *duration != 0))
        .collect();
    let avg_game_duration = if durations.is_empty() {
        0
    } else {
        durations.iter().sum::<i64>() / durations.len() as i64
    };
    let all_games: Vec<f64> = input
        .match_stats
        .iter()
        .map(|row| row.games_played as f64)
        .collect();
    let all_win_rates: Vec<f64> = input
        .match_stats
        .iter()
        .filter(|row| row.games_played > 0)
        .map(|row| row.wins as f64 / row.games_played as f64)
        .collect();
    let all_kdas: Vec<f64> = input
        .match_stats
        .iter()
        .filter(|row| row.games_played > 0)
        .map(|row| (row.total_kills + row.total_assists) as f64 / row.total_deaths.max(1) as f64)
        .collect();
    let hero_counts: BTreeMap<i64, usize> =
        input
            .heroes
            .iter()
            .fold(BTreeMap::new(), |mut counts, row| {
                *counts.entry(row.discord_id).or_default() += 1;
                counts
            });
    let all_unique_heroes: Vec<f64> = input
        .match_stats
        .iter()
        .map(|row| *hero_counts.get(&row.discord_id).unwrap_or(&0) as f64)
        .collect();
    let all_total_kda: Vec<f64> = input
        .match_stats
        .iter()
        .map(|row| (row.total_kills + row.total_assists) as f64)
        .collect();
    let win_rate = if details.games_played > 0 {
        details.wins as f64 / details.games_played as f64
    } else {
        0.0
    };
    let kda = (total_kills + total_assists) as f64 / total_deaths.max(1) as f64;
    let games_percentile = compute_percentile(details.games_played as f64, &all_games);
    let flavor_key = if games_percentile >= 75.0 {
        "games_played_high"
    } else if games_percentile >= 30.0 {
        "games_played_mid"
    } else {
        "games_played_low"
    };

    Some(PersonalSummaryWrapped {
        discord_id: input.discord_id,
        discord_username: player_name,
        games_played: details.games_played,
        wins: details.wins,
        losses: details.losses,
        win_rate,
        rating_change: input.rating_change.unwrap_or(0),
        total_kills,
        total_deaths,
        total_assists,
        avg_game_duration,
        unique_heroes,
        games_played_percentile: games_percentile,
        win_rate_percentile: compute_percentile(win_rate, &all_win_rates),
        kda_percentile: compute_percentile(kda, &all_kdas),
        unique_heroes_percentile: compute_percentile(unique_heroes as f64, &all_unique_heroes),
        total_kda_percentile: compute_percentile(
            (total_kills + total_assists) as f64,
            &all_total_kda,
        ),
        flavor_text: get_flavor(flavor_key, 0, &[]),
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct PairwiseRaw {
    pub peer_id: i64,
    pub games: i64,
    pub wins: i64,
    pub win_rate: f64,
}

#[derive(Clone, Debug, Default)]
pub struct PairwiseInput {
    pub best_teammates: Vec<PairwiseRaw>,
    pub most_played_with: Vec<PairwiseRaw>,
    pub worst_matchups: Vec<PairwiseRaw>,
    pub best_matchups: Vec<PairwiseRaw>,
    pub most_played_against: Vec<PairwiseRaw>,
}

pub fn pairwise_player_ids(input: &PairwiseInput) -> Vec<i64> {
    let mut seen = BTreeSet::new();
    input
        .best_teammates
        .iter()
        .chain(&input.most_played_with)
        .chain(&input.worst_matchups)
        .chain(&input.best_matchups)
        .chain(&input.most_played_against)
        .filter_map(|entry| seen.insert(entry.peer_id).then_some(entry.peer_id))
        .collect()
}

fn resolve_pairwise(rows: &[PairwiseRaw], names: &BTreeMap<i64, String>) -> Vec<PairwiseEntry> {
    rows.iter()
        .map(|row| PairwiseEntry {
            discord_id: row.peer_id,
            username: names
                .get(&row.peer_id)
                .cloned()
                .unwrap_or_else(|| row.peer_id.to_string()),
            games: row.games,
            wins: row.wins,
            win_rate: row.win_rate,
        })
        .collect()
}

pub fn pairwise_wrapped(
    input: Option<&PairwiseInput>,
    names: &BTreeMap<i64, String>,
) -> Option<PairwiseWrapped> {
    let input = input?;
    if input.best_teammates.is_empty()
        && input.most_played_with.is_empty()
        && input.worst_matchups.is_empty()
        && input.best_matchups.is_empty()
        && input.most_played_against.is_empty()
    {
        return None;
    }
    Some(PairwiseWrapped {
        best_teammates: resolve_pairwise(&input.best_teammates, names),
        most_played_with: resolve_pairwise(&input.most_played_with, names),
        nemesis: resolve_pairwise(&input.worst_matchups, names)
            .into_iter()
            .next(),
        punching_bag: resolve_pairwise(&input.best_matchups, names)
            .into_iter()
            .next(),
        most_played_against: resolve_pairwise(&input.most_played_against, names),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageDealPurchase {
    pub id: i64,
    pub guild_id: i64,
    pub buyer_discord_id: i64,
    pub partner_discord_id: i64,
    pub games_committed: i64,
    pub games_remaining: i64,
    pub jc_spent: i64,
    pub year: i32,
}

pub fn package_deal_wrapped<'a>(
    player_id: i64,
    purchases: impl Iterator<Item = &'a PackageDealPurchase>,
) -> Option<PackageDealWrapped> {
    let purchases: Vec<_> = purchases.collect();
    if purchases.is_empty() {
        return None;
    }
    let mut result = PackageDealWrapped::default();
    let mut buyers = BTreeSet::new();
    for purchase in purchases {
        result.total_games_committed += purchase.games_committed;
        if purchase.buyer_discord_id == player_id {
            result.times_bought += 1;
            result.jc_spent += purchase.jc_spent;
        }
        if purchase.partner_discord_id == player_id {
            result.times_bought_on_you += 1;
            result.jc_spent_on_you += purchase.jc_spent;
            buyers.insert(purchase.buyer_discord_id);
        }
    }
    result.unique_buyers = buyers.len();
    Some(result)
}

pub fn hero_spotlight(
    player_id: i64,
    heroes: &[HeroRow],
    hero_name: impl Fn(i64) -> Option<String>,
) -> Option<HeroSpotlightWrapped> {
    let mut own: Vec<_> = heroes
        .iter()
        .filter(|hero| hero.discord_id == player_id)
        .cloned()
        .collect();
    own.sort_by_key(|hero| std::cmp::Reverse(hero.picks));
    let top = own.first()?;
    let entries = own
        .iter()
        .take(3)
        .map(|hero| HeroSpotlightEntry {
            name: hero_name(hero.hero_id).unwrap_or_else(|| format!("Hero #{}", hero.hero_id)),
            picks: hero.picks,
            wins: hero.wins,
            win_rate: if hero.picks > 0 {
                hero.wins as f64 / hero.picks as f64
            } else {
                0.0
            },
            kda: (hero.total_kills + hero.total_assists) as f64 / hero.total_deaths.max(1) as f64,
        })
        .collect();
    Some(HeroSpotlightWrapped {
        top_hero_name: hero_name(top.hero_id).unwrap_or_else(|| format!("Hero #{}", top.hero_id)),
        top_hero_picks: top.picks,
        top_hero_wins: top.wins,
        top_hero_win_rate: if top.picks > 0 {
            top.wins as f64 / top.picks as f64
        } else {
            0.0
        },
        top_3_heroes: entries,
        unique_heroes: own.len(),
    })
}

pub fn role_breakdown(matches: &[YearMatchRow]) -> Option<RoleBreakdownWrapped> {
    if matches.is_empty() {
        return None;
    }
    let mut lane_freq = BTreeMap::new();
    for row in matches {
        if row.enrichment_present && matches!(row.lane_role, Some(1..=3)) {
            *lane_freq
                .entry(row.lane_role.expect("checked lane"))
                .or_default() += 1;
        }
    }
    Some(RoleBreakdownWrapped {
        total_games: lane_freq.values().sum(),
        lane_freq,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct DegenScore {
    pub total: i64,
    pub title: String,
    pub emoji: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrappedAward {
    pub category: String,
    pub title: String,
    pub stat_name: String,
    pub stat_value: String,
    pub discord_id: i64,
    pub discord_username: String,
    pub emoji: String,
    pub flavor_text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WrappedAwardStats {
    pub discord_id: i64,
    pub discord_username: String,
    pub games_played: i64,
    pub avg_gpm: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WrappedRatingChange {
    pub discord_id: i64,
    pub discord_username: String,
    pub rating_change: f64,
}

const fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let adjusted_year = year - if month <= 2 { 1 } else { 0 };
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = month as i32 + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i32 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    (era * 146_097 + day_of_era - 719_468) as i64
}

pub const fn year_timestamps(year: i32) -> (i64, i64) {
    (
        days_from_civil(year, 1, 1) * 86_400,
        days_from_civil(year + 1, 1, 1) * 86_400,
    )
}

fn award_from_stats(
    stats: &WrappedAwardStats,
    category: &str,
    title: &str,
    stat_name: &str,
    stat_value: String,
    emoji: &str,
) -> WrappedAward {
    WrappedAward {
        category: category.to_owned(),
        title: title.to_owned(),
        stat_name: stat_name.to_owned(),
        stat_value,
        discord_id: stats.discord_id,
        discord_username: stats.discord_username.clone(),
        emoji: emoji.to_owned(),
        flavor_text: String::new(),
    }
}

pub fn generate_awards(
    match_stats: &[WrappedAwardStats],
    rating_changes: &[WrappedRatingChange],
) -> Vec<WrappedAward> {
    let mut awards = Vec::new();
    if let Some(stats) = match_stats
        .iter()
        .max_by(|left, right| left.avg_gpm.total_cmp(&right.avg_gpm))
    {
        awards.push(award_from_stats(
            stats,
            "performance",
            "Gold Goblin",
            "Best GPM",
            format!("{:.0} avg", stats.avg_gpm),
            "💰",
        ));
    }
    if let Some(stats) = match_stats.iter().max_by_key(|stats| stats.games_played) {
        awards.push(award_from_stats(
            stats,
            "fun",
            "No Life",
            "Most Games",
            stats.games_played.to_string(),
            "🕰️",
        ));
    }
    if let Some(change) = rating_changes
        .iter()
        .filter(|change| change.rating_change > 0.0)
        .max_by(|left, right| left.rating_change.total_cmp(&right.rating_change))
    {
        awards.push(WrappedAward {
            category: "rating".to_owned(),
            title: "Elo Inflation".to_owned(),
            stat_name: "Rating Gained".to_owned(),
            stat_value: format!("+{:.0}", change.rating_change),
            discord_id: change.discord_id,
            discord_username: change.discord_username.clone(),
            emoji: "📈".to_owned(),
            flavor_text: String::new(),
        });
    }
    if let Some(change) = rating_changes
        .iter()
        .filter(|change| change.rating_change < 0.0)
        .min_by(|left, right| left.rating_change.total_cmp(&right.rating_change))
    {
        awards.push(WrappedAward {
            category: "rating".to_owned(),
            title: "The Cliff".to_owned(),
            stat_name: "Rating Lost".to_owned(),
            stat_value: format!("{:.0}", change.rating_change),
            discord_id: change.discord_id,
            discord_username: change.discord_username.clone(),
            emoji: "📉".to_owned(),
            flavor_text: String::new(),
        });
    }
    awards
}

#[cfg(test)]
#[path = "wrapped_story/tests.rs"]
mod tests;
