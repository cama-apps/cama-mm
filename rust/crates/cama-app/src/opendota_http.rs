//! Production OpenDota HTTP transport and typed application adapters.
//!
//! The transport is deliberately fail-soft at the remote boundary: HTTP
//! failures, oversized bodies, and malformed JSON become `None` (or an empty
//! profile component), matching the Python integration. Network work runs on
//! a dedicated four-thread Tokio runtime so slow OpenDota calls cannot consume
//! the runtime used for SQLite or Discord work.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, PoisonError, mpsc};
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
pub const PUBLISHED_ANONYMOUS_REQUESTS_PER_MINUTE: u32 = 60;
pub const PUBLISHED_AUTHENTICATED_REQUESTS_PER_MINUTE: u32 = 300;
/// Operational caps deliberately retain headroom below OpenDota's current
/// published ceilings. Capacity-one buckets also prevent startup bursts.
pub const ANONYMOUS_REQUESTS_PER_MINUTE: u32 = 50;
pub const AUTHENTICATED_REQUESTS_PER_MINUTE: u32 = 250;
pub const REQUESTS_PER_DAY: u32 = 3_000;
pub const DEFAULT_DAILY_QUOTA_STATE_PATH: &str = ".cache/opendota/daily-quota-v1";

const DEFAULT_RETRY_DELAYS_SECONDS: &[u64] = &[1, 5, 20, 60, 180];
const RETRYABLE_STATUS_CODES: &[StatusCode] = &[
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
    /// Optional daily safety cap. OpenDota's anonymous hosted quota is
    /// 3000/day; premium keyed calls are not given an artificial daily cap
    /// unless a caller deliberately sets one.
    pub requests_per_day: Option<u32>,
    /// Optional durable ledger for the daily cap. Production enables this so
    /// a container restart cannot forget requests already sent that UTC day.
    pub daily_quota_state_path: Option<PathBuf>,
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
            .field("requests_per_day", &self.requests_per_day)
            .field("daily_quota_state_path", &self.daily_quota_state_path)
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
        let authenticated = api_key.is_some();
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
            requests_per_day: if authenticated {
                None
            } else {
                Some(REQUESTS_PER_DAY)
            },
            daily_quota_state_path: None,
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
        if config.requests_per_day.is_some() {
            config.daily_quota_state_path = Some(PathBuf::from(DEFAULT_DAILY_QUOTA_STATE_PATH));
        }
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
    #[error("failed to initialize OpenDota quota state at {path}: {source}")]
    QuotaState {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Quota information useful for health/admin surfaces. Upstream remaining
/// values are populated only after the server sends the corresponding headers.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OpenDotaQuotaSnapshot {
    pub local_daily_limit: Option<u32>,
    pub local_daily_used: u32,
    pub upstream_remaining_minute: Option<u32>,
    pub upstream_remaining_day: Option<u32>,
    pub rate_limited_responses: u64,
    pub local_rejections: u64,
    pub cooldown_remaining_seconds: Option<u64>,
    pub persistence_healthy: bool,
}

impl OpenDotaQuotaSnapshot {
    #[must_use]
    pub fn request_is_blocked(&self) -> bool {
        self.local_daily_limit
            .is_some_and(|limit| self.local_daily_used >= limit)
            || self.upstream_remaining_minute == Some(0)
            || (self.local_daily_limit.is_some() && self.upstream_remaining_day == Some(0))
            || !self.persistence_healthy
    }
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
    upstream_remaining: Option<u32>,
    upstream_reset_at: Option<Instant>,
}

#[derive(Debug)]
struct TokenBucket {
    capacity: f64,
    tokens_per_second: f64,
    state: Mutex<TokenBucketState>,
}

/// One token bucket per (endpoint, requests-per-minute) identity. The bucket
/// deliberately has capacity one: a normal token bucket with capacity equal to
/// the minute quota would allow a full startup burst that is unsafe for a
/// rolling-window upstream quota.
static SHARED_RATE_LIMITERS: StdMutex<BTreeMap<(String, u32), Arc<TokenBucket>>> =
    StdMutex::new(BTreeMap::new());
type DailyQuotaKey = (String, Option<u32>, Option<PathBuf>);
type DailyQuotaMap = BTreeMap<DailyQuotaKey, Arc<DailyQuota>>;
static SHARED_DAILY_QUOTAS: StdMutex<DailyQuotaMap> = StdMutex::new(BTreeMap::new());
static OPENDOTA_EXECUTOR: OnceLock<Arc<Runtime>> = OnceLock::new();
static OPENDOTA_EXECUTOR_INIT: StdMutex<()> = StdMutex::new(());
static BUNDLED_HERO_NAMES: OnceLock<BTreeMap<i64, String>> = OnceLock::new();

const MINUTE_WINDOW: Duration = Duration::from_secs(60);
const DAY_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
const UTC_DAY_SECONDS: i64 = 24 * 60 * 60;

fn bundled_hero_names() -> BTreeMap<i64, String> {
    BUNDLED_HERO_NAMES
        .get_or_init(|| {
            serde_json::from_str::<BTreeMap<String, String>>(include_str!("../data/heroes.json"))
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
            // Capacity one enforces spacing between every wire attempt. This
            // is intentionally stricter than a burst-capable token bucket.
            capacity: 1.0,
            tokens_per_second: f64::from(requests_per_minute) / 60.0,
            state: Mutex::new(TokenBucketState {
                tokens: 1.0,
                last_update: Instant::now(),
                upstream_remaining: None,
                upstream_reset_at: None,
            }),
        }
    }

    async fn acquire(&self, timeout: Duration) -> bool {
        let started_at = Instant::now();
        loop {
            let wait = {
                let mut state = self.state.lock().await;
                let now = Instant::now();
                if state.upstream_reset_at.is_some_and(|reset| reset <= now) {
                    state.upstream_remaining = None;
                    state.upstream_reset_at = None;
                }
                let elapsed = now.duration_since(state.last_update).as_secs_f64();
                state.tokens = (state.tokens + elapsed * self.tokens_per_second).min(self.capacity);
                state.last_update = now;
                if state.tokens >= 1.0 && state.upstream_remaining != Some(0) {
                    state.tokens -= 1.0;
                    if let Some(remaining) = &mut state.upstream_remaining {
                        *remaining = remaining.saturating_sub(1);
                    }
                    return true;
                }

                let local_wait = if state.tokens >= 1.0 {
                    Duration::ZERO
                } else {
                    Duration::from_secs_f64((1.0 - state.tokens) / self.tokens_per_second)
                };
                let upstream_wait = if state.upstream_remaining == Some(0) {
                    state
                        .upstream_reset_at
                        .map_or(MINUTE_WINDOW, |reset| reset.saturating_duration_since(now))
                } else {
                    Duration::ZERO
                };
                local_wait.max(upstream_wait)
            };

            let elapsed = started_at.elapsed();
            if elapsed >= timeout {
                return false;
            }
            let remaining = timeout - elapsed;
            tokio::time::sleep(wait.min(remaining).max(Duration::from_millis(1))).await;
        }
    }

    async fn refund(&self) {
        let mut state = self.state.lock().await;
        state.tokens = (state.tokens + 1.0).min(self.capacity);
        // Keep any upstream reservation conservative when another task may
        // have raced this rollback.
    }

    async fn observe_remaining(&self, remaining: u32, reset_after: Duration) {
        let mut state = self.state.lock().await;
        let now = Instant::now();
        if state.upstream_reset_at.is_some_and(|reset| reset <= now) {
            state.upstream_remaining = None;
            state.upstream_reset_at = None;
        }
        state.upstream_remaining = Some(
            state
                .upstream_remaining
                .map_or(remaining, |current| current.min(remaining)),
        );
        let reset_at = now + reset_after;
        state.upstream_reset_at = Some(
            state
                .upstream_reset_at
                // Never shorten a live upstream window. In particular, a
                // later 429 with a longer Retry-After must extend the shared
                // circuit instead of reopening at an older, earlier reset.
                .map_or(reset_at, |current| current.max(reset_at)),
        );
    }

    async fn upstream_remaining(&self) -> Option<u32> {
        let mut state = self.state.lock().await;
        let now = Instant::now();
        if state.upstream_reset_at.is_some_and(|reset| reset <= now) {
            state.upstream_remaining = None;
            state.upstream_reset_at = None;
        }
        state.upstream_remaining
    }

    async fn cooldown_remaining(&self) -> Option<Duration> {
        let mut state = self.state.lock().await;
        let now = Instant::now();
        if state.upstream_reset_at.is_some_and(|reset| reset <= now) {
            state.upstream_remaining = None;
            state.upstream_reset_at = None;
        }
        if state.upstream_remaining != Some(0) {
            return None;
        }
        state
            .upstream_reset_at
            .map(|reset| reset.saturating_duration_since(now))
    }
}

fn shared_rate_limiter(base_url: &str, requests_per_minute: u32) -> Arc<TokenBucket> {
    // Match TokenBucket::new's clamp so the key and the bucket rate agree.
    let requests_per_minute = requests_per_minute.max(1);
    let mut limiters = SHARED_RATE_LIMITERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    limiters
        .entry((base_url.to_owned(), requests_per_minute))
        .or_insert_with(|| Arc::new(TokenBucket::new(requests_per_minute)))
        .clone()
}

#[derive(Debug)]
struct DailyQuotaState {
    day: i64,
    used: u32,
    upstream_remaining: Option<u32>,
    upstream_reset_at: Option<Instant>,
    rate_limited_responses: u64,
    local_rejections: u64,
    persistence_healthy: bool,
}

#[derive(Debug)]
struct DailyQuota {
    limit: Option<u32>,
    state_path: Option<PathBuf>,
    state: Mutex<DailyQuotaState>,
}

impl DailyQuota {
    fn new(limit: Option<u32>, state_path: Option<PathBuf>) -> std::io::Result<Self> {
        let day = utc_day_index();
        let used = match (limit, state_path.as_deref()) {
            (Some(_), Some(path)) => load_daily_quota_state(path, day)?,
            _ => 0,
        };
        if let (Some(_), Some(path)) = (limit, state_path.as_deref()) {
            persist_daily_quota_state(path, day, used)?;
        }
        Ok(Self {
            limit,
            state_path,
            state: Mutex::new(DailyQuotaState {
                day,
                used,
                upstream_remaining: None,
                upstream_reset_at: None,
                rate_limited_responses: 0,
                local_rejections: 0,
                persistence_healthy: true,
            }),
        })
    }

    async fn acquire(&self, timeout: Duration) -> bool {
        let started_at = Instant::now();
        loop {
            let wait = {
                let mut state = self.state.lock().await;
                let now = Instant::now();
                let day = utc_day_index();
                if state.day != day {
                    state.day = day;
                    state.used = match (self.limit, self.state_path.as_deref()) {
                        (Some(_), Some(path)) => match load_daily_quota_state(path, day) {
                            Ok(used) => used,
                            Err(_) => {
                                state.persistence_healthy = false;
                                state.local_rejections = state.local_rejections.saturating_add(1);
                                return false;
                            }
                        },
                        _ => 0,
                    };
                    state.upstream_remaining = None;
                    state.upstream_reset_at = None;
                    state.persistence_healthy = true;
                }
                if state.upstream_reset_at.is_some_and(|reset| reset <= now) {
                    state.upstream_remaining = None;
                    state.upstream_reset_at = None;
                }
                if !state.persistence_healthy {
                    state.local_rejections = state.local_rejections.saturating_add(1);
                    return false;
                }
                let local_available = self.limit.is_none_or(|limit| state.used < limit);
                if local_available && state.upstream_remaining != Some(0) {
                    let next_used = state.used.saturating_add(1);
                    if let Some(path) = self.state_path.as_deref()
                        && self.limit.is_some()
                        && persist_daily_quota_state(path, state.day, next_used).is_err()
                    {
                        state.persistence_healthy = false;
                        state.local_rejections = state.local_rejections.saturating_add(1);
                        return false;
                    }
                    state.used = next_used;
                    if let Some(remaining) = &mut state.upstream_remaining {
                        *remaining = remaining.saturating_sub(1);
                    }
                    return true;
                }

                let local_wait = if local_available {
                    Duration::ZERO
                } else {
                    duration_until_next_utc_day()
                };
                let upstream_wait = if state.upstream_remaining == Some(0) {
                    state
                        .upstream_reset_at
                        .map_or(DAY_WINDOW, |reset| reset.saturating_duration_since(now))
                } else {
                    Duration::ZERO
                };
                local_wait.max(upstream_wait)
            };

            let elapsed = started_at.elapsed();
            if elapsed >= timeout {
                let mut state = self.state.lock().await;
                state.local_rejections = state.local_rejections.saturating_add(1);
                return false;
            }
            let remaining = timeout - elapsed;
            tokio::time::sleep(wait.min(remaining).max(Duration::from_millis(1))).await;
        }
    }

    async fn observe_remaining(&self, remaining: u32, reset_after: Duration) {
        if self.limit.is_none() {
            // Premium calls may expose the free-call balance even though paid
            // overage is allowed. Do not turn that billing counter into a hard
            // daily block for authenticated traffic.
            return;
        }
        let mut state = self.state.lock().await;
        let now = Instant::now();
        if state.upstream_reset_at.is_some_and(|reset| reset <= now) {
            state.upstream_remaining = None;
            state.upstream_reset_at = None;
        }
        state.upstream_remaining = Some(
            state
                .upstream_remaining
                .map_or(remaining, |current| current.min(remaining)),
        );
        let reset_at = now + reset_after;
        state.upstream_reset_at = Some(
            state
                .upstream_reset_at
                // As with the minute circuit, retain the later live reset so
                // a subsequent daily header can only make the guard stricter.
                .map_or(reset_at, |current| current.max(reset_at)),
        );
        if let Some(limit) = self.limit {
            state.used = state.used.max(limit.saturating_sub(remaining));
            if let Some(path) = self.state_path.as_deref()
                && persist_daily_quota_state(path, state.day, state.used).is_err()
            {
                state.persistence_healthy = false;
            }
        }
    }

    async fn record_rate_limited(&self) {
        let mut state = self.state.lock().await;
        state.rate_limited_responses = state.rate_limited_responses.saturating_add(1);
    }

    async fn record_local_rejection(&self) {
        let mut state = self.state.lock().await;
        state.local_rejections = state.local_rejections.saturating_add(1);
    }

    async fn snapshot(&self) -> DailyQuotaSnapshot {
        let mut state = self.state.lock().await;
        if state.day != utc_day_index() {
            state.day = utc_day_index();
            state.used = 0;
            state.upstream_remaining = None;
            state.upstream_reset_at = None;
            state.persistence_healthy = true;
        }
        let now = Instant::now();
        if state.upstream_reset_at.is_some_and(|reset| reset <= now) {
            state.upstream_remaining = None;
            state.upstream_reset_at = None;
        }
        DailyQuotaSnapshot {
            upstream_remaining: state.upstream_remaining,
            used: state.used,
            rate_limited_responses: state.rate_limited_responses,
            local_rejections: state.local_rejections,
            persistence_healthy: state.persistence_healthy,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct DailyQuotaSnapshot {
    upstream_remaining: Option<u32>,
    used: u32,
    rate_limited_responses: u64,
    local_rejections: u64,
    persistence_healthy: bool,
}

fn shared_daily_quota(
    base_url: &str,
    limit: Option<u32>,
    state_path: Option<PathBuf>,
) -> std::io::Result<Arc<DailyQuota>> {
    let mut quotas = SHARED_DAILY_QUOTAS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let key = (base_url.to_owned(), limit, state_path.clone());
    if let Some(quota) = quotas.get(&key) {
        return Ok(quota.clone());
    }
    let quota = Arc::new(DailyQuota::new(limit, state_path)?);
    Ok(quotas.entry(key).or_insert_with(|| quota.clone()).clone())
}

fn utc_day_index() -> i64 {
    Utc::now().timestamp().div_euclid(UTC_DAY_SECONDS)
}

fn duration_until_next_utc_day() -> Duration {
    let now = Utc::now().timestamp();
    let next = (now.div_euclid(UTC_DAY_SECONDS) + 1) * UTC_DAY_SECONDS;
    Duration::from_secs(next.saturating_sub(now).max(1) as u64)
}

fn load_daily_quota_state(path: &Path, current_day: i64) -> std::io::Result<u32> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut fields = raw.split_whitespace();
    let day = fields
        .next()
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing quota day"))?;
    let used = fields
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing quota use"))?;
    if fields.next().is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "unexpected trailing quota data",
        ));
    }
    Ok(if day == current_day { used } else { 0 })
}

fn persist_daily_quota_state(path: &Path, day: i64, used: u32) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    let mut file = fs::File::create(&temporary)?;
    writeln!(file, "{day} {used}")?;
    file.sync_all()?;
    fs::rename(temporary, path)
}

/// One cloneable production client implementing the profile, registration,
/// discovery, enrichment, role, and match-history ports.
#[derive(Clone)]
pub struct OpenDotaHttpClient {
    http: Client,
    config: Arc<OpenDotaHttpConfig>,
    limiter: Arc<TokenBucket>,
    daily_quota: Arc<DailyQuota>,
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
            .field("requests_per_day", &self.config.requests_per_day)
            .field("retry_delays", &self.config.retry_delays)
            .field("hero_name_count", &self.config.hero_names.len())
            .finish_non_exhaustive()
    }
}

/// Application-level production composition root for every OpenDota consumer.
///
/// Each accessor returns a cheap clone of the same logical client: reqwest's
/// connection pool, the token bucket, retry policy, and the dedicated executor
/// remain shared. The bucket is shared by (endpoint, requests-per-minute)
/// identity, so a client built for a different endpoint or rate gets its own. The concrete return type implements all of the typed ports,
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
        Self::from_production_settings_with_quota_path(api_key, retry_delays_seconds, None)
    }

    /// Construct production services while overriding the durable anonymous
    /// quota ledger location. Runtime deployment points this beside the SQLite
    /// database on its existing persistent data mount.
    pub fn from_production_settings_with_quota_path(
        api_key: Option<String>,
        retry_delays_seconds: &[i64],
        daily_quota_state_path: Option<PathBuf>,
    ) -> Result<Self, OpenDotaHttpBuildError> {
        let mut config = OpenDotaHttpConfig::production(api_key);
        if config.requests_per_day.is_some()
            && let Some(path) = daily_quota_state_path
        {
            config.daily_quota_state_path = Some(path);
        }
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

    /// Return shared quota state for health and bulk-work preflights.
    pub async fn quota_snapshot(&self) -> OpenDotaQuotaSnapshot {
        self.client.quota_snapshot().await
    }

    /// Blocking companion for synchronous discovery/enrichment workers.
    pub fn quota_snapshot_blocking(
        &self,
    ) -> Result<OpenDotaQuotaSnapshot, OpenDotaHttpExecutorError> {
        let client = self.client.clone();
        self.client
            .run_on_dedicated(async move { client.quota_snapshot().await })
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
        let limiter = shared_rate_limiter(&config.base_url, config.requests_per_minute);
        let quota_path = config.daily_quota_state_path.clone();
        let daily_quota = shared_daily_quota(
            &config.base_url,
            config.requests_per_day,
            quota_path.clone(),
        )
        .map_err(|source| OpenDotaHttpBuildError::QuotaState {
            path: quota_path.unwrap_or_else(|| PathBuf::from("<memory>")),
            source,
        })?;
        Ok(Self {
            http,
            config: Arc::new(config),
            limiter,
            daily_quota,
            executor,
            sleeper: Arc::new(TokioRetrySleeper),
        })
    }

    #[cfg(test)]
    fn with_test_sleeper(mut self, sleeper: Arc<dyn RetrySleeper>) -> Self {
        self.sleeper = sleeper;
        self
    }

    /// Return shared quota state without exposing API credentials.
    pub async fn quota_snapshot(&self) -> OpenDotaQuotaSnapshot {
        let daily = self.daily_quota.snapshot().await;
        let cooldown_remaining_seconds = self
            .limiter
            .cooldown_remaining()
            .await
            .map(|duration| duration.as_secs().max(1));
        OpenDotaQuotaSnapshot {
            local_daily_limit: self.config.requests_per_day,
            local_daily_used: daily.used,
            upstream_remaining_minute: self.limiter.upstream_remaining().await,
            upstream_remaining_day: daily.upstream_remaining,
            rate_limited_responses: daily.rate_limited_responses,
            local_rejections: daily.local_rejections,
            cooldown_remaining_seconds,
            persistence_healthy: daily.persistence_healthy,
        }
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
    /// always present. Discovery only needs identity and time, so project
    /// those two fields instead of downloading full historical stat rows.
    /// This lets production safely inspect a deeper history without adding
    /// requests or risking oversized responses.
    pub async fn get_player_matches(&self, steam_id: i64, limit: usize) -> Option<Vec<Value>> {
        self.get_player_matches_diagnostic(steam_id, limit)
            .await
            .ok()
    }

    async fn get_player_matches_diagnostic(
        &self,
        steam_id: i64,
        limit: usize,
    ) -> Result<Vec<Value>, String> {
        let params = vec![
            ("limit".to_owned(), limit.to_string()),
            // OpenDota defaults this endpoint to significant matches only.
            // Recorded in-house/Turbo/non-standard lobbies must remain visible
            // to discovery, and disabling the filter does not add a request.
            ("significant".to_owned(), "0".to_owned()),
            ("project".to_owned(), "match_id".to_owned()),
            ("project".to_owned(), "start_time".to_owned()),
        ];
        self.get_json_diagnostic(&format!("/players/{steam_id}/matches"), &params)
            .await
            .and_then(|value| {
                value.as_array().cloned().ok_or_else(|| {
                    format!("OpenDota player history for {steam_id} was not a JSON array")
                })
            })
    }

    /// Fetch aggregate player counts (including the region breakdown).
    pub async fn get_player_counts(&self, steam_id: i64) -> Option<Value> {
        self.get_json(&format!("/players/{steam_id}/counts"), &[])
            .await
    }

    /// Fetch raw match details. Empty/error objects are treated as missing,
    /// just as the Python integration does after JSON decoding.
    pub async fn get_match_details(&self, match_id: i64) -> Option<Value> {
        self.get_match_details_diagnostic(match_id).await.ok()
    }

    async fn get_match_details_diagnostic(&self, match_id: i64) -> Result<Value, String> {
        let value = self
            .get_json_diagnostic(&format!("/matches/{match_id}"), &[])
            .await?;
        let object = value
            .as_object()
            .ok_or_else(|| format!("OpenDota match {match_id} details were not a JSON object"))?;
        if object.is_empty() || object.contains_key("error") {
            Err(format!(
                "OpenDota match {match_id} returned an empty or error payload"
            ))
        } else {
            Ok(value)
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
        self.get_json_diagnostic(path, params).await.ok()
    }

    async fn get_json_diagnostic(
        &self,
        path: &str,
        params: &[(String, String)],
    ) -> Result<Value, String> {
        let response = self.make_request(path, params).await.ok_or_else(|| {
            format!(
                "OpenDota {path} produced no response (request deadline, local quota, or network failure)"
            )
        })?;
        if !response.status().is_success() {
            return Err(format!(
                "OpenDota {path} returned HTTP {}",
                response.status().as_u16()
            ));
        }
        let body = read_bounded(response, self.config.max_response_bytes)
            .await
            .ok_or_else(|| format!("OpenDota {path} returned an unreadable or oversized body"))?;
        serde_json::from_slice(&body)
            .map_err(|error| format!("OpenDota {path} returned invalid JSON: {error}"))
    }

    async fn make_request(&self, path: &str, params: &[(String, String)]) -> Option<Response> {
        let started_at = Instant::now();
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
            let quota_timeout = self.config.rate_limit_wait.min(remaining);
            if quota_timeout.is_zero() {
                self.daily_quota.record_local_rejection().await;
                return None;
            }
            if !self.limiter.acquire(quota_timeout).await {
                self.daily_quota.record_local_rejection().await;
                return None;
            }
            if !self.daily_quota.acquire(quota_timeout).await {
                self.limiter.refund().await;
                return None;
            }
            // Quota is acquired immediately before each send, not once before
            // the retry loop. This makes retries count as separate wire calls.
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

            self.observe_response_rate_limits(&response).await;

            if response.status() == StatusCode::TOO_MANY_REQUESTS {
                self.daily_quota.record_rate_limited().await;
                // A 429 is an instruction to stop, not a transient server
                // error to amplify. Open a shared circuit for future callers
                // and return without issuing another request in this loop.
                let cooldown = retry_after_duration(&response)
                    .unwrap_or(MINUTE_WINDOW)
                    .max(Duration::from_secs(1));
                self.limiter.observe_remaining(0, cooldown).await;
                return Some(response);
            }

            if !RETRYABLE_STATUS_CODES.contains(&response.status()) {
                return Some(response);
            }
            if attempt >= delays.len() {
                return Some(response);
            }
            let delay = delays[attempt].min(
                self.config
                    .request_deadline
                    .saturating_sub(started_at.elapsed()),
            );
            self.sleeper.sleep(delay).await;
        }
        None
    }

    async fn observe_response_rate_limits(&self, response: &Response) {
        if let Some(remaining) = response_header_u32(response, MINUTE_REMAINING_HEADERS) {
            self.limiter
                .observe_remaining(
                    remaining,
                    response_header_duration(response, MINUTE_RESET_HEADERS, MINUTE_WINDOW),
                )
                .await;
        }
        if let Some(remaining) = response_header_u32(response, DAY_REMAINING_HEADERS) {
            self.daily_quota
                .observe_remaining(
                    remaining,
                    response_header_duration(response, DAY_RESET_HEADERS, DAY_WINDOW),
                )
                .await;
        }
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

const MINUTE_REMAINING_HEADERS: &[&str] = &[
    "x-rate-limit-remaining-minute",
    "x-ratelimit-remaining-minute",
    "x-rate-limit-remaining",
    "x-ratelimit-remaining",
];
const DAY_REMAINING_HEADERS: &[&str] = &["x-rate-limit-remaining-day", "x-ratelimit-remaining-day"];
const MINUTE_RESET_HEADERS: &[&str] = &[
    "x-rate-limit-reset-minute",
    "x-ratelimit-reset-minute",
    "x-rate-limit-reset",
    "x-ratelimit-reset",
];
const DAY_RESET_HEADERS: &[&str] = &["x-rate-limit-reset-day", "x-ratelimit-reset-day"];

fn response_header<'a>(response: &'a Response, names: &[&str]) -> Option<&'a str> {
    names.iter().find_map(|name| {
        response
            .headers()
            .get(*name)
            .and_then(|value| value.to_str().ok())
    })
}

fn response_header_u32(response: &Response, names: &[&str]) -> Option<u32> {
    response_header(response, names)
        .and_then(|value| value.trim().parse::<u64>().ok())
        .and_then(|value| u32::try_from(value).ok())
}

fn response_header_duration(response: &Response, names: &[&str], fallback: Duration) -> Duration {
    let Some(value) =
        response_header(response, names).and_then(|value| value.trim().parse::<u64>().ok())
    else {
        return fallback;
    };

    // Providers commonly send reset as either seconds-from-now or a Unix
    // timestamp. Accept seconds and milliseconds timestamps, failing closed
    // to the normal window when a malformed/unsupported value is supplied.
    if value >= 1_000_000_000_000 {
        let now = Utc::now().timestamp_millis();
        return Duration::from_millis(value.saturating_sub(now.max(0) as u64));
    }
    if value >= 1_000_000_000 {
        let now = Utc::now().timestamp();
        return Duration::from_secs(value.saturating_sub(now.max(0) as u64));
    }
    Duration::from_secs(value)
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
            last_hits_at_10: object
                .get("lh_t")
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
        self.run_on_dedicated(async move {
            client
                .get_player_matches_diagnostic(steam_id.0, limit)
                .await
        })
        .map_err(|error| DiscoveryPortError::new(error.to_string()))?
        .map(|value| Some(project_history_matches(value)))
        .map_err(DiscoveryPortError::new)
    }

    fn match_details(
        &self,
        match_id: ValveMatchId,
    ) -> Result<Option<OpenDotaMatchDetails>, DiscoveryPortError> {
        let client = self.clone();
        self.run_on_dedicated(async move { client.get_match_details_diagnostic(match_id.0).await })
            .map_err(|error| DiscoveryPortError::new(error.to_string()))?
            .and_then(|value| {
                project_match_details(value, match_id.0)
                    .ok_or_else(|| format!("OpenDota match {} payload was incomplete", match_id.0))
            })
            .map(Some)
            .map_err(DiscoveryPortError::new)
    }
}

impl DotabuffIdExtractionPort for OpenDotaHttpClient {
    fn extract_player_id_from_dotabuff(&self, url: &str) -> Option<SteamId> {
        const STEAM_ID64_OFFSET: i64 = 76_561_197_960_265_728;
        const STEAM_ACCOUNT_ID_UPPER_BOUND: i64 = 1_i64 << 32;

        // Dotabuff profile links have existed in both forms: the current
        // account-ID form (`/players/<Steam32>`) and older links generated
        // from the 64-bit Steam ID. Parse the URL structurally so a query,
        // fragment, or profile sub-route cannot become part of the ID, and
        // do not accept a numeric prefix from an otherwise malformed segment.
        let parsed = reqwest::Url::parse(url).ok()?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return None;
        }
        let host = parsed.host_str()?.trim_end_matches('.');
        if host != "dotabuff.com" && !host.ends_with(".dotabuff.com") {
            return None;
        }
        let mut segments = parsed.path_segments()?;
        if segments.next()? != "players" {
            return None;
        }
        let raw_id = segments.next()?.parse::<i64>().ok()?;
        let steam_id = if (1..STEAM_ACCOUNT_ID_UPPER_BOUND).contains(&raw_id) {
            raw_id
        } else {
            raw_id.checked_sub(STEAM_ID64_OFFSET)?
        };
        (1..STEAM_ACCOUNT_ID_UPPER_BOUND)
            .contains(&steam_id)
            .then_some(SteamId(steam_id))
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
