//! Discord-independent embed presentation models and builders.

use std::collections::{BTreeMap, BTreeSet};

use cama_domain::embed_safety::{EmbedModel, FIELD_VALUE_LIMIT, truncate_field};
use cama_domain::formatting::{JOPACOIN_EMOTE, ROLE_EMOJIS, TOMBSTONE_EMOJI};
use cama_domain::rating::CamaRatingSystem;
use cama_domain::region::{PlayerRegionInput, summarize_region};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LobbyKind {
    Open,
    LowSkill,
}

impl LobbyKind {
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Open => "All You Can Feed",
            Self::LowSkill => "Whine & Cheese",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Open => "🍽️ All You Can Feed",
            Self::LowSkill => "🧀 Whine & Cheese",
        }
    }

    const fn eligibility_text(self) -> &'static str {
        match self {
            Self::Open => "Open to all ratings",
            Self::LowSkill => "Glicko below 1400",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LobbyPlayer {
    pub discord_id: i64,
    pub name: String,
    pub glicko_rating: Option<f64>,
    pub mmr: Option<i32>,
    pub preferred_roles: Vec<u8>,
    pub guild_id: Option<i64>,
    pub preferred_region: Option<String>,
    pub inferred_region: Option<String>,
}

impl LobbyPlayer {
    #[must_use]
    pub fn new(discord_id: i64, name: impl Into<String>) -> Self {
        Self {
            discord_id,
            name: name.into(),
            glicko_rating: None,
            mmr: None,
            preferred_roles: Vec::new(),
            guild_id: None,
            preferred_region: None,
            inferred_region: None,
        }
    }
}

pub trait BankruptcyPenaltySource {
    fn get_bulk_states(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, i64>, String>;

    fn get_penalty_games(&self, discord_id: i64, guild_id: Option<i64>) -> Result<i64, String>;
}

fn penalty_games(
    source: Option<&dyn BankruptcyPenaltySource>,
    discord_id: i64,
    guild_id: Option<i64>,
) -> i64 {
    if discord_id <= 0 {
        return 0;
    }
    source
        .and_then(|source| source.get_penalty_games(discord_id, guild_id).ok())
        .unwrap_or(0)
}

fn bulk_penalty_games(
    source: Option<&dyn BankruptcyPenaltySource>,
    ids_by_guild: &BTreeMap<Option<i64>, Vec<i64>>,
) -> BTreeMap<i64, i64> {
    let Some(source) = source else {
        return BTreeMap::new();
    };

    let mut penalties = BTreeMap::new();
    for (guild_id, discord_ids) in ids_by_guild {
        match source.get_bulk_states(discord_ids, *guild_id) {
            Ok(states) => penalties.extend(states),
            Err(_) => {
                penalties.extend(discord_ids.iter().map(|discord_id| {
                    (
                        *discord_id,
                        penalty_games(Some(source), *discord_id, *guild_id),
                    )
                }));
            }
        }
    }
    penalties
}

/// Format the lobby roster by stable Discord identity rather than positional
/// row order. Missing rows retain their own placeholder slots.
#[must_use]
pub fn format_player_list(
    players: &[LobbyPlayer],
    player_ids: &[i64],
    bankruptcy_source: Option<&dyn BankruptcyPenaltySource>,
    guild_id: Option<i64>,
) -> (String, usize) {
    if players.is_empty() {
        return ("No players yet".to_owned(), 0);
    }

    let players_by_id = players
        .iter()
        .map(|player| (player.discord_id, player))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    let unique_ids = player_ids
        .iter()
        .copied()
        .filter(|discord_id| seen.insert(*discord_id))
        .collect::<Vec<_>>();

    let mut penalty_ids_by_guild: BTreeMap<Option<i64>, Vec<i64>> = BTreeMap::new();
    for discord_id in unique_ids
        .iter()
        .copied()
        .filter(|discord_id| *discord_id >= 0)
    {
        let player = players_by_id.get(&discord_id).copied();
        let lookup_guild = guild_id.or_else(|| player.and_then(|player| player.guild_id));
        penalty_ids_by_guild
            .entry(lookup_guild)
            .or_default()
            .push(discord_id);
    }
    let penalties = bulk_penalty_games(bankruptcy_source, &penalty_ids_by_guild);

    let rating_system = CamaRatingSystem::default();
    let mut rated = unique_ids
        .iter()
        .map(|discord_id| {
            let player = players_by_id.get(discord_id).copied();
            let rating = player.map_or_else(
                || rating_system.mmr_to_rating(4_000),
                |player| {
                    player
                        .glicko_rating
                        .unwrap_or_else(|| rating_system.mmr_to_rating(player.mmr.unwrap_or(4_000)))
                },
            );
            (rating, player, *discord_id)
        })
        .collect::<Vec<_>>();
    rated.sort_by(|left, right| right.0.total_cmp(&left.0));

    let lines = rated
        .into_iter()
        .enumerate()
        .map(|(index, (rating, player, discord_id))| {
            let real_user = discord_id >= 0;
            let tombstone = if real_user && penalties.get(&discord_id).copied().unwrap_or(0) > 0 {
                format!("{TOMBSTONE_EMOJI} ")
            } else {
                String::new()
            };
            let Some(player) = player else {
                let display = if real_user {
                    format!("{tombstone}<@{discord_id}>")
                } else {
                    "Unknown player".to_owned()
                };
                return format!("{}. {display} *(unregistered)*", index + 1);
            };

            let display = if real_user {
                format!("{tombstone}<@{discord_id}>")
            } else {
                player.name.clone()
            };
            let mut line = format!("{}. {display}", index + 1);
            if player.glicko_rating.is_some() {
                line.push_str(&format!(" [{}]", rating_system.rating_to_display(rating)));
            }
            let role_display = player
                .preferred_roles
                .iter()
                .filter_map(|role| {
                    ROLE_EMOJIS
                        .iter()
                        .find(|(key, _)| key.parse::<u8>().ok() == Some(*role))
                        .map(|(_, emoji)| *emoji)
                })
                .collect::<Vec<_>>()
                .join(" ");
            if !role_display.is_empty() {
                line.push(' ');
                line.push_str(&role_display);
            }
            line
        })
        .collect::<Vec<_>>();

    (lines.join("\n"), unique_ids.len())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LobbyEmbedRequest {
    pub kind: LobbyKind,
    pub created_at_epoch: Option<i64>,
    pub regular_count: usize,
    pub ready_threshold: usize,
    pub max_players: usize,
    pub guild_id: Option<i64>,
    pub bonus_pool_preview: Option<i64>,
}

impl Default for LobbyEmbedRequest {
    fn default() -> Self {
        Self {
            kind: LobbyKind::Open,
            created_at_epoch: None,
            regular_count: 0,
            ready_threshold: 10,
            max_players: 14,
            guild_id: None,
            bonus_pool_preview: None,
        }
    }
}

#[must_use]
pub fn create_lobby_embed(
    request: LobbyEmbedRequest,
    players: &[LobbyPlayer],
    player_ids: &[i64],
    bankruptcy_source: Option<&dyn BankruptcyPenaltySource>,
) -> EmbedModel {
    let timestamp = request.created_at_epoch.map_or_else(
        || "Opened just now".to_owned(),
        |epoch| format!("Opened at <t:{epoch}:t>"),
    );
    let mut embed = EmbedModel {
        title: Some(request.kind.label().to_owned()),
        description: Some(format!(
            "{}\nJoin to play!\n{timestamp}",
            request.kind.eligibility_text()
        )),
        ..EmbedModel::default()
    };
    let (player_list, _) =
        format_player_list(players, player_ids, bankruptcy_source, request.guild_id);
    embed.add_field(
        format!("**Players ({})** ⚔️", request.regular_count),
        if players.is_empty() {
            "No players yet".to_owned()
        } else {
            truncate_field(&player_list, FIELD_VALUE_LIMIT)
        },
        false,
    );
    let region_players = players
        .iter()
        .map(|player| PlayerRegionInput {
            preferred_region: player.preferred_region.as_deref(),
            inferred_region: player.inferred_region.as_deref(),
        })
        .collect::<Vec<_>>();
    embed.add_field("🌎 Lobby Region", summarize_region(&region_players), false);
    if let Some(bonus_pool) = request.bonus_pool_preview {
        embed.add_field(
            "🎲 Bonus Pool",
            format!("**{bonus_pool} {JOPACOIN_EMOTE}** available if this lobby shuffles next."),
            false,
        );
    }
    if request.regular_count >= request.ready_threshold {
        embed.add_field("✅ Ready!", "Run `/readycheck` before `/shuffle`.", false);
    } else {
        let needed = request.ready_threshold - request.regular_count;
        embed.add_field(
            "Status",
            format!(
                "🟢 Open - Need {needed} more player{}!",
                if needed > 1 { "s" } else { "" }
            ),
            false,
        );
    }
    embed.footer = Some(format!(
        "Total: {}/{}",
        request.regular_count, request.max_players
    ));
    embed
}

#[must_use]
pub fn format_number(number: Option<i64>) -> String {
    match number {
        None => "—".to_owned(),
        Some(number) if number >= 1_000 => format!("{:.1}k", number as f64 / 1_000.0),
        Some(number) => number.to_string(),
    }
}

#[must_use]
pub fn format_duration(seconds: Option<i64>) -> String {
    match seconds {
        Some(seconds) if seconds > 0 => format!("{}:{:02}", seconds / 60, seconds % 60),
        _ => "—".to_owned(),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MatchParticipant {
    pub discord_id: Option<i64>,
    pub guild_id: Option<i64>,
    pub hero_id: Option<i64>,
    /// Runtime-resolved bundled/Dotabase name. Older callers may omit this and
    /// retain the small compatibility fallback below.
    pub hero_name: Option<String>,
    pub kills: Option<i64>,
    pub deaths: Option<i64>,
    pub assists: Option<i64>,
    pub hero_damage: Option<i64>,
    pub net_worth: Option<i64>,
    pub lane_role: Option<i64>,
    pub lane_efficiency: Option<i64>,
}

fn hero_name(hero_id: i64) -> &'static str {
    match hero_id {
        1 => "Anti-Mage",
        2 => "Axe",
        _ => "Unknown",
    }
}

fn lane_outcomes(
    radiant: &[MatchParticipant],
    dire: &[MatchParticipant],
) -> BTreeMap<(bool, usize), char> {
    fn lanes(participants: &[MatchParticipant]) -> BTreeMap<i64, Vec<(usize, i64)>> {
        let mut lanes: BTreeMap<i64, Vec<(usize, i64)>> = BTreeMap::new();
        for (index, participant) in participants.iter().enumerate() {
            if let (Some(lane @ 1..=3), Some(efficiency)) =
                (participant.lane_role, participant.lane_efficiency)
            {
                lanes.entry(lane).or_default().push((index, efficiency));
            }
        }
        lanes
    }
    fn average(players: &[(usize, i64)]) -> f64 {
        players.iter().map(|(_, value)| *value as f64).sum::<f64>() / players.len() as f64
    }

    let radiant_lanes = lanes(radiant);
    let dire_lanes = lanes(dire);
    let mut outcomes = BTreeMap::new();
    for (radiant_lane, dire_lane) in [(1, 3), (2, 2), (3, 1)] {
        let (Some(radiant_players), Some(dire_players)) =
            (radiant_lanes.get(&radiant_lane), dire_lanes.get(&dire_lane))
        else {
            continue;
        };
        let radiant_average = average(radiant_players);
        let dire_average = average(dire_players);
        let (radiant_outcome, dire_outcome) = if radiant_average > dire_average + 5.0 {
            ('W', 'L')
        } else if dire_average > radiant_average + 5.0 {
            ('L', 'W')
        } else {
            ('D', 'D')
        };
        outcomes.extend(
            radiant_players
                .iter()
                .map(|(index, _)| ((true, *index), radiant_outcome)),
        );
        outcomes.extend(
            dire_players
                .iter()
                .map(|(index, _)| ((false, *index), dire_outcome)),
        );
    }
    outcomes
}

fn lane_name(lane_role: i64) -> &'static str {
    match lane_role {
        0 => "Roam",
        1 => "Safe",
        2 => "Mid",
        3 => "Off",
        4 => "Jgl",
        _ => "",
    }
}

fn format_team_field(
    participants: &[MatchParticipant],
    radiant: bool,
    outcomes: &BTreeMap<(bool, usize), char>,
    bankruptcy_source: Option<&dyn BankruptcyPenaltySource>,
    guild_id: Option<i64>,
) -> String {
    if participants.is_empty() {
        return "No data".to_owned();
    }
    participants
        .iter()
        .enumerate()
        .map(|(index, participant)| {
            let fallback_hero = participant.hero_id.unwrap_or(0);
            let hero = participant
                .hero_name
                .as_deref()
                .unwrap_or_else(|| hero_name(fallback_hero));
            let kills = participant.kills.unwrap_or(0);
            let deaths = participant.deaths.unwrap_or(0);
            let assists = participant.assists.unwrap_or(0);
            let damage = format_number(participant.hero_damage);
            let net_worth = format_number(participant.net_worth);
            let lane = participant.lane_role.map_or_else(String::new, |lane_role| {
                let name = lane_name(lane_role);
                outcomes
                    .get(&(radiant, index))
                    .map_or_else(|| name.to_owned(), |outcome| format!("{name} {outcome}"))
            });
            let tombstone = participant
                .discord_id
                .map_or_else(String::new, |discord_id| {
                    let lookup_guild = guild_id.or(participant.guild_id);
                    if penalty_games(bankruptcy_source, discord_id, lookup_guild) > 0 {
                        format!("{TOMBSTONE_EMOJI} ")
                    } else {
                        String::new()
                    }
                });
            let player = participant.discord_id.filter(|id| *id > 0).map_or_else(
                || "?".to_owned(),
                |discord_id| format!("{tombstone}<@{discord_id}>"),
            );
            let mut stats = vec![format!("`{kills}/{deaths}/{assists}`")];
            if damage != "—" {
                stats.push(format!("`{damage} dmg`"));
            }
            if net_worth != "—" {
                stats.push(format!("`{net_worth} nw`"));
            }
            if !lane.is_empty() {
                stats.push(format!("`{lane}`"));
            }
            format!("{player} **{hero}** {}", stats.join(" "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnrichedMatchRequest {
    pub match_id: i64,
    pub valve_match_id: Option<i64>,
    pub duration_seconds: Option<i64>,
    pub radiant_score: Option<i64>,
    pub dire_score: Option<i64>,
    pub winning_team: i64,
    pub show_mvp: bool,
    pub draft: bool,
    pub guild_id: Option<i64>,
}

#[must_use]
pub fn create_enriched_match_embed(
    request: &EnrichedMatchRequest,
    radiant: &[MatchParticipant],
    dire: &[MatchParticipant],
    bankruptcy_source: Option<&dyn BankruptcyPenaltySource>,
) -> EmbedModel {
    let winner = if request.winning_team == 1 {
        "Radiant"
    } else {
        "Dire"
    };
    let mut title = format!(
        "{} Match #{} - {winner} Victory",
        if request.draft { "👑" } else { "🎲" },
        request.match_id
    );
    let duration = format_duration(request.duration_seconds);
    if duration != "—" {
        title.push_str(&format!(" ({duration})"));
    }
    let mut description = Vec::new();
    if let (Some(radiant_score), Some(dire_score)) = (request.radiant_score, request.dire_score) {
        description.push(format!("**Score:** {radiant_score} - {dire_score}"));
    }
    if let Some(match_id) = request.valve_match_id {
        description.push(format!(
            "[OpenDota](https://www.opendota.com/matches/{match_id}) | [DotaBuff](https://www.dotabuff.com/matches/{match_id}) | [STRATZ](https://stratz.com/matches/{match_id})"
        ));
    }
    let mut embed = EmbedModel {
        title: Some(title),
        description: (!description.is_empty()).then(|| description.join("\n")),
        ..EmbedModel::default()
    };
    let outcomes = lane_outcomes(radiant, dire);
    embed.add_field(
        format!(
            "🟢 RADIANT{}",
            if request.winning_team == 1 {
                " (Winner)"
            } else {
                ""
            }
        ),
        truncate_field(
            &format_team_field(
                radiant,
                true,
                &outcomes,
                bankruptcy_source,
                request.guild_id,
            ),
            FIELD_VALUE_LIMIT,
        ),
        false,
    );
    embed.add_field(
        format!(
            "🔴 DIRE{}",
            if request.winning_team == 2 {
                " (Winner)"
            } else {
                ""
            }
        ),
        truncate_field(
            &format_team_field(dire, false, &outcomes, bankruptcy_source, request.guild_id),
            FIELD_VALUE_LIMIT,
        ),
        false,
    );
    let winning_players = if request.winning_team == 1 {
        radiant
    } else {
        dire
    };
    if request.show_mvp
        && let Some(mvp) = winning_players
            .iter()
            .max_by_key(|participant| participant.hero_damage.unwrap_or(0))
        && let Some(hero_id) = mvp.hero_id.filter(|hero_id| *hero_id != 0)
    {
        embed.footer = Some(format!(
            "MVP: {} ({} damage)",
            mvp.hero_name
                .as_deref()
                .unwrap_or_else(|| hero_name(hero_id)),
            format_number(mvp.hero_damage)
        ));
    }
    embed
}

#[must_use]
pub fn create_match_summary_embed(
    match_id: i64,
    winning_team: i64,
    radiant: &[MatchParticipant],
    dire: &[MatchParticipant],
    valve_match_id: Option<i64>,
) -> EmbedModel {
    create_match_summary_embed_with_context(
        match_id,
        winning_team,
        radiant,
        dire,
        valve_match_id,
        None,
        "shuffle",
        None,
    )
}

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn create_match_summary_embed_with_context(
    match_id: i64,
    winning_team: i64,
    radiant: &[MatchParticipant],
    dire: &[MatchParticipant],
    valve_match_id: Option<i64>,
    bankruptcy_source: Option<&dyn BankruptcyPenaltySource>,
    lobby_type: &str,
    guild_id: Option<i64>,
) -> EmbedModel {
    fn simple_team(
        participants: &[MatchParticipant],
        bankruptcy_source: Option<&dyn BankruptcyPenaltySource>,
        guild_id: Option<i64>,
    ) -> String {
        if participants.is_empty() {
            return "No data".to_owned();
        }
        participants
            .iter()
            .map(|participant| {
                let tombstone = participant
                    .discord_id
                    .map_or_else(String::new, |discord_id| {
                        let lookup_guild = guild_id.or(participant.guild_id);
                        if penalty_games(bankruptcy_source, discord_id, lookup_guild) > 0 {
                            format!("{TOMBSTONE_EMOJI} ")
                        } else {
                            String::new()
                        }
                    });
                match participant.hero_id.filter(|hero_id| *hero_id != 0) {
                    Some(hero_id) => format!(
                        "{tombstone}**{}** ({}/{}/{})",
                        participant
                            .hero_name
                            .as_deref()
                            .unwrap_or_else(|| hero_name(hero_id)),
                        participant.kills.unwrap_or(0),
                        participant.deaths.unwrap_or(0),
                        participant.assists.unwrap_or(0)
                    ),
                    None => format!("{tombstone}<@{}>", participant.discord_id.unwrap_or(0)),
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    let winner = if winning_team == 1 { "Radiant" } else { "Dire" };
    let lobby_emoji = if lobby_type == "draft" {
        "👑"
    } else {
        "🎲"
    };
    let mut embed = EmbedModel {
        title: Some(format!(
            "{lobby_emoji} Match #{match_id} - {winner} Victory"
        )),
        ..EmbedModel::default()
    };
    embed.add_field(
        format!(
            "🟢 Radiant{}",
            if winning_team == 1 { " (Winner)" } else { "" }
        ),
        simple_team(radiant, bankruptcy_source, guild_id),
        true,
    );
    embed.add_field(
        format!(
            "🔴 Dire{}",
            if winning_team == 2 { " (Winner)" } else { "" }
        ),
        simple_team(dire, bankruptcy_source, guild_id),
        true,
    );
    if let Some(valve_match_id) = valve_match_id {
        embed.add_field(
            "Links",
            format!(
                "[OpenDota](https://www.opendota.com/matches/{valve_match_id}) | [DotaBuff](https://www.dotabuff.com/matches/{valve_match_id})"
            ),
            false,
        );
    }
    embed
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use cama_domain::embed_safety::validate_embed;

    use super::*;

    fn field<'a>(
        embed: &'a EmbedModel,
        name_fragment: &str,
    ) -> &'a cama_domain::embed_safety::EmbedField {
        embed
            .fields
            .iter()
            .find(|field| field.name.contains(name_fragment))
            .expect("expected embed field")
    }

    fn participant(hero_id: i64) -> MatchParticipant {
        MatchParticipant {
            hero_id: Some(hero_id),
            ..MatchParticipant::default()
        }
    }

    fn enriched_request(match_id: i64, winning_team: i64) -> EnrichedMatchRequest {
        EnrichedMatchRequest {
            match_id,
            valve_match_id: None,
            duration_seconds: None,
            radiant_score: None,
            dire_score: None,
            winning_team,
            show_mvp: true,
            draft: false,
            guild_id: None,
        }
    }

    #[test]
    fn test_lobby_embed_uses_public_lowskill_identity() {
        let embed = create_lobby_embed(
            LobbyEmbedRequest {
                kind: LobbyKind::LowSkill,
                ..LobbyEmbedRequest::default()
            },
            &[],
            &[],
            None,
        );
        assert_eq!(embed.title.as_deref(), Some("🧀 Whine & Cheese"));
        assert!(
            embed
                .description
                .as_deref()
                .is_some_and(|description| description.starts_with("Glicko below 1400\n"))
        );
    }

    #[test]
    fn test_lobby_embed_uses_public_open_identity() {
        let embed = create_lobby_embed(LobbyEmbedRequest::default(), &[], &[], None);
        assert_eq!(embed.title.as_deref(), Some("🍽️ All You Can Feed"));
        assert!(
            embed
                .description
                .as_deref()
                .is_some_and(|description| description.starts_with("Open to all ratings\n"))
        );
    }

    #[test]
    fn test_lobby_embed_shows_supplied_bonus_pool_preview_even_when_empty() {
        let embed = create_lobby_embed(
            LobbyEmbedRequest {
                bonus_pool_preview: Some(0),
                ..LobbyEmbedRequest::default()
            },
            &[],
            &[],
            None,
        );
        assert_eq!(
            field(&embed, "🎲 Bonus Pool").value,
            "**0 <:jopacoin:954159801049440297>** available if this lobby shuffles next."
        );
    }

    #[test]
    fn test_lobby_embeds_do_not_expose_captain_eligibility() {
        let embed = create_lobby_embed(LobbyEmbedRequest::default(), &[], &[], None);
        let rendered = format!("{embed:?}").to_lowercase();
        assert!(!rendered.contains("captain"));
    }

    #[test]
    fn test_lobby_embed_uses_discord_timestamp_format() {
        let epoch = 1_767_380_200;
        let embed = create_lobby_embed(
            LobbyEmbedRequest {
                created_at_epoch: Some(epoch),
                regular_count: 5,
                ..LobbyEmbedRequest::default()
            },
            &[],
            &[],
            None,
        );
        let description = embed.description.expect("lobby description");
        assert!(description.contains(&format!("<t:{epoch}:t>")));
        assert!(description.contains("Opened at"));
    }

    #[test]
    fn test_lobby_embed_no_created_at_fallback() {
        let embed = create_lobby_embed(LobbyEmbedRequest::default(), &[], &[], None);
        assert!(
            embed
                .description
                .as_deref()
                .is_some_and(|description| description.contains("Opened just now"))
        );
    }

    #[test]
    fn test_lobby_embed_timestamp_is_unix_epoch() {
        let epoch = 1_800_000_000;
        let embed = create_lobby_embed(
            LobbyEmbedRequest {
                created_at_epoch: Some(epoch),
                regular_count: 3,
                ..LobbyEmbedRequest::default()
            },
            &[],
            &[],
            None,
        );
        assert!(epoch > 1_577_836_800);
        assert!(
            embed
                .description
                .as_deref()
                .is_some_and(|description| description.contains("<t:1800000000:t>"))
        );
    }

    #[test]
    fn test_lobby_embed_defaults_region_to_us_west() {
        let mut player = LobbyPlayer::new(1, "Player");
        player.mmr = Some(3_000);
        let embed = create_lobby_embed(
            LobbyEmbedRequest {
                created_at_epoch: Some(1_800_000_000),
                regular_count: 1,
                ..LobbyEmbedRequest::default()
            },
            &[player],
            &[1],
            None,
        );
        assert_eq!(field(&embed, "🌎 Lobby Region").value, "US West");
    }

    #[test]
    fn test_format_number_small() {
        assert_eq!(format_number(Some(500)), "500");
        assert_eq!(format_number(Some(0)), "0");
        assert_eq!(format_number(Some(999)), "999");
    }

    #[test]
    fn test_format_number_large() {
        assert_eq!(format_number(Some(1_000)), "1.0k");
        assert_eq!(format_number(Some(45_123)), "45.1k");
        assert_eq!(format_number(Some(100_000)), "100.0k");
    }

    #[test]
    fn test_format_number_none() {
        assert_eq!(format_number(None), "—");
    }

    #[test]
    fn test_format_duration_valid() {
        assert_eq!(format_duration(Some(1_985)), "33:05");
        assert_eq!(format_duration(Some(3_600)), "60:00");
        assert_eq!(format_duration(Some(90)), "1:30");
        assert_eq!(format_duration(Some(59)), "0:59");
    }

    #[test]
    fn test_format_duration_invalid() {
        assert_eq!(format_duration(None), "—");
        assert_eq!(format_duration(Some(0)), "—");
        assert_eq!(format_duration(Some(-100)), "—");
    }

    #[test]
    fn test_creates_embed_with_radiant_victory() {
        let mut radiant = participant(1);
        radiant.kills = Some(10);
        radiant.deaths = Some(2);
        radiant.assists = Some(5);
        radiant.hero_damage = Some(25_000);
        radiant.net_worth = Some(18_000);
        let mut dire = participant(2);
        dire.kills = Some(3);
        dire.deaths = Some(8);
        dire.assists = Some(2);
        dire.hero_damage = Some(12_000);
        dire.net_worth = Some(10_000);
        let embed = create_enriched_match_embed(
            &EnrichedMatchRequest {
                valve_match_id: Some(7_500_000_000),
                duration_seconds: Some(1_985),
                radiant_score: Some(45),
                dire_score: Some(32),
                ..enriched_request(123, 1)
            },
            &[radiant],
            &[dire],
            None,
        );
        let title = embed.title.as_deref().expect("match title");
        assert!(title.contains("Match #123"));
        assert!(title.contains("Radiant Victory"));
        assert!(title.contains("33:05"));
        let description = embed.description.as_deref().expect("match description");
        assert!(description.contains("45 - 32"));
        assert!(description.contains("opendota.com/matches/7500000000"));
        assert!(description.contains("dotabuff.com/matches/7500000000"));
        assert!(validate_embed(&embed).is_empty());
    }

    #[test]
    fn test_creates_embed_with_dire_victory() {
        let embed = create_enriched_match_embed(&enriched_request(456, 2), &[], &[], None);
        assert!(
            embed
                .title
                .as_deref()
                .is_some_and(|title| title.contains("Dire Victory"))
        );
    }

    #[test]
    fn test_embed_has_team_fields() {
        let embed = create_enriched_match_embed(
            &EnrichedMatchRequest {
                duration_seconds: Some(1_800),
                radiant_score: Some(30),
                dire_score: Some(25),
                valve_match_id: Some(123),
                ..enriched_request(789, 1)
            },
            &[participant(1)],
            &[participant(2)],
            None,
        );
        assert!(
            embed
                .fields
                .iter()
                .any(|field| field.name.contains("RADIANT"))
        );
        assert!(embed.fields.iter().any(|field| field.name.contains("DIRE")));
        assert!(
            embed
                .fields
                .iter()
                .any(|field| field.name.contains("Winner"))
        );
    }

    #[test]
    fn test_lane_won_lost_shown_in_embed() {
        let mut radiant = participant(1);
        radiant.lane_role = Some(2);
        radiant.lane_efficiency = Some(72);
        let mut dire = participant(2);
        dire.lane_role = Some(2);
        dire.lane_efficiency = Some(55);
        let embed =
            create_enriched_match_embed(&enriched_request(123, 1), &[radiant], &[dire], None);
        assert!(field(&embed, "RADIANT").value.contains("Mid W"));
        assert!(field(&embed, "DIRE").value.contains("Mid L"));
    }

    #[test]
    fn test_lane_draw_when_close_efficiency() {
        let mut radiant = participant(1);
        radiant.lane_role = Some(2);
        radiant.lane_efficiency = Some(65);
        let mut dire = participant(2);
        dire.lane_role = Some(2);
        dire.lane_efficiency = Some(63);
        let embed =
            create_enriched_match_embed(&enriched_request(456, 1), &[radiant], &[dire], None);
        assert!(field(&embed, "RADIANT").value.contains("Mid D"));
        assert!(field(&embed, "DIRE").value.contains("Mid D"));
    }

    #[test]
    fn test_side_lanes_compare_correctly() {
        let mut radiant = participant(1);
        radiant.lane_role = Some(1);
        radiant.lane_efficiency = Some(75);
        let mut dire = participant(2);
        dire.lane_role = Some(3);
        dire.lane_efficiency = Some(50);
        let embed =
            create_enriched_match_embed(&enriched_request(789, 1), &[radiant], &[dire], None);
        assert!(field(&embed, "RADIANT").value.contains("Safe W"));
        assert!(field(&embed, "DIRE").value.contains("Off L"));
    }

    #[test]
    fn test_lane_role_without_efficiency() {
        let mut radiant = participant(1);
        radiant.lane_role = Some(3);
        let embed = create_enriched_match_embed(&enriched_request(456, 1), &[radiant], &[], None);
        let value = &field(&embed, "RADIANT").value;
        assert!(value.contains("Off"));
        assert!(!value.contains("Off W"));
        assert!(!value.contains("Off L"));
    }

    #[test]
    fn test_five_player_team_field_stays_within_limit() {
        let players = (0..5)
            .map(|index| MatchParticipant {
                discord_id: Some(100_000_000_000_000_000 + index),
                hero_id: Some(0),
                kills: Some(99),
                deaths: Some(99),
                assists: Some(99),
                hero_damage: Some(999_999),
                net_worth: Some(999_999),
                lane_role: Some(index % 4 + 1),
                ..MatchParticipant::default()
            })
            .collect::<Vec<_>>();
        let embed = create_enriched_match_embed(
            &EnrichedMatchRequest {
                duration_seconds: Some(3_600),
                radiant_score: Some(50),
                dire_score: Some(50),
                ..enriched_request(1, 1)
            },
            &players,
            &players,
            None,
        );
        assert!(validate_embed(&embed).is_empty());
    }

    #[test]
    fn test_creates_simple_embed() {
        let embed =
            create_match_summary_embed(100, 1, &[participant(1)], &[participant(2)], Some(123_456));
        let title = embed.title.as_deref().expect("summary title");
        assert!(title.contains("Match #100"));
        assert!(title.contains("Radiant Victory"));
    }

    #[test]
    fn test_shows_links_when_valve_match_id_present() {
        let embed = create_match_summary_embed(100, 2, &[], &[], Some(9_999_999));
        let links = embed
            .fields
            .iter()
            .filter(|field| field.name == "Links")
            .collect::<Vec<_>>();
        assert_eq!(links.len(), 1);
        assert!(links[0].value.contains("9999999"));
    }

    struct ActiveSummaryPenalty;

    impl BankruptcyPenaltySource for ActiveSummaryPenalty {
        fn get_bulk_states(
            &self,
            _discord_ids: &[i64],
            _guild_id: Option<i64>,
        ) -> Result<BTreeMap<i64, i64>, String> {
            Ok(BTreeMap::new())
        }

        fn get_penalty_games(&self, discord_id: i64, guild_id: Option<i64>) -> Result<i64, String> {
            Ok(i64::from(discord_id == 1 && guild_id == Some(77)))
        }
    }

    #[test]
    fn simple_match_context_preserves_draft_and_bankruptcy_display() {
        let mut radiant = participant(1);
        radiant.discord_id = Some(1);
        radiant.guild_id = Some(77);
        let embed = create_match_summary_embed_with_context(
            100,
            1,
            &[radiant],
            &[participant(2)],
            None,
            Some(&ActiveSummaryPenalty),
            "draft",
            Some(77),
        );
        assert!(
            embed
                .title
                .as_deref()
                .is_some_and(|title| title.starts_with('👑'))
        );
        assert!(field(&embed, "Radiant").value.starts_with(TOMBSTONE_EMOJI));
    }

    fn rated_player(discord_id: i64, rating: f64) -> LobbyPlayer {
        let mut player = LobbyPlayer::new(discord_id, format!("Player {discord_id}"));
        player.glicko_rating = Some(rating);
        player
    }

    #[test]
    fn test_missing_middle_id_does_not_shift_pairings() {
        let rating_system = CamaRatingSystem::default();
        let (text, count) = format_player_list(
            &[rated_player(1, 1_000.0), rated_player(3, 900.0)],
            &[1, 2, 3],
            None,
            None,
        );
        assert_eq!(count, 3);
        let line = |discord_id| {
            text.lines()
                .find(|line| line.contains(&format!("<@{discord_id}>")))
                .expect("player line")
        };
        assert!(line(1).contains(&format!("[{}]", rating_system.rating_to_display(1_000.0))));
        assert!(line(3).contains(&format!("[{}]", rating_system.rating_to_display(900.0))));
        assert!(line(2).contains("unregistered"));
        assert!(!line(2).contains(&format!("[{}]", rating_system.rating_to_display(900.0))));
    }

    #[test]
    fn test_missing_last_id_is_not_dropped() {
        let (text, count) = format_player_list(&[rated_player(1, 1_000.0)], &[1, 2], None, None);
        assert_eq!(count, 2);
        assert!(text.contains("<@2>"));
        assert!(text.contains("unregistered"));
    }

    #[derive(Default)]
    struct RecordingPenaltySource {
        bulk_calls: RefCell<Vec<(Vec<i64>, Option<i64>)>>,
        point_calls: Cell<usize>,
    }

    impl BankruptcyPenaltySource for RecordingPenaltySource {
        fn get_bulk_states(
            &self,
            discord_ids: &[i64],
            guild_id: Option<i64>,
        ) -> Result<BTreeMap<i64, i64>, String> {
            self.bulk_calls
                .borrow_mut()
                .push((discord_ids.to_vec(), guild_id));
            Ok(BTreeMap::from([(4, 2)]))
        }

        fn get_penalty_games(
            &self,
            _discord_id: i64,
            _guild_id: Option<i64>,
        ) -> Result<i64, String> {
            self.point_calls.set(self.point_calls.get() + 1);
            Err("point bankruptcy lookup used".to_owned())
        }
    }

    #[test]
    fn test_format_player_list_bulk_loads_bankruptcy_penalties() {
        let players = (1..15)
            .map(|discord_id| {
                let mut player = rated_player(discord_id, 1_000.0);
                player.guild_id = Some(77);
                player
            })
            .collect::<Vec<_>>();
        let ids = (1..15).collect::<Vec<_>>();
        let source = RecordingPenaltySource::default();
        let (text, count) = format_player_list(&players, &ids, Some(&source), Some(77));
        assert_eq!(count, 14);
        assert!(text.contains(&format!("{TOMBSTONE_EMOJI} <@4>")));
        assert_eq!(source.bulk_calls.borrow().as_slice(), &[(ids, Some(77))]);
        assert_eq!(source.point_calls.get(), 0);
    }

    #[derive(Default)]
    struct MutablePenaltySource {
        penalties: RefCell<BTreeMap<i64, i64>>,
        bulk_calls: RefCell<Vec<Vec<i64>>>,
    }

    impl MutablePenaltySource {
        fn set(&self, discord_id: i64, penalty_games: i64) {
            self.penalties
                .borrow_mut()
                .insert(discord_id, penalty_games);
        }
    }

    impl BankruptcyPenaltySource for MutablePenaltySource {
        fn get_bulk_states(
            &self,
            discord_ids: &[i64],
            _guild_id: Option<i64>,
        ) -> Result<BTreeMap<i64, i64>, String> {
            self.bulk_calls.borrow_mut().push(discord_ids.to_vec());
            let penalties = self.penalties.borrow();
            Ok(discord_ids
                .iter()
                .filter_map(|discord_id| {
                    penalties
                        .get(discord_id)
                        .copied()
                        .map(|penalty| (*discord_id, penalty))
                })
                .collect())
        }

        fn get_penalty_games(
            &self,
            discord_id: i64,
            _guild_id: Option<i64>,
        ) -> Result<i64, String> {
            Ok(self
                .penalties
                .borrow()
                .get(&discord_id)
                .copied()
                .unwrap_or_default())
        }
    }

    fn bankruptcy_display_player(discord_id: i64, name: &str) -> LobbyPlayer {
        let mut player = LobbyPlayer::new(discord_id, name);
        player.guild_id = Some(77);
        player
    }

    #[test]
    fn test_tombstone_not_shown_for_non_bankrupted_player() {
        let player = bankruptcy_display_player(1_001, "NormalPlayer");
        let source = MutablePenaltySource::default();
        let (text, count) = format_player_list(&[player], &[1_001], Some(&source), Some(77));
        assert_eq!(count, 1);
        assert!(!text.contains(TOMBSTONE_EMOJI));
    }

    #[test]
    fn test_tombstone_shown_for_bankrupted_player() {
        let player = bankruptcy_display_player(1_002, "BankruptPlayer");
        let source = MutablePenaltySource::default();
        source.set(1_002, 5);
        let (text, _) = format_player_list(&[player], &[1_002], Some(&source), Some(77));
        assert!(text.contains(&format!("{TOMBSTONE_EMOJI} <@1002>")));
    }

    #[test]
    fn test_tombstone_disappears_after_penalty_games() {
        let player = bankruptcy_display_player(1_003, "RecoveringPlayer");
        let source = MutablePenaltySource::default();
        source.set(1_003, 1);
        let (active, _) = format_player_list(
            std::slice::from_ref(&player),
            &[1_003],
            Some(&source),
            Some(77),
        );
        assert!(active.contains(TOMBSTONE_EMOJI));
        source.set(1_003, 0);
        let (recovered, _) = format_player_list(&[player], &[1_003], Some(&source), Some(77));
        assert!(!recovered.contains(TOMBSTONE_EMOJI));
    }

    #[test]
    fn test_tombstone_in_lobby_player_list() {
        let players = [
            bankruptcy_display_player(1_004, "NormalPlayer"),
            bankruptcy_display_player(1_005, "BankruptPlayer"),
        ];
        let source = MutablePenaltySource::default();
        source.set(1_005, 5);
        let (text, count) = format_player_list(&players, &[1_004, 1_005], Some(&source), Some(77));
        assert_eq!(count, 2);
        assert!(text.contains(&format!("{TOMBSTONE_EMOJI} <@1005>")));
        assert!(!text.contains(&format!("{TOMBSTONE_EMOJI} <@1004>")));
    }

    #[test]
    fn test_tombstone_in_lobby_embed() {
        let players = [
            bankruptcy_display_player(1_010, "NormalPlayer"),
            bankruptcy_display_player(1_011, "BankruptPlayer"),
        ];
        let source = MutablePenaltySource::default();
        source.set(1_011, 5);
        let embed = create_lobby_embed(
            LobbyEmbedRequest {
                regular_count: 2,
                guild_id: Some(77),
                ..LobbyEmbedRequest::default()
            },
            &players,
            &[1_010, 1_011],
            Some(&source),
        );
        let roster = embed
            .fields
            .iter()
            .find(|field| field.name.contains("Players"))
            .expect("lobby player field");
        assert!(roster.value.contains(&format!("{TOMBSTONE_EMOJI} <@1011>")));
    }

    #[test]
    fn test_tombstone_in_match_summary_uses_participant_guild() {
        let mut radiant = participant(1);
        radiant.discord_id = Some(1_016);
        radiant.guild_id = Some(77);
        radiant.hero_id = None;
        radiant.hero_name = None;
        let source = MutablePenaltySource::default();
        source.set(1_016, 5);
        let embed = create_match_summary_embed_with_context(
            1,
            1,
            &[radiant],
            &[],
            None,
            Some(&source),
            "shuffle",
            None,
        );
        assert!(
            field(&embed, "Radiant")
                .value
                .contains(&format!("{TOMBSTONE_EMOJI} <@1016>"))
        );
    }

    #[test]
    fn test_fake_users_excluded_from_bankruptcy_check() {
        let player = bankruptcy_display_player(-1, "FakeUser");
        let source = MutablePenaltySource::default();
        source.set(-1, 5);
        let (text, count) = format_player_list(&[player], &[-1], Some(&source), Some(77));
        assert_eq!(count, 1);
        assert!(text.contains("FakeUser"));
        assert!(!text.contains(TOMBSTONE_EMOJI));
        assert!(source.bulk_calls.borrow().is_empty());
    }

    #[test]
    fn test_multiple_bankrupted_players_in_lobby() {
        let players = (2_000..2_005)
            .map(|discord_id| bankruptcy_display_player(discord_id, "Player"))
            .collect::<Vec<_>>();
        let source = MutablePenaltySource::default();
        source.set(2_001, 5);
        source.set(2_003, 5);
        let ids = (2_000..2_005).collect::<Vec<_>>();
        let (text, count) = format_player_list(&players, &ids, Some(&source), Some(77));
        assert_eq!(count, 5);
        assert!(text.contains(&format!("{TOMBSTONE_EMOJI} <@2001>")));
        assert!(text.contains(&format!("{TOMBSTONE_EMOJI} <@2003>")));
    }

    #[test]
    fn test_graceful_handling_of_missing_bankruptcy_repo() {
        let player = bankruptcy_display_player(3_000, "TestPlayer");
        let (text, count) = format_player_list(&[player], &[3_000], None, Some(77));
        assert_eq!(count, 1);
        assert!(text.contains("<@3000>"));
        assert!(!text.contains(TOMBSTONE_EMOJI));
    }

    #[test]
    fn test_tombstone_constant_defined() {
        assert_eq!(TOMBSTONE_EMOJI, "🪦");
        assert!(!TOMBSTONE_EMOJI.is_empty());
    }
}
