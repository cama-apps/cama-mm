//! Cached live-match snapshots for Dota-hosted matches.
//!
//! Valve's live endpoints are polled by one supervised worker and the result
//! is served from memory to every viewer.  A second optional worker accepts
//! authenticated Dota Game State Integration (GSI) callbacks.  No request
//! handler calls Valve, and upstream response bodies are normalized into the
//! small typed snapshot before they enter the cache.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use cama_domain::dota_lobby::{account_id as dota_account_id, gameplay_has_started};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::net::TcpListener;

use crate::dota_host_config::DotaHostConfig;
use crate::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};

/// Valve's API is intentionally sampled at a modest fixed rate.  Viewer
/// traffic never changes this cadence or creates another upstream request.
pub const DOTA_LIVE_POLL_INTERVAL: Duration = Duration::from_secs(15);
/// Maximum accepted upstream or GSI JSON body size.
pub const DOTA_LIVE_MAX_BODY_BYTES: usize = 1_048_576;
/// Maximum number of match registrations retained by the in-memory feed.
pub const DOTA_LIVE_MAX_MATCHES: usize = 100;
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_UPSTREAM_BASE: &str = "https://api.steampowered.com";
const GSI_FRESHNESS_SECONDS: i64 = 30;

type MatchKey = (i64, i64);

/// Where a normalized live snapshot came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveSnapshotSource {
    RealtimeStats,
    LiveLeagueGames,
    Gsi,
}

impl LiveSnapshotSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RealtimeStats => "realtime_stats",
            Self::LiveLeagueGames => "live_league_games",
            Self::Gsi => "gsi",
        }
    }
}

/// Whether the delay value came from a live source or from the configured
/// spectator-delay fallback.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveDelaySource {
    Source,
    Configured,
}

/// One item field returned by a live source.  Every field is optional because
/// Valve and GSI omit fields at different game stages.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LiveItemSnapshot {
    pub id: Option<u32>,
    pub name: Option<String>,
    pub charges: Option<i64>,
}

/// Player stats present in the most recent source sample.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LivePlayerSnapshot {
    pub account_id: Option<u32>,
    pub hero_id: Option<u32>,
    pub hero_name: Option<String>,
    pub kills: Option<i64>,
    pub deaths: Option<i64>,
    pub assists: Option<i64>,
    pub last_hits: Option<i64>,
    pub denies: Option<i64>,
    pub gold: Option<i64>,
    pub net_worth: Option<i64>,
    pub items: Option<Vec<LiveItemSnapshot>>,
}

/// Map samples contain only positions explicitly supplied by the current feed.
/// League masks carry building identity/status, but no world coordinates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveMapFrame {
    pub match_id: u64,
    pub game_time: i64,
    pub heroes: Vec<LiveMapHero>,
    pub buildings: Vec<LiveMapBuilding>,
    pub roshan_respawn_seconds: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveMapHero {
    pub hero_id: u32,
    pub radiant: bool,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub respawn_seconds: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveMapBuilding {
    pub radiant: bool,
    pub name: String,
    pub destroyed: bool,
    pub x: Option<f64>,
    pub y: Option<f64>,
}

/// Normalized, public-safe snapshot returned to a live viewer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LiveMatchSnapshot {
    pub guild_id: i64,
    pub pending_match_id: i64,
    pub match_id: u64,
    pub source: LiveSnapshotSource,
    /// Local receipt time for the upstream/GSI sample.
    pub observed_at: i64,
    /// Local time at which this cache copy was written.  It is separate from
    /// `observed_at` so a caller can measure forwarding delay.
    pub fetched_at: i64,
    /// Set after a finished match or when the source has not refreshed within
    /// the cache freshness window.
    pub stale: bool,
    /// Source-provided or derived delay, when the source exposes it.
    pub delay_seconds: Option<i64>,
    pub delay_source: Option<LiveDelaySource>,
    pub game_time_seconds: Option<i64>,
    pub radiant_score: Option<i64>,
    pub dire_score: Option<i64>,
    pub players: Option<Vec<LivePlayerSnapshot>>,
    pub announcement_frame: Option<cama_domain::live_announcements::LiveAnnouncementFrame>,
    pub map_frame: Option<LiveMapFrame>,
}

impl Default for LiveMatchSnapshot {
    fn default() -> Self {
        Self {
            guild_id: 0,
            pending_match_id: 0,
            match_id: 0,
            source: LiveSnapshotSource::RealtimeStats,
            observed_at: 0,
            fetched_at: 0,
            stale: true,
            delay_seconds: None,
            delay_source: None,
            game_time_seconds: None,
            radiant_score: None,
            dire_score: None,
            players: None,
            announcement_frame: None,
            map_frame: None,
        }
    }
}

/// HTTP response envelope adds cache freshness without altering the sample's
/// source fields.
#[derive(Clone, Debug, PartialEq, Serialize)]
struct LiveMatchResponse {
    snapshot: LiveMatchSnapshot,
    age_seconds: i64,
}

#[derive(Clone, Debug)]
struct MatchRegistration {
    guild_id: i64,
    pending_match_id: i64,
    match_id: u64,
    server_id: Option<u64>,
    league: u32,
    /// The TV-delay setting frozen into the hosting session.  It takes
    /// precedence over this process's deployment default so a recovered
    /// match keeps the delay it was created with.
    delay_seconds: Option<i64>,
    expected_account_ids: BTreeSet<u32>,
    /// The registration can collect private Valve/GSI samples while betting
    /// remains open, but public snapshot access stays gated until this flips.
    betting_closed: bool,
    /// Monotonic evidence from a validated GSI game-rules state.  This is
    /// intentionally separate from the public snapshot so a pre-close sample
    /// cannot disclose the detailed scoreboard.
    gameplay_started: bool,
    active: bool,
    published_at: i64,
    snapshot: Option<LiveMatchSnapshot>,
}

#[derive(Clone)]
pub struct DotaLiveFeed {
    client: Client,
    upstream_base: String,
    web_api_key: Option<String>,
    live_token: Option<String>,
    gsi_token: Option<String>,
    live_bind: Option<SocketAddr>,
    configured_delay_seconds: Option<i64>,
    matches: Arc<RwLock<BTreeMap<MatchKey, MatchRegistration>>>,
}

#[derive(Debug, Error, Eq, PartialEq)]
enum LiveIngestError {
    #[error("GSI authentication failed")]
    Unauthorized,
    #[error("GSI payload is invalid")]
    InvalidPayload,
    #[error("GSI match is not an active registered match")]
    UnknownMatch,
    #[error("GSI roster does not match the registered roster")]
    RosterMismatch,
}

#[derive(Debug, Error, Eq, PartialEq)]
enum UpstreamError {
    #[error("live source unavailable")]
    Unavailable,
    #[error("live source body exceeded the configured limit")]
    BodyTooLarge,
    #[error("live source returned invalid JSON")]
    InvalidJson,
}

impl DotaLiveFeed {
    /// Construct the live cache and its Valve HTTP client.  Construction is
    /// offline; an absent Web API key simply makes Valve polling unavailable
    /// while GSI and the authenticated viewer endpoint remain usable.
    pub fn new(config: &DotaHostConfig) -> Result<Arc<Self>, String> {
        Self::new_with_upstream_base(config, DEFAULT_UPSTREAM_BASE.to_owned())
    }

    /// Test and operator hook for a compatible Valve API proxy.  Production
    /// callers should use [`Self::new`].
    pub fn new_with_upstream_base(
        config: &DotaHostConfig,
        upstream_base: impl Into<String>,
    ) -> Result<Arc<Self>, String> {
        let client = Client::builder()
            .timeout(UPSTREAM_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "could not construct Dota live HTTP client".to_owned())?;
        Ok(Arc::new(Self {
            client,
            upstream_base: upstream_base.into().trim_end_matches('/').to_owned(),
            web_api_key: config
                .web_api_key
                .as_ref()
                .map(|secret| secret.expose().to_owned()),
            live_token: config
                .live_token
                .as_ref()
                .map(|secret| secret.expose().to_owned()),
            gsi_token: config
                .gsi_token
                .as_ref()
                .map(|secret| secret.expose().to_owned()),
            live_bind: config.live_bind,
            configured_delay_seconds: tv_delay_seconds(config.tv_delay),
            matches: Arc::new(RwLock::new(BTreeMap::new())),
        }))
    }

    /// Register a match before or after its betting window closes.
    ///
    /// Before closure the registration remains private: Valve polling and
    /// authenticated GSI ingestion can establish game-start evidence, while
    /// public snapshot, summary, and HTTP access remain unavailable.
    #[allow(clippy::too_many_arguments)]
    pub async fn publish_match(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        match_id: u64,
        server_id: Option<u64>,
        league: u32,
        tv_delay: u32,
        betting_closed: bool,
        expected_account_ids: Vec<u32>,
    ) {
        self.publish_match_inner(
            guild_id,
            pending_match_id,
            match_id,
            server_id,
            league,
            tv_delay_seconds(tv_delay),
            expected_account_ids,
            betting_closed,
        );
    }

    /// Synchronous registration helper for adapters that do not need the
    /// async host-facing method. GSI samples are accepted only when every
    /// expected account is present and no unexpected account is substituted.
    #[allow(clippy::too_many_arguments)]
    pub fn publish_match_with_expected_accounts(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        match_id: u64,
        server_id: Option<u64>,
        league: u32,
        expected_account_ids: Vec<u32>,
        betting_closed: bool,
    ) {
        self.publish_match_inner(
            guild_id,
            pending_match_id,
            match_id,
            server_id,
            league,
            self.configured_delay_seconds,
            expected_account_ids,
            betting_closed,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn publish_match_inner(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        match_id: u64,
        server_id: Option<u64>,
        league: u32,
        delay_seconds: Option<i64>,
        expected_account_ids: Vec<u32>,
        betting_closed: bool,
    ) {
        if match_id == 0 {
            return;
        }
        let expected_account_ids = expected_account_ids
            .into_iter()
            .filter(|id| *id > 0)
            .collect();
        if let Ok(mut matches) = self.matches.write() {
            let key = (guild_id, pending_match_id);
            if let Some(existing) = matches.get_mut(&key)
                && existing.match_id == match_id
            {
                // The host polls its lifecycle repeatedly. Keep the cached
                // sample and its timestamps when the same match is published
                // again, while refreshing mutable routing/roster metadata.
                existing.server_id = server_id;
                existing.league = league;
                existing.delay_seconds = delay_seconds;
                existing.expected_account_ids = expected_account_ids;
                // Betting closure is a one-way gate. A later recovery pass
                // must not reopen a registration that was already published.
                existing.betting_closed |= betting_closed;
                existing.active = true;
                return;
            }
            matches.insert(
                key,
                MatchRegistration {
                    guild_id,
                    pending_match_id,
                    match_id,
                    server_id,
                    league,
                    delay_seconds,
                    expected_account_ids,
                    betting_closed,
                    gameplay_started: false,
                    active: true,
                    published_at: now_unix(),
                    snapshot: None,
                },
            );
            trim_match_cache(&mut matches);
        }
    }

    /// Stop polling a match while retaining its last sample for post-match
    /// status and replay reconciliation.  The retained view is marked stale
    /// when returned by [`Self::snapshot`] or [`Self::summary`].
    pub async fn finish_match(&self, guild_id: i64, pending_match_id: i64) {
        if let Ok(mut matches) = self.matches.write()
            && let Some(registration) = matches.get_mut(&(guild_id, pending_match_id))
        {
            registration.active = false;
        }
    }

    /// Read the cached snapshot without making a network request.
    #[must_use]
    pub fn snapshot(&self, guild_id: i64, pending_match_id: i64) -> Option<LiveMatchSnapshot> {
        let matches = self.matches.read().ok()?;
        let registration = matches.get(&(guild_id, pending_match_id))?;
        if !registration.betting_closed {
            return None;
        }
        let mut snapshot = registration.snapshot.clone()?;
        snapshot.stale |= !registration.active;
        if now_unix().saturating_sub(snapshot.fetched_at)
            > i64::try_from(DOTA_LIVE_POLL_INTERVAL.as_secs().saturating_mul(3)).unwrap_or(i64::MAX)
        {
            snapshot.stale = true;
        }
        Some(snapshot)
    }

    /// Return whether a validated GSI update has observed the match in a
    /// playable game-rules state. The evidence remains true for the lifetime
    /// of this registration, including after it is marked inactive.
    #[must_use]
    pub fn gameplay_started(&self, guild_id: i64, pending_match_id: i64, match_id: u64) -> bool {
        self.matches
            .read()
            .ok()
            .and_then(|matches| matches.get(&(guild_id, pending_match_id)).cloned())
            .is_some_and(|registration| {
                registration.match_id == match_id && registration.gameplay_started
            })
    }

    /// Render a small Discord-friendly status from cached data.
    #[must_use]
    pub async fn summary(&self, guild_id: i64, pending_match_id: i64) -> Option<String> {
        let snapshot = self.snapshot(guild_id, pending_match_id)?;
        let mut parts = vec![format!("Live Dota match {}", snapshot.match_id)];
        if let Some(seconds) = snapshot.game_time_seconds {
            let sign = if seconds < 0 { "-" } else { "" };
            let absolute = seconds.unsigned_abs();
            parts.push(format!(
                "time {sign}{:02}:{:02}",
                absolute / 60,
                absolute % 60
            ));
        }
        if let (Some(radiant), Some(dire)) = (snapshot.radiant_score, snapshot.dire_score) {
            parts.push(format!("R {radiant}–{dire} D"));
        }
        if let Some(players) = &snapshot.players {
            parts.push(format!("{} players", players.len()));
        }
        parts.push(format!("source {}", snapshot.source.as_str()));
        let age = now_unix().saturating_sub(snapshot.fetched_at).max(0);
        parts.push(if snapshot.stale {
            format!("stale ({age}s old)")
        } else {
            format!("updated {age}s ago")
        });
        if let Some(delay) = snapshot.delay_seconds {
            parts.push(format!("delay {delay}s"));
        }
        Some(parts.join(" · "))
    }

    /// Build the configured live-feed workers.  Both workers are supervised by
    /// the normal runtime; hosting itself does not require either telemetry
    /// polling or the HTTP/GSI endpoint.
    #[must_use]
    pub fn workers(self: Arc<Self>) -> Vec<BackgroundWorkerSpec> {
        let mut workers = Vec::new();
        if self.web_api_key.is_some() {
            workers.push(BackgroundWorkerSpec::new(
                "dota-live-valve-poll",
                Arc::new(ValvePollingWorker {
                    feed: Arc::clone(&self),
                }),
            ));
        }
        if let Some(bind) = self.http_bind() {
            workers.push(BackgroundWorkerSpec::new(
                "dota-live-http",
                Arc::new(LiveHttpWorker {
                    feed: Arc::clone(&self),
                    bind,
                }),
            ));
        }
        workers
    }

    /// Poll every currently active registration once.  This is public for a
    /// deterministic startup/recovery check and for tests; the worker calls
    /// it at the configured 15-second cadence.
    pub async fn poll_once(&self) {
        let targets = self.poll_targets();
        for target in targets {
            match self.fetch_target(&target).await {
                Ok(Some(snapshot)) => self.store_snapshot(target.key(), snapshot),
                Ok(None) | Err(_) => self.mark_stale(target.key()),
            }
        }
    }

    /// Ingest a decoded GSI document.  The HTTP handler uses the byte-limited
    /// JSON path below; this method is also useful to an embedded GSI adapter.
    pub fn ingest_gsi(&self, payload: Value, observed_at: i64) -> Result<(), String> {
        self.ingest_gsi_inner(payload, observed_at)
            .map(|snapshot| {
                self.store_snapshot((snapshot.guild_id, snapshot.pending_match_id), snapshot)
            })
            .map_err(|error| error.to_string())
    }

    fn http_bind(&self) -> Option<SocketAddr> {
        self.live_bind
    }

    fn poll_targets(&self) -> Vec<PollTarget> {
        self.matches
            .read()
            .map(|matches| {
                matches
                    .values()
                    .filter(|registration| registration.active)
                    .map(PollTarget::from)
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn fetch_target(
        &self,
        target: &PollTarget,
    ) -> Result<Option<LiveMatchSnapshot>, UpstreamError> {
        let Some(key) = self.web_api_key.as_deref() else {
            return Ok(None);
        };
        let mut partial_realtime = None;
        if let Some(server_id) = target.server_id {
            let response = self
                .client
                .get(format!(
                    "{}/IDOTA2MatchStats_570/GetRealtimeStats/v1/",
                    self.upstream_base
                ))
                .query(&[("key", key), ("server_steam_id", &server_id.to_string())])
                .send()
                .await;
            // Unpublished private matches, outages, and malformed responses
            // must all leave the independent league feed available.
            if let Ok(response) = response
                && response.status().is_success()
                && let Ok(body) = read_limited_json(response).await
                && let Some(snapshot) = normalize_realtime_stats(
                    &body,
                    target.guild_id,
                    target.pending_match_id,
                    target.match_id,
                    now_unix(),
                )
            {
                let snapshot = self.apply_configured_delay(snapshot, target.delay_seconds);
                if snapshot.announcement_frame.is_some() {
                    return Ok(Some(snapshot));
                }
                // A partial realtime frame can still serve other live consumers,
                // but must not prevent the independent complete league fallback.
                partial_realtime = Some(snapshot);
            }
        }

        let league_result = async {
            let response = self
                .client
                .get(format!(
                    "{}/IDOTA2Match_570/GetLiveLeagueGames/v1/",
                    self.upstream_base
                ))
                .query(&[
                    ("key", key),
                    ("league_id", &target.league.to_string()),
                    ("match_id", &target.match_id.to_string()),
                ])
                .send()
                .await
                .map_err(|_| UpstreamError::Unavailable)?;
            if !response.status().is_success() {
                return Err(UpstreamError::Unavailable);
            }
            let body = read_limited_json(response).await?;
            Ok(normalize_live_league_games(
                &body,
                target.guild_id,
                target.pending_match_id,
                target.match_id,
                now_unix(),
            )
            .map(|snapshot| self.apply_configured_delay(snapshot, target.delay_seconds)))
        }
        .await;
        match league_result {
            Ok(Some(snapshot)) => Ok(Some(snapshot)),
            _ if partial_realtime.is_some() => Ok(partial_realtime),
            result => result,
        }
    }

    fn apply_configured_delay(
        &self,
        mut snapshot: LiveMatchSnapshot,
        match_delay_seconds: Option<i64>,
    ) -> LiveMatchSnapshot {
        let configured_delay_seconds = match_delay_seconds.or(self.configured_delay_seconds);
        if snapshot.delay_seconds.is_none()
            && let Some(delay) = configured_delay_seconds
        {
            snapshot.delay_seconds = Some(delay);
            snapshot.delay_source = Some(LiveDelaySource::Configured);
        }
        snapshot
    }

    fn store_snapshot(&self, key: MatchKey, mut snapshot: LiveMatchSnapshot) {
        if let Ok(mut matches) = self.matches.write()
            && let Some(registration) = matches.get_mut(&key)
            && registration.active
            && registration.match_id == snapshot.match_id
        {
            if let Some(existing) = registration.snapshot.as_ref()
                && existing.source == LiveSnapshotSource::Gsi
                && now_unix().saturating_sub(existing.fetched_at) <= GSI_FRESHNESS_SECONDS
                && snapshot.source != LiveSnapshotSource::Gsi
            {
                // Keep a fresh observer feed ahead of a thinner Valve sample
                // and avoid discarding it during a transient Valve outage.
                return;
            }
            snapshot.stale = false;
            registration.snapshot = Some(snapshot);
        }
    }

    fn mark_stale(&self, key: MatchKey) {
        if let Ok(mut matches) = self.matches.write()
            && let Some(registration) = matches.get_mut(&key)
            && let Some(snapshot) = registration.snapshot.as_mut()
        {
            if snapshot.source == LiveSnapshotSource::Gsi
                && now_unix().saturating_sub(snapshot.fetched_at) <= GSI_FRESHNESS_SECONDS
            {
                return;
            }
            snapshot.stale = true;
        }
    }

    fn ingest_gsi_inner(
        &self,
        payload: Value,
        observed_at: i64,
    ) -> Result<LiveMatchSnapshot, LiveIngestError> {
        let expected_token = self
            .gsi_token
            .as_deref()
            .ok_or(LiveIngestError::Unauthorized)?;
        let supplied_token = payload
            .get("auth")
            .and_then(Value::as_object)
            .and_then(|auth| auth.get("token"))
            .and_then(Value::as_str)
            .ok_or(LiveIngestError::Unauthorized)?;
        if supplied_token != expected_token {
            return Err(LiveIngestError::Unauthorized);
        }
        let map = payload
            .get("map")
            .and_then(Value::as_object)
            .ok_or(LiveIngestError::InvalidPayload)?;
        let match_id = map
            .get("matchid")
            .or_else(|| map.get("match_id"))
            .and_then(value_u64)
            .ok_or(LiveIngestError::InvalidPayload)?;
        let (key, registration) = {
            let matches = self
                .matches
                .read()
                .map_err(|_| LiveIngestError::UnknownMatch)?;
            matches
                .iter()
                .find(|(_, registration)| registration.active && registration.match_id == match_id)
                .map(|(key, registration)| (*key, registration.clone()))
                .ok_or(LiveIngestError::UnknownMatch)?
        };
        let players = parse_gsi_players(&payload);
        if !registration.expected_account_ids.is_empty() {
            let observed = players
                .as_ref()
                .into_iter()
                .flatten()
                .filter_map(|player| player.account_id)
                .collect::<BTreeSet<_>>();
            let has_unknown_account = players
                .as_ref()
                .into_iter()
                .flatten()
                .any(|player| player.account_id.is_none());
            if has_unknown_account || !observed.is_subset(&registration.expected_account_ids) {
                return Err(LiveIngestError::RosterMismatch);
            }
        }
        let game_state = parse_gsi_game_state(map);
        if game_state.is_some_and(gameplay_has_started) {
            // All source, identity, and frozen-roster checks have succeeded at
            // this point. Do not let an invalid or substituted source sample
            // create game-start evidence.
            let mut matches = self
                .matches
                .write()
                .map_err(|_| LiveIngestError::UnknownMatch)?;
            let current = matches
                .get_mut(&key)
                .filter(|registration| registration.active && registration.match_id == match_id)
                .ok_or(LiveIngestError::UnknownMatch)?;
            current.gameplay_started = true;
        }
        let source_delay = optional_i64(map, "delay");
        let configured_delay_seconds = registration.delay_seconds.or(self.configured_delay_seconds);
        Ok(LiveMatchSnapshot {
            guild_id: key.0,
            pending_match_id: key.1,
            match_id,
            source: LiveSnapshotSource::Gsi,
            observed_at,
            fetched_at: now_unix(),
            stale: false,
            delay_seconds: source_delay.or(configured_delay_seconds),
            delay_source: source_delay
                .map(|_| LiveDelaySource::Source)
                .or_else(|| configured_delay_seconds.map(|_| LiveDelaySource::Configured)),
            game_time_seconds: optional_i64(map, "game_time"),
            radiant_score: optional_i64(map, "radiant_score"),
            dire_score: optional_i64(map, "dire_score"),
            players,
            announcement_frame: None,
            map_frame: None,
        })
    }
}

#[derive(Clone, Debug)]
struct PollTarget {
    guild_id: i64,
    pending_match_id: i64,
    match_id: u64,
    server_id: Option<u64>,
    league: u32,
    delay_seconds: Option<i64>,
}

impl PollTarget {
    fn from(registration: &MatchRegistration) -> Self {
        Self {
            guild_id: registration.guild_id,
            pending_match_id: registration.pending_match_id,
            match_id: registration.match_id,
            server_id: registration.server_id,
            league: registration.league,
            delay_seconds: registration.delay_seconds,
        }
    }

    const fn key(&self) -> MatchKey {
        (self.guild_id, self.pending_match_id)
    }
}

struct ValvePollingWorker {
    feed: Arc<DotaLiveFeed>,
}

#[async_trait]
impl BackgroundWorker for ValvePollingWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        loop {
            self.feed.poll_once().await;
            if !context.sleep(DOTA_LIVE_POLL_INTERVAL).await {
                return Ok(());
            }
        }
    }
}

struct LiveHttpWorker {
    feed: Arc<DotaLiveFeed>,
    bind: SocketAddr,
}

#[async_trait]
impl BackgroundWorker for LiveHttpWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        let listener = TcpListener::bind(self.bind)
            .await
            .map_err(|_| "could not bind Dota live HTTP listener".to_owned())?;
        let app = Router::new()
            .route("/matches/{guild_id}/{pending_match_id}", get(get_match))
            .route("/gsi", post(post_gsi))
            .with_state(Arc::clone(&self.feed))
            .layer(axum::extract::DefaultBodyLimit::max(
                DOTA_LIVE_MAX_BODY_BYTES,
            ));
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { context.cancelled().await })
            .await
            .map_err(|_| "Dota live HTTP worker stopped".to_owned())
    }
}

async fn get_match(
    State(feed): State<Arc<DotaLiveFeed>>,
    AxumPath((guild_id, pending_match_id)): AxumPath<(i64, i64)>,
    headers: HeaderMap,
) -> Response {
    if !feed.authorize_viewer(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(snapshot) = feed.snapshot(guild_id, pending_match_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let age_seconds = now_unix().saturating_sub(snapshot.fetched_at).max(0);
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Json(LiveMatchResponse {
            snapshot,
            age_seconds,
        }),
    )
        .into_response()
}

async fn post_gsi(State(feed): State<Arc<DotaLiveFeed>>, body: Bytes) -> Response {
    let payload = match serde_json::from_slice::<Value>(&body) {
        Ok(payload) => payload,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    match feed.ingest_gsi_inner(payload, now_unix()) {
        Ok(snapshot) => {
            feed.store_snapshot((snapshot.guild_id, snapshot.pending_match_id), snapshot);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(LiveIngestError::Unauthorized) => StatusCode::UNAUTHORIZED.into_response(),
        Err(LiveIngestError::UnknownMatch) => StatusCode::NOT_FOUND.into_response(),
        Err(LiveIngestError::InvalidPayload | LiveIngestError::RosterMismatch) => {
            StatusCode::UNPROCESSABLE_ENTITY.into_response()
        }
    }
}

impl DotaLiveFeed {
    fn authorize_viewer(&self, headers: &HeaderMap) -> bool {
        let Some(expected) = self.live_token.as_deref() else {
            return false;
        };
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .is_some_and(|supplied| supplied == expected)
    }
}

async fn read_limited_json(mut response: reqwest::Response) -> Result<Value, UpstreamError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| UpstreamError::Unavailable)?
    {
        if body.len().saturating_add(chunk.len()) > DOTA_LIVE_MAX_BODY_BYTES {
            return Err(UpstreamError::BodyTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    parse_limited_json_bytes(&body)
}

fn parse_limited_json_bytes(body: &[u8]) -> Result<Value, UpstreamError> {
    if body.len() > DOTA_LIVE_MAX_BODY_BYTES {
        return Err(UpstreamError::BodyTooLarge);
    }
    serde_json::from_slice(body).map_err(|_| UpstreamError::InvalidJson)
}

/// Normalize a `GetRealtimeStats` response while enforcing the expected Valve
/// match identity.  A missing or mismatched ID returns `None` so callers can
/// use the league-games endpoint as a safe fallback.
pub fn normalize_realtime_stats(
    payload: &Value,
    guild_id: i64,
    pending_match_id: i64,
    expected_match_id: u64,
    observed_at: i64,
) -> Option<LiveMatchSnapshot> {
    let result = payload.get("result").unwrap_or(payload);
    if !live_match_identity_matches(result, expected_match_id) {
        return None;
    }
    let match_id = expected_match_id;
    Some(snapshot_from_scoreboard(
        result,
        guild_id,
        pending_match_id,
        match_id,
        LiveSnapshotSource::RealtimeStats,
        observed_at,
    ))
}

/// Normalize the matching entry from `GetLiveLeagueGames`.
pub fn normalize_live_league_games(
    payload: &Value,
    guild_id: i64,
    pending_match_id: i64,
    expected_match_id: u64,
    observed_at: i64,
) -> Option<LiveMatchSnapshot> {
    let result = payload.get("result").unwrap_or(payload);
    let games = result.get("games")?.as_array()?;
    let game = games
        .iter()
        .find(|game| live_match_identity_matches(game, expected_match_id))?;
    Some(snapshot_from_scoreboard(
        game,
        guild_id,
        pending_match_id,
        expected_match_id,
        LiveSnapshotSource::LiveLeagueGames,
        observed_at,
    ))
}

// Alias/nested identities must agree: accepting the first matching alias
// could attach another match's scoreboard to this spectator channel.
fn live_match_identity_matches(value: &Value, expected: u64) -> bool {
    let mut identities = [Some(value), value.get("match"), value.get("scoreboard")]
        .into_iter()
        .flatten()
        .flat_map(|value| [value.get("match_id"), value.get("matchid")])
        .flatten()
        .peekable();
    identities.peek().is_some() && identities.all(|value| value_u64(value) == Some(expected))
}

fn snapshot_from_scoreboard(
    value: &Value,
    guild_id: i64,
    pending_match_id: i64,
    match_id: u64,
    source: LiveSnapshotSource,
    observed_at: i64,
) -> LiveMatchSnapshot {
    let scoreboard = value.get("scoreboard").unwrap_or(value);
    let match_result = value
        .get("match")
        .filter(|match_result| match_result.is_object());
    let source_delay = first_i64(
        scoreboard,
        &["delay", "stream_delay_s", "tv_delay", "delay_seconds"],
    )
    .or_else(|| {
        first_i64(
            value,
            &["delay", "stream_delay_s", "tv_delay", "delay_seconds"],
        )
    })
    .or_else(|| {
        match_result.and_then(|value| first_i64(value, &["delay", "tv_delay", "delay_seconds"]))
    });
    let players = scoreboard
        .get("players")
        .or_else(|| value.get("allplayers"))
        .and_then(parse_player_collection)
        .filter(|players| !players.is_empty())
        .or_else(|| parse_team_scoreboard_players(scoreboard))
        .or_else(|| parse_team_array_players(scoreboard.get("teams")))
        .or_else(|| parse_team_array_players(value.get("teams")))
        .or_else(|| {
            match_result
                .and_then(|value| value.get("players"))
                .and_then(parse_player_collection)
        });
    let game_time_seconds = first_game_time(scoreboard)
        .or_else(|| first_game_time(value))
        .or_else(|| match_result.and_then(first_game_time));
    let radiant_score = first_i64(scoreboard, &["radiant_score", "radiant_team_score"])
        .or_else(|| first_i64(value, &["radiant_score", "radiant_team_score"]))
        .or_else(|| nested_i64(scoreboard, "radiant", "score"))
        .or_else(|| nested_i64(value, "radiant", "score"))
        .or_else(|| team_array_score(scoreboard.get("teams"), 2))
        .or_else(|| team_array_score(value.get("teams"), 2));
    let dire_score = first_i64(scoreboard, &["dire_score", "dire_team_score"])
        .or_else(|| first_i64(value, &["dire_score", "dire_team_score"]))
        .or_else(|| nested_i64(scoreboard, "dire", "score"))
        .or_else(|| nested_i64(value, "dire", "score"))
        .or_else(|| team_array_score(scoreboard.get("teams"), 3))
        .or_else(|| team_array_score(value.get("teams"), 3));
    let announcement_frame = announcement_frame(
        value,
        match_id,
        game_time_seconds,
        radiant_score,
        dire_score,
    );
    let map_frame = announcement_frame
        .as_ref()
        .and_then(|frame| map_frame(value, frame));
    LiveMatchSnapshot {
        guild_id,
        pending_match_id,
        match_id,
        source,
        observed_at,
        fetched_at: now_unix(),
        stale: false,
        delay_seconds: source_delay,
        delay_source: source_delay.map(|_| LiveDelaySource::Source),
        game_time_seconds,
        radiant_score,
        dire_score,
        players,
        announcement_frame,
        map_frame,
    }
}

fn announcement_frame(
    value: &Value,
    match_id: u64,
    game_time: Option<i64>,
    radiant_score: Option<i64>,
    dire_score: Option<i64>,
) -> Option<cama_domain::live_announcements::LiveAnnouncementFrame> {
    use cama_domain::live_announcements::{LiveAnnouncementFrame, LiveBuilding};
    // Delta frames require reconstruction before any event analysis.
    if value
        .get("delta_frame")
        .is_some_and(|delta| delta.as_bool() != Some(false))
    {
        return None;
    }
    let scoreboard = value.get("scoreboard").unwrap_or(value);
    if scoreboard
        .get("delta_frame")
        .is_some_and(|delta| delta.as_bool() != Some(false))
    {
        return None;
    }
    let teams = [
        announcement_team(scoreboard, 2),
        announcement_team(scoreboard, 3),
    ];
    let mut players = Vec::new();
    let mut worth = [None, None];
    for (index, team) in teams.into_iter().enumerate() {
        let Some(team) = team else { continue };
        let team_players = announcement_players(team, index == 0);
        // A supplied but invalid aggregate is not permission to substitute a
        // different metric. Never sum a partial roster or missing player gold.
        worth[index] = match team.get("net_worth") {
            Some(value) => value_i64(value).filter(|worth| *worth >= 0),
            None if team_players.len() == 5 => team
                .get("players")
                .and_then(Value::as_array)
                .and_then(|players| {
                    players.iter().try_fold(0_i64, |sum, player| {
                        let worth = player.get("net_worth").and_then(value_i64)?;
                        (worth >= 0).then_some(())?;
                        sum.checked_add(worth)
                    })
                }),
            None => None,
        };
        players.extend(team_players);
    }
    // Duplicate hero/account identities must not manufacture a complete
    // ten-player baseline for first blood. Invalidate ambiguous rosters.
    let mut hero_ids = BTreeSet::new();
    let mut account_ids = BTreeSet::new();
    let duplicate_accounts = teams.into_iter().flatten().any(|team| {
        team.get("players")
            .and_then(Value::as_array)
            .is_some_and(|players| {
                players
                    .iter()
                    .filter_map(|player| parse_player(player, None).account_id)
                    .any(|account| !account_ids.insert(account))
            })
    });
    if duplicate_accounts
        || players
            .iter()
            .any(|player| !hero_ids.insert(player.hero_id))
    {
        players.clear();
        // Explicit team totals stand independently of the individual roster.
        for (index, team) in teams.into_iter().enumerate() {
            if team.is_some_and(|team| team.get("net_worth").is_none()) {
                worth[index] = None;
            }
        }
    }
    let mut buildings: Vec<LiveBuilding> = value
        .get("buildings")
        .and_then(Value::as_array)
        .map(|buildings| {
            buildings
                .iter()
                .take(64)
                .filter_map(|building| {
                    let team = first_i64(building, &["team"])?;
                    if ![2, 3].contains(&team) {
                        return None;
                    }
                    let destroyed = building.get("destroyed")?.as_bool()?;
                    let x = building.get("x")?.as_f64()?;
                    let y = building.get("y")?.as_f64()?;
                    Some(LiveBuilding {
                        key: format!("{team}:{x}:{y}"),
                        radiant: team == 2,
                        destroyed,
                        name: announcement_building_name(building),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if buildings.is_empty() {
        // League scoreboards report surviving towers/barracks as bitmasks.
        // Decode the documented single-team masks (not the combined mask).
        // Missing/malformed masks are not zero (all destroyed).
        for (index, team) in teams.into_iter().enumerate() {
            let Some(team) = team else { continue };
            for (field, bits) in [("tower_state", 11), ("barracks_state", 6)] {
                let Some(mask) = team
                    .get(field)
                    .and_then(value_u64)
                    .filter(|mask| *mask < (1 << bits))
                else {
                    continue;
                };
                for bit in 0..bits {
                    buildings.push(LiveBuilding {
                        key: format!("league:{index}:{field}:{bit}"),
                        radiant: index == 0,
                        destroyed: mask & (1 << bit) == 0,
                        name: Some(league_building_name(field, bit)),
                    });
                }
            }
        }
    }
    Some(LiveAnnouncementFrame {
        match_id,
        game_time: game_time?,
        radiant_score,
        dire_score,
        radiant_net_worth: worth[0],
        dire_net_worth: worth[1],
        buildings,
        players,
        // GetRealtimeStats' terse schema and LiveLeagueGames provide no
        // verified first-blood/fight/Roshan log or winner. Do not accept a
        // made-up `events` or `radiant_win` field from a compatible proxy.
        events: Vec::new(),
        radiant_win: None,
    })
}

fn map_frame(
    value: &Value,
    frame: &cama_domain::live_announcements::LiveAnnouncementFrame,
) -> Option<LiveMapFrame> {
    // The normalizer must establish an unambiguous complete roster first.
    // Never place unknown players, merge sources, or carry old locations ahead.
    if frame.players.len() != 10 {
        return None;
    }
    let scoreboard = value.get("scoreboard").unwrap_or(value);
    let mut heroes = Vec::new();
    for number in [2, 3] {
        let team = announcement_team(scoreboard, number)?;
        for value in team.get("players")?.as_array()? {
            let hero_id = parse_player(value, None).hero_id?;
            if !frame
                .players
                .iter()
                .any(|hero| hero.hero_id == hero_id && hero.radiant == (number == 2))
            {
                return None;
            }
            // Captured GetLiveLeagueGames uses world-space position_x/y.
            // Do not assume other sources' x/y use this coordinate system.
            let position =
                coordinate(value.get("position_x")).zip(coordinate(value.get("position_y")));
            let (x, y) = position.map_or((None, None), |(x, y)| (Some(x), Some(y)));
            heroes.push(LiveMapHero {
                hero_id,
                radiant: number == 2,
                x,
                y,
                respawn_seconds: value
                    .get("respawn_timer")
                    .and_then(value_i64)
                    .filter(|n| (0..=600).contains(n)),
            });
        }
    }
    // An all-zero frame is a common upstream placeholder, not ten heroes mid.
    if heroes.is_empty()
        || heroes
            .iter()
            .all(|hero| hero.x.zip(hero.y).is_none_or(|(x, y)| x == 0.0 && y == 0.0))
    {
        return None;
    }
    let buildings = frame
        .buildings
        .iter()
        .map(|building| LiveMapBuilding {
            radiant: building.radiant,
            name: building.name.clone().unwrap_or_else(|| "structure".into()),
            destroyed: building.destroyed,
            // League status bits have no positions. Keep identity/status in the
            // map legend until this source supplies verified world coordinates.
            x: None,
            y: None,
        })
        .collect();
    Some(LiveMapFrame {
        match_id: frame.match_id,
        game_time: frame.game_time,
        heroes,
        buildings,
        roshan_respawn_seconds: scoreboard
            .get("roshan_respawn_timer")
            .and_then(value_i64)
            .filter(|n| (1..=660).contains(n)),
    })
}
fn coordinate(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && (-16384.0..=16384.0).contains(v))
}

// Single-team wire layout, documented by the API library's own reference:
// https://demodota2api.readthedocs.io/en/latest/responses.html#single-team-tower-status
// Bits 0..8 are top/middle/bottom T1..T3. Bits 9/10 guard the Ancient.
fn league_building_name(field: &str, bit: u32) -> String {
    if field == "tower_state" {
        if bit < 9 {
            let lane = ["top", "mid", "bottom"][(bit / 3) as usize];
            format!("{lane} tier {} tower", bit % 3 + 1)
        } else {
            format!(
                "{} tier 4 tower",
                if bit == 9 {
                    "upper Ancient"
                } else {
                    "lower Ancient"
                }
            )
        }
    } else {
        let lane = ["top", "mid", "bottom"][(bit / 2) as usize];
        format!(
            "{lane} {} barracks",
            if bit.is_multiple_of(2) {
                "melee"
            } else {
                "ranged"
            }
        )
    }
}

fn announcement_team(value: &Value, number: i64) -> Option<&Value> {
    if let Some(teams) = value.get("teams").and_then(Value::as_array) {
        let mut matches = teams
            .iter()
            .filter(|team| first_i64(team, &["team_number", "team"]) == Some(number));
        let team = matches.next()?;
        return matches.next().is_none().then_some(team);
    }
    value
        .get(if number == 2 { "radiant" } else { "dire" })
        .filter(|team| team.is_object())
}

fn announcement_players(
    team: &Value,
    radiant: bool,
) -> Vec<cama_domain::live_announcements::LiveHero> {
    use cama_domain::live_announcements::LiveHero;
    let Some(players) = team.get("players").and_then(Value::as_array) else {
        return Vec::new();
    };
    if players.len() > 5 {
        return Vec::new();
    }
    let mut slots = BTreeSet::new();
    let mut player_slots = BTreeSet::new();
    players
        .iter()
        .filter_map(|value| {
            let number = if radiant { 2 } else { 3 };
            if value
                .get("team")
                .is_some_and(|team| value_i64(team) != Some(number))
            {
                return None;
            }
            if let Some(slot) = value.get("team_slot") {
                let slot = value_i64(slot)?;
                if !(0..=4).contains(&slot) || !slots.insert(slot) {
                    return None;
                }
            }
            if let Some(slot) = value.get("player_slot") {
                let slot = value_i64(slot)?;
                let base = if radiant { 0 } else { 128 };
                if !(base..=base + 4).contains(&slot) || !player_slots.insert(slot) {
                    return None;
                }
            }
            let player = parse_player(value, None);
            let hero_id = player.hero_id?;
            Some(LiveHero {
                hero_id,
                account_id: player.account_id,
                display_name: None,
                // Source `name` is a player's Steam name, not a hero identity.
                // Resolve the trusted local hero table to avoid mentions/markup.
                name: cama_app::hero_lookup::hero_name(i64::from(hero_id)),
                radiant,
                kills: player.kills.filter(|kills| *kills >= 0),
                deaths: player.deaths.filter(|deaths| *deaths >= 0),
            })
        })
        .collect()
}

fn announcement_building_name(building: &Value) -> Option<String> {
    // Terse type IDs: tower=0, barracks=1, Ancient=2. Cross-checked against
    // 13k/night-stalker, fe78ce7ec036, protobuf/protocol/enums.proto.
    // The wire schema does not distinguish melee/ranged barracks. Its lane
    // numbers are not the hero-role ELaneType enum: do not guess a lane or
    // identify a destroyed zeroed tombstone from coordinates/array position.
    match first_i64(building, &["type"])? {
        0 => {
            let tier = first_i64(building, &["tier"])?;
            (1..=4)
                .contains(&tier)
                .then(|| format!("tier {tier} tower"))
        }
        1 => Some("barracks".to_owned()),
        2 => Some("Ancient".to_owned()),
        _ => None,
    }
}

fn parse_team_scoreboard_players(value: &Value) -> Option<Vec<LivePlayerSnapshot>> {
    let mut players = Vec::new();
    for team in ["radiant", "dire"] {
        let Some(team_players) = value
            .get(team)
            .and_then(|team| team.get("players"))
            .and_then(parse_player_collection)
        else {
            continue;
        };
        players.extend(team_players);
    }
    (!players.is_empty()).then_some(players)
}

fn parse_team_array_players(value: Option<&Value>) -> Option<Vec<LivePlayerSnapshot>> {
    let teams = value?.as_array()?;
    let mut players = Vec::new();
    for team in teams {
        let Some(team_players) = team.get("players").and_then(parse_player_collection) else {
            continue;
        };
        players.extend(team_players);
    }
    (!players.is_empty()).then_some(players)
}

fn team_array_score(value: Option<&Value>, team_number: i64) -> Option<i64> {
    value?.as_array()?.iter().find_map(|team| {
        let number = object_value(team, "team_number")
            .or_else(|| object_value(team, "team"))
            .and_then(value_i64);
        (number == Some(team_number))
            .then(|| object_value(team, "score").and_then(value_i64))
            .flatten()
    })
}

fn parse_player_collection(value: &Value) -> Option<Vec<LivePlayerSnapshot>> {
    if let Some(players) = value.as_array() {
        return Some(
            players
                .iter()
                .map(|player| parse_player(player, None))
                .collect(),
        );
    }
    let object = value.as_object()?;
    Some(
        object
            .iter()
            .map(|(account, player)| parse_player(player, value_account_id_text(account)))
            .collect(),
    )
}

fn parse_gsi_players(payload: &Value) -> Option<Vec<LivePlayerSnapshot>> {
    if let Some(players) = payload.get("allplayers").and_then(parse_player_collection)
        && !players.is_empty()
    {
        return Some(players);
    }
    let player = payload.get("player")?;
    if let Some(players) = parse_gsi_team_players(player, payload.get("hero"), payload.get("items"))
    {
        return Some(players);
    }
    Some(vec![parse_player(player, None)])
}

fn parse_player(value: &Value, account_hint: Option<u32>) -> LivePlayerSnapshot {
    let stats = value
        .get("player")
        .filter(|player| player.is_object())
        .unwrap_or(value);
    let account_id = object_value(stats, "account_id")
        .or_else(|| object_value(stats, "accountid"))
        .or_else(|| object_value(stats, "steamid"))
        .or_else(|| object_value(stats, "steam_id"))
        .and_then(value_account_id)
        .or(account_hint);
    let hero = value.get("hero").or_else(|| stats.get("hero"));
    let hero_id = object_value(stats, "hero_id")
        .or_else(|| object_value(stats, "heroid"))
        .or_else(|| hero.and_then(|hero| object_value(hero, "id")))
        .and_then(value_u32);
    let hero_name = object_value(stats, "hero_name")
        .or_else(|| hero.and_then(|hero| object_value(hero, "name")))
        .and_then(Value::as_str)
        .map(str::to_owned);
    LivePlayerSnapshot {
        account_id,
        hero_id,
        hero_name,
        kills: optional_i64_from_value(stats, "kills")
            .or_else(|| optional_i64_from_value(stats, "kill_count")),
        deaths: optional_i64_from_value(stats, "deaths")
            .or_else(|| optional_i64_from_value(stats, "death_count"))
            .or_else(|| optional_i64_from_value(stats, "death")),
        assists: optional_i64_from_value(stats, "assists")
            .or_else(|| optional_i64_from_value(stats, "assists_count")),
        last_hits: optional_i64_from_value(stats, "last_hits")
            .or_else(|| optional_i64_from_value(stats, "lh_count")),
        denies: optional_i64_from_value(stats, "denies")
            .or_else(|| optional_i64_from_value(stats, "denies_count")),
        gold: optional_i64_from_value(stats, "gold"),
        net_worth: optional_i64_from_value(stats, "net_worth")
            .or_else(|| optional_i64_from_value(stats, "net_gold")),
        items: parse_items(value.get("items").or_else(|| stats.get("items")))
            .or_else(|| parse_inline_items(stats)),
    }
}

fn parse_gsi_team_players(
    value: &Value,
    hero: Option<&Value>,
    items: Option<&Value>,
) -> Option<Vec<LivePlayerSnapshot>> {
    let teams = value.as_object()?;
    let has_team_slots = teams.values().any(|team| {
        team.as_object()
            .is_some_and(|players| players.keys().any(|slot| slot.starts_with("player")))
    });
    if !has_team_slots {
        return None;
    }
    let mut result = Vec::new();
    for (team, team_players) in teams {
        let Some(team_players) = team_players.as_object() else {
            continue;
        };
        for (slot, value) in team_players {
            if !value.is_object() {
                continue;
            }
            let mut player = parse_player(value, None);
            if let Some(hero) = nested_team_slot(hero, team, slot) {
                merge_hero(&mut player, hero);
            }
            if player.items.is_none() {
                player.items =
                    nested_team_slot(items, team, slot).and_then(|items| parse_items(Some(items)));
            }
            result.push(player);
        }
    }
    Some(result)
}

fn nested_team_slot<'a>(value: Option<&'a Value>, team: &str, slot: &str) -> Option<&'a Value> {
    value?.as_object()?.get(team)?.as_object()?.get(slot)
}

fn merge_hero(player: &mut LivePlayerSnapshot, hero: &Value) {
    if player.hero_id.is_none() {
        player.hero_id = object_value(hero, "id").and_then(value_u32);
    }
    if player.hero_name.is_none() {
        player.hero_name = object_value(hero, "name")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
}

fn parse_items(value: Option<&Value>) -> Option<Vec<LiveItemSnapshot>> {
    let value = value?;
    if let Some(items) = value.as_array() {
        return Some(items.iter().map(parse_item).collect());
    }
    let object = value.as_object()?;
    Some(object.iter().map(|(_, item)| parse_item(item)).collect())
}

fn parse_item(value: &Value) -> LiveItemSnapshot {
    let id = object_value(value, "item_id")
        .or_else(|| object_value(value, "id"))
        .and_then(value_u32);
    let name = object_value(value, "name")
        .and_then(Value::as_str)
        .map(str::to_owned);
    LiveItemSnapshot {
        id: id.or_else(|| value_u32(value)),
        name,
        charges: optional_i64_from_value(value, "charges"),
    }
}

fn parse_inline_items(value: &Value) -> Option<Vec<LiveItemSnapshot>> {
    let object = value.as_object()?;
    let items = object
        .iter()
        .filter(|(key, _)| key.starts_with("item"))
        .map(|(_, value)| parse_item(value))
        .filter(|item| item.id.is_some() || item.name.is_some() || item.charges.is_some())
        .collect::<Vec<_>>();
    (!items.is_empty()).then_some(items)
}

fn object_value<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_object()?.get(key)
}

fn optional_i64(object: &Map<String, Value>, key: &str) -> Option<i64> {
    object.get(key).and_then(value_i64)
}

fn optional_i64_from_value(value: &Value, key: &str) -> Option<i64> {
    object_value(value, key).and_then(value_i64)
}

fn nested_i64(value: &Value, parent: &str, key: &str) -> Option<i64> {
    value
        .get(parent)
        .and_then(|parent| object_value(parent, key))
        .and_then(value_i64)
}

fn first_i64(value: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| object_value(value, key).and_then(value_i64))
}

// Valve's league scoreboard duration is a floating-point seconds value.
// Floor only clock fields; identities and counters must remain exact integers.
fn first_game_time(value: &Value) -> Option<i64> {
    ["game_time", "duration", "game_time_seconds"]
        .iter()
        .find_map(|key| {
            let value = object_value(value, key)?;
            value_i64(value).or_else(|| {
                let seconds = value.as_f64()?;
                (seconds.is_finite() && (-86_400.0..=86_400.0).contains(&seconds))
                    .then(|| seconds.floor() as i64)
            })
        })
}

fn value_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn value_u32(value: &Value) -> Option<u32> {
    value_u64(value)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
}

fn value_account_id(value: &Value) -> Option<u32> {
    account_id_from_raw(value_u64(value)?)
}

fn account_id_from_raw(raw: u64) -> Option<u32> {
    if raw <= u64::from(u32::MAX) {
        return u32::try_from(raw).ok().filter(|id| *id > 0);
    }
    dota_account_id(i64::try_from(raw).ok()?)
}

fn value_account_id_text(value: &str) -> Option<u32> {
    let raw = value.parse::<u64>().ok()?;
    account_id_from_raw(raw)
}

/// Decode the optional GSI `map.game_state` value without assigning a
/// default. GSI normally sends Valve's symbolic names, but accepting the raw
/// numeric wire value makes the start signal work with relays and fixtures
/// that preserve the protobuf enum directly.
fn parse_gsi_game_state(map: &Map<String, Value>) -> Option<i32> {
    let value = map.get("game_state")?;
    match value {
        Value::Number(value) => value.as_i64().and_then(|value| i32::try_from(value).ok()),
        Value::String(value) => match value.as_str() {
            "DOTA_GAMERULES_STATE_INIT" => Some(0),
            "DOTA_GAMERULES_STATE_WAIT_FOR_PLAYERS_TO_LOAD" => Some(1),
            "DOTA_GAMERULES_STATE_HERO_SELECTION" => Some(2),
            "DOTA_GAMERULES_STATE_STRATEGY_TIME" => Some(3),
            "DOTA_GAMERULES_STATE_PRE_GAME" => Some(4),
            "DOTA_GAMERULES_STATE_GAME_IN_PROGRESS" => Some(5),
            "DOTA_GAMERULES_STATE_POST_GAME" => Some(6),
            "DOTA_GAMERULES_STATE_DISCONNECT" => Some(7),
            "DOTA_GAMERULES_STATE_TEAM_SHOWCASE" => Some(8),
            "DOTA_GAMERULES_STATE_CUSTOM_GAME_SETUP" => Some(9),
            "DOTA_GAMERULES_STATE_WAIT_FOR_MAP_TO_LOAD" => Some(10),
            "DOTA_GAMERULES_STATE_SCENARIO_SETUP" => Some(11),
            "DOTA_GAMERULES_STATE_PLAYER_DRAFT" => Some(12),
            "DOTA_GAMERULES_STATE_LAST" => Some(13),
            value => value.parse::<i32>().ok(),
        },
        Value::Bool(_) | Value::Array(_) | Value::Object(_) | Value::Null => None,
    }
}

fn value_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn tv_delay_seconds(value: u32) -> Option<i64> {
    match value {
        0 => Some(10),
        1 => Some(60),
        2 => Some(120),
        3 => Some(300),
        4 => Some(900),
        _ => None,
    }
}

fn trim_match_cache(matches: &mut BTreeMap<MatchKey, MatchRegistration>) {
    while matches.len() > DOTA_LIVE_MAX_MATCHES {
        let candidate = matches
            .iter()
            .filter(|(_, registration)| !registration.active)
            .min_by_key(|(_, registration)| registration.published_at)
            .map(|(key, _)| *key)
            .or_else(|| {
                matches
                    .iter()
                    .min_by_key(|(_, registration)| registration.published_at)
                    .map(|(key, _)| *key)
            });
        let Some(candidate) = candidate else { break };
        matches.remove(&candidate);
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "dota_live/tests.rs"]
mod tests;
