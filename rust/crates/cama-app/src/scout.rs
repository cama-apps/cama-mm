//! Adapter-neutral Scout aggregation and Discord presentation policy.
//!
//! This module owns no gateway connection and starts no Discord client. The
//! The native production renderer lives in [`media`] and preserves the same
//! mobile report layout without requiring Python or Pillow at runtime.

use std::collections::{BTreeMap, BTreeSet};

use cama_db::scout_repository::ScoutDataPort;

use crate::embeds::LobbyKind;

#[path = "scout/media.rs"]
pub mod media;

pub const EMBED_FIELD_LIMIT: usize = 1_024;
pub const AMBIGUOUS_LOBBY_MESSAGE: &str = "❓ You're queued in both 🍽️ All You Can Feed and 🧀 Whine & Cheese — add the `lobby` option to pick one.";
pub const NO_PLAYERS_MESSAGE: &str =
    "No players found. Use `@mentions` or specify a `team` (radiant/dire) during an active match.";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScoutHero {
    pub hero_id: i64,
    pub games: i64,
    pub wins: i64,
    pub losses: i64,
    pub bans: i64,
    pub primary_role: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScoutData {
    pub player_count: usize,
    /// Python omits this key for the special empty-input response.
    pub total_matches: Option<i64>,
    pub heroes: Vec<ScoutHero>,
}

#[derive(Clone, Debug)]
pub struct ScoutService<P> {
    port: P,
}

impl<P> ScoutService<P> {
    pub const fn new(port: P) -> Self {
        Self { port }
    }
}

impl<P: ScoutDataPort> ScoutService<P> {
    pub fn get_scout_data(
        &self,
        player_ids: &[i64],
        guild_id: Option<i64>,
        limit: usize,
    ) -> Result<ScoutData, P::Error> {
        if player_ids.is_empty() {
            return Ok(ScoutData {
                player_count: 0,
                total_matches: None,
                heroes: Vec::new(),
            });
        }

        let player_stats = self.port.player_hero_stats(player_ids, guild_id)?;
        let bans = self.port.opposing_bans(player_ids, guild_id)?;
        let total_matches = self.port.match_count(player_ids, guild_id)?;

        #[derive(Default)]
        struct Aggregate {
            games: i64,
            wins: i64,
            losses: i64,
            roles: Vec<i64>,
        }

        let mut aggregate: BTreeMap<i64, Aggregate> = BTreeMap::new();
        for heroes in player_stats.values() {
            for hero in heroes {
                let combined = aggregate.entry(hero.hero_id).or_default();
                combined.games += hero.games;
                combined.wins += hero.wins;
                combined.losses += hero.losses;
                if hero.primary_role != 0 {
                    combined.roles.push(hero.primary_role);
                }
            }
        }

        let mut heroes = aggregate
            .into_iter()
            .map(|(hero_id, aggregate)| ScoutHero {
                hero_id,
                games: aggregate.games,
                wins: aggregate.wins,
                losses: aggregate.losses,
                bans: bans.get(&hero_id).copied().unwrap_or(0),
                primary_role: mode_or_default(&aggregate.roles, 1),
            })
            .collect::<Vec<_>>();
        heroes.sort_by(|left, right| {
            let left_total = left.games + left.bans;
            let right_total = right.games + right.bans;
            right_total
                .cmp(&left_total)
                .then_with(|| left.hero_id.cmp(&right.hero_id))
        });
        heroes.truncate(limit);

        Ok(ScoutData {
            player_count: player_ids.len(),
            total_matches: Some(total_matches),
            heroes,
        })
    }
}

fn mode_or_default(values: &[i64], default: i64) -> i64 {
    let mut counts = BTreeMap::new();
    for value in values {
        *counts.entry(*value).or_insert(0_usize) += 1;
    }
    counts
        .into_iter()
        .max_by(|(left_value, left_count), (right_value, right_count)| {
            left_count
                .cmp(right_count)
                .then_with(|| right_value.cmp(left_value))
        })
        .map_or(default, |(value, _)| value)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScoutReportInput {
    pub scout_data: ScoutData,
    pub player_names: Vec<String>,
    pub title: String,
}

pub trait ScoutImageRenderer {
    type Error;

    fn render(&self, report: &ScoutReportInput) -> Result<Vec<u8>, Self::Error>;
}

pub fn draw_scout_report<R: ScoutImageRenderer>(
    renderer: &R,
    report: &ScoutReportInput,
) -> Result<Vec<u8>, R::Error> {
    renderer.render(report)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Teams {
    pub radiant: Vec<i64>,
    pub dire: Vec<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DraftContext {
    pub teams: Teams,
    pub player_pool: Vec<i64>,
    pub lobby_kind: LobbyKind,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ContextSources {
    pub invoking_pending_match: Option<Teams>,
    pub guild_pending_match: Option<Teams>,
    pub draft: Option<DraftContext>,
    pub open_lobby: Vec<i64>,
    pub low_skill_lobby: Vec<i64>,
    pub invoking_user_lobbies: Vec<LobbyKind>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TeamContext {
    pub radiant: Vec<i64>,
    pub dire: Vec<i64>,
    pub flat: Vec<i64>,
    pub source_label: Option<String>,
    pub split: bool,
    pub filtered_prefix: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextError {
    AmbiguousLobby,
}

fn split_context(teams: &Teams, label: &str, filtered_prefix: &str) -> Option<TeamContext> {
    if teams.radiant.is_empty() && teams.dire.is_empty() {
        return None;
    }
    Some(TeamContext {
        radiant: teams.radiant.clone(),
        dire: teams.dire.clone(),
        flat: teams.radiant.iter().chain(&teams.dire).copied().collect(),
        source_label: Some(label.to_owned()),
        split: true,
        filtered_prefix: filtered_prefix.to_owned(),
    })
}

pub fn resolve_team_context(
    sources: &ContextSources,
    has_invoking_user: bool,
    explicit_lobby: Option<LobbyKind>,
) -> Result<TeamContext, ContextError> {
    let candidate_kinds = if let Some(kind) = explicit_lobby {
        vec![kind]
    } else if has_invoking_user {
        sources.invoking_user_lobbies.clone()
    } else {
        Vec::new()
    };

    let first_pending = if has_invoking_user {
        sources.invoking_pending_match.as_ref()
    } else {
        sources.guild_pending_match.as_ref()
    };
    if let Some(context) = first_pending.and_then(|teams| split_context(teams, "Active Match", ""))
    {
        return Ok(context);
    }

    if let Some(draft) = &sources.draft {
        let matches_invoker =
            candidate_kinds.is_empty() || candidate_kinds.contains(&draft.lobby_kind);
        if matches_invoker {
            if let Some(context) = split_context(&draft.teams, "Draft", "Draft ") {
                return Ok(context);
            }
            if !draft.player_pool.is_empty() {
                return Ok(TeamContext {
                    flat: draft.player_pool.clone(),
                    source_label: Some("Draft Pool".to_owned()),
                    ..TeamContext::default()
                });
            }
        }
    }

    if candidate_kinds.len() > 1 {
        return Err(ContextError::AmbiguousLobby);
    }
    let resolved_kind = candidate_kinds.first().copied().unwrap_or(LobbyKind::Open);
    let players = match resolved_kind {
        LobbyKind::Open => &sources.open_lobby,
        LobbyKind::LowSkill => &sources.low_skill_lobby,
    };
    if !players.is_empty() {
        return Ok(TeamContext {
            flat: players.clone(),
            source_label: Some(resolved_kind.label().to_owned()),
            ..TeamContext::default()
        });
    }

    if let Some(context) = sources
        .guild_pending_match
        .as_ref()
        .and_then(|teams| split_context(teams, "Active Match", ""))
    {
        return Ok(context);
    }
    Ok(TeamContext::default())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TeamFilter {
    Radiant,
    Dire,
}

pub fn resolve_player_context(
    sources: &ContextSources,
    has_invoking_user: bool,
    team_filter: Option<TeamFilter>,
    explicit_lobby: Option<LobbyKind>,
) -> Result<(Vec<i64>, Option<String>), ContextError> {
    let context = resolve_team_context(sources, has_invoking_user, explicit_lobby)?;
    if context.flat.is_empty() {
        return Ok((Vec::new(), None));
    }
    if context.split {
        match team_filter {
            Some(TeamFilter::Radiant) => {
                return Ok((
                    context.radiant,
                    Some(format!("{}Radiant", context.filtered_prefix)),
                ));
            }
            Some(TeamFilter::Dire) => {
                return Ok((
                    context.dire,
                    Some(format!("{}Dire", context.filtered_prefix)),
                ));
            }
            None => {}
        }
    }
    Ok((context.flat, context.source_label))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScoutEmbedField {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScoutEmbed {
    pub title: String,
    pub fields: Vec<ScoutEmbedField>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerProfile {
    pub discord_id: i64,
    pub name: String,
}

#[must_use]
pub fn build_link_lines(
    discord_ids: &[i64],
    name_map: &BTreeMap<i64, String>,
    steam_map: &BTreeMap<i64, Vec<i64>>,
) -> Vec<String> {
    discord_ids
        .iter()
        .map(|discord_id| {
            let Some(name) = name_map.get(discord_id) else {
                return format!("<@{discord_id}> — not registered in this server");
            };
            let Some(steam_ids) = steam_map.get(discord_id).filter(|ids| !ids.is_empty()) else {
                return format!("**{name}** — no linked Steam account");
            };
            let links = steam_ids
                .iter()
                .enumerate()
                .map(|(index, steam_id)| {
                    let label = if steam_ids.len() == 1 {
                        "Dotabuff".to_owned()
                    } else {
                        format!("Dotabuff {}", index + 1)
                    };
                    format!("[{label}](https://www.dotabuff.com/players/{steam_id})")
                })
                .collect::<Vec<_>>()
                .join(" · ");
            format!("**{name}** — {links}")
        })
        .collect()
}

pub fn add_player_fields(embed: &mut ScoutEmbed, name: &str, lines: &[String]) {
    if lines.is_empty() {
        embed.fields.push(ScoutEmbedField {
            name: name.to_owned(),
            value: "—".to_owned(),
        });
        return;
    }

    let mut chunks = vec![Vec::<String>::new()];
    for line in lines {
        let current = chunks.last_mut().expect("initial field chunk");
        let joined_len = current.iter().map(|item| item.len() + 1).sum::<usize>() + line.len();
        if !current.is_empty() && joined_len > EMBED_FIELD_LIMIT {
            chunks.push(vec![line.clone()]);
        } else {
            current.push(line.clone());
        }
    }
    for (index, chunk) in chunks.into_iter().enumerate() {
        embed.fields.push(ScoutEmbedField {
            name: if index == 0 {
                name.to_owned()
            } else {
                format!("{name} ({})", index + 1)
            },
            // A single line longer than the limit is emitted alone, so the
            // chunk still needs truncating -- the domain helper this loop
            // mirrors guards the same way.
            value: cama_domain::embed_safety::truncate_field(&chunk.join("\n"), EMBED_FIELD_LIMIT),
        });
    }
}

#[must_use]
pub fn parse_mentions(text: &str) -> Vec<i64> {
    let bytes = text.as_bytes();
    let mut ids = Vec::new();
    let mut index = 0;
    while index + 3 < bytes.len() {
        if bytes[index] != b'<' || bytes[index + 1] != b'@' {
            index += 1;
            continue;
        }
        let mut digit_start = index + 2;
        if bytes[digit_start] == b'!' {
            digit_start += 1;
        }
        let mut end = digit_start;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end > digit_start && end < bytes.len() && bytes[end] == b'>' {
            if let Ok(id) = text[digit_start..end].parse() {
                ids.push(id);
            }
            index = end + 1;
        } else {
            index += 1;
        }
    }
    ids
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportPlan {
    pub player_ids: Vec<i64>,
    pub title: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReportOutcome {
    Message(String),
    Render(ReportPlan),
}

pub struct ReportRequest<'a> {
    pub sources: &'a ContextSources,
    pub has_invoking_user: bool,
    pub mentions: Option<&'a str>,
    pub team_filter: Option<TeamFilter>,
    pub explicit_lobby: Option<LobbyKind>,
    pub has_hero_data: bool,
}

#[must_use]
pub fn plan_report(request: &ReportRequest<'_>) -> ReportOutcome {
    let mentioned = request.mentions.map(parse_mentions).unwrap_or_default();
    let mentioned = stable_unique(&mentioned);
    let resolution = if mentioned.is_empty() {
        resolve_player_context(
            request.sources,
            request.has_invoking_user,
            request.team_filter,
            request.explicit_lobby,
        )
    } else {
        let count = mentioned.len();
        Ok((
            mentioned,
            Some(format!(
                "{count} Player{}",
                if count == 1 { "" } else { "s" }
            )),
        ))
    };
    let (player_ids, source_label) = match resolution {
        Ok(value) => value,
        Err(ContextError::AmbiguousLobby) => {
            return ReportOutcome::Message(AMBIGUOUS_LOBBY_MESSAGE.to_owned());
        }
    };
    if player_ids.is_empty() {
        return ReportOutcome::Message(NO_PLAYERS_MESSAGE.to_owned());
    }
    if !request.has_hero_data {
        return ReportOutcome::Message(
            "No enriched match data found for these players. Matches need to be enriched with `/enrich match` or `/enrich discover` first.".to_owned(),
        );
    }
    ReportOutcome::Render(ReportPlan {
        player_ids,
        title: source_label.map_or_else(
            || "SCOUT REPORT".to_owned(),
            |label| format!("SCOUT: {label}"),
        ),
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LinksOutcome {
    Message(String),
    Embed(ScoutEmbed),
}

pub struct LinksRequest<'a> {
    pub sources: &'a ContextSources,
    pub has_invoking_user: bool,
    pub mentions: Option<&'a str>,
    pub team_filter: Option<TeamFilter>,
    pub explicit_lobby: Option<LobbyKind>,
    pub profiles: &'a [PlayerProfile],
    pub steam_ids: &'a BTreeMap<i64, Vec<i64>>,
}

#[must_use]
pub fn plan_links(request: &LinksRequest<'_>) -> LinksOutcome {
    let mentioned = request.mentions.map(parse_mentions).unwrap_or_default();
    let mentioned = stable_unique(&mentioned);

    let mut radiant = Vec::new();
    let mut dire = Vec::new();
    let mut flat = Vec::new();
    let source_label;
    let mut two_teams = false;
    if !mentioned.is_empty() {
        let count = mentioned.len();
        flat = mentioned;
        source_label = Some(format!(
            "{count} Player{}",
            if count == 1 { "" } else { "s" }
        ));
    } else {
        let context = match resolve_team_context(
            request.sources,
            request.has_invoking_user,
            request.explicit_lobby,
        ) {
            Ok(context) => context,
            Err(ContextError::AmbiguousLobby) => {
                return LinksOutcome::Message(AMBIGUOUS_LOBBY_MESSAGE.to_owned());
            }
        };
        if context.split {
            match request.team_filter {
                Some(TeamFilter::Radiant) => {
                    flat = context.radiant;
                    source_label = Some(format!("{}Radiant", context.filtered_prefix));
                }
                Some(TeamFilter::Dire) => {
                    flat = context.dire;
                    source_label = Some(format!("{}Dire", context.filtered_prefix));
                }
                None => {
                    radiant = context.radiant;
                    dire = context.dire;
                    source_label = context.source_label;
                    two_teams = true;
                }
            }
        } else {
            flat = context.flat;
            source_label = context.source_label;
        }
    }
    if flat.is_empty() && radiant.is_empty() && dire.is_empty() {
        return LinksOutcome::Message(NO_PLAYERS_MESSAGE.to_owned());
    }

    let name_map = request
        .profiles
        .iter()
        .map(|profile| (profile.discord_id, profile.name.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut embed = ScoutEmbed {
        title: source_label.map_or_else(
            || "Dotabuff Links".to_owned(),
            |label| format!("Dotabuff Links — {label}"),
        ),
        fields: Vec::new(),
    };
    if two_teams {
        add_player_fields(
            &mut embed,
            "Radiant",
            &build_link_lines(&radiant, &name_map, request.steam_ids),
        );
        add_player_fields(
            &mut embed,
            "Dire",
            &build_link_lines(&dire, &name_map, request.steam_ids),
        );
    } else {
        add_player_fields(
            &mut embed,
            "Players",
            &build_link_lines(&flat, &name_map, request.steam_ids),
        );
    }
    LinksOutcome::Embed(embed)
}

fn stable_unique(values: &[i64]) -> Vec<i64> {
    let mut seen = BTreeSet::new();
    values
        .iter()
        .copied()
        .filter(|value| seen.insert(*value))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use cama_db::scout_repository::HeroStat;

    use super::*;

    #[derive(Clone, Debug, Default)]
    struct MemoryScoutPort {
        stats: BTreeMap<i64, Vec<HeroStat>>,
        bans: BTreeMap<i64, i64>,
        match_count: i64,
    }

    impl ScoutDataPort for MemoryScoutPort {
        type Error = Infallible;

        fn player_hero_stats(
            &self,
            _discord_ids: &[i64],
            _guild_id: Option<i64>,
        ) -> Result<BTreeMap<i64, Vec<HeroStat>>, Self::Error> {
            Ok(self.stats.clone())
        }

        fn opposing_bans(
            &self,
            _discord_ids: &[i64],
            _guild_id: Option<i64>,
        ) -> Result<BTreeMap<i64, i64>, Self::Error> {
            Ok(self.bans.clone())
        }

        fn match_count(
            &self,
            _discord_ids: &[i64],
            _guild_id: Option<i64>,
        ) -> Result<i64, Self::Error> {
            Ok(self.match_count)
        }
    }

    fn hero(hero_id: i64, games: i64, wins: i64, role: i64) -> HeroStat {
        HeroStat {
            hero_id,
            games,
            wins,
            losses: games - wins,
            primary_role: role,
        }
    }

    fn active_match(radiant: &[i64], dire: &[i64]) -> Teams {
        Teams {
            radiant: radiant.to_vec(),
            dire: dire.to_vec(),
        }
    }

    fn profile(discord_id: i64, name: &str) -> PlayerProfile {
        PlayerProfile {
            discord_id,
            name: name.to_owned(),
        }
    }

    fn expect_report(outcome: ReportOutcome) -> ReportPlan {
        match outcome {
            ReportOutcome::Render(plan) => plan,
            ReportOutcome::Message(message) => panic!("unexpected message: {message}"),
        }
    }

    fn expect_embed(outcome: LinksOutcome) -> ScoutEmbed {
        match outcome {
            LinksOutcome::Embed(embed) => embed,
            LinksOutcome::Message(message) => panic!("unexpected message: {message}"),
        }
    }

    #[test]
    fn test_get_scout_data_empty_list() {
        let result = ScoutService::new(MemoryScoutPort::default())
            .get_scout_data(&[], Some(42), 10)
            .expect("empty scout data");
        assert_eq!(
            result,
            ScoutData {
                player_count: 0,
                total_matches: None,
                heroes: Vec::new(),
            }
        );
    }

    #[test]
    fn test_get_scout_data_includes_total_matches() {
        let port = MemoryScoutPort {
            stats: BTreeMap::from([(100, vec![hero(1, 3, 3, 1)])]),
            match_count: 3,
            ..MemoryScoutPort::default()
        };
        let result = ScoutService::new(port)
            .get_scout_data(&[100], Some(42), 10)
            .expect("scout data");
        assert_eq!(result.total_matches, Some(3));
    }

    #[test]
    fn test_get_scout_data_sorts_by_total() {
        let port = MemoryScoutPort {
            stats: BTreeMap::from([(100, vec![hero(1, 5, 5, 1), hero(2, 1, 1, 2)])]),
            bans: BTreeMap::from([(2, 3)]),
            match_count: 6,
        };
        let result = ScoutService::new(port)
            .get_scout_data(&[100], Some(42), 10)
            .expect("sorted scout data");
        assert_eq!(result.heroes[0].hero_id, 1);
        assert_eq!(result.heroes[1].hero_id, 2);
        assert_eq!(result.heroes[1].bans, 3);
    }

    #[test]
    fn test_get_scout_data_aggregation() {
        let port = MemoryScoutPort {
            stats: BTreeMap::from([(100, vec![hero(1, 1, 1, 1)]), (200, vec![hero(1, 1, 1, 1)])]),
            match_count: 2,
            ..MemoryScoutPort::default()
        };
        let result = ScoutService::new(port)
            .get_scout_data(&[100, 200], Some(42), 10)
            .expect("aggregated scout data");
        assert_eq!(result.player_count, 2);
        assert_eq!(result.heroes[0].games, 2);
    }

    #[test]
    fn test_get_scout_data_limit() {
        let port = MemoryScoutPort {
            stats: BTreeMap::from([(
                100,
                (1..=15).map(|hero_id| hero(hero_id, 1, 1, 1)).collect(),
            )]),
            match_count: 15,
            ..MemoryScoutPort::default()
        };
        let result = ScoutService::new(port)
            .get_scout_data(&[100], Some(42), 5)
            .expect("limited scout data");
        assert_eq!(result.heroes.len(), 5);
    }

    #[test]
    fn test_get_scout_data_includes_bans() {
        let port = MemoryScoutPort {
            stats: BTreeMap::from([(100, vec![hero(1, 1, 1, 1)])]),
            bans: BTreeMap::from([(1, 1)]),
            match_count: 1,
        };
        let result = ScoutService::new(port)
            .get_scout_data(&[100], Some(42), 10)
            .expect("scout data with bans");
        assert_eq!(result.heroes[0].hero_id, 1);
        assert_eq!(result.heroes[0].bans, 1);
    }

    struct PngRenderer;

    impl ScoutImageRenderer for PngRenderer {
        type Error = Infallible;

        fn render(&self, _report: &ScoutReportInput) -> Result<Vec<u8>, Self::Error> {
            Ok(b"\x89PNG\r\n\x1a\n".to_vec())
        }
    }

    #[test]
    fn test_draw_scout_report_empty_data() {
        let report = ScoutReportInput {
            scout_data: ScoutData {
                player_count: 0,
                total_matches: Some(0),
                heroes: Vec::new(),
            },
            player_names: Vec::new(),
            title: "Test Scout".to_owned(),
        };
        let result = draw_scout_report(&PngRenderer, &report).expect("render scout report");
        assert!(result.starts_with(b"\x89PNG"));
    }

    #[test]
    fn test_draw_scout_report_with_data() {
        let report = ScoutReportInput {
            scout_data: ScoutData {
                player_count: 2,
                total_matches: Some(15),
                heroes: vec![ScoutHero {
                    hero_id: 1,
                    games: 10,
                    wins: 7,
                    losses: 3,
                    bans: 2,
                    primary_role: 1,
                }],
            },
            player_names: vec!["Player1".to_owned(), "Player2".to_owned()],
            title: "SCOUT: Radiant".to_owned(),
        };
        let result = draw_scout_report(&PngRenderer, &report).expect("render scout report");
        assert!(result.starts_with(b"\x89PNG"));
    }

    #[test]
    fn test_no_context_returns_empty() {
        let context =
            resolve_team_context(&ContextSources::default(), false, None).expect("empty context");
        assert_eq!(context, TeamContext::default());
    }

    #[test]
    fn test_lobby_has_no_team_split() {
        let sources = ContextSources {
            open_lobby: vec![5, 6, 7],
            ..ContextSources::default()
        };
        let context = resolve_team_context(&sources, false, None).expect("lobby context");
        assert!(!context.split);
        assert_eq!(
            context.flat.into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([5, 6, 7])
        );
        assert_eq!(
            context.source_label.as_deref(),
            Some(LobbyKind::Open.label())
        );
    }

    #[test]
    fn test_lobby_ignores_legacy_conditional_players() {
        let sources = ContextSources {
            open_lobby: vec![5, 6],
            ..ContextSources::default()
        };
        let context = resolve_team_context(&sources, false, None).expect("lobby context");
        assert_eq!(
            context.flat.iter().copied().collect::<BTreeSet<_>>(),
            BTreeSet::from([5, 6])
        );
        assert!(!context.flat.contains(&9));
    }

    #[test]
    fn test_pending_shuffle_splits_teams() {
        let sources = ContextSources {
            guild_pending_match: Some(active_match(&[1, 2, 3], &[4, 5, 6])),
            ..ContextSources::default()
        };
        let context = resolve_team_context(&sources, false, None).expect("match context");
        assert!(context.split);
        assert_eq!(context.radiant, [1, 2, 3]);
        assert_eq!(context.dire, [4, 5, 6]);
        assert_eq!(context.flat, [1, 2, 3, 4, 5, 6]);
        assert_eq!(context.source_label.as_deref(), Some("Active Match"));
    }

    #[test]
    fn test_pending_shuffle_takes_priority_over_lobby() {
        let sources = ContextSources {
            guild_pending_match: Some(active_match(&[1, 2], &[3, 4])),
            open_lobby: vec![90, 91],
            ..ContextSources::default()
        };
        let context = resolve_team_context(&sources, false, None).expect("match context");
        assert!(context.split);
        assert_eq!(context.flat, [1, 2, 3, 4]);
    }

    #[test]
    fn test_sibling_pending_match_does_not_hide_invokers_lobby() {
        let sources = ContextSources {
            guild_pending_match: Some(active_match(&[1], &[2])),
            open_lobby: vec![1, 2],
            low_skill_lobby: vec![7, 8],
            invoking_user_lobbies: vec![LobbyKind::LowSkill],
            ..ContextSources::default()
        };
        let context = resolve_team_context(&sources, true, None).expect("invoker lobby");
        assert_eq!(
            context.flat.into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([7, 8])
        );
        assert_eq!(
            context.source_label.as_deref(),
            Some(LobbyKind::LowSkill.label())
        );
    }

    #[test]
    fn test_bystander_with_no_seat_still_sees_the_guilds_live_match() {
        let sources = ContextSources {
            guild_pending_match: Some(active_match(&[1, 2], &[3, 4])),
            ..ContextSources::default()
        };
        let context = resolve_team_context(&sources, true, None).expect("guild match fallback");
        assert!(context.split);
        assert_eq!(context.radiant, [1, 2]);
        assert_eq!(context.dire, [3, 4]);
        assert_eq!(context.source_label.as_deref(), Some("Active Match"));
    }

    #[test]
    fn test_invokers_pending_match_is_selected_when_two_are_active() {
        let sources = ContextSources {
            invoking_pending_match: Some(active_match(&[7, 9], &[8, 10])),
            ..ContextSources::default()
        };
        let context = resolve_team_context(&sources, true, None).expect("invoker match");
        assert_eq!(context.radiant, [7, 9]);
        assert_eq!(context.dire, [8, 10]);
        assert_eq!(context.source_label.as_deref(), Some("Active Match"));
    }

    #[test]
    fn test_returns_flat_list_and_label() {
        let sources = ContextSources {
            guild_pending_match: Some(active_match(&[1, 2], &[3, 4])),
            ..ContextSources::default()
        };
        assert_eq!(
            resolve_player_context(&sources, false, None, None).expect("player context"),
            (vec![1, 2, 3, 4], Some("Active Match".to_owned()))
        );
    }

    #[test]
    fn test_team_filter_narrows_to_one_team() {
        let sources = ContextSources {
            guild_pending_match: Some(active_match(&[1, 2], &[3, 4])),
            ..ContextSources::default()
        };
        assert_eq!(
            resolve_player_context(&sources, false, Some(TeamFilter::Radiant), None)
                .expect("radiant context"),
            (vec![1, 2], Some("Radiant".to_owned()))
        );
        assert_eq!(
            resolve_player_context(&sources, false, Some(TeamFilter::Dire), None)
                .expect("dire context"),
            (vec![3, 4], Some("Dire".to_owned()))
        );
    }

    #[test]
    fn test_draft_filter_keeps_draft_prefix() {
        let sources = ContextSources {
            draft: Some(DraftContext {
                teams: active_match(&[1, 2], &[3, 4]),
                player_pool: Vec::new(),
                lobby_kind: LobbyKind::Open,
            }),
            ..ContextSources::default()
        };
        assert_eq!(
            resolve_player_context(&sources, false, Some(TeamFilter::Radiant), None)
                .expect("draft radiant"),
            (vec![1, 2], Some("Draft Radiant".to_owned()))
        );
        assert_eq!(
            resolve_player_context(&sources, false, Some(TeamFilter::Dire), None)
                .expect("draft dire"),
            (vec![3, 4], Some("Draft Dire".to_owned()))
        );
    }

    #[test]
    fn test_no_context_returns_empty_tuple() {
        assert_eq!(
            resolve_player_context(&ContextSources::default(), false, None, None)
                .expect("empty player context"),
            (Vec::new(), None)
        );
    }

    #[test]
    fn test_uses_invokers_whine_and_cheese_roster_and_label() {
        let sources = ContextSources {
            open_lobby: vec![1, 2],
            low_skill_lobby: vec![7, 8],
            invoking_user_lobbies: vec![LobbyKind::LowSkill],
            ..ContextSources::default()
        };
        let plan = expect_report(plan_report(&ReportRequest {
            sources: &sources,
            has_invoking_user: true,
            mentions: None,
            team_filter: None,
            explicit_lobby: None,
            has_hero_data: true,
        }));
        assert_eq!(
            plan.player_ids.into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([7, 8])
        );
        assert_eq!(
            plan.title,
            format!("SCOUT: {}", LobbyKind::LowSkill.label())
        );
    }

    #[test]
    fn test_both_lobbies_without_a_choice_asks_which_one() {
        let sources = ContextSources {
            open_lobby: vec![1, 2],
            low_skill_lobby: vec![7, 8],
            invoking_user_lobbies: vec![LobbyKind::Open, LobbyKind::LowSkill],
            ..ContextSources::default()
        };
        assert_eq!(
            plan_report(&ReportRequest {
                sources: &sources,
                has_invoking_user: true,
                mentions: None,
                team_filter: None,
                explicit_lobby: None,
                has_hero_data: true,
            }),
            ReportOutcome::Message(AMBIGUOUS_LOBBY_MESSAGE.to_owned())
        );
    }

    #[test]
    fn test_both_lobbies_with_a_choice_scouts_that_lobby() {
        let sources = ContextSources {
            open_lobby: vec![1, 2],
            low_skill_lobby: vec![7, 8],
            invoking_user_lobbies: vec![LobbyKind::Open, LobbyKind::LowSkill],
            ..ContextSources::default()
        };
        let plan = expect_report(plan_report(&ReportRequest {
            sources: &sources,
            has_invoking_user: true,
            mentions: None,
            team_filter: None,
            explicit_lobby: Some(LobbyKind::Open),
            has_hero_data: true,
        }));
        assert_eq!(
            plan.player_ids.into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([1, 2])
        );
        assert_eq!(plan.title, format!("SCOUT: {}", LobbyKind::Open.label()));
    }

    #[test]
    fn test_pending_match_outranks_the_lobby_ambiguity() {
        let sources = ContextSources {
            invoking_pending_match: Some(active_match(&[7, 9], &[8, 10])),
            open_lobby: vec![1, 2],
            low_skill_lobby: vec![7, 8],
            invoking_user_lobbies: vec![LobbyKind::Open, LobbyKind::LowSkill],
            ..ContextSources::default()
        };
        let plan = expect_report(plan_report(&ReportRequest {
            sources: &sources,
            has_invoking_user: true,
            mentions: None,
            team_filter: None,
            explicit_lobby: None,
            has_hero_data: true,
        }));
        assert_eq!(
            plan.player_ids.into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([7, 8, 9, 10])
        );
        assert_eq!(plan.title, "SCOUT: Active Match");
    }

    #[test]
    fn test_single_account_links_to_dotabuff() {
        assert_eq!(
            build_link_lines(
                &[100],
                &BTreeMap::from([(100, "Alice".to_owned())]),
                &BTreeMap::from([(100, vec![111])]),
            ),
            ["**Alice** — [Dotabuff](https://www.dotabuff.com/players/111)"]
        );
    }

    #[test]
    fn test_multiple_accounts_are_all_listed_and_numbered() {
        assert_eq!(
            build_link_lines(
                &[200],
                &BTreeMap::from([(200, "Bob".to_owned())]),
                &BTreeMap::from([(200, vec![222, 333])]),
            ),
            [
                "**Bob** — [Dotabuff 1](https://www.dotabuff.com/players/222) · [Dotabuff 2](https://www.dotabuff.com/players/333)"
            ]
        );
    }

    #[test]
    fn test_player_without_account_is_noted() {
        assert_eq!(
            build_link_lines(
                &[300],
                &BTreeMap::from([(300, "Carol".to_owned())]),
                &BTreeMap::from([(300, Vec::new())]),
            ),
            ["**Carol** — no linked Steam account"]
        );
    }

    #[test]
    fn test_unregistered_player_is_listed_without_a_link() {
        let lines = build_link_lines(
            &[400],
            &BTreeMap::new(),
            &BTreeMap::from([(400, vec![444])]),
        );
        assert_eq!(lines, ["<@400> — not registered in this server"]);
        assert!(!lines[0].to_lowercase().contains("dotabuff"));
    }

    #[test]
    fn test_empty_lines_render_a_placeholder_field() {
        let mut embed = ScoutEmbed {
            title: String::new(),
            fields: Vec::new(),
        };
        add_player_fields(&mut embed, "Players", &[]);
        assert_eq!(embed.fields.len(), 1);
        assert_eq!(embed.fields[0].value, "—");
    }

    #[test]
    fn test_long_list_splits_across_multiple_fields() {
        let mut embed = ScoutEmbed {
            title: String::new(),
            fields: Vec::new(),
        };
        add_player_fields(&mut embed, "Players", &vec!["x".repeat(100); 30]);
        assert!(embed.fields.len() >= 3);
        assert_eq!(embed.fields[0].name, "Players");
        assert_eq!(embed.fields[1].name, "Players (2)");
        assert!(
            embed
                .fields
                .iter()
                .all(|field| field.value.len() <= EMBED_FIELD_LIMIT)
        );
        assert_eq!(
            embed
                .fields
                .iter()
                .map(|field| field.value.matches('\n').count() + 1)
                .sum::<usize>(),
            30
        );
    }

    #[test]
    fn test_no_context_reports_no_players() {
        let outcome = plan_links(&LinksRequest {
            sources: &ContextSources::default(),
            has_invoking_user: true,
            mentions: None,
            team_filter: None,
            explicit_lobby: None,
            profiles: &[],
            steam_ids: &BTreeMap::new(),
        });
        let LinksOutcome::Message(message) = outcome else {
            panic!("expected message");
        };
        assert!(message.contains("No players found"));
    }

    #[test]
    fn test_pending_shuffle_builds_two_team_embed() {
        let sources = ContextSources {
            guild_pending_match: Some(active_match(&[1, 2], &[3, 4])),
            ..ContextSources::default()
        };
        let profiles = [
            profile(1, "R1"),
            profile(2, "R2"),
            profile(3, "D1"),
            profile(4, "D2"),
        ];
        let steam_ids =
            BTreeMap::from([(1, vec![11]), (2, vec![22]), (3, vec![33]), (4, vec![44])]);
        let embed = expect_embed(plan_links(&LinksRequest {
            sources: &sources,
            has_invoking_user: false,
            mentions: None,
            team_filter: None,
            explicit_lobby: None,
            profiles: &profiles,
            steam_ids: &steam_ids,
        }));
        assert_eq!(
            embed
                .fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["Radiant", "Dire"]
        );
        assert!(embed.fields[0].value.contains("R1"));
        assert!(embed.fields[0].value.contains("players/11"));
        assert!(embed.fields[1].value.contains("players/33"));
    }

    #[test]
    fn test_explicit_mentions_list_those_players() {
        let profiles = [profile(7, "Solo")];
        let steam_ids = BTreeMap::from([(7, vec![77])]);
        let embed = expect_embed(plan_links(&LinksRequest {
            sources: &ContextSources::default(),
            has_invoking_user: false,
            mentions: Some("<@7>"),
            team_filter: None,
            explicit_lobby: None,
            profiles: &profiles,
            steam_ids: &steam_ids,
        }));
        assert_eq!(
            embed
                .fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["Players"]
        );
        assert!(embed.fields[0].value.contains("players/77"));
    }

    #[test]
    fn test_lobby_lists_players_flat() {
        let sources = ContextSources {
            open_lobby: vec![5, 6],
            ..ContextSources::default()
        };
        let profiles = [profile(5, "P5"), profile(6, "P6")];
        let steam_ids = BTreeMap::from([(5, vec![55]), (6, vec![66])]);
        let embed = expect_embed(plan_links(&LinksRequest {
            sources: &sources,
            has_invoking_user: true,
            mentions: None,
            team_filter: None,
            explicit_lobby: None,
            profiles: &profiles,
            steam_ids: &steam_ids,
        }));
        assert_eq!(
            embed
                .fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["Players"]
        );
        assert!(embed.fields[0].value.contains("players/55"));
        assert!(embed.fields[0].value.contains("players/66"));
    }

    fn dual_lobby_sources(member_kinds: Vec<LobbyKind>, pending: Option<Teams>) -> ContextSources {
        ContextSources {
            invoking_pending_match: pending,
            open_lobby: vec![1, 2],
            low_skill_lobby: vec![7, 8],
            invoking_user_lobbies: member_kinds,
            ..ContextSources::default()
        }
    }

    fn dual_lobby_profiles() -> Vec<PlayerProfile> {
        [1, 2, 7, 8, 9, 10]
            .into_iter()
            .map(|id| profile(id, &format!("P{id}")))
            .collect()
    }

    fn dual_lobby_steam() -> BTreeMap<i64, Vec<i64>> {
        [1, 2, 7, 8, 9, 10]
            .into_iter()
            .map(|id| (id, vec![id * 11]))
            .collect()
    }

    #[test]
    fn test_uses_invokers_whine_and_cheese_roster() {
        let sources = dual_lobby_sources(vec![LobbyKind::LowSkill], None);
        let embed = expect_embed(plan_links(&LinksRequest {
            sources: &sources,
            has_invoking_user: true,
            mentions: None,
            team_filter: None,
            explicit_lobby: None,
            profiles: &dual_lobby_profiles(),
            steam_ids: &dual_lobby_steam(),
        }));
        let value = &embed.fields[0].value;
        assert!(value.contains("players/77") && value.contains("players/88"));
        assert!(!value.contains("players/11"));
    }

    #[test]
    fn test_both_lobbies_without_a_choice_asks_which_one_links() {
        let sources = dual_lobby_sources(vec![LobbyKind::Open, LobbyKind::LowSkill], None);
        assert_eq!(
            plan_links(&LinksRequest {
                sources: &sources,
                has_invoking_user: true,
                mentions: None,
                team_filter: None,
                explicit_lobby: None,
                profiles: &dual_lobby_profiles(),
                steam_ids: &dual_lobby_steam(),
            }),
            LinksOutcome::Message(AMBIGUOUS_LOBBY_MESSAGE.to_owned())
        );
    }

    #[test]
    fn test_both_lobbies_with_a_choice_lists_that_lobby() {
        let sources = dual_lobby_sources(vec![LobbyKind::Open, LobbyKind::LowSkill], None);
        let embed = expect_embed(plan_links(&LinksRequest {
            sources: &sources,
            has_invoking_user: true,
            mentions: None,
            team_filter: None,
            explicit_lobby: Some(LobbyKind::Open),
            profiles: &dual_lobby_profiles(),
            steam_ids: &dual_lobby_steam(),
        }));
        let value = &embed.fields[0].value;
        assert!(value.contains("players/11") && value.contains("players/22"));
        assert!(!value.contains("players/77"));
    }

    #[test]
    fn test_pending_match_outranks_the_lobby_ambiguity_links() {
        let sources = dual_lobby_sources(
            vec![LobbyKind::Open, LobbyKind::LowSkill],
            Some(active_match(&[7, 9], &[8, 10])),
        );
        let embed = expect_embed(plan_links(&LinksRequest {
            sources: &sources,
            has_invoking_user: true,
            mentions: None,
            team_filter: None,
            explicit_lobby: None,
            profiles: &dual_lobby_profiles(),
            steam_ids: &dual_lobby_steam(),
        }));
        assert_eq!(
            embed
                .fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["Radiant", "Dire"]
        );
        assert!(embed.fields[0].value.contains("players/99"));
    }

    #[test]
    fn test_team_filter_lists_only_that_team() {
        let sources = ContextSources {
            guild_pending_match: Some(active_match(&[1, 2], &[3, 4])),
            ..ContextSources::default()
        };
        let profiles = [
            profile(1, "P1"),
            profile(2, "P2"),
            profile(3, "P3"),
            profile(4, "P4"),
        ];
        let steam_ids =
            BTreeMap::from([(1, vec![11]), (2, vec![22]), (3, vec![33]), (4, vec![44])]);
        let embed = expect_embed(plan_links(&LinksRequest {
            sources: &sources,
            has_invoking_user: false,
            mentions: None,
            team_filter: Some(TeamFilter::Radiant),
            explicit_lobby: None,
            profiles: &profiles,
            steam_ids: &steam_ids,
        }));
        assert_eq!(
            embed
                .fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["Players"]
        );
        let value = &embed.fields[0].value;
        assert!(value.contains("players/11") && value.contains("players/22"));
        assert!(!value.contains("players/33") && !value.contains("players/44"));
    }
}
