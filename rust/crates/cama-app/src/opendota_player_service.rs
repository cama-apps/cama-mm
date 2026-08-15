//! Offline OpenDota player-profile policy and orchestration.
//!
//! No concrete HTTP or Discord dependency lives here. The production runtime
//! must provide rate-limited HTTP, bundled hero metadata, player lookup, and a
//! clock through the typed ports below. Successful profile and complete Dota
//! tab results are cached for one hour; concurrent profile misses share one
//! refresh keyed by `(Discord ID, Steam ID)`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use cama_db::opendota_player::{OpenDotaPlayerRepository, OpenDotaPlayerRepositoryError};
use thiserror::Error;

use crate::dedicated_lobby_channel::UserId;
use crate::match_discovery::{SteamId, ValveMatchId};

pub const CACHE_TTL_SECONDS: i64 = 3_600;

pub trait PlayerSteamIdPort: Send + Sync {
    type Error: Display;

    fn steam_id(&self, discord_id: UserId) -> Result<Option<SteamId>, Self::Error>;
}

impl PlayerSteamIdPort for OpenDotaPlayerRepository {
    type Error = OpenDotaPlayerRepositoryError;

    fn steam_id(&self, discord_id: UserId) -> Result<Option<SteamId>, Self::Error> {
        self.get_steam_id(discord_id.0).map(|id| id.map(SteamId))
    }
}

pub trait OpenDotaPlayerClock: Send + Sync {
    fn now_seconds(&self) -> i64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemOpenDotaPlayerClock;

impl OpenDotaPlayerClock for SystemOpenDotaPlayerClock {
    fn now_seconds(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs() as i64)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Error)]
#[error("OpenDota player transport failed: {message}")]
pub struct OpenDotaPlayerPortError {
    pub message: String,
}

impl OpenDotaPlayerPortError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlayerIdentity {
    pub persona_name: Option<String>,
    pub avatar: Option<String>,
    pub rank_tier: Option<i64>,
    pub mmr_estimate: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WinLoss {
    pub wins: i64,
    pub losses: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProfileAverages {
    pub kills: f64,
    pub deaths: f64,
    pub assists: f64,
    pub gpm: i64,
    pub xpm: i64,
    pub last_hits: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TopHero {
    pub hero_id: i64,
    pub hero_name: String,
    pub games: i64,
    pub wins: i64,
    pub win_rate: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectedMatch {
    pub match_id: Option<ValveMatchId>,
    pub hero_id: Option<i64>,
    pub lane_role: Option<i64>,
    pub player_slot: Option<u16>,
    pub radiant_win: bool,
    pub kills: i64,
    pub deaths: i64,
    pub assists: i64,
    pub duration_seconds: i64,
    pub start_time: i64,
}

pub trait OpenDotaPlayerApiPort: Send + Sync {
    fn player_identity(
        &self,
        steam_id: SteamId,
    ) -> Result<Option<PlayerIdentity>, OpenDotaPlayerPortError>;

    fn win_loss(&self, steam_id: SteamId) -> Result<WinLoss, OpenDotaPlayerPortError>;

    fn profile_averages(
        &self,
        steam_id: SteamId,
    ) -> Result<ProfileAverages, OpenDotaPlayerPortError>;

    fn top_heroes(&self, steam_id: SteamId) -> Result<Vec<TopHero>, OpenDotaPlayerPortError>;

    fn projected_matches(
        &self,
        steam_id: SteamId,
        limit: usize,
    ) -> Result<Vec<ProjectedMatch>, OpenDotaPlayerPortError>;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HeroAttribute {
    Strength,
    Agility,
    Intelligence,
    #[default]
    Universal,
}

impl HeroAttribute {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Strength => "str",
            Self::Agility => "agi",
            Self::Intelligence => "int",
            Self::Universal => "all",
        }
    }

    #[must_use]
    pub fn from_bundled_name(value: &str) -> Self {
        match value {
            "strength" => Self::Strength,
            "agility" => Self::Agility,
            "intelligence" => Self::Intelligence,
            _ => Self::Universal,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeroMetadata {
    pub name: String,
    pub attribute: HeroAttribute,
    pub role_weights: BTreeMap<String, f64>,
}

pub trait OpenDotaHeroCatalog: Send + Sync {
    fn heroes(&self) -> Result<BTreeMap<i64, HeroMetadata>, OpenDotaPlayerPortError>;
}

#[derive(Clone, Debug, Default)]
pub struct RecordedHeroCatalog {
    heroes: BTreeMap<i64, HeroMetadata>,
}

impl RecordedHeroCatalog {
    #[must_use]
    pub fn new(heroes: BTreeMap<i64, HeroMetadata>) -> Self {
        Self { heroes }
    }

    #[must_use]
    pub fn from_rows(rows: &[RecordedHeroRow<'_>]) -> Self {
        let heroes = rows
            .iter()
            .map(|row| {
                let role_weights = row
                    .roles
                    .split('|')
                    .zip(row.role_levels.split('|'))
                    .filter(|(role, _)| !role.is_empty())
                    .map(|(role, level)| (role.to_owned(), level.parse().unwrap_or(1.0)))
                    .collect();
                (
                    row.id,
                    HeroMetadata {
                        name: row.name.to_owned(),
                        attribute: HeroAttribute::from_bundled_name(row.primary_attribute),
                        role_weights,
                    },
                )
            })
            .collect();
        Self { heroes }
    }
}

impl OpenDotaHeroCatalog for RecordedHeroCatalog {
    fn heroes(&self) -> Result<BTreeMap<i64, HeroMetadata>, OpenDotaPlayerPortError> {
        Ok(self.heroes.clone())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RecordedHeroRow<'a> {
    pub id: i64,
    pub name: &'a str,
    pub primary_attribute: &'a str,
    pub roles: &'a str,
    pub role_levels: &'a str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecentMatch {
    pub match_id: Option<ValveMatchId>,
    pub hero_id: Option<i64>,
    pub hero_name: String,
    pub kills: i64,
    pub deaths: i64,
    pub assists: i64,
    pub won: bool,
    pub duration_seconds: i64,
    pub start_time: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerProfile {
    pub steam_id: SteamId,
    pub persona_name: String,
    pub avatar: Option<String>,
    pub rank_tier: Option<i64>,
    pub mmr_estimate: Option<i64>,
    pub wins: i64,
    pub losses: i64,
    pub win_rate: f64,
    pub averages: ProfileAverages,
    pub top_heroes: Vec<TopHero>,
    pub recent_matches: Vec<RecentMatch>,
    pub last_match_id: Option<ValveMatchId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProfileEmbedField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProfileEmbedPlan {
    pub title: String,
    pub fields: Vec<ProfileEmbedField>,
    pub footer: String,
    pub last_match_id: Option<ValveMatchId>,
}

pub type Distribution = BTreeMap<String, f64>;

#[derive(Clone, Debug, PartialEq)]
pub struct FullStats {
    pub steam_id: SteamId,
    pub persona_name: String,
    pub rank_tier: Option<i64>,
    pub mmr_estimate: Option<i64>,
    pub total_wins: i64,
    pub total_losses: i64,
    pub total_win_rate: f64,
    pub averages: ProfileAverages,
    pub attribute_distribution: Distribution,
    pub lane_distribution: Distribution,
    pub lane_parsed_count: usize,
    pub top_heroes: Vec<TopHero>,
    pub recent_matches: Vec<RecentMatch>,
    pub recent_wins: i64,
    pub recent_losses: i64,
    pub recent_win_rate: f64,
    pub matches_analyzed: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DotaTabStats {
    pub role_distribution: Option<Distribution>,
    pub full_stats: Option<FullStats>,
}

#[derive(Clone, Debug)]
pub struct ProfileRequest {
    pub discord_id: UserId,
    pub force_refresh: bool,
    pub steam_id: Option<SteamId>,
    pub recent_matches: Option<Vec<ProjectedMatch>>,
}

impl ProfileRequest {
    #[must_use]
    pub const fn new(discord_id: UserId) -> Self {
        Self {
            discord_id,
            force_refresh: false,
            steam_id: None,
            recent_matches: None,
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum OpenDotaPlayerServiceError {
    #[error("player Steam-ID lookup failed: {0}")]
    PlayerLookup(String),
    #[error("OpenDota player cache lock was poisoned")]
    CachePoisoned,
}

#[derive(Clone, Debug)]
struct TimedProfile {
    cached_at: i64,
    profile: Arc<PlayerProfile>,
}

#[derive(Clone, Debug)]
struct TimedDotaTab {
    cached_at: i64,
    stats: Arc<DotaTabStats>,
}

#[derive(Debug, Default)]
struct CacheState {
    profiles: BTreeMap<UserId, TimedProfile>,
    profile_inflight: BTreeSet<(UserId, SteamId)>,
    dota_tabs: BTreeMap<(UserId, SteamId, usize), TimedDotaTab>,
}

#[derive(Debug, Default)]
struct MatchPromise {
    value: Mutex<Option<Vec<ProjectedMatch>>>,
    ready: Condvar,
}

impl MatchPromise {
    fn complete(&self, matches: Vec<ProjectedMatch>) -> Result<(), OpenDotaPlayerServiceError> {
        let mut value = self
            .value
            .lock()
            .map_err(|_| OpenDotaPlayerServiceError::CachePoisoned)?;
        *value = Some(matches);
        self.ready.notify_all();
        Ok(())
    }

    fn wait(&self) -> Result<Vec<ProjectedMatch>, OpenDotaPlayerServiceError> {
        let mut value = self
            .value
            .lock()
            .map_err(|_| OpenDotaPlayerServiceError::CachePoisoned)?;
        while value.is_none() {
            value = self
                .ready
                .wait(value)
                .map_err(|_| OpenDotaPlayerServiceError::CachePoisoned)?;
        }
        Ok(value.clone().unwrap_or_default())
    }
}

pub struct OpenDotaPlayerService<P, A, C, H> {
    player: P,
    api: A,
    clock: C,
    heroes: H,
    cache: Mutex<CacheState>,
    profile_ready: Condvar,
}

impl<P, A, C, H> OpenDotaPlayerService<P, A, C, H>
where
    P: PlayerSteamIdPort,
    A: OpenDotaPlayerApiPort,
    C: OpenDotaPlayerClock,
    H: OpenDotaHeroCatalog,
{
    #[must_use]
    pub const fn new(player: P, api: A, clock: C, heroes: H) -> Self {
        Self {
            player,
            api,
            clock,
            heroes,
            cache: Mutex::new(CacheState {
                profiles: BTreeMap::new(),
                profile_inflight: BTreeSet::new(),
                dota_tabs: BTreeMap::new(),
            }),
            profile_ready: Condvar::new(),
        }
    }

    pub fn get_player_profile(
        &self,
        request: ProfileRequest,
    ) -> Result<Option<Arc<PlayerProfile>>, OpenDotaPlayerServiceError> {
        self.get_player_profile_with_promise(request, None)
    }

    fn get_player_profile_with_promise(
        &self,
        request: ProfileRequest,
        matches: Option<Arc<MatchPromise>>,
    ) -> Result<Option<Arc<PlayerProfile>>, OpenDotaPlayerServiceError> {
        let Some(steam_id) = self.resolve_steam_id(request.discord_id, request.steam_id)? else {
            return Ok(None);
        };
        let key = (request.discord_id, steam_id);
        let now = self.clock.now_seconds();
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| OpenDotaPlayerServiceError::CachePoisoned)?;
        loop {
            if !request.force_refresh
                && let Some(cached) = cache.profiles.get(&request.discord_id)
                && cached.profile.steam_id == steam_id
                && is_fresh(now, cached.cached_at)
            {
                return Ok(Some(Arc::clone(&cached.profile)));
            }
            if cache.profile_inflight.insert(key) {
                break;
            }
            cache = self
                .profile_ready
                .wait(cache)
                .map_err(|_| OpenDotaPlayerServiceError::CachePoisoned)?;
            if !cache.profile_inflight.contains(&key) {
                return Ok(cache.profiles.get(&request.discord_id).and_then(|cached| {
                    (cached.profile.steam_id == steam_id && is_fresh(now, cached.cached_at))
                        .then(|| Arc::clone(&cached.profile))
                }));
            }
        }
        drop(cache);

        let profile = self
            .fetch_profile(steam_id, request.recent_matches, matches.as_deref())
            .map(Arc::new);
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| OpenDotaPlayerServiceError::CachePoisoned)?;
        if let Some(profile) = &profile {
            cache.profiles.insert(
                request.discord_id,
                TimedProfile {
                    cached_at: self.clock.now_seconds(),
                    profile: Arc::clone(profile),
                },
            );
        }
        cache.profile_inflight.remove(&key);
        self.profile_ready.notify_all();
        Ok(profile)
    }

    fn resolve_steam_id(
        &self,
        discord_id: UserId,
        supplied: Option<SteamId>,
    ) -> Result<Option<SteamId>, OpenDotaPlayerServiceError> {
        supplied.map_or_else(
            || {
                self.player
                    .steam_id(discord_id)
                    .map_err(|error| OpenDotaPlayerServiceError::PlayerLookup(error.to_string()))
            },
            |steam_id| Ok(Some(steam_id)),
        )
    }

    fn fetch_profile(
        &self,
        steam_id: SteamId,
        supplied_matches: Option<Vec<ProjectedMatch>>,
        matches_promise: Option<&MatchPromise>,
    ) -> Option<PlayerProfile> {
        let (identity, win_loss, averages, top_heroes) = std::thread::scope(|scope| {
            let identity = scope.spawn(|| self.api.player_identity(steam_id));
            let win_loss = scope.spawn(|| self.api.win_loss(steam_id));
            let averages = scope.spawn(|| self.api.profile_averages(steam_id));
            let top_heroes = scope.spawn(|| self.api.top_heroes(steam_id));
            (
                identity.join(),
                win_loss.join(),
                averages.join(),
                top_heroes.join(),
            )
        });
        let identity = identity.ok()?.ok()??;
        let win_loss = win_loss.ok()?.ok()?;
        let averages = averages.ok()?.ok()?;
        let top_heroes = top_heroes.ok()?.ok()?;
        let matches = supplied_matches.unwrap_or_else(|| {
            matches_promise.map_or_else(
                || self.api.projected_matches(steam_id, 5).unwrap_or_default(),
                |promise| promise.wait().unwrap_or_default(),
            )
        });
        let recent_matches = self.format_recent_matches(&matches, 5);
        Some(PlayerProfile {
            steam_id,
            persona_name: identity
                .persona_name
                .unwrap_or_else(|| "Unknown".to_owned()),
            avatar: identity.avatar,
            rank_tier: identity.rank_tier,
            mmr_estimate: identity.mmr_estimate,
            wins: win_loss.wins,
            losses: win_loss.losses,
            win_rate: calculate_win_rate(win_loss.wins, win_loss.losses),
            averages,
            top_heroes,
            last_match_id: recent_matches.first().and_then(|item| item.match_id),
            recent_matches,
        })
    }

    pub fn format_profile_embed(
        &self,
        discord_id: UserId,
        target_name: &str,
    ) -> Result<Option<ProfileEmbedPlan>, OpenDotaPlayerServiceError> {
        let Some(profile) = self.get_player_profile(ProfileRequest::new(discord_id))? else {
            return Ok(None);
        };
        let heroes = profile
            .top_heroes
            .iter()
            .take(3)
            .enumerate()
            .map(|(index, hero)| {
                format!(
                    "{}. {} - {} games ({}%)",
                    index + 1,
                    hero.hero_name,
                    hero.games,
                    hero.win_rate
                )
            })
            .collect::<Vec<_>>();
        let recent = profile
            .recent_matches
            .iter()
            .take(3)
            .map(|item| {
                format!(
                    "[{}] {} ({}/{}/{})",
                    if item.won { "W" } else { "L" },
                    item.hero_name,
                    item.kills,
                    item.deaths,
                    item.assists
                )
            })
            .collect::<Vec<_>>();
        Ok(Some(ProfileEmbedPlan {
            title: format!("Profile: {target_name}"),
            fields: vec![
                ProfileEmbedField {
                    name: "Overall".to_owned(),
                    value: format!(
                        "{}W / {}L ({}%)",
                        profile.wins, profile.losses, profile.win_rate
                    ),
                    inline: true,
                },
                ProfileEmbedField {
                    name: "Avg KDA".to_owned(),
                    value: format!(
                        "{}/{}/{}",
                        profile.averages.kills, profile.averages.deaths, profile.averages.assists
                    ),
                    inline: true,
                },
                ProfileEmbedField {
                    name: "Avg GPM/XPM".to_owned(),
                    value: format!("{} / {}", profile.averages.gpm, profile.averages.xpm),
                    inline: true,
                },
                ProfileEmbedField {
                    name: "Top Heroes".to_owned(),
                    value: if heroes.is_empty() {
                        "No data".to_owned()
                    } else {
                        heroes.join("\n")
                    },
                    inline: false,
                },
                ProfileEmbedField {
                    name: "Recent Matches".to_owned(),
                    value: if recent.is_empty() {
                        "No data".to_owned()
                    } else {
                        recent.join("\n")
                    },
                    inline: false,
                },
            ],
            footer: format!("Data from OpenDota | Steam ID: {}", profile.steam_id.0),
            last_match_id: profile.last_match_id,
        }))
    }

    pub fn get_full_stats(
        &self,
        discord_id: UserId,
        match_limit: usize,
    ) -> Result<Option<FullStats>, OpenDotaPlayerServiceError> {
        let Some(steam_id) = self.resolve_steam_id(discord_id, None)? else {
            return Ok(None);
        };
        let Some(profile) = self.get_player_profile(ProfileRequest {
            discord_id,
            steam_id: Some(steam_id),
            ..ProfileRequest::new(discord_id)
        })?
        else {
            return Ok(None);
        };
        let matches = self
            .api
            .projected_matches(steam_id, match_limit)
            .unwrap_or_default();
        Ok(Some(self.build_full_stats(steam_id, &profile, &matches)))
    }

    pub fn get_dota_tab_stats(
        &self,
        discord_id: UserId,
        match_limit: usize,
        supplied_steam_id: Option<SteamId>,
    ) -> Result<Arc<DotaTabStats>, OpenDotaPlayerServiceError> {
        let Some(steam_id) = self.resolve_steam_id(discord_id, supplied_steam_id)? else {
            return Ok(Arc::new(DotaTabStats::default()));
        };
        let key = (discord_id, steam_id, match_limit);
        let now = self.clock.now_seconds();
        {
            let mut cache = self
                .cache
                .lock()
                .map_err(|_| OpenDotaPlayerServiceError::CachePoisoned)?;
            if let Some(cached) = cache.dota_tabs.get(&key) {
                if is_fresh(now, cached.cached_at) {
                    return Ok(Arc::clone(&cached.stats));
                }
                cache.dota_tabs.remove(&key);
            }
        }

        let matches_promise = Arc::new(MatchPromise::default());
        let (profile, matches) = std::thread::scope(|scope| {
            let profile_promise = Arc::clone(&matches_promise);
            let profile = scope.spawn(|| {
                self.get_player_profile_with_promise(
                    ProfileRequest {
                        discord_id,
                        steam_id: Some(steam_id),
                        ..ProfileRequest::new(discord_id)
                    },
                    Some(profile_promise),
                )
            });
            let matches = self
                .api
                .projected_matches(steam_id, match_limit)
                .unwrap_or_default();
            let completed = matches_promise.complete(matches.clone());
            let profile = profile.join();
            (profile, completed.map(|()| matches))
        });
        let profile = profile.map_err(|_| OpenDotaPlayerServiceError::CachePoisoned)??;
        let matches = matches?;
        let role_distribution = self.calculate_role_distribution(&matches);
        let full_stats = profile
            .as_deref()
            .map(|profile| self.build_full_stats(steam_id, profile, &matches));
        let result = Arc::new(DotaTabStats {
            role_distribution,
            full_stats,
        });
        if result.full_stats.is_some() {
            self.cache
                .lock()
                .map_err(|_| OpenDotaPlayerServiceError::CachePoisoned)?
                .dota_tabs
                .insert(
                    key,
                    TimedDotaTab {
                        cached_at: self.clock.now_seconds(),
                        stats: Arc::clone(&result),
                    },
                );
        }
        Ok(result)
    }

    fn build_full_stats(
        &self,
        steam_id: SteamId,
        profile: &PlayerProfile,
        matches: &[ProjectedMatch],
    ) -> FullStats {
        let recent = &matches[..matches.len().min(20)];
        let recent_wins = recent.iter().filter(|item| did_win(item)).count() as i64;
        let recent_losses = recent.len() as i64 - recent_wins;
        let (lane_distribution, lane_parsed_count) = calculate_lane_distribution(matches);
        FullStats {
            steam_id,
            persona_name: profile.persona_name.clone(),
            rank_tier: profile.rank_tier,
            mmr_estimate: profile.mmr_estimate,
            total_wins: profile.wins,
            total_losses: profile.losses,
            total_win_rate: profile.win_rate,
            averages: profile.averages,
            attribute_distribution: self.calculate_attribute_distribution(matches),
            lane_distribution,
            lane_parsed_count,
            top_heroes: profile.top_heroes.clone(),
            recent_matches: profile.recent_matches.clone(),
            recent_wins,
            recent_losses,
            recent_win_rate: calculate_win_rate(recent_wins, recent_losses),
            matches_analyzed: matches.len(),
        }
    }

    fn format_recent_matches(&self, matches: &[ProjectedMatch], limit: usize) -> Vec<RecentMatch> {
        let heroes = self.heroes.heroes().unwrap_or_default();
        matches
            .iter()
            .take(limit)
            .map(|item| RecentMatch {
                match_id: item.match_id,
                hero_id: item.hero_id,
                hero_name: item
                    .hero_id
                    .and_then(|id| heroes.get(&id))
                    .map_or_else(|| "Unknown".to_owned(), |hero| hero.name.clone()),
                kills: item.kills,
                deaths: item.deaths,
                assists: item.assists,
                won: did_win(item),
                duration_seconds: item.duration_seconds,
                start_time: item.start_time,
            })
            .collect()
    }

    pub fn calculate_attribute_distribution(&self, matches: &[ProjectedMatch]) -> Distribution {
        let heroes = self.heroes.heroes().unwrap_or_default();
        calculate_attribute_distribution(matches, &heroes)
    }

    pub fn calculate_role_distribution(&self, matches: &[ProjectedMatch]) -> Option<Distribution> {
        let heroes = self.heroes.heroes().ok()?;
        calculate_role_distribution(matches, &heroes)
    }
}

#[must_use]
pub fn calculate_win_rate(wins: i64, losses: i64) -> f64 {
    let total = wins.saturating_add(losses);
    if total == 0 {
        0.0
    } else {
        round_tenth(wins as f64 / total as f64 * 100.0)
    }
}

#[must_use]
pub fn did_win(match_data: &ProjectedMatch) -> bool {
    (match_data.player_slot.unwrap_or(0) < 128) == match_data.radiant_win
}

#[must_use]
pub fn calculate_lane_distribution(matches: &[ProjectedMatch]) -> (Distribution, usize) {
    let lanes = [
        (0, "Roaming"),
        (1, "Safe Lane"),
        (2, "Mid"),
        (3, "Off Lane"),
        (4, "Jungle"),
    ];
    let mut counts = lanes
        .iter()
        .map(|(_, name)| ((*name).to_owned(), 0_usize))
        .collect::<BTreeMap<_, _>>();
    let mut total = 0_usize;
    for item in matches {
        if let Some((_, name)) = lanes.iter().find(|(lane, _)| Some(*lane) == item.lane_role) {
            *counts.entry((*name).to_owned()).or_default() += 1;
            total += 1;
        }
    }
    let distribution = counts
        .into_iter()
        .map(|(lane, count)| {
            let percentage = if total == 0 {
                0.0
            } else {
                round_tenth(count as f64 / total as f64 * 100.0)
            };
            (lane, percentage)
        })
        .collect();
    (distribution, total)
}

fn calculate_attribute_distribution(
    matches: &[ProjectedMatch],
    heroes: &BTreeMap<i64, HeroMetadata>,
) -> Distribution {
    let mut counts = ["str", "agi", "int", "all"]
        .into_iter()
        .map(|attribute| (attribute.to_owned(), 0_usize))
        .collect::<BTreeMap<_, _>>();
    let mut total = 0_usize;
    for item in matches {
        if let Some(hero) = item.hero_id.and_then(|id| heroes.get(&id)) {
            *counts.entry(hero.attribute.code().to_owned()).or_default() += 1;
            total += 1;
        }
    }
    counts
        .into_iter()
        .map(|(attribute, count)| {
            let percentage = if total == 0 {
                0.0
            } else {
                round_tenth(count as f64 / total as f64 * 100.0)
            };
            (attribute, percentage)
        })
        .collect()
}

fn calculate_role_distribution(
    matches: &[ProjectedMatch],
    heroes: &BTreeMap<i64, HeroMetadata>,
) -> Option<Distribution> {
    let mut counts = BTreeMap::<String, f64>::new();
    let mut total = 0.0;
    for item in matches {
        if let Some(hero) = item.hero_id.and_then(|id| heroes.get(&id)) {
            for (role, weight) in &hero.role_weights {
                *counts.entry(role.clone()).or_default() += weight;
                total += weight;
            }
        }
    }
    (total > 0.0).then(|| {
        counts
            .into_iter()
            .map(|(role, count)| (role, round_tenth(count / total * 100.0)))
            .collect()
    })
}

const fn is_fresh(now: i64, cached_at: i64) -> bool {
    now.saturating_sub(cached_at) < CACHE_TTL_SECONDS
}

fn round_tenth(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests;
