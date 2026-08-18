//! Production OpenDota HTTP transport and typed application adapters.
//!
//! The transport is deliberately fail-soft at the remote boundary: HTTP
//! failures, oversized bodies, and malformed JSON become `None` (or an empty
//! profile component), matching the Python integration. Network work runs on
//! a dedicated four-thread Tokio runtime so slow OpenDota calls cannot consume
//! the runtime used for SQLite or Discord work.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use cama_domain::role_derivation::FARM_PRIORITY_MINUTE;
use chrono::Utc;
use reqwest::{Client, Response, StatusCode};
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::runtime::{Builder, Runtime};
use tokio::sync::Mutex;

use crate::match_discovery::{
    DiscoveryPortError, DotabuffIdExtractionPort, EnrichedParticipantStats, FantasyStats,
    OpenDotaDiscoveryPort, OpenDotaMatchDetails, OpenDotaPlayer, PlayerHistoryMatch, SteamId,
    ValveMatchId, WrappedPlayerTelemetry,
};
use crate::opendota_player_service::{
    HeroMetadata, OpenDotaPlayerApiPort, OpenDotaPlayerPortError, OpenDotaPlayerService,
    PlayerIdentity, ProfileAverages, ProjectedMatch, RecordedHeroCatalog,
    SystemOpenDotaPlayerClock, TopHero, WinLoss,
};
use crate::player_mmr_fallback::{
    OpenDotaMmrValue, OpenDotaPlayerData, OpenDotaRegistrationPort, get_player_mmr_from_data,
};

pub const DEFAULT_BASE_URL: &str = "https://api.opendota.com/api";
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_REQUEST_DEADLINE: Duration = Duration::from_secs(90);
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;
pub const DEFAULT_RATE_LIMIT_WAIT: Duration = Duration::from_secs(30);
pub const ANONYMOUS_REQUESTS_PER_MINUTE: u32 = 60;
pub const AUTHENTICATED_REQUESTS_PER_MINUTE: u32 = 1_200;

const DEFAULT_RETRY_DELAYS_SECONDS: &[u64] = &[1, 5, 20, 60, 180];
const RETRYABLE_STATUS_CODES: &[StatusCode] = &[
    StatusCode::TOO_MANY_REQUESTS,
    StatusCode::INTERNAL_SERVER_ERROR,
    StatusCode::BAD_GATEWAY,
    StatusCode::SERVICE_UNAVAILABLE,
    StatusCode::GATEWAY_TIMEOUT,
];

pub type OpenDotaRequestObserver = Arc<dyn Fn(&str) + Send + Sync>;

/// Complete production policy for one shared OpenDota transport.
#[derive(Clone)]
pub struct OpenDotaHttpConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub request_timeout: Duration,
    pub request_deadline: Duration,
    pub max_response_bytes: usize,
    pub rate_limit_wait: Duration,
    pub requests_per_minute: u32,
    pub retry_delays: Vec<Duration>,
    /// Optional bundled hero names used by the profile adapter.
    pub hero_names: BTreeMap<i64, String>,
    /// Process-level monitoring hook invoked immediately before each actual
    /// HTTP attempt (including retries). Rate-limit rejection invokes no hook,
    /// matching Python's request-attempt counter.
    pub request_observer: Option<OpenDotaRequestObserver>,
}

impl fmt::Debug for OpenDotaHttpConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenDotaHttpConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("request_timeout", &self.request_timeout)
            .field("request_deadline", &self.request_deadline)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("rate_limit_wait", &self.rate_limit_wait)
            .field("requests_per_minute", &self.requests_per_minute)
            .field("retry_delays", &self.retry_delays)
            .field("hero_name_count", &self.hero_names.len())
            .field("request_observer", &self.request_observer.is_some())
            .finish()
    }
}

impl OpenDotaHttpConfig {
    #[must_use]
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        let api_key = api_key.filter(|value| !value.is_empty());
        let requests_per_minute = if api_key.is_some() {
            AUTHENTICATED_REQUESTS_PER_MINUTE
        } else {
            ANONYMOUS_REQUESTS_PER_MINUTE
        };
        Self {
            base_url: base_url.into(),
            api_key,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            request_deadline: DEFAULT_REQUEST_DEADLINE,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            rate_limit_wait: DEFAULT_RATE_LIMIT_WAIT,
            requests_per_minute,
            retry_delays: DEFAULT_RETRY_DELAYS_SECONDS
                .iter()
                .copied()
                .map(Duration::from_secs)
                .collect(),
            hero_names: BTreeMap::new(),
            request_observer: None,
        }
    }

    #[must_use]
    pub fn with_request_observer(
        mut self,
        observer: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        self.request_observer = Some(Arc::new(observer));
        self
    }

    #[must_use]
    pub fn production(api_key: Option<String>) -> Self {
        let mut config = Self::new(DEFAULT_BASE_URL, api_key);
        config.hero_names = bundled_hero_names();
        config
    }
}

impl Default for OpenDotaHttpConfig {
    fn default() -> Self {
        Self::production(None)
    }
}

#[derive(Debug, Error)]
pub enum OpenDotaHttpBuildError {
    #[error("failed to build the OpenDota HTTP client: {0}")]
    Http(#[from] reqwest::Error),
    #[error("failed to build the dedicated OpenDota executor: {0}")]
    Executor(#[from] std::io::Error),
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("the dedicated OpenDota executor stopped unexpectedly")]
pub struct OpenDotaHttpExecutorError;

trait RetrySleeper: Send + Sync {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

struct TokioRetrySleeper;

impl RetrySleeper for TokioRetrySleeper {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(duration))
    }
}

#[derive(Debug)]
struct TokenBucketState {
    tokens: f64,
    last_update: Instant,
}

#[derive(Debug)]
struct TokenBucket {
    capacity: f64,
    tokens_per_second: f64,
    state: Mutex<TokenBucketState>,
}

static SHARED_RATE_LIMITER: OnceLock<Arc<TokenBucket>> = OnceLock::new();
static OPENDOTA_EXECUTOR: OnceLock<Arc<Runtime>> = OnceLock::new();
static OPENDOTA_EXECUTOR_INIT: StdMutex<()> = StdMutex::new(());
static BUNDLED_HERO_NAMES: OnceLock<BTreeMap<i64, String>> = OnceLock::new();

fn bundled_hero_names() -> BTreeMap<i64, String> {
    BUNDLED_HERO_NAMES
        .get_or_init(|| {
            serde_json::from_str::<BTreeMap<String, String>>(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../../utils/heroes.json"
            )))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(id, name)| id.parse().ok().map(|id| (id, name)))
            .collect()
        })
        .clone()
}

impl TokenBucket {
    fn new(requests_per_minute: u32) -> Self {
        let requests_per_minute = requests_per_minute.max(1);
        Self {
            capacity: f64::from(requests_per_minute),
            tokens_per_second: f64::from(requests_per_minute) / 60.0,
            state: Mutex::new(TokenBucketState {
                tokens: f64::from(requests_per_minute),
                last_update: Instant::now(),
            }),
        }
    }

    async fn acquire(&self, timeout: Duration) -> bool {
        let started_at = Instant::now();
        loop {
            {
                let mut state = self.state.lock().await;
                let now = Instant::now();
                let elapsed = now.duration_since(state.last_update).as_secs_f64();
                state.tokens = (state.tokens + elapsed * self.tokens_per_second).min(self.capacity);
                state.last_update = now;
                if state.tokens >= 1.0 {
                    state.tokens -= 1.0;
                    return true;
                }
            }

            let elapsed = started_at.elapsed();
            if elapsed >= timeout {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(100).min(timeout - elapsed)).await;
        }
    }
}

/// One cloneable production client implementing the profile, registration,
/// discovery, enrichment, role, and match-history ports.
#[derive(Clone)]
pub struct OpenDotaHttpClient {
    http: Client,
    config: Arc<OpenDotaHttpConfig>,
    limiter: Arc<TokenBucket>,
    executor: Arc<Runtime>,
    sleeper: Arc<dyn RetrySleeper>,
}

impl fmt::Debug for OpenDotaHttpClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenDotaHttpClient")
            .field(
                "api_key",
                &self.config.api_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("request_timeout", &self.config.request_timeout)
            .field("request_deadline", &self.config.request_deadline)
            .field("max_response_bytes", &self.config.max_response_bytes)
            .field("requests_per_minute", &self.config.requests_per_minute)
            .field("retry_delays", &self.config.retry_delays)
            .field("hero_name_count", &self.config.hero_names.len())
            .finish_non_exhaustive()
    }
}

/// Application-level production composition root for every OpenDota consumer.
///
/// Each accessor returns a cheap clone of the same logical client: reqwest's
/// connection pool, the token bucket, retry policy, and the dedicated executor
/// remain shared. The concrete return type implements all of the typed ports,
/// so callers cannot accidentally receive a marker or test double.
#[derive(Clone, Debug)]
pub struct OpenDotaRuntimeServices {
    client: OpenDotaHttpClient,
}

impl OpenDotaRuntimeServices {
    /// Construct the shared production transport from environment settings.
    pub fn from_env() -> Result<Self, OpenDotaHttpBuildError> {
        OpenDotaHttpClient::from_env().map(Self::new)
    }

    /// Construct the production transport from an injectable environment
    /// lookup. Runtime tests use this without mutating process-global state.
    pub fn from_lookup(
        lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, OpenDotaHttpBuildError> {
        OpenDotaHttpClient::from_lookup(lookup).map(Self::new)
    }

    /// Construct from the runtime's single immutable configuration snapshot.
    /// Negative retry delays are rejected by the HTTP duration type and fall
    /// back to the canonical production sequence, matching `from_env`'s
    /// fail-soft handling of malformed delay configuration.
    pub fn from_production_settings(
        api_key: Option<String>,
        retry_delays_seconds: &[i64],
    ) -> Result<Self, OpenDotaHttpBuildError> {
        let mut config = OpenDotaHttpConfig::production(api_key);
        if retry_delays_seconds.iter().all(|delay| *delay >= 0) {
            config.retry_delays = retry_delays_seconds
                .iter()
                .map(|delay| Duration::from_secs(*delay as u64))
                .collect();
        }
        Self::with_config(config)
    }

    #[must_use]
    pub const fn new(client: OpenDotaHttpClient) -> Self {
        Self { client }
    }

    pub fn with_config(config: OpenDotaHttpConfig) -> Result<Self, OpenDotaHttpBuildError> {
        OpenDotaHttpClient::with_config(config).map(Self::new)
    }

    /// Attach the process usage monitor before cloning any typed API ports.
    #[must_use]
    pub fn with_request_observer(
        mut self,
        observer: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        Arc::make_mut(&mut self.client.config).request_observer = Some(Arc::new(observer));
        self
    }

    #[must_use]
    pub const fn shared_client(&self) -> &OpenDotaHttpClient {
        &self.client
    }

    /// Resolve a bundled hero name without issuing an HTTP request.
    #[must_use]
    pub fn hero_name(&self, hero_id: i64) -> Option<&str> {
        self.client
            .config
            .hero_names
            .get(&hero_id)
            .map(String::as_str)
    }

    #[must_use]
    pub fn player_api(&self) -> OpenDotaHttpClient {
        self.client.clone()
    }

    #[must_use]
    pub fn discovery_api(&self) -> OpenDotaHttpClient {
        self.client.clone()
    }

    #[must_use]
    pub fn enrichment_api(&self) -> OpenDotaHttpClient {
        self.client.clone()
    }

    #[must_use]
    pub fn registration_api(&self) -> OpenDotaHttpClient {
        self.client.clone()
    }

    #[must_use]
    pub fn dotabuff_id_extractor(&self) -> OpenDotaHttpClient {
        self.client.clone()
    }

    /// Compose the live profile service with the shared HTTP client and the
    /// existing SQLite Steam-ID repository.
    #[must_use]
    pub fn player_service(
        &self,
        player_repository: cama_db::opendota_player::OpenDotaPlayerRepository,
        heroes: RecordedHeroCatalog,
    ) -> OpenDotaPlayerService<
        cama_db::opendota_player::OpenDotaPlayerRepository,
        OpenDotaHttpClient,
        SystemOpenDotaPlayerClock,
        RecordedHeroCatalog,
    > {
        OpenDotaPlayerService::new(
            player_repository,
            self.player_api(),
            SystemOpenDotaPlayerClock,
            heroes,
        )
    }

    /// Build the live profile service with the same bundled hero names used by
    /// the HTTP projections. Dotabase role/attribute weights are not present in
    /// this repository, so those optional charts remain empty while names,
    /// lane distribution, win rate, KDA, and top-hero results stay complete.
    #[must_use]
    pub fn profile_player_service(
        &self,
        database_path: impl AsRef<std::path::Path>,
    ) -> OpenDotaPlayerService<
        cama_db::opendota_player::OpenDotaPlayerRepository,
        OpenDotaHttpClient,
        SystemOpenDotaPlayerClock,
        RecordedHeroCatalog,
    > {
        let heroes = self
            .client
            .config
            .hero_names
            .iter()
            .map(|(id, name)| {
                (
                    *id,
                    HeroMetadata {
                        name: name.clone(),
                        ..HeroMetadata::default()
                    },
                )
            })
            .collect();
        self.player_service(
            cama_db::opendota_player::OpenDotaPlayerRepository::new(database_path),
            RecordedHeroCatalog::new(heroes),
        )
    }
}

impl OpenDotaHttpClient {
    /// Build a production client for the official OpenDota API.
    pub fn new(api_key: Option<String>) -> Result<Self, OpenDotaHttpBuildError> {
        Self::with_config(OpenDotaHttpConfig::production(api_key))
    }

    /// Build a production client from `OPENDOTA_API_KEY` and the Python-
    /// compatible comma-separated `ENRICHMENT_RETRY_DELAYS` override.
    pub fn from_env() -> Result<Self, OpenDotaHttpBuildError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    pub fn from_lookup(
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, OpenDotaHttpBuildError> {
        let api_key = lookup("OPENDOTA_API_KEY").filter(|value| !value.is_empty());
        let mut config = OpenDotaHttpConfig::production(api_key);
        if let Some(raw) = lookup("ENRICHMENT_RETRY_DELAYS")
            && let Some(delays) = parse_retry_delays(&raw)
        {
            config.retry_delays = delays;
        }
        Self::with_config(config)
    }

    /// Build a client for an explicit endpoint. Production composition uses
    /// the default endpoint; tests use this constructor with loopback only.
    pub fn with_config(mut config: OpenDotaHttpConfig) -> Result<Self, OpenDotaHttpBuildError> {
        config.base_url = config.base_url.trim_end_matches('/').to_owned();
        config.api_key = config.api_key.filter(|value| !value.is_empty());
        if config.retry_delays.is_empty() {
            // Python preserves one retry when the configured list is empty.
            config.retry_delays.push(Duration::ZERO);
        }
        let http = Client::builder()
            .connect_timeout(config.request_timeout)
            .user_agent("cama-mm-rust/0.1")
            .build()?;
        let executor = dedicated_executor()?;
        let limiter = SHARED_RATE_LIMITER
            .get_or_init(|| Arc::new(TokenBucket::new(config.requests_per_minute)))
            .clone();
        Ok(Self {
            http,
            config: Arc::new(config),
            limiter,
            executor,
            sleeper: Arc::new(TokioRetrySleeper),
        })
    }

    #[cfg(test)]
    fn with_test_sleeper(mut self, sleeper: Arc<dyn RetrySleeper>) -> Self {
        self.sleeper = sleeper;
        self
    }

    /// Fetch the raw `/players/{id}` payload, returning `None` on any remote
    /// or JSON-safety failure.
    pub async fn get_player_data(&self, steam_id: i64) -> Option<Value> {
        self.get_json(&format!("/players/{steam_id}"), &[]).await
    }

    /// Fetch the raw hero history used by role calculations.
    pub async fn get_player_roles(&self, steam_id: i64) -> Option<Value> {
        self.get_json(&format!("/players/{steam_id}/heroes"), &[])
            .await
    }

    /// Fetch a bounded recent-match list. The `limit` query parameter is
    /// always present, matching the hardened Python endpoint.
    pub async fn get_player_matches(&self, steam_id: i64, limit: usize) -> Option<Vec<Value>> {
        let params = vec![("limit".to_owned(), limit.to_string())];
        self.get_json(&format!("/players/{steam_id}/matches"), &params)
            .await
            .and_then(|value| value.as_array().cloned())
    }

    /// Fetch aggregate player counts (including the region breakdown).
    pub async fn get_player_counts(&self, steam_id: i64) -> Option<Value> {
        self.get_json(&format!("/players/{steam_id}/counts"), &[])
            .await
    }

    /// Fetch raw match details. Empty/error objects are treated as missing,
    /// just as the Python integration does after JSON decoding.
    pub async fn get_match_details(&self, match_id: i64) -> Option<Value> {
        let value = self.get_json(&format!("/matches/{match_id}"), &[]).await?;
        let object = value.as_object()?;
        if object.is_empty() || object.contains_key("error") {
            None
        } else {
            Some(value)
        }
    }

    async fn get_projected_matches(&self, steam_id: i64, limit: usize) -> Option<Vec<Value>> {
        let mut params = vec![("limit".to_owned(), limit.to_string())];
        for field in [
            "hero_id",
            "lane_role",
            "player_slot",
            "radiant_win",
            "kills",
            "deaths",
            "assists",
            "duration",
            "start_time",
            "match_id",
        ] {
            params.push(("project".to_owned(), field.to_owned()));
        }
        self.get_json(&format!("/players/{steam_id}/matches"), &params)
            .await
            .and_then(|value| value.as_array().cloned())
    }

    async fn get_json(&self, path: &str, params: &[(String, String)]) -> Option<Value> {
        let response = self.make_request(path, params).await?;
        if !response.status().is_success() {
            return None;
        }
        let body = read_bounded(response, self.config.max_response_bytes).await?;
        serde_json::from_slice(&body).ok()
    }

    async fn make_request(&self, path: &str, params: &[(String, String)]) -> Option<Response> {
        let started_at = Instant::now();
        let rate_limit_timeout = self
            .config
            .rate_limit_wait
            .min(self.config.request_deadline);
        if rate_limit_timeout.is_zero() || !self.limiter.acquire(rate_limit_timeout).await {
            return None;
        }

        let mut query = params.to_vec();
        if let Some(api_key) = &self.config.api_key {
            query.push(("api_key".to_owned(), api_key.clone()));
        }
        let url = format!("{}{path}", self.config.base_url);
        let delays = &self.config.retry_delays;
        for attempt in 0..=delays.len() {
            let remaining = self
                .config
                .request_deadline
                .saturating_sub(started_at.elapsed());
            if remaining.is_zero() {
                return None;
            }
            if let Some(observer) = &self.config.request_observer {
                observer("opendota");
            }
            let response = self
                .http
                .get(&url)
                .query(&query)
                .timeout(self.config.request_timeout.min(remaining))
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                Err(_) if attempt < delays.len() => {
                    let delay = delays[attempt].min(
                        self.config
                            .request_deadline
                            .saturating_sub(started_at.elapsed()),
                    );
                    self.sleeper.sleep(delay).await;
                    continue;
                }
                Err(_) => return None,
            };

            if !RETRYABLE_STATUS_CODES.contains(&response.status()) {
                return Some(response);
            }
            if attempt >= delays.len() {
                return Some(response);
            }

            let mut delay = delays[attempt];
            if response.status() == StatusCode::TOO_MANY_REQUESTS
                && let Some(server_delay) = retry_after_duration(&response)
            {
                delay = delay.max(server_delay);
            }
            delay = delay.min(
                self.config
                    .request_deadline
                    .saturating_sub(started_at.elapsed()),
            );
            self.sleeper.sleep(delay).await;
        }
        None
    }

    fn run_on_dedicated<T, F>(&self, future: F) -> Result<T, OpenDotaHttpExecutorError>
    where
        T: Send + 'static,
        F: Future<Output = T> + Send + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.executor.spawn(async move {
            let output = future.await;
            let _ = sender.send(output);
        });
        receiver.recv().map_err(|_| OpenDotaHttpExecutorError)
    }

    fn sync_player_data(&self, steam_id: i64) -> Result<Option<Value>, OpenDotaHttpExecutorError> {
        let client = self.clone();
        self.run_on_dedicated(async move { client.get_player_data(steam_id).await })
    }

    fn sync_projected_matches(
        &self,
        steam_id: i64,
        limit: usize,
    ) -> Result<Option<Vec<Value>>, OpenDotaHttpExecutorError> {
        let client = self.clone();
        self.run_on_dedicated(async move { client.get_projected_matches(steam_id, limit).await })
    }

    fn sync_json_path(&self, path: String) -> Result<Option<Value>, OpenDotaHttpExecutorError> {
        let client = self.clone();
        self.run_on_dedicated(async move { client.get_json(&path, &[]).await })
    }
}

fn dedicated_executor() -> Result<Arc<Runtime>, std::io::Error> {
    if let Some(executor) = OPENDOTA_EXECUTOR.get() {
        return Ok(executor.clone());
    }
    let _initializing = OPENDOTA_EXECUTOR_INIT
        .lock()
        .map_err(|_| std::io::Error::other("OpenDota executor initializer was poisoned"))?;
    if let Some(executor) = OPENDOTA_EXECUTOR.get() {
        return Ok(executor.clone());
    }
    let executor = Arc::new(
        Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("opendota-io")
            .enable_all()
            .build()?,
    );
    let _ = OPENDOTA_EXECUTOR.set(executor.clone());
    Ok(OPENDOTA_EXECUTOR.get().cloned().unwrap_or(executor))
}

async fn read_bounded(mut response: Response, max_bytes: usize) -> Option<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return None;
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body)
}

fn retry_after_duration(response: &Response) -> Option<Duration> {
    let raw = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?;
    if let Ok(seconds) = raw.parse::<i64>() {
        return Some(Duration::from_secs(seconds.max(0) as u64));
    }
    let parsed = chrono::DateTime::parse_from_rfc2822(raw).ok()?;
    let seconds = parsed.timestamp() - Utc::now().timestamp();
    Some(Duration::from_secs(seconds.max(0) as u64))
}

fn parse_retry_delays(raw: &str) -> Option<Vec<Duration>> {
    raw.split(',')
        .filter(|part| !part.trim().is_empty())
        .map(|part| part.trim().parse::<u64>().ok().map(Duration::from_secs))
        .collect()
}

fn value_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_f64().and_then(truncate_finite_f64_to_i64))
        .or_else(|| value.as_str()?.trim().parse().ok())
}

fn field_i64(object: &Map<String, Value>, name: &str) -> Option<i64> {
    object.get(name).and_then(value_i64)
}

fn value_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
}

fn field_f64(object: &Map<String, Value>, name: &str) -> Option<f64> {
    object.get(name).and_then(value_f64)
}

fn field_bool(object: &Map<String, Value>, name: &str) -> Option<bool> {
    object.get(name).and_then(|value| {
        value
            .as_bool()
            .or_else(|| value_i64(value).map(|value| value != 0))
    })
}

fn round_one(value: f64) -> f64 {
    format!("{value:.1}").parse().unwrap_or(value)
}

fn project_player_identity(value: Value) -> Option<PlayerIdentity> {
    let object = value.as_object()?;
    if object.is_empty() {
        return None;
    }
    let profile = object.get("profile").and_then(Value::as_object);
    Some(PlayerIdentity {
        persona_name: profile
            .and_then(|profile| profile.get("personaname"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        avatar: profile
            .and_then(|profile| profile.get("avatar"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        rank_tier: field_i64(object, "rank_tier"),
        mmr_estimate: object
            .get("mmr_estimate")
            .and_then(Value::as_object)
            .and_then(|estimate| field_i64(estimate, "estimate")),
    })
}

fn project_win_loss(value: Option<Value>) -> WinLoss {
    let object = value.as_ref().and_then(Value::as_object);
    WinLoss {
        wins: object
            .and_then(|value| field_i64(value, "win"))
            .unwrap_or(0),
        losses: object
            .and_then(|value| field_i64(value, "lose"))
            .unwrap_or(0),
    }
}

fn project_profile_averages(value: Option<Value>) -> ProfileAverages {
    let mut result = ProfileAverages::default();
    let Some(items) = value.as_ref().and_then(Value::as_array) else {
        return result;
    };
    for item in items {
        let Some(item) = item.as_object() else {
            continue;
        };
        let Some(field) = item.get("field").and_then(Value::as_str) else {
            continue;
        };
        let n = field_f64(item, "n").unwrap_or(0.0);
        if n <= 0.0 {
            continue;
        }
        let average = field_f64(item, "sum").unwrap_or(0.0) / n;
        match field {
            "kills" => result.kills = round_one(average),
            "deaths" => result.deaths = round_one(average),
            "assists" => result.assists = round_one(average),
            "gold_per_min" => result.gpm = average as i64,
            "xp_per_min" => result.xpm = average as i64,
            "last_hits" => result.last_hits = average as i64,
            _ => {}
        }
    }
    result
}

fn project_top_heroes(value: Option<Value>, hero_names: &BTreeMap<i64, String>) -> Vec<TopHero> {
    let Some(items) = value.as_ref().and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut heroes = items
        .iter()
        .filter_map(|item| item.as_object())
        .filter_map(|item| {
            let hero_id = field_i64(item, "hero_id")?;
            let games = field_i64(item, "games").unwrap_or(0);
            let wins = field_i64(item, "win").unwrap_or(0);
            Some((hero_id, games, wins))
        })
        .collect::<Vec<_>>();
    heroes.sort_by_key(|(_, games, _)| std::cmp::Reverse(*games));
    heroes
        .into_iter()
        .take(5)
        .filter(|(_, games, _)| *games > 0)
        .map(|(hero_id, games, wins)| TopHero {
            hero_id,
            hero_name: hero_names
                .get(&hero_id)
                .cloned()
                .unwrap_or_else(|| format!("Hero {hero_id}")),
            games,
            wins,
            win_rate: round_one((wins as f64 / games as f64) * 100.0),
        })
        .collect()
}

fn project_matches(values: Option<Vec<Value>>) -> Vec<ProjectedMatch> {
    values
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| {
            let object = value.as_object()?;
            Some(ProjectedMatch {
                match_id: field_i64(object, "match_id").map(ValveMatchId),
                hero_id: field_i64(object, "hero_id"),
                lane_role: field_i64(object, "lane_role"),
                player_slot: field_i64(object, "player_slot")
                    .and_then(|value| u16::try_from(value).ok()),
                radiant_win: field_bool(object, "radiant_win").unwrap_or(false),
                kills: field_i64(object, "kills").unwrap_or(0),
                deaths: field_i64(object, "deaths").unwrap_or(0),
                assists: field_i64(object, "assists").unwrap_or(0),
                duration_seconds: field_i64(object, "duration").unwrap_or(0),
                start_time: field_i64(object, "start_time").unwrap_or(0),
            })
        })
        .collect()
}

fn project_history_matches(values: Vec<Value>) -> Vec<PlayerHistoryMatch> {
    values
        .into_iter()
        .filter_map(|value| {
            let object = value.as_object()?;
            Some(PlayerHistoryMatch {
                match_id: ValveMatchId(field_i64(object, "match_id")?),
                start_time: field_i64(object, "start_time").unwrap_or(0),
            })
        })
        .collect()
}

fn project_match_details(value: Value, requested_match_id: i64) -> Option<OpenDotaMatchDetails> {
    let object = value.as_object()?;
    let raw_payload = serde_json::to_string(&value).ok();
    let players = object
        .get("players")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |players| {
            players.iter().filter_map(project_match_player).collect()
        });
    Some(OpenDotaMatchDetails {
        match_id: ValveMatchId(field_i64(object, "match_id").unwrap_or(requested_match_id)),
        duration_seconds: field_i64(object, "duration").unwrap_or(0),
        radiant_win: field_bool(object, "radiant_win").unwrap_or(false),
        radiant_score: field_i64(object, "radiant_score").unwrap_or(0),
        dire_score: field_i64(object, "dire_score").unwrap_or(0),
        game_mode: field_i64(object, "game_mode").unwrap_or(0),
        comeback: field_i64(object, "comeback"),
        throw_amount: field_i64(object, "throw"),
        raw_payload,
        players,
    })
}

fn project_match_player(value: &Value) -> Option<OpenDotaPlayer> {
    let object = value.as_object()?;
    let purchase_keys = object
        .get("purchase_log")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("key").and_then(Value::as_str))
                .map(ToOwned::to_owned)
                .collect()
        });
    Some(OpenDotaPlayer {
        account_id: field_i64(object, "account_id").map(SteamId),
        player_slot: field_i64(object, "player_slot").and_then(|value| u16::try_from(value).ok()),
        stats: EnrichedParticipantStats {
            hero_id: field_i64(object, "hero_id").unwrap_or(0),
            kills: field_i64(object, "kills").unwrap_or(0),
            deaths: field_i64(object, "deaths").unwrap_or(0),
            assists: field_i64(object, "assists").unwrap_or(0),
            gpm: field_i64(object, "gold_per_min").unwrap_or(0),
            xpm: field_i64(object, "xp_per_min").unwrap_or(0),
            hero_damage: field_i64(object, "hero_damage").unwrap_or(0),
            tower_damage: field_i64(object, "tower_damage").unwrap_or(0),
            last_hits: field_i64(object, "last_hits").unwrap_or(0),
            denies: field_i64(object, "denies").unwrap_or(0),
            net_worth: field_i64(object, "net_worth")
                .or_else(|| field_i64(object, "total_gold"))
                .unwrap_or(0),
            hero_healing: field_i64(object, "hero_healing").unwrap_or(0),
            lane_role: field_i64(object, "lane_role"),
            lane_efficiency: field_i64(object, "lane_efficiency_pct"),
            gold_at_10: object
                .get("gold_t")
                .and_then(Value::as_array)
                .and_then(|series| series.get(FARM_PRIORITY_MINUTE))
                .and_then(Value::as_i64),
            towers_killed: field_i64(object, "towers_killed"),
            roshans_killed: field_i64(object, "roshans_killed"),
            teamfight_participation: field_f64(object, "teamfight_participation"),
            obs_placed: field_i64(object, "obs_placed"),
            sen_placed: field_i64(object, "sen_placed"),
            camps_stacked: field_i64(object, "camps_stacked"),
            rune_pickups: field_i64(object, "rune_pickups"),
            firstblood_claimed: Some(i64::from(
                field_bool(object, "firstblood_claimed").unwrap_or(false),
            )),
            stuns: field_f64(object, "stuns"),
        },
        fantasy: FantasyStats {
            kills: field_f64(object, "kills").unwrap_or(0.0),
            deaths: object.get("deaths").and_then(value_f64),
            assists: field_f64(object, "assists").unwrap_or(0.0),
            last_hits: field_f64(object, "last_hits").unwrap_or(0.0),
            denies: field_f64(object, "denies").unwrap_or(0.0),
            gold_per_min: field_f64(object, "gold_per_min").unwrap_or(0.0),
            xp_per_min: field_f64(object, "xp_per_min").unwrap_or(0.0),
            towers_killed: field_f64(object, "towers_killed").unwrap_or(0.0),
            roshans_killed: field_f64(object, "roshans_killed").unwrap_or(0.0),
            teamfight_participation: field_f64(object, "teamfight_participation").unwrap_or(0.0),
            obs_placed: field_f64(object, "obs_placed").unwrap_or(0.0),
            sen_placed: field_f64(object, "sen_placed").unwrap_or(0.0),
            camps_stacked: field_f64(object, "camps_stacked").unwrap_or(0.0),
            rune_pickups: field_f64(object, "rune_pickups").unwrap_or(0.0),
            firstblood_claimed: field_bool(object, "firstblood_claimed").unwrap_or(false),
            stuns: field_f64(object, "stuns").unwrap_or(0.0),
            hero_healing: field_f64(object, "hero_healing").unwrap_or(0.0),
        },
        wrapped: WrappedPlayerTelemetry {
            actions_per_min: field_i64(object, "actions_per_min"),
            courier_kills: field_i64(object, "courier_kills"),
            pings: field_i64(object, "pings"),
            lane_role: field_i64(object, "lane_role"),
            purchase_keys,
        },
    })
}

fn project_registration_player(value: Value) -> Option<OpenDotaPlayerData> {
    let object = value.as_object()?;
    if object.is_empty() {
        return Some(OpenDotaPlayerData::default());
    }
    Some(OpenDotaPlayerData {
        has_payload_content: true,
        mmr_estimate: object
            .get("mmr_estimate")
            .and_then(Value::as_object)
            .and_then(|estimate| estimate.get("estimate"))
            .and_then(project_mmr_value),
        computed_mmr: object.get("computed_mmr").and_then(project_mmr_value),
        solo_competitive_rank: object
            .get("solo_competitive_rank")
            .and_then(project_mmr_value),
        rank_tier: field_i64(object, "rank_tier"),
        leaderboard_rank: field_i64(object, "leaderboard_rank"),
    })
}

fn project_mmr_value(value: &Value) -> Option<OpenDotaMmrValue> {
    match value {
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                return Some(OpenDotaMmrValue::Integer(value));
            }

            // Python's `int(float)` truncates toward zero. OpenDota's
            // `computed_mmr` is sometimes a JSON float even though the
            // registration domain stores whole MMR. Preserve that behavior,
            // but do not rely on Rust's saturating float-to-int cast for
            // non-finite or out-of-range values.
            number
                .as_f64()
                .and_then(truncate_finite_f64_to_i64)
                .map(OpenDotaMmrValue::Integer)
                // Keep an unsafe numeric scalar truthy and unparseable so the
                // fallback policy reports it instead of silently skipping to a
                // lower-priority field.
                .or_else(|| Some(OpenDotaMmrValue::Text(number.to_string())))
        }
        Value::String(value) => Some(OpenDotaMmrValue::Text(value.to_owned())),
        _ => None,
    }
}

fn truncate_finite_f64_to_i64(value: f64) -> Option<i64> {
    // `i64::MAX as f64` rounds up to 2^63, so the upper bound must be
    // exclusive. i64::MIN is exactly representable as f64.
    const I64_EXCLUSIVE_UPPER_BOUND: f64 = 9_223_372_036_854_775_808.0;

    (value.is_finite() && value >= i64::MIN as f64 && value < I64_EXCLUSIVE_UPPER_BOUND)
        .then(|| value.trunc() as i64)
}

impl OpenDotaPlayerApiPort for OpenDotaHttpClient {
    fn player_identity(
        &self,
        steam_id: SteamId,
    ) -> Result<Option<PlayerIdentity>, OpenDotaPlayerPortError> {
        self.sync_player_data(steam_id.0)
            .map(|value| value.and_then(project_player_identity))
            .map_err(|error| OpenDotaPlayerPortError::new(error.to_string()))
    }

    fn win_loss(&self, steam_id: SteamId) -> Result<WinLoss, OpenDotaPlayerPortError> {
        self.sync_json_path(format!("/players/{}/wl", steam_id.0))
            .map(project_win_loss)
            .map_err(|error| OpenDotaPlayerPortError::new(error.to_string()))
    }

    fn profile_averages(
        &self,
        steam_id: SteamId,
    ) -> Result<ProfileAverages, OpenDotaPlayerPortError> {
        self.sync_json_path(format!("/players/{}/totals", steam_id.0))
            .map(project_profile_averages)
            .map_err(|error| OpenDotaPlayerPortError::new(error.to_string()))
    }

    fn top_heroes(&self, steam_id: SteamId) -> Result<Vec<TopHero>, OpenDotaPlayerPortError> {
        self.sync_json_path(format!("/players/{}/heroes", steam_id.0))
            .map(|value| project_top_heroes(value, &self.config.hero_names))
            .map_err(|error| OpenDotaPlayerPortError::new(error.to_string()))
    }

    fn projected_matches(
        &self,
        steam_id: SteamId,
        limit: usize,
    ) -> Result<Vec<ProjectedMatch>, OpenDotaPlayerPortError> {
        self.sync_projected_matches(steam_id.0, limit)
            .map(project_matches)
            .map_err(|error| OpenDotaPlayerPortError::new(error.to_string()))
    }
}

impl OpenDotaDiscoveryPort for OpenDotaHttpClient {
    fn player_matches(
        &self,
        steam_id: SteamId,
        limit: usize,
    ) -> Result<Option<Vec<PlayerHistoryMatch>>, DiscoveryPortError> {
        let client = self.clone();
        self.run_on_dedicated(async move { client.get_player_matches(steam_id.0, limit).await })
            .map(|value| value.map(project_history_matches))
            .map_err(|error| DiscoveryPortError::new(error.to_string()))
    }

    fn match_details(
        &self,
        match_id: ValveMatchId,
    ) -> Result<Option<OpenDotaMatchDetails>, DiscoveryPortError> {
        let client = self.clone();
        self.run_on_dedicated(async move { client.get_match_details(match_id.0).await })
            .map(|value| value.and_then(|value| project_match_details(value, match_id.0)))
            .map_err(|error| DiscoveryPortError::new(error.to_string()))
    }
}

impl DotabuffIdExtractionPort for OpenDotaHttpClient {
    fn extract_player_id_from_dotabuff(&self, url: &str) -> Option<SteamId> {
        const STEAM_ID64_OFFSET: i64 = 76_561_197_960_265_728;
        let suffix = url.split_once("/players/")?.1;
        let digits = suffix
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>();
        let dotabuff_id = digits.parse::<i64>().ok()?;
        dotabuff_id
            .checked_sub(STEAM_ID64_OFFSET)
            .filter(|steam_id| *steam_id > 0)
            .map(SteamId)
    }
}

impl OpenDotaRegistrationPort for OpenDotaHttpClient {
    type Error = OpenDotaPlayerPortError;

    fn get_player_data(
        &mut self,
        steam_id: i64,
    ) -> Result<Option<OpenDotaPlayerData>, Self::Error> {
        self.sync_player_data(steam_id)
            .map(|value| value.and_then(project_registration_player))
            .map_err(|error| OpenDotaPlayerPortError::new(error.to_string()))
    }

    fn get_player_mmr_from_data(
        &mut self,
        player_data: &OpenDotaPlayerData,
    ) -> Result<Option<i64>, Self::Error> {
        get_player_mmr_from_data(Some(player_data))
            .map_err(|error| OpenDotaPlayerPortError::new(error.to_string()))
    }
}

#[cfg(test)]
mod tests;
