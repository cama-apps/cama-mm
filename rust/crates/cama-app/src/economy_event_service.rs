//! Typed, dependency-injected policy for daily economy events.
//!
//! This module owns scheduling, selection, balance-sheet, and atomic-
//! application policy without opening SQLite or Discord. Runtime composition
//! uses the existing-schema SQLite facade while the in-memory adapters below
//! keep retry and concurrency behavior independently executable.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Timelike, Utc, Weekday};
use thiserror::Error;

pub const SECOND_ANNOUNCEMENT_DELAY_SECONDS: i64 = 12 * 60 * 60;
const SECONDS_PER_DAY: i64 = 86_400;
const TREND_WINDOWS: [i64; 3] = [1, 3, 7];
const FLOW_WINDOWS: [i64; 2] = [3, 7];
const SEVERITY_INTENSITY: [f64; 5] = [0.5, 1.0, 1.5, 2.25, 3.0];

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct GuildId(pub i64);

impl GuildId {
    #[must_use]
    pub const fn normalize(raw: Option<i64>) -> Self {
        Self(match raw {
            Some(value) => value,
            None => 0,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Deflationary,
    Neutral,
    Boon,
}

impl Direction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Deflationary => "deflationary",
            Self::Neutral => "neutral",
            Self::Boon => "boon",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EventTemplate {
    pub name: &'static str,
    pub hero: &'static str,
    pub direction: Direction,
    pub flavor: &'static str,
    pub ability_icon_slug: &'static str,
    pub reward_step: f64,
    pub gamba_win_step: f64,
    pub gamba_loss_step: f64,
    pub bet_step: f64,
    pub prediction_payout_step: f64,
    pub spread_step: i32,
    pub reserve_burn_step: f64,
    pub reserve_release_step: f64,
    pub wallet_burn_step: f64,
}

impl EventTemplate {
    #[must_use]
    pub fn ability_icon_url(self) -> String {
        format!(
            "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/abilities/{}.png",
            self.ability_icon_slug
        )
    }
}

pub const EVENT_CATALOG: [EventTemplate; 15] = [
    EventTemplate {
        name: "Ravage",
        hero: "Tidehunter",
        direction: Direction::Deflationary,
        flavor: "A tidal shock tears through every corner of the Jopacoin economy.",
        ability_icon_slug: "tidehunter_ravage",
        reward_step: -0.08,
        gamba_win_step: -0.03,
        gamba_loss_step: 0.04,
        bet_step: -0.015,
        prediction_payout_step: -0.005,
        spread_step: 1,
        reserve_burn_step: 0.004,
        reserve_release_step: 0.0,
        wallet_burn_step: 0.0,
    },
    EventTemplate {
        name: "Black Hole",
        hero: "Enigma",
        direction: Direction::Deflationary,
        flavor: "Liquidity is dragged toward a singularity and refuses to escape.",
        ability_icon_slug: "enigma_black_hole",
        reward_step: -0.05,
        gamba_win_step: -0.04,
        gamba_loss_step: 0.08,
        bet_step: -0.02,
        prediction_payout_step: -0.008,
        spread_step: 2,
        reserve_burn_step: 0.003,
        reserve_release_step: 0.0,
        wallet_burn_step: 0.0,
    },
    EventTemplate {
        name: "Doom",
        hero: "Doom",
        direction: Direction::Deflationary,
        flavor: "The server's opt-in rewards have been marked for destruction.",
        ability_icon_slug: "doom_bringer_doom",
        reward_step: -0.15,
        gamba_win_step: -0.08,
        gamba_loss_step: 0.10,
        bet_step: -0.025,
        prediction_payout_step: -0.01,
        spread_step: 1,
        reserve_burn_step: 0.0,
        reserve_release_step: 0.0,
        wallet_burn_step: 0.0,
    },
    EventTemplate {
        name: "Echo Slam",
        hero: "Earthshaker",
        direction: Direction::Deflationary,
        flavor: "Every transaction makes the next shock hit harder.",
        ability_icon_slug: "earthshaker_echo_slam",
        reward_step: -0.07,
        gamba_win_step: -0.04,
        gamba_loss_step: 0.12,
        bet_step: -0.015,
        prediction_payout_step: -0.005,
        spread_step: 1,
        reserve_burn_step: 0.002,
        reserve_release_step: 0.0,
        wallet_burn_step: 0.0,
    },
    EventTemplate {
        name: "Reaper's Scythe",
        hero: "Necrophos",
        direction: Direction::Deflationary,
        flavor: "Large voluntary risks now carry a much sharper edge.",
        ability_icon_slug: "necrolyte_reapers_scythe",
        reward_step: -0.04,
        gamba_win_step: -0.10,
        gamba_loss_step: 0.14,
        bet_step: -0.035,
        prediction_payout_step: -0.012,
        spread_step: 1,
        reserve_burn_step: 0.0,
        reserve_release_step: 0.0,
        wallet_burn_step: 0.0,
    },
    EventTemplate {
        name: "Global Silence",
        hero: "Silencer",
        direction: Direction::Deflationary,
        flavor: "Bonus rewards vanish and market makers fall quiet.",
        ability_icon_slug: "silencer_global_silence",
        reward_step: -0.12,
        gamba_win_step: -0.03,
        gamba_loss_step: 0.0,
        bet_step: -0.01,
        prediction_payout_step: 0.0,
        spread_step: 2,
        reserve_burn_step: 0.0,
        reserve_release_step: 0.0,
        wallet_burn_step: 0.0,
    },
    EventTemplate {
        name: "Sanity's Eclipse",
        hero: "Outworld Destroyer",
        direction: Direction::Deflationary,
        flavor: "A rare hard shock erases a thin layer of exposed liquidity.",
        ability_icon_slug: "obsidian_destroyer_sanity_eclipse",
        reward_step: -0.05,
        gamba_win_step: -0.03,
        gamba_loss_step: 0.0,
        bet_step: -0.01,
        prediction_payout_step: -0.005,
        spread_step: 1,
        reserve_burn_step: 0.0,
        reserve_release_step: 0.0,
        wallet_burn_step: 0.0005,
    },
    EventTemplate {
        name: "Chronosphere",
        hero: "Faceless Void",
        direction: Direction::Neutral,
        flavor: "Time stops around the order books while the wider economy catches up.",
        ability_icon_slug: "faceless_void_chronosphere",
        reward_step: 0.0,
        gamba_win_step: 0.0,
        gamba_loss_step: 0.0,
        bet_step: 0.0,
        prediction_payout_step: 0.0,
        spread_step: 2,
        reserve_burn_step: 0.0,
        reserve_release_step: 0.0,
        wallet_burn_step: 0.0,
    },
    EventTemplate {
        name: "Song of the Siren",
        hero: "Naga Siren",
        direction: Direction::Neutral,
        flavor: "Volatility sleeps; both upside and downside soften for a day.",
        ability_icon_slug: "naga_siren_song_of_the_siren",
        reward_step: 0.0,
        gamba_win_step: -0.04,
        gamba_loss_step: -0.04,
        bet_step: 0.0,
        prediction_payout_step: 0.0,
        spread_step: 1,
        reserve_burn_step: 0.0,
        reserve_release_step: 0.0,
        wallet_burn_step: 0.0,
    },
    EventTemplate {
        name: "Sunder",
        hero: "Terrorblade",
        direction: Direction::Neutral,
        flavor: "Wallet and Reserve liquidity trade places without changing supply.",
        ability_icon_slug: "terrorblade_sunder",
        reward_step: 0.0,
        gamba_win_step: 0.0,
        gamba_loss_step: 0.0,
        bet_step: 0.0,
        prediction_payout_step: 0.0,
        spread_step: 0,
        reserve_burn_step: 0.0,
        reserve_release_step: 0.002,
        wallet_burn_step: 0.0,
    },
    EventTemplate {
        name: "Hand of God",
        hero: "Chen",
        direction: Direction::Boon,
        flavor: "The Reserve opens and every voluntary economy surface receives aid.",
        ability_icon_slug: "chen_hand_of_god",
        reward_step: 0.05,
        gamba_win_step: 0.05,
        gamba_loss_step: -0.04,
        bet_step: 0.015,
        prediction_payout_step: 0.005,
        spread_step: -1,
        reserve_burn_step: 0.0,
        reserve_release_step: 0.004,
        wallet_burn_step: 0.0,
    },
    EventTemplate {
        name: "Guardian Angel",
        hero: "Omniknight",
        direction: Direction::Boon,
        flavor: "Losses are softened and market liquidity receives divine protection.",
        ability_icon_slug: "omniknight_guardian_angel",
        reward_step: 0.03,
        gamba_win_step: 0.03,
        gamba_loss_step: -0.08,
        bet_step: 0.01,
        prediction_payout_step: 0.004,
        spread_step: -1,
        reserve_burn_step: 0.0,
        reserve_release_step: 0.0,
        wallet_burn_step: 0.0,
    },
    EventTemplate {
        name: "Reincarnation",
        hero: "Wraith King",
        direction: Direction::Boon,
        flavor: "Defeated wagers rise again with part of their value restored.",
        ability_icon_slug: "skeleton_king_reincarnation",
        reward_step: 0.04,
        gamba_win_step: 0.06,
        gamba_loss_step: -0.10,
        bet_step: 0.015,
        prediction_payout_step: 0.006,
        spread_step: -1,
        reserve_burn_step: 0.0,
        reserve_release_step: 0.0,
        wallet_burn_step: 0.0,
    },
    EventTemplate {
        name: "Stampede",
        hero: "Centaur Warrunner",
        direction: Direction::Boon,
        flavor: "Activity surges and liquidity charges into every open market.",
        ability_icon_slug: "centaur_stampede",
        reward_step: 0.10,
        gamba_win_step: 0.04,
        gamba_loss_step: -0.03,
        bet_step: 0.02,
        prediction_payout_step: 0.005,
        spread_step: -1,
        reserve_burn_step: 0.0,
        reserve_release_step: 0.0,
        wallet_burn_step: 0.0,
    },
    EventTemplate {
        name: "Supernova",
        hero: "Phoenix",
        direction: Direction::Boon,
        flavor: "Reserve liquidity is reborn as a burst of server-wide economic heat.",
        ability_icon_slug: "phoenix_supernova",
        reward_step: 0.08,
        gamba_win_step: 0.07,
        gamba_loss_step: -0.05,
        bet_step: 0.02,
        prediction_payout_step: 0.008,
        spread_step: -1,
        reserve_burn_step: 0.0,
        reserve_release_step: 0.006,
        wallet_burn_step: 0.0,
    },
];

#[derive(Clone, Debug, PartialEq)]
pub struct EconomyEventEffects {
    pub reward_multiplier: f64,
    pub gamba_win_multiplier: f64,
    pub gamba_loss_multiplier: f64,
    pub bet_payout_multiplier: f64,
    pub prediction_payout_multiplier: f64,
    pub prediction_depth_multiplier: f64,
    pub prediction_spread_ticks_delta: i32,
    pub reserve_burn_jc: i64,
    pub reserve_release_jc: i64,
    pub wallet_burn_rate: f64,
    pub wallet_burn_jc: i64,
    pub reserve_voting_disabled: bool,
}

impl Default for EconomyEventEffects {
    fn default() -> Self {
        Self {
            reward_multiplier: 1.0,
            gamba_win_multiplier: 1.0,
            gamba_loss_multiplier: 1.0,
            bet_payout_multiplier: 1.0,
            prediction_payout_multiplier: 1.0,
            prediction_depth_multiplier: 1.0,
            prediction_spread_ticks_delta: 0,
            reserve_burn_jc: 0,
            reserve_release_jc: 0,
            wallet_burn_rate: 0.0,
            wallet_burn_jc: 0,
            reserve_voting_disabled: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PersistedEconomyEventEffects {
    pub reward_multiplier: Option<f64>,
    pub gamba_win_multiplier: Option<f64>,
    pub gamba_loss_multiplier: Option<f64>,
    pub bet_payout_multiplier: Option<f64>,
    pub prediction_payout_multiplier: Option<f64>,
    pub prediction_depth_multiplier: Option<f64>,
    pub prediction_spread_ticks_delta: Option<i32>,
    pub reserve_burn_jc: Option<i64>,
    pub reserve_release_jc: Option<i64>,
    pub wallet_burn_rate: Option<f64>,
    pub wallet_burn_jc: Option<i64>,
    pub reserve_voting_disabled: Option<bool>,
}

impl EconomyEventEffects {
    /// Decode persisted fields while deliberately neutralizing the retired
    /// prediction-depth modifier carried by legacy rows.
    #[must_use]
    pub fn from_persisted(raw: &PersistedEconomyEventEffects) -> Self {
        Self {
            reward_multiplier: raw.reward_multiplier.unwrap_or(1.0),
            gamba_win_multiplier: raw.gamba_win_multiplier.unwrap_or(1.0),
            gamba_loss_multiplier: raw.gamba_loss_multiplier.unwrap_or(1.0),
            bet_payout_multiplier: raw.bet_payout_multiplier.unwrap_or(1.0),
            prediction_payout_multiplier: raw.prediction_payout_multiplier.unwrap_or(1.0),
            prediction_depth_multiplier: 1.0,
            prediction_spread_ticks_delta: raw.prediction_spread_ticks_delta.unwrap_or(0),
            reserve_burn_jc: raw.reserve_burn_jc.unwrap_or(0),
            reserve_release_jc: raw.reserve_release_jc.unwrap_or(0),
            wallet_burn_rate: raw.wallet_burn_rate.unwrap_or(0.0),
            wallet_burn_jc: raw.wallet_burn_jc.unwrap_or(0),
            reserve_voting_disabled: raw.reserve_voting_disabled.unwrap_or(false),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BalanceSheet {
    pub player_wallets: i64,
    pub positive_wallets: i64,
    pub visible_debt: i64,
    pub player_count: usize,
    pub average_wallet: f64,
    pub reserve_available: i64,
    pub reserve_locked: i64,
    pub reserve_next_match_pot: i64,
    pub prediction_open_cash: i64,
    pub wager_escrow: i64,
    pub monetary_stock: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SurfaceVolumes {
    pub reward_credits: f64,
    pub gamba_credits: f64,
    pub gamba_debits: f64,
    pub bet_payouts: f64,
    pub prediction_payouts: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MonetaryTrend {
    pub requested_days: i64,
    pub elapsed_days: f64,
    pub anchor_date: String,
    pub anchor_stock: i64,
    pub current_stock: i64,
    pub change_jc: i64,
    pub period_rate: f64,
    pub daily_rate: f64,
    pub annualized_rate: f64,
    pub average_daily_change_jc: f64,
}

pub type MonetaryTrends = BTreeMap<i64, Option<MonetaryTrend>>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlowIndicator {
    pub days: i64,
    pub net_flow_jc: i64,
    pub gross_flow_jc: i64,
    pub inflow_jc: i64,
    pub outflow_jc: i64,
    pub entry_count: usize,
    pub active_players: usize,
    pub active_player_rate: f64,
    pub average_daily_net_jc: f64,
    pub average_daily_gross_jc: f64,
    pub daily_turnover_rate: f64,
}

pub type FlowIndicators = BTreeMap<i64, FlowIndicator>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DistributionIndicators {
    pub positive_player_count: usize,
    pub median_positive_wallet: f64,
    pub top_decile_count: usize,
    pub top_decile_share: f64,
    pub gini: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyMode {
    Normal,
    Disabled,
    Recovery,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PolicyState {
    pub guild_id: GuildId,
    pub mode: PolicyMode,
    pub target_annual_rate: f64,
    pub inflation_ceiling: f64,
    pub recovery_started_at: Option<i64>,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub guild_id: GuildId,
    pub snapshot_date: String,
    pub captured_at: i64,
    pub balance_sheet: BalanceSheet,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventStatus {
    Pending,
    Active,
    Failed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EconomyEvent {
    pub event_id: i64,
    pub guild_id: GuildId,
    pub event_date: String,
    pub name: String,
    pub hero: String,
    pub direction: Direction,
    pub severity: u8,
    pub target_effect_jc: i64,
    pub forecast_flow_jc: i64,
    pub expected_effect_jc: i64,
    pub direct_effect_jc: i64,
    pub actual_stock_change_jc: Option<i64>,
    pub monetary_stock_before: i64,
    pub effects: EconomyEventEffects,
    pub announcement: String,
    pub starts_at: i64,
    pub ends_at: i64,
    pub created_at: i64,
    pub announced_at: Option<i64>,
    pub reminder_announced_at: Option<i64>,
    pub status: EventStatus,
}

#[derive(Clone, Debug)]
pub struct EventDraft {
    pub event_date: String,
    pub name: String,
    pub hero: String,
    pub direction: Direction,
    pub severity: u8,
    pub target_effect_jc: i64,
    pub forecast_flow_jc: i64,
    pub expected_effect_jc: i64,
    pub monetary_stock_before: i64,
    pub effects: EconomyEventEffects,
    pub announcement: String,
    pub starts_at: i64,
    pub ends_at: i64,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppliedEvent {
    pub effects: EconomyEventEffects,
    pub direct_effect_jc: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EventClaim {
    Claimed(EconomyEvent),
    Existing(EconomyEvent),
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LedgerAccount {
    Player(i64),
    Nonprofit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LedgerEntry {
    pub guild_id: GuildId,
    pub account: LedgerAccount,
    pub delta: i64,
    pub source: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum EconomyEventError {
    #[error("invalid event date: {0}")]
    InvalidEventDate(String),
    #[error("timestamp is outside chrono's supported range: {0}")]
    InvalidTimestamp(i64),
    #[error("event store failure: {0}")]
    Store(String),
    #[error("economy application failure: {0}")]
    Economy(String),
    #[error("event {event_id} does not belong to guild {guild_id}")]
    EventOwnership { guild_id: i64, event_id: i64 },
}

pub trait Clock: Clone + Send + Sync + 'static {
    fn now(&self) -> i64;
}

#[derive(Clone, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        Utc::now().timestamp()
    }
}

#[derive(Clone, Debug)]
pub struct FixedClock {
    now: Arc<AtomicI64>,
}

impl FixedClock {
    #[must_use]
    pub fn new(now: i64) -> Self {
        Self {
            now: Arc::new(AtomicI64::new(now)),
        }
    }

    pub fn set(&self, now: i64) {
        self.now.store(now, Ordering::SeqCst);
    }
}

impl Clock for FixedClock {
    fn now(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
}

pub trait EventRng: Send + 'static {
    fn choose_index(&mut self, guild_id: GuildId, event_date: &str, upper: usize) -> usize;
}

#[derive(Clone, Debug, Default)]
pub struct DeterministicEventRng;

impl EventRng for DeterministicEventRng {
    fn choose_index(&mut self, guild_id: GuildId, event_date: &str, upper: usize) -> usize {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for byte in format!("{}:{event_date}", guild_id.0).bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        usize::try_from(hash % u64::try_from(upper.max(1)).unwrap_or(u64::MAX)).unwrap_or(0)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScriptedEventRng {
    choices: VecDeque<usize>,
}

impl ScriptedEventRng {
    #[must_use]
    pub fn new(choices: impl IntoIterator<Item = usize>) -> Self {
        Self {
            choices: choices.into_iter().collect(),
        }
    }
}

impl EventRng for ScriptedEventRng {
    fn choose_index(&mut self, _guild_id: GuildId, _event_date: &str, upper: usize) -> usize {
        self.choices.pop_front().unwrap_or(0) % upper.max(1)
    }
}

pub trait EventStore: Clone + Send + Sync + 'static {
    fn ensure_policy(
        &self,
        guild_id: GuildId,
        mode: PolicyMode,
        target_annual_rate: f64,
        inflation_ceiling: f64,
        now: i64,
    ) -> Result<PolicyState, EconomyEventError>;
    fn event_for_date(
        &self,
        guild_id: GuildId,
        event_date: &str,
    ) -> Result<Option<EconomyEvent>, EconomyEventError>;
    fn claim_event(
        &self,
        guild_id: GuildId,
        draft: EventDraft,
    ) -> Result<EventClaim, EconomyEventError>;
    fn activate_event(
        &self,
        guild_id: GuildId,
        event_id: i64,
        applied: AppliedEvent,
    ) -> Result<EconomyEvent, EconomyEventError>;
    fn mark_event_failed(&self, guild_id: GuildId, event_id: i64) -> Result<(), EconomyEventError>;
    fn reconcile_prior_event(
        &self,
        guild_id: GuildId,
        event_date: &str,
        current_stock: i64,
    ) -> Result<(), EconomyEventError>;
    fn save_snapshot(&self, snapshot: Snapshot) -> Result<(), EconomyEventError>;
    fn latest_snapshot(&self, guild_id: GuildId) -> Result<Option<Snapshot>, EconomyEventError>;
    fn monetary_trends(
        &self,
        guild_id: GuildId,
        current_stock: i64,
        now: i64,
    ) -> Result<MonetaryTrends, EconomyEventError>;
    fn mark_announced(
        &self,
        guild_id: GuildId,
        event_id: i64,
        now: i64,
    ) -> Result<(), EconomyEventError>;
    fn mark_reminder_announced(
        &self,
        guild_id: GuildId,
        event_id: i64,
        now: i64,
    ) -> Result<(), EconomyEventError>;
}

pub trait EconomyPort: Clone + Send + Sync + 'static {
    fn capture_balance_sheet(&self, guild_id: GuildId) -> Result<BalanceSheet, EconomyEventError>;
    fn surface_daily_volumes(
        &self,
        guild_id: GuildId,
        lookback_days: i64,
        now: i64,
    ) -> Result<SurfaceVolumes, EconomyEventError>;
    fn flow_indicators(
        &self,
        guild_id: GuildId,
        current_stock: i64,
        player_count: usize,
        now: i64,
    ) -> Result<FlowIndicators, EconomyEventError>;
    fn distribution_indicators(
        &self,
        guild_id: GuildId,
    ) -> Result<DistributionIndicators, EconomyEventError>;
    fn apply_event_atomic(
        &self,
        guild_id: GuildId,
        event: &EconomyEvent,
    ) -> Result<AppliedEvent, EconomyEventError>;
}

#[derive(Default)]
struct StoreState {
    next_event_id: i64,
    policies: BTreeMap<GuildId, PolicyState>,
    events: BTreeMap<(GuildId, String), EconomyEvent>,
    snapshots: BTreeMap<(GuildId, String), Snapshot>,
}

#[derive(Clone, Default)]
pub struct MemoryEventStore {
    state: Arc<Mutex<StoreState>>,
}

impl MemoryEventStore {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, StoreState>, EconomyEventError> {
        self.state
            .lock()
            .map_err(|_| EconomyEventError::Store("memory store lock poisoned".into()))
    }

    pub fn seed_snapshot(&self, snapshot: Snapshot) -> Result<(), EconomyEventError> {
        self.save_snapshot(snapshot)
    }
}

impl EventStore for MemoryEventStore {
    fn ensure_policy(
        &self,
        guild_id: GuildId,
        mode: PolicyMode,
        target_annual_rate: f64,
        inflation_ceiling: f64,
        now: i64,
    ) -> Result<PolicyState, EconomyEventError> {
        let mut state = self.lock()?;
        let previous = state.policies.get(&guild_id);
        let recovery_started_at = if mode == PolicyMode::Recovery {
            previous
                .filter(|policy| policy.mode == PolicyMode::Recovery)
                .and_then(|policy| policy.recovery_started_at)
                .or(Some(now))
        } else {
            previous.and_then(|policy| policy.recovery_started_at)
        };
        let policy = PolicyState {
            guild_id,
            mode,
            target_annual_rate,
            inflation_ceiling,
            recovery_started_at,
            updated_at: now,
        };
        state.policies.insert(guild_id, policy.clone());
        Ok(policy)
    }

    fn event_for_date(
        &self,
        guild_id: GuildId,
        event_date: &str,
    ) -> Result<Option<EconomyEvent>, EconomyEventError> {
        Ok(self
            .lock()?
            .events
            .get(&(guild_id, event_date.to_owned()))
            .cloned())
    }

    fn claim_event(
        &self,
        guild_id: GuildId,
        draft: EventDraft,
    ) -> Result<EventClaim, EconomyEventError> {
        let mut state = self.lock()?;
        let key = (guild_id, draft.event_date.clone());
        if let Some(event) = state.events.get(&key) {
            return Ok(EventClaim::Existing(event.clone()));
        }
        state.next_event_id += 1;
        let event = EconomyEvent {
            event_id: state.next_event_id,
            guild_id,
            event_date: draft.event_date,
            name: draft.name,
            hero: draft.hero,
            direction: draft.direction,
            severity: draft.severity,
            target_effect_jc: draft.target_effect_jc,
            forecast_flow_jc: draft.forecast_flow_jc,
            expected_effect_jc: draft.expected_effect_jc,
            direct_effect_jc: 0,
            actual_stock_change_jc: None,
            monetary_stock_before: draft.monetary_stock_before,
            effects: draft.effects,
            announcement: draft.announcement,
            starts_at: draft.starts_at,
            ends_at: draft.ends_at,
            created_at: draft.created_at,
            announced_at: None,
            reminder_announced_at: None,
            status: EventStatus::Pending,
        };
        state.events.insert(key, event.clone());
        Ok(EventClaim::Claimed(event))
    }

    fn activate_event(
        &self,
        guild_id: GuildId,
        event_id: i64,
        applied: AppliedEvent,
    ) -> Result<EconomyEvent, EconomyEventError> {
        let mut state = self.lock()?;
        let Some(event) = state
            .events
            .values_mut()
            .find(|event| event.event_id == event_id && event.guild_id == guild_id)
        else {
            return Err(EconomyEventError::EventOwnership {
                guild_id: guild_id.0,
                event_id,
            });
        };
        event.effects = applied.effects;
        event.direct_effect_jc = applied.direct_effect_jc;
        event.status = EventStatus::Active;
        Ok(event.clone())
    }

    fn mark_event_failed(&self, guild_id: GuildId, event_id: i64) -> Result<(), EconomyEventError> {
        let mut state = self.lock()?;
        let Some(event) = state
            .events
            .values_mut()
            .find(|event| event.event_id == event_id && event.guild_id == guild_id)
        else {
            return Err(EconomyEventError::EventOwnership {
                guild_id: guild_id.0,
                event_id,
            });
        };
        event.status = EventStatus::Failed;
        Ok(())
    }

    fn reconcile_prior_event(
        &self,
        guild_id: GuildId,
        event_date: &str,
        current_stock: i64,
    ) -> Result<(), EconomyEventError> {
        let mut state = self.lock()?;
        if let Some(event) = state
            .events
            .iter_mut()
            .filter(|((candidate_guild, candidate_date), event)| {
                *candidate_guild == guild_id
                    && candidate_date.as_str() < event_date
                    && event.actual_stock_change_jc.is_none()
            })
            .map(|(_, event)| event)
            .next_back()
        {
            event.actual_stock_change_jc = Some(current_stock - event.monetary_stock_before);
        }
        Ok(())
    }

    fn save_snapshot(&self, snapshot: Snapshot) -> Result<(), EconomyEventError> {
        self.lock()?.snapshots.insert(
            (snapshot.guild_id, snapshot.snapshot_date.clone()),
            snapshot,
        );
        Ok(())
    }

    fn latest_snapshot(&self, guild_id: GuildId) -> Result<Option<Snapshot>, EconomyEventError> {
        Ok(self
            .lock()?
            .snapshots
            .iter()
            .filter(|((candidate, _), _)| *candidate == guild_id)
            .map(|(_, snapshot)| snapshot.clone())
            .next_back())
    }

    fn monetary_trends(
        &self,
        guild_id: GuildId,
        current_stock: i64,
        now: i64,
    ) -> Result<MonetaryTrends, EconomyEventError> {
        let state = self.lock()?;
        let mut trends = BTreeMap::new();
        for days in TREND_WINDOWS {
            let target_age = days * SECONDS_PER_DAY;
            let minimum_age = target_age * 3 / 4;
            let maximum_age = target_age * 2;
            let anchor = state
                .snapshots
                .values()
                .filter(|snapshot| snapshot.guild_id == guild_id)
                .filter(|snapshot| {
                    let age = now - snapshot.captured_at;
                    (minimum_age..=maximum_age).contains(&age)
                })
                .min_by_key(|snapshot| {
                    (
                        (snapshot.captured_at - (now - target_age)).abs(),
                        -snapshot.captured_at,
                    )
                });
            let trend = anchor.and_then(|snapshot| {
                let anchor_stock = snapshot.balance_sheet.monetary_stock;
                let elapsed_days = (now - snapshot.captured_at) as f64 / SECONDS_PER_DAY as f64;
                if anchor_stock <= 0 || current_stock <= 0 || elapsed_days <= 0.0 {
                    return None;
                }
                let ratio = current_stock as f64 / anchor_stock as f64;
                let change_jc = current_stock - anchor_stock;
                Some(MonetaryTrend {
                    requested_days: days,
                    elapsed_days,
                    anchor_date: snapshot.snapshot_date.clone(),
                    anchor_stock,
                    current_stock,
                    change_jc,
                    period_rate: ratio - 1.0,
                    daily_rate: ratio.powf(1.0 / elapsed_days) - 1.0,
                    annualized_rate: ratio.powf(365.0 / elapsed_days) - 1.0,
                    average_daily_change_jc: change_jc as f64 / elapsed_days,
                })
            });
            trends.insert(days, trend);
        }
        Ok(trends)
    }

    fn mark_announced(
        &self,
        guild_id: GuildId,
        event_id: i64,
        now: i64,
    ) -> Result<(), EconomyEventError> {
        let mut state = self.lock()?;
        let Some(event) = state
            .events
            .values_mut()
            .find(|event| event.guild_id == guild_id && event.event_id == event_id)
        else {
            return Err(EconomyEventError::EventOwnership {
                guild_id: guild_id.0,
                event_id,
            });
        };
        event.announced_at.get_or_insert(now);
        Ok(())
    }

    fn mark_reminder_announced(
        &self,
        guild_id: GuildId,
        event_id: i64,
        now: i64,
    ) -> Result<(), EconomyEventError> {
        let mut state = self.lock()?;
        let Some(event) = state
            .events
            .values_mut()
            .find(|event| event.guild_id == guild_id && event.event_id == event_id)
        else {
            return Err(EconomyEventError::EventOwnership {
                guild_id: guild_id.0,
                event_id,
            });
        };
        event.reminder_announced_at.get_or_insert(now);
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
struct GuildEconomy {
    players: BTreeMap<i64, i64>,
    reserve_available: i64,
    reserve_locked: i64,
    reserve_next_match_pot: i64,
    prediction_open_cash: i64,
    wager_escrow: i64,
}

#[derive(Default)]
struct EconomyState {
    guilds: BTreeMap<GuildId, GuildEconomy>,
    ledger: Vec<LedgerEntry>,
    applied: BTreeMap<(GuildId, i64), AppliedEvent>,
    fail_next_apply: Option<String>,
}

#[derive(Clone, Default)]
pub struct MemoryEconomy {
    state: Arc<Mutex<EconomyState>>,
}

impl MemoryEconomy {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, EconomyState>, EconomyEventError> {
        self.state
            .lock()
            .map_err(|_| EconomyEventError::Economy("memory economy lock poisoned".into()))
    }

    pub fn add_player(
        &self,
        guild_id: GuildId,
        player_id: i64,
        balance: i64,
    ) -> Result<(), EconomyEventError> {
        self.lock()?
            .guilds
            .entry(guild_id)
            .or_default()
            .players
            .insert(player_id, balance);
        Ok(())
    }

    pub fn set_player_balance(
        &self,
        guild_id: GuildId,
        player_id: i64,
        balance: i64,
    ) -> Result<(), EconomyEventError> {
        self.add_player(guild_id, player_id, balance)
    }

    pub fn player_balance(
        &self,
        guild_id: GuildId,
        player_id: i64,
    ) -> Result<Option<i64>, EconomyEventError> {
        Ok(self
            .lock()?
            .guilds
            .get(&guild_id)
            .and_then(|guild| guild.players.get(&player_id).copied()))
    }

    pub fn set_reserve(&self, guild_id: GuildId, amount: i64) -> Result<(), EconomyEventError> {
        self.lock()?
            .guilds
            .entry(guild_id)
            .or_default()
            .reserve_available = amount;
        Ok(())
    }

    pub fn reserve(&self, guild_id: GuildId) -> Result<i64, EconomyEventError> {
        Ok(self
            .lock()?
            .guilds
            .get(&guild_id)
            .map_or(0, |guild| guild.reserve_available))
    }

    pub fn add_ledger_entry(&self, entry: LedgerEntry) -> Result<(), EconomyEventError> {
        self.lock()?.ledger.push(entry);
        Ok(())
    }

    pub fn ledger_totals(
        &self,
        guild_id: GuildId,
        source: &str,
    ) -> Result<BTreeMap<&'static str, i64>, EconomyEventError> {
        let state = self.lock()?;
        let mut totals = BTreeMap::new();
        for entry in state
            .ledger
            .iter()
            .filter(|entry| entry.guild_id == guild_id && entry.source == source)
        {
            let account = match entry.account {
                LedgerAccount::Player(_) => "player",
                LedgerAccount::Nonprofit => "nonprofit",
            };
            *totals.entry(account).or_insert(0) += entry.delta;
        }
        Ok(totals)
    }

    pub fn fail_next_apply(&self, message: impl Into<String>) -> Result<(), EconomyEventError> {
        self.lock()?.fail_next_apply = Some(message.into());
        Ok(())
    }
}

impl EconomyPort for MemoryEconomy {
    fn capture_balance_sheet(&self, guild_id: GuildId) -> Result<BalanceSheet, EconomyEventError> {
        let state = self.lock()?;
        let guild = state.guilds.get(&guild_id).cloned().unwrap_or_default();
        let player_wallets = guild.players.values().sum();
        let positive_wallets = guild.players.values().filter(|value| **value > 0).sum();
        let visible_debt = -guild
            .players
            .values()
            .filter(|value| **value < 0)
            .sum::<i64>();
        let player_count = guild.players.len();
        let average_wallet = if player_count == 0 {
            0.0
        } else {
            player_wallets as f64 / player_count as f64
        };
        let monetary_stock = player_wallets
            + guild.reserve_available
            + guild.reserve_locked
            + guild.reserve_next_match_pot
            + guild.prediction_open_cash
            + guild.wager_escrow;
        Ok(BalanceSheet {
            player_wallets,
            positive_wallets,
            visible_debt,
            player_count,
            average_wallet,
            reserve_available: guild.reserve_available,
            reserve_locked: guild.reserve_locked,
            reserve_next_match_pot: guild.reserve_next_match_pot,
            prediction_open_cash: guild.prediction_open_cash,
            wager_escrow: guild.wager_escrow,
            monetary_stock,
        })
    }

    fn surface_daily_volumes(
        &self,
        guild_id: GuildId,
        lookback_days: i64,
        now: i64,
    ) -> Result<SurfaceVolumes, EconomyEventError> {
        let days = lookback_days.max(1);
        let cutoff = now - days * SECONDS_PER_DAY;
        let state = self.lock()?;
        let mut result = SurfaceVolumes::default();
        for entry in state
            .ledger
            .iter()
            .filter(|entry| entry.guild_id == guild_id && entry.created_at >= cutoff)
        {
            let credits = entry.delta.max(0) as f64 / days as f64;
            let debits = (-entry.delta).max(0) as f64 / days as f64;
            match entry.source.as_str() {
                "dig" | "trivia" | "player_trivia" | "mana_reward" | "manashop_buff" => {
                    result.reward_credits += credits;
                }
                "gamba" => {
                    result.gamba_credits += credits;
                    result.gamba_debits += debits;
                }
                "bet_settlement" => result.bet_payouts += credits,
                "prediction_resolution" => result.prediction_payouts += credits,
                _ => {}
            }
        }
        Ok(result)
    }

    fn flow_indicators(
        &self,
        guild_id: GuildId,
        current_stock: i64,
        player_count: usize,
        now: i64,
    ) -> Result<FlowIndicators, EconomyEventError> {
        let state = self.lock()?;
        let mut result = BTreeMap::new();
        for days in FLOW_WINDOWS {
            let cutoff = now - days * SECONDS_PER_DAY;
            let entries = state.ledger.iter().filter(|entry| {
                entry.guild_id == guild_id
                    && (cutoff..=now).contains(&entry.created_at)
                    && entry.source != "ledger_backfill"
            });
            let mut net = 0;
            let mut gross = 0;
            let mut inflow = 0;
            let mut outflow = 0;
            let mut count = 0;
            let mut active_players = BTreeSet::new();
            for entry in entries {
                net += entry.delta;
                gross += entry.delta.abs();
                inflow += entry.delta.max(0);
                outflow += (-entry.delta).max(0);
                count += 1;
                if let LedgerAccount::Player(player_id) = entry.account {
                    active_players.insert(player_id);
                }
            }
            let active_count = active_players.len();
            result.insert(
                days,
                FlowIndicator {
                    days,
                    net_flow_jc: net,
                    gross_flow_jc: gross,
                    inflow_jc: inflow,
                    outflow_jc: outflow,
                    entry_count: count,
                    active_players: active_count,
                    active_player_rate: if player_count == 0 {
                        0.0
                    } else {
                        active_count as f64 / player_count as f64
                    },
                    average_daily_net_jc: net as f64 / days as f64,
                    average_daily_gross_jc: gross as f64 / days as f64,
                    daily_turnover_rate: if current_stock > 0 {
                        gross as f64 / days as f64 / current_stock as f64
                    } else {
                        0.0
                    },
                },
            );
        }
        Ok(result)
    }

    fn distribution_indicators(
        &self,
        guild_id: GuildId,
    ) -> Result<DistributionIndicators, EconomyEventError> {
        let state = self.lock()?;
        let mut balances: Vec<i64> = state
            .guilds
            .get(&guild_id)
            .into_iter()
            .flat_map(|guild| guild.players.values().copied())
            .filter(|balance| *balance > 0)
            .collect();
        balances.sort_unstable();
        let count = balances.len();
        let total: i64 = balances.iter().sum();
        if count == 0 || total <= 0 {
            return Ok(DistributionIndicators::default());
        }
        let top_decile_count = count.div_ceil(10).max(1);
        let midpoint = count / 2;
        let median = if count % 2 == 1 {
            balances[midpoint] as f64
        } else {
            (balances[midpoint - 1] + balances[midpoint]) as f64 / 2.0
        };
        let weighted_sum: i64 = balances
            .iter()
            .enumerate()
            .map(|(index, balance)| i64::try_from(index + 1).unwrap_or(i64::MAX) * balance)
            .sum();
        let gini = ((2 * weighted_sum) as f64 / (count as f64 * total as f64)
            - (count as f64 + 1.0) / count as f64)
            .clamp(0.0, 1.0);
        Ok(DistributionIndicators {
            positive_player_count: count,
            median_positive_wallet: median,
            top_decile_count,
            top_decile_share: balances[count - top_decile_count..].iter().sum::<i64>() as f64
                / total as f64,
            gini,
        })
    }

    fn apply_event_atomic(
        &self,
        guild_id: GuildId,
        event: &EconomyEvent,
    ) -> Result<AppliedEvent, EconomyEventError> {
        if event.guild_id != guild_id {
            return Err(EconomyEventError::EventOwnership {
                guild_id: guild_id.0,
                event_id: event.event_id,
            });
        }
        let mut state = self.lock()?;
        if let Some(applied) = state.applied.get(&(guild_id, event.event_id)) {
            return Ok(applied.clone());
        }
        if let Some(message) = state.fail_next_apply.take() {
            return Err(EconomyEventError::Economy(message));
        }

        let mut effects = event.effects.clone();
        let mut new_entries = Vec::new();
        let guild = state.guilds.entry(guild_id).or_default();
        let reserve_burn = effects.reserve_burn_jc.max(0).min(guild.reserve_available);
        if reserve_burn > 0 {
            guild.reserve_available -= reserve_burn;
            new_entries.push(LedgerEntry {
                guild_id,
                account: LedgerAccount::Nonprofit,
                delta: -reserve_burn,
                source: "economy_event".into(),
                created_at: event.created_at,
            });
        }

        let mut wallet_burn = 0;
        let wallet_burn_rate = effects.wallet_burn_rate.max(0.0);
        if wallet_burn_rate > 0.0 {
            for (player_id, balance) in &mut guild.players {
                if *balance <= 0 {
                    continue;
                }
                let debit = (*balance as f64 * wallet_burn_rate) as i64;
                if debit > 0 {
                    *balance -= debit;
                    wallet_burn += debit;
                    new_entries.push(LedgerEntry {
                        guild_id,
                        account: LedgerAccount::Player(*player_id),
                        delta: -debit,
                        source: "economy_event".into(),
                        created_at: event.created_at,
                    });
                }
            }
        }

        let requested_release = effects.reserve_release_jc.max(0);
        let reserve_release = requested_release.min(guild.reserve_available);
        let mut released = 0;
        if reserve_release > 0 && !guild.players.is_empty() {
            let player_count = i64::try_from(guild.players.len()).unwrap_or(i64::MAX);
            let quotient = reserve_release / player_count;
            let remainder = reserve_release % player_count;
            guild.reserve_available -= reserve_release;
            new_entries.push(LedgerEntry {
                guild_id,
                account: LedgerAccount::Nonprofit,
                delta: -reserve_release,
                source: "economy_event".into(),
                created_at: event.created_at,
            });
            for (index, (player_id, balance)) in guild.players.iter_mut().enumerate() {
                let credit =
                    quotient + i64::from(i64::try_from(index).unwrap_or(i64::MAX) < remainder);
                if credit > 0 {
                    *balance += credit;
                    released += credit;
                    new_entries.push(LedgerEntry {
                        guild_id,
                        account: LedgerAccount::Player(*player_id),
                        delta: credit,
                        source: "economy_event".into(),
                        created_at: event.created_at,
                    });
                }
            }
        }

        effects.reserve_burn_jc = reserve_burn;
        effects.wallet_burn_jc = wallet_burn;
        effects.reserve_release_jc = released;
        let applied = AppliedEvent {
            effects,
            direct_effect_jc: -reserve_burn - wallet_burn,
        };
        state.ledger.extend(new_entries);
        state
            .applied
            .insert((guild_id, event.event_id), applied.clone());
        Ok(applied)
    }
}

#[derive(Clone, Debug)]
pub struct EconomyEventConfig {
    pub enabled: bool,
    pub normal_annual_rate: f64,
    pub inflation_ceiling: f64,
    pub lookback_days: i64,
    pub max_reserve_burn_pct: f64,
    pub max_wallet_burn_pct: f64,
    pub trigger_hour_local: u8,
}

impl Default for EconomyEventConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            normal_annual_rate: 0.02,
            inflation_ceiling: 0.10,
            lookback_days: 7,
            max_reserve_burn_pct: 0.03,
            max_wallet_burn_pct: 0.0025,
            trigger_hour_local: 10,
        }
    }
}

impl EconomyEventConfig {
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.normal_annual_rate = self
            .normal_annual_rate
            .max(-0.99)
            .min(self.inflation_ceiling);
        self.lookback_days = self.lookback_days.max(1);
        self.max_reserve_burn_pct = self.max_reserve_burn_pct.clamp(0.0, 1.0);
        self.max_wallet_burn_pct = self.max_wallet_burn_pct.clamp(0.0, 0.05);
        self.trigger_hour_local = self.trigger_hour_local.min(23);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReserveVotingRestriction {
    pub event_id: i64,
    pub name: String,
    pub severity: u8,
}

pub struct EconomyEventService<S, E, C, R> {
    store: S,
    economy: E,
    clock: C,
    rng: R,
    config: EconomyEventConfig,
}

impl<S, E, C, R> EconomyEventService<S, E, C, R>
where
    S: EventStore,
    E: EconomyPort,
    C: Clock,
    R: EventRng,
{
    #[must_use]
    pub fn new(store: S, economy: E, clock: C, rng: R, config: EconomyEventConfig) -> Self {
        Self {
            store,
            economy,
            clock,
            rng,
            config: config.normalized(),
        }
    }

    pub fn ensure_policy(&self, guild_id: Option<i64>) -> Result<PolicyState, EconomyEventError> {
        self.ensure_policy_at(GuildId::normalize(guild_id), self.clock.now())
    }

    fn ensure_policy_at(
        &self,
        guild_id: GuildId,
        now: i64,
    ) -> Result<PolicyState, EconomyEventError> {
        let mode = if self.config.enabled {
            PolicyMode::Normal
        } else {
            PolicyMode::Disabled
        };
        self.store.ensure_policy(
            guild_id,
            mode,
            self.config.normal_annual_rate,
            self.config.inflation_ceiling,
            now,
        )
    }

    pub fn event_date_for_timestamp(&self, timestamp: i64) -> Result<String, EconomyEventError> {
        let local = pacific_local(timestamp)?;
        let mut date = local.date();
        if local.hour() < u32::from(self.config.trigger_hour_local) {
            date -= Duration::days(1);
        }
        Ok(date.format("%Y-%m-%d").to_string())
    }

    pub fn event_window(&self, event_date: &str) -> Result<(i64, i64), EconomyEventError> {
        let date = parse_event_date(event_date)?;
        let start = pacific_timestamp(date, self.config.trigger_hour_local)?;
        let end = pacific_timestamp(
            date.checked_add_signed(Duration::days(1))
                .ok_or_else(|| EconomyEventError::InvalidEventDate(event_date.to_owned()))?,
            self.config.trigger_hour_local,
        )?;
        Ok((start, end))
    }

    pub fn seconds_until_next_trigger(&self, now: Option<i64>) -> Result<i64, EconomyEventError> {
        let now = now.unwrap_or_else(|| self.clock.now());
        let local = pacific_local(now)?;
        let mut date = local.date();
        let mut next = pacific_timestamp(date, self.config.trigger_hour_local)?;
        if now >= next {
            date = date
                .checked_add_signed(Duration::days(1))
                .ok_or(EconomyEventError::InvalidTimestamp(now))?;
            next = pacific_timestamp(date, self.config.trigger_hour_local)?;
        }
        Ok((next - now).max(0))
    }

    /// Whether the Pacific-local daily trigger has not happened yet.
    ///
    /// Production SQLite composition uses this scheduler guard while sharing
    /// the same DST-aware calendar policy as the dependency-injected service.
    pub fn is_before_daily_trigger(&self, now: i64) -> Result<bool, EconomyEventError> {
        Ok(pacific_local(now)?.hour() < u32::from(self.config.trigger_hour_local))
    }

    pub fn get_effects(
        &self,
        guild_id: Option<i64>,
    ) -> Result<EconomyEventEffects, EconomyEventError> {
        if !self.config.enabled {
            return Ok(EconomyEventEffects::default());
        }
        let guild_id = GuildId::normalize(guild_id);
        let event_date = self.event_date_for_timestamp(self.clock.now())?;
        Ok(self
            .store
            .event_for_date(guild_id, &event_date)?
            .filter(|event| event.status == EventStatus::Active)
            .map_or_else(EconomyEventEffects::default, |event| event.effects))
    }

    pub fn get_reserve_voting_restriction(
        &self,
        guild_id: Option<i64>,
        now: Option<i64>,
    ) -> Result<Option<ReserveVotingRestriction>, EconomyEventError> {
        if !self.config.enabled {
            return Ok(None);
        }
        let now = now.unwrap_or_else(|| self.clock.now());
        let guild_id = GuildId::normalize(guild_id);
        let event_date = self.event_date_for_timestamp(now)?;
        let restriction = self
            .store
            .event_for_date(guild_id, &event_date)?
            .filter(|event| {
                event.status == EventStatus::Active
                    && event.direction == Direction::Deflationary
                    && event.severity >= 3
                    && event.starts_at <= now
                    && now < event.ends_at
            })
            .map(|event| ReserveVotingRestriction {
                event_id: event.event_id,
                name: event.name,
                severity: event.severity,
            });
        Ok(restriction)
    }

    pub fn adjust_reward(
        &self,
        guild_id: Option<i64>,
        amount: i64,
    ) -> Result<i64, EconomyEventError> {
        if amount <= 0 {
            return Ok(amount);
        }
        let multiplier = self.get_effects(guild_id)?.reward_multiplier;
        Ok((((amount as f64 * multiplier) + 0.5) as i64).max(0))
    }

    #[must_use]
    pub fn band_for_weekly_deviation(weekly_deviation: f64) -> (Direction, u8) {
        let magnitude = weekly_deviation.abs();
        if magnitude <= 0.05 {
            return (Direction::Neutral, 1);
        }
        let direction = if weekly_deviation > 0.0 {
            Direction::Deflationary
        } else {
            Direction::Boon
        };
        let severity = if magnitude <= 0.10 {
            1
        } else if magnitude <= 0.20 {
            2
        } else if magnitude <= 0.40 {
            3
        } else if magnitude <= 0.80 {
            4
        } else {
            5
        };
        (direction, severity)
    }

    pub fn ensure_daily_event(
        &mut self,
        guild_id: Option<i64>,
        now: Option<i64>,
        explicit_event_date: Option<&str>,
    ) -> Result<(Option<EconomyEvent>, bool), EconomyEventError> {
        let now = now.unwrap_or_else(|| self.clock.now());
        let guild_id = GuildId::normalize(guild_id);
        let event_date = match explicit_event_date {
            Some(date) => {
                parse_event_date(date)?;
                date.to_owned()
            }
            None => self.event_date_for_timestamp(now)?,
        };
        let policy = self.ensure_policy_at(guild_id, now)?;
        if !self.config.enabled || policy.mode == PolicyMode::Disabled {
            return Ok((None, false));
        }

        if let Some(existing) = self.store.event_for_date(guild_id, &event_date)? {
            if existing.status == EventStatus::Active {
                return Ok((Some(existing), false));
            }
            let recovered = self.finish_claim(guild_id, existing, &event_date, now)?;
            return Ok((Some(recovered), false));
        }

        if explicit_event_date.is_none()
            && pacific_local(now)?.hour() < u32::from(self.config.trigger_hour_local)
        {
            return Ok((None, false));
        }

        let before = self.economy.capture_balance_sheet(guild_id)?;
        let monetary_stock = before.monetary_stock;
        self.store
            .reconcile_prior_event(guild_id, &event_date, monetary_stock)?;
        let trends = self.store.monetary_trends(guild_id, monetary_stock, now)?;
        let (direction, severity, required_effect, observed_daily_flow) = trends
            .get(&7)
            .and_then(Option::as_ref)
            .map_or((Direction::Neutral, 1, 0, 0), |weekly| {
                let elapsed_days = weekly.elapsed_days.max(1.0);
                let target_period_rate =
                    (1.0 + policy.target_annual_rate).powf(elapsed_days / 365.0) - 1.0;
                let weekly_deviation = weekly.period_rate - target_period_rate;
                let (direction, severity) = Self::band_for_weekly_deviation(weekly_deviation);
                let target_change =
                    (weekly.anchor_stock as f64 * target_period_rate).round_ties_even() as i64;
                let required_effect = ((target_change - weekly.change_jc) as f64 / elapsed_days)
                    .round_ties_even() as i64;
                let observed = (weekly.change_jc as f64 / elapsed_days).round_ties_even() as i64;
                (direction, severity, required_effect, observed)
            });

        let volumes =
            self.economy
                .surface_daily_volumes(guild_id, self.config.lookback_days, now)?;
        let mut candidates: Vec<Candidate> = EVENT_CATALOG
            .iter()
            .copied()
            .filter(|template| template.direction == direction)
            .map(|template| {
                let effects = self.effects_for(template, severity, &before);
                let expected = Self::estimate_effect(&effects, &volumes, &before);
                Candidate {
                    distance: required_effect.abs_diff(expected),
                    template,
                    effects,
                    expected,
                }
            })
            .collect();
        candidates.sort_by_key(|candidate| candidate.distance);
        let shortlist_len = candidates.len().min(3);
        let chosen =
            candidates[self.rng.choose_index(guild_id, &event_date, shortlist_len)].clone();
        let (starts_at, ends_at) = self.event_window(&event_date)?;
        let draft = EventDraft {
            event_date: event_date.clone(),
            name: chosen.template.name.into(),
            hero: chosen.template.hero.into(),
            direction: chosen.template.direction,
            severity,
            target_effect_jc: required_effect,
            forecast_flow_jc: observed_daily_flow,
            expected_effect_jc: chosen.expected,
            monetary_stock_before: monetary_stock,
            announcement: Self::announcement_text(
                chosen.template,
                severity,
                &chosen.effects,
                required_effect,
                observed_daily_flow,
            ),
            effects: chosen.effects,
            starts_at,
            ends_at,
            created_at: now,
        };
        let (event, created) = match self.store.claim_event(guild_id, draft)? {
            EventClaim::Claimed(event) => (event, true),
            EventClaim::Existing(event) => (event, false),
        };
        if event.status == EventStatus::Active {
            return Ok((Some(event), false));
        }
        let active = self.finish_claim(guild_id, event, &event_date, now)?;
        Ok((Some(active), created))
    }

    fn finish_claim(
        &self,
        guild_id: GuildId,
        event: EconomyEvent,
        event_date: &str,
        now: i64,
    ) -> Result<EconomyEvent, EconomyEventError> {
        let applied = match self.economy.apply_event_atomic(guild_id, &event) {
            Ok(applied) => applied,
            Err(error) => {
                self.store.mark_event_failed(guild_id, event.event_id)?;
                return Err(error);
            }
        };
        let active = self
            .store
            .activate_event(guild_id, event.event_id, applied)?;
        let after = self.economy.capture_balance_sheet(guild_id)?;
        self.store.save_snapshot(Snapshot {
            guild_id,
            snapshot_date: event_date.to_owned(),
            captured_at: now,
            balance_sheet: after,
        })?;
        Ok(active)
    }

    pub fn mark_event_announced(
        &self,
        guild_id: Option<i64>,
        event_id: i64,
        now: Option<i64>,
    ) -> Result<(), EconomyEventError> {
        self.store.mark_announced(
            GuildId::normalize(guild_id),
            event_id,
            now.unwrap_or_else(|| self.clock.now()),
        )
    }

    pub fn mark_event_reminder_announced(
        &self,
        guild_id: Option<i64>,
        event_id: i64,
        now: Option<i64>,
    ) -> Result<(), EconomyEventError> {
        self.store.mark_reminder_announced(
            GuildId::normalize(guild_id),
            event_id,
            now.unwrap_or_else(|| self.clock.now()),
        )
    }

    #[must_use]
    pub fn pending_announcement_slot(event: &EconomyEvent, now: i64) -> Option<&'static str> {
        if event.announced_at.is_none() {
            return Some("initial");
        }
        if event.reminder_announced_at.is_some() {
            return None;
        }
        let reminder_at = event.starts_at + SECOND_ANNOUNCEMENT_DELAY_SECONDS;
        (reminder_at <= now && now < event.ends_at).then_some("reminder")
    }

    pub fn format_event(event: &EconomyEvent) -> (String, String) {
        const LEVELS: [&str; 5] = ["I", "II", "III", "IV", "V"];
        let level = LEVELS[usize::from(event.severity.clamp(1, 5) - 1)];
        let mut lines = vec![if event.announcement.is_empty() {
            "The economy has shifted.".into()
        } else {
            event.announcement.clone()
        }];
        let mut direct = Vec::new();
        if event.effects.reserve_burn_jc != 0 {
            direct.push(format!(
                "Reserve burned: **{} JC**",
                format_integer(event.effects.reserve_burn_jc)
            ));
        }
        if event.effects.wallet_burn_jc != 0 {
            direct.push(format!(
                "Wallet liquidity burned: **{} JC**",
                format_integer(event.effects.wallet_burn_jc)
            ));
        }
        if event.effects.reserve_release_jc != 0 {
            direct.push(format!(
                "Reserve released: **{} JC**",
                format_integer(event.effects.reserve_release_jc)
            ));
        }
        if !direct.is_empty() {
            lines.push(direct.join("\n"));
        }
        (
            format!("{} — Level {level}", event.name),
            lines.join("\n\n"),
        )
    }

    #[must_use]
    pub fn effects_for(
        &self,
        template: EventTemplate,
        severity: u8,
        balance_sheet: &BalanceSheet,
    ) -> EconomyEventEffects {
        let severity = severity.clamp(1, 5);
        let intensity = SEVERITY_INTENSITY[usize::from(severity - 1)];
        let multiplier =
            |step: f64, low: f64, high: f64| round_to((1.0 + step * intensity).clamp(low, high), 4);
        let available = balance_sheet.reserve_available.max(0);
        let burn_pct = (template.reserve_burn_step * intensity)
            .max(0.0)
            .min(self.config.max_reserve_burn_pct);
        let wallet_burn_rate = (template.wallet_burn_step * intensity)
            .max(0.0)
            .min(self.config.max_wallet_burn_pct);
        let release_pct = (template.reserve_release_step * intensity).max(0.0);
        EconomyEventEffects {
            reward_multiplier: multiplier(template.reward_step, 0.25, 2.0),
            gamba_win_multiplier: multiplier(template.gamba_win_step, 0.25, 2.0),
            gamba_loss_multiplier: multiplier(template.gamba_loss_step, 0.25, 2.0),
            bet_payout_multiplier: multiplier(template.bet_step, 0.5, 1.5),
            prediction_payout_multiplier: multiplier(template.prediction_payout_step, 0.9, 1.1),
            prediction_depth_multiplier: 1.0,
            prediction_spread_ticks_delta: (f64::from(template.spread_step) * intensity)
                .round_ties_even() as i32,
            reserve_burn_jc: (available as f64 * burn_pct) as i64,
            reserve_release_jc: (available as f64 * release_pct) as i64,
            wallet_burn_rate: round_to(wallet_burn_rate, 6),
            wallet_burn_jc: 0,
            reserve_voting_disabled: template.direction == Direction::Deflationary && severity >= 3,
        }
    }

    #[must_use]
    pub fn estimate_effect(
        effects: &EconomyEventEffects,
        volumes: &SurfaceVolumes,
        balance_sheet: &BalanceSheet,
    ) -> i64 {
        let mut expected = 0.0;
        expected += volumes.reward_credits * (effects.reward_multiplier - 1.0);
        expected += volumes.gamba_credits * (effects.gamba_win_multiplier - 1.0);
        expected -= 0.25 * volumes.gamba_debits * (effects.gamba_loss_multiplier - 1.0);
        expected += volumes.bet_payouts * (effects.bet_payout_multiplier - 1.0);
        expected += volumes.prediction_payouts * (effects.prediction_payout_multiplier - 1.0);
        expected -= effects.reserve_burn_jc as f64;
        expected -=
            (balance_sheet.positive_wallets as f64 * effects.wallet_burn_rate) as i64 as f64;
        expected.round_ties_even() as i64
    }

    #[must_use]
    pub fn announcement_text(
        template: EventTemplate,
        severity: u8,
        effects: &EconomyEventEffects,
        required_effect: i64,
        observed_daily_flow: i64,
    ) -> String {
        let mut lines = vec![template.flavor.to_owned()];
        if effects.reserve_burn_jc != 0 {
            lines.push(format!(
                "The uncommitted Jopa Reserve loses **{} JC**.",
                format_integer(effects.reserve_burn_jc)
            ));
        }
        if effects.reserve_release_jc != 0 {
            lines.push(format!(
                "The Jopa Reserve releases **{} JC**.",
                format_integer(effects.reserve_release_jc)
            ));
        }
        if effects.wallet_burn_rate != 0.0 {
            lines.push(format!(
                "Positive wallets lose **{:.2}%**.",
                effects.wallet_burn_rate * 100.0
            ));
        }
        if effects.reward_multiplier != 1.0 {
            lines.push(format!(
                "Generated rewards (dig, trivia, and mana): **{:+.0}%**.",
                (effects.reward_multiplier - 1.0) * 100.0
            ));
        }
        if effects.gamba_win_multiplier != 1.0 || effects.gamba_loss_multiplier != 1.0 {
            lines.push(format!(
                "Gamba wins: **{:+.0}%**; losses: **{:+.0}%**.",
                (effects.gamba_win_multiplier - 1.0) * 100.0,
                (effects.gamba_loss_multiplier - 1.0) * 100.0
            ));
        }
        if effects.bet_payout_multiplier != 1.0 {
            lines.push(format!(
                "Placed-bet payouts resolving today: **{:+.1}%**.",
                (effects.bet_payout_multiplier - 1.0) * 100.0
            ));
        }
        let prediction_payout = (effects.prediction_payout_multiplier - 1.0) * 100.0;
        let mut prediction_parts = Vec::new();
        if prediction_payout != 0.0 {
            prediction_parts.push(format!("resolution **{prediction_payout:+.1}%**"));
        }
        if effects.prediction_spread_ticks_delta != 0 {
            prediction_parts.push(format!(
                "spread **{:+} ticks**",
                effects.prediction_spread_ticks_delta
            ));
        }
        if !prediction_parts.is_empty() {
            lines.push(format!(
                "Prediction markets: {}.",
                prediction_parts.join(", ")
            ));
        }
        lines.push(format!(
            "Observed 7-day stock movement: **{} JC/day**.",
            format_signed_integer(observed_daily_flow)
        ));
        lines.push(format!(
            "Today's target correction: **{} JC**.",
            format_signed_integer(required_effect)
        ));
        if severity == 5 {
            lines.push("**Aghanim's Scepter intensity is active.**".into());
        }
        lines.join("\n")
    }
}

#[derive(Clone)]
struct Candidate {
    distance: u64,
    template: EventTemplate,
    effects: EconomyEventEffects,
    expected: i64,
}

fn parse_event_date(value: &str) -> Result<NaiveDate, EconomyEventError> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| EconomyEventError::InvalidEventDate(value.to_owned()))
}

fn nth_weekday(year: i32, month: u32, weekday: Weekday, occurrence: u32) -> NaiveDate {
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid fixed calendar month");
    let delta = (7 + weekday.num_days_from_sunday() as i64
        - first.weekday().num_days_from_sunday() as i64)
        % 7;
    first + Duration::days(delta + 7 * i64::from(occurrence - 1))
}

fn pacific_offset_for_local(date: NaiveDate, hour: u8) -> i64 {
    let start = nth_weekday(date.year(), 3, Weekday::Sun, 2);
    let end = nth_weekday(date.year(), 11, Weekday::Sun, 1);
    let daylight =
        date > start && date < end || date == start && hour >= 3 || date == end && hour < 2;
    if daylight { -7 * 3_600 } else { -8 * 3_600 }
}

fn pacific_timestamp(date: NaiveDate, hour: u8) -> Result<i64, EconomyEventError> {
    let local = date
        .and_hms_opt(u32::from(hour), 0, 0)
        .ok_or_else(|| EconomyEventError::InvalidEventDate(date.to_string()))?;
    Ok(local.and_utc().timestamp() - pacific_offset_for_local(date, hour))
}

fn pacific_local(timestamp: i64) -> Result<NaiveDateTime, EconomyEventError> {
    let utc = chrono::DateTime::from_timestamp(timestamp, 0)
        .ok_or(EconomyEventError::InvalidTimestamp(timestamp))?;
    let year = utc.year();
    let start_date = nth_weekday(year, 3, Weekday::Sun, 2);
    let end_date = nth_weekday(year, 11, Weekday::Sun, 1);
    let start_utc = start_date
        .and_hms_opt(10, 0, 0)
        .expect("valid transition hour")
        .and_utc()
        .timestamp();
    let end_utc = end_date
        .and_hms_opt(9, 0, 0)
        .expect("valid transition hour")
        .and_utc()
        .timestamp();
    let offset = if (start_utc..end_utc).contains(&timestamp) {
        -7 * 3_600
    } else {
        -8 * 3_600
    };
    chrono::DateTime::from_timestamp(timestamp + offset, 0)
        .map(|local| local.naive_utc())
        .ok_or(EconomyEventError::InvalidTimestamp(timestamp))
}

fn round_to(value: f64, decimals: u32) -> f64 {
    let scale = 10_f64.powi(i32::try_from(decimals).unwrap_or(i32::MAX));
    (value * scale).round_ties_even() / scale
}

fn format_integer(value: i64) -> String {
    let negative = value < 0;
    let digits = value.unsigned_abs().to_string();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            result.push(',');
        }
        result.push(character);
    }
    if negative {
        format!("-{result}")
    } else {
        result
    }
}

fn format_signed_integer(value: i64) -> String {
    if value >= 0 {
        format!("+{}", format_integer(value))
    } else {
        format_integer(value)
    }
}

#[cfg(test)]
mod tests;
