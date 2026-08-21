//! Guild-scoped player-trivia generation and short-lived snapshot caching.
//!
//! The Python service reads one heterogeneous repository snapshot, freezes it,
//! and gives every caller an isolated copy. This additive Rust service keeps
//! the same boundary with typed rows: repository adapters own SQL decoding,
//! while question generation remains deterministic and Discord-neutral.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use cama_db::predictions_repository::CONTRACT_VALUE;
use cama_domain::player::{OPENSKILL_DISPLAY_SCALE, OPENSKILL_MIN_MU};
use cama_domain::rating::CALIBRATION_RD_THRESHOLD;
use thiserror::Error;

pub type DiscordId = i64;
pub type GuildId = i64;

pub const PLAYER_TRIVIA_SNAPSHOT_CACHE_TTL_SECONDS: f64 = 45.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlayerTriviaPlayer {
    pub discord_id: DiscordId,
    pub username: String,
    pub wins: i64,
    pub losses: i64,
    pub balance: i64,
    pub lowest_balance_ever: Option<i64>,
    pub glicko_rating: Option<f64>,
    pub glicko_rd: Option<f64>,
    pub openskill_mu: Option<f64>,
    pub openskill_sigma: Option<f64>,
    pub last_match_unix: Option<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TipRow {
    pub sender_id: DiscordId,
    pub recipient_id: DiscordId,
    pub amount: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WheelSpinRow {
    pub spin_id: i64,
    pub discord_id: DiscordId,
    pub spin_time: i64,
    pub outcome_code: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParticipantRow {
    pub discord_id: DiscordId,
    pub match_id: i64,
    pub match_time: i64,
    pub hero_id: i64,
    pub won: bool,
    pub kills: Option<f64>,
    pub assists: Option<f64>,
    pub gpm: Option<f64>,
    pub hero_damage: Option<f64>,
    pub tower_damage: Option<f64>,
    pub fantasy_points: Option<f64>,
    pub lane_role: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RatingRow {
    pub discord_id: DiscordId,
    pub rating: Option<f64>,
    pub rating_before: Option<f64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PairingRow {
    pub player1_id: DiscordId,
    pub player2_id: DiscordId,
    pub games_together: i64,
    pub wins_together: i64,
    pub games_against: i64,
    pub player1_wins_against: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DoubleSpinRow {
    pub discord_id: DiscordId,
    pub won: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TriviaHistoryRow {
    pub discord_id: DiscordId,
    pub streak: i64,
    pub jc_earned: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TunnelRow {
    pub discord_id: DiscordId,
    pub total_digs: i64,
    pub max_depth: i64,
    pub total_jc_earned: i64,
    pub prestige_level: i64,
    pub best_run_score: i64,
    pub pickaxe_tier: i64,
    pub stat_strength: i64,
    pub stat_smarts: i64,
    pub stat_stamina: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BankruptcyRow {
    pub discord_id: DiscordId,
    pub bankruptcy_count: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DisbursementVoteRow {
    pub proposal_id: i64,
    pub discord_id: DiscordId,
    pub vote_method: String,
    pub proposal_outcome: String,
    pub finalized_at: Option<i64>,
}

/// Public prediction facts deliberately omit creator and metadata columns.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PredictionPositionRow {
    pub prediction_id: i64,
    pub discord_id: DiscordId,
    pub status: String,
    pub outcome: String,
    pub yes_contracts: i64,
    pub yes_cost_basis_total: i64,
    pub no_contracts: i64,
    pub no_cost_basis_total: i64,
    pub bankruptcy_penalty: i64,
    pub vanity_tax: i64,
    pub low_priority_tax: i64,
    pub question: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PredictionTradeRow {
    pub prediction_id: i64,
    pub discord_id: DiscordId,
    pub contracts: i64,
    pub status: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SettledBetRow {
    pub bet_id: i64,
    pub discord_id: DiscordId,
    pub match_id: i64,
    pub team_bet_on: String,
    pub winning_team: i64,
    pub amount: i64,
    pub leverage: i64,
    pub effective_bet: Option<i64>,
    pub payout: i64,
    pub settlement_vanity_tax: i64,
    pub settlement_low_priority_tax: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MafiaGameRow {
    pub game_id: i64,
    pub phase: String,
    pub status: String,
    pub winner: Option<String>,
    pub mvp_id: Option<DiscordId>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MafiaPlayerRow {
    pub game_id: i64,
    pub discord_id: DiscordId,
    pub role: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigArtifactRow {
    pub discord_id: DiscordId,
    pub is_relic: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigAchievementRow {
    pub discord_id: DiscordId,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigActionRow {
    pub actor_id: DiscordId,
    pub action_type: String,
    pub depth_before: Option<i64>,
    pub depth_after: Option<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MafiaActionRow {
    pub game_id: i64,
    pub actor_id: DiscordId,
    pub action_type: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecalibrationRow {
    pub discord_id: DiscordId,
    pub total_recalibrations: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlayerTriviaSnapshot {
    pub players: Vec<PlayerTriviaPlayer>,
    pub participants: Vec<ParticipantRow>,
    pub ratings: Vec<RatingRow>,
    pub pairings: Vec<PairingRow>,
    pub tips: Vec<TipRow>,
    pub wheel_spins: Vec<WheelSpinRow>,
    pub double_spins: Vec<DoubleSpinRow>,
    pub trivia_sessions: Vec<TriviaHistoryRow>,
    pub tunnels: Vec<TunnelRow>,
    pub bankruptcies: Vec<BankruptcyRow>,
    pub disbursement_votes: Vec<DisbursementVoteRow>,
    pub prediction_positions: Vec<PredictionPositionRow>,
    pub prediction_trades: Vec<PredictionTradeRow>,
    pub bets: Vec<SettledBetRow>,
    pub dig_artifacts: Vec<DigArtifactRow>,
    pub dig_achievements: Vec<DigAchievementRow>,
    pub dig_actions: Vec<DigActionRow>,
    pub mafia_games: Vec<MafiaGameRow>,
    pub mafia_players: Vec<MafiaPlayerRow>,
    pub mafia_actions: Vec<MafiaActionRow>,
    pub recalibrations: Vec<RecalibrationRow>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerTriviaQuestionRecord {
    pub question_key: String,
    pub category: String,
    pub question_text: String,
    pub options: [String; 4],
    pub correct_index: usize,
    pub explanation: String,
    pub spicy: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerTriviaQuestion {
    pub key: String,
    pub category: String,
    pub text: String,
    pub options: [String; 4],
    pub correct_index: usize,
    pub explanation: String,
    pub spicy: bool,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum PlayerTriviaQuestionError {
    #[error("player-trivia options must be distinct")]
    DuplicateOptions,
    #[error("correct_index must identify one of the four options")]
    InvalidCorrectIndex,
}

impl PlayerTriviaQuestion {
    pub fn new(
        key: impl Into<String>,
        category: impl Into<String>,
        text: impl Into<String>,
        options: [String; 4],
        correct_index: usize,
        explanation: impl Into<String>,
        spicy: bool,
    ) -> Result<Self, PlayerTriviaQuestionError> {
        if correct_index >= options.len() {
            return Err(PlayerTriviaQuestionError::InvalidCorrectIndex);
        }
        let distinct = options
            .iter()
            .map(|option| option.to_lowercase())
            .collect::<BTreeSet<_>>();
        if distinct.len() != options.len() {
            return Err(PlayerTriviaQuestionError::DuplicateOptions);
        }
        Ok(Self {
            key: key.into(),
            category: category.into(),
            text: text.into(),
            options,
            correct_index,
            explanation: explanation.into(),
            spicy,
        })
    }

    #[must_use]
    pub fn correct_answer(&self) -> &str {
        &self.options[self.correct_index]
    }

    #[must_use]
    pub fn to_record(&self) -> PlayerTriviaQuestionRecord {
        PlayerTriviaQuestionRecord {
            question_key: self.key.clone(),
            category: self.category.clone(),
            question_text: self.text.clone(),
            options: self.options.clone(),
            correct_index: self.correct_index,
            explanation: self.explanation.clone(),
            spicy: self.spicy,
        }
    }
}

pub trait PlayerTriviaSnapshotRepository: Send + Sync {
    fn load_snapshot(&self, guild_id: GuildId) -> Result<PlayerTriviaSnapshot, String>;

    fn recent_question_keys(
        &self,
        discord_id: DiscordId,
        guild_id: GuildId,
        since: i64,
    ) -> Result<Vec<String>, String>;
}

pub trait PlayerTriviaSessionRepository: Send + Sync {
    type StartResult;
    type AnswerResult;
    type FinishResult;
    type CancelResult;

    fn try_start_session(
        &self,
        discord_id: DiscordId,
        guild_id: GuildId,
        questions: &[PlayerTriviaQuestionRecord],
        now: i64,
        cooldown_seconds: i64,
        bypass: bool,
    ) -> Result<Self::StartResult, String>;

    fn settle_answer(
        &self,
        session_id: i64,
        question_number: i64,
        selected_index: usize,
        reward: i64,
        answered_at: i64,
    ) -> Result<Self::AnswerResult, String>;

    fn finish_session(
        &self,
        session_id: i64,
        status: &str,
        completed_at: i64,
    ) -> Result<Self::FinishResult, String>;

    fn cancel_session_if_unanswered(&self, session_id: i64) -> Result<Self::CancelResult, String>;
}

pub trait PlayerTriviaRandom: Send {
    fn next_index(&mut self, upper: usize) -> usize;

    fn shuffle_enabled(&self) -> bool {
        true
    }
}

#[derive(Clone, Debug)]
pub struct SeededTriviaRandom {
    state: u64,
}

impl SeededTriviaRandom {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }
}

impl Default for SeededTriviaRandom {
    fn default() -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos() as u64);
        Self::new(seed)
    }
}

impl PlayerTriviaRandom for SeededTriviaRandom {
    fn next_index(&mut self, upper: usize) -> usize {
        debug_assert!(upper > 0);
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        // The low bits of a power-of-two LCG are highly regular -- this
        // multiplier and increment make the parity alternate on every call --
        // so taking `state % upper` would leave the shuffled answer positions
        // patterned and partly predictable. Scale the high bits instead.
        let scaled = (u128::from(self.state) * upper as u128) >> 64;
        scaled as usize
    }
}

pub trait MonotonicClock: Send + Sync {
    fn now(&self) -> f64;
}

#[derive(Debug)]
pub struct SystemMonotonicClock {
    origin: Instant,
}

impl Default for SystemMonotonicClock {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl MonotonicClock for SystemMonotonicClock {
    fn now(&self) -> f64 {
        self.origin.elapsed().as_secs_f64()
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum PlayerTriviaServiceError {
    #[error("player-trivia repository failed: {0}")]
    Repository(String),
    #[error("player-trivia cache lock was poisoned")]
    CachePoisoned,
    #[error("player-trivia random source lock was poisoned")]
    RandomPoisoned,
    #[error("player-trivia snapshot load completed without a result")]
    MissingSnapshotResult,
}

#[derive(Clone)]
struct CacheEntry {
    snapshot: Arc<PlayerTriviaSnapshot>,
    expires_at: f64,
}

#[derive(Default)]
struct CacheState {
    snapshots: BTreeMap<GuildId, CacheEntry>,
    loads: BTreeMap<GuildId, Arc<SnapshotLoad>>,
}

#[derive(Default)]
struct SnapshotLoad {
    result: Mutex<Option<Result<Arc<PlayerTriviaSnapshot>, String>>>,
    complete: Condvar,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Candidate {
    question: PlayerTriviaQuestion,
    identity: String,
}

pub struct PlayerTriviaService<R, G = SeededTriviaRandom, C = SystemMonotonicClock> {
    repository: Arc<R>,
    random: Mutex<G>,
    clock: Arc<C>,
    snapshot_cache_ttl_seconds: f64,
    cache: Mutex<CacheState>,
}

impl<R> PlayerTriviaService<R> {
    #[must_use]
    pub fn new(repository: Arc<R>) -> Self {
        Self::with_dependencies(
            repository,
            SeededTriviaRandom::default(),
            Arc::new(SystemMonotonicClock::default()),
            PLAYER_TRIVIA_SNAPSHOT_CACHE_TTL_SECONDS,
        )
    }
}

impl<R, G, C> PlayerTriviaService<R, G, C> {
    #[must_use]
    pub fn with_dependencies(
        repository: Arc<R>,
        random: G,
        clock: Arc<C>,
        snapshot_cache_ttl_seconds: f64,
    ) -> Self {
        Self {
            repository,
            random: Mutex::new(random),
            clock,
            snapshot_cache_ttl_seconds: snapshot_cache_ttl_seconds.max(0.0),
            cache: Mutex::new(CacheState::default()),
        }
    }

    #[must_use]
    pub fn repository(&self) -> &R {
        &self.repository
    }
}

impl<R, G, C> PlayerTriviaService<R, G, C>
where
    R: PlayerTriviaSessionRepository,
{
    pub fn try_start_session(
        &self,
        discord_id: DiscordId,
        guild_id: GuildId,
        questions: &[PlayerTriviaQuestion],
        now: i64,
        cooldown_seconds: i64,
        bypass: bool,
    ) -> Result<R::StartResult, String> {
        let records = questions
            .iter()
            .map(PlayerTriviaQuestion::to_record)
            .collect::<Vec<_>>();
        self.repository.try_start_session(
            discord_id,
            guild_id,
            &records,
            now,
            cooldown_seconds,
            bypass,
        )
    }

    pub fn settle_answer(
        &self,
        session_id: i64,
        question_number: i64,
        selected_index: usize,
        reward: i64,
        answered_at: i64,
    ) -> Result<R::AnswerResult, String> {
        self.repository.settle_answer(
            session_id,
            question_number,
            selected_index,
            reward,
            answered_at,
        )
    }

    pub fn finish_session(
        &self,
        session_id: i64,
        status: &str,
        completed_at: i64,
    ) -> Result<R::FinishResult, String> {
        self.repository
            .finish_session(session_id, status, completed_at)
    }

    pub fn cancel_session_if_unanswered(&self, session_id: i64) -> Result<R::CancelResult, String> {
        self.repository.cancel_session_if_unanswered(session_id)
    }
}

impl<R, G, C> PlayerTriviaService<R, G, C>
where
    R: PlayerTriviaSnapshotRepository,
    G: PlayerTriviaRandom,
    C: MonotonicClock,
{
    fn wait_for_snapshot(
        load: &SnapshotLoad,
    ) -> Result<Arc<PlayerTriviaSnapshot>, PlayerTriviaServiceError> {
        let mut result = load
            .result
            .lock()
            .map_err(|_| PlayerTriviaServiceError::CachePoisoned)?;
        while result.is_none() {
            result = load
                .complete
                .wait(result)
                .map_err(|_| PlayerTriviaServiceError::CachePoisoned)?;
        }
        result
            .as_ref()
            .ok_or(PlayerTriviaServiceError::MissingSnapshotResult)?
            .clone()
            .map_err(PlayerTriviaServiceError::Repository)
    }

    pub fn raw_snapshot(
        &self,
        guild_id: GuildId,
    ) -> Result<Arc<PlayerTriviaSnapshot>, PlayerTriviaServiceError> {
        let now = self.clock.now();
        let (load, should_load) = {
            let mut cache = self
                .cache
                .lock()
                .map_err(|_| PlayerTriviaServiceError::CachePoisoned)?;
            cache.snapshots.retain(|_, entry| now < entry.expires_at);
            if let Some(entry) = cache.snapshots.get(&guild_id) {
                return Ok(Arc::clone(&entry.snapshot));
            }
            if let Some(load) = cache.loads.get(&guild_id) {
                (Arc::clone(load), false)
            } else {
                let load = Arc::new(SnapshotLoad::default());
                cache.loads.insert(guild_id, Arc::clone(&load));
                (load, true)
            }
        };

        if !should_load {
            return Self::wait_for_snapshot(&load);
        }

        let loaded = self.repository.load_snapshot(guild_id).map(Arc::new);
        let expires_at = self.clock.now() + self.snapshot_cache_ttl_seconds;
        {
            let mut cache = self
                .cache
                .lock()
                .map_err(|_| PlayerTriviaServiceError::CachePoisoned)?;
            if let Ok(snapshot) = &loaded {
                cache.snapshots.insert(
                    guild_id,
                    CacheEntry {
                        snapshot: Arc::clone(snapshot),
                        expires_at,
                    },
                );
            }
            cache.loads.remove(&guild_id);
        }
        {
            let mut result = load
                .result
                .lock()
                .map_err(|_| PlayerTriviaServiceError::CachePoisoned)?;
            *result = Some(loaded.clone());
            load.complete.notify_all();
        }
        loaded.map_err(PlayerTriviaServiceError::Repository)
    }

    pub fn snapshot_for_generation(
        &self,
        guild_id: GuildId,
    ) -> Result<PlayerTriviaSnapshot, PlayerTriviaServiceError> {
        self.raw_snapshot(guild_id)
            .map(|snapshot| snapshot.as_ref().clone())
    }

    pub fn cached_guild_ids(&self) -> Result<BTreeSet<GuildId>, PlayerTriviaServiceError> {
        let cache = self
            .cache
            .lock()
            .map_err(|_| PlayerTriviaServiceError::CachePoisoned)?;
        Ok(cache.snapshots.keys().copied().collect())
    }

    pub fn generate_questions(
        &self,
        user_id: DiscordId,
        guild_id: GuildId,
        current_member_ids: Option<&BTreeSet<DiscordId>>,
        count: usize,
        include_spicy: bool,
        recent_days: i64,
    ) -> Result<Vec<PlayerTriviaQuestion>, PlayerTriviaServiceError> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let snapshot = self.snapshot_for_generation(guild_id)?;
        let players = snapshot
            .players
            .iter()
            .filter(|player| {
                player.discord_id > 0
                    && current_member_ids.is_none_or(|allowed| allowed.contains(&player.discord_id))
            })
            .map(|player| (player.discord_id, player))
            .collect::<BTreeMap<_, _>>();
        if players.len() < 4 {
            return Ok(Vec::new());
        }

        let since = unix_now().saturating_sub(recent_days.max(0).saturating_mul(86_400));
        let recent = self
            .repository
            .recent_question_keys(user_id, guild_id, since)
            .map_err(PlayerTriviaServiceError::Repository)?
            .into_iter()
            .collect::<BTreeSet<_>>();

        let mut random = self
            .random
            .lock()
            .map_err(|_| PlayerTriviaServiceError::RandomPoisoned)?;
        let mut candidates = Vec::new();
        build_match_questions(&mut candidates, &snapshot, &players, &mut *random);
        build_rating_questions(&mut candidates, &snapshot, &players, &mut *random);
        build_hero_questions(&mut candidates, &snapshot, &players, &mut *random);
        build_pairing_questions(&mut candidates, &snapshot, &players, &mut *random);
        build_economy_and_tip_questions(&mut candidates, &snapshot, &players, &mut *random);
        build_betting_questions(&mut candidates, &snapshot, &players, &mut *random);
        build_wheel_questions(&mut candidates, &snapshot, &players, &mut *random);
        build_double_questions(&mut candidates, &snapshot, &players, &mut *random);
        build_prediction_questions(&mut candidates, &snapshot, &players, &mut *random);
        build_dig_questions(&mut candidates, &snapshot, &players, &mut *random);
        build_mafia_questions(&mut candidates, &snapshot, &players, &mut *random);
        build_trivia_history_questions(&mut candidates, &snapshot, &players, &mut *random);
        build_disbursement_questions(&mut candidates, &snapshot, &players, &mut *random);

        if random.shuffle_enabled() {
            shuffle(&mut candidates, &mut *random);
        }
        let mut selected = Vec::new();
        let mut category_counts = BTreeMap::<String, usize>::new();
        let mut identity_counts = BTreeMap::<String, usize>::new();
        let mut keys = BTreeSet::new();
        for candidate in candidates {
            let question = candidate.question;
            if recent.contains(&question.key)
                || (!include_spicy && question.spicy)
                || keys.contains(&question.key)
                || category_counts
                    .get(&question.category)
                    .copied()
                    .unwrap_or(0)
                    >= 2
                || identity_counts
                    .get(&candidate.identity)
                    .copied()
                    .unwrap_or(0)
                    >= 2
            {
                continue;
            }
            keys.insert(question.key.clone());
            *category_counts
                .entry(question.category.clone())
                .or_default() += 1;
            *identity_counts.entry(candidate.identity).or_default() += 1;
            selected.push(question);
            if selected.len() == count {
                break;
            }
        }
        Ok(selected)
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

fn player_label(player_id: DiscordId) -> String {
    format!("<@{player_id}>")
}

fn humanize_code(value: &str) -> String {
    value
        .trim()
        .replace(['_', '-'], " ")
        .split_whitespace()
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                format!("{}{}", first.to_uppercase(), chars.as_str().to_lowercase())
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn hero_name(hero_id: i64) -> String {
    static HERO_NAMES: std::sync::OnceLock<BTreeMap<i64, String>> = std::sync::OnceLock::new();
    HERO_NAMES
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
        .get(&hero_id)
        .cloned()
        .unwrap_or_else(|| format!("Hero {hero_id}"))
}

const fn lane_name(lane: i64) -> Option<&'static str> {
    match lane {
        0 => Some("Roaming"),
        1 => Some("Safe Lane"),
        2 => Some("Mid Lane"),
        3 => Some("Off Lane"),
        4 => Some("Jungle"),
        _ => None,
    }
}

fn safe_market_descriptor(value: &str) -> String {
    value
        .replace('@', "＠")
        .replace(['\\', '*', '_', '`', '~', '|', '>', '[', ']'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(190)
        .collect()
}

fn shuffle<T>(values: &mut [T], random: &mut impl PlayerTriviaRandom) {
    for upper in (2..=values.len()).rev() {
        values.swap(upper - 1, random.next_index(upper));
    }
}

fn four_options(
    correct: String,
    distractors: impl IntoIterator<Item = String>,
    random: &mut impl PlayerTriviaRandom,
) -> Option<([String; 4], usize)> {
    if correct.is_empty() {
        return None;
    }
    let correct_folded = correct.to_lowercase();
    let mut unique = BTreeMap::<String, String>::new();
    for distractor in distractors {
        let folded = distractor.to_lowercase();
        if !distractor.is_empty() && folded != correct_folded {
            unique.entry(folded).or_insert(distractor);
        }
    }
    if unique.len() < 3 {
        return None;
    }
    let mut pool = unique.into_values().collect::<Vec<_>>();
    let mut choices = Vec::with_capacity(4);
    for _ in 0..3 {
        choices.push(pool.remove(random.next_index(pool.len())));
    }
    choices.push(correct.clone());
    if random.shuffle_enabled() {
        shuffle(&mut choices, random);
    }
    let correct_index = choices.iter().position(|choice| choice == &correct)?;
    let options = choices.try_into().ok()?;
    Some((options, correct_index))
}

#[expect(
    clippy::too_many_arguments,
    reason = "centralizes the immutable player-leader question contract"
)]
fn add_player_leader(
    candidates: &mut Vec<Candidate>,
    players: &BTreeMap<DiscordId, &PlayerTriviaPlayer>,
    key: &str,
    category: &str,
    text: &str,
    values: BTreeMap<DiscordId, f64>,
    minimum: Option<f64>,
    lowest: bool,
    display: impl Fn(f64) -> f64,
    explanation: impl Fn(&str, f64) -> String,
    spicy: bool,
    random: &mut impl PlayerTriviaRandom,
) {
    let mut usable = values
        .into_iter()
        .filter(|(player_id, value)| players.contains_key(player_id) && value.is_finite())
        .collect::<Vec<_>>();
    if usable.len() < 4 {
        return;
    }
    usable.sort_by(|left, right| {
        let ordering = left.1.total_cmp(&right.1).then(left.0.cmp(&right.0));
        if lowest { ordering } else { ordering.reverse() }
    });
    let (winner_id, winner_value) = usable[0];
    if minimum.is_some_and(|threshold| {
        if lowest {
            winner_value > threshold
        } else {
            winner_value < threshold
        }
    }) || winner_value == usable[1].1
        || display(winner_value) == display(usable[1].1)
    {
        return;
    }
    let correct = player_label(winner_id);
    let distractors = usable
        .iter()
        .skip(1)
        .map(|(player_id, _)| player_label(*player_id));
    let Some((options, correct_index)) = four_options(correct, distractors, random) else {
        return;
    };
    let label = player_label(winner_id);
    let question = PlayerTriviaQuestion::new(
        key,
        category,
        text,
        options,
        correct_index,
        explanation(&label, winner_value),
        spicy,
    )
    .expect("four_options always produces a structurally valid question");
    candidates.push(Candidate {
        question,
        identity: format!("player:{winner_id}"),
    });
}

#[expect(
    clippy::too_many_arguments,
    reason = "centralizes the immutable value-question contract"
)]
fn add_value_question(
    candidates: &mut Vec<Candidate>,
    key: String,
    category: &str,
    text: String,
    correct: String,
    distractors: impl IntoIterator<Item = String>,
    explanation: String,
    identity: String,
    spicy: bool,
    random: &mut impl PlayerTriviaRandom,
) {
    let Some((options, correct_index)) = four_options(correct, distractors, random) else {
        return;
    };
    let question = PlayerTriviaQuestion::new(
        key,
        category,
        text,
        options,
        correct_index,
        explanation,
        spicy,
    )
    .expect("four_options always produces a structurally valid question");
    candidates.push(Candidate { question, identity });
}

fn build_match_questions(
    candidates: &mut Vec<Candidate>,
    snapshot: &PlayerTriviaSnapshot,
    players: &BTreeMap<DiscordId, &PlayerTriviaPlayer>,
    random: &mut impl PlayerTriviaRandom,
) {
    let games = players
        .iter()
        .map(|(player_id, player)| (*player_id, player.wins.saturating_add(player.losses) as f64))
        .collect::<BTreeMap<_, _>>();
    let wins = players
        .iter()
        .map(|(player_id, player)| (*player_id, player.wins.max(0) as f64))
        .collect::<BTreeMap<_, _>>();
    let losses = players
        .iter()
        .map(|(player_id, player)| (*player_id, player.losses.max(0) as f64))
        .collect::<BTreeMap<_, _>>();
    let rates = players
        .iter()
        .filter_map(|(player_id, player)| {
            let total = player.wins.saturating_add(player.losses);
            (total >= 10).then_some((*player_id, player.wins as f64 / total as f64))
        })
        .collect::<BTreeMap<_, _>>();
    add_player_leader(
        candidates,
        players,
        "matches:most_games",
        "matches",
        "Who has played the most recorded inhouse matches?",
        games,
        Some(3.0),
        false,
        |value| value,
        |name, value| format!("As of game start, {name} had played {value:.0} recorded matches."),
        false,
        random,
    );
    let mut rows_by_player = BTreeMap::<DiscordId, Vec<&ParticipantRow>>::new();
    for row in &snapshot.participants {
        if players.contains_key(&row.discord_id) {
            rows_by_player.entry(row.discord_id).or_default().push(row);
        }
    }
    let win_streaks = rows_by_player
        .into_iter()
        .filter_map(|(player_id, mut rows)| {
            rows.sort_by_key(|row| (row.match_time, row.match_id));
            if !rows.last().is_some_and(|row| row.won) {
                return None;
            }
            let streak = rows.iter().rev().take_while(|row| row.won).count();
            Some((player_id, streak as f64))
        })
        .collect();
    add_player_leader(
        candidates,
        players,
        "matches:longest_current_win_streak",
        "matches",
        "Who is riding the longest current recorded-match win streak?",
        win_streaks,
        Some(2.0),
        false,
        |value| value,
        |name, value| format!("As of game start, {name} was on a {value:.0}-match win streak."),
        false,
        random,
    );
    add_player_leader(
        candidates,
        players,
        "matches:most_wins",
        "matches",
        "Who has the most recorded match wins?",
        wins,
        Some(2.0),
        false,
        |value| value,
        |name, value| format!("As of game start, {name} had {value:.0} recorded wins."),
        false,
        random,
    );
    add_player_leader(
        candidates,
        players,
        "matches:highest_win_rate",
        "matches",
        "Among players with at least 10 games, who has the highest match win rate?",
        rates.clone(),
        None,
        false,
        |value| cama_domain::formatting::python_round_decimals(value * 100.0, 1),
        |name, value| {
            format!(
                "As of game start, {name}'s win rate was {:.1}%.",
                value * 100.0
            )
        },
        false,
        random,
    );
    add_player_leader(
        candidates,
        players,
        "matches:lowest_win_rate",
        "matches",
        "Among players with at least 10 games, who has the lowest match win rate?",
        rates,
        None,
        true,
        |value| cama_domain::formatting::python_round_decimals(value * 100.0, 1),
        |name, value| {
            format!(
                "As of game start, {name}'s win rate was {:.1}%.",
                value * 100.0
            )
        },
        true,
        random,
    );
    add_player_leader(
        candidates,
        players,
        "matches:most_losses",
        "matches",
        "Who has the most recorded match losses?",
        losses,
        Some(2.0),
        false,
        |value| value,
        |name, value| format!("As of game start, {name} had {value:.0} recorded losses."),
        true,
        random,
    );

    let records = players
        .iter()
        .filter(|(_, player)| player.wins.saturating_add(player.losses) > 0)
        .map(|(player_id, player)| {
            (
                *player_id,
                format!("{}W–{}L", player.wins.max(0), player.losses.max(0)),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for (player_id, correct) in &records {
        add_value_question(
            candidates,
            format!("matches:record:{player_id}"),
            "matches",
            format!(
                "What is {}'s recorded match record?",
                player_label(*player_id)
            ),
            correct.clone(),
            records
                .iter()
                .filter(|(other_id, _)| *other_id != player_id)
                .map(|(_, value)| value.clone()),
            format!(
                "As of game start, {} was {correct}.",
                player_label(*player_id)
            ),
            format!("player:{player_id}"),
            false,
            random,
        );
    }
}

fn build_rating_questions(
    candidates: &mut Vec<Candidate>,
    snapshot: &PlayerTriviaSnapshot,
    players: &BTreeMap<DiscordId, &PlayerTriviaPlayer>,
    random: &mut impl PlayerTriviaRandom,
) {
    let mut glicko = BTreeMap::new();
    let mut openskill = BTreeMap::new();
    for (player_id, player) in players {
        let games = player.wins.saturating_add(player.losses);
        let (Some(rating), Some(rd), Some(mu), Some(sigma)) = (
            player.glicko_rating,
            player.glicko_rd,
            player.openskill_mu,
            player.openskill_sigma,
        ) else {
            continue;
        };
        if games < 10
            || player
                .last_match_unix
                .is_some_and(|timestamp| timestamp < unix_now().saturating_sub(180 * 86_400))
            || rd > CALIBRATION_RD_THRESHOLD
            || sigma > 4.0
        {
            continue;
        }
        glicko.insert(*player_id, rating.round_ties_even().clamp(0.0, 3_000.0));
        openskill.insert(
            *player_id,
            ((mu - OPENSKILL_MIN_MU) * OPENSKILL_DISPLAY_SCALE)
                .round_ties_even()
                .clamp(0.0, 3_000.0),
        );
    }
    if glicko.len() < 4 {
        return;
    }
    add_player_leader(
        candidates,
        players,
        "ratings:glicko_leader",
        "ratings",
        "Among active calibrated players, who has the highest Glicko rating?",
        glicko.clone(),
        None,
        false,
        f64::round,
        |name, value| format!("As of game start, {name}'s Glicko display rating was {value:.0}."),
        false,
        random,
    );
    add_player_leader(
        candidates,
        players,
        "ratings:openskill_leader",
        "ratings",
        "Among active calibrated players, who has the highest OpenSkill display rating?",
        openskill.clone(),
        None,
        false,
        f64::round,
        |name, value| {
            format!("As of game start, {name}'s OpenSkill display rating was {value:.0}.")
        },
        false,
        random,
    );
    let gaps = glicko
        .iter()
        .map(|(player_id, value)| (*player_id, (value - openskill[player_id]).abs()))
        .collect::<BTreeMap<_, _>>();
    add_player_leader(
        candidates,
        players,
        "ratings:largest_display_gap",
        "ratings",
        "Whose Glicko and OpenSkill display ratings disagree by the most points?",
        gaps.clone(),
        Some(1.0),
        false,
        f64::round,
        |name, value| {
            format!("As of game start, {name}'s two 0–3000 ratings were {value:.0} points apart.")
        },
        false,
        random,
    );
    add_player_leader(
        candidates,
        players,
        "ratings:smallest_display_gap",
        "ratings",
        "Whose Glicko and OpenSkill display ratings are closest together?",
        gaps,
        None,
        true,
        f64::round,
        |name, value| {
            format!(
                "As of game start, {name}'s two 0–3000 ratings were only {value:.0} points apart."
            )
        },
        false,
        random,
    );

    let ranked = |values: &BTreeMap<DiscordId, f64>| {
        let mut ordered = values.iter().collect::<Vec<_>>();
        ordered.sort_by(|left, right| right.1.total_cmp(left.1).then(left.0.cmp(right.0)));
        ordered
            .into_iter()
            .enumerate()
            .map(|(index, (player_id, _))| (*player_id, index + 1))
            .collect::<BTreeMap<_, _>>()
    };
    let glicko_rank = ranked(&glicko);
    let openskill_rank = ranked(&openskill);
    let rank_gaps = glicko_rank
        .iter()
        .map(|(player_id, rank)| (*player_id, rank.abs_diff(openskill_rank[player_id]) as f64))
        .collect::<BTreeMap<_, _>>();
    add_player_leader(
        candidates,
        players,
        "ratings:largest_rank_gap",
        "ratings",
        "Whose Glicko rank and OpenSkill rank disagree by the most places?",
        rank_gaps.clone(),
        Some(1.0),
        false,
        |value| value,
        |name, value| {
            format!("As of game start, {name}'s two rating ranks were {value:.0} places apart.")
        },
        false,
        random,
    );
    add_player_leader(
        candidates,
        players,
        "ratings:smallest_rank_gap",
        "ratings",
        "Whose Glicko rank and OpenSkill rank are closest?",
        rank_gaps,
        None,
        true,
        |value| value,
        |name, value| {
            format!("As of game start, {name}'s two rating ranks were {value:.0} places apart.")
        },
        false,
        random,
    );

    let rank_options = (1..=glicko.len())
        .map(|rank| format!("#{rank}"))
        .collect::<Vec<_>>();
    for (player_id, rank) in &glicko_rank {
        let correct = format!("#{rank}");
        add_value_question(
            candidates,
            format!("ratings:glicko_rank:{player_id}"),
            "ratings",
            format!(
                "What is {}'s Glicko rank among active calibrated players?",
                player_label(*player_id)
            ),
            correct.clone(),
            rank_options
                .iter()
                .filter(|value| *value != &correct)
                .cloned(),
            format!(
                "As of game start, {} ranked {correct} by Glicko.",
                player_label(*player_id)
            ),
            format!("player:{player_id}"),
            false,
            random,
        );
    }

    add_player_leader(
        candidates,
        players,
        "ratings:most_recalibrations",
        "ratings",
        "Who has completed the most recorded rating recalibrations?",
        snapshot
            .recalibrations
            .iter()
            .map(|row| (row.discord_id, row.total_recalibrations as f64))
            .collect(),
        Some(1.0),
        false,
        |value| value,
        |name, value| format!("As of game start, {name} had completed {value:.0} recalibrations."),
        false,
        random,
    );
    let mut largest_gain = BTreeMap::<DiscordId, f64>::new();
    for row in &snapshot.ratings {
        let (Some(after), Some(before)) = (row.rating, row.rating_before) else {
            continue;
        };
        if !players.contains_key(&row.discord_id) {
            continue;
        }
        let gain = after - before;
        largest_gain
            .entry(row.discord_id)
            .and_modify(|current| *current = current.max(gain))
            .or_insert(gain);
    }
    add_player_leader(
        candidates,
        players,
        "ratings:largest_single_gain",
        "ratings",
        "Who has the largest single-match Glicko rating gain on record?",
        largest_gain,
        Some(0.1),
        false,
        |value| cama_domain::formatting::python_round_decimals(value, 1),
        |name, value| {
            format!(
                "As of game start, {name}'s largest one-match Glicko gain was {value:.1} points."
            )
        },
        false,
        random,
    );
}

fn build_hero_questions(
    candidates: &mut Vec<Candidate>,
    snapshot: &PlayerTriviaSnapshot,
    players: &BTreeMap<DiscordId, &PlayerTriviaPlayer>,
    random: &mut impl PlayerTriviaRandom,
) {
    let mut distinct_played = BTreeMap::<DiscordId, BTreeSet<i64>>::new();
    let mut distinct_won = BTreeMap::<DiscordId, BTreeSet<i64>>::new();
    let mut distinct_lost = BTreeMap::<DiscordId, BTreeSet<i64>>::new();
    let mut row_counts = BTreeMap::<DiscordId, usize>::new();
    for row in &snapshot.participants {
        if !players.contains_key(&row.discord_id) || row.hero_id <= 0 {
            continue;
        }
        *row_counts.entry(row.discord_id).or_default() += 1;
        distinct_played
            .entry(row.discord_id)
            .or_default()
            .insert(row.hero_id);
        if row.won {
            distinct_won
                .entry(row.discord_id)
                .or_default()
                .insert(row.hero_id);
        } else {
            distinct_lost
                .entry(row.discord_id)
                .or_default()
                .insert(row.hero_id);
        }
    }
    let played_values = distinct_played
        .into_iter()
        .filter(|(player_id, _)| row_counts.get(player_id).copied().unwrap_or(0) >= 3)
        .map(|(player_id, heroes)| (player_id, heroes.len() as f64))
        .collect();
    add_player_leader(
        candidates,
        players,
        "heroes:most_distinct_played",
        "heroes",
        "Who has played the widest variety of heroes in enriched matches?",
        played_values,
        Some(2.0),
        false,
        |value| value,
        |name, value| {
            format!(
                "As of game start, {name} had played {value:.0} distinct heroes in enriched matches."
            )
        },
        false,
        random,
    );
    let won_values = distinct_won
        .into_iter()
        .filter(|(player_id, _)| row_counts.get(player_id).copied().unwrap_or(0) >= 3)
        .map(|(player_id, heroes)| (player_id, heroes.len() as f64))
        .collect();
    add_player_leader(
        candidates,
        players,
        "heroes:most_distinct_won",
        "heroes",
        "Who has recorded wins with the most distinct heroes?",
        won_values,
        Some(2.0),
        false,
        |value| value,
        |name, value| format!("As of game start, {name} had won with {value:.0} distinct heroes."),
        false,
        random,
    );
    let lost_values = distinct_lost
        .into_iter()
        .filter(|(player_id, _)| row_counts.get(player_id).copied().unwrap_or(0) >= 3)
        .map(|(player_id, heroes)| (player_id, heroes.len() as f64))
        .collect();
    add_player_leader(
        candidates,
        players,
        "heroes:most_distinct_lost",
        "heroes",
        "Who has recorded losses with the most distinct heroes?",
        lost_values,
        Some(2.0),
        false,
        |value| value,
        |name, value| format!("As of game start, {name} had lost with {value:.0} distinct heroes."),
        false,
        random,
    );

    let eligible = snapshot
        .participants
        .iter()
        .filter(|row| players.contains_key(&row.discord_id) && row.hero_id > 0)
        .fold(
            BTreeMap::<DiscordId, Vec<&ParticipantRow>>::new(),
            |mut rows, row| {
                rows.entry(row.discord_id).or_default().push(row);
                rows
            },
        )
        .into_iter()
        .filter(|(_, rows)| rows.len() >= 3)
        .collect::<BTreeMap<_, _>>();
    let all_hero_names = eligible
        .values()
        .flat_map(|rows| rows.iter().map(|row| hero_name(row.hero_id)))
        .collect::<BTreeSet<_>>();
    for (player_id, rows) in &eligible {
        let mut counts = BTreeMap::<i64, i64>::new();
        let mut wins = BTreeMap::<i64, i64>::new();
        for row in rows {
            *counts.entry(row.hero_id).or_default() += 1;
            if row.won {
                *wins.entry(row.hero_id).or_default() += 1;
            }
        }
        let mut ordered_counts = counts.iter().collect::<Vec<_>>();
        ordered_counts.sort_by_key(|(hero_id, count)| (std::cmp::Reverse(**count), **hero_id));
        if ordered_counts[0].1 >= &3
            && ordered_counts
                .get(1)
                .is_none_or(|runner_up| runner_up.1 < ordered_counts[0].1)
        {
            let hero = hero_name(*ordered_counts[0].0);
            add_value_question(
                candidates,
                format!("heroes:most_played:{player_id}"),
                "heroes",
                format!(
                    "Which hero has {} played most often?",
                    player_label(*player_id)
                ),
                hero.clone(),
                all_hero_names.iter().filter(|name| *name != &hero).cloned(),
                format!(
                    "As of game start, {} had {} enriched games on {hero}.",
                    player_label(*player_id),
                    ordered_counts[0].1
                ),
                format!("player:{player_id}"),
                false,
                random,
            );
        }
        let latest = rows
            .iter()
            .max_by_key(|row| (row.match_time, row.match_id))
            .expect("eligible player has participant rows");
        let latest_hero = hero_name(latest.hero_id);
        add_value_question(
            candidates,
            format!("heroes:most_recent:{player_id}"),
            "heroes",
            format!(
                "Which hero did {} play most recently?",
                player_label(*player_id)
            ),
            latest_hero.clone(),
            all_hero_names
                .iter()
                .filter(|name| *name != &latest_hero)
                .cloned(),
            format!(
                "Ordering enriched matches by match date and match ID, {}'s latest hero was {latest_hero}.",
                player_label(*player_id)
            ),
            format!("player:{player_id}"),
            false,
            random,
        );
        let mut ordered_wins = wins.iter().collect::<Vec<_>>();
        ordered_wins.sort_by_key(|(hero_id, count)| (std::cmp::Reverse(**count), **hero_id));
        if ordered_wins.first().is_some_and(|row| *row.1 >= 2)
            && ordered_wins
                .get(1)
                .is_none_or(|runner_up| runner_up.1 < ordered_wins[0].1)
        {
            let hero = hero_name(*ordered_wins[0].0);
            add_value_question(
                candidates,
                format!("heroes:most_wins:{player_id}"),
                "heroes",
                format!(
                    "Which hero has {} won with most often?",
                    player_label(*player_id)
                ),
                hero.clone(),
                all_hero_names.iter().filter(|name| *name != &hero).cloned(),
                format!(
                    "As of game start, {} had {} wins on {hero}.",
                    player_label(*player_id),
                    ordered_wins[0].1
                ),
                format!("player:{player_id}"),
                false,
                random,
            );
        }

        if rows.len() >= 8 {
            let mut rates = counts
                .iter()
                .filter(|(_, games)| **games >= 3)
                .map(|(hero_id, games)| {
                    (
                        *hero_id,
                        *wins.get(hero_id).unwrap_or(&0) as f64 / *games as f64,
                        *games,
                    )
                })
                .collect::<Vec<_>>();
            rates.sort_by(|left, right| left.1.total_cmp(&right.1).then(left.0.cmp(&right.0)));
            if rates.len() >= 4
                && cama_domain::formatting::python_round_decimals(rates[0].1 * 100.0, 1)
                    != cama_domain::formatting::python_round_decimals(rates[1].1 * 100.0, 1)
            {
                let hero = hero_name(rates[0].0);
                add_value_question(
                    candidates,
                    format!("heroes:worst_win_rate:{player_id}"),
                    "heroes",
                    format!(
                        "Which qualifying hero gives {} their lowest win rate?",
                        player_label(*player_id)
                    ),
                    hero.clone(),
                    rates.iter().skip(1).map(|row| hero_name(row.0)),
                    format!(
                        "As of game start, {} was {:.1}% across {} games on {hero}.",
                        player_label(*player_id),
                        rates[0].1 * 100.0,
                        rates[0].2
                    ),
                    format!("player:{player_id}"),
                    true,
                    random,
                );
            }
        }

        let mut lane_counts = BTreeMap::<i64, i64>::new();
        for lane in rows.iter().filter_map(|row| row.lane_role) {
            if lane_name(lane).is_some() {
                *lane_counts.entry(lane).or_default() += 1;
            }
        }
        let mut lanes = lane_counts.iter().collect::<Vec<_>>();
        lanes.sort_by_key(|(lane, count)| (std::cmp::Reverse(**count), **lane));
        if lane_counts.values().sum::<i64>() >= 5
            && !lanes.is_empty()
            && lanes
                .get(1)
                .is_none_or(|runner_up| runner_up.1 < lanes[0].1)
        {
            let lane = lane_name(*lanes[0].0).expect("validated lane");
            add_value_question(
                candidates,
                format!("lanes:most_common:{player_id}"),
                "lanes",
                format!(
                    "Which lane has {} played most often?",
                    player_label(*player_id)
                ),
                lane.to_owned(),
                ["Roaming", "Safe Lane", "Mid Lane", "Off Lane", "Jungle"]
                    .into_iter()
                    .filter(|name| *name != lane)
                    .map(str::to_owned),
                format!(
                    "As of game start, {} had {} enriched games in the {lane}.",
                    player_label(*player_id),
                    lanes[0].1
                ),
                format!("player:{player_id}"),
                false,
                random,
            );
        }
    }
    build_performance_questions(candidates, &eligible, players, random);
}

fn build_performance_questions(
    candidates: &mut Vec<Candidate>,
    enriched: &BTreeMap<DiscordId, Vec<&ParticipantRow>>,
    players: &BTreeMap<DiscordId, &PlayerTriviaPlayer>,
    random: &mut impl PlayerTriviaRandom,
) {
    type Metric = (
        &'static str,
        &'static str,
        usize,
        fn(&ParticipantRow) -> Option<f64>,
    );
    let metrics: [Metric; 6] = [
        ("kills", "average kills", 1, |row| row.kills),
        ("assists", "average assists", 1, |row| row.assists),
        ("gpm", "average GPM", 0, |row| row.gpm),
        ("hero_damage", "average hero damage", 0, |row| {
            row.hero_damage
        }),
        ("tower_damage", "average tower damage", 0, |row| {
            row.tower_damage
        }),
        ("fantasy_points", "average fantasy points", 1, |row| {
            row.fantasy_points
        }),
    ];
    for (field, label, digits, measure) in metrics {
        let values = enriched
            .iter()
            .filter_map(|(player_id, rows)| {
                let measured = rows
                    .iter()
                    .filter_map(|row| measure(row))
                    .collect::<Vec<_>>();
                (measured.len() >= 10).then_some((
                    *player_id,
                    measured.iter().sum::<f64>() / measured.len() as f64,
                ))
            })
            .collect();

        add_player_leader(
            candidates,
            players,
            &format!("performance:highest_{field}_average"),
            "performance",
            &format!("Among players with at least 10 enriched games, who has the highest {label}?"),
            values,
            None,
            false,
            move |value| cama_domain::formatting::python_round_decimals(value, digits),
            move |name, value| format!("As of game start, {name}'s {label} was {value:.digits$}."),
            false,
            random,
        );
    }

    type RecordMetric = (
        &'static str,
        &'static str,
        fn(&ParticipantRow) -> Option<f64>,
    );
    let record_metrics: [RecordMetric; 3] = [
        ("kills", "single-match kill record", |row| row.kills),
        ("gpm", "single-match GPM record", |row| row.gpm),
        ("hero_damage", "single-match hero-damage record", |row| {
            row.hero_damage
        }),
    ];
    for (field, label, measure) in record_metrics {
        let values = enriched
            .iter()
            .filter_map(|(player_id, rows)| {
                rows.iter()
                    .filter_map(|row| measure(row))
                    .max_by(f64::total_cmp)
                    .map(|value| (*player_id, value))
            })
            .collect();
        add_player_leader(
            candidates,
            players,
            &format!("performance:record_{field}"),
            "performance",
            &format!("Who holds the highest {label}?"),
            values,
            None,
            false,
            f64::round,
            move |name, value| format!("As of game start, {name}'s {label} was {value:.0}."),
            false,
            random,
        );
    }
}

fn build_pairing_questions(
    candidates: &mut Vec<Candidate>,
    snapshot: &PlayerTriviaSnapshot,
    players: &BTreeMap<DiscordId, &PlayerTriviaPlayer>,
    random: &mut impl PlayerTriviaRandom,
) {
    type PairRecord = (i64, i64);
    let mut together = BTreeMap::<DiscordId, BTreeMap<DiscordId, PairRecord>>::new();
    let mut against = BTreeMap::<DiscordId, BTreeMap<DiscordId, PairRecord>>::new();
    for row in &snapshot.pairings {
        if row.player1_id == row.player2_id
            || !players.contains_key(&row.player1_id)
            || !players.contains_key(&row.player2_id)
        {
            continue;
        }
        let games_together = row.games_together.max(0);
        let wins_together = row.wins_together.clamp(0, games_together);
        let games_against = row.games_against.max(0);
        let player1_wins = row.player1_wins_against.clamp(0, games_against);
        if games_together > 0 {
            together
                .entry(row.player1_id)
                .or_default()
                .insert(row.player2_id, (games_together, wins_together));
            together
                .entry(row.player2_id)
                .or_default()
                .insert(row.player1_id, (games_together, wins_together));
        }
        if games_against > 0 {
            against
                .entry(row.player1_id)
                .or_default()
                .insert(row.player2_id, (games_against, player1_wins));
            against.entry(row.player2_id).or_default().insert(
                row.player1_id,
                (games_against, games_against - player1_wins),
            );
        }
    }

    for player_id in players.keys().copied() {
        if let Some(teammates) = together.get(&player_id).filter(|rows| rows.len() >= 4) {
            let mut by_games = teammates.iter().collect::<Vec<_>>();
            by_games.sort_by_key(|(other_id, (games, _))| (std::cmp::Reverse(*games), **other_id));
            if by_games[0].1.0 >= 5 && by_games[0].1.0 > by_games[1].1.0 {
                let teammate_id = *by_games[0].0;
                let games = by_games[0].1.0;
                add_value_question(
                    candidates,
                    format!("pairings:most_common_teammate:{player_id}"),
                    "pairings",
                    format!(
                        "Who has played alongside {} most often?",
                        player_label(player_id)
                    ),
                    player_label(teammate_id),
                    by_games.iter().skip(1).map(|row| player_label(*row.0)),
                    format!(
                        "As of game start, {} and {} had shared {games} matches.",
                        player_label(player_id),
                        player_label(teammate_id)
                    ),
                    format!("player:{teammate_id}"),
                    false,
                    random,
                );
            }
            let mut qualifying = teammates
                .iter()
                .filter(|(_, (games, _))| *games >= 5)
                .map(|(other_id, (games, wins))| {
                    (*other_id, *games, *wins, *wins as f64 / *games as f64)
                })
                .collect::<Vec<_>>();
            qualifying.sort_by(|left, right| right.3.total_cmp(&left.3).then(left.0.cmp(&right.0)));
            if qualifying.len() >= 4
                && cama_domain::formatting::python_round_decimals(qualifying[0].3 * 100.0, 1)
                    != cama_domain::formatting::python_round_decimals(qualifying[1].3 * 100.0, 1)
            {
                let (teammate_id, games, wins, rate) = qualifying[0];
                add_value_question(
                    candidates,
                    format!("pairings:best_teammate:{player_id}"),
                    "pairings",
                    format!(
                        "Among qualifying teammates, who has the highest win rate with {}?",
                        player_label(player_id)
                    ),
                    player_label(teammate_id),
                    qualifying.iter().skip(1).map(|row| player_label(row.0)),
                    format!(
                        "As of game start, they were {wins}W–{}L together ({:.1}%).",
                        games - wins,
                        rate * 100.0
                    ),
                    format!("player:{teammate_id}"),
                    false,
                    random,
                );
            }
            qualifying.sort_by(|left, right| left.3.total_cmp(&right.3).then(left.0.cmp(&right.0)));
            if qualifying.len() >= 4
                && cama_domain::formatting::python_round_decimals(qualifying[0].3 * 100.0, 1)
                    != cama_domain::formatting::python_round_decimals(qualifying[1].3 * 100.0, 1)
            {
                let (teammate_id, games, wins, rate) = qualifying[0];
                add_value_question(
                    candidates,
                    format!("pairings:worst_teammate:{player_id}"),
                    "pairings",
                    format!(
                        "Among qualifying teammates, who has the lowest win rate with {}?",
                        player_label(player_id)
                    ),
                    player_label(teammate_id),
                    qualifying.iter().skip(1).map(|row| player_label(row.0)),
                    format!(
                        "As of game start, they were {wins}W–{}L together ({:.1}%).",
                        games - wins,
                        rate * 100.0
                    ),
                    format!("player:{teammate_id}"),
                    true,
                    random,
                );
            }
        }

        if let Some(opponents) = against.get(&player_id).filter(|rows| rows.len() >= 4) {
            let mut by_games = opponents.iter().collect::<Vec<_>>();
            by_games.sort_by_key(|(other_id, (games, _))| (std::cmp::Reverse(*games), **other_id));
            if by_games[0].1.0 >= 5 && by_games[0].1.0 > by_games[1].1.0 {
                let opponent_id = *by_games[0].0;
                let games = by_games[0].1.0;
                add_value_question(
                    candidates,
                    format!("pairings:most_common_opponent:{player_id}"),
                    "pairings",
                    format!(
                        "Who has faced {} as an opponent most often?",
                        player_label(player_id)
                    ),
                    player_label(opponent_id),
                    by_games.iter().skip(1).map(|row| player_label(*row.0)),
                    format!(
                        "As of game start, {} had faced {} {games} times.",
                        player_label(player_id),
                        player_label(opponent_id)
                    ),
                    format!("player:{opponent_id}"),
                    false,
                    random,
                );
            }
            let mut qualifying = opponents
                .iter()
                .filter(|(_, (games, _))| *games >= 5)
                .map(|(other_id, (games, wins))| {
                    (*other_id, *games, *wins, *wins as f64 / *games as f64)
                })
                .collect::<Vec<_>>();
            qualifying.sort_by(|left, right| right.3.total_cmp(&left.3).then(left.0.cmp(&right.0)));
            if qualifying.len() >= 4
                && cama_domain::formatting::python_round_decimals(qualifying[0].3 * 100.0, 1)
                    != cama_domain::formatting::python_round_decimals(qualifying[1].3 * 100.0, 1)
            {
                let (opponent_id, games, wins, rate) = qualifying[0];
                add_value_question(
                    candidates,
                    format!("pairings:best_opponent:{player_id}"),
                    "pairings",
                    format!(
                        "Against which qualifying opponent does {} have the best win rate?",
                        player_label(player_id)
                    ),
                    player_label(opponent_id),
                    qualifying.iter().skip(1).map(|row| player_label(row.0)),
                    format!(
                        "As of game start, {} was {wins}W–{}L against {} ({:.1}%).",
                        player_label(player_id),
                        games - wins,
                        player_label(opponent_id),
                        rate * 100.0
                    ),
                    format!("player:{opponent_id}"),
                    false,
                    random,
                );
            }
        }
    }
}

fn build_economy_and_tip_questions(
    candidates: &mut Vec<Candidate>,
    snapshot: &PlayerTriviaSnapshot,
    players: &BTreeMap<DiscordId, &PlayerTriviaPlayer>,
    random: &mut impl PlayerTriviaRandom,
) {
    let balances = players
        .iter()
        .map(|(player_id, player)| (*player_id, player.balance as f64))
        .collect();
    add_player_leader(
        candidates,
        players,
        "economy:highest_balance",
        "economy",
        "Who has the highest current Jopacoin balance?",
        balances,
        None,
        false,
        |value| value,
        |name, value| format!("As of game start, {name} had {value:.0} JC."),
        false,
        random,
    );
    let balances = players
        .iter()
        .map(|(player_id, player)| (*player_id, player.balance as f64))
        .collect();
    add_player_leader(
        candidates,
        players,
        "economy:lowest_balance",
        "economy",
        "Who has the lowest current Jopacoin balance?",
        balances,
        None,
        true,
        |value| value,
        |name, value| format!("As of game start, {name} had {value:.0} JC."),
        true,
        random,
    );
    let lowest_ever = players
        .iter()
        .map(|(player_id, player)| {
            (
                *player_id,
                player.lowest_balance_ever.unwrap_or(player.balance) as f64,
            )
        })
        .collect();
    add_player_leader(
        candidates,
        players,
        "economy:deepest_historical_debt",
        "economy",
        "Who has recorded the deepest all-time Jopacoin debt?",
        lowest_ever,
        None,
        true,
        |value| value,
        |name, value| {
            format!("As of game start, {name}'s lowest recorded balance was {value:.0} JC.")
        },
        true,
        random,
    );

    let balance_options = players
        .iter()
        .map(|(player_id, player)| (*player_id, format!("{} JC", player.balance)))
        .collect::<BTreeMap<_, _>>();
    for (player_id, correct) in &balance_options {
        add_value_question(
            candidates,
            format!("economy:balance:{player_id}"),
            "economy",
            format!(
                "What was {}'s Jopacoin balance when this game began?",
                player_label(*player_id)
            ),
            correct.clone(),
            balance_options
                .iter()
                .filter(|(other_id, _)| *other_id != player_id)
                .map(|(_, value)| value.clone()),
            format!(
                "At the question snapshot, {} had {correct}.",
                player_label(*player_id)
            ),
            format!("player:{player_id}"),
            false,
            random,
        );
    }

    let bankruptcies = snapshot
        .bankruptcies
        .iter()
        .map(|row| (row.discord_id, row.bankruptcy_count as f64))
        .collect();
    add_player_leader(
        candidates,
        players,
        "economy:most_bankruptcies",
        "economy",
        "Who has the most recorded bankruptcy declarations?",
        bankruptcies,
        Some(1.0),
        false,
        |value| value,
        |name, value| format!("As of game start, {name} had declared bankruptcy {value:.0} times."),
        false,
        random,
    );

    let mut sent_amount = BTreeMap::<DiscordId, i64>::new();
    let mut sent_count = BTreeMap::<DiscordId, i64>::new();
    let mut received_amount = BTreeMap::<DiscordId, i64>::new();
    let mut received_count = BTreeMap::<DiscordId, i64>::new();
    for row in &snapshot.tips {
        if players.contains_key(&row.sender_id) {
            *sent_amount.entry(row.sender_id).or_default() += row.amount.max(0);
            *sent_count.entry(row.sender_id).or_default() += 1;
        }
        if players.contains_key(&row.recipient_id) {
            *received_amount.entry(row.recipient_id).or_default() += row.amount.max(0);
            *received_count.entry(row.recipient_id).or_default() += 1;
        }
    }
    add_player_leader(
        candidates,
        players,
        "tips:most_jc_sent",
        "tips",
        "Who has sent the most total JC through tips?",
        sent_amount
            .into_iter()
            .map(|(player_id, value)| (player_id, value as f64))
            .collect(),
        Some(1.0),
        false,
        |value| value,
        |name, value| format!("As of game start, {name} had sent {value:.0} JC in tips."),
        false,
        random,
    );
    add_player_leader(
        candidates,
        players,
        "tips:most_transactions_sent",
        "tips",
        "Who has sent the most individual tip transactions?",
        sent_count
            .into_iter()
            .map(|(player_id, value)| (player_id, value as f64))
            .collect(),
        Some(2.0),
        false,
        |value| value,
        |name, value| format!("As of game start, {name} had sent {value:.0} tips."),
        false,
        random,
    );
    add_player_leader(
        candidates,
        players,
        "tips:most_transactions_received",
        "tips",
        "Who has received the most individual tip transactions?",
        received_count
            .into_iter()
            .map(|(player_id, value)| (player_id, value as f64))
            .collect(),
        Some(2.0),
        false,
        |value| value,
        |name, value| format!("As of game start, {name} had received {value:.0} tips."),
        false,
        random,
    );
    add_player_leader(
        candidates,
        players,
        "tips:most_jc_received",
        "tips",
        "Who has received the most total JC through tips?",
        received_amount
            .into_iter()
            .map(|(player_id, value)| (player_id, value as f64))
            .collect(),
        Some(1.0),
        false,
        |value| value,
        |name, value| format!("As of game start, {name} had received {value:.0} JC in tips."),
        false,
        random,
    );
}

fn build_betting_questions(
    candidates: &mut Vec<Candidate>,
    snapshot: &PlayerTriviaSnapshot,
    players: &BTreeMap<DiscordId, &PlayerTriviaPlayer>,
    random: &mut impl PlayerTriviaRandom,
) {
    #[derive(Default)]
    struct BetStats {
        bets: i64,
        wins: i64,
        wagered: i64,
        pnl: i64,
    }
    let mut stats = BTreeMap::<DiscordId, BetStats>::new();
    let mut taxed_settlements = BTreeSet::new();
    for row in &snapshot.bets {
        if !players.contains_key(&row.discord_id) || !matches!(row.winning_team, 1 | 2) {
            continue;
        }
        let amount = row.amount.max(0);
        let leverage = row.leverage.max(1);
        let effective = row
            .effective_bet
            .unwrap_or_else(|| amount.saturating_mul(leverage))
            .max(0);
        let won = (row.team_bet_on.eq_ignore_ascii_case("radiant") && row.winning_team == 1)
            || (row.team_bet_on.eq_ignore_ascii_case("dire") && row.winning_team == 2);
        let tax_key = (row.discord_id, row.match_id);
        // One settlement withholds both profit taxes together, so they share
        // the single first-sighting guard.
        let settled = won && taxed_settlements.insert(tax_key);
        let vanity_tax = if settled {
            row.settlement_vanity_tax
        } else {
            0
        };
        let low_priority_tax = if settled {
            row.settlement_low_priority_tax
        } else {
            0
        };
        let player = stats.entry(row.discord_id).or_default();
        player.bets += 1;
        player.wins += i64::from(won);
        player.wagered = player.wagered.saturating_add(effective);
        player.pnl += row.payout - effective - vanity_tax - low_priority_tax;
    }
    add_player_leader(
        candidates,
        players,
        "betting:most_bets",
        "betting",
        "Who has placed the most settled match bets?",
        stats
            .iter()
            .map(|(player_id, row)| (*player_id, row.bets as f64))
            .collect(),
        Some(5.0),
        false,
        |value| value,
        |name, value| format!("As of game start, {name} had {value:.0} settled bets."),
        false,
        random,
    );
    add_player_leader(
        candidates,
        players,
        "betting:most_wagered",
        "betting",
        "Who has wagered the most total leveraged JC on settled matches?",
        stats
            .iter()
            .filter(|(_, row)| row.bets >= 5)
            .map(|(player_id, row)| (*player_id, row.wagered as f64))
            .collect(),
        Some(1.0),
        false,
        |value| value,
        |name, value| format!("As of game start, {name} had wagered {value:.0} leveraged JC."),
        false,
        random,
    );
    add_player_leader(
        candidates,
        players,
        "betting:highest_win_rate",
        "betting",
        "Among players with at least five settled bets, who has the best betting win rate?",
        stats
            .iter()
            .filter(|(_, row)| row.bets >= 5)
            .map(|(player_id, row)| (*player_id, row.wins as f64 / row.bets as f64))
            .collect(),
        None,
        false,
        |value| cama_domain::formatting::python_round_decimals(value * 100.0, 1),
        |name, value| {
            format!(
                "As of game start, {name}'s settled-bet win rate was {:.1}%.",
                value * 100.0
            )
        },
        false,
        random,
    );
    let losses = stats
        .into_iter()
        .filter(|(_, row)| row.bets >= 5)
        .map(|(player_id, row)| (player_id, row.pnl.saturating_neg().max(0) as f64))
        .collect();
    add_player_leader(
        candidates,
        players,
        "betting:largest_loss",
        "betting",
        "Who has the largest cumulative loss on settled match bets?",
        losses,
        Some(1.0),
        false,
        |value| value,
        |name, value| format!("As of game start, {name} was down {value:.0} JC on settled bets."),
        true,
        random,
    );
}

fn code_slug(value: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in value.to_lowercase().chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            separator = false;
            slug.push(character);
        } else {
            separator = true;
        }
        if slug.len() >= 48 {
            break;
        }
    }
    slug.trim_end_matches('-').to_owned()
}

fn count_option(value: i64) -> String {
    if value == 1 {
        "1 time".to_owned()
    } else {
        format!("{value} times")
    }
}

fn build_wheel_questions(
    candidates: &mut Vec<Candidate>,
    snapshot: &PlayerTriviaSnapshot,
    players: &BTreeMap<DiscordId, &PlayerTriviaPlayer>,
    random: &mut impl PlayerTriviaRandom,
) {
    let rows = snapshot
        .wheel_spins
        .iter()
        .filter(|row| players.contains_key(&row.discord_id))
        .collect::<Vec<_>>();
    let mut spin_counts = BTreeMap::<DiscordId, i64>::new();
    let mut by_player = BTreeMap::<DiscordId, BTreeMap<String, i64>>::new();
    let mut by_code = BTreeMap::<String, Vec<&WheelSpinRow>>::new();
    for row in rows {
        *spin_counts.entry(row.discord_id).or_default() += 1;
        let Some(raw_code) = row
            .outcome_code
            .as_deref()
            .filter(|code| !code.trim().is_empty())
        else {
            continue;
        };
        let code = humanize_code(raw_code);
        *by_player
            .entry(row.discord_id)
            .or_default()
            .entry(code.clone())
            .or_default() += 1;
        by_code.entry(code).or_default().push(row);
    }
    add_player_leader(
        candidates,
        players,
        "wheel:most_spins",
        "wheel",
        "Who has the most recorded Wheel of Fortune spins?",
        spin_counts
            .into_iter()
            .map(|(player_id, count)| (player_id, count as f64))
            .collect(),
        Some(2.0),
        false,
        |value| value,
        |name, value| format!("As of game start, {name} had {value:.0} recorded wheel spins."),
        false,
        random,
    );

    let all_codes = by_code.keys().cloned().collect::<Vec<_>>();
    for (player_id, outcomes) in &by_player {
        let total = outcomes.values().sum::<i64>();
        let mut ordered = outcomes.iter().collect::<Vec<_>>();
        ordered.sort_by(|left, right| right.1.cmp(left.1).then(left.0.cmp(right.0)));
        if total < 3
            || ordered.is_empty()
            || ordered
                .get(1)
                .is_some_and(|runner_up| runner_up.1 == ordered[0].1)
        {
            continue;
        }
        let (code, times) = ordered[0];
        add_value_question(
            candidates,
            format!("wheel:most_common_outcome:{player_id}"),
            "wheel",
            format!(
                "Which exact wheel outcome has {} recorded most often?",
                player_label(*player_id)
            ),
            code.clone(),
            all_codes.iter().filter(|other| *other != code).cloned(),
            format!(
                "As of game start, {} had landed {code} {times} times.",
                player_label(*player_id)
            ),
            format!("player:{player_id}"),
            false,
            random,
        );
    }

    for (code, outcome_rows) in by_code {
        let mut exact_counts = players
            .keys()
            .map(|player_id| (*player_id, 0_i64))
            .collect::<BTreeMap<_, _>>();
        for row in &outcome_rows {
            *exact_counts.entry(row.discord_id).or_default() += 1;
        }
        let slug = code_slug(&code);
        add_player_leader(
            candidates,
            players,
            &format!("wheel:exact_outcome_leader:{slug}"),
            "wheel",
            &format!("Who has recorded the exact wheel outcome {code} most often?"),
            exact_counts
                .iter()
                .map(|(player_id, count)| (*player_id, *count as f64))
                .collect(),
            Some(2.0),
            false,
            |value| value,
            |name, value| format!("As of game start, {name} had recorded {code} {value:.0} times."),
            false,
            random,
        );
        for (player_id, count) in &exact_counts {
            if *count < 2 {
                continue;
            }
            let mut plausible = exact_counts
                .iter()
                .filter_map(|(other_id, other_count)| {
                    (other_id != player_id && other_count != count).then_some(*other_count)
                })
                .collect::<BTreeSet<_>>();
            for value in [
                count.saturating_sub(1),
                count.saturating_add(1),
                count / 2,
                count.saturating_mul(2),
                0,
            ] {
                if value != *count {
                    plausible.insert(value);
                }
                if plausible.len() >= 3 {
                    break;
                }
            }
            let correct = count_option(*count);
            add_value_question(
                candidates,
                format!("wheel:exact_outcome_count:{slug}:{player_id}"),
                "wheel",
                format!(
                    "How many times has {} recorded the exact wheel outcome {code}?",
                    player_label(*player_id)
                ),
                correct.clone(),
                plausible.into_iter().map(count_option),
                format!(
                    "At the question snapshot, {} had recorded {code} {correct}.",
                    player_label(*player_id)
                ),
                format!("player:{player_id}"),
                false,
                random,
            );
        }
        let mut latest = outcome_rows;
        latest.sort_by_key(|row| {
            (
                std::cmp::Reverse(row.spin_time),
                std::cmp::Reverse(row.spin_id),
            )
        });
        if let Some(row) = latest.first() {
            let player_id = row.discord_id;
            add_value_question(
                candidates,
                format!(
                    "wheel:latest_exact_outcome:{}",
                    row.outcome_code
                        .as_deref()
                        .unwrap_or_default()
                        .to_lowercase()
                ),
                "wheel",
                format!("Who most recently recorded the exact wheel outcome {code}?"),
                player_label(player_id),
                players
                    .keys()
                    .filter(|other_id| **other_id != player_id)
                    .map(|other_id| player_label(*other_id)),
                format!(
                    "The latest recorded {code} outcome belonged to {}.",
                    player_label(player_id)
                ),
                format!("player:{player_id}"),
                false,
                random,
            );
        }
    }
}

fn build_double_questions(
    candidates: &mut Vec<Candidate>,
    snapshot: &PlayerTriviaSnapshot,
    players: &BTreeMap<DiscordId, &PlayerTriviaPlayer>,
    random: &mut impl PlayerTriviaRandom,
) {
    let mut attempts = BTreeMap::<DiscordId, i64>::new();
    let mut wins = BTreeMap::<DiscordId, i64>::new();
    for row in &snapshot.double_spins {
        if !players.contains_key(&row.discord_id) {
            continue;
        }
        *attempts.entry(row.discord_id).or_default() += 1;
        *wins.entry(row.discord_id).or_default() += i64::from(row.won);
    }
    add_player_leader(
        candidates,
        players,
        "double:most_attempts",
        "double-or-nothing",
        "Who has attempted Double or Nothing most often?",
        attempts
            .iter()
            .map(|(player_id, count)| (*player_id, *count as f64))
            .collect(),
        Some(2.0),
        false,
        |value| value,
        |name, value| {
            format!("As of game start, {name} had {value:.0} Double-or-Nothing attempts.")
        },
        false,
        random,
    );
    add_player_leader(
        candidates,
        players,
        "double:most_wins",
        "double-or-nothing",
        "Who has the most recorded Double-or-Nothing wins?",
        wins.iter()
            .map(|(player_id, count)| (*player_id, *count as f64))
            .collect(),
        Some(1.0),
        false,
        |value| value,
        |name, value| {
            format!("As of game start, {name} had won Double or Nothing {value:.0} times.")
        },
        false,
        random,
    );
    let rates = attempts
        .iter()
        .filter(|(_, attempts)| **attempts >= 5)
        .map(|(player_id, attempts)| {
            (
                *player_id,
                *wins.get(player_id).unwrap_or(&0) as f64 / *attempts as f64,
            )
        })
        .collect();
    add_player_leader(
        candidates,
        players,
        "double:highest_win_rate",
        "double-or-nothing",
        "Among players with five attempts, who has the best Double-or-Nothing win rate?",
        rates,
        None,
        false,
        |value| cama_domain::formatting::python_round_decimals(value * 100.0, 1),
        |name, value| {
            format!(
                "As of game start, {name}'s Double-or-Nothing win rate was {:.1}%.",
                value * 100.0
            )
        },
        false,
        random,
    );
}

fn prediction_market_name(row: &PredictionPositionRow) -> String {
    row.question
        .as_deref()
        .map(safe_market_descriptor)
        .filter(|descriptor| !descriptor.is_empty())
        .map_or_else(
            || format!("Market #{}", row.prediction_id),
            |descriptor| format!("Market #{} — {descriptor}", row.prediction_id),
        )
}

fn prediction_pnl(row: &PredictionPositionRow) -> Option<i64> {
    if !row.status.eq_ignore_ascii_case("resolved") || row.prediction_id <= 0 {
        return None;
    }
    let winning_contracts = if row.outcome.eq_ignore_ascii_case("yes") {
        row.yes_contracts
    } else if row.outcome.eq_ignore_ascii_case("no") {
        row.no_contracts
    } else {
        return None;
    };
    Some(
        winning_contracts
            .saturating_mul(CONTRACT_VALUE)
            .saturating_sub(row.yes_cost_basis_total)
            .saturating_sub(row.no_cost_basis_total)
            .saturating_sub(row.bankruptcy_penalty)
            .saturating_sub(row.vanity_tax)
            .saturating_sub(row.low_priority_tax),
    )
}

fn build_prediction_questions(
    candidates: &mut Vec<Candidate>,
    snapshot: &PlayerTriviaSnapshot,
    players: &BTreeMap<DiscordId, &PlayerTriviaPlayer>,
    random: &mut impl PlayerTriviaRandom,
) {
    let mut by_player = BTreeMap::<DiscordId, Vec<(&PredictionPositionRow, i64, String)>>::new();
    let mut all_markets = BTreeSet::new();
    for row in &snapshot.prediction_positions {
        if !players.contains_key(&row.discord_id) {
            continue;
        }
        let Some(pnl) = prediction_pnl(row) else {
            continue;
        };
        let market = prediction_market_name(row);
        all_markets.insert(market.clone());
        by_player
            .entry(row.discord_id)
            .or_default()
            .push((row, pnl, market));
    }
    let total_pnl = by_player
        .iter()
        .filter(|(_, rows)| rows.len() >= 3)
        .map(|(player_id, rows)| {
            (
                *player_id,
                rows.iter().map(|(_, pnl, _)| *pnl).sum::<i64>() as f64,
            )
        })
        .collect();
    let wins = by_player
        .iter()
        .filter(|(_, rows)| rows.len() >= 3)
        .map(|(player_id, rows)| {
            (
                *player_id,
                rows.iter().filter(|(_, pnl, _)| *pnl > 0).count() as f64,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let losses = by_player
        .iter()
        .filter(|(_, rows)| rows.len() >= 3)
        .map(|(player_id, rows)| {
            (
                *player_id,
                rows.iter().filter(|(_, pnl, _)| *pnl < 0).count() as f64,
            )
        })
        .collect();
    add_player_leader(
        candidates,
        players,
        "predictions:most_wins",
        "predictions",
        "Among players with three resolved positions, who has the most profitable prediction markets?",
        wins,
        Some(1.0),
        false,
        |value| value,
        |name, value| {
            format!(
                "As of game start, {name} had finished profitable on {value:.0} resolved markets."
            )
        },
        false,
        random,
    );
    add_player_leader(
        candidates,
        players,
        "predictions:best_total_pnl",
        "predictions",
        "Among players with three resolved positions, who has the highest total prediction P&L?",
        total_pnl,
        None,
        false,
        |value| value,
        |name, value| {
            format!("As of game start, {name}'s resolved prediction P&L was {value:+.0} JC.")
        },
        false,
        random,
    );
    add_player_leader(
        candidates,
        players,
        "predictions:most_losses",
        "predictions",
        "Who has lost JC on the most resolved prediction markets?",
        losses,
        Some(1.0),
        false,
        |value| value,
        |name, value| {
            format!(
                "As of game start, {name} had finished negative on {value:.0} resolved markets."
            )
        },
        true,
        random,
    );

    for (player_id, rows) in &by_player {
        if rows.len() < 3 {
            continue;
        }
        let losing = rows
            .iter()
            .filter(|(_, pnl, _)| *pnl < 0)
            .map(|(_, _, market)| market.clone())
            .collect::<BTreeSet<_>>();
        let non_losses = all_markets.difference(&losing).cloned().collect::<Vec<_>>();
        let winning = rows
            .iter()
            .filter(|(_, pnl, _)| *pnl > 0)
            .map(|(_, _, market)| market.clone())
            .collect::<BTreeSet<_>>();
        let non_wins = all_markets
            .difference(&winning)
            .cloned()
            .collect::<Vec<_>>();
        let mut ordered = rows.clone();
        ordered.sort_by_key(|(row, _, _)| row.prediction_id);
        for (row, pnl, market) in ordered {
            if pnl < 0 {
                add_value_question(
                    candidates,
                    format!("predictions:loss_market:{player_id}:{}", row.prediction_id),
                    "predictions",
                    format!(
                        "Which resolved market did {} finish with a loss on?",
                        player_label(*player_id)
                    ),
                    market.clone(),
                    non_losses.clone(),
                    format!(
                        "As of game start, {} had a {pnl:+} JC result on {market}.",
                        player_label(*player_id)
                    ),
                    format!("player:{player_id}"),
                    false,
                    random,
                );
            } else if pnl > 0 {
                add_value_question(
                    candidates,
                    format!("predictions:win_market:{player_id}:{}", row.prediction_id),
                    "predictions",
                    format!(
                        "Which resolved market did {} finish with a profit on?",
                        player_label(*player_id)
                    ),
                    market.clone(),
                    non_wins.clone(),
                    format!(
                        "As of game start, {} had a +{pnl} JC result on {market}.",
                        player_label(*player_id)
                    ),
                    format!("player:{player_id}"),
                    false,
                    random,
                );
            }
        }
    }
    let resolved_ids = snapshot
        .prediction_positions
        .iter()
        .filter(|row| row.status.eq_ignore_ascii_case("resolved"))
        .map(|row| row.prediction_id)
        .collect::<BTreeSet<_>>();
    let mut trade_count = BTreeMap::<DiscordId, i64>::new();
    let mut trade_volume = BTreeMap::<DiscordId, i64>::new();
    for row in &snapshot.prediction_trades {
        if by_player.get(&row.discord_id).map_or(0, Vec::len) < 3
            || (!resolved_ids.contains(&row.prediction_id)
                && !row.status.eq_ignore_ascii_case("resolved"))
        {
            continue;
        }
        *trade_count.entry(row.discord_id).or_default() += 1;
        *trade_volume.entry(row.discord_id).or_default() += row.contracts.max(0);
    }
    add_player_leader(
        candidates,
        players,
        "predictions:most_trades",
        "predictions",
        "Who has made the most trades in resolved prediction markets?",
        trade_count
            .into_iter()
            .map(|(player_id, value)| (player_id, value as f64))
            .collect(),
        Some(3.0),
        false,
        |value| value,
        |name, value| {
            format!("As of game start, {name} had made {value:.0} trades in resolved markets.")
        },
        false,
        random,
    );
    add_player_leader(
        candidates,
        players,
        "predictions:most_contract_volume",
        "predictions",
        "Who has traded the most contracts in resolved prediction markets?",
        trade_volume
            .into_iter()
            .map(|(player_id, value)| (player_id, value as f64))
            .collect(),
        Some(1.0),
        false,
        |value| value,
        |name, value| {
            format!("As of game start, {name} had traded {value:.0} resolved-market contracts.")
        },
        false,
        random,
    );
}

fn build_dig_questions(
    candidates: &mut Vec<Candidate>,
    snapshot: &PlayerTriviaSnapshot,
    players: &BTreeMap<DiscordId, &PlayerTriviaPlayer>,
    random: &mut impl PlayerTriviaRandom,
) {
    let tunnels = snapshot
        .tunnels
        .iter()
        .filter(|row| players.contains_key(&row.discord_id))
        .map(|row| (row.discord_id, row))
        .collect::<BTreeMap<_, _>>();
    type TunnelMetric = (
        &'static str,
        &'static str,
        &'static str,
        f64,
        fn(&TunnelRow) -> i64,
    );
    let metrics: [TunnelMetric; 5] = [
        (
            "most_digs",
            "Who has made the most recorded digs?",
            "recorded digs",
            5.0,
            |row| row.total_digs,
        ),
        (
            "deepest_tunnel",
            "Who has reached the greatest all-time tunnel depth?",
            "maximum depth",
            1.0,
            |row| row.max_depth,
        ),
        (
            "most_jc_earned",
            "Who has earned the most total JC from digging?",
            "JC earned from digging",
            1.0,
            |row| row.total_jc_earned,
        ),
        (
            "highest_prestige",
            "Who has the highest Dig prestige level?",
            "Dig prestige level",
            1.0,
            |row| row.prestige_level,
        ),
        (
            "best_run_score",
            "Who has the highest recorded Dig run score?",
            "best Dig run score",
            1.0,
            |row| row.best_run_score,
        ),
    ];
    for (suffix, text, label, minimum, value) in metrics {
        add_player_leader(
            candidates,
            players,
            &format!("dig:{suffix}"),
            "dig",
            text,
            tunnels
                .iter()
                .map(|(player_id, row)| (*player_id, value(row) as f64))
                .collect(),
            Some(minimum),
            false,
            |number| number,
            move |name, number| format!("As of game start, {name}'s {label} was {number:.0}."),
            false,
            random,
        );
    }

    type ProfileMetric = (
        &'static str,
        &'static str,
        &'static str,
        fn(&TunnelRow) -> i64,
    );
    let profiles: [ProfileMetric; 9] = [
        ("total_digs", "total recorded dig count", "digs", |row| {
            row.total_digs
        }),
        (
            "max_depth",
            "all-time maximum tunnel depth",
            "blocks",
            |row| row.max_depth,
        ),
        ("prestige_level", "Dig prestige level", "levels", |row| {
            row.prestige_level
        }),
        ("pickaxe_tier", "current pickaxe tier", "tiers", |row| {
            row.pickaxe_tier
        }),
        (
            "total_jc_earned",
            "all-time JC earned from Dig",
            "JC",
            |row| row.total_jc_earned,
        ),
        ("best_run_score", "best Dig run score", "points", |row| {
            row.best_run_score
        }),
        ("stat_strength", "Dig Strength stat", "points", |row| {
            row.stat_strength
        }),
        ("stat_smarts", "Dig Smarts stat", "points", |row| {
            row.stat_smarts
        }),
        ("stat_stamina", "Dig Stamina stat", "points", |row| {
            row.stat_stamina
        }),
    ];
    for (field, label, unit, value) in profiles {
        let rendered = tunnels
            .iter()
            .map(|(player_id, row)| (*player_id, format!("{} {unit}", value(row))))
            .collect::<BTreeMap<_, _>>();
        for (player_id, correct) in &rendered {
            add_value_question(
                candidates,
                format!("dig:profile:{field}:{player_id}"),
                "dig",
                format!(
                    "What was {}'s {label} when this game began?",
                    player_label(*player_id)
                ),
                correct.clone(),
                rendered
                    .iter()
                    .filter(|(other_id, _)| *other_id != player_id)
                    .map(|(_, rendered)| rendered.clone()),
                format!(
                    "At the question snapshot, {}'s {label} was {correct}.",
                    player_label(*player_id)
                ),
                format!("player:{player_id}"),
                false,
                random,
            );
        }
    }

    let mut artifact_counts = BTreeMap::<DiscordId, i64>::new();
    let mut relic_counts = BTreeMap::<DiscordId, i64>::new();
    for row in &snapshot.dig_artifacts {
        if players.contains_key(&row.discord_id) {
            *artifact_counts.entry(row.discord_id).or_default() += 1;
            *relic_counts.entry(row.discord_id).or_default() += i64::from(row.is_relic);
        }
    }
    let achievement_counts = snapshot
        .dig_achievements
        .iter()
        .filter(|row| players.contains_key(&row.discord_id))
        .fold(BTreeMap::<DiscordId, i64>::new(), |mut counts, row| {
            *counts.entry(row.discord_id).or_default() += 1;
            counts
        });
    for (key, text, label, values) in [
        (
            "dig:most_artifacts",
            "Who has found the most recorded Dig artifacts?",
            "Dig artifacts",
            artifact_counts,
        ),
        (
            "dig:most_relics",
            "Who has found the most recorded Dig relics?",
            "Dig relics",
            relic_counts,
        ),
        (
            "dig:most_achievements",
            "Who has unlocked the most Dig achievements?",
            "Dig achievements",
            achievement_counts,
        ),
    ] {
        add_player_leader(
            candidates,
            players,
            key,
            "dig",
            text,
            values
                .into_iter()
                .map(|(player_id, value)| (player_id, value as f64))
                .collect(),
            Some(1.0),
            false,
            |value| value,
            move |name, value| format!("As of game start, {name} had {value:.0} {label}."),
            false,
            random,
        );
    }
    let cave_ins = snapshot
        .dig_actions
        .iter()
        .filter(|row| {
            let action = row.action_type.to_lowercase();
            players.contains_key(&row.actor_id)
                && (matches!(action.as_str(), "cave_in" | "cave-in" | "collapse")
                    || (matches!(action.as_str(), "dig" | "dig_action" | "event")
                        && row.depth_after.unwrap_or(0) < row.depth_before.unwrap_or(0)))
        })
        .fold(BTreeMap::<DiscordId, i64>::new(), |mut counts, row| {
            *counts.entry(row.actor_id).or_default() += 1;
            counts
        });
    add_player_leader(
        candidates,
        players,
        "dig:most_cave_ins",
        "dig",
        "Who has the most recorded Dig cave-ins or depth-losing collapses?",
        cave_ins
            .into_iter()
            .map(|(player_id, value)| (player_id, value as f64))
            .collect(),
        Some(1.0),
        false,
        |value| value,
        |name, value| {
            format!(
                "As of game start, {name} had {value:.0} logged cave-ins or Dig actions whose depth decreased."
            )
        },
        true,
        random,
    );
}

fn build_mafia_questions(
    candidates: &mut Vec<Candidate>,
    snapshot: &PlayerTriviaSnapshot,
    players: &BTreeMap<DiscordId, &PlayerTriviaPlayer>,
    random: &mut impl PlayerTriviaRandom,
) {
    let games = snapshot
        .mafia_games
        .iter()
        .filter(|game| {
            game.game_id > 0
                && game.phase.eq_ignore_ascii_case("resolved")
                && !game.status.eq_ignore_ascii_case("cancelled")
                && game.winner.as_deref().is_some_and(|winner| {
                    ["town", "mafia", "jester"]
                        .iter()
                        .any(|allowed| winner.eq_ignore_ascii_case(allowed))
                })
        })
        .map(|game| (game.game_id, game))
        .collect::<BTreeMap<_, _>>();
    let mut games_played = BTreeMap::<DiscordId, i64>::new();
    let mut wins = BTreeMap::<DiscordId, i64>::new();
    let mut role_counts = BTreeMap::<DiscordId, BTreeMap<String, i64>>::new();
    for row in &snapshot.mafia_players {
        let Some(game) = games.get(&row.game_id) else {
            continue;
        };
        if !players.contains_key(&row.discord_id) {
            continue;
        }
        *games_played.entry(row.discord_id).or_default() += 1;
        let role = row.role.to_uppercase();
        let winner = game.winner.as_deref().unwrap_or_default().to_uppercase();
        *wins.entry(row.discord_id).or_default() += i64::from(mafia_role_won(&role, &winner));
        if !role.is_empty() {
            *role_counts
                .entry(row.discord_id)
                .or_default()
                .entry(role)
                .or_default() += 1;
        }
    }
    add_player_leader(
        candidates,
        players,
        "mafia:most_games",
        "mafia",
        "Who has played the most resolved, non-cancelled Mafia games?",
        games_played
            .iter()
            .map(|(player_id, value)| (*player_id, *value as f64))
            .collect(),
        Some(3.0),
        false,
        |value| value,
        |name, value| {
            format!("As of game start, {name} had played {value:.0} resolved Mafia games.")
        },
        false,
        random,
    );
    add_player_leader(
        candidates,
        players,
        "mafia:most_wins",
        "mafia",
        "Among players with three resolved Mafia games, who has the most wins?",
        wins.iter()
            .filter(|(player_id, _)| games_played.get(player_id).copied().unwrap_or(0) >= 3)
            .map(|(player_id, value)| (*player_id, *value as f64))
            .collect(),
        Some(1.0),
        false,
        |value| value,
        |name, value| format!("As of game start, {name} had won {value:.0} resolved Mafia games."),
        false,
        random,
    );
    let mvp_counts = games
        .values()
        .filter_map(|game| game.mvp_id)
        .filter(|player_id| games_played.get(player_id).copied().unwrap_or(0) >= 3)
        .fold(
            BTreeMap::<DiscordId, i64>::new(),
            |mut counts, player_id| {
                *counts.entry(player_id).or_default() += 1;
                counts
            },
        );
    add_player_leader(
        candidates,
        players,
        "mafia:most_mvp_awards",
        "mafia",
        "Who has earned the most Mafia MVP awards?",
        mvp_counts
            .into_iter()
            .map(|(player_id, value)| (player_id, value as f64))
            .collect(),
        Some(1.0),
        false,
        |value| value,
        |name, value| format!("As of game start, {name} had earned {value:.0} Mafia MVP awards."),
        false,
        random,
    );

    let all_roles = role_counts
        .values()
        .flat_map(|counts| counts.keys().map(|role| humanize_code(role)))
        .collect::<BTreeSet<_>>();
    for (player_id, counts) in &role_counts {
        if games_played.get(player_id).copied().unwrap_or(0) < 3 {
            continue;
        }
        let mut ordered = counts.iter().collect::<Vec<_>>();
        ordered.sort_by(|left, right| right.1.cmp(left.1).then(left.0.cmp(right.0)));
        if ordered.is_empty()
            || ordered
                .get(1)
                .is_some_and(|runner_up| runner_up.1 == ordered[0].1)
        {
            continue;
        }
        let role = humanize_code(ordered[0].0);
        add_value_question(
            candidates,
            format!("mafia:most_common_role:{player_id}"),
            "mafia",
            format!(
                "Which Mafia role has {} received most often?",
                player_label(*player_id)
            ),
            role.clone(),
            all_roles.iter().filter(|other| *other != &role).cloned(),
            format!(
                "Across resolved, non-cancelled games, {} had the {role} role {} times.",
                player_label(*player_id),
                ordered[0].1
            ),
            format!("player:{player_id}"),
            false,
            random,
        );
    }

    let action_labels = [
        ("KILL", "Mafia kill choices"),
        ("VIG_KILL", "Vigilante kill choices"),
        ("SAVE", "Doctor save choices"),
        ("INVESTIGATE", "Detective investigations"),
        ("VOTE", "Mafia votes"),
    ];
    for (action_type, label) in action_labels {
        let counts = snapshot
            .mafia_actions
            .iter()
            .filter(|row| {
                games.contains_key(&row.game_id)
                    && row.action_type.eq_ignore_ascii_case(action_type)
                    && games_played.get(&row.actor_id).copied().unwrap_or(0) >= 3
            })
            .fold(BTreeMap::<DiscordId, i64>::new(), |mut counts, row| {
                *counts.entry(row.actor_id).or_default() += 1;
                counts
            });
        add_player_leader(
            candidates,
            players,
            &format!("mafia:most_{}_actions", action_type.to_lowercase()),
            "mafia",
            &format!("Who has made the most recorded {label} in resolved Mafia games?"),
            counts
                .into_iter()
                .map(|(player_id, value)| (player_id, value as f64))
                .collect(),
            Some(1.0),
            false,
            |value| value,
            move |name, value| {
                format!("As of game start, {name} had made {value:.0} recorded {label}.")
            },
            false,
            random,
        );
    }
}

fn mafia_role_won(role: &str, winner: &str) -> bool {
    match winner {
        "TOWN" => matches!(role, "TOWNIE" | "DOCTOR" | "DETECTIVE" | "VIGILANTE"),
        "MAFIA" => role == "MAFIA",
        "JESTER" => role == "JESTER",
        _ => false,
    }
}

fn build_trivia_history_questions(
    candidates: &mut Vec<Candidate>,
    snapshot: &PlayerTriviaSnapshot,
    players: &BTreeMap<DiscordId, &PlayerTriviaPlayer>,
    random: &mut impl PlayerTriviaRandom,
) {
    let mut sessions = BTreeMap::<DiscordId, i64>::new();
    let mut earned = BTreeMap::<DiscordId, i64>::new();
    let mut streak = BTreeMap::<DiscordId, i64>::new();
    for row in &snapshot.trivia_sessions {
        if !players.contains_key(&row.discord_id) {
            continue;
        }
        *sessions.entry(row.discord_id).or_default() += 1;
        *earned.entry(row.discord_id).or_default() += row.jc_earned;
        streak
            .entry(row.discord_id)
            .and_modify(|best| *best = (*best).max(row.streak.max(0)))
            .or_insert(row.streak.max(0));
    }
    for (key, text, values, minimum) in [
        (
            "trivia:most_sessions",
            "Who has played the most recorded trivia sessions?",
            sessions,
            2.0,
        ),
        (
            "trivia:most_jc_earned",
            "Who has earned the most total JC from recorded trivia sessions?",
            earned,
            1.0,
        ),
        (
            "trivia:best_streak",
            "Who has the longest recorded trivia answer streak?",
            streak,
            2.0,
        ),
    ] {
        add_player_leader(
            candidates,
            players,
            key,
            "trivia",
            text,
            values
                .into_iter()
                .map(|(player_id, value)| (player_id, value as f64))
                .collect(),
            Some(minimum),
            false,
            |value| value,
            |name, value| format!("As of game start, {name}'s recorded total was {value:.0}."),
            false,
            random,
        );
    }
}

fn build_disbursement_questions(
    candidates: &mut Vec<Candidate>,
    snapshot: &PlayerTriviaSnapshot,
    players: &BTreeMap<DiscordId, &PlayerTriviaPlayer>,
    random: &mut impl PlayerTriviaRandom,
) {
    let rows = snapshot
        .disbursement_votes
        .iter()
        .filter(|row| {
            players.contains_key(&row.discord_id)
                && row.finalized_at.is_some_and(|timestamp| timestamp > 0)
                && !row.proposal_outcome.trim().is_empty()
        })
        .collect::<Vec<_>>();
    let mut ballot_counts = BTreeMap::<DiscordId, i64>::new();
    let mut methods = BTreeMap::<DiscordId, BTreeMap<String, i64>>::new();
    let mut all_methods = BTreeSet::new();
    let mut outcomes = BTreeMap::<String, i64>::new();
    for row in rows {
        *ballot_counts.entry(row.discord_id).or_default() += 1;
        let method = humanize_code(&row.vote_method);
        if !method.is_empty() {
            all_methods.insert(method.clone());
            *methods
                .entry(row.discord_id)
                .or_default()
                .entry(method)
                .or_default() += 1;
        }
        let outcome = humanize_code(&row.proposal_outcome);
        if !outcome.is_empty() {
            *outcomes.entry(outcome).or_default() += 1;
        }
    }
    add_player_leader(
        candidates,
        players,
        "disburse:most_finalized_ballots",
        "disbursement",
        "Who has cast the most ballots in finalized nonprofit disbursements?",
        ballot_counts
            .into_iter()
            .map(|(player_id, value)| (player_id, value as f64))
            .collect(),
        Some(2.0),
        false,
        |value| value,
        |name, value| {
            format!("As of game start, {name} had cast {value:.0} finalized disbursement ballots.")
        },
        false,
        random,
    );
    for (player_id, player_methods) in methods {
        if player_methods.values().sum::<i64>() < 3 {
            continue;
        }
        let mut ordered = player_methods.iter().collect::<Vec<_>>();
        ordered.sort_by(|left, right| right.1.cmp(left.1).then(left.0.cmp(right.0)));
        if ordered
            .get(1)
            .is_some_and(|runner_up| runner_up.1 == ordered[0].1)
        {
            continue;
        }
        let (method, votes) = ordered[0];
        add_value_question(
            candidates,
            format!("disburse:most_common_vote:{player_id}"),
            "disbursement",
            format!(
                "Which finalized disbursement method has {} voted for most often?",
                player_label(player_id)
            ),
            method.clone(),
            all_methods.iter().filter(|other| *other != method).cloned(),
            format!(
                "As of game start, {} had cast {votes} finalized ballots for {method}.",
                player_label(player_id)
            ),
            format!("player:{player_id}"),
            false,
            random,
        );
    }
    if outcomes.len() >= 4 {
        let mut ordered = outcomes.iter().collect::<Vec<_>>();
        ordered.sort_by(|left, right| right.1.cmp(left.1).then(left.0.cmp(right.0)));
        if ordered
            .get(1)
            .is_none_or(|runner_up| runner_up.1 < ordered[0].1)
        {
            let (outcome, proposals) = ordered[0];
            add_value_question(
                candidates,
                "disburse:most_common_final_outcome".to_owned(),
                "disbursement",
                "Which outcome has occurred most often in finalized nonprofit disbursements?"
                    .to_owned(),
                outcome.clone(),
                outcomes.keys().filter(|other| *other != outcome).cloned(),
                format!(
                    "Across finalized disbursements, {outcome} was recorded {proposals} times."
                ),
                format!("disbursement-outcome:{}", outcome.to_lowercase()),
                false,
                random,
            );
        }
    }
}

#[cfg(test)]
#[path = "player_trivia_service/tests.rs"]
mod tests;
