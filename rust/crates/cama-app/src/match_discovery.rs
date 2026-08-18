//! Match-discovery, enrichment-validation, and fantasy-scoring policy.
//!
//! This module deliberately has no HTTP, Discord, or concrete database client.
//! Repositories, OpenDota, enrichment writes, and time are narrow ports. The
//! policy implements guild-scoped discovery, successful-result caching,
//! transient retry behavior, candidate-detail validation precedence, dry-run
//! safety, concurrent-run idempotency, UTC timestamp parsing, and enrichment
//! validation.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cama_db::core_repositories::MatchRepository as CoreMatchRepository;
use cama_db::match_recording_repository::{
    EnrichmentParticipantUpdate as DbEnrichmentParticipantUpdate,
    MatchEnrichmentRequest as DbMatchEnrichmentRequest,
    MatchParticipantFarmRow as DbMatchParticipantFarmRow, MatchRecordingRepository,
    WrappedEnrichmentFactUpdate as DbWrappedEnrichmentFactUpdate,
};
use cama_db::opendota_player::OpenDotaPlayerRepository;
use cama_domain::position_estimate::{ParticipantFarmStats, estimate_team_positions};
use chrono::{DateTime, NaiveDateTime};
use rusqlite::types::Value as SqlValue;

use crate::dedicated_lobby_channel::{GuildId, UserId};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InternalMatchId(pub i64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ValveMatchId(pub i64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SteamId(pub i64);

#[derive(Clone, Debug, PartialEq)]
pub enum MatchTimeValue {
    Missing,
    Unix(f64),
    Text(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TeamSide {
    Radiant,
    Dire,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InternalMatch {
    pub match_id: InternalMatchId,
    pub match_date: MatchTimeValue,
    pub winning_team: TeamSide,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InternalParticipant {
    pub discord_id: UserId,
    pub side: TeamSide,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlayerHistoryMatch {
    pub match_id: ValveMatchId,
    pub start_time: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FantasyStats {
    pub kills: f64,
    /// `None` differs from a real zero: Python omits the three-point survival
    /// baseline when the key itself is absent.
    pub deaths: Option<f64>,
    pub assists: f64,
    pub last_hits: f64,
    pub denies: f64,
    pub gold_per_min: f64,
    pub xp_per_min: f64,
    pub towers_killed: f64,
    pub roshans_killed: f64,
    pub teamfight_participation: f64,
    pub obs_placed: f64,
    pub sen_placed: f64,
    pub camps_stacked: f64,
    pub rune_pickups: f64,
    pub firstblood_claimed: bool,
    pub stuns: f64,
    pub hero_healing: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpenDotaPlayer {
    pub account_id: Option<SteamId>,
    pub player_slot: Option<u16>,
    /// Persisted participant columns projected from the OpenDota payload.
    pub stats: EnrichedParticipantStats,
    pub fantasy: FantasyStats,
    /// Small, typed subset retained for Cama Wrapped. The full OpenDota JSON
    /// remains a transport concern and is not required to compute these facts.
    pub wrapped: WrappedPlayerTelemetry,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EnrichedParticipantStats {
    pub hero_id: i64,
    pub kills: i64,
    pub deaths: i64,
    pub assists: i64,
    pub gpm: i64,
    pub xpm: i64,
    pub hero_damage: i64,
    pub tower_damage: i64,
    pub last_hits: i64,
    /// Creep score at the 12-minute mark (OpenDota's `lh_t[12]`); `None`
    /// for matches shorter than 12 minutes or lacking a parsed timeline.
    pub last_hits_at_12: Option<i64>,
    pub denies: i64,
    pub net_worth: i64,
    pub hero_healing: i64,
    pub lane_role: Option<i64>,
    pub lane_efficiency: Option<i64>,
    pub towers_killed: Option<i64>,
    pub roshans_killed: Option<i64>,
    pub teamfight_participation: Option<f64>,
    pub obs_placed: Option<i64>,
    pub sen_placed: Option<i64>,
    pub camps_stacked: Option<i64>,
    pub rune_pickups: Option<i64>,
    pub firstblood_claimed: Option<i64>,
    pub stuns: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WrappedPlayerTelemetry {
    pub actions_per_min: Option<i64>,
    pub courier_kills: Option<i64>,
    pub pings: Option<i64>,
    pub lane_role: Option<i64>,
    pub purchase_keys: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OpenDotaMatchDetails {
    pub match_id: ValveMatchId,
    pub duration_seconds: i64,
    pub radiant_win: bool,
    pub radiant_score: i64,
    pub dire_score: i64,
    pub game_mode: i64,
    pub comeback: Option<i64>,
    pub throw_amount: Option<i64>,
    /// Canonical JSON, when supplied by the transport, for compatible storage.
    pub raw_payload: Option<String>,
    pub players: Vec<OpenDotaPlayer>,
}

impl OpenDotaMatchDetails {
    #[must_use]
    pub fn roster(match_id: ValveMatchId, account_ids: impl IntoIterator<Item = SteamId>) -> Self {
        Self {
            match_id,
            duration_seconds: 0,
            radiant_win: true,
            radiant_score: 0,
            dire_score: 0,
            game_mode: 0,
            comeback: None,
            throw_amount: None,
            raw_payload: None,
            players: account_ids
                .into_iter()
                .map(|account_id| OpenDotaPlayer {
                    account_id: Some(account_id),
                    ..OpenDotaPlayer::default()
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct DiscoveryPortError {
    pub message: String,
}

impl DiscoveryPortError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub trait MatchDiscoveryReadPort: Send + Sync {
    fn get_match(
        &self,
        match_id: InternalMatchId,
        guild_id: GuildId,
    ) -> Result<Option<InternalMatch>, DiscoveryPortError>;

    fn get_match_participants(
        &self,
        match_id: InternalMatchId,
        guild_id: GuildId,
    ) -> Result<Vec<InternalParticipant>, DiscoveryPortError>;

    fn get_matches_without_enrichment(
        &self,
        guild_id: GuildId,
        limit: usize,
    ) -> Result<Vec<InternalMatch>, DiscoveryPortError>;

    fn get_match_participants_bulk(
        &self,
        match_ids: &[InternalMatchId],
        guild_id: GuildId,
    ) -> Result<BTreeMap<InternalMatchId, Vec<InternalParticipant>>, DiscoveryPortError>;
}

pub trait PlayerSteamIdPort: Send + Sync {
    fn get_steam_ids_bulk(
        &self,
        discord_ids: &[UserId],
        guild_id: GuildId,
    ) -> Result<BTreeMap<UserId, Vec<SteamId>>, DiscoveryPortError>;
}

impl MatchDiscoveryReadPort for CoreMatchRepository {
    fn get_match(
        &self,
        match_id: InternalMatchId,
        guild_id: GuildId,
    ) -> Result<Option<InternalMatch>, DiscoveryPortError> {
        CoreMatchRepository::get_match(self, match_id.0, Some(guild_id.0))
            .map(|row| row.map(core_match_to_internal))
            .map_err(|error| DiscoveryPortError::new(error.to_string()))
    }

    fn get_match_participants(
        &self,
        match_id: InternalMatchId,
        guild_id: GuildId,
    ) -> Result<Vec<InternalParticipant>, DiscoveryPortError> {
        CoreMatchRepository::get_match_participants(self, match_id.0, Some(guild_id.0))
            .map(|rows| {
                rows.into_iter()
                    .map(|row| InternalParticipant {
                        discord_id: UserId(row.discord_id),
                        side: sqlite_side(&row.side),
                    })
                    .collect()
            })
            .map_err(|error| DiscoveryPortError::new(error.to_string()))
    }

    fn get_matches_without_enrichment(
        &self,
        guild_id: GuildId,
        limit: usize,
    ) -> Result<Vec<InternalMatch>, DiscoveryPortError> {
        CoreMatchRepository::get_matches_without_enrichment(self, Some(guild_id.0), limit)
            .map(|rows| {
                rows.into_iter()
                    .map(|row| InternalMatch {
                        match_id: InternalMatchId(row.match_id),
                        match_date: sqlite_match_time(row.match_date),
                        winning_team: sqlite_side_from_winner(&row.winning_team),
                    })
                    .collect()
            })
            .map_err(|error| DiscoveryPortError::new(error.to_string()))
    }

    fn get_match_participants_bulk(
        &self,
        match_ids: &[InternalMatchId],
        guild_id: GuildId,
    ) -> Result<BTreeMap<InternalMatchId, Vec<InternalParticipant>>, DiscoveryPortError> {
        let ids = match_ids
            .iter()
            .map(|match_id| match_id.0)
            .collect::<Vec<_>>();
        CoreMatchRepository::get_match_participants_bulk(self, &ids, Some(guild_id.0))
            .map(|rows| {
                rows.into_iter()
                    .map(|(match_id, participants)| {
                        (
                            InternalMatchId(match_id),
                            participants
                                .into_iter()
                                .map(|participant| InternalParticipant {
                                    discord_id: UserId(participant.discord_id),
                                    side: sqlite_side(&participant.side),
                                })
                                .collect(),
                        )
                    })
                    .collect()
            })
            .map_err(|error| DiscoveryPortError::new(error.to_string()))
    }
}

impl PlayerSteamIdPort for OpenDotaPlayerRepository {
    fn get_steam_ids_bulk(
        &self,
        discord_ids: &[UserId],
        guild_id: GuildId,
    ) -> Result<BTreeMap<UserId, Vec<SteamId>>, DiscoveryPortError> {
        let ids = discord_ids
            .iter()
            .map(|discord_id| discord_id.0)
            .collect::<Vec<_>>();
        OpenDotaPlayerRepository::get_steam_ids_bulk(self, &ids, Some(guild_id.0))
            .map(|rows| {
                discord_ids
                    .iter()
                    .copied()
                    .map(|discord_id| {
                        let steam_ids = rows
                            .get(&discord_id.0)
                            .into_iter()
                            .flatten()
                            .copied()
                            .map(SteamId)
                            .collect();
                        (discord_id, steam_ids)
                    })
                    .collect()
            })
            .map_err(|error| DiscoveryPortError::new(error.to_string()))
    }
}

fn core_match_to_internal(row: cama_db::core_repositories::MatchSummary) -> InternalMatch {
    InternalMatch {
        match_id: InternalMatchId(row.match_id),
        match_date: sqlite_match_time(row.match_date),
        winning_team: sqlite_side_from_winner(&row.winning_team),
    }
}

fn sqlite_match_time(value: SqlValue) -> MatchTimeValue {
    match value {
        SqlValue::Integer(value) => MatchTimeValue::Unix(value as f64),
        SqlValue::Real(value) => MatchTimeValue::Unix(value),
        SqlValue::Text(value) => MatchTimeValue::Text(value),
        SqlValue::Null | SqlValue::Blob(_) => MatchTimeValue::Missing,
    }
}

fn sqlite_side_from_winner(value: &SqlValue) -> TeamSide {
    match value {
        SqlValue::Integer(1) | SqlValue::Real(1.0) => TeamSide::Radiant,
        SqlValue::Text(value) if value == "1" => TeamSide::Radiant,
        _ => TeamSide::Dire,
    }
}

fn sqlite_side(value: &SqlValue) -> TeamSide {
    match value {
        SqlValue::Text(value) if value == "radiant" => TeamSide::Radiant,
        SqlValue::Integer(1) | SqlValue::Real(1.0) => TeamSide::Radiant,
        _ => TeamSide::Dire,
    }
}

pub trait OpenDotaDiscoveryPort: Send + Sync {
    /// `Ok(None)` is a transient/unparsed response. It must not be cached.
    fn player_matches(
        &self,
        steam_id: SteamId,
        limit: usize,
    ) -> Result<Option<Vec<PlayerHistoryMatch>>, DiscoveryPortError>;

    fn match_details(
        &self,
        match_id: ValveMatchId,
    ) -> Result<Option<OpenDotaMatchDetails>, DiscoveryPortError>;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EnrichmentSource {
    #[default]
    Manual,
    Auto,
}

impl EnrichmentSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiscoveryEnrichmentRequest {
    pub internal_match_id: InternalMatchId,
    pub valve_match_id: ValveMatchId,
    pub guild_id: GuildId,
    pub source: EnrichmentSource,
    pub confidence: Option<f64>,
    /// A validated discovery payload is passed through to prevent a duplicate
    /// details request and a time-of-check/time-of-use roster change.
    pub validated_details: Option<OpenDotaMatchDetails>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryEnrichmentOutcome {
    pub success: bool,
    pub validation_error: Option<String>,
}

pub trait DiscoveryEnrichmentPort: Send + Sync {
    fn enrich_match(
        &self,
        request: DiscoveryEnrichmentRequest,
    ) -> Result<DiscoveryEnrichmentOutcome, DiscoveryPortError>;
}

pub trait DiscoveryClock: Send + Sync {
    fn now_epoch_seconds(&self) -> i64;
    fn sleep(&self, duration: Duration);
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemDiscoveryClock;

impl DiscoveryClock for SystemDiscoveryClock {
    fn now_epoch_seconds(&self) -> i64 {
        chrono::Utc::now().timestamp()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

#[derive(Clone, Debug)]
pub struct DiscoveryConfig {
    pub minimum_players_with_steam_id: usize,
    pub minimum_roster_matches: usize,
    pub time_window_seconds: i64,
    pub history_limit: usize,
    pub batch_limit: usize,
    pub match_details_cache_size: usize,
    pub transient_retry_delay: Duration,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            minimum_players_with_steam_id: 5,
            minimum_roster_matches: 10,
            time_window_seconds: 3 * 60 * 60,
            history_limit: 100,
            batch_limit: 1_000,
            match_details_cache_size: 32,
            transient_retry_delay: Duration::from_millis(1),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryStatus {
    Discovered,
    LowConfidence,
    NoSteamIds,
    NoCandidates,
    NoTimestamp,
    NotFound,
    ValidationFailed,
    InProgress,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiscoveryResult {
    pub match_id: InternalMatchId,
    pub status: DiscoveryStatus,
    pub valve_match_id: Option<ValveMatchId>,
    pub confidence: Option<f64>,
    pub player_count: usize,
    pub total_players: usize,
    pub players_with_steam_id: usize,
    pub validation_error: Option<String>,
}

impl DiscoveryResult {
    fn status(match_id: InternalMatchId, status: DiscoveryStatus) -> Self {
        Self {
            match_id,
            status,
            valve_match_id: None,
            confidence: None,
            player_count: 0,
            total_players: 0,
            players_with_steam_id: 0,
            validation_error: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BatchDiscoveryResult {
    pub total_unenriched: usize,
    pub discovered: usize,
    pub skipped_low_confidence: usize,
    pub skipped_no_steam_ids: usize,
    pub skipped_validation_failed: usize,
    pub errors: usize,
    pub details: Vec<DiscoveryResult>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DiscoveryKey {
    guild_id: GuildId,
    match_id: InternalMatchId,
}

#[derive(Clone, Debug)]
enum RunState {
    Running,
    Completed {
        result: DiscoveryResult,
        completed_at: i64,
    },
}

#[derive(Default)]
struct BatchCache {
    histories: BTreeMap<SteamId, Vec<PlayerHistoryMatch>>,
    history_attempts: BTreeMap<SteamId, usize>,
    details: BTreeMap<ValveMatchId, OpenDotaMatchDetails>,
    detail_order: VecDeque<ValveMatchId>,
}

impl BatchCache {
    fn history<A: OpenDotaDiscoveryPort, C: DiscoveryClock>(
        &mut self,
        api: &A,
        clock: &C,
        steam_id: SteamId,
        config: &DiscoveryConfig,
    ) -> Option<Vec<PlayerHistoryMatch>> {
        if let Some(cached) = self.histories.get(&steam_id) {
            return Some(cached.clone());
        }
        let attempts = self.history_attempts.entry(steam_id).or_default();
        if *attempts > 0 {
            clock.sleep(config.transient_retry_delay);
        }
        *attempts += 1;
        match api.player_matches(steam_id, config.history_limit) {
            Ok(Some(history)) => {
                // Successful emptiness is meaningful and must be cached.
                self.histories.insert(steam_id, history.clone());
                Some(history)
            }
            Ok(None) | Err(_) => None,
        }
    }

    fn details<A: OpenDotaDiscoveryPort>(
        &mut self,
        api: &A,
        match_id: ValveMatchId,
        config: &DiscoveryConfig,
    ) -> Option<OpenDotaMatchDetails> {
        if let Some(cached) = self.details.get(&match_id) {
            return Some(cached.clone());
        }
        let details = api.match_details(match_id).ok().flatten()?;
        if config.match_details_cache_size > 0 {
            while self.details.len() >= config.match_details_cache_size {
                let Some(oldest) = self.detail_order.pop_front() else {
                    break;
                };
                self.details.remove(&oldest);
            }
            self.detail_order.push_back(match_id);
            self.details.insert(match_id, details.clone());
        }
        Some(details)
    }
}

#[derive(Default)]
struct Candidate {
    discord_ids: BTreeSet<UserId>,
    start_time: i64,
}

pub struct MatchDiscoveryService<M, P, A, E, C> {
    match_repository: M,
    player_repository: P,
    api: A,
    enrichment: E,
    clock: C,
    config: DiscoveryConfig,
    runs: Mutex<BTreeMap<DiscoveryKey, RunState>>,
}

impl<M, P, A, E, C> MatchDiscoveryService<M, P, A, E, C>
where
    M: MatchDiscoveryReadPort,
    P: PlayerSteamIdPort,
    A: OpenDotaDiscoveryPort,
    E: DiscoveryEnrichmentPort,
    C: DiscoveryClock,
{
    #[must_use]
    pub fn new(match_repository: M, player_repository: P, api: A, enrichment: E, clock: C) -> Self {
        Self {
            match_repository,
            player_repository,
            api,
            enrichment,
            clock,
            config: DiscoveryConfig::default(),
            runs: Mutex::new(BTreeMap::new()),
        }
    }

    #[must_use]
    pub fn with_config(mut self, config: DiscoveryConfig) -> Self {
        self.config = config;
        self
    }

    fn lock_runs(&self) -> MutexGuard<'_, BTreeMap<DiscoveryKey, RunState>> {
        self.runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Public, idempotent single-match entry point. Concurrent duplicate jobs
    /// cannot issue duplicate HTTP/enrichment writes; a successful live run is
    /// cached for subsequent triggers in the same process.
    #[must_use]
    pub fn discover_match(
        &self,
        match_id: InternalMatchId,
        guild_id: Option<GuildId>,
        dry_run: bool,
    ) -> DiscoveryResult {
        let guild_id = guild_id.unwrap_or(GuildId::DEFAULT);
        let key = DiscoveryKey { guild_id, match_id };
        if !dry_run {
            let mut runs = self.lock_runs();
            match runs.get(&key) {
                Some(RunState::Running) => {
                    return DiscoveryResult::status(match_id, DiscoveryStatus::InProgress);
                }
                Some(RunState::Completed { result, .. }) => return result.clone(),
                None => {
                    runs.insert(key, RunState::Running);
                }
            }
        }

        let result = self.load_and_discover(match_id, guild_id, dry_run);
        if !dry_run {
            let mut runs = self.lock_runs();
            if result.status == DiscoveryStatus::Discovered {
                runs.insert(
                    key,
                    RunState::Completed {
                        result: result.clone(),
                        completed_at: self.clock.now_epoch_seconds(),
                    },
                );
            } else {
                runs.remove(&key);
            }
        }
        result
    }

    fn load_and_discover(
        &self,
        match_id: InternalMatchId,
        guild_id: GuildId,
        dry_run: bool,
    ) -> DiscoveryResult {
        let internal_match = match self.match_repository.get_match(match_id, guild_id) {
            Ok(Some(internal_match)) => internal_match,
            Ok(None) => return DiscoveryResult::status(match_id, DiscoveryStatus::NotFound),
            Err(error) => {
                let mut result = DiscoveryResult::status(match_id, DiscoveryStatus::Error);
                result.validation_error = Some(error.to_string());
                return result;
            }
        };
        let participants = match self
            .match_repository
            .get_match_participants(match_id, guild_id)
        {
            Ok(participants) => participants,
            Err(error) => {
                let mut result = DiscoveryResult::status(match_id, DiscoveryStatus::Error);
                result.validation_error = Some(error.to_string());
                return result;
            }
        };
        let discord_ids = stable_discord_ids(&participants);
        let steam_ids = match self
            .player_repository
            .get_steam_ids_bulk(&discord_ids, guild_id)
        {
            Ok(steam_ids) => steam_ids,
            Err(error) => {
                let mut result = DiscoveryResult::status(match_id, DiscoveryStatus::Error);
                result.validation_error = Some(error.to_string());
                return result;
            }
        };
        let mut cache = BatchCache::default();
        self.discover_loaded(
            &internal_match,
            &participants,
            &steam_ids,
            guild_id,
            dry_run,
            &mut cache,
        )
    }

    #[must_use]
    pub fn discover_all_matches(
        &self,
        guild_id: Option<GuildId>,
        dry_run: bool,
    ) -> BatchDiscoveryResult {
        let guild_id = guild_id.unwrap_or(GuildId::DEFAULT);
        let matches = match self
            .match_repository
            .get_matches_without_enrichment(guild_id, self.config.batch_limit)
        {
            Ok(matches) => matches,
            Err(error) => {
                return BatchDiscoveryResult {
                    errors: 1,
                    details: vec![DiscoveryResult {
                        validation_error: Some(error.to_string()),
                        ..DiscoveryResult::status(InternalMatchId(0), DiscoveryStatus::Error)
                    }],
                    ..BatchDiscoveryResult::default()
                };
            }
        };
        let mut summary = BatchDiscoveryResult {
            total_unenriched: matches.len(),
            ..BatchDiscoveryResult::default()
        };
        let match_ids = matches
            .iter()
            .map(|internal_match| internal_match.match_id)
            .collect::<Vec<_>>();
        let participants_by_match = match self
            .match_repository
            .get_match_participants_bulk(&match_ids, guild_id)
        {
            Ok(participants) => participants,
            Err(error) => {
                summary.errors = matches.len();
                summary.details = matches
                    .iter()
                    .map(|internal_match| DiscoveryResult {
                        validation_error: Some(error.to_string()),
                        ..DiscoveryResult::status(internal_match.match_id, DiscoveryStatus::Error)
                    })
                    .collect();
                return summary;
            }
        };
        let discord_ids = participants_by_match
            .values()
            .flat_map(|participants| participants.iter().map(|player| player.discord_id))
            .fold(Vec::new(), |mut ids, discord_id| {
                if !ids.contains(&discord_id) {
                    ids.push(discord_id);
                }
                ids
            });
        let steam_ids = match self
            .player_repository
            .get_steam_ids_bulk(&discord_ids, guild_id)
        {
            Ok(steam_ids) => steam_ids,
            Err(error) => {
                summary.errors = matches.len();
                summary.details = matches
                    .iter()
                    .map(|internal_match| DiscoveryResult {
                        validation_error: Some(error.to_string()),
                        ..DiscoveryResult::status(internal_match.match_id, DiscoveryStatus::Error)
                    })
                    .collect();
                return summary;
            }
        };

        let mut cache = BatchCache::default();
        for internal_match in matches {
            let participants = participants_by_match
                .get(&internal_match.match_id)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let result = self.discover_loaded(
                &internal_match,
                participants,
                &steam_ids,
                guild_id,
                dry_run,
                &mut cache,
            );
            match result.status {
                DiscoveryStatus::Discovered => summary.discovered += 1,
                DiscoveryStatus::LowConfidence => summary.skipped_low_confidence += 1,
                DiscoveryStatus::NoSteamIds => summary.skipped_no_steam_ids += 1,
                DiscoveryStatus::ValidationFailed => summary.skipped_validation_failed += 1,
                DiscoveryStatus::Error => summary.errors += 1,
                DiscoveryStatus::NoCandidates
                | DiscoveryStatus::NoTimestamp
                | DiscoveryStatus::NotFound
                | DiscoveryStatus::InProgress => {}
            }
            summary.details.push(result);
        }
        summary
    }

    #[allow(clippy::too_many_arguments)]
    fn discover_loaded(
        &self,
        internal_match: &InternalMatch,
        participants: &[InternalParticipant],
        discord_to_steam_ids: &BTreeMap<UserId, Vec<SteamId>>,
        guild_id: GuildId,
        dry_run: bool,
        cache: &mut BatchCache,
    ) -> DiscoveryResult {
        let mut steam_ids = Vec::new();
        let mut steam_to_discord = BTreeMap::new();
        for participant in participants {
            for steam_id in discord_to_steam_ids
                .get(&participant.discord_id)
                .into_iter()
                .flatten()
            {
                if steam_to_discord
                    .insert(*steam_id, participant.discord_id)
                    .is_none()
                {
                    steam_ids.push(*steam_id);
                }
            }
        }
        let players_with_steam_id = participants
            .iter()
            .filter(|participant| {
                discord_to_steam_ids
                    .get(&participant.discord_id)
                    .is_some_and(|ids| !ids.is_empty())
            })
            .count();
        if players_with_steam_id < self.config.minimum_players_with_steam_id {
            let mut result =
                DiscoveryResult::status(internal_match.match_id, DiscoveryStatus::NoSteamIds);
            result.players_with_steam_id = players_with_steam_id;
            return result;
        }
        let Some(match_time) = parse_match_time(&internal_match.match_date) else {
            return DiscoveryResult::status(internal_match.match_id, DiscoveryStatus::NoTimestamp);
        };

        let mut candidates: BTreeMap<ValveMatchId, Candidate> = BTreeMap::new();
        let mut rejected_details = BTreeSet::new();
        let mut unavailable_details = BTreeSet::new();
        let mut selected: Option<(ValveMatchId, OpenDotaMatchDetails, usize)> = None;
        let mut best_detail: Option<(ValveMatchId, usize)> = None;

        for steam_id in steam_ids {
            let Some(history) = cache.history(&self.api, &self.clock, steam_id, &self.config)
            else {
                continue;
            };
            for history_match in history {
                if (history_match.start_time - match_time).abs() > self.config.time_window_seconds {
                    continue;
                }
                let discord_id = steam_to_discord.get(&steam_id).copied();
                let candidate =
                    candidates
                        .entry(history_match.match_id)
                        .or_insert_with(|| Candidate {
                            start_time: history_match.start_time,
                            ..Candidate::default()
                        });
                if let Some(discord_id) = discord_id {
                    candidate.discord_ids.insert(discord_id);
                }
            }

            let mut unchecked = candidates
                .iter()
                .filter(|(match_id, _)| {
                    !rejected_details.contains(*match_id)
                        && !unavailable_details.contains(*match_id)
                })
                .map(|(match_id, candidate)| (*match_id, candidate))
                .collect::<Vec<_>>();
            unchecked.sort_by(|(left_id, left), (right_id, right)| {
                (left.start_time - match_time)
                    .abs()
                    .cmp(&(right.start_time - match_time).abs())
                    .then_with(|| right.discord_ids.len().cmp(&left.discord_ids.len()))
                    .then_with(|| left_id.cmp(right_id))
            });
            for (candidate_id, _) in unchecked {
                let Some(details) = cache.details(&self.api, candidate_id, &self.config) else {
                    unavailable_details.insert(candidate_id);
                    continue;
                };
                let matched_count =
                    count_roster_matches(&details, participants, discord_to_steam_ids);
                if best_detail.is_none_or(|(_, best_count)| matched_count > best_count) {
                    best_detail = Some((candidate_id, matched_count));
                }
                if matched_count >= self.config.minimum_roster_matches {
                    selected = Some((candidate_id, details, matched_count));
                    break;
                }
                rejected_details.insert(candidate_id);
            }
            if selected.is_some() {
                break;
            }
        }

        if candidates.is_empty() {
            return DiscoveryResult::status(internal_match.match_id, DiscoveryStatus::NoCandidates);
        }

        let (best_match_id, best_player_count, selected_details) =
            if let Some((match_id, details, count)) = selected {
                (Some(match_id), count, Some(details))
            } else {
                let naive = candidates
                    .iter()
                    .filter(|(match_id, _)| !rejected_details.contains(*match_id))
                    .max_by(|(left_id, left), (right_id, right)| {
                        left.discord_ids
                            .len()
                            .cmp(&right.discord_ids.len())
                            .then_with(|| right_id.cmp(left_id))
                    })
                    .map(|(match_id, candidate)| (*match_id, candidate.discord_ids.len()));
                match naive.or(best_detail) {
                    Some((match_id, count)) => (Some(match_id), count, None),
                    None => (None, 0, None),
                }
            };
        let confidence = best_player_count as f64 / players_with_steam_id as f64;
        let status =
            if best_match_id.is_some() && best_player_count >= self.config.minimum_roster_matches {
                DiscoveryStatus::Discovered
            } else {
                DiscoveryStatus::LowConfidence
            };
        let mut result = DiscoveryResult {
            match_id: internal_match.match_id,
            status,
            valve_match_id: best_match_id,
            confidence: Some(confidence),
            player_count: best_player_count,
            total_players: players_with_steam_id,
            players_with_steam_id,
            validation_error: None,
        };
        if status == DiscoveryStatus::Discovered
            && !dry_run
            && let Some(valve_match_id) = best_match_id
        {
            match self.enrichment.enrich_match(DiscoveryEnrichmentRequest {
                internal_match_id: internal_match.match_id,
                valve_match_id,
                guild_id,
                source: EnrichmentSource::Auto,
                confidence: Some(confidence),
                validated_details: selected_details,
            }) {
                Ok(outcome) if outcome.success => {}
                Ok(outcome) => {
                    result.status = DiscoveryStatus::ValidationFailed;
                    result.validation_error = outcome.validation_error;
                }
                Err(error) => {
                    result.status = DiscoveryStatus::ValidationFailed;
                    result.validation_error = Some(error.to_string());
                }
            }
        }
        result
    }

    #[must_use]
    pub fn completed_at(&self, match_id: InternalMatchId, guild_id: GuildId) -> Option<i64> {
        let runs = self.lock_runs();
        match runs.get(&DiscoveryKey { guild_id, match_id }) {
            Some(RunState::Completed { completed_at, .. }) => Some(*completed_at),
            Some(RunState::Running) | None => None,
        }
    }
}

fn stable_discord_ids(participants: &[InternalParticipant]) -> Vec<UserId> {
    participants
        .iter()
        .map(|participant| participant.discord_id)
        .fold(Vec::new(), |mut ids, discord_id| {
            if !ids.contains(&discord_id) {
                ids.push(discord_id);
            }
            ids
        })
}

#[must_use]
pub fn count_roster_matches(
    details: &OpenDotaMatchDetails,
    participants: &[InternalParticipant],
    discord_to_steam_ids: &BTreeMap<UserId, Vec<SteamId>>,
) -> usize {
    let account_ids = details
        .players
        .iter()
        .filter_map(|player| player.account_id)
        .collect::<BTreeSet<_>>();
    participants
        .iter()
        .filter(|participant| {
            discord_to_steam_ids
                .get(&participant.discord_id)
                .into_iter()
                .flatten()
                .any(|steam_id| account_ids.contains(steam_id))
        })
        .count()
}

#[must_use]
pub fn parse_match_time(value: &MatchTimeValue) -> Option<i64> {
    match value {
        MatchTimeValue::Missing => None,
        MatchTimeValue::Unix(value) if value.is_finite() => Some(*value as i64),
        MatchTimeValue::Unix(_) => None,
        MatchTimeValue::Text(value) => {
            let value = value.trim();
            if value.is_empty() {
                return None;
            }
            DateTime::parse_from_rfc3339(value)
                .map(|date_time| date_time.timestamp())
                .or_else(|_| {
                    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
                        .map(|date_time| date_time.and_utc().timestamp())
                })
                .ok()
        }
    }
}

/// Python-compatible DPC-inspired fantasy scoring. Tower damage is
/// intentionally absent; only confirmed tower kills score points.
#[must_use]
pub fn calculate_fantasy_points(stats: &FantasyStats) -> f64 {
    let mut points = stats.kills * 0.3;
    if let Some(deaths) = stats.deaths {
        points += 3.0 - deaths * 0.3;
    }
    points += stats.assists * 0.15;
    points += stats.last_hits * 0.003;
    points += stats.denies * 0.003;
    points += stats.gold_per_min * 0.002;
    points += stats.xp_per_min * 0.002;
    points += stats.towers_killed;
    points += stats.roshans_killed;
    points += stats.teamfight_participation * 3.0;
    points += (stats.obs_placed + stats.sen_placed) * 0.5;
    points += stats.camps_stacked * 0.5;
    points += stats.rune_pickups * 0.25;
    if stats.firstblood_claimed {
        points += 4.0;
    }
    points += stats.stuns * 0.07;
    points += stats.hero_healing * 0.0004;
    round_two(points)
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EnrichmentValidationError {
    #[error("Only {matched}/{linked} players matched (need {required})")]
    TooFewPlayers {
        matched: usize,
        linked: usize,
        required: usize,
    },
    #[error("Winning team mismatch: internal={internal:?}, OpenDota={opendota:?}")]
    WinningTeamMismatch {
        internal: TeamSide,
        opendota: TeamSide,
    },
    #[error("Player {discord_id:?} on wrong team: internal={internal:?}, OpenDota={opendota:?}")]
    PlayerSideMismatch {
        discord_id: UserId,
        internal: TeamSide,
        opendota: TeamSide,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnrichmentValidation {
    pub matched_players: BTreeMap<UserId, SteamId>,
}

pub fn validate_enrichment(
    internal_match: &InternalMatch,
    details: &OpenDotaMatchDetails,
    participants: &[InternalParticipant],
    discord_to_steam_ids: &BTreeMap<UserId, Vec<SteamId>>,
    minimum_roster_matches: usize,
) -> Result<EnrichmentValidation, EnrichmentValidationError> {
    let by_account = details
        .players
        .iter()
        .filter_map(|player| player.account_id.map(|account_id| (account_id, player)))
        .collect::<BTreeMap<_, _>>();
    let mut matched_players = BTreeMap::new();
    for participant in participants {
        if let Some(steam_id) = discord_to_steam_ids
            .get(&participant.discord_id)
            .into_iter()
            .flatten()
            .find(|steam_id| by_account.contains_key(steam_id))
        {
            matched_players.insert(participant.discord_id, *steam_id);
        }
    }
    let linked = participants
        .iter()
        .filter(|participant| {
            discord_to_steam_ids
                .get(&participant.discord_id)
                .is_some_and(|ids| !ids.is_empty())
        })
        .count();
    if matched_players.len() < minimum_roster_matches {
        return Err(EnrichmentValidationError::TooFewPlayers {
            matched: matched_players.len(),
            linked,
            required: minimum_roster_matches,
        });
    }
    let opendota_winner = if details.radiant_win {
        TeamSide::Radiant
    } else {
        TeamSide::Dire
    };
    if internal_match.winning_team != opendota_winner {
        return Err(EnrichmentValidationError::WinningTeamMismatch {
            internal: internal_match.winning_team,
            opendota: opendota_winner,
        });
    }
    for participant in participants {
        let Some(steam_id) = matched_players.get(&participant.discord_id) else {
            continue;
        };
        let Some(player) = by_account.get(steam_id) else {
            continue;
        };
        let opendota_side = if player.player_slot.unwrap_or(0) < 128 {
            TeamSide::Radiant
        } else {
            TeamSide::Dire
        };
        if participant.side != opendota_side {
            return Err(EnrichmentValidationError::PlayerSideMismatch {
                discord_id: participant.discord_id,
                internal: participant.side,
                opendota: opendota_side,
            });
        }
    }
    Ok(EnrichmentValidation { matched_players })
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParticipantFantasyUpdate {
    pub discord_id: UserId,
    pub fantasy_points: f64,
    pub side: TeamSide,
    pub stats: EnrichedParticipantStats,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WrappedEnrichmentFact {
    pub discord_id: UserId,
    pub actions_per_min: Option<i64>,
    pub courier_kills: Option<i64>,
    pub pings: Option<i64>,
    pub rapier_count: usize,
    pub lane_role: Option<i64>,
    pub comeback: Option<i64>,
    pub throw_amount: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnrichmentWrite {
    pub internal_match_id: InternalMatchId,
    pub valve_match_id: ValveMatchId,
    pub guild_id: GuildId,
    pub source: EnrichmentSource,
    pub confidence: Option<f64>,
    pub duration_seconds: i64,
    pub radiant_score: i64,
    pub dire_score: i64,
    pub game_mode: i64,
    pub raw_payload: Option<String>,
    pub participant_updates: Vec<ParticipantFantasyUpdate>,
    pub wrapped_facts: Vec<WrappedEnrichmentFact>,
}

pub trait EnrichmentWritePort: Send + Sync {
    fn apply_enrichment_atomic(&self, write: EnrichmentWrite) -> Result<(), DiscoveryPortError>;

    /// Farm/lane stats for every participant of a match, grouped by team,
    /// for estimating which position each one actually played. Values are
    /// unset until `apply_enrichment_atomic` has populated them.
    fn get_participant_farm_stats(
        &self,
        match_id: InternalMatchId,
        guild_id: GuildId,
    ) -> Result<Vec<PositionFarmRow>, DiscoveryPortError>;

    /// Persist `(discord_id, position)` estimates produced by
    /// [`estimate_match_positions`].
    fn store_estimated_positions(
        &self,
        match_id: InternalMatchId,
        guild_id: GuildId,
        estimates: &[(i64, &str)],
    ) -> Result<usize, DiscoveryPortError>;

    /// Match IDs with enrichment data but no stored position estimate yet,
    /// oldest first, for [`MatchEnrichmentService::backfill_estimated_positions`].
    fn matches_needing_position_estimate(
        &self,
        guild_id: GuildId,
        limit: usize,
    ) -> Result<Vec<InternalMatchId>, DiscoveryPortError>;
}

/// One participant's team assignment and farm stats, as read through
/// [`EnrichmentWritePort::get_participant_farm_stats`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PositionFarmRow {
    pub team_number: i64,
    pub stats: ParticipantFarmStats,
}

/// Estimate each participant's actually-played position from a match's farm
/// rows, grouped by `team_number`. Best-effort: a team that isn't exactly
/// five players, or that [`estimate_team_positions`] can't confidently
/// resolve (ambiguous mid lane, missing enrichment data), is silently
/// omitted rather than failing the whole match — this is supplementary
/// derived data, not a condition for enrichment to succeed.
#[must_use]
pub fn estimate_match_positions(rows: &[PositionFarmRow]) -> Vec<(i64, &'static str)> {
    let mut by_team: BTreeMap<i64, Vec<ParticipantFarmStats>> = BTreeMap::new();
    for row in rows {
        by_team.entry(row.team_number).or_default().push(row.stats);
    }
    by_team
        .into_values()
        .filter_map(|team| estimate_team_positions(&team).ok())
        .flatten()
        .collect()
}

impl EnrichmentWritePort for MatchRecordingRepository {
    fn apply_enrichment_atomic(&self, write: EnrichmentWrite) -> Result<(), DiscoveryPortError> {
        let participant_updates = write
            .participant_updates
            .iter()
            .map(|update| DbEnrichmentParticipantUpdate {
                discord_id: update.discord_id.0,
                hero_id: Some(update.stats.hero_id),
                kills: Some(update.stats.kills),
                deaths: Some(update.stats.deaths),
                assists: Some(update.stats.assists),
                gpm: Some(update.stats.gpm),
                xpm: Some(update.stats.xpm),
                hero_damage: Some(update.stats.hero_damage),
                tower_damage: Some(update.stats.tower_damage),
                last_hits: Some(update.stats.last_hits),
                last_hits_at_12: update.stats.last_hits_at_12,
                denies: Some(update.stats.denies),
                net_worth: Some(update.stats.net_worth),
                hero_healing: Some(update.stats.hero_healing),
                lane_role: update.stats.lane_role,
                lane_efficiency: update.stats.lane_efficiency,
                towers_killed: update.stats.towers_killed,
                roshans_killed: update.stats.roshans_killed,
                teamfight_participation: update.stats.teamfight_participation,
                obs_placed: update.stats.obs_placed,
                sen_placed: update.stats.sen_placed,
                camps_stacked: update.stats.camps_stacked,
                rune_pickups: update.stats.rune_pickups,
                firstblood_claimed: update.stats.firstblood_claimed,
                stuns: update.stats.stuns,
                fantasy_points: Some(update.fantasy_points),
            })
            .collect::<Vec<_>>();
        let wrapped_facts = write
            .wrapped_facts
            .iter()
            .map(|fact| DbWrappedEnrichmentFactUpdate {
                discord_id: fact.discord_id.0,
                actions_per_min: fact.actions_per_min,
                courier_kills: fact.courier_kills,
                pings: fact.pings,
                rapier_count: fact.rapier_count,
                lane_role: fact.lane_role,
                comeback: fact.comeback,
                throw_amount: fact.throw_amount,
            })
            .collect::<Vec<_>>();
        MatchRecordingRepository::apply_enrichment_atomic(
            self,
            DbMatchEnrichmentRequest {
                match_id: write.internal_match_id.0,
                guild_id: Some(write.guild_id.0),
                valve_match_id: write.valve_match_id.0,
                duration_seconds: write.duration_seconds,
                radiant_score: write.radiant_score,
                dire_score: write.dire_score,
                game_mode: write.game_mode,
                enrichment_data: write.raw_payload.as_deref(),
                enrichment_source: Some(write.source.as_str()),
                enrichment_confidence: write.confidence,
                participant_updates: &participant_updates,
                wrapped_facts: &wrapped_facts,
            },
        )
        .map(|_| ())
        .map_err(|error| DiscoveryPortError::new(error.to_string()))
    }

    fn get_participant_farm_stats(
        &self,
        match_id: InternalMatchId,
        guild_id: GuildId,
    ) -> Result<Vec<PositionFarmRow>, DiscoveryPortError> {
        MatchRecordingRepository::get_participant_farm_stats(self, match_id.0, Some(guild_id.0))
            .map(|rows| {
                rows.into_iter()
                    .map(|row: DbMatchParticipantFarmRow| PositionFarmRow {
                        team_number: row.team_number,
                        stats: row.stats,
                    })
                    .collect()
            })
            .map_err(|error| DiscoveryPortError::new(error.to_string()))
    }

    fn store_estimated_positions(
        &self,
        match_id: InternalMatchId,
        guild_id: GuildId,
        estimates: &[(i64, &str)],
    ) -> Result<usize, DiscoveryPortError> {
        MatchRecordingRepository::store_estimated_positions(
            self,
            match_id.0,
            Some(guild_id.0),
            estimates,
        )
        .map_err(|error| DiscoveryPortError::new(error.to_string()))
    }

    fn matches_needing_position_estimate(
        &self,
        guild_id: GuildId,
        limit: usize,
    ) -> Result<Vec<InternalMatchId>, DiscoveryPortError> {
        MatchRecordingRepository::matches_missing_position_estimates(self, Some(guild_id.0), limit)
            .map(|match_ids| match_ids.into_iter().map(InternalMatchId).collect())
            .map_err(|error| DiscoveryPortError::new(error.to_string()))
    }
}

impl<T: EnrichmentWritePort + ?Sized> EnrichmentWritePort for &T {
    fn apply_enrichment_atomic(&self, write: EnrichmentWrite) -> Result<(), DiscoveryPortError> {
        (**self).apply_enrichment_atomic(write)
    }

    fn get_participant_farm_stats(
        &self,
        match_id: InternalMatchId,
        guild_id: GuildId,
    ) -> Result<Vec<PositionFarmRow>, DiscoveryPortError> {
        (**self).get_participant_farm_stats(match_id, guild_id)
    }

    fn store_estimated_positions(
        &self,
        match_id: InternalMatchId,
        guild_id: GuildId,
        estimates: &[(i64, &str)],
    ) -> Result<usize, DiscoveryPortError> {
        (**self).store_estimated_positions(match_id, guild_id, estimates)
    }

    fn matches_needing_position_estimate(
        &self,
        guild_id: GuildId,
        limit: usize,
    ) -> Result<Vec<InternalMatchId>, DiscoveryPortError> {
        (**self).matches_needing_position_estimate(guild_id, limit)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnrichmentInput {
    pub internal_match: InternalMatch,
    pub valve_match_id: ValveMatchId,
    pub guild_id: GuildId,
    pub details: OpenDotaMatchDetails,
    pub participants: Vec<InternalParticipant>,
    pub discord_to_steam_ids: BTreeMap<UserId, Vec<SteamId>>,
    pub source: EnrichmentSource,
    pub confidence: Option<f64>,
    pub skip_validation: bool,
    pub minimum_roster_matches: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnrichmentResult {
    pub success: bool,
    pub players_enriched: usize,
    pub players_not_found: Vec<SteamId>,
    pub duration_seconds: i64,
    pub radiant_win: bool,
    pub radiant_fantasy: f64,
    pub dire_fantasy: f64,
    pub validation_error: Option<EnrichmentValidationError>,
}

pub struct MatchEnrichmentPolicy<W> {
    writer: W,
}

impl<W: EnrichmentWritePort> MatchEnrichmentPolicy<W> {
    #[must_use]
    pub const fn new(writer: W) -> Self {
        Self { writer }
    }

    pub fn enrich_match(&self, input: EnrichmentInput) -> EnrichmentResult {
        let validation = if input.skip_validation {
            let accounts = input
                .details
                .players
                .iter()
                .filter_map(|player| player.account_id)
                .collect::<BTreeSet<_>>();
            EnrichmentValidation {
                matched_players: input
                    .participants
                    .iter()
                    .filter_map(|participant| {
                        input
                            .discord_to_steam_ids
                            .get(&participant.discord_id)
                            .into_iter()
                            .flatten()
                            .find(|steam_id| accounts.contains(steam_id))
                            .map(|steam_id| (participant.discord_id, *steam_id))
                    })
                    .collect(),
            }
        } else {
            match validate_enrichment(
                &input.internal_match,
                &input.details,
                &input.participants,
                &input.discord_to_steam_ids,
                input.minimum_roster_matches,
            ) {
                Ok(validation) => validation,
                Err(error) => {
                    return EnrichmentResult {
                        success: false,
                        players_enriched: 0,
                        players_not_found: Vec::new(),
                        duration_seconds: input.details.duration_seconds,
                        radiant_win: input.details.radiant_win,
                        radiant_fantasy: 0.0,
                        dire_fantasy: 0.0,
                        validation_error: Some(error),
                    };
                }
            }
        };

        let by_account = input
            .details
            .players
            .iter()
            .filter_map(|player| player.account_id.map(|account_id| (account_id, player)))
            .collect::<BTreeMap<_, _>>();
        let side_by_discord = input
            .participants
            .iter()
            .map(|participant| (participant.discord_id, participant.side))
            .collect::<BTreeMap<_, _>>();
        let mut updates = Vec::new();
        let mut players_not_found = Vec::new();
        let wrapped_facts = input
            .participants
            .iter()
            .map(|participant| {
                let player = input
                    .discord_to_steam_ids
                    .get(&participant.discord_id)
                    .into_iter()
                    .flatten()
                    .find_map(|steam_id| by_account.get(steam_id).copied());
                WrappedEnrichmentFact {
                    discord_id: participant.discord_id,
                    actions_per_min: player.and_then(|player| player.wrapped.actions_per_min),
                    courier_kills: player.and_then(|player| player.wrapped.courier_kills),
                    pings: player.and_then(|player| player.wrapped.pings),
                    rapier_count: player.map_or(0, |player| {
                        player
                            .wrapped
                            .purchase_keys
                            .iter()
                            .filter(|key| key.as_str() == "rapier")
                            .count()
                    }),
                    lane_role: player.and_then(|player| player.wrapped.lane_role),
                    comeback: input.details.comeback,
                    throw_amount: input.details.throw_amount,
                }
            })
            .collect::<Vec<_>>();
        let mut radiant_fantasy = 0.0;
        let mut dire_fantasy = 0.0;
        for participant in &input.participants {
            let discord_id = participant.discord_id;
            let Some(steam_id) = validation.matched_players.get(&discord_id).copied() else {
                if let Some(primary) = input
                    .discord_to_steam_ids
                    .get(&discord_id)
                    .and_then(|steam_ids| steam_ids.first())
                    .copied()
                {
                    players_not_found.push(primary);
                }
                continue;
            };
            let Some(player) = by_account.get(&steam_id) else {
                continue;
            };
            let Some(side) = side_by_discord.get(&discord_id).copied() else {
                continue;
            };
            let fantasy_points = calculate_fantasy_points(&player.fantasy);
            match side {
                TeamSide::Radiant => radiant_fantasy += fantasy_points,
                TeamSide::Dire => dire_fantasy += fantasy_points,
            }
            updates.push(ParticipantFantasyUpdate {
                discord_id,
                fantasy_points,
                side,
                stats: player.stats.clone(),
            });
        }
        if updates.is_empty() {
            return EnrichmentResult {
                success: false,
                players_enriched: 0,
                players_not_found,
                duration_seconds: input.details.duration_seconds,
                radiant_win: input.details.radiant_win,
                radiant_fantasy: 0.0,
                dire_fantasy: 0.0,
                validation_error: None,
            };
        }
        let players_enriched = updates.len();
        let write = EnrichmentWrite {
            internal_match_id: input.internal_match.match_id,
            valve_match_id: input.valve_match_id,
            guild_id: input.guild_id,
            source: input.source,
            confidence: input.confidence,
            duration_seconds: input.details.duration_seconds,
            radiant_score: input.details.radiant_score,
            dire_score: input.details.dire_score,
            game_mode: input.details.game_mode,
            raw_payload: input.details.raw_payload.clone(),
            participant_updates: updates,
            wrapped_facts,
        };
        if self.writer.apply_enrichment_atomic(write).is_err() {
            return EnrichmentResult {
                success: false,
                players_enriched: 0,
                players_not_found: Vec::new(),
                duration_seconds: input.details.duration_seconds,
                radiant_win: input.details.radiant_win,
                radiant_fantasy: 0.0,
                dire_fantasy: 0.0,
                validation_error: None,
            };
        }
        EnrichmentResult {
            success: true,
            players_enriched,
            players_not_found,
            duration_seconds: input.details.duration_seconds,
            radiant_win: input.details.radiant_win,
            radiant_fantasy: round_two(radiant_fantasy),
            dire_fantasy: round_two(dire_fantasy),
            validation_error: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnrichMatchRequest {
    pub internal_match_id: InternalMatchId,
    pub valve_match_id: ValveMatchId,
    pub guild_id: Option<GuildId>,
    pub source: EnrichmentSource,
    pub confidence: Option<f64>,
    pub skip_validation: bool,
    pub opendota_match_data: Option<OpenDotaMatchDetails>,
}

impl EnrichMatchRequest {
    #[must_use]
    pub const fn manual(
        internal_match_id: InternalMatchId,
        valve_match_id: ValveMatchId,
        guild_id: Option<GuildId>,
    ) -> Self {
        Self {
            internal_match_id,
            valve_match_id,
            guild_id,
            source: EnrichmentSource::Manual,
            confidence: None,
            skip_validation: false,
            opendota_match_data: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenSkillEnrichmentResult {
    pub success: bool,
    pub players_updated: usize,
    pub error: Option<String>,
}

pub trait OpenSkillEnrichmentPort: Send + Sync {
    fn update_openskill_ratings_for_match(
        &self,
        match_id: InternalMatchId,
        guild_id: Option<GuildId>,
    ) -> Result<OpenSkillEnrichmentResult, DiscoveryPortError>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchEnrichmentServiceResult {
    pub success: bool,
    pub error: Option<String>,
    pub validation_error: Option<String>,
    pub players_enriched: usize,
    pub players_not_found: Vec<SteamId>,
    pub duration_seconds: i64,
    pub radiant_win: bool,
    pub radiant_score: i64,
    pub dire_score: i64,
    pub radiant_fantasy: f64,
    pub dire_fantasy: f64,
    pub fantasy_points_calculated: bool,
    pub openskill_update: Option<OpenSkillEnrichmentResult>,
    /// Participants for whom [`estimate_match_positions`] confidently
    /// resolved and stored a position; always 0 when `success` is false or
    /// a team's data didn't support an estimate.
    pub positions_estimated: usize,
}

impl MatchEnrichmentServiceResult {
    fn failed(message: impl Into<String>) -> Self {
        Self {
            success: false,
            error: Some(message.into()),
            validation_error: None,
            players_enriched: 0,
            players_not_found: Vec::new(),
            duration_seconds: 0,
            radiant_win: false,
            radiant_score: 0,
            dire_score: 0,
            radiant_fantasy: 0.0,
            dire_fantasy: 0.0,
            fantasy_points_calculated: false,
            openskill_update: None,
            positions_estimated: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamIdBackfillCandidate {
    pub discord_id: UserId,
    pub dotabuff_url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BulkSteamIdLinkOutcome {
    pub success: bool,
    pub is_primary: bool,
    pub error: Option<String>,
}

pub trait SteamIdBackfillPort: Send + Sync {
    fn players_missing_steam_ids(
        &self,
    ) -> Result<Vec<SteamIdBackfillCandidate>, DiscoveryPortError>;

    fn add_steam_ids_bulk(
        &self,
        links: &[(UserId, SteamId)],
    ) -> Result<Vec<BulkSteamIdLinkOutcome>, DiscoveryPortError>;
}

pub trait DotabuffIdExtractionPort: Send + Sync {
    fn extract_player_id_from_dotabuff(&self, url: &str) -> Option<SteamId>;
}

impl SteamIdBackfillPort for OpenDotaPlayerRepository {
    fn players_missing_steam_ids(
        &self,
    ) -> Result<Vec<SteamIdBackfillCandidate>, DiscoveryPortError> {
        OpenDotaPlayerRepository::get_all_with_dotabuff_no_steam_id(self)
            .map(|rows| {
                rows.into_iter()
                    .map(|row| SteamIdBackfillCandidate {
                        discord_id: UserId(row.discord_id),
                        dotabuff_url: row.dotabuff_url,
                    })
                    .collect()
            })
            .map_err(|error| DiscoveryPortError::new(error.to_string()))
    }

    fn add_steam_ids_bulk(
        &self,
        links: &[(UserId, SteamId)],
    ) -> Result<Vec<BulkSteamIdLinkOutcome>, DiscoveryPortError> {
        let links = links
            .iter()
            .map(|(discord_id, steam_id)| (discord_id.0, steam_id.0))
            .collect::<Vec<_>>();
        let added_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs() as i64);
        OpenDotaPlayerRepository::add_steam_ids_bulk(self, &links, added_at)
            .map(|outcomes| {
                outcomes
                    .into_iter()
                    .map(|outcome| BulkSteamIdLinkOutcome {
                        success: outcome.success,
                        is_primary: outcome.is_primary,
                        error: outcome.error,
                    })
                    .collect()
            })
            .map_err(|error| DiscoveryPortError::new(error.to_string()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamIdBackfillResult {
    pub players_updated: usize,
    pub players_failed: Vec<UserId>,
}

/// Typed orchestration for the Python enrichment service. Network transport,
/// SQLite writes, and OpenSkill updates remain behind narrow ports; passing a
/// preloaded payload preserves discovery's one-fetch validation contract.
pub struct MatchEnrichmentService<M, P, A, W, O> {
    match_repository: M,
    player_repository: P,
    opendota: A,
    writer: W,
    openskill: Option<O>,
    minimum_roster_matches: usize,
}

impl<M, P, A, W, O> MatchEnrichmentService<M, P, A, W, O> {
    #[must_use]
    pub const fn new(
        match_repository: M,
        player_repository: P,
        opendota: A,
        writer: W,
        openskill: Option<O>,
    ) -> Self {
        Self {
            match_repository,
            player_repository,
            opendota,
            writer,
            openskill,
            minimum_roster_matches: 10,
        }
    }

    #[must_use]
    pub const fn with_minimum_roster_matches(mut self, minimum: usize) -> Self {
        self.minimum_roster_matches = minimum;
        self
    }
}

impl<M, P, A, W, O> MatchEnrichmentService<M, P, A, W, O>
where
    M: MatchDiscoveryReadPort,
    P: PlayerSteamIdPort,
    A: OpenDotaDiscoveryPort,
    W: EnrichmentWritePort,
    O: OpenSkillEnrichmentPort,
{
    pub fn enrich_match(&self, request: EnrichMatchRequest) -> MatchEnrichmentServiceResult {
        let normalized_guild = request.guild_id.unwrap_or(GuildId(0));
        let internal_match = match self
            .match_repository
            .get_match(request.internal_match_id, normalized_guild)
        {
            Ok(Some(internal_match)) => internal_match,
            Ok(None) => {
                return MatchEnrichmentServiceResult::failed(format!(
                    "Internal match {} not found",
                    request.internal_match_id.0
                ));
            }
            Err(error) => return MatchEnrichmentServiceResult::failed(error.to_string()),
        };
        let details = match request.opendota_match_data {
            Some(details) => details,
            None => match self.opendota.match_details(request.valve_match_id) {
                Ok(Some(details)) => details,
                Ok(None) => {
                    return MatchEnrichmentServiceResult::failed(
                        "Failed to fetch match from OpenDota API",
                    );
                }
                Err(error) => return MatchEnrichmentServiceResult::failed(error.to_string()),
            },
        };
        let radiant_score = details.radiant_score;
        let dire_score = details.dire_score;
        let participants = match self
            .match_repository
            .get_match_participants(request.internal_match_id, normalized_guild)
        {
            Ok(participants) => participants,
            Err(error) => return MatchEnrichmentServiceResult::failed(error.to_string()),
        };
        let discord_ids = participants
            .iter()
            .map(|participant| participant.discord_id)
            .collect::<Vec<_>>();
        let discord_to_steam_ids = match self
            .player_repository
            .get_steam_ids_bulk(&discord_ids, normalized_guild)
        {
            Ok(steam_ids) => steam_ids,
            Err(error) => return MatchEnrichmentServiceResult::failed(error.to_string()),
        };
        let policy = MatchEnrichmentPolicy::new(&self.writer);
        let result = policy.enrich_match(EnrichmentInput {
            internal_match,
            valve_match_id: request.valve_match_id,
            guild_id: normalized_guild,
            details,
            participants,
            discord_to_steam_ids,
            source: request.source,
            confidence: request.confidence,
            skip_validation: request.skip_validation,
            minimum_roster_matches: self.minimum_roster_matches,
        });
        let validation_error = result.validation_error.as_ref().map(ToString::to_string);
        let mut service_result = MatchEnrichmentServiceResult {
            success: result.success,
            error: validation_error
                .as_ref()
                .map(|error| format!("Validation failed: {error}")),
            validation_error,
            players_enriched: result.players_enriched,
            players_not_found: result.players_not_found,
            duration_seconds: result.duration_seconds,
            radiant_win: result.radiant_win,
            radiant_score,
            dire_score,
            radiant_fantasy: result.radiant_fantasy,
            dire_fantasy: result.dire_fantasy,
            fantasy_points_calculated: result.success,
            openskill_update: None,
            positions_estimated: 0,
        };
        if service_result.success
            && let Some(openskill) = &self.openskill
        {
            service_result.openskill_update = Some(
                openskill
                    .update_openskill_ratings_for_match(request.internal_match_id, request.guild_id)
                    .unwrap_or_else(|error| OpenSkillEnrichmentResult {
                        success: false,
                        players_updated: 0,
                        error: Some(error.to_string()),
                    }),
            );
        }
        if service_result.success {
            // Best-effort: an unreadable or ambiguous team simply yields no
            // estimate for that team rather than failing enrichment, which
            // already succeeded and is the part that matters.
            let estimates = self
                .writer
                .get_participant_farm_stats(request.internal_match_id, normalized_guild)
                .map(|rows| estimate_match_positions(&rows))
                .unwrap_or_default();
            if !estimates.is_empty() {
                service_result.positions_estimated = self
                    .writer
                    .store_estimated_positions(
                        request.internal_match_id,
                        normalized_guild,
                        &estimates,
                    )
                    .unwrap_or(0);
            }
        }
        if !service_result.success && service_result.error.is_none() {
            service_result.error =
                Some("No participants matched the OpenDota match data".to_owned());
        }
        service_result
    }
}

impl<M, P, A, W, O> DiscoveryEnrichmentPort for MatchEnrichmentService<M, P, A, W, O>
where
    M: MatchDiscoveryReadPort,
    P: PlayerSteamIdPort,
    A: OpenDotaDiscoveryPort,
    W: EnrichmentWritePort,
    O: OpenSkillEnrichmentPort,
{
    fn enrich_match(
        &self,
        request: DiscoveryEnrichmentRequest,
    ) -> Result<DiscoveryEnrichmentOutcome, DiscoveryPortError> {
        let result = MatchEnrichmentService::enrich_match(
            self,
            EnrichMatchRequest {
                internal_match_id: request.internal_match_id,
                valve_match_id: request.valve_match_id,
                guild_id: Some(request.guild_id),
                source: request.source,
                confidence: request.confidence,
                skip_validation: false,
                opendota_match_data: request.validated_details,
            },
        );
        Ok(DiscoveryEnrichmentOutcome {
            success: result.success,
            validation_error: result.validation_error.or(result.error),
        })
    }
}

impl<M, P, A, W, O> MatchEnrichmentService<M, P, A, W, O>
where
    P: SteamIdBackfillPort,
    A: DotabuffIdExtractionPort,
{
    pub fn backfill_steam_ids(&self) -> Result<SteamIdBackfillResult, DiscoveryPortError> {
        let players = self.player_repository.players_missing_steam_ids()?;
        let mut failed_by_index = BTreeMap::new();
        let mut extracted = Vec::new();
        for (index, player) in players.iter().enumerate() {
            if let Some(steam_id) = self
                .opendota
                .extract_player_id_from_dotabuff(&player.dotabuff_url)
            {
                extracted.push((index, player.discord_id, steam_id));
            } else {
                failed_by_index.insert(index, player.discord_id);
            }
        }

        let mut players_updated = 0;
        if !extracted.is_empty() {
            let links = extracted
                .iter()
                .map(|(_, discord_id, steam_id)| (*discord_id, *steam_id))
                .collect::<Vec<_>>();
            let outcomes = self.player_repository.add_steam_ids_bulk(&links)?;
            if outcomes.len() != extracted.len() {
                return Err(DiscoveryPortError::new(
                    "Steam ID batch result count did not match request count",
                ));
            }
            for ((index, discord_id, _), outcome) in extracted.into_iter().zip(outcomes) {
                if outcome.success {
                    players_updated += 1;
                } else {
                    failed_by_index.insert(index, discord_id);
                }
            }
        }
        Ok(SteamIdBackfillResult {
            players_updated,
            players_failed: (0..players.len())
                .filter_map(|index| failed_by_index.get(&index).copied())
                .collect(),
        })
    }
}

/// Outcome of one [`MatchEnrichmentService::backfill_estimated_positions`]
/// call. `matches_found == limit` means more candidates likely remain —
/// call again to continue the backfill.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PositionBackfillResult {
    pub matches_found: usize,
    pub matches_updated: usize,
    pub positions_estimated: usize,
}

impl<M, P, A, W, O> MatchEnrichmentService<M, P, A, W, O>
where
    W: EnrichmentWritePort,
{
    /// Estimate and store positions for matches that already have OpenDota
    /// enrichment data but predate position estimation, up to `limit`
    /// matches per call. Safe to call repeatedly: matches with no
    /// confidently-resolvable team are simply skipped again next time, and
    /// re-estimating an already-estimated match just overwrites it with the
    /// same result.
    pub fn backfill_estimated_positions(
        &self,
        guild_id: GuildId,
        limit: usize,
    ) -> Result<PositionBackfillResult, DiscoveryPortError> {
        let match_ids = self
            .writer
            .matches_needing_position_estimate(guild_id, limit)?;
        let mut result = PositionBackfillResult {
            matches_found: match_ids.len(),
            ..PositionBackfillResult::default()
        };
        for match_id in match_ids {
            let rows = self.writer.get_participant_farm_stats(match_id, guild_id)?;
            let estimates = estimate_match_positions(&rows);
            if estimates.is_empty() {
                continue;
            }
            let stored = self
                .writer
                .store_estimated_positions(match_id, guild_id, &estimates)?;
            if stored > 0 {
                result.matches_updated += 1;
                result.positions_estimated += stored;
            }
        }
        Ok(result)
    }
}

fn round_two(value: f64) -> f64 {
    // Decimal formatting rounds the original binary float directly. Scaling
    // first can round 11.214999... to an exact 1121.5 intermediate and produce
    // 11.22, whereas Python's `round(value, 2)` correctly returns 11.21.
    format!("{value:.2}").parse().unwrap_or(value)
}

#[cfg(test)]
#[path = "match_discovery/tests.rs"]
mod tests;
