//! Adapter-neutral Cama Wrapped story policies.
//!
//! These pure aggregation, selection, immutable purchase-history, and layout
//! contracts are shared by the native live runtime and adapter-neutral tests.

use std::collections::{BTreeMap, BTreeSet};

include!("wrapped_story/flavor_catalog.rs");

pub const SLIDE_WIDTH: u32 = 800;
pub const SLIDE_HEIGHT: u32 = 600;
pub const MAX_AWARDS: usize = 6;

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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueryAudit {
    pub match_stats_reads: usize,
    pub hero_reads: usize,
    pub year_match_reads: usize,
    pub match_detail_reads: usize,
    pub steam_id_reads: usize,
    pub player_rating_reads: usize,
    pub aggregate_rating_reads: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorySnapshot {
    pub discord_id: i64,
    pub year: i32,
    pub guild_id: Option<i64>,
    pub match_stats: Vec<MatchStatsRow>,
    pub heroes: Vec<HeroRow>,
    pub matches: Vec<YearMatchRow>,
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

pub fn build_story_snapshot(
    discord_id: i64,
    year: i32,
    guild_id: Option<i64>,
    match_stats: Vec<MatchStatsRow>,
    heroes: Vec<HeroRow>,
    matches: Vec<YearMatchRow>,
    audit: &mut QueryAudit,
) -> StorySnapshot {
    audit.match_stats_reads += 1;
    audit.hero_reads += 1;
    audit.year_match_reads += 1;
    StorySnapshot {
        discord_id,
        year,
        guild_id,
        match_stats,
        heroes,
        matches,
    }
}

pub fn player_scoped_rating_change(value: Option<i64>, audit: &mut QueryAudit) -> i64 {
    audit.player_rating_reads += 1;
    value.unwrap_or(0)
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

pub const fn pairwise_query_scope(guild_id: Option<i64>) -> Option<i64> {
    guild_id
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

#[derive(Clone, Debug, Default)]
pub struct PackageDealLedger {
    purchases: Vec<PackageDealPurchase>,
    next_id: i64,
}

impl PackageDealLedger {
    pub fn purchase(
        &mut self,
        guild_id: i64,
        buyer_discord_id: i64,
        partner_discord_id: i64,
        games: i64,
        cost: i64,
        year: i32,
    ) -> i64 {
        self.next_id += 1;
        self.purchases.push(PackageDealPurchase {
            id: self.next_id,
            guild_id,
            buyer_discord_id,
            partner_discord_id,
            games_committed: games,
            games_remaining: games,
            jc_spent: cost,
            year,
        });
        self.next_id
    }

    pub fn decrement(&mut self, guild_id: i64, ids: &[i64]) {
        for purchase in &mut self.purchases {
            if purchase.guild_id == guild_id
                && ids.contains(&purchase.id)
                && purchase.games_remaining > 0
            {
                purchase.games_remaining -= 1;
            }
        }
    }

    pub fn active_involving(&self, guild_id: i64, player_id: i64) -> Vec<PackageDealPurchase> {
        self.purchases
            .iter()
            .filter(|purchase| {
                purchase.guild_id == guild_id
                    && purchase.games_remaining > 0
                    && (purchase.buyer_discord_id == player_id
                        || purchase.partner_discord_id == player_id)
            })
            .cloned()
            .collect()
    }

    pub fn wrapped(&self, player_id: i64, year: i32, guild_id: i64) -> Option<PackageDealWrapped> {
        package_deal_wrapped(
            player_id,
            self.purchases.iter().filter(|purchase| {
                purchase.guild_id == guild_id
                    && purchase.year == year
                    && (purchase.buyer_discord_id == player_id
                        || purchase.partner_discord_id == player_id)
            }),
        )
    }
}

pub fn package_deal_from_source(
    source: Option<&PackageDealLedger>,
    player_id: i64,
    year: i32,
    guild_id: i64,
) -> Option<PackageDealWrapped> {
    source.and_then(|ledger| ledger.wrapped(player_id, year, guild_id))
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlideSpec {
    pub width: u32,
    pub height: u32,
    pub kind: &'static str,
}

pub fn slide_spec(kind: &'static str) -> SlideSpec {
    SlideSpec {
        width: SLIDE_WIDTH,
        height: SLIDE_HEIGHT,
        kind,
    }
}

pub fn word_wrap(text: &str, max_width: usize) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let max_chars = (max_width / 6).max(1);
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() == 1 && words[0].chars().count() > max_chars {
        let keep = max_chars.saturating_sub(2);
        return vec![format!(
            "{}..",
            words[0].chars().take(keep).collect::<String>()
        )];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in words {
        let candidate_len =
            current.chars().count() + usize::from(!current.is_empty()) + word.chars().count();
        if !current.is_empty() && candidate_len > max_chars {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Award {
    pub discord_id: i64,
    pub title: String,
}

pub fn select_awards_for_viewer(awards: &[Award], viewer_id: i64, max_awards: usize) -> Vec<Award> {
    let mut selected: Vec<_> = awards
        .iter()
        .filter(|award| award.discord_id == viewer_id)
        .take(max_awards)
        .cloned()
        .collect();
    if selected.len() < max_awards {
        selected.extend(
            awards
                .iter()
                .filter(|award| award.discord_id != viewer_id)
                .take(max_awards - selected.len())
                .cloned(),
        );
    }
    selected
}

#[derive(Clone, Debug, PartialEq)]
pub struct DegenScore {
    pub total: i64,
    pub title: String,
    pub emoji: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GamblingStats {
    pub total_bets: i64,
    pub win_rate: f64,
    pub net_pnl: i64,
    pub roi: f64,
    pub degen_score: DegenScore,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WrappedGamblingData {
    pub pnl_series: Vec<(i64, i64)>,
    pub stats: GamblingStats,
    pub recalculated_degen_score: bool,
    pub snapshot_consumer_count: usize,
}

pub fn wrapped_gambling_data(
    pnl_series: Vec<(i64, i64)>,
    player_stats: GamblingStats,
) -> WrappedGamblingData {
    WrappedGamblingData {
        pnl_series,
        stats: player_stats,
        recalculated_degen_score: false,
        snapshot_consumer_count: 5,
    }
}

pub const RECORD_SLIDE_TITLES: [&str; 5] = [
    "Combat",
    "Farming",
    "Impact",
    "Vision & Utility",
    "Endurance & Streaks",
];

#[derive(Clone, Debug, PartialEq)]
pub struct PersonalRecord {
    pub stat_key: &'static str,
    pub stat_label: &'static str,
    pub value: Option<f64>,
    pub display_value: String,
    pub hero_id: Option<i64>,
    pub is_worst: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerRecordsWrapped {
    pub discord_username: String,
    pub games_played: usize,
    pub records: Vec<PersonalRecord>,
}

impl PlayerRecordsWrapped {
    pub fn get_slides(&self) -> Vec<(&'static str, Vec<&PersonalRecord>)> {
        RECORD_SLIDE_TITLES
            .into_iter()
            .map(|title| {
                let records = self
                    .records
                    .iter()
                    .filter(|record| record_slide_title(record.stat_key) == title)
                    .collect();
                (title, records)
            })
            .collect()
    }
}

fn record_slide_title(stat_key: &str) -> &'static str {
    if stat_key.starts_with("gpm") || stat_key.starts_with("xpm") {
        "Farming"
    } else if matches!(stat_key, "apm_best" | "courier_kills_best" | "pings_worst") {
        "Vision & Utility"
    } else if matches!(stat_key, "comeback_best" | "throw_worst") {
        "Impact"
    } else if stat_key == "rapiers_best" {
        "Endurance & Streaks"
    } else {
        "Combat"
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WrappedEnrichmentFacts {
    pub actions_per_min: Option<i64>,
    pub courier_kills: Option<i64>,
    pub pings: Option<i64>,
    pub rapier_count: Option<i64>,
    pub comeback: Option<i64>,
    pub throw: Option<i64>,
}

type EnrichmentRecordSpec = (
    &'static str,
    &'static str,
    fn(&WrappedEnrichmentFacts) -> Option<i64>,
    bool,
);

#[derive(Clone, Debug, PartialEq)]
pub struct WrappedRecordRow {
    pub match_id: i64,
    pub guild_id: i64,
    pub discord_id: i64,
    pub hero_id: Option<i64>,
    pub kills: i64,
    pub deaths: i64,
    pub assists: i64,
    pub gpm: i64,
    pub xpm: i64,
    pub won: Option<bool>,
    pub enrichment: Option<WrappedEnrichmentFacts>,
}

pub fn compute_streaks(
    matches: &[(Option<bool>, Option<i64>)],
) -> (usize, usize, Option<i64>, Option<i64>) {
    let mut best_win = 0;
    let mut best_loss = 0;
    let mut current_win = 0;
    let mut current_loss = 0;
    let mut best_win_end = None;
    let mut best_loss_end = None;
    for (index, (won, _)) in matches.iter().enumerate() {
        match won {
            Some(true) => {
                current_win += 1;
                current_loss = 0;
                if current_win > best_win {
                    best_win = current_win;
                    best_win_end = Some(index);
                }
            }
            Some(false) => {
                current_loss += 1;
                current_win = 0;
                if current_loss > best_loss {
                    best_loss = current_loss;
                    best_loss_end = Some(index);
                }
            }
            None => {
                current_win = 0;
                current_loss = 0;
            }
        }
    }
    let breaker = |end: Option<usize>, expected: bool| {
        end.and_then(|index| matches.get(index + 1))
            .and_then(|(won, hero)| (*won == Some(expected)).then_some(*hero).flatten())
    };
    (
        best_win,
        best_loss,
        breaker(best_win_end, false),
        breaker(best_loss_end, true),
    )
}

fn record(
    stat_key: &'static str,
    stat_label: &'static str,
    value: Option<f64>,
    display_suffix: &'static str,
    hero_id: Option<i64>,
    is_worst: bool,
) -> PersonalRecord {
    PersonalRecord {
        stat_key,
        stat_label,
        value,
        display_value: value.map_or_else(
            || "N/A".to_owned(),
            |number| {
                if number.fract() == 0.0 {
                    format!("{number:.0}{display_suffix}")
                } else {
                    format!("{number:.2}{display_suffix}")
                }
            },
        ),
        hero_id,
        is_worst,
    }
}

pub fn player_records(
    username: Option<&str>,
    rows: &[WrappedRecordRow],
    minimum_games: usize,
) -> Option<PlayerRecordsWrapped> {
    if rows.len() < minimum_games {
        return None;
    }
    let username = username?;
    let max_by = |value: fn(&WrappedRecordRow) -> f64| {
        rows.iter()
            .max_by(|left, right| value(left).total_cmp(&value(right)))
    };
    let min_by = |value: fn(&WrappedRecordRow) -> f64| {
        rows.iter()
            .min_by(|left, right| value(left).total_cmp(&value(right)))
    };
    let kda = |row: &WrappedRecordRow| (row.kills + row.assists) as f64 / row.deaths.max(1) as f64;
    let kills_best = max_by(|row| row.kills as f64).expect("nonempty rows");
    let deaths_worst = max_by(|row| row.deaths as f64).expect("nonempty rows");
    let kda_best = max_by(kda).expect("nonempty rows");
    let kda_worst = min_by(kda).expect("nonempty rows");
    let gpm_worst = min_by(|row| row.gpm as f64).expect("nonempty rows");
    let xpm_worst = min_by(|row| row.xpm as f64).expect("nonempty rows");
    let mut records = vec![
        record(
            "kills_best",
            "Most Kills",
            Some(kills_best.kills as f64),
            " kills",
            kills_best.hero_id,
            false,
        ),
        record(
            "kda_best",
            "Best KDA",
            Some(kda(kda_best)),
            " KDA",
            kda_best.hero_id,
            false,
        ),
        record(
            "deaths_worst",
            "Feeding Frenzy",
            Some(deaths_worst.deaths as f64),
            " deaths",
            deaths_worst.hero_id,
            true,
        ),
        record(
            "kda_worst",
            "Clown Fiesta",
            Some(kda(kda_worst)),
            " KDA",
            kda_worst.hero_id,
            true,
        ),
        record(
            "gpm_worst",
            "Lowest GPM",
            Some(gpm_worst.gpm as f64),
            " GPM",
            gpm_worst.hero_id,
            true,
        ),
        record(
            "xpm_worst",
            "Lowest XPM",
            Some(xpm_worst.xpm as f64),
            " XPM",
            xpm_worst.hero_id,
            true,
        ),
    ];
    let enrichment_specs: [EnrichmentRecordSpec; 6] = [
        (
            "apm_best",
            "Highest APM",
            |facts| facts.actions_per_min,
            false,
        ),
        (
            "courier_kills_best",
            "Courier Hunter",
            |facts| facts.courier_kills,
            false,
        ),
        ("pings_worst", "Most Pings", |facts| facts.pings, true),
        (
            "rapiers_best",
            "Divine Collector",
            |facts| facts.rapier_count,
            false,
        ),
        (
            "comeback_best",
            "Biggest Comeback",
            |facts| facts.comeback,
            false,
        ),
        ("throw_worst", "Biggest Throw", |facts| facts.throw, true),
    ];
    for (key, label, extract, is_worst) in enrichment_specs {
        let candidate = rows
            .iter()
            .filter_map(|row| {
                row.enrichment
                    .as_ref()
                    .and_then(extract)
                    .map(|value| (row, value))
            })
            .max_by_key(|(_, value)| *value);
        records.push(record(
            key,
            label,
            candidate.map(|(_, value)| value as f64),
            "",
            candidate.and_then(|(row, _)| row.hero_id),
            is_worst,
        ));
    }
    Some(PlayerRecordsWrapped {
        discord_username: username.to_owned(),
        games_played: rows.len(),
        records,
    })
}

#[derive(Clone, Debug, Default)]
pub struct WrappedMatchStore {
    rows: Vec<WrappedRecordRow>,
}

impl WrappedMatchStore {
    pub fn insert(&mut self, row: WrappedRecordRow) {
        self.rows.push(row);
    }

    pub fn player_year_matches(&self, discord_id: i64, guild_id: i64) -> Vec<WrappedRecordRow> {
        self.rows
            .iter()
            .filter(|row| row.discord_id == discord_id && row.guild_id == guild_id)
            .cloned()
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordSlideSpec {
    pub width: u32,
    pub height: u32,
    pub mode: &'static str,
    pub worst_records: usize,
    pub unavailable_records: usize,
}

pub fn records_slide_spec(records: &[PersonalRecord]) -> RecordSlideSpec {
    RecordSlideSpec {
        width: SLIDE_WIDTH,
        height: SLIDE_HEIGHT,
        mode: "RGBA",
        worst_records: records.iter().filter(|record| record.is_worst).count(),
        unavailable_records: records
            .iter()
            .filter(|record| record.value.is_none())
            .count(),
    }
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerWrapped;

pub const fn server_wrapped(has_match_data: bool) -> Option<ServerWrapped> {
    if has_match_data {
        Some(ServerWrapped)
    } else {
        None
    }
}

pub fn player_wrapped_for_registration(player_name: Option<&str>) -> Option<String> {
    player_name.map(str::to_owned)
}

#[cfg(test)]
mod tests;
