//! Deterministic, side-effect-free team and pool balancing.
//!
//! This is a faithful Rust implementation of the Python `BalancedShuffler`
//! scoring contract.  It deliberately owns no persistence, Discord, or random
//! system state: callers supply player snapshots, constraints, wait metadata,
//! and (when sampling is required) a seed.  Request-local caches are discarded
//! after every public shuffle call.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::player::{OPENSKILL_DISPLAY_SCALE, Player};
use crate::team::{
    ROLES, Team, TeamError, calculate_off_role_value, compute_optimal_role_assignments,
};
use crate::team_balancing::{
    ADJUSTED_VALUE_DIFF_WEIGHT, role_matchup_delta_from_values, role_parity_delta_from_values,
};

/// Low-priority shoppers contribute half-strength penalties as actors.
pub const LOW_PRIORITY_EFFECTIVENESS: f64 = 0.5;
/// Avoids aimed at a low-priority player are twice as effective.
pub const LOW_PRIORITY_AVOID_TARGET_EFFECTIVENESS: f64 = 2.0;
/// Match goodness penalty for every selected low-priority player.
pub const LOW_PRIORITY_GOODNESS_PENALTY: f64 = 660.0;
/// Goodness bonus for grouping low-priority players on one team.
pub const LOW_PRIORITY_TEAM_GROUPING_BONUS: f64 = 100.0;

/// Canonical five-position role assignment.
pub type RoleAssignment = [String; 5];

/// Directed request that two players should not be put on the same team.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SoftAvoid {
    pub avoider_discord_id: i64,
    pub avoided_discord_id: i64,
}

/// Directed request that two players should remain on the same team.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageDeal {
    pub buyer_discord_id: i64,
    pub partner_discord_id: i64,
}

/// Failures returned by public shuffle operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShufflerError {
    IncorrectPlayerCount { expected: usize, actual: usize },
    TooFewPoolPlayers { minimum: usize, actual: usize },
    NoMatchupEvaluated,
    Team(TeamError),
}

impl Display for ShufflerError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncorrectPlayerCount { expected, actual } => {
                write!(formatter, "need exactly {expected} players, got {actual}")
            }
            Self::TooFewPoolPlayers { minimum, actual } => {
                write!(formatter, "need at least {minimum} players, got {actual}")
            }
            Self::NoMatchupEvaluated => formatter.write_str("shuffler produced no matchup"),
            Self::Team(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for ShufflerError {}

impl From<TeamError> for ShufflerError {
    fn from(value: TeamError) -> Self {
        Self::Team(value)
    }
}

/// Injectable immutable role-assignment cache.
///
/// Production uses a process-wide cache.  Tests can inject a recording or
/// empty provider without changing the optimizer itself.
pub trait RoleAssignmentProvider: Send + Sync {
    fn get(&self, player_roles_key: &[Vec<String>]) -> Arc<Vec<RoleAssignment>>;
}

#[derive(Default)]
struct GlobalRoleAssignmentProvider;

type RoleKey = Vec<Vec<String>>;

fn global_role_cache() -> &'static Mutex<HashMap<RoleKey, Arc<Vec<RoleAssignment>>>> {
    static CACHE: OnceLock<Mutex<HashMap<RoleKey, Arc<Vec<RoleAssignment>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

impl RoleAssignmentProvider for GlobalRoleAssignmentProvider {
    fn get(&self, player_roles_key: &[Vec<String>]) -> Arc<Vec<RoleAssignment>> {
        let key = player_roles_key.to_vec();
        let mut cache = global_role_cache()
            .lock()
            .expect("global role-assignment cache mutex was poisoned");
        if let Some(assignments) = cache.get(&key) {
            return Arc::clone(assignments);
        }

        let assignments = Arc::new(
            compute_optimal_role_assignments(player_roles_key)
                .into_iter()
                .filter_map(|assignment| assignment.try_into().ok())
                .collect(),
        );
        cache.insert(key, Arc::clone(&assignments));
        assignments
    }
}

#[derive(Default)]
struct DiagnosticCounters {
    role_cache_requests: AtomicUsize,
    rd_priority_calls: AtomicUsize,
    fixed_splits_evaluated: AtomicUsize,
    pool_matchups_evaluated: AtomicUsize,
    pool_signatures_built: AtomicUsize,
    draft_pool_scores_evaluated: AtomicUsize,
    role_metrics_built: AtomicUsize,
    player_value_reads: AtomicUsize,
    team_summaries_built: AtomicUsize,
    unconstrained_score_calls: AtomicUsize,
    constrained_score_calls: AtomicUsize,
    constraint_penalty_calls: AtomicUsize,
    branch_matchups_evaluated: AtomicUsize,
    branch_player_selections_pruned: AtomicUsize,
    branch_team_splits_pruned: AtomicUsize,
    fixed_team1_keys: Mutex<Vec<Vec<usize>>>,
    team_summary_keys: Mutex<Vec<Vec<usize>>>,
}

/// Read-only instrumentation snapshot used to pin optimization behavior.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShufflerDiagnostics {
    pub role_cache_requests: usize,
    pub rd_priority_calls: usize,
    pub fixed_splits_evaluated: usize,
    pub pool_matchups_evaluated: usize,
    pub pool_signatures_built: usize,
    pub draft_pool_scores_evaluated: usize,
    pub role_metrics_built: usize,
    pub player_value_reads: usize,
    pub team_summaries_built: usize,
    pub unconstrained_score_calls: usize,
    pub constrained_score_calls: usize,
    pub constraint_penalty_calls: usize,
    pub branch_matchups_evaluated: usize,
    pub branch_player_selections_pruned: usize,
    pub branch_team_splits_pruned: usize,
    pub fixed_team1_keys: Vec<Vec<usize>>,
    pub team_summary_keys: Vec<Vec<usize>>,
}

impl DiagnosticCounters {
    fn snapshot(&self) -> ShufflerDiagnostics {
        let load = |counter: &AtomicUsize| counter.load(AtomicOrdering::Relaxed);
        ShufflerDiagnostics {
            role_cache_requests: load(&self.role_cache_requests),
            rd_priority_calls: load(&self.rd_priority_calls),
            fixed_splits_evaluated: load(&self.fixed_splits_evaluated),
            pool_matchups_evaluated: load(&self.pool_matchups_evaluated),
            pool_signatures_built: load(&self.pool_signatures_built),
            draft_pool_scores_evaluated: load(&self.draft_pool_scores_evaluated),
            role_metrics_built: load(&self.role_metrics_built),
            player_value_reads: load(&self.player_value_reads),
            team_summaries_built: load(&self.team_summaries_built),
            unconstrained_score_calls: load(&self.unconstrained_score_calls),
            constrained_score_calls: load(&self.constrained_score_calls),
            constraint_penalty_calls: load(&self.constraint_penalty_calls),
            branch_matchups_evaluated: load(&self.branch_matchups_evaluated),
            branch_player_selections_pruned: load(&self.branch_player_selections_pruned),
            branch_team_splits_pruned: load(&self.branch_team_splits_pruned),
            fixed_team1_keys: self
                .fixed_team1_keys
                .lock()
                .expect("diagnostics mutex was poisoned")
                .clone(),
            team_summary_keys: self
                .team_summary_keys
                .lock()
                .expect("diagnostics mutex was poisoned")
                .clone(),
        }
    }

    fn reset(&self) {
        for counter in [
            &self.role_cache_requests,
            &self.rd_priority_calls,
            &self.fixed_splits_evaluated,
            &self.pool_matchups_evaluated,
            &self.pool_signatures_built,
            &self.draft_pool_scores_evaluated,
            &self.role_metrics_built,
            &self.player_value_reads,
            &self.team_summaries_built,
            &self.unconstrained_score_calls,
            &self.constrained_score_calls,
            &self.constraint_penalty_calls,
            &self.branch_matchups_evaluated,
            &self.branch_player_selections_pruned,
            &self.branch_team_splits_pruned,
        ] {
            counter.store(0, AtomicOrdering::Relaxed);
        }
        self.fixed_team1_keys
            .lock()
            .expect("diagnostics mutex was poisoned")
            .clear();
        self.team_summary_keys
            .lock()
            .expect("diagnostics mutex was poisoned")
            .clear();
    }
}

/// Avoid/deal/low-priority constraints for one shuffle.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShuffleConstraints<'a> {
    pub avoids: Option<&'a [SoftAvoid]>,
    pub deals: Option<&'a [PackageDeal]>,
    pub low_priority_ids: Option<&'a HashSet<i64>>,
}

/// Pool-wide inputs whose values do not belong in the shuffler configuration.
#[derive(Clone, Copy, Debug)]
pub struct PoolOptions<'a> {
    pub exclusion_counts: Option<&'a HashMap<String, usize>>,
    pub recent_match_names: Option<&'a HashSet<String>>,
    pub constraints: ShuffleConstraints<'a>,
    pub lobby_wait_minutes: Option<&'a HashMap<i64, i64>>,
    /// Deterministic seed used only when a pool is too large to enumerate.
    pub sampling_seed: u64,
}

impl Default for PoolOptions<'_> {
    fn default() -> Self {
        Self {
            exclusion_counts: None,
            recent_match_names: None,
            constraints: ShuffleConstraints::default(),
            lobby_wait_minutes: None,
            sampling_seed: 0xCA4A_5A11,
        }
    }
}

/// Search controls retained for differential and pruning tests.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BranchSearchOptions {
    pub use_split_role_bound: bool,
    /// Replaces the greedy numeric upper bound while retaining its valid teams.
    pub initial_upper_bound: Option<f64>,
}

impl Default for BranchSearchOptions {
    fn default() -> Self {
        Self {
            use_split_role_bound: true,
            initial_upper_bound: None,
        }
    }
}

/// Canonical, order-independent pool result signature.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PoolSignature(pub Vec<String>, pub Vec<String>, pub Vec<String>);

/// Numeric score override for deterministic pool-selection instrumentation.
#[derive(Clone, Debug, PartialEq)]
pub struct PoolScoreOverride {
    pub total_score: f64,
    pub value_diff: f64,
    pub total_off_roles: usize,
    pub signature: Option<PoolSignature>,
}

/// Loggable components for a fully evaluated pool matchup.
#[derive(Clone, Debug, PartialEq)]
pub struct PoolLogEntry {
    pub total_score: f64,
    pub value_diff: f64,
    pub off_role_penalty: f64,
    pub parity_penalty: f64,
    pub exclusion_penalty: f64,
    pub team1_value: f64,
    pub team2_value: f64,
    pub team1_off_roles: usize,
    pub team2_off_roles: usize,
    pub excluded_names: Vec<String>,
}

/// One materialized split inside a pool selection.
#[derive(Clone, Debug, PartialEq)]
pub struct PoolMatchup {
    pub team1: Team,
    pub team2: Team,
    pub total_score: f64,
    pub value_diff: f64,
    pub total_off_roles: usize,
    pub log_entry: PoolLogEntry,
}

/// A successful pool shuffle.
#[derive(Clone, Debug, PartialEq)]
pub struct PoolResult {
    pub team1: Team,
    pub team2: Team,
    pub excluded: Vec<Player>,
}

/// The Python Draft command's eight-player non-captain pool result.
/// Captains are scored separately from the selected pool and are therefore
/// intentionally not repeated in either player vector.
#[derive(Clone, Debug, PartialEq)]
pub struct DraftPoolResult {
    pub selected_players: Vec<Player>,
    pub excluded_players: Vec<Player>,
    pub pool_score: f64,
}

/// A greedy upper-bound result used by the 14-player search.
#[derive(Clone, Debug, PartialEq)]
pub struct GreedyResult {
    pub team1: Team,
    pub team2: Team,
    pub excluded: Vec<Player>,
    pub score: f64,
}

/// Balanced team-selection and role-assignment policy.
pub struct BalancedShuffler {
    pub use_glicko: bool,
    pub consider_roles: bool,
    pub use_openskill: bool,
    pub use_jopacoin: bool,
    pub off_role_multiplier: f64,
    pub off_role_flat_value_penalty: f64,
    pub off_role_flat_penalty: f64,
    pub role_matchup_delta_weight: f64,
    pub exclusion_penalty_weight: f64,
    pub rd_priority_weight: f64,
    pub recent_match_penalty_weight: f64,
    pub soft_avoid_penalty: f64,
    pub package_deal_penalty: f64,
    pub package_deal_split_penalty: f64,
    pub rating_spread_divisor: f64,
    pub region_split: bool,
    pub region_split_penalty: f64,
    role_assignment_provider: Arc<dyn RoleAssignmentProvider>,
    diagnostics: Arc<DiagnosticCounters>,
    /// Uniformly selects among equally-scored ten-player matchups, mirroring
    /// Python's `random.choice(best_matchups)`. Injectable so tests stay
    /// deterministic; the default draws real entropy.
    tie_breaker: Arc<dyn Fn(usize) -> usize + Send + Sync>,
}

impl Clone for BalancedShuffler {
    fn clone(&self) -> Self {
        Self {
            use_glicko: self.use_glicko,
            consider_roles: self.consider_roles,
            use_openskill: self.use_openskill,
            use_jopacoin: self.use_jopacoin,
            off_role_multiplier: self.off_role_multiplier,
            off_role_flat_value_penalty: self.off_role_flat_value_penalty,
            off_role_flat_penalty: self.off_role_flat_penalty,
            role_matchup_delta_weight: self.role_matchup_delta_weight,
            exclusion_penalty_weight: self.exclusion_penalty_weight,
            rd_priority_weight: self.rd_priority_weight,
            recent_match_penalty_weight: self.recent_match_penalty_weight,
            soft_avoid_penalty: self.soft_avoid_penalty,
            package_deal_penalty: self.package_deal_penalty,
            package_deal_split_penalty: self.package_deal_split_penalty,
            rating_spread_divisor: self.rating_spread_divisor,
            region_split: self.region_split,
            region_split_penalty: self.region_split_penalty,
            role_assignment_provider: Arc::clone(&self.role_assignment_provider),
            diagnostics: Arc::new(DiagnosticCounters::default()),
            tie_breaker: Arc::clone(&self.tie_breaker),
        }
    }
}

impl Default for BalancedShuffler {
    fn default() -> Self {
        Self {
            use_glicko: true,
            consider_roles: true,
            use_openskill: false,
            use_jopacoin: false,
            off_role_multiplier: 0.95,
            off_role_flat_value_penalty: 100.0,
            off_role_flat_penalty: 670.0,
            role_matchup_delta_weight: 0.18,
            exclusion_penalty_weight: 80.0,
            rd_priority_weight: 0.2,
            recent_match_penalty_weight: 230.0,
            soft_avoid_penalty: 160.0,
            package_deal_penalty: 90.0,
            package_deal_split_penalty: 90.0,
            rating_spread_divisor: 10.0,
            region_split: false,
            region_split_penalty: 500.0,
            role_assignment_provider: Arc::new(GlobalRoleAssignmentProvider),
            diagnostics: Arc::new(DiagnosticCounters::default()),
            tie_breaker: Arc::new(|count| {
                if count <= 1 {
                    0
                } else {
                    fastrand::usize(..count)
                }
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct RoleAssignmentMetrics {
    role_assignments: RoleAssignment,
    team_value: f64,
    off_role_count: usize,
    role_values: [f64; 5],
}

#[derive(Clone, Debug)]
struct TeamRoleMetricsSummary {
    metrics: Arc<Vec<RoleAssignmentMetrics>>,
    min_team_value: f64,
    max_team_value: f64,
    min_off_role_count: usize,
    team_values_are_finite: bool,
}

#[derive(Default)]
struct ScoringContext {
    player_values: HashMap<usize, f64>,
    team_metrics: HashMap<(Vec<usize>, usize), Arc<Vec<RoleAssignmentMetrics>>>,
    team_summaries: HashMap<(Vec<usize>, usize), TeamRoleMetricsSummary>,
    assigned_metrics: HashMap<(Vec<usize>, RoleAssignment), RoleAssignmentMetrics>,
}

struct ScoredTeams {
    team1: Team,
    team2: Team,
    score: f64,
    team1_metrics: RoleAssignmentMetrics,
    team2_metrics: RoleAssignmentMetrics,
}

#[derive(Clone, Copy)]
struct RoleScorePolicy {
    rd_priority: Option<f64>,
    value_diff_weight: f64,
}

struct PoolSelection<'a> {
    selected_indices: Vec<usize>,
    selected_players: Vec<&'a Player>,
    excluded_players: Vec<&'a Player>,
    excluded_names: Vec<String>,
    recent_penalty: f64,
    exclusion_penalty: f64,
    combo_penalty: f64,
    rd_priority: f64,
    preselection_score: f64,
}

impl BalancedShuffler {
    /// Replace the process-wide role cache with an injected provider.
    #[must_use]
    pub fn with_role_assignment_provider(
        mut self,
        provider: Arc<dyn RoleAssignmentProvider>,
    ) -> Self {
        self.role_assignment_provider = provider;
        self
    }

    /// Replace the tied-matchup selector. Tests inject `|_| 0` to pin the
    /// pre-randomization first-split behavior.
    #[must_use]
    pub fn with_tie_breaker(
        mut self,
        tie_breaker: Arc<dyn Fn(usize) -> usize + Send + Sync>,
    ) -> Self {
        self.tie_breaker = tie_breaker;
        self
    }

    /// Return accumulated diagnostics without changing search behavior.
    #[must_use]
    pub fn diagnostics(&self) -> ShufflerDiagnostics {
        self.diagnostics.snapshot()
    }

    /// Reset instrumentation while retaining configuration and injected caches.
    pub fn reset_diagnostics(&self) {
        self.diagnostics.reset();
    }

    /// Read role assignments through the configured immutable cache.
    #[must_use]
    pub fn get_cached_role_assignments(&self, players: &[Player]) -> Arc<Vec<RoleAssignment>> {
        let refs: Vec<&Player> = players.iter().collect();
        self.cached_role_assignments(&refs)
    }

    fn cached_role_assignments(&self, players: &[&Player]) -> Arc<Vec<RoleAssignment>> {
        self.diagnostics
            .role_cache_requests
            .fetch_add(1, AtomicOrdering::Relaxed);
        let key: Vec<Vec<String>> = players
            .iter()
            .map(|player| player.preferred_roles.clone().unwrap_or_default())
            .collect();
        self.role_assignment_provider.get(&key)
    }

    /// `(max - min) / rating_spread_divisor`, or zero for fewer than two values.
    #[must_use]
    pub fn calculate_rating_spread_penalty(&self, player_values: &[f64]) -> f64 {
        if player_values.len() < 2 {
            return 0.0;
        }
        let minimum = player_values.iter().copied().fold(f64::INFINITY, f64::min);
        let maximum = player_values
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        (maximum - minimum) / self.rating_spread_divisor
    }

    /// Average five-player team total divided by ten.
    #[must_use]
    pub fn calculate_lobby_rating_bonus(player_values: &[f64]) -> f64 {
        player_values.iter().sum::<f64>() / 20.0
    }

    /// Total whole minutes waited by selected players with Discord IDs.
    #[must_use]
    pub fn calculate_lobby_wait_bonus(
        players: &[&Player],
        lobby_wait_minutes: Option<&HashMap<i64, i64>>,
    ) -> f64 {
        let Some(wait_minutes) = lobby_wait_minutes else {
            return 0.0;
        };
        players
            .iter()
            .filter_map(|player| player.discord_id)
            .map(|discord_id| wait_minutes.get(&discord_id).copied().unwrap_or_default())
            .sum::<i64>() as f64
    }

    /// Active rating system uncertainty bonus.
    #[must_use]
    pub fn calculate_rd_priority(&self, players: &[&Player]) -> f64 {
        self.diagnostics
            .rd_priority_calls
            .fetch_add(1, AtomicOrdering::Relaxed);
        if self.rd_priority_weight <= 0.0 {
            return 0.0;
        }
        let total = if self.use_openskill {
            players
                .iter()
                .map(|player| player.os_sigma.unwrap_or_default() * OPENSKILL_DISPLAY_SCALE)
                .sum::<f64>()
        } else {
            players
                .iter()
                .map(|player| player.glicko_rd.unwrap_or_default())
                .sum::<f64>()
        };
        total * self.rd_priority_weight
    }

    /// Add directed avoid penalties for pairs placed together.
    #[must_use]
    pub fn calculate_soft_avoid_penalty(
        &self,
        team1_ids: &HashSet<i64>,
        team2_ids: &HashSet<i64>,
        avoids: Option<&[SoftAvoid]>,
        low_priority_ids: Option<&HashSet<i64>>,
    ) -> f64 {
        self.diagnostics
            .constraint_penalty_calls
            .fetch_add(1, AtomicOrdering::Relaxed);
        avoids.map_or(0.0, |items| {
            items
                .iter()
                .filter_map(|avoid| {
                    let together = (team1_ids.contains(&avoid.avoider_discord_id)
                        && team1_ids.contains(&avoid.avoided_discord_id))
                        || (team2_ids.contains(&avoid.avoider_discord_id)
                            && team2_ids.contains(&avoid.avoided_discord_id));
                    if !together {
                        return None;
                    }
                    let mut effectiveness = if low_priority_ids
                        .is_some_and(|ids| ids.contains(&avoid.avoider_discord_id))
                    {
                        LOW_PRIORITY_EFFECTIVENESS
                    } else {
                        1.0
                    };
                    if low_priority_ids.is_some_and(|ids| ids.contains(&avoid.avoided_discord_id)) {
                        effectiveness *= LOW_PRIORITY_AVOID_TARGET_EFFECTIVENESS;
                    }
                    Some(self.soft_avoid_penalty * effectiveness)
                })
                .sum()
        })
    }

    /// Add directed package-deal penalties for pairs placed apart.
    #[must_use]
    pub fn calculate_package_deal_penalty(
        &self,
        team1_ids: &HashSet<i64>,
        team2_ids: &HashSet<i64>,
        deals: Option<&[PackageDeal]>,
        low_priority_ids: Option<&HashSet<i64>>,
    ) -> f64 {
        self.diagnostics
            .constraint_penalty_calls
            .fetch_add(1, AtomicOrdering::Relaxed);
        deals.map_or(0.0, |items| {
            items
                .iter()
                .filter_map(|deal| {
                    let opposite = (team1_ids.contains(&deal.buyer_discord_id)
                        && team2_ids.contains(&deal.partner_discord_id))
                        || (team2_ids.contains(&deal.buyer_discord_id)
                            && team1_ids.contains(&deal.partner_discord_id));
                    if !opposite {
                        return None;
                    }
                    let effectiveness = if low_priority_ids
                        .is_some_and(|ids| ids.contains(&deal.buyer_discord_id))
                    {
                        LOW_PRIORITY_EFFECTIVENESS
                    } else {
                        1.0
                    };
                    Some(self.package_deal_penalty * effectiveness)
                })
                .sum()
        })
    }

    /// Penalize package-deal pairs split between selected and excluded players.
    #[must_use]
    pub fn calculate_package_deal_split_penalty(
        &self,
        selected_ids: &HashSet<i64>,
        excluded_ids: &HashSet<i64>,
        deals: Option<&[PackageDeal]>,
        low_priority_ids: Option<&HashSet<i64>>,
    ) -> f64 {
        deals.map_or(0.0, |items| {
            items
                .iter()
                .filter_map(|deal| {
                    let split = (selected_ids.contains(&deal.buyer_discord_id)
                        && excluded_ids.contains(&deal.partner_discord_id))
                        || (excluded_ids.contains(&deal.buyer_discord_id)
                            && selected_ids.contains(&deal.partner_discord_id));
                    if !split {
                        return None;
                    }
                    let effectiveness = if low_priority_ids
                        .is_some_and(|ids| ids.contains(&deal.buyer_discord_id))
                    {
                        LOW_PRIORITY_EFFECTIVENESS
                    } else {
                        1.0
                    };
                    Some(self.package_deal_split_penalty * effectiveness)
                })
                .sum()
        })
    }

    /// Penalize each selected low-priority Discord ID by
    /// [`LOW_PRIORITY_GOODNESS_PENALTY`].
    #[must_use]
    pub fn calculate_low_priority_penalty(
        selected_ids: &HashSet<i64>,
        low_priority_ids: Option<&HashSet<i64>>,
    ) -> f64 {
        low_priority_ids.map_or(0.0, |ids| {
            selected_ids.intersection(ids).count() as f64 * LOW_PRIORITY_GOODNESS_PENALTY
        })
    }

    /// Reward placing active low-priority players together, applying the
    /// grouping adjustment once per matchup rather than once per player.
    #[must_use]
    pub fn calculate_low_priority_team_adjustment(
        team1_ids: &HashSet<i64>,
        team2_ids: &HashSet<i64>,
        low_priority_ids: Option<&HashSet<i64>>,
    ) -> f64 {
        let Some(low_priority_ids) = low_priority_ids else {
            return 0.0;
        };
        Self::low_priority_team_adjustment_from_counts(
            team1_ids.intersection(low_priority_ids).count(),
            team2_ids.intersection(low_priority_ids).count(),
        )
    }

    #[must_use]
    fn low_priority_team_adjustment_from_counts(team1_count: usize, team2_count: usize) -> f64 {
        let largest_group = team1_count.max(team2_count);
        -(largest_group.saturating_sub(1) as f64) * LOW_PRIORITY_TEAM_GROUPING_BONUS
    }

    #[must_use]
    fn low_priority_team_adjustment_lower_bound(
        selected_ids: &HashSet<i64>,
        low_priority_ids: Option<&HashSet<i64>>,
    ) -> f64 {
        let Some(low_priority_ids) = low_priority_ids else {
            return 0.0;
        };
        let largest_possible_group =
            Team::TEAM_SIZE.min(selected_ids.intersection(low_priority_ids).count());
        Self::low_priority_team_adjustment_from_counts(largest_possible_group, 0)
    }

    fn player_value(&self, player: &Player, context: &mut ScoringContext) -> f64 {
        let key = player_key(player);
        if let Some(value) = context.player_values.get(&key) {
            return *value;
        }
        self.diagnostics
            .player_value_reads
            .fetch_add(1, AtomicOrdering::Relaxed);
        let value = player.get_value(self.use_glicko, self.use_openskill, self.use_jopacoin);
        context.player_values.insert(key, value);
        value
    }

    fn role_assignment_metrics(
        &self,
        players: &[&Player],
        roles: &RoleAssignment,
        values: &[f64],
    ) -> RoleAssignmentMetrics {
        self.diagnostics
            .role_metrics_built
            .fetch_add(1, AtomicOrdering::Relaxed);
        let mut team_value = 0.0;
        let mut off_role_count = 0;
        let mut role_values = [0.0; 5];
        for ((player, assigned_role), base_value) in
            players.iter().zip(roles).zip(values.iter().copied())
        {
            let on_role = player
                .preferred_roles
                .as_ref()
                .is_some_and(|preferred| preferred.contains(assigned_role));
            // Unclamped: jopacoin balances go negative and must stay ordered.
            let base_value =
                crate::player::apply_role_factor(base_value, player.role_factor_for(assigned_role));
            let effective_value = if on_role {
                base_value
            } else {
                off_role_count += 1;
                calculate_off_role_value(
                    base_value,
                    self.off_role_multiplier,
                    self.off_role_flat_value_penalty,
                )
            };
            team_value += effective_value;
            if let Ok(index) = assigned_role.parse::<usize>()
                && (1..=5).contains(&index)
            {
                role_values[index - 1] = effective_value;
            }
        }
        RoleAssignmentMetrics {
            role_assignments: roles.clone(),
            team_value,
            off_role_count,
            role_values,
        }
    }

    fn team_role_metrics(
        &self,
        players: &[&Player],
        max_assignments: usize,
        context: &mut ScoringContext,
    ) -> Arc<Vec<RoleAssignmentMetrics>> {
        let identities = player_keys(players);
        let cache_key = (identities.clone(), max_assignments);
        if let Some(metrics) = context.team_metrics.get(&cache_key) {
            return Arc::clone(metrics);
        }

        let assignments = self.cached_role_assignments(players);
        let values: Vec<f64> = players
            .iter()
            .map(|player| self.player_value(player, context))
            .collect();
        let metrics = Arc::new(
            assignments
                .iter()
                .take(max_assignments)
                .map(|roles| self.role_assignment_metrics(players, roles, &values))
                .collect::<Vec<_>>(),
        );
        for metric in metrics.iter() {
            context.assigned_metrics.insert(
                (identities.clone(), metric.role_assignments.clone()),
                metric.clone(),
            );
        }
        context.team_metrics.insert(cache_key, Arc::clone(&metrics));
        metrics
    }

    fn team_role_metrics_summary(
        &self,
        players: &[&Player],
        max_assignments: usize,
        context: &mut ScoringContext,
    ) -> TeamRoleMetricsSummary {
        let identities = player_keys(players);
        let key = (identities.clone(), max_assignments);
        if let Some(summary) = context.team_summaries.get(&key) {
            return summary.clone();
        }

        self.diagnostics
            .team_summaries_built
            .fetch_add(1, AtomicOrdering::Relaxed);
        self.diagnostics
            .team_summary_keys
            .lock()
            .expect("diagnostics mutex was poisoned")
            .push(identities);
        let metrics = self.team_role_metrics(players, max_assignments, context);
        let finite =
            !metrics.is_empty() && metrics.iter().all(|metric| metric.team_value.is_finite());
        let summary = if finite {
            TeamRoleMetricsSummary {
                min_team_value: metrics
                    .iter()
                    .map(|metric| metric.team_value)
                    .fold(f64::INFINITY, f64::min),
                max_team_value: metrics
                    .iter()
                    .map(|metric| metric.team_value)
                    .fold(f64::NEG_INFINITY, f64::max),
                min_off_role_count: metrics
                    .iter()
                    .map(|metric| metric.off_role_count)
                    .min()
                    .unwrap_or_default(),
                metrics,
                team_values_are_finite: true,
            }
        } else {
            TeamRoleMetricsSummary {
                metrics,
                min_team_value: 0.0,
                max_team_value: 0.0,
                min_off_role_count: 0,
                team_values_are_finite: false,
            }
        };
        context.team_summaries.insert(key, summary.clone());
        summary
    }

    fn assigned_metrics(
        &self,
        players: &[&Player],
        roles: &RoleAssignment,
        context: &mut ScoringContext,
    ) -> RoleAssignmentMetrics {
        let identities = player_keys(players);
        let key = (identities.clone(), roles.clone());
        if let Some(metrics) = context.assigned_metrics.get(&key) {
            return metrics.clone();
        }
        let values: Vec<f64> = players
            .iter()
            .map(|player| self.player_value(player, context))
            .collect();
        let metrics = self.role_assignment_metrics(players, roles, &values);
        context.assigned_metrics.insert(key, metrics.clone());
        metrics
    }

    fn score_unconstrained_metrics(
        &self,
        team1_metrics: &[RoleAssignmentMetrics],
        team2_metrics: &[RoleAssignmentMetrics],
        rd_priority: f64,
        value_diff_weight: f64,
    ) -> (Option<RoleAssignment>, Option<RoleAssignment>, f64) {
        self.diagnostics
            .unconstrained_score_calls
            .fetch_add(1, AtomicOrdering::Relaxed);
        let mut best_roles1 = None;
        let mut best_roles2 = None;
        let mut best_score = f64::INFINITY;
        for metric1 in team1_metrics {
            for metric2 in team2_metrics {
                let value_diff = (metric1.team_value - metric2.team_value).abs();
                let off_role_penalty = (metric1.off_role_count + metric2.off_role_count) as f64
                    * self.off_role_flat_penalty;
                let matchup =
                    role_matchup_delta_from_values(&metric1.role_values, &metric2.role_values);
                let parity =
                    role_parity_delta_from_values(&metric1.role_values, &metric2.role_values);
                let score = value_diff * value_diff_weight
                    + off_role_penalty
                    + (matchup + parity) * self.role_matchup_delta_weight
                    - rd_priority;
                if score < best_score {
                    best_score = score;
                    best_roles1 = Some(metric1.role_assignments.clone());
                    best_roles2 = Some(metric2.role_assignments.clone());
                }
            }
        }
        (best_roles1, best_roles2, best_score)
    }

    fn score_role_refs(
        &self,
        team1_players: &[&Player],
        team2_players: &[&Player],
        max_assignments: usize,
        constraints: ShuffleConstraints<'_>,
        context: &mut ScoringContext,
        policy: RoleScorePolicy,
    ) -> (Option<RoleAssignment>, Option<RoleAssignment>, f64) {
        let rd_priority = policy.rd_priority.unwrap_or_else(|| {
            let mut selected = team1_players.to_vec();
            selected.extend_from_slice(team2_players);
            self.calculate_rd_priority(&selected)
        });
        let team1_metrics = self.team_role_metrics(team1_players, max_assignments, context);
        let team2_metrics = self.team_role_metrics(team2_players, max_assignments, context);
        let constrained = constraints.avoids.is_some_and(|items| !items.is_empty())
            || constraints.deals.is_some_and(|items| !items.is_empty())
            || (self.region_split && self.region_split_penalty > 0.0);
        let team1_ids = discord_ids(team1_players);
        let team2_ids = discord_ids(team2_players);
        let low_priority_team_adjustment = Self::calculate_low_priority_team_adjustment(
            &team1_ids,
            &team2_ids,
            constraints.low_priority_ids,
        );
        if !constrained {
            let (roles1, roles2, score) = self.score_unconstrained_metrics(
                &team1_metrics,
                &team2_metrics,
                rd_priority,
                policy.value_diff_weight,
            );
            return (roles1, roles2, score + low_priority_team_adjustment);
        }

        self.diagnostics
            .constrained_score_calls
            .fetch_add(1, AtomicOrdering::Relaxed);
        let avoid_penalty = self.calculate_soft_avoid_penalty(
            &team1_ids,
            &team2_ids,
            constraints.avoids,
            constraints.low_priority_ids,
        );
        let deal_penalty = self.calculate_package_deal_penalty(
            &team1_ids,
            &team2_ids,
            constraints.deals,
            constraints.low_priority_ids,
        );
        self.diagnostics
            .constraint_penalty_calls
            .fetch_add(1, AtomicOrdering::Relaxed);
        let region_penalty = self.calculate_region_split_penalty(team1_players, team2_players);

        let mut best_roles1 = None;
        let mut best_roles2 = None;
        let mut best_score = f64::INFINITY;
        for metric1 in team1_metrics.iter() {
            for metric2 in team2_metrics.iter() {
                let value_diff = (metric1.team_value - metric2.team_value).abs();
                let off_role_penalty = (metric1.off_role_count + metric2.off_role_count) as f64
                    * self.off_role_flat_penalty;
                let matchup =
                    role_matchup_delta_from_values(&metric1.role_values, &metric2.role_values);
                let parity =
                    role_parity_delta_from_values(&metric1.role_values, &metric2.role_values);
                let score = value_diff * policy.value_diff_weight
                    + off_role_penalty
                    + (matchup + parity) * self.role_matchup_delta_weight
                    - rd_priority
                    + avoid_penalty
                    + deal_penalty
                    + low_priority_team_adjustment
                    + region_penalty;
                if score < best_score {
                    best_score = score;
                    best_roles1 = Some(metric1.role_assignments.clone());
                    best_roles2 = Some(metric2.role_assignments.clone());
                }
            }
        }
        (best_roles1, best_roles2, best_score)
    }

    /// Score a fixed five-versus-five matchup without constructing teams.
    pub fn score_role_assignments_for_matchup(
        &self,
        team1_players: &[Player],
        team2_players: &[Player],
        max_assignments: usize,
        constraints: ShuffleConstraints<'_>,
    ) -> Result<(Option<RoleAssignment>, Option<RoleAssignment>, f64), ShufflerError> {
        ensure_five(team1_players)?;
        ensure_five(team2_players)?;
        let refs1: Vec<&Player> = team1_players.iter().collect();
        let refs2: Vec<&Player> = team2_players.iter().collect();
        Ok(self.score_role_refs(
            &refs1,
            &refs2,
            max_assignments,
            constraints,
            &mut ScoringContext::default(),
            RoleScorePolicy {
                rd_priority: None,
                value_diff_weight: ADJUSTED_VALUE_DIFF_WEIGHT,
            },
        ))
    }

    fn optimize_refs(
        &self,
        team1_players: &[&Player],
        team2_players: &[&Player],
        max_assignments: usize,
        constraints: ShuffleConstraints<'_>,
        context: &mut ScoringContext,
        rd_priority: Option<f64>,
    ) -> Result<ScoredTeams, ShufflerError> {
        let (roles1, roles2, score) = self.score_role_refs(
            team1_players,
            team2_players,
            max_assignments,
            constraints,
            context,
            RoleScorePolicy {
                rd_priority,
                value_diff_weight: ADJUSTED_VALUE_DIFF_WEIGHT,
            },
        );
        let mut team1 = Team::new(
            team1_players
                .iter()
                .map(|player| (*player).clone())
                .collect(),
            roles1.as_ref().map(role_strings),
        )?;
        let mut team2 = Team::new(
            team2_players
                .iter()
                .map(|player| (*player).clone())
                .collect(),
            roles2.as_ref().map(role_strings),
        )?;
        if roles1.is_none() {
            team1.ensure_role_assignments();
        }
        if roles2.is_none() {
            team2.ensure_role_assignments();
        }
        let assigned1 = team_role_array(&team1);
        let assigned2 = team_role_array(&team2);
        let team1_metrics = self.assigned_metrics(team1_players, &assigned1, context);
        let team2_metrics = self.assigned_metrics(team2_players, &assigned2, context);
        Ok(ScoredTeams {
            team1,
            team2,
            score,
            team1_metrics,
            team2_metrics,
        })
    }

    /// Materialize the best roles for a fixed matchup.
    pub fn optimize_role_assignments_for_matchup(
        &self,
        team1_players: &[Player],
        team2_players: &[Player],
        max_assignments: usize,
        constraints: ShuffleConstraints<'_>,
    ) -> Result<(Team, Team, f64), ShufflerError> {
        ensure_five(team1_players)?;
        ensure_five(team2_players)?;
        let refs1: Vec<&Player> = team1_players.iter().collect();
        let refs2: Vec<&Player> = team2_players.iter().collect();
        let scored = self.optimize_refs(
            &refs1,
            &refs2,
            max_assignments,
            constraints,
            &mut ScoringContext::default(),
            None,
        )?;
        Ok((scored.team1, scored.team2, scored.score))
    }

    /// Shuffle exactly ten players using all 126 unordered splits.
    pub fn shuffle(&self, players: &[Player]) -> Result<(Team, Team), ShufflerError> {
        self.shuffle_with_constraints(players, ShuffleConstraints::default())
    }

    /// Shuffle ten players with avoid/deal/low-priority constraints.
    pub fn shuffle_with_constraints(
        &self,
        players: &[Player],
        constraints: ShuffleConstraints<'_>,
    ) -> Result<(Team, Team), ShufflerError> {
        if players.len() != 10 {
            return Err(ShufflerError::IncorrectPlayerCount {
                expected: 10,
                actual: players.len(),
            });
        }
        let refs: Vec<&Player> = players.iter().collect();
        let splits = unique_team_splits();
        let mut context = ScoringContext::default();
        let rd_priority = self.calculate_rd_priority(&refs);
        // Python collects every matchup tied at the best score and picks one
        // with `random.choice`, so equally-balanced pools do not produce the
        // same teams on every shuffle. Mirror that instead of keeping the
        // first minimal split.
        let mut best_matchups: Vec<(Team, Team)> = Vec::new();
        let mut best_score = f64::INFINITY;
        for (team1_indices, team2_indices) in splits {
            let team1_refs = pick_refs(&refs, &team1_indices);
            let team2_refs = pick_refs(&refs, &team2_indices);
            self.diagnostics
                .fixed_splits_evaluated
                .fetch_add(1, AtomicOrdering::Relaxed);
            self.diagnostics
                .fixed_team1_keys
                .lock()
                .expect("diagnostics mutex was poisoned")
                .push(player_keys(&team1_refs));
            let scored = self.optimize_refs(
                &team1_refs,
                &team2_refs,
                20,
                constraints,
                &mut context,
                Some(rd_priority),
            )?;
            if scored.score < best_score {
                best_score = scored.score;
                best_matchups.clear();
                best_matchups.push((scored.team1, scored.team2));
            } else if scored.score == best_score {
                best_matchups.push((scored.team1, scored.team2));
            }
        }
        if best_matchups.is_empty() {
            return Err(ShufflerError::NoMatchupEvaluated);
        }
        let chosen = (self.tie_breaker)(best_matchups.len()).min(best_matchups.len() - 1);
        Ok(best_matchups.swap_remove(chosen))
    }

    /// Score a Draft command's eight non-captain pool without materializing
    /// discarded teams.  Captains distinguish the two sides, so all 70
    /// `C(8, 4)` candidate splits are evaluated, matching Python's
    /// `_score_draft_pool` helper and its three-role-assignment budget.
    pub fn score_draft_pool(
        &self,
        captain_a: &Player,
        captain_b: &Player,
        pool: &[Player],
    ) -> Result<f64, ShufflerError> {
        if pool.len() != 8 {
            return Err(ShufflerError::IncorrectPlayerCount {
                expected: 8,
                actual: pool.len(),
            });
        }
        self.score_draft_pool_refs(captain_a, captain_b, pool, &mut ScoringContext::default())
    }

    /// Select exactly eight non-captains for Draft.  The routing and penalty
    /// semantics are intentionally the Python counterparts: eight candidates
    /// score directly, nine through twelve enumerate every pool, and larger
    /// pools use [`Self::select_draft_pool_beam`].
    pub fn select_draft_pool(
        &self,
        captain_a: &Player,
        captain_b: &Player,
        candidates: &[Player],
        exclusion_counts: Option<&HashMap<String, usize>>,
        recent_match_names: Option<&HashSet<String>>,
    ) -> Result<DraftPoolResult, ShufflerError> {
        if candidates.len() < 8 {
            return Err(ShufflerError::TooFewPoolPlayers {
                minimum: 8,
                actual: candidates.len(),
            });
        }
        if candidates.len() == 8 {
            let mut context = ScoringContext::default();
            let best_split_score =
                self.score_draft_pool_refs(captain_a, captain_b, candidates, &mut context)?;
            let recent_penalty = draft_recent_penalty(
                candidates,
                recent_match_names,
                self.recent_match_penalty_weight,
            );
            return Ok(DraftPoolResult {
                selected_players: candidates.to_vec(),
                excluded_players: Vec::new(),
                pool_score: best_split_score + recent_penalty,
            });
        }
        if candidates.len() > 12 {
            return self.select_draft_pool_beam(
                captain_a,
                captain_b,
                candidates,
                exclusion_counts,
                recent_match_names,
            );
        }

        let mut context = ScoringContext::default();
        let mut best_score = f64::INFINITY;
        let mut best_selected = Vec::new();
        let mut best_excluded = Vec::new();
        for selected_indices in combinations(candidates.len(), 8) {
            let selected = selected_indices
                .iter()
                .map(|index| &candidates[*index])
                .collect::<Vec<_>>();
            let selected_set: HashSet<usize> = selected_indices.iter().copied().collect();
            let excluded = candidates
                .iter()
                .enumerate()
                .filter_map(|(index, player)| (!selected_set.contains(&index)).then_some(player))
                .collect::<Vec<_>>();
            let best_split_score =
                self.score_draft_pool_ref_slice(captain_a, captain_b, &selected, &mut context)?;
            let exclusion_penalty = draft_exclusion_penalty(
                excluded.iter().copied(),
                exclusion_counts,
                self.exclusion_penalty_weight,
            );
            let recent_penalty = draft_recent_penalty(
                selected.iter().copied(),
                recent_match_names,
                self.recent_match_penalty_weight,
            );
            let score = best_split_score + exclusion_penalty + recent_penalty;
            if score < best_score {
                best_score = score;
                best_selected = selected.into_iter().cloned().collect();
                best_excluded = excluded.into_iter().cloned().collect();
            }
        }
        if best_selected.is_empty() {
            return Err(ShufflerError::NoMatchupEvaluated);
        }
        Ok(DraftPoolResult {
            selected_players: best_selected,
            excluded_players: best_excluded,
            pool_score: best_score,
        })
    }

    /// Beam-search counterpart of Python's Draft pool selector.  It keeps the
    /// same 35/8/150/5 controls, stable candidate order, single-player swaps,
    /// score memoization, and early exits.
    pub fn select_draft_pool_beam(
        &self,
        captain_a: &Player,
        captain_b: &Player,
        candidates: &[Player],
        exclusion_counts: Option<&HashMap<String, usize>>,
        recent_match_names: Option<&HashSet<String>>,
    ) -> Result<DraftPoolResult, ShufflerError> {
        if candidates.len() < 8 {
            return Err(ShufflerError::TooFewPoolPlayers {
                minimum: 8,
                actual: candidates.len(),
            });
        }
        let mut context = ScoringContext::default();
        let mut sorted_indices: Vec<usize> = (0..candidates.len()).collect();
        sorted_indices.sort_by(|left, right| {
            self.player_value(&candidates[*right], &mut context)
                .total_cmp(&self.player_value(&candidates[*left], &mut context))
        });
        let initial_pool = sorted_indices[..8].to_vec();
        let initial_excluded = sorted_indices[8..].to_vec();
        let initial_score = self.score_draft_pool_selection(
            captain_a,
            captain_b,
            candidates,
            &initial_pool,
            &initial_excluded,
            exclusion_counts,
            recent_match_names,
            &mut context,
        )?;
        const EARLY_EXIT_THRESHOLD: f64 = 150.0;
        if initial_score < EARLY_EXIT_THRESHOLD {
            return Ok(draft_pool_result_from_indices(
                candidates,
                &initial_pool,
                &initial_excluded,
                initial_score,
            ));
        }

        type BeamEntry = (Vec<usize>, Vec<usize>, f64);
        let mut current_beams: Vec<BeamEntry> = vec![(
            initial_pool.clone(),
            initial_excluded.clone(),
            initial_score,
        )];
        let mut best_pool = initial_pool;
        let mut best_excluded = initial_excluded;
        let mut best_score = initial_score;
        let mut score_cache: HashMap<Vec<String>, f64> = HashMap::new();
        score_cache.insert(draft_pool_key(candidates, &best_pool), initial_score);
        let mut stagnant_iterations = 0;

        for _iteration in 0..35 {
            let mut neighbors: Vec<BeamEntry> = Vec::new();
            let mut seen_pools: HashSet<Vec<String>> = HashSet::new();
            for (pool, _ignored_excluded, _score) in &current_beams {
                let pool_set: HashSet<usize> = pool.iter().copied().collect();
                let outside = (0..candidates.len())
                    .filter(|index| !pool_set.contains(index))
                    .collect::<Vec<_>>();
                for remove_at in 0..pool.len() {
                    for in_index in &outside {
                        let mut new_pool = pool.clone();
                        new_pool[remove_at] = *in_index;
                        let key = draft_pool_key(candidates, &new_pool);
                        if !seen_pools.insert(key.clone()) {
                            continue;
                        }
                        let new_excluded = (0..candidates.len())
                            .filter(|index| !new_pool.contains(index))
                            .collect::<Vec<_>>();
                        let score = if let Some(score) = score_cache.get(&key) {
                            *score
                        } else {
                            let score = self.score_draft_pool_selection(
                                captain_a,
                                captain_b,
                                candidates,
                                &new_pool,
                                &new_excluded,
                                exclusion_counts,
                                recent_match_names,
                                &mut context,
                            )?;
                            score_cache.insert(key, score);
                            score
                        };
                        if score < EARLY_EXIT_THRESHOLD {
                            return Ok(draft_pool_result_from_indices(
                                candidates,
                                &new_pool,
                                &new_excluded,
                                score,
                            ));
                        }
                        neighbors.push((new_pool, new_excluded, score));
                    }
                }
            }
            if neighbors.is_empty() {
                break;
            }
            neighbors.sort_by(|left, right| left.2.total_cmp(&right.2));
            neighbors.truncate(8);
            current_beams = neighbors;
            if current_beams[0].2 < best_score {
                best_pool = current_beams[0].0.clone();
                best_excluded = current_beams[0].1.clone();
                best_score = current_beams[0].2;
                stagnant_iterations = 0;
            } else {
                stagnant_iterations += 1;
                if stagnant_iterations >= 5 {
                    break;
                }
            }
        }
        Ok(draft_pool_result_from_indices(
            candidates,
            &best_pool,
            &best_excluded,
            best_score,
        ))
    }

    fn score_draft_pool_refs(
        &self,
        captain_a: &Player,
        captain_b: &Player,
        pool: &[Player],
        context: &mut ScoringContext,
    ) -> Result<f64, ShufflerError> {
        if pool.len() != 8 {
            return Err(ShufflerError::IncorrectPlayerCount {
                expected: 8,
                actual: pool.len(),
            });
        }
        let pool_refs = pool.iter().collect::<Vec<_>>();
        self.score_draft_pool_ref_slice(captain_a, captain_b, &pool_refs, context)
    }

    fn score_draft_pool_ref_slice(
        &self,
        captain_a: &Player,
        captain_b: &Player,
        pool_refs: &[&Player],
        context: &mut ScoringContext,
    ) -> Result<f64, ShufflerError> {
        if pool_refs.len() != 8 {
            return Err(ShufflerError::IncorrectPlayerCount {
                expected: 8,
                actual: pool_refs.len(),
            });
        }
        let mut all_players = vec![captain_a, captain_b];
        all_players.extend(pool_refs.iter().copied());
        let rd_priority = self.calculate_rd_priority(&all_players);
        let mut best_score = f64::INFINITY;
        for (team_a_indices, team_b_indices) in draft_pool_splits() {
            let mut team_a = vec![captain_a];
            team_a.extend(team_a_indices.iter().map(|index| pool_refs[*index]));
            let mut team_b = vec![captain_b];
            team_b.extend(team_b_indices.iter().map(|index| pool_refs[*index]));
            let (_, _, score) = self.score_role_refs(
                &team_a,
                &team_b,
                3,
                ShuffleConstraints::default(),
                context,
                RoleScorePolicy {
                    rd_priority: Some(rd_priority),
                    // Draft keeps its historical unweighted value-difference score.
                    value_diff_weight: 1.0,
                },
            );
            if score < best_score {
                best_score = score;
            }
        }
        Ok(best_score)
    }

    #[allow(clippy::too_many_arguments)]
    fn score_draft_pool_selection(
        &self,
        captain_a: &Player,
        captain_b: &Player,
        candidates: &[Player],
        selected_indices: &[usize],
        excluded_indices: &[usize],
        exclusion_counts: Option<&HashMap<String, usize>>,
        recent_match_names: Option<&HashSet<String>>,
        context: &mut ScoringContext,
    ) -> Result<f64, ShufflerError> {
        self.diagnostics
            .draft_pool_scores_evaluated
            .fetch_add(1, AtomicOrdering::Relaxed);
        let selected = selected_indices
            .iter()
            .map(|index| &candidates[*index])
            .collect::<Vec<_>>();
        let best_split_score =
            self.score_draft_pool_ref_slice(captain_a, captain_b, &selected, context)?;
        let excluded = excluded_indices
            .iter()
            .map(|index| &candidates[*index])
            .collect::<Vec<_>>();
        Ok(best_split_score
            + draft_exclusion_penalty(
                excluded.iter().copied(),
                exclusion_counts,
                self.exclusion_penalty_weight,
            )
            + draft_recent_penalty(
                selected.iter().copied(),
                recent_match_names,
                self.recent_match_penalty_weight,
            ))
    }

    /// Greedy snake-draft upper bound used by branch-and-bound.
    pub fn greedy_shuffle(
        &self,
        players: &[Player],
        options: PoolOptions<'_>,
    ) -> Result<GreedyResult, ShufflerError> {
        if players.len() < 10 {
            return Err(ShufflerError::TooFewPoolPlayers {
                minimum: 10,
                actual: players.len(),
            });
        }
        let refs: Vec<&Player> = players.iter().collect();
        self.greedy_refs(&refs, options, &mut ScoringContext::default())
    }

    fn greedy_refs(
        &self,
        players: &[&Player],
        options: PoolOptions<'_>,
        context: &mut ScoringContext,
    ) -> Result<GreedyResult, ShufflerError> {
        let exclusion_counts = options.exclusion_counts;
        let recent_names = options.recent_match_names;
        let mut sorted = players.to_vec();
        sorted.sort_by(|left, right| {
            self.player_value(right, context)
                .total_cmp(&self.player_value(left, context))
        });

        let excluded_count = players.len().saturating_sub(10);
        let (selected, excluded) = if excluded_count > 0 {
            let mut order = sorted.clone();
            order.sort_by(|left, right| {
                let effective = |player: &&Player| {
                    let base = exclusion_counts
                        .and_then(|counts| counts.get(&player.name))
                        .copied()
                        .unwrap_or_default() as f64;
                    if recent_names.is_some_and(|names| names.contains(&player.name))
                        && self.exclusion_penalty_weight != 0.0
                    {
                        base - self.recent_match_penalty_weight / self.exclusion_penalty_weight
                    } else {
                        base
                    }
                };
                effective(left).total_cmp(&effective(right)).then_with(|| {
                    self.player_value(left, context)
                        .total_cmp(&self.player_value(right, context))
                })
            });
            let excluded = order[..excluded_count].to_vec();
            let excluded_keys: HashSet<usize> =
                excluded.iter().map(|player| player_key(player)).collect();
            let selected = sorted
                .into_iter()
                .filter(|player| !excluded_keys.contains(&player_key(player)))
                .collect();
            (selected, excluded)
        } else {
            (sorted, Vec::new())
        };

        let mut team1 = Vec::with_capacity(5);
        let mut team2 = Vec::with_capacity(5);
        for (index, player) in selected.iter().copied().enumerate() {
            let round = index / 2;
            let goes_team1 = if round % 2 == 0 {
                index % 2 == 0
            } else {
                index % 2 == 1
            };
            if goes_team1 {
                team1.push(player);
            } else {
                team2.push(player);
            }
        }
        let rd_priority = self.calculate_rd_priority(&selected);
        let scored = self.optimize_refs(
            &team1,
            &team2,
            3,
            options.constraints,
            context,
            Some(rd_priority),
        )?;

        let exclusion_penalty = excluded
            .iter()
            .map(|player| {
                exclusion_counts
                    .and_then(|counts| counts.get(&player.name))
                    .copied()
                    .unwrap_or_default() as f64
            })
            .sum::<f64>()
            * self.exclusion_penalty_weight;
        let selected_names: HashSet<&str> =
            selected.iter().map(|player| player.name.as_str()).collect();
        let recent_penalty = recent_names.map_or(0.0, |names| {
            selected_names
                .iter()
                .filter(|name| names.contains(**name))
                .count() as f64
                * self.recent_match_penalty_weight
        });
        let selected_ids = discord_ids(&selected);
        let excluded_ids = discord_ids(&excluded);
        let split_penalty = self.calculate_package_deal_split_penalty(
            &selected_ids,
            &excluded_ids,
            options.constraints.deals,
            options.constraints.low_priority_ids,
        );
        let low_priority_penalty = Self::calculate_low_priority_penalty(
            &selected_ids,
            options.constraints.low_priority_ids,
        );
        let values: Vec<f64> = selected
            .iter()
            .map(|player| self.player_value(player, context))
            .collect();
        let score = scored.score
            + exclusion_penalty
            + recent_penalty
            + split_penalty
            + low_priority_penalty
            + self.calculate_rating_spread_penalty(&values)
            - Self::calculate_lobby_rating_bonus(&values)
            - Self::calculate_lobby_wait_bonus(&selected, options.lobby_wait_minutes);
        Ok(GreedyResult {
            team1: scored.team1,
            team2: scored.team2,
            excluded: excluded.into_iter().cloned().collect(),
            score,
        })
    }

    /// Shuffle a pool.  Pools of 14 use branch-and-bound; larger pools use a
    /// deterministic bounded sample and 24-roster preselection.
    pub fn shuffle_from_pool(
        &self,
        players: &[Player],
        options: PoolOptions<'_>,
    ) -> Result<PoolResult, ShufflerError> {
        self.shuffle_from_pool_impl(players, options, &mut |_, _, _| None)
    }

    /// Pool shuffle with a deterministic numeric/signature override hook.
    ///
    /// Returning `None` evaluates the real optimizer.  Returning `Some` is
    /// useful for pinning hot-path comparison behavior independently of score
    /// arithmetic; production callers should use [`Self::shuffle_from_pool`].
    pub fn shuffle_from_pool_with_score_hook<F>(
        &self,
        players: &[Player],
        options: PoolOptions<'_>,
        hook: &mut F,
    ) -> Result<PoolResult, ShufflerError>
    where
        F: FnMut(usize, &[&Player], &[&Player]) -> Option<PoolScoreOverride>,
    {
        self.shuffle_from_pool_impl(players, options, hook)
    }

    fn shuffle_from_pool_impl<F>(
        &self,
        players: &[Player],
        options: PoolOptions<'_>,
        hook: &mut F,
    ) -> Result<PoolResult, ShufflerError>
    where
        F: FnMut(usize, &[&Player], &[&Player]) -> Option<PoolScoreOverride>,
    {
        if players.len() < 10 {
            return Err(ShufflerError::TooFewPoolPlayers {
                minimum: 10,
                actual: players.len(),
            });
        }
        if players.len() == 10 {
            let (team1, team2) = self.shuffle_with_constraints(players, options.constraints)?;
            return Ok(PoolResult {
                team1,
                team2,
                excluded: Vec::new(),
            });
        }
        if players.len() == 14 {
            return self.shuffle_branch_bound(players, options, BranchSearchOptions::default());
        }

        let refs: Vec<&Player> = players.iter().collect();
        let mut context = ScoringContext::default();
        let total_combinations = binomial(players.len(), 10);
        let selected_sources = if total_combinations > 2_500 {
            sample_combinations(players.len(), 10, 2_500, options.sampling_seed)
        } else {
            combinations(players.len(), 10)
        };

        let mut selections = Vec::with_capacity(selected_sources.len());
        for selected_indices in selected_sources {
            selections.push(self.build_pool_selection(
                &refs,
                selected_indices,
                options,
                &mut context,
            ));
        }
        if players.len() >= 15 {
            selections.sort_by(|left, right| {
                left.preselection_score
                    .total_cmp(&right.preselection_score)
                    .then_with(|| left.selected_indices.cmp(&right.selected_indices))
            });
            selections.truncate(24);
        }

        let mut best_matchup: Option<PoolMatchup> = None;
        let mut best_excluded: Vec<Player> = Vec::new();
        let mut best_signature: Option<PoolSignature> = None;
        let mut evaluation_index = 0;
        for selection in selections {
            for (team1_indices, team2_indices) in unique_team_splits() {
                let team1_refs = pick_refs(&selection.selected_players, &team1_indices);
                let team2_refs = pick_refs(&selection.selected_players, &team2_indices);
                self.diagnostics
                    .pool_matchups_evaluated
                    .fetch_add(1, AtomicOrdering::Relaxed);
                let override_score = hook(evaluation_index, &team1_refs, &team2_refs);
                evaluation_index += 1;
                let (matchup, signature_override) = if let Some(override_score) = override_score {
                    (
                        materialize_override_matchup(
                            &team1_refs,
                            &team2_refs,
                            &selection.excluded_names,
                            selection.exclusion_penalty,
                            &override_score,
                        )?,
                        override_score.signature,
                    )
                } else {
                    (
                        self.evaluate_pool_matchup_refs(
                            &team1_refs,
                            &team2_refs,
                            selection.combo_penalty,
                            selection.exclusion_penalty,
                            &selection.excluded_names,
                            selection.recent_penalty,
                            selection.rd_priority,
                            3,
                            options.constraints,
                            &mut context,
                        )?,
                        None,
                    )
                };

                let current_prefix = (
                    matchup.total_score,
                    matchup.value_diff,
                    matchup.total_off_roles,
                );
                let best_prefix = best_matchup
                    .as_ref()
                    .map_or((f64::INFINITY, f64::INFINITY, usize::MAX), |best| {
                        (best.total_score, best.value_diff, best.total_off_roles)
                    });
                if numeric_prefix_cmp(current_prefix, best_prefix) == Ordering::Greater {
                    continue;
                }
                self.diagnostics
                    .pool_signatures_built
                    .fetch_add(1, AtomicOrdering::Relaxed);
                let signature = signature_override.unwrap_or_else(|| {
                    pool_signature(&matchup.team1, &matchup.team2, &selection.excluded_names)
                });
                if !pool_candidate_is_better(
                    current_prefix,
                    &signature,
                    best_prefix,
                    best_signature.as_ref(),
                ) {
                    continue;
                }
                best_signature = Some(signature);
                best_excluded = selection
                    .excluded_players
                    .iter()
                    .map(|player| (*player).clone())
                    .collect();
                best_matchup = Some(matchup);
            }
        }

        let best = best_matchup.ok_or(ShufflerError::NoMatchupEvaluated)?;
        Ok(PoolResult {
            team1: best.team1,
            team2: best.team2,
            excluded: best_excluded,
        })
    }

    fn build_pool_selection<'a>(
        &self,
        players: &[&'a Player],
        selected_indices: Vec<usize>,
        options: PoolOptions<'_>,
        context: &mut ScoringContext,
    ) -> PoolSelection<'a> {
        let selected_set: HashSet<usize> = selected_indices.iter().copied().collect();
        let selected_players: Vec<&Player> = selected_indices
            .iter()
            .map(|index| players[*index])
            .collect();
        let excluded_players: Vec<&Player> = players
            .iter()
            .enumerate()
            .filter(|(index, _)| !selected_set.contains(index))
            .map(|(_, player)| *player)
            .collect();
        let excluded_names: Vec<String> = excluded_players
            .iter()
            .map(|player| player.name.clone())
            .collect();
        let selected_names: HashSet<&str> = selected_players
            .iter()
            .map(|player| player.name.as_str())
            .collect();
        let recent_penalty = options.recent_match_names.map_or(0.0, |recent| {
            selected_names
                .iter()
                .filter(|name| recent.contains(**name))
                .count() as f64
                * self.recent_match_penalty_weight
        });
        let exclusion_penalty = excluded_names
            .iter()
            .map(|name| {
                options
                    .exclusion_counts
                    .and_then(|counts| counts.get(name))
                    .copied()
                    .unwrap_or_default() as f64
            })
            .sum::<f64>()
            * self.exclusion_penalty_weight;
        let selected_ids = discord_ids(&selected_players);
        let excluded_ids = discord_ids(&excluded_players);
        let split_penalty = self.calculate_package_deal_split_penalty(
            &selected_ids,
            &excluded_ids,
            options.constraints.deals,
            options.constraints.low_priority_ids,
        );
        let low_priority_penalty = Self::calculate_low_priority_penalty(
            &selected_ids,
            options.constraints.low_priority_ids,
        );
        let values: Vec<f64> = selected_players
            .iter()
            .map(|player| self.player_value(player, context))
            .collect();
        let combo_penalty = exclusion_penalty
            + split_penalty
            + low_priority_penalty
            + self.calculate_rating_spread_penalty(&values)
            - Self::calculate_lobby_rating_bonus(&values)
            - Self::calculate_lobby_wait_bonus(&selected_players, options.lobby_wait_minutes);
        let rd_priority = self.calculate_rd_priority(&selected_players);
        let total_value: f64 = values.iter().sum();
        let split_value_diff = if values.iter().all(|value| value.is_finite()) {
            unique_team_splits()
                .into_iter()
                .map(|(team1, _)| {
                    let team1_value: f64 = team1.iter().map(|index| values[*index]).sum();
                    (total_value - 2.0 * team1_value).abs()
                })
                .fold(f64::INFINITY, f64::min)
        } else {
            0.0
        };
        let low_priority_team_lower_bound = Self::low_priority_team_adjustment_lower_bound(
            &selected_ids,
            options.constraints.low_priority_ids,
        );
        PoolSelection {
            selected_indices,
            selected_players,
            excluded_players,
            excluded_names,
            recent_penalty,
            exclusion_penalty,
            combo_penalty,
            rd_priority,
            preselection_score: combo_penalty + recent_penalty - rd_priority
                + split_value_diff * ADJUSTED_VALUE_DIFF_WEIGHT
                + low_priority_team_lower_bound,
        }
    }

    /// Fully score and materialize one pool split.
    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_pool_matchup(
        &self,
        team1_players: &[Player],
        team2_players: &[Player],
        combo_penalty: f64,
        exclusion_penalty: f64,
        excluded_names: &[String],
        recent_penalty: f64,
        rd_priority: f64,
        max_assignments: usize,
        constraints: ShuffleConstraints<'_>,
    ) -> Result<PoolMatchup, ShufflerError> {
        ensure_five(team1_players)?;
        ensure_five(team2_players)?;
        let refs1: Vec<&Player> = team1_players.iter().collect();
        let refs2: Vec<&Player> = team2_players.iter().collect();
        self.evaluate_pool_matchup_refs(
            &refs1,
            &refs2,
            combo_penalty,
            exclusion_penalty,
            excluded_names,
            recent_penalty,
            rd_priority,
            max_assignments,
            constraints,
            &mut ScoringContext::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_pool_matchup_refs(
        &self,
        team1_players: &[&Player],
        team2_players: &[&Player],
        combo_penalty: f64,
        exclusion_penalty: f64,
        excluded_names: &[String],
        recent_penalty: f64,
        rd_priority: f64,
        max_assignments: usize,
        constraints: ShuffleConstraints<'_>,
        context: &mut ScoringContext,
    ) -> Result<PoolMatchup, ShufflerError> {
        let scored = self.optimize_refs(
            team1_players,
            team2_players,
            max_assignments,
            constraints,
            context,
            Some(rd_priority),
        )?;
        let value_diff = (scored.team1_metrics.team_value - scored.team2_metrics.team_value).abs();
        let total_off_roles =
            scored.team1_metrics.off_role_count + scored.team2_metrics.off_role_count;
        let off_role_penalty = total_off_roles as f64 * self.off_role_flat_penalty;
        let matchup = role_matchup_delta_from_values(
            &scored.team1_metrics.role_values,
            &scored.team2_metrics.role_values,
        );
        let parity = role_parity_delta_from_values(
            &scored.team1_metrics.role_values,
            &scored.team2_metrics.role_values,
        );
        let parity_penalty = (matchup + parity) * self.role_matchup_delta_weight;
        let total_score = scored.score + combo_penalty + recent_penalty;
        Ok(PoolMatchup {
            team1: scored.team1,
            team2: scored.team2,
            total_score,
            value_diff,
            total_off_roles,
            log_entry: PoolLogEntry {
                total_score,
                value_diff,
                off_role_penalty,
                parity_penalty,
                exclusion_penalty,
                team1_value: scored.team1_metrics.team_value,
                team2_value: scored.team2_metrics.team_value,
                team1_off_roles: scored.team1_metrics.off_role_count,
                team2_off_roles: scored.team2_metrics.off_role_count,
                excluded_names: excluded_names.to_vec(),
            },
        })
    }

    /// Fourteen-player branch-and-bound search with an admissible split bound.
    pub fn shuffle_branch_bound(
        &self,
        players: &[Player],
        options: PoolOptions<'_>,
        search: BranchSearchOptions,
    ) -> Result<PoolResult, ShufflerError> {
        if players.len() != 14 {
            return Err(ShufflerError::IncorrectPlayerCount {
                expected: 14,
                actual: players.len(),
            });
        }
        let refs: Vec<&Player> = players.iter().collect();
        let mut context = ScoringContext::default();
        let greedy = self.greedy_refs(&refs, options, &mut context)?;
        let mut best_score = search.initial_upper_bound.unwrap_or(greedy.score);
        let mut best_result = PoolResult {
            team1: greedy.team1,
            team2: greedy.team2,
            excluded: greedy.excluded,
        };

        let values: Vec<f64> = refs
            .iter()
            .map(|player| self.player_value(player, &mut context))
            .collect();
        let avoids_nonempty = options
            .constraints
            .avoids
            .is_some_and(|items| !items.is_empty());
        let deals_nonempty = options
            .constraints
            .deals
            .is_some_and(|items| !items.is_empty());
        let low_priority_nonempty = options
            .constraints
            .low_priority_ids
            .is_some_and(|ids| !ids.is_empty());
        let base_terms_nonnegative = self.off_role_flat_penalty.is_finite()
            && self.off_role_flat_penalty >= 0.0
            && self.role_matchup_delta_weight.is_finite()
            && self.role_matchup_delta_weight >= 0.0
            && (!avoids_nonempty
                || (self.soft_avoid_penalty.is_finite() && self.soft_avoid_penalty >= 0.0))
            && (!deals_nonempty
                || (self.package_deal_penalty.is_finite() && self.package_deal_penalty >= 0.0));
        let recent_nonnegative =
            self.recent_match_penalty_weight.is_finite() && self.recent_match_penalty_weight >= 0.0;
        let use_split_bound = search.use_split_role_bound
            && base_terms_nonnegative
            && !avoids_nonempty
            && !deals_nonempty
            && (!self.region_split || self.region_split_penalty <= 0.0);

        let low_priority_player_mask = refs
            .iter()
            .enumerate()
            .filter(|(_, player)| {
                low_priority_nonempty
                    && player.discord_id.is_some_and(|id| {
                        options.constraints.low_priority_ids.unwrap().contains(&id)
                    })
            })
            .fold(0_usize, |mask, (index, _)| mask | (1_usize << index));

        for selected_indices in combinations(14, 10) {
            let selected = pick_refs_vec(&refs, &selected_indices);
            let selected_set: HashSet<usize> = selected_indices.iter().copied().collect();
            let selected_mask = selected_indices
                .iter()
                .fold(0_usize, |mask, index| mask | (1_usize << index));
            let excluded: Vec<&Player> = refs
                .iter()
                .enumerate()
                .filter(|(index, _)| !selected_set.contains(index))
                .map(|(_, player)| *player)
                .collect();
            let excluded_names: Vec<String> =
                excluded.iter().map(|player| player.name.clone()).collect();
            let exclusion_penalty = excluded_names
                .iter()
                .map(|name| {
                    options
                        .exclusion_counts
                        .and_then(|counts| counts.get(name))
                        .copied()
                        .unwrap_or_default() as f64
                })
                .sum::<f64>()
                * self.exclusion_penalty_weight;
            let selected_ids = discord_ids(&selected);
            let excluded_ids = discord_ids(&excluded);
            let split_penalty = self.calculate_package_deal_split_penalty(
                &selected_ids,
                &excluded_ids,
                options.constraints.deals,
                options.constraints.low_priority_ids,
            );
            let low_priority_penalty = Self::calculate_low_priority_penalty(
                &selected_ids,
                options.constraints.low_priority_ids,
            );
            let low_priority_team_lower_bound = Self::low_priority_team_adjustment_lower_bound(
                &selected_ids,
                options.constraints.low_priority_ids,
            );
            let selected_values: Vec<f64> = selected_indices
                .iter()
                .map(|index| values[*index])
                .collect();
            let combo_penalty = exclusion_penalty
                + split_penalty
                + low_priority_penalty
                + self.calculate_rating_spread_penalty(&selected_values)
                - Self::calculate_lobby_rating_bonus(&selected_values)
                - Self::calculate_lobby_wait_bonus(&selected, options.lobby_wait_minutes);
            let rd_priority = self.calculate_rd_priority(&selected);
            let combo_lower_bound = combo_penalty - rd_priority + low_priority_team_lower_bound;
            if base_terms_nonnegative && recent_nonnegative && combo_lower_bound >= best_score {
                self.diagnostics
                    .branch_player_selections_pruned
                    .fetch_add(1, AtomicOrdering::Relaxed);
                continue;
            }
            let selected_names: HashSet<&str> =
                selected.iter().map(|player| player.name.as_str()).collect();
            let recent_penalty = options.recent_match_names.map_or(0.0, |recent| {
                selected_names
                    .iter()
                    .filter(|name| recent.contains(**name))
                    .count() as f64
                    * self.recent_match_penalty_weight
            });
            let lower_bound = recent_penalty + combo_lower_bound;
            if base_terms_nonnegative && lower_bound >= best_score {
                self.diagnostics
                    .branch_team_splits_pruned
                    .fetch_add(126, AtomicOrdering::Relaxed);
                continue;
            }

            for (team1_indices, team2_indices) in unique_team_splits() {
                let team1_refs = pick_refs(&selected, &team1_indices);
                let team2_refs = pick_refs(&selected, &team2_indices);
                let team1_mask = team1_indices.iter().fold(0_usize, |mask, index| {
                    mask | (1_usize << selected_indices[*index])
                });
                let team2_mask = selected_mask ^ team1_mask;
                let low_priority_team_adjustment = Self::low_priority_team_adjustment_from_counts(
                    (team1_mask & low_priority_player_mask).count_ones() as usize,
                    (team2_mask & low_priority_player_mask).count_ones() as usize,
                );
                let (roles1, roles2, base_score) = if use_split_bound {
                    let summary1 = self.team_role_metrics_summary(&team1_refs, 3, &mut context);
                    let summary2 = self.team_role_metrics_summary(&team2_refs, 3, &mut context);
                    if summary1.team_values_are_finite && summary2.team_values_are_finite {
                        let min_value_diff = 0.0_f64
                            .max(summary1.min_team_value - summary2.max_team_value)
                            .max(summary2.min_team_value - summary1.max_team_value);
                        let min_off_role_penalty =
                            (summary1.min_off_role_count + summary2.min_off_role_count) as f64
                                * self.off_role_flat_penalty;
                        let split_lower_bound = min_value_diff * ADJUSTED_VALUE_DIFF_WEIGHT
                            + min_off_role_penalty
                            - rd_priority
                            + recent_penalty
                            + combo_penalty
                            + low_priority_team_adjustment;
                        if split_lower_bound.is_finite() && split_lower_bound >= best_score {
                            self.diagnostics
                                .branch_team_splits_pruned
                                .fetch_add(1, AtomicOrdering::Relaxed);
                            continue;
                        }
                    }
                    let (roles1, roles2, base_score) = self.score_unconstrained_metrics(
                        &summary1.metrics,
                        &summary2.metrics,
                        rd_priority,
                        ADJUSTED_VALUE_DIFF_WEIGHT,
                    );
                    (roles1, roles2, base_score + low_priority_team_adjustment)
                } else {
                    self.score_role_refs(
                        &team1_refs,
                        &team2_refs,
                        3,
                        options.constraints,
                        &mut context,
                        RoleScorePolicy {
                            rd_priority: Some(rd_priority),
                            value_diff_weight: ADJUSTED_VALUE_DIFF_WEIGHT,
                        },
                    )
                };
                self.diagnostics
                    .branch_matchups_evaluated
                    .fetch_add(1, AtomicOrdering::Relaxed);
                let total_score = base_score + recent_penalty + combo_penalty;
                if total_score >= best_score {
                    continue;
                }
                let (Some(roles1), Some(roles2)) = (roles1, roles2) else {
                    continue;
                };
                best_score = total_score;
                best_result = PoolResult {
                    team1: Team::new(
                        team1_refs.iter().map(|player| (*player).clone()).collect(),
                        Some(role_strings(&roles1)),
                    )?,
                    team2: Team::new(
                        team2_refs.iter().map(|player| (*player).clone()).collect(),
                        Some(role_strings(&roles2)),
                    )?,
                    excluded: excluded.iter().map(|player| (*player).clone()).collect(),
                };
            }
        }
        Ok(best_result)
    }

    fn calculate_region_split_penalty(
        &self,
        team1_players: &[&Player],
        team2_players: &[&Player],
    ) -> f64 {
        if !self.region_split || self.region_split_penalty <= 0.0 {
            return 0.0;
        }
        region_split_mismatches(team1_players, team2_players) as f64 * self.region_split_penalty
    }
}

fn ensure_five(players: &[Player]) -> Result<(), ShufflerError> {
    if players.len() == 5 {
        Ok(())
    } else {
        Err(ShufflerError::IncorrectPlayerCount {
            expected: 5,
            actual: players.len(),
        })
    }
}

fn player_key(player: &Player) -> usize {
    std::ptr::from_ref(player).addr()
}

fn player_keys(players: &[&Player]) -> Vec<usize> {
    players.iter().map(|player| player_key(player)).collect()
}

fn discord_ids(players: &[&Player]) -> HashSet<i64> {
    players
        .iter()
        .filter_map(|player| player.discord_id.filter(|discord_id| *discord_id != 0))
        .collect()
}

fn role_strings(roles: &RoleAssignment) -> Vec<String> {
    roles.to_vec()
}

fn default_role_assignment() -> RoleAssignment {
    ROLES.map(str::to_owned)
}

fn team_role_array(team: &Team) -> RoleAssignment {
    team.role_assignments
        .as_ref()
        .and_then(|roles| roles.clone().try_into().ok())
        .unwrap_or_else(default_role_assignment)
}

fn pick_refs<'a>(players: &[&'a Player], indices: &[usize; 5]) -> Vec<&'a Player> {
    indices.iter().map(|index| players[*index]).collect()
}

fn pick_refs_vec<'a>(players: &[&'a Player], indices: &[usize]) -> Vec<&'a Player> {
    indices.iter().map(|index| players[*index]).collect()
}

fn unique_team_splits() -> Vec<([usize; 5], [usize; 5])> {
    combinations_from(1, 10, 4)
        .into_iter()
        .map(|others| {
            let team1 = [0, others[0], others[1], others[2], others[3]];
            let team1_set: HashSet<usize> = team1.into_iter().collect();
            let team2: [usize; 5] = (0..10)
                .filter(|index| !team1_set.contains(index))
                .collect::<Vec<_>>()
                .try_into()
                .expect("a five-player complement");
            (team1, team2)
        })
        .collect()
}

fn draft_pool_splits() -> &'static [(Vec<usize>, Vec<usize>)] {
    static SPLITS: OnceLock<Vec<(Vec<usize>, Vec<usize>)>> = OnceLock::new();
    SPLITS.get_or_init(|| {
        combinations(8, 4)
            .into_iter()
            .map(|team_a| {
                let team_a_set: HashSet<usize> = team_a.iter().copied().collect();
                let team_b = (0..8)
                    .filter(|index| !team_a_set.contains(index))
                    .collect::<Vec<_>>();
                (team_a, team_b)
            })
            .collect()
    })
}

fn draft_pool_key(players: &[Player], selected_indices: &[usize]) -> Vec<String> {
    let mut names = selected_indices
        .iter()
        .map(|index| players[*index].name.clone())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn draft_pool_result_from_indices(
    candidates: &[Player],
    selected_indices: &[usize],
    excluded_indices: &[usize],
    pool_score: f64,
) -> DraftPoolResult {
    DraftPoolResult {
        selected_players: selected_indices
            .iter()
            .map(|index| candidates[*index].clone())
            .collect(),
        excluded_players: excluded_indices
            .iter()
            .map(|index| candidates[*index].clone())
            .collect(),
        pool_score,
    }
}

fn draft_exclusion_penalty<'a, I>(
    players: I,
    exclusion_counts: Option<&HashMap<String, usize>>,
    weight: f64,
) -> f64
where
    I: IntoIterator<Item = &'a Player>,
{
    players
        .into_iter()
        .map(|player| {
            exclusion_counts
                .and_then(|counts| counts.get(&player.name))
                .copied()
                .unwrap_or_default() as f64
        })
        .sum::<f64>()
        * weight
}

fn draft_recent_penalty<'a, I>(
    players: I,
    recent_match_names: Option<&HashSet<String>>,
    weight: f64,
) -> f64
where
    I: IntoIterator<Item = &'a Player>,
{
    let Some(recent_match_names) = recent_match_names else {
        return 0.0;
    };
    players
        .into_iter()
        .filter(|player| recent_match_names.contains(&player.name))
        .count() as f64
        * weight
}

fn combinations(n: usize, k: usize) -> Vec<Vec<usize>> {
    combinations_from(0, n, k)
}

fn combinations_from(start: usize, end: usize, k: usize) -> Vec<Vec<usize>> {
    fn visit(
        next: usize,
        end: usize,
        remaining: usize,
        current: &mut Vec<usize>,
        output: &mut Vec<Vec<usize>>,
    ) {
        if remaining == 0 {
            output.push(current.clone());
            return;
        }
        if end.saturating_sub(next) < remaining {
            return;
        }
        for value in next..=end - remaining {
            current.push(value);
            visit(value + 1, end, remaining - 1, current, output);
            current.pop();
        }
    }
    let mut output = Vec::new();
    visit(start, end, k, &mut Vec::with_capacity(k), &mut output);
    output
}

fn binomial(n: usize, k: usize) -> usize {
    let k = k.min(n.saturating_sub(k));
    (1..=k).fold(1_usize, |value, index| value * (n + 1 - index) / index)
}

#[derive(Clone, Copy)]
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    fn index(&mut self, upper: usize) -> usize {
        (self.next() % upper as u64) as usize
    }
}

fn sample_combinations(n: usize, k: usize, maximum: usize, seed: u64) -> Vec<Vec<usize>> {
    let mut random = SplitMix64(seed);
    let mut seen: HashSet<Vec<usize>> = HashSet::new();
    let mut attempts = maximum.saturating_mul(25);
    while seen.len() < maximum && attempts > 0 {
        attempts -= 1;
        let mut values: Vec<usize> = (0..n).collect();
        for index in 0..k {
            let chosen = index + random.index(n - index);
            values.swap(index, chosen);
        }
        let mut sample = values[..k].to_vec();
        sample.sort_unstable();
        seen.insert(sample);
    }
    let mut samples: Vec<Vec<usize>> = seen.into_iter().collect();
    samples.sort();
    samples
}

fn pool_signature(team1: &Team, team2: &Team, excluded_names: &[String]) -> PoolSignature {
    let mut names1: Vec<String> = team1
        .players
        .iter()
        .map(|player| player.name.clone())
        .collect();
    let mut names2: Vec<String> = team2
        .players
        .iter()
        .map(|player| player.name.clone())
        .collect();
    let mut excluded = excluded_names.to_vec();
    names1.sort();
    names2.sort();
    excluded.sort();
    if names2 < names1 {
        std::mem::swap(&mut names1, &mut names2);
    }
    PoolSignature(names1, names2, excluded)
}

fn numeric_prefix_cmp(left: (f64, f64, usize), right: (f64, f64, usize)) -> Ordering {
    if left.0 < right.0 {
        Ordering::Less
    } else if left.0 > right.0 {
        Ordering::Greater
    } else if left.1 < right.1 {
        Ordering::Less
    } else if left.1 > right.1 {
        Ordering::Greater
    } else {
        left.2.cmp(&right.2)
    }
}

fn pool_candidate_is_better(
    candidate: (f64, f64, usize),
    candidate_signature: &PoolSignature,
    best: (f64, f64, usize),
    best_signature: Option<&PoolSignature>,
) -> bool {
    match numeric_prefix_cmp(candidate, best) {
        Ordering::Less => true,
        Ordering::Greater => false,
        Ordering::Equal => best_signature.is_none_or(|current| candidate_signature < current),
    }
}

fn materialize_override_matchup(
    team1_players: &[&Player],
    team2_players: &[&Player],
    excluded_names: &[String],
    exclusion_penalty: f64,
    score: &PoolScoreOverride,
) -> Result<PoolMatchup, ShufflerError> {
    let roles = default_role_assignment().to_vec();
    let team1 = Team::new(
        team1_players
            .iter()
            .map(|player| (*player).clone())
            .collect(),
        Some(roles.clone()),
    )?;
    let team2 = Team::new(
        team2_players
            .iter()
            .map(|player| (*player).clone())
            .collect(),
        Some(roles),
    )?;
    Ok(PoolMatchup {
        team1,
        team2,
        total_score: score.total_score,
        value_diff: score.value_diff,
        total_off_roles: score.total_off_roles,
        log_entry: PoolLogEntry {
            total_score: score.total_score,
            value_diff: score.value_diff,
            off_role_penalty: 0.0,
            parity_penalty: 0.0,
            exclusion_penalty,
            team1_value: 0.0,
            team2_value: 0.0,
            team1_off_roles: 0,
            team2_off_roles: 0,
            excluded_names: excluded_names.to_vec(),
        },
    })
}

fn resolved_region(player: &Player) -> Option<&'static str> {
    for value in [
        player.preferred_region.as_deref(),
        player.inferred_region.as_deref(),
    ] {
        match value {
            Some("USE") => return Some("USE"),
            Some("USW") => return Some("USW"),
            _ => {}
        }
    }
    None
}

fn region_split_mismatches(team1: &[&Player], team2: &[&Player]) -> usize {
    let count = |players: &[&Player], region: &str| {
        players
            .iter()
            .filter(|player| resolved_region(player) == Some(region))
            .count()
    };
    let team1_use = count(team1, "USE");
    let team1_usw = count(team1, "USW");
    let team2_use = count(team2, "USE");
    let team2_usw = count(team2, "USW");
    (team1_use + team2_usw).min(team1_usw + team2_use)
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};

    use super::{
        BalancedShuffler, BranchSearchOptions, LOW_PRIORITY_GOODNESS_PENALTY, PackageDeal,
        PoolOptions, PoolResult, PoolScoreOverride, PoolSignature, RoleAssignment,
        RoleAssignmentProvider, ShuffleConstraints, SoftAvoid, SplitMix64, default_role_assignment,
        region_split_mismatches,
    };
    use crate::player::Player;
    use crate::team::Team;
    use crate::team_balancing::TeamBalancingService;

    fn approx(left: f64, right: f64) {
        assert!(
            (left - right).abs() <= 1e-9_f64.max(right.abs() * 1e-12),
            "{left} != {right}"
        );
    }

    fn role_list(roles: &[&str]) -> Option<Vec<String>> {
        Some(roles.iter().map(|role| (*role).to_owned()).collect())
    }

    /// Role-neutral by construction: an even record in every role yields a
    /// role factor of exactly 1.0, keeping these fixtures focused on the
    /// balance formula instead of the role-performance multiplier.
    fn neutral_role_records()
    -> std::collections::BTreeMap<String, crate::role_performance::RoleRecord> {
        crate::team::ROLES
            .iter()
            .map(|role| {
                (
                    (*role).to_owned(),
                    crate::role_performance::RoleRecord::new(5, 5),
                )
            })
            .collect()
    }

    fn player(name: impl Into<String>, value: i64, roles: &[&str]) -> Player {
        Player {
            role_records: neutral_role_records(),
            mmr: Some(value),
            preferred_roles: role_list(roles),
            ..Player::new(name)
        }
    }

    fn full_flex_player(name: impl Into<String>, value: i64, discord_id: i64) -> Player {
        Player {
            discord_id: Some(discord_id),
            ..player(name, value, &["1", "2", "3", "4", "5"])
        }
    }

    fn no_rating_constraints() -> ShuffleConstraints<'static> {
        ShuffleConstraints::default()
    }

    fn configured_mmr_shuffler() -> BalancedShuffler {
        BalancedShuffler {
            use_glicko: false,
            ..BalancedShuffler::default()
        }
    }

    fn selected_ids(result: &PoolResult) -> HashSet<i64> {
        result
            .team1
            .players
            .iter()
            .chain(&result.team2.players)
            .filter_map(|player| player.discord_id)
            .collect()
    }

    fn names(players: &[Player]) -> HashSet<String> {
        players.iter().map(|player| player.name.clone()).collect()
    }

    fn team_names(team: &Team) -> Vec<String> {
        team.players
            .iter()
            .map(|player| player.name.clone())
            .collect()
    }

    #[derive(Default)]
    struct RecordingProvider {
        calls: Mutex<Vec<Vec<Vec<String>>>>,
        value: Arc<Vec<RoleAssignment>>,
    }

    impl RoleAssignmentProvider for RecordingProvider {
        fn get(&self, player_roles_key: &[Vec<String>]) -> Arc<Vec<RoleAssignment>> {
            self.calls
                .lock()
                .expect("recording provider mutex")
                .push(player_roles_key.to_vec());
            Arc::clone(&self.value)
        }
    }

    #[test]
    fn test_low_priority_penalty_is_660_per_selected_player() {
        let selected = HashSet::from([100, 101, 102]);
        let low_priority = HashSet::from([100, 101, 999]);
        assert_eq!(
            BalancedShuffler::calculate_low_priority_penalty(&selected, Some(&low_priority)),
            1_320.0
        );
    }

    #[test]
    fn test_optimized_role_metrics_apply_default_off_role_value_adjustment() {
        let player = player("Core", 3_000, &["1"]);
        let shuffler = BalancedShuffler {
            use_glicko: false,
            ..BalancedShuffler::default()
        };
        let roles = ["2", "1", "3", "4", "5"].map(str::to_owned);
        let metrics = shuffler.role_assignment_metrics(&[&player], &roles, &[3_000.0]);

        assert_eq!(metrics.team_value, 2_750.0);
        assert_eq!(metrics.role_values, [0.0, 2_750.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn test_default_off_role_goodness_adds_670_per_player() {
        let team1_players = (0..5)
            .map(|index| {
                player(
                    format!("OnRole{index}"),
                    1_000,
                    &[["1", "2", "3", "4", "5"][index]],
                )
            })
            .collect::<Vec<_>>();
        let team2_players = ["2", "1", "3", "4", "5"]
            .into_iter()
            .enumerate()
            .map(|(index, role)| player(format!("Team2-{index}"), 1_000, &[role]))
            .collect::<Vec<_>>();
        let shuffler = BalancedShuffler {
            use_glicko: false,
            off_role_multiplier: 1.0,
            off_role_flat_value_penalty: 0.0,
            role_matchup_delta_weight: 0.0,
            ..BalancedShuffler::default()
        };
        let assignment = ["1", "2", "3", "4", "5"].map(str::to_owned);
        let team1_metrics = shuffler.role_assignment_metrics(
            &team1_players.iter().collect::<Vec<_>>(),
            &assignment,
            &[1_000.0; 5],
        );
        let team2_metrics = shuffler.role_assignment_metrics(
            &team2_players.iter().collect::<Vec<_>>(),
            &assignment,
            &[1_000.0; 5],
        );
        let (_, _, score) = shuffler.score_unconstrained_metrics(
            &[team1_metrics],
            &[team2_metrics],
            0.0,
            super::ADJUSTED_VALUE_DIFF_WEIGHT,
        );

        assert_eq!(score, 1_340.0);
    }

    #[test]
    fn test_low_priority_team_adjustment_singletons_are_not_discounted() {
        let team1 = HashSet::from([100]);
        let team2 = HashSet::from([200]);
        let low_priority = HashSet::from([100, 200]);
        assert_eq!(
            BalancedShuffler::calculate_low_priority_team_adjustment(
                &team1,
                &team2,
                Some(&low_priority),
            ),
            0.0
        );
    }

    #[test]
    fn test_low_priority_team_adjustment_two_in_one_team_is_minus_100() {
        let team1 = HashSet::from([100, 101]);
        let team2 = HashSet::new();
        let low_priority = HashSet::from([100, 101]);
        assert_eq!(
            BalancedShuffler::calculate_low_priority_team_adjustment(
                &team1,
                &team2,
                Some(&low_priority),
            ),
            -100.0
        );
    }

    #[test]
    fn test_low_priority_team_adjustment_two_each_team_is_minus_100() {
        let team1 = HashSet::from([100, 101]);
        let team2 = HashSet::from([200, 201]);
        let low_priority = HashSet::from([100, 101, 200, 201]);
        assert_eq!(
            BalancedShuffler::calculate_low_priority_team_adjustment(
                &team1,
                &team2,
                Some(&low_priority),
            ),
            -100.0
        );
    }

    #[test]
    fn test_low_priority_team_adjustment_three_vs_two_is_minus_200() {
        let team1 = HashSet::from([100, 101, 102]);
        let team2 = HashSet::from([200, 201]);
        let low_priority = HashSet::from([100, 101, 102, 200, 201]);
        assert_eq!(
            BalancedShuffler::calculate_low_priority_team_adjustment(
                &team1,
                &team2,
                Some(&low_priority),
            ),
            -200.0
        );
    }

    #[test]
    fn test_low_priority_team_adjustment_four_vs_one_is_minus_300() {
        let team1 = HashSet::from([100, 101, 102, 103]);
        let team2 = HashSet::from([200]);
        let low_priority = HashSet::from([100, 101, 102, 103, 200]);
        assert_eq!(
            BalancedShuffler::calculate_low_priority_team_adjustment(
                &team1,
                &team2,
                Some(&low_priority),
            ),
            -300.0
        );
    }

    #[test]
    fn test_low_priority_team_adjustment_five_in_one_team_is_minus_400() {
        let team1 = HashSet::from([100, 101, 102, 103, 104]);
        let team2 = HashSet::new();
        let low_priority = HashSet::from([100, 101, 102, 103, 104]);
        assert_eq!(
            BalancedShuffler::calculate_low_priority_team_adjustment(
                &team1,
                &team2,
                Some(&low_priority),
            ),
            -400.0
        );
    }

    #[test]
    fn test_matchup_score_subtracts_low_priority_team_adjustment_once() {
        let team1 = (0..5)
            .map(|index| {
                player(
                    format!("Team1-{index}"),
                    1_000,
                    &[["1", "2", "3", "4", "5"][index]],
                )
            })
            .enumerate()
            .map(|(index, mut player)| {
                player.discord_id = Some(100 + index as i64);
                player
            })
            .collect::<Vec<_>>();
        let team2 = (0..5)
            .map(|index| {
                player(
                    format!("Team2-{index}"),
                    1_000,
                    &[["1", "2", "3", "4", "5"][index]],
                )
            })
            .enumerate()
            .map(|(index, mut player)| {
                player.discord_id = Some(200 + index as i64);
                player
            })
            .collect::<Vec<_>>();
        let low_priority = HashSet::from([100, 101, 200, 201]);
        let shuffler = BalancedShuffler {
            use_glicko: false,
            rd_priority_weight: 0.0,
            ..BalancedShuffler::default()
        };

        let (_, _, score) = shuffler
            .score_role_assignments_for_matchup(
                &team1,
                &team2,
                20,
                ShuffleConstraints {
                    low_priority_ids: Some(&low_priority),
                    ..ShuffleConstraints::default()
                },
            )
            .expect("matchup scores");
        assert_eq!(score, -100.0);
    }

    #[test]
    fn test_fixed_ten_search_does_not_stop_before_low_priority_grouping_bonus() {
        let players = (0..10)
            .map(|index| full_flex_player(format!("Player{index}"), 1_500, 100 + index))
            .collect::<Vec<_>>();
        let low_priority = HashSet::from([100, 105]);
        let shuffler = BalancedShuffler {
            use_glicko: false,
            rd_priority_weight: 0.0,
            ..BalancedShuffler::default()
        };
        let (team1, team2) = shuffler
            .shuffle_with_constraints(
                &players,
                ShuffleConstraints {
                    low_priority_ids: Some(&low_priority),
                    ..ShuffleConstraints::default()
                },
            )
            .expect("fixed ten shuffle");
        let team1_ids = team1
            .players
            .iter()
            .filter_map(|player| player.discord_id)
            .collect::<HashSet<_>>();
        let team2_ids = team2
            .players
            .iter()
            .filter_map(|player| player.discord_id)
            .collect::<HashSet<_>>();
        assert!(
            low_priority.is_subset(&team1_ids) || low_priority.is_subset(&team2_ids),
            "low-priority players should be grouped: {team1_ids:?} / {team2_ids:?}"
        );
    }

    #[test]
    fn test_branch_bound_groups_selected_low_priority_players() {
        let players = (0..14)
            .map(|index| full_flex_player(format!("Player{index:02}"), 1_500, 100 + index))
            .collect::<Vec<_>>();
        let low_priority = HashSet::from([100, 101]);
        let exclusion_counts = players
            .iter()
            .map(|player| {
                (
                    player.name.clone(),
                    if low_priority.contains(&player.discord_id.unwrap()) {
                        100
                    } else {
                        0
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let shuffler = BalancedShuffler {
            use_glicko: false,
            exclusion_penalty_weight: 70.0,
            recent_match_penalty_weight: 0.0,
            rd_priority_weight: 0.0,
            ..BalancedShuffler::default()
        };
        let result = shuffler
            .shuffle_branch_bound(
                &players,
                PoolOptions {
                    exclusion_counts: Some(&exclusion_counts),
                    constraints: ShuffleConstraints {
                        low_priority_ids: Some(&low_priority),
                        ..ShuffleConstraints::default()
                    },
                    ..PoolOptions::default()
                },
                BranchSearchOptions::default(),
            )
            .expect("branch-bound shuffle");
        let team1_ids = result
            .team1
            .players
            .iter()
            .filter_map(|player| player.discord_id)
            .collect::<HashSet<_>>();
        let team2_ids = result
            .team2
            .players
            .iter()
            .filter_map(|player| player.discord_id)
            .collect::<HashSet<_>>();
        let excluded_ids = result
            .excluded
            .iter()
            .filter_map(|player| player.discord_id)
            .collect::<HashSet<_>>();
        assert!(low_priority.is_disjoint(&excluded_ids));
        assert!(low_priority.is_subset(&team1_ids) || low_priority.is_subset(&team2_ids));
    }

    #[test]
    fn test_split_bound_matches_reference_with_low_priority_grouping_two() {
        let players = seeded_pool(0x14A9, "Seeded", 3);
        let low_priority = players[..2]
            .iter()
            .filter_map(|player| player.discord_id)
            .collect::<HashSet<_>>();
        let exclusion_counts = players
            .iter()
            .map(|player| {
                (
                    player.name.clone(),
                    if low_priority.contains(&player.discord_id.unwrap()) {
                        20
                    } else {
                        0
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let recent_names = players
            .iter()
            .skip(10)
            .map(|player| player.name.clone())
            .collect::<HashSet<_>>();
        let shuffler = BalancedShuffler {
            off_role_multiplier: 0.8,
            off_role_flat_penalty: 60.0,
            role_matchup_delta_weight: 0.35,
            exclusion_penalty_weight: 100.0,
            rd_priority_weight: 0.08,
            recent_match_penalty_weight: 75.0,
            rating_spread_divisor: 15.0,
            ..BalancedShuffler::default()
        };
        let options = PoolOptions {
            exclusion_counts: Some(&exclusion_counts),
            recent_match_names: Some(&recent_names),
            constraints: ShuffleConstraints {
                low_priority_ids: Some(&low_priority),
                ..ShuffleConstraints::default()
            },
            ..PoolOptions::default()
        };
        let reference = shuffler
            .shuffle_branch_bound(
                &players,
                options,
                BranchSearchOptions {
                    use_split_role_bound: false,
                    initial_upper_bound: Some(f64::INFINITY),
                },
            )
            .expect("reference search");
        let optimized = shuffler
            .shuffle_branch_bound(
                &players,
                options,
                BranchSearchOptions {
                    use_split_role_bound: true,
                    initial_upper_bound: Some(f64::INFINITY),
                },
            )
            .expect("optimized search");
        assert_eq!(
            result_signature(&optimized, false),
            result_signature(&reference, false)
        );
    }

    #[test]
    fn test_split_bound_matches_reference_with_low_priority_grouping_five() {
        let players = seeded_pool(0x14AC, "Seeded", 3);
        let low_priority = players[..5]
            .iter()
            .filter_map(|player| player.discord_id)
            .collect::<HashSet<_>>();
        let exclusion_counts = players
            .iter()
            .map(|player| {
                (
                    player.name.clone(),
                    if low_priority.contains(&player.discord_id.unwrap()) {
                        20
                    } else {
                        0
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let recent_names = players
            .iter()
            .skip(10)
            .map(|player| player.name.clone())
            .collect::<HashSet<_>>();
        let shuffler = BalancedShuffler {
            off_role_multiplier: 0.8,
            off_role_flat_penalty: 60.0,
            role_matchup_delta_weight: 0.35,
            exclusion_penalty_weight: 100.0,
            rd_priority_weight: 0.08,
            recent_match_penalty_weight: 75.0,
            rating_spread_divisor: 15.0,
            ..BalancedShuffler::default()
        };
        let options = PoolOptions {
            exclusion_counts: Some(&exclusion_counts),
            recent_match_names: Some(&recent_names),
            constraints: ShuffleConstraints {
                low_priority_ids: Some(&low_priority),
                ..ShuffleConstraints::default()
            },
            ..PoolOptions::default()
        };
        let reference = shuffler
            .shuffle_branch_bound(
                &players,
                options,
                BranchSearchOptions {
                    use_split_role_bound: false,
                    initial_upper_bound: Some(f64::INFINITY),
                },
            )
            .expect("reference search");
        let optimized = shuffler
            .shuffle_branch_bound(
                &players,
                options,
                BranchSearchOptions {
                    use_split_role_bound: true,
                    initial_upper_bound: Some(f64::INFINITY),
                },
            )
            .expect("optimized search");
        assert_eq!(
            result_signature(&optimized, false),
            result_signature(&reference, false)
        );
    }

    fn soft_avoid_teams() -> (HashSet<i64>, HashSet<i64>) {
        (
            HashSet::from([1000, 1001, 1002, 1003, 1004]),
            HashSet::from([1005, 1006, 1007, 1008, 1009]),
        )
    }

    fn soft_avoid_players(count: usize) -> Vec<Player> {
        (0..count)
            .map(|index| Player {
                mmr: Some(3_000 + i64::try_from(index).expect("small fixture") * 100),
                glicko_rating: Some(1_500.0),
                glicko_rd: Some(100.0),
                discord_id: Some(1_000 + i64::try_from(index).expect("small fixture")),
                ..Player::new(format!("Player{}", index + 1))
            })
            .collect()
    }

    #[test]
    fn test_no_avoids_returns_zero() {
        let (team1, team2) = soft_avoid_teams();
        let shuffler = BalancedShuffler::default();
        assert_eq!(
            shuffler.calculate_soft_avoid_penalty(&team1, &team2, None, None),
            0.0
        );
        assert_eq!(
            shuffler.calculate_soft_avoid_penalty(&team1, &team2, Some(&[]), None),
            0.0
        );
    }

    #[test]
    fn test_avoid_same_team_applies_penalty() {
        let (team1, team2) = soft_avoid_teams();
        let avoids = [SoftAvoid {
            avoider_discord_id: 1000,
            avoided_discord_id: 1001,
        }];
        assert_eq!(
            BalancedShuffler {
                soft_avoid_penalty: 500.0,
                ..BalancedShuffler::default()
            }
            .calculate_soft_avoid_penalty(&team1, &team2, Some(&avoids), None),
            500.0
        );
    }

    #[test]
    fn test_default_avoid_same_team_adds_160_goodness() {
        let (team1, team2) = soft_avoid_teams();
        let avoids = [SoftAvoid {
            avoider_discord_id: 1000,
            avoided_discord_id: 1001,
        }];

        assert_eq!(
            BalancedShuffler::default().calculate_soft_avoid_penalty(
                &team1,
                &team2,
                Some(&avoids),
                None,
            ),
            160.0
        );
    }

    #[test]
    fn test_low_priority_avoid_modifiers_apply_by_direction() {
        let (team1, team2) = soft_avoid_teams();
        let avoids = [SoftAvoid {
            avoider_discord_id: 1000,
            avoided_discord_id: 1001,
        }];
        let shuffler = BalancedShuffler {
            soft_avoid_penalty: 500.0,
            ..BalancedShuffler::default()
        };
        assert_eq!(
            shuffler.calculate_soft_avoid_penalty(
                &team1,
                &team2,
                Some(&avoids),
                Some(&HashSet::from([1000])),
            ),
            250.0
        );
        assert_eq!(
            shuffler.calculate_soft_avoid_penalty(
                &team1,
                &team2,
                Some(&avoids),
                Some(&HashSet::from([1001])),
            ),
            1_000.0
        );
        assert_eq!(
            shuffler.calculate_soft_avoid_penalty(
                &team1,
                &team2,
                Some(&avoids),
                Some(&HashSet::from([1000, 1001])),
            ),
            500.0
        );
    }

    #[test]
    fn test_shuffle_display_uses_directional_low_priority_avoid_strength() {
        let (team1, team2) = soft_avoid_teams();
        let selected = team1.union(&team2).copied().collect::<HashSet<_>>();
        let avoids = [SoftAvoid {
            avoider_discord_id: 1000,
            avoided_discord_id: 1001,
        }];
        let shuffler = BalancedShuffler::default();
        let total_penalty = |low_priority: Option<&HashSet<i64>>| {
            shuffler.calculate_soft_avoid_penalty(&team1, &team2, Some(&avoids), low_priority)
                + BalancedShuffler::calculate_low_priority_penalty(&selected, low_priority)
        };
        let target_only = HashSet::from([1001]);
        let both = HashSet::from([1000, 1001]);
        let baseline = total_penalty(None);
        let target_penalty = total_penalty(Some(&target_only));
        let both_penalty = total_penalty(Some(&both));

        assert_eq!(
            target_penalty - baseline,
            LOW_PRIORITY_GOODNESS_PENALTY + shuffler.soft_avoid_penalty
        );
        assert_eq!(
            both_penalty - target_penalty,
            LOW_PRIORITY_GOODNESS_PENALTY - shuffler.soft_avoid_penalty
        );
    }

    #[test]
    fn test_shuffle_accounts_for_all_sixteen_players() {
        let players = (0..16)
            .map(|index| {
                full_flex_player(
                    format!("Player{index}"),
                    1_500 + i64::from(index),
                    1_000 + i64::from(index),
                )
            })
            .collect::<Vec<_>>();
        let result = BalancedShuffler {
            use_glicko: false,
            ..BalancedShuffler::default()
        }
        .shuffle_from_pool(&players, PoolOptions::default())
        .expect("sixteen-player shuffle succeeds");
        let selected = selected_ids(&result);
        let excluded = result
            .excluded
            .iter()
            .filter_map(|player| player.discord_id)
            .collect::<HashSet<_>>();
        assert_eq!(selected.len(), 10);
        assert_eq!(excluded.len(), 6);
        assert_eq!(
            selected.union(&excluded).copied().collect::<HashSet<_>>(),
            players
                .iter()
                .filter_map(|player| player.discord_id)
                .collect::<HashSet<_>>()
        );
    }

    #[test]
    fn test_goodness_adds_660_per_active_low_priority_player() {
        let selected = (1_000..1_010).collect::<HashSet<_>>();
        let active = HashSet::from([1_000, 1_001]);
        let baseline = BalancedShuffler::calculate_low_priority_penalty(&selected, None);
        let penalized = BalancedShuffler::calculate_low_priority_penalty(&selected, Some(&active));
        assert_eq!(penalized - baseline, 1_320.0);
    }

    #[test]
    fn test_goodness_subtracts_selected_players_lobby_wait_minutes() {
        let players = (0..10)
            .map(|index| full_flex_player(format!("Player{index}"), 1_500, 1_000 + index))
            .collect::<Vec<_>>();
        let refs = players.iter().collect::<Vec<_>>();
        let waits = players
            .iter()
            .enumerate()
            .map(|(index, player)| {
                (
                    player.discord_id.expect("fixture ID"),
                    i64::try_from(index + 1).expect("small fixture"),
                )
            })
            .collect::<HashMap<_, _>>();
        let baseline = BalancedShuffler::calculate_lobby_wait_bonus(&refs, None);
        let waited = BalancedShuffler::calculate_lobby_wait_bonus(&refs, Some(&waits));
        assert_eq!(baseline - waited, -55.0);
    }

    #[test]
    fn test_goodness_subtracts_rd_priority_bonus() {
        let make_players = |rd| {
            (0..10)
                .map(|index| Player {
                    discord_id: Some(1_000 + index),
                    glicko_rd: Some(rd),
                    ..Player::new(format!("Player{index}"))
                })
                .collect::<Vec<_>>()
        };
        let shuffler = BalancedShuffler::default();
        let high = make_players(350.0);
        let low = make_players(100.0);
        let high_refs = high.iter().collect::<Vec<_>>();
        let low_refs = low.iter().collect::<Vec<_>>();
        assert_eq!(
            shuffler.calculate_rd_priority(&high_refs) - shuffler.calculate_rd_priority(&low_refs),
            10.0 * 250.0 * shuffler.rd_priority_weight
        );
    }

    #[test]
    fn test_goodness_adds_230_for_selected_last_match_player() {
        let players = (0..10)
            .map(|index| {
                player(
                    format!("Player{index}"),
                    1_500,
                    &[match index % 5 {
                        0 => "1",
                        1 => "2",
                        2 => "3",
                        3 => "4",
                        _ => "5",
                    }],
                )
            })
            .collect::<Vec<_>>();
        let recent = HashSet::from([players[0].name.clone()]);
        let shuffler = BalancedShuffler {
            use_glicko: false,
            rd_priority_weight: 0.0,
            ..BalancedShuffler::default()
        };
        let baseline = shuffler
            .greedy_shuffle(&players, PoolOptions::default())
            .expect("baseline shuffle");
        let penalized = shuffler
            .greedy_shuffle(
                &players,
                PoolOptions {
                    recent_match_names: Some(&recent),
                    ..PoolOptions::default()
                },
            )
            .expect("recent-player shuffle");
        assert_eq!(penalized.score - baseline.score, 230.0);
    }

    #[test]
    fn goodness_weights_adjusted_team_value_difference() {
        let team1 = vec![
            player("RadiantCarry", 1_100, &["1"]),
            player("RadiantMid", 1_000, &["2"]),
            player("RadiantOfflane", 1_000, &["3"]),
            player("RadiantSoft", 1_000, &["4"]),
            player("RadiantHard", 1_000, &["5"]),
        ];
        let team2 = vec![
            player("DireCarry", 1_000, &["1"]),
            player("DireMid", 1_000, &["2"]),
            player("DireOfflane", 1_000, &["3"]),
            player("DireSoft", 1_000, &["4"]),
            player("DireHard", 1_000, &["5"]),
        ];
        let shuffler = BalancedShuffler {
            use_glicko: false,
            off_role_multiplier: 1.0,
            off_role_flat_value_penalty: 0.0,
            off_role_flat_penalty: 0.0,
            role_matchup_delta_weight: 0.0,
            rd_priority_weight: 0.0,
            ..BalancedShuffler::default()
        };

        let (_, _, score) = shuffler
            .score_role_assignments_for_matchup(&team1, &team2, 1, ShuffleConstraints::default())
            .expect("fixed matchup scores");

        approx(score, 110.0);
    }

    #[test]
    fn adjusted_value_weight_changes_borderline_pool_selection() {
        let ratings = [
            1_832, 1_604, 1_380, 392, 2_072, 1_847, 432, 1_273, 865, 1_006, 678,
        ];
        let exclusion_factors = [6, 4, 1, 3, 0, 0, 6, 5, 1, 2, 1];
        let players = ratings
            .into_iter()
            .enumerate()
            .map(|(index, rating)| {
                full_flex_player(
                    format!("Player{index}"),
                    rating,
                    1_000 + i64::try_from(index).expect("small fixture index"),
                )
            })
            .collect::<Vec<_>>();
        let exclusion_counts = players
            .iter()
            .zip(exclusion_factors)
            .map(|(player, factor)| (player.name.clone(), factor))
            .collect::<HashMap<_, _>>();
        let shuffler = BalancedShuffler {
            use_glicko: false,
            off_role_multiplier: 1.0,
            off_role_flat_value_penalty: 0.0,
            off_role_flat_penalty: 0.0,
            role_matchup_delta_weight: 0.0,
            rd_priority_weight: 0.0,
            rating_spread_divisor: f64::INFINITY,
            ..BalancedShuffler::default()
        };

        let result = shuffler
            .shuffle_from_pool(
                &players,
                PoolOptions {
                    exclusion_counts: Some(&exclusion_counts),
                    ..PoolOptions::default()
                },
            )
            .expect("borderline pool shuffles");

        assert_eq!(
            result
                .excluded
                .iter()
                .map(|player| player.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Player4"]
        );
    }

    #[test]
    fn large_pool_preselection_weights_adjusted_value_difference() {
        let mut players = (0..15)
            .map(|index| full_flex_player(format!("Player{index}"), 1_000, index + 1))
            .collect::<Vec<_>>();
        players[0].mmr = Some(1_100);
        let refs = players.iter().collect::<Vec<_>>();
        let shuffler = BalancedShuffler {
            use_glicko: false,
            rd_priority_weight: 0.0,
            rating_spread_divisor: f64::INFINITY,
            ..BalancedShuffler::default()
        };

        let selection = shuffler.build_pool_selection(
            &refs,
            (0..10).collect(),
            PoolOptions::default(),
            &mut super::ScoringContext::default(),
        );

        approx(selection.preselection_score, -395.0);
    }

    #[test]
    fn test_goodness_score_respects_role_matchup_weight() {
        let team1 = vec![
            player("RadiantCarry", 2_000, &["1"]),
            player("RadiantMid", 1_500, &["2"]),
            player("RadiantOfflane", 1_000, &["3"]),
            player("RadiantSoft", 1_000, &["4"]),
            player("RadiantHard", 1_000, &["5"]),
        ];
        let team2 = vec![
            player("DireCarry", 1_400, &["1"]),
            player("DireMid", 1_500, &["2"]),
            player("DireOfflane", 1_900, &["3"]),
            player("DireSoft", 1_000, &["4"]),
            player("DireHard", 1_000, &["5"]),
        ];
        let values = team1
            .iter()
            .chain(&team2)
            .map(|player| player.mmr.expect("fixture MMR") as f64)
            .collect::<Vec<_>>();
        let shuffler = BalancedShuffler {
            use_glicko: false,
            off_role_multiplier: 1.0,
            off_role_flat_penalty: 0.0,
            role_matchup_delta_weight: 0.17,
            rd_priority_weight: 0.0,
            ..BalancedShuffler::default()
        };
        let combo_penalty = shuffler.calculate_rating_spread_penalty(&values)
            - BalancedShuffler::calculate_lobby_rating_bonus(&values);
        let matchup = shuffler
            .evaluate_pool_matchup(
                &team1,
                &team2,
                combo_penalty,
                0.0,
                &[],
                0.0,
                0.0,
                1,
                ShuffleConstraints::default(),
            )
            .expect("fixed role matchup evaluates");
        approx(matchup.total_score, 105.0);
    }

    #[test]
    fn test_region_shuffle_mode_splits_usw_and_use() {
        let players = (0..10)
            .map(|index| Player {
                preferred_region: Some(if index < 5 { "USW" } else { "USE" }.to_owned()),
                ..full_flex_player(format!("Player{index}"), 1_500, 1_000 + index)
            })
            .collect::<Vec<_>>();
        let (team1, team2) = BalancedShuffler {
            use_glicko: false,
            off_role_flat_penalty: 0.0,
            role_matchup_delta_weight: 0.0,
            rd_priority_weight: 0.0,
            region_split: true,
            region_split_penalty: 1_000.0,
            ..BalancedShuffler::default()
        }
        .shuffle(&players)
        .expect("region shuffle succeeds");
        let team_regions = [&team1, &team2]
            .into_iter()
            .map(|team| {
                team.players
                    .iter()
                    .filter_map(|player| player.preferred_region.as_deref())
                    .collect::<HashSet<_>>()
            })
            .collect::<Vec<_>>();
        assert!(team_regions.contains(&HashSet::from(["USW"])));
        assert!(team_regions.contains(&HashSet::from(["USE"])));
    }

    #[test]
    fn test_avoid_opposite_teams_no_penalty() {
        let (team1, team2) = soft_avoid_teams();
        let avoids = [SoftAvoid {
            avoider_discord_id: 1000,
            avoided_discord_id: 1005,
        }];
        assert_eq!(
            BalancedShuffler {
                soft_avoid_penalty: 500.0,
                ..BalancedShuffler::default()
            }
            .calculate_soft_avoid_penalty(&team1, &team2, Some(&avoids), None),
            0.0
        );
    }

    #[test]
    fn test_bidirectional_avoids_additive() {
        let (team1, team2) = soft_avoid_teams();
        let avoids = [
            SoftAvoid {
                avoider_discord_id: 1000,
                avoided_discord_id: 1001,
            },
            SoftAvoid {
                avoider_discord_id: 1001,
                avoided_discord_id: 1000,
            },
        ];
        assert_eq!(
            BalancedShuffler {
                soft_avoid_penalty: 500.0,
                ..BalancedShuffler::default()
            }
            .calculate_soft_avoid_penalty(&team1, &team2, Some(&avoids), None),
            1_000.0
        );
    }

    #[test]
    fn test_multiple_avoids_same_team() {
        let (team1, team2) = soft_avoid_teams();
        let avoids = [
            SoftAvoid {
                avoider_discord_id: 1000,
                avoided_discord_id: 1001,
            },
            SoftAvoid {
                avoider_discord_id: 1002,
                avoided_discord_id: 1003,
            },
        ];
        assert_eq!(
            BalancedShuffler {
                soft_avoid_penalty: 500.0,
                ..BalancedShuffler::default()
            }
            .calculate_soft_avoid_penalty(&team1, &team2, Some(&avoids), None),
            1_000.0
        );
    }

    #[test]
    fn test_mixed_avoids() {
        let (team1, team2) = soft_avoid_teams();
        let avoids = [
            SoftAvoid {
                avoider_discord_id: 1000,
                avoided_discord_id: 1001,
            },
            SoftAvoid {
                avoider_discord_id: 1000,
                avoided_discord_id: 1005,
            },
            SoftAvoid {
                avoider_discord_id: 1005,
                avoided_discord_id: 1006,
            },
        ];
        assert_eq!(
            BalancedShuffler {
                soft_avoid_penalty: 500.0,
                ..BalancedShuffler::default()
            }
            .calculate_soft_avoid_penalty(&team1, &team2, Some(&avoids), None),
            1_000.0
        );
    }

    #[test]
    fn test_shuffle_accepts_avoids_parameter() {
        let players = soft_avoid_players(10);
        let avoids = [SoftAvoid {
            avoider_discord_id: 1000,
            avoided_discord_id: 1001,
        }];
        let (team1, team2) = BalancedShuffler {
            consider_roles: false,
            ..BalancedShuffler::default()
        }
        .shuffle_with_constraints(
            &players,
            ShuffleConstraints {
                avoids: Some(&avoids),
                ..ShuffleConstraints::default()
            },
        )
        .expect("avoid-constrained shuffle succeeds");
        assert_eq!(team1.players.len(), 5);
        assert_eq!(team2.players.len(), 5);
    }

    #[test]
    fn test_shuffle_from_pool_accepts_avoids_parameter() {
        let players = soft_avoid_players(14);
        let avoids = [SoftAvoid {
            avoider_discord_id: 1000,
            avoided_discord_id: 1001,
        }];
        let result = BalancedShuffler {
            consider_roles: false,
            ..BalancedShuffler::default()
        }
        .shuffle_from_pool(
            &players,
            PoolOptions {
                constraints: ShuffleConstraints {
                    avoids: Some(&avoids),
                    ..ShuffleConstraints::default()
                },
                ..PoolOptions::default()
            },
        )
        .expect("avoid-constrained pool shuffle succeeds");
        assert_eq!(result.team1.players.len(), 5);
        assert_eq!(result.team2.players.len(), 5);
        assert_eq!(result.excluded.len(), 4);
    }

    #[test]
    fn test_avoid_influences_team_selection() {
        let players = soft_avoid_players(10);
        let avoids = [SoftAvoid {
            avoider_discord_id: 1000,
            avoided_discord_id: 1001,
        }];
        let (team1, team2) = BalancedShuffler {
            consider_roles: false,
            rd_priority_weight: 0.0,
            soft_avoid_penalty: 10_000.0,
            ..BalancedShuffler::default()
        }
        .shuffle_with_constraints(
            &players,
            ShuffleConstraints {
                avoids: Some(&avoids),
                ..ShuffleConstraints::default()
            },
        )
        .expect("avoid-constrained shuffle succeeds");
        let team1_ids = team1
            .players
            .iter()
            .filter_map(|player| player.discord_id)
            .collect::<HashSet<_>>();
        let team2_ids = team2
            .players
            .iter()
            .filter_map(|player| player.discord_id)
            .collect::<HashSet<_>>();
        assert!(
            !((team1_ids.contains(&1000) && team1_ids.contains(&1001))
                || (team2_ids.contains(&1000) && team2_ids.contains(&1001)))
        );
    }

    #[test]
    fn test_low_priority_direction_changes_the_selected_split() {
        let mut players = soft_avoid_players(10);
        players[0].glicko_rating = Some(1_600.0);
        players[1].glicko_rating = Some(1_400.0);
        let avoids = [SoftAvoid {
            avoider_discord_id: 1000,
            avoided_discord_id: 1001,
        }];
        let shuffler = BalancedShuffler {
            consider_roles: false,
            rd_priority_weight: 0.0,
            soft_avoid_penalty: 150.0,
            ..BalancedShuffler::default()
        };
        let target_low_priority = HashSet::from([1001]);
        let both_low_priority = HashSet::from([1000, 1001]);
        let (target_team1, _) = shuffler
            .shuffle_with_constraints(
                &players,
                ShuffleConstraints {
                    avoids: Some(&avoids),
                    low_priority_ids: Some(&target_low_priority),
                    ..ShuffleConstraints::default()
                },
            )
            .expect("target-low-priority shuffle succeeds");
        let (both_team1, _) = shuffler
            .shuffle_with_constraints(
                &players,
                ShuffleConstraints {
                    avoids: Some(&avoids),
                    low_priority_ids: Some(&both_low_priority),
                    ..ShuffleConstraints::default()
                },
            )
            .expect("both-low-priority shuffle succeeds");
        let target_team1_ids = target_team1
            .players
            .iter()
            .filter_map(|player| player.discord_id)
            .collect::<HashSet<_>>();
        let both_team1_ids = both_team1
            .players
            .iter()
            .filter_map(|player| player.discord_id)
            .collect::<HashSet<_>>();
        assert_ne!(
            target_team1_ids.contains(&1000),
            target_team1_ids.contains(&1001)
        );
        assert_eq!(
            both_team1_ids.contains(&1000),
            both_team1_ids.contains(&1001)
        );
    }

    #[test]
    fn test_spread_penalty_calculation() {
        let shuffler = BalancedShuffler {
            use_glicko: false,
            consider_roles: false,
            ..BalancedShuffler::default()
        };
        approx(
            shuffler.calculate_rating_spread_penalty(&[1_000.0, 1_500.0, 2_000.0]),
            100.0,
        );
    }

    #[test]
    fn test_spread_penalty_zero_when_equal() {
        let shuffler = BalancedShuffler {
            use_glicko: false,
            consider_roles: false,
            ..BalancedShuffler::default()
        };
        assert_eq!(
            shuffler.calculate_rating_spread_penalty(&[1_500.0; 10]),
            0.0
        );
    }

    #[test]
    fn test_spread_penalty_single_player() {
        let shuffler = BalancedShuffler {
            use_glicko: false,
            consider_roles: false,
            ..BalancedShuffler::default()
        };
        assert_eq!(shuffler.calculate_rating_spread_penalty(&[1_500.0]), 0.0);
    }

    #[test]
    fn test_spread_penalty_empty() {
        let shuffler = BalancedShuffler {
            use_glicko: false,
            consider_roles: false,
            ..BalancedShuffler::default()
        };
        assert_eq!(shuffler.calculate_rating_spread_penalty(&[]), 0.0);
    }

    #[test]
    fn test_custom_divisor() {
        let shuffler = BalancedShuffler {
            use_glicko: false,
            consider_roles: false,
            rating_spread_divisor: 5.0,
            ..BalancedShuffler::default()
        };
        approx(
            shuffler.calculate_rating_spread_penalty(&[1_000.0, 2_000.0]),
            200.0,
        );
    }

    #[test]
    fn test_default_divisor_from_config() {
        let shuffler = BalancedShuffler {
            use_glicko: false,
            consider_roles: false,
            ..BalancedShuffler::default()
        };
        assert_eq!(shuffler.rating_spread_divisor, 10.0);
    }

    #[test]
    fn test_pool_shuffle_penalizes_outlier() {
        let mut players: Vec<Player> = (0..11)
            .map(|index| Player {
                mmr: Some(1_400 + index * 20),
                preferred_roles: role_list(&["1"]),
                discord_id: Some(100 + index),
                ..Player::new(format!("Close{index}"))
            })
            .collect();
        players.push(Player {
            mmr: Some(3_000),
            preferred_roles: role_list(&["1"]),
            discord_id: Some(200),
            ..Player::new("Outlier")
        });

        let result = BalancedShuffler {
            use_glicko: false,
            consider_roles: false,
            rating_spread_divisor: 1.0,
            ..BalancedShuffler::default()
        }
        .shuffle_from_pool(&players, PoolOptions::default())
        .expect("pool shuffle succeeds");
        assert!(
            result
                .excluded
                .iter()
                .any(|player| player.discord_id == Some(200))
        );
    }

    fn package_players(count: usize, rating: i64) -> Vec<Player> {
        (0..count)
            .map(|index| Player {
                mmr: Some(rating),
                glicko_rating: Some(rating as f64),
                preferred_roles: role_list(&["3"]),
                discord_id: Some(100 + index as i64),
                ..Player::new(format!("Player{index}"))
            })
            .collect()
    }

    fn package_team_ids() -> (HashSet<i64>, HashSet<i64>) {
        (
            HashSet::from([100, 101, 102, 103, 104]),
            HashSet::from([105, 106, 107, 108, 109]),
        )
    }

    #[test]
    fn test_package_deal_penalty_when_separated() {
        let (team1_ids, team2_ids) = package_team_ids();
        let deals = [PackageDeal {
            buyer_discord_id: 100,
            partner_discord_id: 105,
        }];
        let shuffler = BalancedShuffler::default();
        assert_eq!(
            shuffler.calculate_package_deal_penalty(&team1_ids, &team2_ids, Some(&deals), None,),
            90.0
        );
    }

    #[test]
    fn test_only_low_priority_buyer_gets_half_strength() {
        let (team1_ids, team2_ids) = package_team_ids();
        let deals = [PackageDeal {
            buyer_discord_id: 100,
            partner_discord_id: 105,
        }];
        let buyer_low_priority = HashSet::from([100]);
        let partner_low_priority = HashSet::from([105]);
        let shuffler = BalancedShuffler {
            package_deal_penalty: 100.0,
            package_deal_split_penalty: 80.0,
            ..BalancedShuffler::default()
        };
        assert_eq!(
            shuffler.calculate_package_deal_penalty(
                &team1_ids,
                &team2_ids,
                Some(&deals),
                Some(&buyer_low_priority),
            ),
            50.0
        );
        assert_eq!(
            shuffler.calculate_package_deal_penalty(
                &team1_ids,
                &team2_ids,
                Some(&deals),
                Some(&partner_low_priority),
            ),
            100.0
        );
        assert_eq!(
            shuffler.calculate_package_deal_split_penalty(
                &HashSet::from([100]),
                &HashSet::from([105]),
                Some(&deals),
                Some(&buyer_low_priority),
            ),
            40.0
        );
        assert_eq!(
            shuffler.calculate_package_deal_split_penalty(
                &HashSet::from([100]),
                &HashSet::from([105]),
                Some(&deals),
                Some(&partner_low_priority),
            ),
            80.0
        );
    }

    #[test]
    fn test_no_penalty_when_together() {
        let (team1_ids, team2_ids) = package_team_ids();
        let deals = [PackageDeal {
            buyer_discord_id: 100,
            partner_discord_id: 101,
        }];
        assert_eq!(
            BalancedShuffler::default().calculate_package_deal_penalty(
                &team1_ids,
                &team2_ids,
                Some(&deals),
                None,
            ),
            0.0
        );
    }

    #[test]
    fn test_multiple_deals_penalty_stacks() {
        let (team1_ids, team2_ids) = package_team_ids();
        let deals = [
            PackageDeal {
                buyer_discord_id: 100,
                partner_discord_id: 105,
            },
            PackageDeal {
                buyer_discord_id: 101,
                partner_discord_id: 106,
            },
        ];
        let shuffler = BalancedShuffler::default();
        assert_eq!(
            shuffler.calculate_package_deal_penalty(&team1_ids, &team2_ids, Some(&deals), None,),
            2.0 * shuffler.package_deal_penalty
        );
    }

    #[test]
    fn test_bidirectional_deals_stack() {
        let (team1_ids, team2_ids) = package_team_ids();
        let deals = [
            PackageDeal {
                buyer_discord_id: 100,
                partner_discord_id: 105,
            },
            PackageDeal {
                buyer_discord_id: 105,
                partner_discord_id: 100,
            },
        ];
        let shuffler = BalancedShuffler::default();
        assert_eq!(
            shuffler.calculate_package_deal_penalty(&team1_ids, &team2_ids, Some(&deals), None,),
            2.0 * shuffler.package_deal_penalty
        );
    }

    #[test]
    fn test_no_deals_no_penalty() {
        let (team1_ids, team2_ids) = package_team_ids();
        let shuffler = BalancedShuffler::default();
        assert_eq!(
            shuffler.calculate_package_deal_penalty(&team1_ids, &team2_ids, Some(&[]), None,),
            0.0
        );
        assert_eq!(
            shuffler.calculate_package_deal_penalty(&team1_ids, &team2_ids, None, None),
            0.0
        );
    }

    #[test]
    fn test_shuffle_respects_package_deals() {
        let players = package_players(10, 4_000);
        let deals = [PackageDeal {
            buyer_discord_id: 100,
            partner_discord_id: 105,
        }];
        let (team1, team2) = BalancedShuffler {
            package_deal_penalty: 10_000.0,
            ..BalancedShuffler::default()
        }
        .shuffle_with_constraints(
            &players,
            ShuffleConstraints {
                deals: Some(&deals),
                ..ShuffleConstraints::default()
            },
        )
        .expect("deal-constrained shuffle succeeds");
        let team1_ids: HashSet<i64> = team1
            .players
            .iter()
            .filter_map(|player| player.discord_id)
            .collect();
        let team2_ids: HashSet<i64> = team2
            .players
            .iter()
            .filter_map(|player| player.discord_id)
            .collect();
        assert!(
            (team1_ids.contains(&100) && team1_ids.contains(&105))
                || (team2_ids.contains(&100) && team2_ids.contains(&105))
        );
    }

    #[test]
    fn test_package_deal_penalty_customizable() {
        assert_eq!(
            BalancedShuffler {
                package_deal_penalty: 999.0,
                ..BalancedShuffler::default()
            }
            .package_deal_penalty,
            999.0
        );
    }

    #[test]
    fn test_package_deal_vs_soft_avoid_independence() {
        let (team1_ids, team2_ids) = package_team_ids();
        let deals = [PackageDeal {
            buyer_discord_id: 100,
            partner_discord_id: 105,
        }];
        let avoids = [SoftAvoid {
            avoider_discord_id: 100,
            avoided_discord_id: 101,
        }];
        let shuffler = BalancedShuffler::default();
        let deal_penalty =
            shuffler.calculate_package_deal_penalty(&team1_ids, &team2_ids, Some(&deals), None);
        let avoid_penalty =
            shuffler.calculate_soft_avoid_penalty(&team1_ids, &team2_ids, Some(&avoids), None);
        assert_eq!(deal_penalty, shuffler.package_deal_penalty);
        assert_eq!(avoid_penalty, shuffler.soft_avoid_penalty);
        assert_ne!(deal_penalty, avoid_penalty);
    }

    #[test]
    fn test_pool_shuffle_with_deals() {
        let players = package_players(11, 4_000);
        let deals = [PackageDeal {
            buyer_discord_id: 100,
            partner_discord_id: 105,
        }];
        let result = BalancedShuffler {
            package_deal_penalty: 5_000.0,
            package_deal_split_penalty: 5_000.0,
            ..BalancedShuffler::default()
        }
        .shuffle_from_pool(
            &players,
            PoolOptions {
                constraints: ShuffleConstraints {
                    deals: Some(&deals),
                    ..ShuffleConstraints::default()
                },
                ..PoolOptions::default()
            },
        )
        .expect("deal-constrained pool shuffle succeeds");
        let team1_ids: HashSet<i64> = result
            .team1
            .players
            .iter()
            .filter_map(|player| player.discord_id)
            .collect();
        let team2_ids: HashSet<i64> = result
            .team2
            .players
            .iter()
            .filter_map(|player| player.discord_id)
            .collect();
        let included: HashSet<i64> = team1_ids.union(&team2_ids).copied().collect();
        assert!(included.contains(&100) && included.contains(&105));
        assert!(
            (team1_ids.contains(&100) && team1_ids.contains(&105))
                || (team2_ids.contains(&100) && team2_ids.contains(&105))
        );
    }

    #[test]
    fn test_branch_bound_respects_split_penalty() {
        let players = package_players(14, 2_000);
        let deals = [PackageDeal {
            buyer_discord_id: 100,
            partner_discord_id: 110,
        }];
        let shuffler = BalancedShuffler {
            package_deal_split_penalty: 5_000.0,
            ..BalancedShuffler::default()
        };
        let result = shuffler
            .shuffle_branch_bound(
                &players,
                PoolOptions {
                    constraints: ShuffleConstraints {
                        deals: Some(&deals),
                        ..ShuffleConstraints::default()
                    },
                    ..PoolOptions::default()
                },
                BranchSearchOptions {
                    initial_upper_bound: Some(f64::INFINITY),
                    ..BranchSearchOptions::default()
                },
            )
            .expect("deal-constrained branch search succeeds");
        let included = selected_ids(&result);
        let excluded: HashSet<i64> = result
            .excluded
            .iter()
            .filter_map(|player| player.discord_id)
            .collect();
        assert!(
            (included.contains(&100) && included.contains(&110))
                || (excluded.contains(&100) && excluded.contains(&110))
        );
        assert!(shuffler.diagnostics().constraint_penalty_calls > 0);
    }

    #[test]
    fn test_greedy_shuffle_includes_split_penalty() {
        let players = package_players(14, 2_000);
        let deals = [PackageDeal {
            buyer_discord_id: 100,
            partner_discord_id: 113,
        }];
        let shuffler = BalancedShuffler {
            package_deal_split_penalty: 1_000.0,
            ..BalancedShuffler::default()
        };
        let with_deal = shuffler
            .greedy_shuffle(
                &players,
                PoolOptions {
                    constraints: ShuffleConstraints {
                        deals: Some(&deals),
                        ..ShuffleConstraints::default()
                    },
                    ..PoolOptions::default()
                },
            )
            .expect("deal-constrained greedy shuffle succeeds");
        let without_deal = shuffler
            .greedy_shuffle(&players, PoolOptions::default())
            .expect("unconstrained greedy shuffle succeeds");
        let included = selected_ids(&PoolResult {
            team1: with_deal.team1.clone(),
            team2: with_deal.team2.clone(),
            excluded: with_deal.excluded.clone(),
        });
        let excluded: HashSet<i64> = with_deal
            .excluded
            .iter()
            .filter_map(|player| player.discord_id)
            .collect();
        assert_ne!(included.contains(&100), included.contains(&113));
        assert_ne!(excluded.contains(&100), excluded.contains(&113));
        approx(
            with_deal.score - without_deal.score,
            shuffler.package_deal_split_penalty,
        );
    }

    fn package_split_ids() -> (HashSet<i64>, HashSet<i64>) {
        (
            HashSet::from([100, 101, 102, 103, 104, 105, 106, 107, 108, 109]),
            HashSet::from([110, 111, 112, 113]),
        )
    }

    #[test]
    fn test_split_penalty_when_one_excluded() {
        let (selected, excluded) = package_split_ids();
        let deals = [PackageDeal {
            buyer_discord_id: 100,
            partner_discord_id: 110,
        }];
        let shuffler = BalancedShuffler::default();
        assert_eq!(
            shuffler
                .calculate_package_deal_split_penalty(&selected, &excluded, Some(&deals), None,),
            90.0
        );
    }

    #[test]
    fn test_package_deal_split_adds_90_goodness() {
        let players = package_players(14, 2_000);
        let deals = [PackageDeal {
            buyer_discord_id: 100,
            partner_discord_id: 113,
        }];
        let shuffler = BalancedShuffler::default();
        let with_deal = shuffler
            .greedy_shuffle(
                &players,
                PoolOptions {
                    constraints: ShuffleConstraints {
                        deals: Some(&deals),
                        ..ShuffleConstraints::default()
                    },
                    ..PoolOptions::default()
                },
            )
            .expect("package-deal split scoring succeeds");
        let without_deal = shuffler
            .greedy_shuffle(&players, PoolOptions::default())
            .expect("baseline split scoring succeeds");
        let included = selected_ids(&PoolResult {
            team1: with_deal.team1.clone(),
            team2: with_deal.team2.clone(),
            excluded: with_deal.excluded.clone(),
        });
        let excluded = with_deal
            .excluded
            .iter()
            .filter_map(|player| player.discord_id)
            .collect::<HashSet<_>>();
        assert_ne!(included.contains(&100), included.contains(&113));
        assert_ne!(excluded.contains(&100), excluded.contains(&113));
        approx(
            with_deal.score - without_deal.score,
            BalancedShuffler::default().package_deal_split_penalty,
        );
    }

    #[test]
    fn test_no_split_penalty_when_both_selected() {
        let (selected, excluded) = package_split_ids();
        let deals = [PackageDeal {
            buyer_discord_id: 100,
            partner_discord_id: 101,
        }];
        assert_eq!(
            BalancedShuffler::default().calculate_package_deal_split_penalty(
                &selected,
                &excluded,
                Some(&deals),
                None,
            ),
            0.0
        );
    }

    #[test]
    fn test_no_split_penalty_when_both_excluded() {
        let (selected, excluded) = package_split_ids();
        let deals = [PackageDeal {
            buyer_discord_id: 110,
            partner_discord_id: 111,
        }];
        assert_eq!(
            BalancedShuffler::default().calculate_package_deal_split_penalty(
                &selected,
                &excluded,
                Some(&deals),
                None,
            ),
            0.0
        );
    }

    #[test]
    fn test_multiple_splits_stack() {
        let (selected, excluded) = package_split_ids();
        let deals = [
            PackageDeal {
                buyer_discord_id: 100,
                partner_discord_id: 110,
            },
            PackageDeal {
                buyer_discord_id: 101,
                partner_discord_id: 111,
            },
        ];
        let shuffler = BalancedShuffler::default();
        assert_eq!(
            shuffler
                .calculate_package_deal_split_penalty(&selected, &excluded, Some(&deals), None,),
            2.0 * shuffler.package_deal_split_penalty
        );
    }

    #[test]
    fn test_reverse_split_direction() {
        let (selected, excluded) = package_split_ids();
        let deals = [PackageDeal {
            buyer_discord_id: 110,
            partner_discord_id: 100,
        }];
        let shuffler = BalancedShuffler::default();
        assert_eq!(
            shuffler
                .calculate_package_deal_split_penalty(&selected, &excluded, Some(&deals), None,),
            shuffler.package_deal_split_penalty
        );
    }

    #[test]
    fn test_no_deals_no_split_penalty() {
        let (selected, excluded) = package_split_ids();
        let shuffler = BalancedShuffler::default();
        assert_eq!(
            shuffler.calculate_package_deal_split_penalty(&selected, &excluded, Some(&[]), None,),
            0.0
        );
        assert_eq!(
            shuffler.calculate_package_deal_split_penalty(&selected, &excluded, None, None),
            0.0
        );
    }

    #[test]
    fn test_split_penalty_customizable() {
        assert_eq!(
            BalancedShuffler {
                package_deal_split_penalty: 888.0,
                ..BalancedShuffler::default()
            }
            .package_deal_split_penalty,
            888.0
        );
    }

    #[test]
    fn test_pool_shuffle_prefers_keeping_deals_together() {
        let players: Vec<Player> = (0..12)
            .map(|index| {
                let rating = if index < 5 {
                    2_500
                } else if index < 7 {
                    2_000
                } else {
                    1_500
                };
                Player {
                    mmr: Some(rating),
                    glicko_rating: Some(rating as f64),
                    preferred_roles: role_list(&["3"]),
                    discord_id: Some(100 + index),
                    ..Player::new(format!("Player{index}"))
                }
            })
            .collect();
        let deals = [PackageDeal {
            buyer_discord_id: 105,
            partner_discord_id: 106,
        }];
        let result = BalancedShuffler {
            package_deal_split_penalty: 5_000.0,
            ..BalancedShuffler::default()
        }
        .shuffle_from_pool(
            &players,
            PoolOptions {
                constraints: ShuffleConstraints {
                    deals: Some(&deals),
                    ..ShuffleConstraints::default()
                },
                ..PoolOptions::default()
            },
        )
        .expect("deal-constrained pool shuffle succeeds");
        let included = selected_ids(&result);
        let excluded: HashSet<i64> = result
            .excluded
            .iter()
            .filter_map(|player| player.discord_id)
            .collect();
        assert!(
            (included.contains(&105) && included.contains(&106))
                || (excluded.contains(&105) && excluded.contains(&106))
        );
    }

    #[test]
    fn test_pool_shuffle_prefers_excluding_low_priority_player() {
        let players: Vec<Player> = (0..11)
            .map(|index| full_flex_player(format!("Player{index}"), 3_000, 100 + index))
            .collect();
        let shuffler = BalancedShuffler {
            use_glicko: false,
            exclusion_penalty_weight: 0.0,
            recent_match_penalty_weight: 0.0,
            rating_spread_divisor: 10.0,
            rd_priority_weight: 0.0,
            ..BalancedShuffler::default()
        };
        let low_priority = HashSet::from([100]);
        let options = PoolOptions {
            constraints: ShuffleConstraints {
                low_priority_ids: Some(&low_priority),
                ..ShuffleConstraints::default()
            },
            sampling_seed: 0,
            ..PoolOptions::default()
        };
        let result = shuffler
            .shuffle_from_pool(&players, options)
            .expect("pool shuffle succeeds");
        assert_eq!(
            result
                .excluded
                .iter()
                .filter_map(|player| player.discord_id)
                .collect::<HashSet<_>>(),
            HashSet::from([100])
        );
    }

    fn assert_long_waiter_selected(player_count: usize, long_waiter_id: i64) {
        let players: Vec<Player> = (0..player_count)
            .map(|index| full_flex_player(format!("Player{index}"), 1_500, 100 + index as i64))
            .collect();
        let shuffler = BalancedShuffler {
            use_glicko: false,
            exclusion_penalty_weight: 0.0,
            recent_match_penalty_weight: 0.0,
            rd_priority_weight: 0.0,
            ..BalancedShuffler::default()
        };
        let waits = HashMap::from([(long_waiter_id, 30)]);
        let result = shuffler
            .shuffle_from_pool(
                &players,
                PoolOptions {
                    lobby_wait_minutes: Some(&waits),
                    ..PoolOptions::default()
                },
            )
            .expect("pool shuffle succeeds");
        assert!(selected_ids(&result).contains(&long_waiter_id));
    }

    #[test]
    fn test_pool_shuffle_includes_player_with_longer_lobby_wait_11_109() {
        assert_long_waiter_selected(11, 109);
    }

    #[test]
    fn test_pool_shuffle_includes_player_with_longer_lobby_wait_14_100() {
        assert_long_waiter_selected(14, 100);
    }

    #[test]
    fn test_get_cached_role_assignments_reuses_immutable_cache_value() {
        let players: Vec<Player> = (0..5)
            .map(|index| {
                player(
                    format!("Player{index}"),
                    0,
                    &[match index {
                        0 => "1",
                        1 => "2",
                        2 => "3",
                        3 => "4",
                        _ => "5",
                    }],
                )
            })
            .collect();
        let expected = Arc::new(vec![default_role_assignment()]);
        let provider = Arc::new(RecordingProvider {
            calls: Mutex::new(Vec::new()),
            value: Arc::clone(&expected),
        });
        let shuffler = BalancedShuffler::default().with_role_assignment_provider(provider.clone());
        let first = shuffler.get_cached_role_assignments(&players);
        let second = shuffler.get_cached_role_assignments(&players);
        assert!(Arc::ptr_eq(&first, &expected));
        assert!(Arc::ptr_eq(&second, &expected));
        let calls = provider.calls.lock().expect("recording provider mutex");
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0],
            vec![
                vec!["1".to_owned()],
                vec!["2".to_owned()],
                vec!["3".to_owned()],
                vec!["4".to_owned()],
                vec!["5".to_owned()],
            ]
        );
        assert_eq!(calls[0], calls[1]);
    }

    #[test]
    fn test_pool_rating_bonus_keeps_high_rating_outlier() {
        let ratings = [
            1_000.0, 1_400.0, 1_450.0, 1_475.0, 1_500.0, 1_525.0, 1_550.0, 1_575.0, 1_600.0,
            1_650.0, 2_200.0,
        ];
        let players: Vec<Player> = ratings
            .into_iter()
            .enumerate()
            .map(|(index, rating)| Player {
                discord_id: Some(index as i64 + 1),
                glicko_rating: Some(rating),
                preferred_roles: role_list(&["1", "2", "3", "4", "5"]),
                ..Player::new(format!("Player{index}"))
            })
            .collect();
        let shuffler = BalancedShuffler {
            off_role_flat_penalty: 0.0,
            role_matchup_delta_weight: 0.0,
            exclusion_penalty_weight: 0.0,
            rd_priority_weight: 0.0,
            recent_match_penalty_weight: 0.0,
            rating_spread_divisor: 10.0,
            ..BalancedShuffler::default()
        };
        let result = shuffler
            .shuffle_from_pool(&players, PoolOptions::default())
            .expect("pool shuffle succeeds");
        assert_eq!(result.excluded.len(), 1);
        assert_eq!(result.excluded[0].glicko_rating, Some(1_475.0));
    }

    #[test]
    fn test_shuffle_evaluates_duplicate_display_names_by_identity() {
        let players: Vec<Player> = (0..10)
            .map(|index| Player {
                mmr: Some(1_500 + index),
                discord_id: Some(index + 1),
                ..Player::new("SameName")
            })
            .collect();
        let shuffler = configured_mmr_shuffler();
        shuffler.shuffle(&players).expect("shuffle succeeds");
        let diagnostics = shuffler.diagnostics();
        assert_eq!(diagnostics.fixed_splits_evaluated, 126);
        let unique: HashSet<Vec<usize>> = diagnostics.fixed_team1_keys.iter().cloned().collect();
        assert_eq!(unique.len(), 126);
        let first_key = std::ptr::from_ref(&players[0]).addr();
        assert!(
            diagnostics
                .fixed_team1_keys
                .iter()
                .all(|split| split.contains(&first_key))
        );
    }

    #[test]
    fn test_rd_priority_bonus_scales_with_sum() {
        let players = [
            Player {
                mmr: Some(1_500),
                glicko_rd: Some(200.0),
                ..Player::new("HighRD")
            },
            Player {
                mmr: Some(1_500),
                glicko_rd: Some(50.0),
                ..Player::new("MidRD")
            },
        ];
        let refs: Vec<&Player> = players.iter().collect();
        let shuffler = BalancedShuffler {
            rd_priority_weight: 0.1,
            ..BalancedShuffler::default()
        };
        approx(shuffler.calculate_rd_priority(&refs), 25.0);
    }

    #[test]
    fn test_shuffle_reuses_rd_priority_for_same_player_selection() {
        let players: Vec<Player> = (0..10)
            .map(|index| Player {
                discord_id: Some(index + 1),
                glicko_rating: Some(1_300.0 + index as f64 * 40.0),
                glicko_rd: Some(75.0 + index as f64),
                preferred_roles: role_list(&["1", "2", "3", "4", "5"]),
                ..Player::new(format!("Player{index}"))
            })
            .collect();
        let shuffler = BalancedShuffler {
            rd_priority_weight: 0.137,
            ..BalancedShuffler::default()
        };
        shuffler.shuffle(&players).expect("shuffle succeeds");
        assert_eq!(shuffler.diagnostics().rd_priority_calls, 1);
    }

    #[test]
    fn test_rd_priority_favors_high_rd_players_in_pool() {
        let mut players: Vec<Player> = (0..10)
            .map(|index| Player {
                mmr: Some(1_500),
                glicko_rating: Some(1_500.0),
                glicko_rd: Some(50.0),
                discord_id: Some(index + 1),
                ..Player::new(format!("Active{index}"))
            })
            .collect();
        players.push(Player {
            mmr: Some(1_500),
            glicko_rating: Some(1_500.0),
            glicko_rd: Some(350.0),
            discord_id: Some(11),
            ..Player::new("HighRD")
        });
        let shuffler = BalancedShuffler {
            rd_priority_weight: 1.0,
            ..BalancedShuffler::default()
        };
        let result = shuffler
            .shuffle_from_pool(&players, PoolOptions::default())
            .expect("pool shuffle succeeds");
        assert!(
            result
                .team1
                .players
                .iter()
                .chain(&result.team2.players)
                .any(|player| player.name == "HighRD")
        );
        assert!(result.excluded.iter().all(|player| player.name != "HighRD"));
    }

    #[test]
    fn test_shuffle_wrong_number_of_players() {
        let players: Vec<Player> = (0..9)
            .map(|index| player(format!("Player{index}"), 1_500, &[]))
            .collect();
        assert!(BalancedShuffler::default().shuffle(&players).is_err());
    }

    #[test]
    fn test_shuffle_balanced_teams() {
        let players: Vec<Player> = (0..10)
            .map(|index| player(format!("P{}", index + 1), 2_000 - index * 100, &[]))
            .collect();
        let shuffler = BalancedShuffler {
            use_glicko: false,
            off_role_flat_penalty: 50.0,
            ..BalancedShuffler::default()
        };
        let (team1, team2) = shuffler.shuffle(&players).expect("shuffle succeeds");
        assert_eq!(team1.players.len(), 5);
        assert_eq!(team2.players.len(), 5);
        let all_names: HashSet<String> = team1
            .players
            .iter()
            .chain(&team2.players)
            .map(|player| player.name.clone())
            .collect();
        assert_eq!(all_names, names(&players));
        let value1 = team1
            .get_team_value(false, 1.0, false, false)
            .expect("roles assigned");
        let value2 = team2
            .get_team_value(false, 1.0, false, false)
            .expect("roles assigned");
        assert!((value1 - value2).abs() <= 100.0);
    }

    #[test]
    fn test_shuffle_with_roles() {
        let players: Vec<Player> = (0..10)
            .map(|index| {
                let role = ["1", "2", "3", "4", "5"][index % 5];
                player(
                    format!("P{}", index + 1),
                    2_000 - index as i64 * 100,
                    &[role],
                )
            })
            .collect();
        let shuffler = BalancedShuffler {
            use_glicko: false,
            off_role_flat_penalty: 50.0,
            ..BalancedShuffler::default()
        };
        let (team1, team2) = shuffler.shuffle(&players).expect("shuffle succeeds");
        assert_eq!(team1.role_assignments.as_ref().map(Vec::len), Some(5));
        assert_eq!(team2.role_assignments.as_ref().map(Vec::len), Some(5));
    }

    #[test]
    fn test_region_split_mode_separates_usw_and_use() {
        let mut players = Vec::new();
        for index in 0..5 {
            players.push(Player {
                preferred_region: Some("USW".to_owned()),
                ..full_flex_player(format!("USW{index}"), 1_500, index + 1)
            });
            players.push(Player {
                preferred_region: Some("USE".to_owned()),
                ..full_flex_player(format!("USE{index}"), 1_500, index + 6)
            });
        }
        let shuffler = BalancedShuffler {
            use_glicko: false,
            off_role_flat_penalty: 0.0,
            role_matchup_delta_weight: 0.0,
            rd_priority_weight: 0.0,
            region_split: true,
            region_split_penalty: 1_000.0,
            ..BalancedShuffler::default()
        };
        let (team1, team2) = shuffler.shuffle(&players).expect("shuffle succeeds");
        let refs1: Vec<&Player> = team1.players.iter().collect();
        let refs2: Vec<&Player> = team2.players.iter().collect();
        assert_eq!(region_split_mismatches(&refs1, &refs2), 0);
        let mut regions = [&team1, &team2]
            .into_iter()
            .map(|team| {
                let mut values = team
                    .players
                    .iter()
                    .filter_map(super::resolved_region)
                    .collect::<Vec<_>>();
                values.sort();
                values.dedup();
                values
            })
            .collect::<Vec<_>>();
        regions.sort();
        assert_eq!(regions, vec![vec!["USE"], vec!["USW"]]);
    }

    fn assert_pool_size(pool_size: usize) {
        let players: Vec<Player> = (0..pool_size)
            .map(|index| player(format!("Player{index}"), 1_500 + index as i64 * 10, &[]))
            .collect();
        let result = BalancedShuffler {
            use_glicko: false,
            off_role_flat_penalty: 50.0,
            ..BalancedShuffler::default()
        }
        .shuffle_from_pool(&players, PoolOptions::default())
        .expect("pool shuffle succeeds");
        assert_eq!(result.team1.players.len(), 5);
        assert_eq!(result.team2.players.len(), 5);
        assert_eq!(result.excluded.len(), pool_size - 10);
        let all: HashSet<String> = result
            .team1
            .players
            .iter()
            .chain(&result.team2.players)
            .chain(&result.excluded)
            .map(|player| player.name.clone())
            .collect();
        assert_eq!(all, names(&players));
    }

    #[test]
    fn test_shuffle_from_pool_12() {
        assert_pool_size(12);
    }

    #[test]
    fn test_shuffle_from_pool_13() {
        assert_pool_size(13);
    }

    #[test]
    fn test_large_pool_bounds_full_role_matchup_evaluations() {
        let roles = [
            &["1"][..],
            &["2"][..],
            &["3"][..],
            &["3"][..],
            &["4"][..],
            &["5"][..],
            &["1", "2"][..],
            &["3", "4"][..],
            &["4", "5"][..],
        ];
        let players: Vec<Player> = (0..20)
            .map(|index| Player {
                discord_id: Some(1_000 + index),
                mmr: Some(1_500 + index * 40),
                glicko_rating: Some(1_500.0 + index as f64 * 20.0),
                glicko_rd: Some(50.0 + index as f64),
                preferred_roles: role_list(roles[index as usize % roles.len()]),
                ..Player::new(format!("Player{index}"))
            })
            .collect();
        let shuffler = BalancedShuffler::default();
        let result = shuffler
            .shuffle_from_pool(
                &players,
                PoolOptions {
                    sampling_seed: 42,
                    ..PoolOptions::default()
                },
            )
            .expect("large pool shuffle succeeds");
        assert_eq!(result.team1.players.len(), 5);
        assert_eq!(result.team2.players.len(), 5);
        assert_eq!(result.excluded.len(), 10);
        assert!(shuffler.diagnostics().pool_matchups_evaluated <= 4_000);
    }

    #[test]
    fn test_pool_signature_only_built_for_numeric_improvements() {
        let players: Vec<Player> = (0..11)
            .map(|index| full_flex_player(format!("Player{index}"), 1_500 + index, index + 1))
            .collect();
        let shuffler = configured_mmr_shuffler();
        let prefixes = [(30.0, 3.0, 1), (40.0, 1.0, 0), (20.0, 10.0, 2)];
        let expected = Arc::new(Mutex::new(Vec::<String>::new()));
        let expected_hook = Arc::clone(&expected);
        let mut hook = move |index: usize, team1: &[&Player], _team2: &[&Player]| {
            let (score, diff, off) = prefixes.get(index).copied().unwrap_or((50.0, 0.0, 0));
            if index == 2 {
                *expected_hook.lock().expect("expected mutex") =
                    team1.iter().map(|player| player.name.clone()).collect();
            }
            Some(PoolScoreOverride {
                total_score: score,
                value_diff: diff,
                total_off_roles: off,
                signature: None,
            })
        };
        let result = shuffler
            .shuffle_from_pool_with_score_hook(&players, PoolOptions::default(), &mut hook)
            .expect("hooked pool shuffle succeeds");
        assert_eq!(shuffler.diagnostics().pool_signatures_built, 2);
        assert_eq!(
            team_names(&result.team1),
            *expected.lock().expect("expected mutex")
        );
    }

    #[test]
    fn test_pool_signature_built_for_exact_numeric_tie() {
        let players: Vec<Player> = (0..11)
            .map(|index| full_flex_player(format!("Player{index}"), 1_500 + index, index + 1))
            .collect();
        let shuffler = configured_mmr_shuffler();
        let expected = Arc::new(Mutex::new(Vec::<String>::new()));
        let expected_hook = Arc::clone(&expected);
        let mut hook = move |index: usize, team1: &[&Player], _team2: &[&Player]| {
            if index == 1 {
                *expected_hook.lock().expect("expected mutex") =
                    team1.iter().map(|player| player.name.clone()).collect();
            }
            Some(if index < 2 {
                PoolScoreOverride {
                    total_score: 10.0,
                    value_diff: 2.0,
                    total_off_roles: 1,
                    signature: Some(PoolSignature(
                        vec![if index == 0 { "z" } else { "a" }.to_owned()],
                        Vec::new(),
                        Vec::new(),
                    )),
                }
            } else {
                PoolScoreOverride {
                    total_score: 20.0,
                    value_diff: 0.0,
                    total_off_roles: 0,
                    signature: None,
                }
            })
        };
        let result = shuffler
            .shuffle_from_pool_with_score_hook(&players, PoolOptions::default(), &mut hook)
            .expect("hooked pool shuffle succeeds");
        assert_eq!(shuffler.diagnostics().pool_signatures_built, 2);
        assert_eq!(
            team_names(&result.team1),
            *expected.lock().expect("expected mutex")
        );
    }

    #[test]
    fn test_shuffle_from_pool_less_than_10() {
        let players: Vec<Player> = (0..9)
            .map(|index| player(format!("Player{index}"), 1_500, &[]))
            .collect();
        assert!(
            BalancedShuffler::default()
                .shuffle_from_pool(&players, PoolOptions::default())
                .is_err()
        );
    }

    #[test]
    fn test_off_role_penalty_applied() {
        let mut players = vec![
            player("CarryA", 900, &["1"]),
            player("CarryB", 1_100, &["1"]),
        ];
        for (index, role) in ["2", "2", "3", "3", "4", "4", "5", "5"]
            .into_iter()
            .enumerate()
        {
            players.push(player(format!("Filler{index}"), 1_000, &[role]));
        }
        let shuffler = BalancedShuffler {
            use_glicko: false,
            off_role_flat_penalty: 1_000.0,
            role_matchup_delta_weight: 0.0,
            ..BalancedShuffler::default()
        };
        let (team1, team2) = shuffler.shuffle(&players).expect("shuffle succeeds");
        assert_eq!(
            team1.get_off_role_count().expect("roles assigned")
                + team2.get_off_role_count().expect("roles assigned"),
            0
        );
    }

    #[test]
    fn test_default_670_off_role_goodness_prefers_lower_off_role_roster() {
        let configured_shuffler = |off_role_flat_penalty| {
            let mut shuffler = BalancedShuffler {
                use_glicko: false,
                off_role_multiplier: 1.0,
                off_role_flat_value_penalty: 0.0,
                role_matchup_delta_weight: 0.0,
                exclusion_penalty_weight: 0.0,
                rd_priority_weight: 0.0,
                recent_match_penalty_weight: 0.0,
                rating_spread_divisor: 1_000_000_000.0,
                ..BalancedShuffler::default()
            };
            if let Some(value) = off_role_flat_penalty {
                shuffler.off_role_flat_penalty = value;
            }
            shuffler.with_tie_breaker(Arc::new(|_| 0))
        };
        let fixture = [
            (3_685, "1"),
            (5_667, "1"),
            (1_791, "1"),
            (5_477, "2"),
            (5_187, "2"),
            (2_118, "3"),
            (1_266, "3"),
            (4_628, "4"),
            (2_244, "4"),
            (1_270, "5"),
            (5_644, "5"),
        ];
        let players = fixture
            .into_iter()
            .enumerate()
            .map(|(index, (mmr, role))| Player {
                discord_id: Some(i64::try_from(index + 1).expect("fixture index fits")),
                ..player(format!("Player{index}"), mmr, &[role])
            })
            .collect::<Vec<_>>();
        let previous = configured_shuffler(Some(550.0))
            .shuffle_from_pool(&players, PoolOptions::default())
            .expect("550-point pool shuffle");
        let actual = configured_shuffler(None)
            .shuffle_from_pool(&players, PoolOptions::default())
            .expect("default pool shuffle");
        let excluded_ids = |result: &PoolResult| {
            result
                .excluded
                .iter()
                .filter_map(|player| player.discord_id)
                .collect::<HashSet<_>>()
        };
        let off_roles = |result: &PoolResult| {
            result.team1.get_off_role_count().expect("team 1 roles")
                + result.team2.get_off_role_count().expect("team 2 roles")
        };

        assert_eq!(excluded_ids(&previous), HashSet::from([7]));
        assert_eq!(off_roles(&previous), 1);
        assert_eq!(excluded_ids(&actual), HashSet::from([3]));
        assert_eq!(
            off_roles(&actual),
            0,
            "the 670-point default should prefer the lower-off-role roster"
        );
    }

    #[test]
    fn test_role_assignments_consider_matchup_delta() {
        let team1 = [
            player("HighMid", 1_791, &["2", "1", "5"]),
            player("FlexMid", 1_409, &["1", "2", "3", "4", "5"]),
            player("Offlane", 1_560, &["3"]),
            player("Soft", 1_464, &["4"]),
            player("Hard", 1_791, &["5"]),
        ];
        let team2 = [
            player("DireCarry", 1_837, &["1"]),
            player("DireMid", 1_973, &["2"]),
            player("DireOfflane", 1_462, &["3"]),
            player("DireSoft", 1_234, &["4"]),
            player("DireHard", 1_070, &["5"]),
        ];
        let shuffler = BalancedShuffler {
            use_glicko: false,
            off_role_multiplier: 1.0,
            ..BalancedShuffler::default()
        };
        let (roles, _, _) = shuffler
            .score_role_assignments_for_matchup(&team1, &team2, 20, no_rating_constraints())
            .expect("score succeeds");
        let roles = roles.expect("assignment selected");
        assert_eq!(roles[0], "2");
        assert_ne!(roles[1], "2");
    }

    #[test]
    fn test_shuffle_from_pool_with_exclusion_counts() {
        let players: Vec<Player> = (0..11)
            .map(|index| full_flex_player(format!("Player{index}"), 1_500, index + 1))
            .collect();
        let mut counts: HashMap<String, usize> = players
            .iter()
            .map(|player| (player.name.clone(), 5))
            .collect();
        counts.insert("Player0".to_owned(), 0);
        let shuffler = BalancedShuffler {
            use_glicko: false,
            exclusion_penalty_weight: 500.0,
            ..BalancedShuffler::default()
        };
        let result = shuffler
            .shuffle_from_pool(
                &players,
                PoolOptions {
                    exclusion_counts: Some(&counts),
                    ..PoolOptions::default()
                },
            )
            .expect("pool shuffle succeeds");
        assert_eq!(
            result
                .excluded
                .iter()
                .map(|player| player.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Player0"]
        );
    }

    #[test]
    fn test_greedy_shuffle_excludes_lowest_effective_exclusion_count() {
        let players: Vec<Player> = (0..12)
            .map(|index| {
                player(
                    format!("Player{index}"),
                    1_000 + index * 200,
                    &[["1", "2", "3", "4", "5"][index as usize % 5]],
                )
            })
            .collect();
        let mut counts: HashMap<String, usize> = players
            .iter()
            .map(|player| (player.name.clone(), 5))
            .collect();
        counts.insert("Player8".to_owned(), 0);
        counts.insert("Player9".to_owned(), 0);
        let shuffler = BalancedShuffler {
            use_glicko: false,
            off_role_flat_penalty: 50.0,
            exclusion_penalty_weight: 5.0,
            ..BalancedShuffler::default()
        };
        let result = shuffler
            .greedy_shuffle(
                &players,
                PoolOptions {
                    exclusion_counts: Some(&counts),
                    ..PoolOptions::default()
                },
            )
            .expect("greedy shuffle succeeds");
        assert_eq!(
            names(&result.excluded),
            HashSet::from(["Player8".to_owned(), "Player9".to_owned()])
        );
        assert_eq!(result.team1.players.len(), 5);
        assert_eq!(result.team2.players.len(), 5);
    }

    #[test]
    fn test_zero_exclusion_penalty_weight_with_recent_participants() {
        let players: Vec<Player> = (0..12)
            .map(|index| {
                player(
                    format!("Player{index}"),
                    1_000 + index * 200,
                    &[["1", "2", "3", "4", "5"][index as usize % 5]],
                )
            })
            .collect();
        let counts: HashMap<String, usize> = players
            .iter()
            .enumerate()
            .map(|(index, player)| (player.name.clone(), index))
            .collect();
        let recent = HashSet::from([
            "Player0".to_owned(),
            "Player3".to_owned(),
            "Player7".to_owned(),
        ]);
        let result = BalancedShuffler {
            use_glicko: false,
            off_role_flat_penalty: 50.0,
            exclusion_penalty_weight: 0.0,
            ..BalancedShuffler::default()
        }
        .greedy_shuffle(
            &players,
            PoolOptions {
                exclusion_counts: Some(&counts),
                recent_match_names: Some(&recent),
                ..PoolOptions::default()
            },
        )
        .expect("zero-weight greedy shuffle succeeds");
        assert_eq!(result.team1.players.len(), 5);
        assert_eq!(result.team2.players.len(), 5);
        assert_eq!(result.excluded.len(), 2);
    }

    fn recent_penalty_players() -> Vec<Player> {
        (0..14)
            .map(|index| {
                full_flex_player(format!("Player{index}"), 3_000 + index * 100, 1_000 + index)
            })
            .collect()
    }

    #[test]
    fn test_config_default_value() {
        assert_eq!(
            BalancedShuffler::default().recent_match_penalty_weight,
            230.0
        );
    }

    #[test]
    fn test_shuffler_uses_default_from_config() {
        assert_eq!(
            BalancedShuffler::default().recent_match_penalty_weight,
            230.0
        );
    }

    #[test]
    fn test_shuffler_accepts_custom_weight() {
        assert_eq!(
            BalancedShuffler {
                recent_match_penalty_weight: 50.0,
                ..BalancedShuffler::default()
            }
            .recent_match_penalty_weight,
            50.0
        );
    }

    #[test]
    fn test_shuffler_zero_weight_disables_penalty() {
        assert_eq!(
            BalancedShuffler {
                recent_match_penalty_weight: 0.0,
                ..BalancedShuffler::default()
            }
            .recent_match_penalty_weight,
            0.0
        );
    }

    #[test]
    fn test_greedy_shuffle_prefers_excluding_recent_players() {
        let players = recent_penalty_players();
        let recent = HashSet::from([players[12].name.clone(), players[13].name.clone()]);
        let counts = players
            .iter()
            .map(|player| (player.name.clone(), 5))
            .collect::<HashMap<_, _>>();
        let result = BalancedShuffler {
            use_glicko: false,
            recent_match_penalty_weight: 100.0,
            exclusion_penalty_weight: 50.0,
            ..BalancedShuffler::default()
        }
        .greedy_shuffle(
            &players,
            PoolOptions {
                exclusion_counts: Some(&counts),
                recent_match_names: Some(&recent),
                ..PoolOptions::default()
            },
        )
        .expect("greedy shuffle");
        assert!(recent.is_subset(&names(&result.excluded)));
    }

    #[test]
    fn test_penalty_disabled_when_zero() {
        let players = recent_penalty_players();
        let recent = HashSet::from([players[0].name.clone(), players[1].name.clone()]);
        let counts = players
            .iter()
            .map(|player| (player.name.clone(), 0))
            .collect::<HashMap<_, _>>();
        let shuffler = BalancedShuffler {
            use_glicko: false,
            recent_match_penalty_weight: 0.0,
            exclusion_penalty_weight: 50.0,
            ..BalancedShuffler::default()
        };
        let with_recent = shuffler
            .greedy_shuffle(
                &players,
                PoolOptions {
                    exclusion_counts: Some(&counts),
                    recent_match_names: Some(&recent),
                    ..PoolOptions::default()
                },
            )
            .expect("shuffle with recent players");
        let without_recent = shuffler
            .greedy_shuffle(
                &players,
                PoolOptions {
                    exclusion_counts: Some(&counts),
                    ..PoolOptions::default()
                },
            )
            .expect("shuffle without recent players");
        assert_eq!(
            names(&with_recent.excluded),
            names(&without_recent.excluded)
        );
        approx(with_recent.score, without_recent.score);
    }

    #[test]
    fn test_greedy_shuffle_includes_recent_penalty() {
        let players = recent_penalty_players();
        let recent = players
            .iter()
            .map(|player| player.name.clone())
            .collect::<HashSet<_>>();
        let counts = players
            .iter()
            .map(|player| (player.name.clone(), 0))
            .collect::<HashMap<_, _>>();
        let shuffler = BalancedShuffler {
            use_glicko: false,
            recent_match_penalty_weight: 25.0,
            ..BalancedShuffler::default()
        };
        let with_recent = shuffler
            .greedy_shuffle(
                &players,
                PoolOptions {
                    exclusion_counts: Some(&counts),
                    recent_match_names: Some(&recent),
                    ..PoolOptions::default()
                },
            )
            .expect("shuffle with recent players");
        let without_recent = shuffler
            .greedy_shuffle(
                &players,
                PoolOptions {
                    exclusion_counts: Some(&counts),
                    ..PoolOptions::default()
                },
            )
            .expect("shuffle without recent players");
        approx(with_recent.score - without_recent.score, 250.0);
    }

    #[test]
    fn test_branch_bound_prefers_excluding_recent_players() {
        let players: Vec<Player> = (0..14)
            .map(|index| full_flex_player(format!("Player{index}"), 1_500, 1_000 + index))
            .collect();
        let recent = players[..4]
            .iter()
            .map(|player| player.name.clone())
            .collect::<HashSet<_>>();
        let counts = players
            .iter()
            .map(|player| (player.name.clone(), 0))
            .collect::<HashMap<_, _>>();
        let result = BalancedShuffler {
            use_glicko: false,
            recent_match_penalty_weight: 50.0,
            exclusion_penalty_weight: 0.0,
            rd_priority_weight: 0.0,
            ..BalancedShuffler::default()
        }
        .shuffle_from_pool(
            &players,
            PoolOptions {
                exclusion_counts: Some(&counts),
                recent_match_names: Some(&recent),
                ..PoolOptions::default()
            },
        )
        .expect("branch-bound shuffle");
        assert_eq!(names(&result.excluded), recent);
    }

    #[test]
    fn test_high_exclusion_count_overrides_recent_penalty() {
        let players = recent_penalty_players();
        let recent = HashSet::from([players[0].name.clone()]);
        let mut counts = players
            .iter()
            .map(|player| (player.name.clone(), 0))
            .collect::<HashMap<_, _>>();
        counts.insert(players[0].name.clone(), 100);
        let result = BalancedShuffler {
            use_glicko: false,
            recent_match_penalty_weight: 25.0,
            exclusion_penalty_weight: 50.0,
            ..BalancedShuffler::default()
        }
        .greedy_shuffle(
            &players,
            PoolOptions {
                exclusion_counts: Some(&counts),
                recent_match_names: Some(&recent),
                ..PoolOptions::default()
            },
        )
        .expect("greedy shuffle");
        let selected = names(&result.team1.players)
            .into_iter()
            .chain(names(&result.team2.players))
            .collect::<HashSet<_>>();
        assert!(selected.contains(&players[0].name));
        assert!(!names(&result.excluded).contains(&players[0].name));
    }

    #[test]
    fn test_default_role_matchup_delta_weight_is_point_eighteen() {
        approx(BalancedShuffler::default().role_matchup_delta_weight, 0.18);
    }

    fn role_delta_fixture() -> (Vec<Player>, Vec<Player>) {
        (
            vec![
                player("Carry1", 2_000, &["1"]),
                player("Mid1", 1_500, &["2"]),
                player("Offlane1", 1_200, &["3"]),
                player("Sup1", 1_100, &["4"]),
                player("Sup2", 1_000, &["5"]),
            ],
            vec![
                player("Carry2", 1_000, &["1"]),
                player("Mid2", 1_500, &["2"]),
                player("Offlane2", 1_900, &["3"]),
                player("Sup3", 1_050, &["4"]),
                player("Sup4", 950, &["5"]),
            ],
        )
    }

    #[test]
    fn test_role_matchup_delta_weight_applied_in_shuffler_scoring() {
        let team1 = vec![
            player("RadiantCarry", 2_000, &["1"]),
            player("RadiantMid", 1_500, &["2"]),
            player("RadiantOfflane", 1_000, &["3"]),
            player("RadiantSoft", 1_000, &["4"]),
            player("RadiantHard", 1_000, &["5"]),
        ];
        let team2 = vec![
            player("DireCarry", 1_400, &["1"]),
            player("DireMid", 1_500, &["2"]),
            player("DireOfflane", 1_900, &["3"]),
            player("DireSoft", 1_000, &["4"]),
            player("DireHard", 1_000, &["5"]),
        ];
        let score = |weight| {
            BalancedShuffler {
                use_glicko: false,
                off_role_multiplier: 1.0,
                off_role_flat_penalty: 0.0,
                role_matchup_delta_weight: weight,
                ..BalancedShuffler::default()
            }
            .optimize_role_assignments_for_matchup(&team1, &team2, 1, no_rating_constraints())
            .expect("optimization succeeds")
            .2
        };
        approx(score(1.0), 2_330.0);
        approx(score(0.5), 1_330.0);
    }

    #[test]
    fn test_pool_log_entry_uses_weighted_parity_penalty() {
        let team1 = vec![
            player("RadiantCarry", 2_000, &["1"]),
            player("RadiantMid", 1_500, &["2"]),
            player("RadiantOfflane", 1_000, &["3"]),
            player("RadiantSoft", 1_000, &["4"]),
            player("RadiantHard", 1_000, &["5"]),
        ];
        let team2 = vec![
            player("DireCarry", 1_400, &["1"]),
            player("DireMid", 1_300, &["2"]),
            player("DireOfflane", 1_900, &["3"]),
            player("DireSoft", 1_000, &["4"]),
            player("DireHard", 1_000, &["5"]),
        ];
        let matchup = BalancedShuffler {
            use_glicko: false,
            off_role_multiplier: 1.0,
            off_role_flat_penalty: 0.0,
            role_matchup_delta_weight: 0.5,
            ..BalancedShuffler::default()
        }
        .evaluate_pool_matchup(
            &team1,
            &team2,
            0.0,
            0.0,
            &[],
            0.0,
            0.0,
            1,
            no_rating_constraints(),
        )
        .expect("matchup evaluates");
        approx(matchup.log_entry.parity_penalty, 1_200.0);
        approx(matchup.total_score, 1_310.0);
    }

    #[test]
    fn test_support_cross_lane_delta_in_shuffler() {
        let team1 = [
            player("RadiantCarry", 1_500, &["1"]),
            player("RadiantMid", 1_500, &["2"]),
            player("RadiantOfflane", 1_500, &["3"]),
            player("RadiantSoft", 1_300, &["4"]),
            player("RadiantHard", 900, &["5"]),
        ];
        let team2 = [
            player("DireCarry", 1_500, &["1"]),
            player("DireMid", 1_500, &["2"]),
            player("DireOfflane", 1_500, &["3"]),
            player("DireSoft", 1_100, &["4"]),
            player("DireHard", 1_200, &["5"]),
        ];
        let shuffler = BalancedShuffler {
            use_glicko: false,
            off_role_multiplier: 1.0,
            ..BalancedShuffler::default()
        };
        let refs1: Vec<&Player> = team1.iter().collect();
        let refs2: Vec<&Player> = team2.iter().collect();
        let mut context = super::ScoringContext::default();
        let metrics1 = shuffler.assigned_metrics(&refs1, &default_role_assignment(), &mut context);
        let metrics2 = shuffler.assigned_metrics(&refs2, &default_role_assignment(), &mut context);
        assert_eq!(
            crate::team_balancing::role_matchup_delta_from_values(
                &metrics1.role_values,
                &metrics2.role_values
            ),
            300.0
        );
    }

    #[test]
    fn test_service_and_shuffler_metrics_helpers_agree() {
        let (players1, players2) = role_delta_fixture();
        let roles = Some(default_role_assignment().to_vec());
        let team1 = Team::new(players1.clone(), roles.clone()).expect("team");
        let team2 = Team::new(players2.clone(), roles).expect("team");
        let service = TeamBalancingService::new(false, 1.0, 50.0, 1.0);
        assert_eq!(
            service
                .calculate_role_matchup_delta(&team1, &team2, false, false)
                .expect("roles assigned"),
            500.0
        );
        assert_eq!(
            service
                .calculate_role_parity_delta(&team1, &team2, false, false)
                .expect("roles assigned"),
            1_800.0
        );
        let matchup = BalancedShuffler {
            use_glicko: false,
            off_role_multiplier: 1.0,
            role_matchup_delta_weight: 1.0,
            ..BalancedShuffler::default()
        }
        .evaluate_pool_matchup(
            &players1,
            &players2,
            0.0,
            0.0,
            &[],
            0.0,
            0.0,
            1,
            no_rating_constraints(),
        )
        .expect("matchup evaluates");
        approx(matchup.log_entry.parity_penalty, 2_300.0);
    }

    #[test]
    fn test_inlined_hot_loops_match_shared_formulas() {
        let (mut players1, mut players2) = role_delta_fixture();
        for (index, player) in players1.iter_mut().chain(players2.iter_mut()).enumerate() {
            player.discord_id = Some(index as i64 + 1);
        }
        let shuffler = BalancedShuffler {
            use_glicko: false,
            off_role_multiplier: 1.0,
            off_role_flat_penalty: 0.0,
            role_matchup_delta_weight: 0.5,
            rd_priority_weight: 0.0,
            ..BalancedShuffler::default()
        };
        let common = shuffler
            .score_role_assignments_for_matchup(&players1, &players2, 1, no_rating_constraints())
            .expect("common scoring succeeds");
        let avoid = [SoftAvoid {
            avoider_discord_id: 1,
            avoided_discord_id: 6,
        }];
        let fallback = shuffler
            .score_role_assignments_for_matchup(
                &players1,
                &players2,
                1,
                ShuffleConstraints {
                    avoids: Some(&avoid),
                    ..ShuffleConstraints::default()
                },
            )
            .expect("fallback scoring succeeds");
        approx(common.2, 1_590.0);
        assert_eq!(common, fallback);
    }

    fn create_players_with_roles(count: usize, base: i64, spread: i64) -> Vec<Player> {
        let roles: [&[&str]; 8] = [
            &["1"],
            &["2"],
            &["3"],
            &["4"],
            &["5"],
            &["1", "2"],
            &["3", "4"],
            &["4", "5"],
        ];
        (0..count)
            .map(|index| Player {
                mmr: Some(base + index as i64 * spread),
                glicko_rating: Some((base + index as i64 * spread / 2) as f64),
                preferred_roles: role_list(roles[index % roles.len()]),
                ..Player::new(format!("Player{index}"))
            })
            .collect()
    }

    #[test]
    fn test_common_path_skips_constraint_penalty_work() {
        let players = create_players_with_roles(10, 1_500, 50);
        let shuffler = BalancedShuffler::default();
        let result = shuffler
            .score_role_assignments_for_matchup(
                &players[..5],
                &players[5..],
                5,
                no_rating_constraints(),
            )
            .expect("scoring succeeds");
        assert!(result.0.is_some() && result.1.is_some() && result.2.is_finite());
        let diagnostics = shuffler.diagnostics();
        assert_eq!(diagnostics.constraint_penalty_calls, 0);
        assert_eq!(diagnostics.unconstrained_score_calls, 1);
    }

    fn assert_constraint_uses_fallback(kind: &str) {
        let mut players = create_players_with_roles(10, 1_500, 50);
        for (index, player) in players.iter_mut().enumerate() {
            player.discord_id = Some(index as i64 + 1);
        }
        let avoids = [SoftAvoid {
            avoider_discord_id: 1,
            avoided_discord_id: 6,
        }];
        let deals = [PackageDeal {
            buyer_discord_id: 1,
            partner_discord_id: 2,
        }];
        let shuffler = BalancedShuffler {
            region_split: kind == "region",
            ..BalancedShuffler::default()
        };
        let constraints = ShuffleConstraints {
            avoids: (kind == "avoid").then_some(&avoids[..]),
            deals: (kind == "deal").then_some(&deals[..]),
            low_priority_ids: None,
        };
        let result = shuffler
            .score_role_assignments_for_matchup(&players[..5], &players[5..], 3, constraints)
            .expect("scoring succeeds");
        assert!(result.0.is_some() && result.1.is_some() && result.2.is_finite());
        assert_eq!(shuffler.diagnostics().constrained_score_calls, 1);
        assert_eq!(shuffler.diagnostics().unconstrained_score_calls, 0);
    }

    #[test]
    fn test_configured_constraints_retain_general_fallback_avoid() {
        assert_constraint_uses_fallback("avoid");
    }

    #[test]
    fn test_configured_constraints_retain_general_fallback_deal() {
        assert_constraint_uses_fallback("deal");
    }

    #[test]
    fn test_configured_constraints_retain_general_fallback_region() {
        assert_constraint_uses_fallback("region");
    }

    fn unit_f64(random: &mut SplitMix64) -> f64 {
        (random.next() >> 11) as f64 / (1_u64 << 53) as f64
    }

    fn uniform(random: &mut SplitMix64, low: f64, high: f64) -> f64 {
        low + unit_f64(random) * (high - low)
    }

    fn random_roles(random: &mut SplitMix64, maximum: usize) -> Vec<String> {
        let count = 1 + random.index(maximum);
        let mut roles = ["1", "2", "3", "4", "5"];
        for index in 0..count {
            let selected = index + random.index(roles.len() - index);
            roles.swap(index, selected);
        }
        roles[..count]
            .iter()
            .map(|role| (*role).to_owned())
            .collect()
    }

    #[test]
    fn test_seeded_randomized_common_path_exactly_matches_zero_penalty_fallback() {
        let mut random = SplitMix64(0xCADA);
        for case in 0..60 {
            let players: Vec<Player> = (0..10)
                .map(|index| Player {
                    discord_id: Some(case * 100 + index + 1),
                    glicko_rating: Some(uniform(&mut random, 900.0, 2_600.0)),
                    glicko_rd: Some(uniform(&mut random, 30.0, 350.0)),
                    preferred_roles: Some(random_roles(&mut random, 5)),
                    ..Player::new(format!("Case{case}Player{index}"))
                })
                .collect();
            let shuffler = BalancedShuffler {
                off_role_multiplier: uniform(&mut random, 0.75, 1.0),
                off_role_flat_penalty: uniform(&mut random, 0.0, 250.0),
                role_matchup_delta_weight: uniform(&mut random, 0.0, 1.5),
                rd_priority_weight: uniform(&mut random, 0.0, 0.25),
                ..BalancedShuffler::default()
            };
            let limit = 1 + random.index(20);
            let common = shuffler
                .score_role_assignments_for_matchup(
                    &players[..5],
                    &players[5..],
                    limit,
                    no_rating_constraints(),
                )
                .expect("common scoring");
            let avoid = [SoftAvoid {
                avoider_discord_id: players[0].discord_id.unwrap(),
                avoided_discord_id: players[5].discord_id.unwrap(),
            }];
            let fallback = shuffler
                .score_role_assignments_for_matchup(
                    &players[..5],
                    &players[5..],
                    limit,
                    ShuffleConstraints {
                        avoids: Some(&avoid),
                        ..ShuffleConstraints::default()
                    },
                )
                .expect("fallback scoring");
            assert_eq!(common, fallback);
        }
    }

    fn seeded_pool(seed: u64, prefix: &str, max_roles: usize) -> Vec<Player> {
        let mut random = SplitMix64(seed);
        (0..14)
            .map(|index| Player {
                discord_id: Some(index + 1),
                glicko_rating: Some(uniform(&mut random, 850.0, 2_750.0)),
                glicko_rd: if index % 5 == 0 {
                    None
                } else {
                    Some(uniform(&mut random, 20.0, 350.0))
                },
                preferred_roles: Some(random_roles(&mut random, max_roles)),
                ..Player::new(format!("{prefix}{index}"))
            })
            .collect()
    }

    fn result_signature(result: &PoolResult, identity_only: bool) -> Vec<String> {
        let team_signature = |team: &Team| {
            let roles = team.role_assignments.as_ref().expect("roles assigned");
            let mut values: Vec<String> = team
                .players
                .iter()
                .zip(roles)
                .map(|(player, role)| {
                    if identity_only {
                        format!("{}:{role}", player.discord_id.unwrap_or_default())
                    } else {
                        format!(
                            "{}:{}:{role}",
                            player.name,
                            player.discord_id.unwrap_or_default()
                        )
                    }
                })
                .collect();
            values.sort();
            values.join("|")
        };
        let mut teams = [team_signature(&result.team1), team_signature(&result.team2)];
        teams.sort();
        let mut excluded: Vec<String> = result
            .excluded
            .iter()
            .map(|player| {
                if identity_only {
                    player.discord_id.unwrap_or_default().to_string()
                } else {
                    format!("{}:{}", player.name, player.discord_id.unwrap_or_default())
                }
            })
            .collect();
        excluded.sort();
        vec![teams[0].clone(), teams[1].clone(), excluded.join("|")]
    }

    fn branch_pair(
        players: &[Player],
        shuffler: &BalancedShuffler,
        options: PoolOptions<'_>,
    ) -> (PoolResult, PoolResult) {
        let reference = shuffler
            .shuffle_branch_bound(
                players,
                options,
                BranchSearchOptions {
                    use_split_role_bound: false,
                    initial_upper_bound: Some(f64::INFINITY),
                },
            )
            .expect("reference search");
        let optimized = shuffler
            .shuffle_branch_bound(
                players,
                options,
                BranchSearchOptions {
                    use_split_role_bound: true,
                    initial_upper_bound: Some(f64::INFINITY),
                },
            )
            .expect("optimized search");
        (reference, optimized)
    }

    #[test]
    fn test_split_bound_indexes_each_team_summary_once() {
        let players = seeded_pool(0x14CA, "Mask", 3);
        let shuffler = BalancedShuffler::default();
        shuffler
            .shuffle_branch_bound(
                &players,
                PoolOptions::default(),
                BranchSearchOptions {
                    initial_upper_bound: Some(f64::INFINITY),
                    ..BranchSearchOptions::default()
                },
            )
            .expect("branch search");
        let diagnostics = shuffler.diagnostics();
        assert!(diagnostics.team_summaries_built > 0);
        let unique: HashSet<Vec<usize>> = diagnostics.team_summary_keys.iter().cloned().collect();
        assert_eq!(unique.len(), diagnostics.team_summary_keys.len());
        assert!(unique.len() <= super::binomial(14, 5));
    }

    fn assert_split_bound_matches(seed: u64, recent_count: usize, max_roles: usize) {
        let players = seeded_pool(seed, "Seeded", max_roles);
        let mut random = SplitMix64(seed ^ 0xB0A0D);
        let counts: HashMap<String, usize> = players
            .iter()
            .map(|player| (player.name.clone(), random.index(6)))
            .collect();
        let recent: HashSet<String> = players
            .iter()
            .take(recent_count)
            .map(|player| player.name.clone())
            .collect();
        let shuffler = BalancedShuffler {
            off_role_multiplier: uniform(&mut random, 0.55, 1.0),
            off_role_flat_penalty: uniform(&mut random, 0.0, 220.0),
            role_matchup_delta_weight: uniform(&mut random, 0.0, 1.25),
            exclusion_penalty_weight: uniform(&mut random, 0.0, 100.0),
            rd_priority_weight: uniform(&mut random, 0.0, 0.3),
            recent_match_penalty_weight: uniform(&mut random, 0.0, 180.0),
            rating_spread_divisor: uniform(&mut random, 5.0, 40.0),
            ..BalancedShuffler::default()
        };
        let options = PoolOptions {
            exclusion_counts: Some(&counts),
            recent_match_names: Some(&recent),
            ..PoolOptions::default()
        };
        let (reference, optimized) = branch_pair(&players, &shuffler, options);
        assert_eq!(
            result_signature(&optimized, false),
            result_signature(&reference, false)
        );
    }

    #[test]
    fn test_split_bound_exactly_matches_reference_search_for_seeded_pools_5121_0_5() {
        assert_split_bound_matches(0x1401, 0, 5);
    }

    #[test]
    fn test_split_bound_exactly_matches_reference_search_for_seeded_pools_5123_7_3() {
        assert_split_bound_matches(0x1403, 7, 3);
    }

    #[test]
    fn test_duplicate_display_names_do_not_change_branch_bound_result() {
        let unique = seeded_pool(0xD00D, "Identity", 3);
        let mut duplicate = seeded_pool(0xD00D, "Identity", 3);
        duplicate[13].name = duplicate[0].name.clone();
        let search = BranchSearchOptions {
            initial_upper_bound: Some(f64::INFINITY),
            ..BranchSearchOptions::default()
        };
        let unique_result = BalancedShuffler::default()
            .shuffle_branch_bound(&unique, PoolOptions::default(), search)
            .expect("unique search");
        let duplicate_result = BalancedShuffler::default()
            .shuffle_branch_bound(&duplicate, PoolOptions::default(), search)
            .expect("duplicate search");
        assert_eq!(
            result_signature(&duplicate_result, true),
            result_signature(&unique_result, true)
        );
    }

    #[test]
    fn test_split_bound_materially_prunes_zero_width_rating_intervals() {
        let players: Vec<Player> = (0..14)
            .map(|index| Player {
                discord_id: Some(index + 1),
                glicko_rating: Some(950.0 + index as f64 * 113.0 + (index * index) as f64),
                glicko_rd: Some(0.0),
                preferred_roles: role_list(&["1", "2", "3", "4", "5"]),
                ..Player::new(format!("Fixed{index}"))
            })
            .collect();
        let settings = BalancedShuffler {
            off_role_multiplier: 1.0,
            off_role_flat_penalty: 0.0,
            role_matchup_delta_weight: 0.0,
            exclusion_penalty_weight: 0.0,
            rd_priority_weight: 0.0,
            recent_match_penalty_weight: 0.0,
            rating_spread_divisor: 1_000_000.0,
            ..BalancedShuffler::default()
        };
        let reference_shuffler = settings.clone();
        let optimized_shuffler = settings;
        let reference = reference_shuffler
            .shuffle_branch_bound(
                &players,
                PoolOptions::default(),
                BranchSearchOptions {
                    use_split_role_bound: false,
                    initial_upper_bound: Some(f64::INFINITY),
                },
            )
            .expect("reference search");
        let optimized = optimized_shuffler
            .shuffle_branch_bound(
                &players,
                PoolOptions::default(),
                BranchSearchOptions {
                    use_split_role_bound: true,
                    initial_upper_bound: Some(f64::INFINITY),
                },
            )
            .expect("optimized search");
        assert_eq!(
            result_signature(&optimized, false),
            result_signature(&reference, false)
        );
        let reference_calls = reference_shuffler.diagnostics().unconstrained_score_calls;
        let optimized_calls = optimized_shuffler.diagnostics().unconstrained_score_calls;
        assert!(
            optimized_calls * 2 < reference_calls,
            "split bound only reduced scorer calls from {reference_calls} to {optimized_calls}"
        );
    }

    fn assert_negative_term_matches(kind: &str) {
        let players = seeded_pool(0x14CE, "Negative", 3);
        let shuffler = BalancedShuffler {
            off_role_multiplier: 0.75,
            off_role_flat_penalty: 110.0,
            role_matchup_delta_weight: if kind == "role" { -0.6 } else { 0.7 },
            recent_match_penalty_weight: if kind == "recent" { -450.0 } else { 125.0 },
            soft_avoid_penalty: if kind == "avoid" { -900.0 } else { 500.0 },
            package_deal_penalty: if kind == "deal" { -900.0 } else { 100.0 },
            rd_priority_weight: 0.09,
            rating_spread_divisor: 20.0,
            ..BalancedShuffler::default()
        };
        let recent: HashSet<String> = if kind == "recent" {
            players[..8]
                .iter()
                .map(|player| player.name.clone())
                .collect()
        } else {
            HashSet::new()
        };
        let avoids = [SoftAvoid {
            avoider_discord_id: players[0].discord_id.unwrap(),
            avoided_discord_id: players[1].discord_id.unwrap(),
        }];
        let deals = [PackageDeal {
            buyer_discord_id: players[2].discord_id.unwrap(),
            partner_discord_id: players[11].discord_id.unwrap(),
        }];
        let options = PoolOptions {
            recent_match_names: Some(&recent),
            constraints: ShuffleConstraints {
                avoids: (kind == "avoid").then_some(&avoids[..]),
                deals: (kind == "deal").then_some(&deals[..]),
                low_priority_ids: None,
            },
            ..PoolOptions::default()
        };
        let (reference, optimized) = branch_pair(&players, &shuffler, options);
        assert_eq!(
            result_signature(&optimized, false),
            result_signature(&reference, false)
        );
    }

    #[test]
    fn test_negative_penalty_terms_match_reference_search_recent() {
        assert_negative_term_matches("recent");
    }

    #[test]
    fn test_negative_penalty_terms_match_reference_search_role() {
        assert_negative_term_matches("role");
    }

    #[test]
    fn test_negative_penalty_terms_match_reference_search_avoid() {
        assert_negative_term_matches("avoid");
    }

    #[test]
    fn test_negative_penalty_terms_match_reference_search_deal() {
        assert_negative_term_matches("deal");
    }

    #[test]
    fn test_split_bound_preserves_empty_assignment_fallback() {
        let players = create_players_with_roles(14, 1_500, 50);
        let provider = Arc::new(RecordingProvider::default());
        let shuffler = BalancedShuffler::default().with_role_assignment_provider(provider);
        let (reference, optimized) = branch_pair(&players, &shuffler, PoolOptions::default());
        assert_eq!(
            result_signature(&optimized, false),
            result_signature(&reference, false)
        );
        for team in [&optimized.team1, &optimized.team2] {
            assert_eq!(
                team.role_assignments
                    .as_ref()
                    .expect("fallback roles")
                    .iter()
                    .cloned()
                    .collect::<HashSet<_>>(),
                ["1", "2", "3", "4", "5"]
                    .map(str::to_owned)
                    .into_iter()
                    .collect()
            );
        }
    }

    #[test]
    fn test_branch_bound_does_not_prune_negative_rd_score() {
        let players: Vec<Player> = (0..14)
            .map(|index| Player {
                mmr: Some(1_500),
                glicko_rd: Some(25.0),
                preferred_roles: role_list(&["1", "2", "3", "4", "5"]),
                ..Player::new(format!("Player{index}"))
            })
            .collect();
        let shuffler = configured_mmr_shuffler();
        shuffler
            .shuffle_branch_bound(
                &players,
                PoolOptions::default(),
                BranchSearchOptions {
                    use_split_role_bound: false,
                    initial_upper_bound: Some(-100.0),
                },
            )
            .expect("branch search");
        assert!(shuffler.diagnostics().branch_matchups_evaluated > 0);
    }

    #[test]
    fn test_branch_bound_is_deterministic_with_fixed_optimizer_scores() {
        let players = create_players_with_roles(14, 1_500, 50);
        let shuffler = BalancedShuffler {
            off_role_flat_penalty: 100.0,
            ..BalancedShuffler::default()
        };
        let first = shuffler
            .shuffle_branch_bound(
                &players,
                PoolOptions::default(),
                BranchSearchOptions::default(),
            )
            .expect("first search");
        let second = shuffler
            .shuffle_branch_bound(
                &players,
                PoolOptions::default(),
                BranchSearchOptions::default(),
            )
            .expect("second search");
        assert_eq!(
            result_signature(&first, false),
            result_signature(&second, false)
        );
    }

    #[test]
    fn test_role_optimizer_is_deterministic_for_same_matchup() {
        let players = create_players_with_roles(10, 1_500, 50);
        let shuffler = BalancedShuffler {
            off_role_flat_penalty: 100.0,
            ..BalancedShuffler::default()
        };
        let first = shuffler
            .optimize_role_assignments_for_matchup(
                &players[..5],
                &players[5..],
                3,
                no_rating_constraints(),
            )
            .expect("first optimization");
        let second = shuffler
            .optimize_role_assignments_for_matchup(
                &players[..5],
                &players[5..],
                3,
                no_rating_constraints(),
            )
            .expect("second optimization");
        assert_eq!(first.0.role_assignments, second.0.role_assignments);
        assert_eq!(first.1.role_assignments, second.1.role_assignments);
        assert_eq!(first.2, second.2);
    }

    #[test]
    fn test_score_only_role_optimizer_matches_materialized_result() {
        let players = create_players_with_roles(10, 1_500, 50);
        let shuffler = BalancedShuffler {
            off_role_flat_penalty: 100.0,
            ..BalancedShuffler::default()
        };
        let scored = shuffler
            .score_role_assignments_for_matchup(
                &players[..5],
                &players[5..],
                3,
                no_rating_constraints(),
            )
            .expect("score-only optimization");
        let materialized = shuffler
            .optimize_role_assignments_for_matchup(
                &players[..5],
                &players[5..],
                3,
                no_rating_constraints(),
            )
            .expect("materialized optimization");
        assert_eq!(
            scored.0.as_ref().map(|roles| roles.to_vec()),
            materialized.0.role_assignments
        );
        assert_eq!(
            scored.1.as_ref().map(|roles| roles.to_vec()),
            materialized.1.role_assignments
        );
        assert_eq!(scored.2, materialized.2);
    }

    #[test]
    fn test_role_optimizer_reads_each_player_value_once() {
        let players: Vec<Player> = (0..10)
            .map(|index| Player {
                glicko_rating: Some(1_200.0 + index as f64 * 75.0),
                glicko_rd: Some(50.0 + index as f64),
                preferred_roles: role_list(&["1", "2", "3", "4", "5"]),
                ..Player::new(format!("Player{index}"))
            })
            .collect();
        let shuffler = BalancedShuffler::default();
        shuffler
            .optimize_role_assignments_for_matchup(
                &players[..5],
                &players[5..],
                20,
                no_rating_constraints(),
            )
            .expect("optimization");
        assert_eq!(shuffler.diagnostics().player_value_reads, players.len());
    }

    #[test]
    fn test_pool_shuffle_reuses_five_player_role_metrics() {
        let players: Vec<Player> = (0..11)
            .map(|index| Player {
                glicko_rating: Some(1_200.0 + index as f64 * 50.0),
                preferred_roles: role_list(&["1", "2", "3", "4", "5"]),
                ..Player::new(format!("Player{index}"))
            })
            .collect();
        let shuffler = BalancedShuffler::default();
        shuffler
            .shuffle_from_pool(&players, PoolOptions::default())
            .expect("pool shuffle");
        assert_eq!(
            shuffler.diagnostics().role_metrics_built,
            super::binomial(11, 5) * 3
        );
    }

    #[test]
    fn test_pool_player_value_cache_is_request_scoped() {
        let mut players = create_players_with_roles(11, 1_500, 50);
        let shuffler = BalancedShuffler::default();
        shuffler
            .shuffle_from_pool(&players, PoolOptions::default())
            .expect("first pool shuffle");
        assert_eq!(shuffler.diagnostics().player_value_reads, 11);
        players[0].glicko_rating = players[0].glicko_rating.map(|rating| rating + 100.0);
        shuffler
            .shuffle_from_pool(&players, PoolOptions::default())
            .expect("second pool shuffle");
        assert_eq!(shuffler.diagnostics().player_value_reads, 22);
    }

    #[test]
    fn test_14_player_pool_with_role_preferences() {
        let roles: [&[&str]; 14] = [
            &["1"],
            &["1"],
            &["2"],
            &["2"],
            &["3"],
            &["3"],
            &["4"],
            &["4"],
            &["5"],
            &["5"],
            &["1", "2", "3"],
            &["4", "5"],
            &["1", "2", "3", "4", "5"],
            &["1", "2", "3", "4", "5"],
        ];
        let players: Vec<Player> = (0..14)
            .map(|index| Player {
                mmr: Some(1_500 + index * 30),
                glicko_rating: Some(1_500.0 + index as f64 * 15.0),
                preferred_roles: role_list(roles[index as usize]),
                ..Player::new(format!("Player{index}"))
            })
            .collect();
        let counts: HashMap<String, usize> = players
            .iter()
            .map(|player| (player.name.clone(), 0))
            .collect();
        let result = BalancedShuffler {
            off_role_flat_penalty: 100.0,
            exclusion_penalty_weight: 75.0,
            ..BalancedShuffler::default()
        }
        .shuffle_from_pool(
            &players,
            PoolOptions {
                exclusion_counts: Some(&counts),
                ..PoolOptions::default()
            },
        )
        .expect("14-player pool shuffle");
        assert_eq!(result.team1.players.len(), 5);
        assert_eq!(result.team2.players.len(), 5);
        assert_eq!(result.excluded.len(), 4);
        assert_eq!(
            result.team1.role_assignments.as_ref().map(Vec::len),
            Some(5)
        );
        assert_eq!(
            result.team2.role_assignments.as_ref().map(Vec::len),
            Some(5)
        );
        assert_eq!(
            result
                .team1
                .players
                .iter()
                .chain(&result.team2.players)
                .chain(&result.excluded)
                .map(|player| player.name.as_str())
                .collect::<HashSet<_>>()
                .len(),
            14
        );
    }

    #[test]
    fn test_default_weight_is_80() {
        assert_eq!(BalancedShuffler::default().exclusion_penalty_weight, 80.0);
    }

    #[test]
    fn test_higher_weight_prevents_repeat_exclusions() {
        let players = create_players_with_roles(14, 1_500, 0);
        let counts: HashMap<String, usize> = players
            .iter()
            .enumerate()
            .map(|(index, player)| (player.name.clone(), usize::from(index < 4) * 2))
            .collect();
        let result = BalancedShuffler {
            off_role_flat_penalty: 100.0,
            exclusion_penalty_weight: 75.0,
            ..BalancedShuffler::default()
        }
        .shuffle_from_pool(
            &players,
            PoolOptions {
                exclusion_counts: Some(&counts),
                ..PoolOptions::default()
            },
        )
        .expect("pool shuffle");
        let protected: HashSet<&str> = players[..4]
            .iter()
            .map(|player| player.name.as_str())
            .collect();
        let excluded_protected = result
            .excluded
            .iter()
            .filter(|player| protected.contains(player.name.as_str()))
            .count();
        assert!(excluded_protected <= 1);
    }

    #[test]
    fn test_shuffler_jopacoin_balancing() {
        let players: Vec<Player> = (0..10)
            .map(|index| Player {
                mmr: Some(1_500),
                preferred_roles: role_list(&[["1", "2", "3", "4", "5"][index % 5]]),
                jopacoin_balance: (index as i64 + 1) * 100,
                ..Player::new(format!("P{index}"))
            })
            .collect();
        let (team1, team2) = BalancedShuffler {
            use_jopacoin: true,
            ..BalancedShuffler::default()
        }
        .shuffle(&players)
        .expect("jopacoin shuffle");
        let value1 = team1
            .get_team_value(true, 0.9, false, true)
            .expect("roles assigned");
        let value2 = team2
            .get_team_value(true, 0.9, false, true)
            .expect("roles assigned");
        assert!((value1 - value2).abs() <= 500.0);
    }

    fn draft_player(name: impl Into<String>, rating: f64) -> Player {
        Player {
            glicko_rating: Some(rating),
            glicko_rd: Some(100.0),
            preferred_roles: role_list(&["1", "2", "3", "4", "5"]),
            ..Player::new(name)
        }
    }

    fn draft_candidates(count: usize, start: f64, step: f64) -> Vec<Player> {
        (0..count)
            .map(|index| draft_player(format!("P{index}"), start + index as f64 * step))
            .collect()
    }

    fn draft_captains(first: f64, second: f64) -> (Player, Player) {
        (draft_player("CaptA", first), draft_player("CaptB", second))
    }

    #[test]
    fn test_draft_rating_fallback_preserves_zero_and_defaults_only_none() {
        let zero = Player {
            glicko_rating: Some(0.0),
            ..Player::new("Zero")
        };
        let missing = Player::new("Missing");
        assert_eq!(zero.get_value(true, false, false), 0.0);
        assert_eq!(missing.get_value(true, false, false), 0.0);
        assert_eq!(missing.glicko_rating.unwrap_or(1500.0), 1500.0);
    }

    #[test]
    fn test_draft_score_returns_single_score() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_600.0);
        let pool = draft_candidates(8, 1_500.0, 20.0);
        let score = BalancedShuffler::default()
            .score_draft_pool(&captain_a, &captain_b, &pool)
            .expect("draft pool score");
        assert!(score.is_finite());
    }

    #[test]
    fn test_draft_score_keeps_unweighted_value_difference() {
        let (captain_a, captain_b) = draft_captains(1_100.0, 1_000.0);
        let pool = draft_candidates(8, 1_000.0, 0.0);
        let shuffler = BalancedShuffler {
            off_role_multiplier: 1.0,
            off_role_flat_value_penalty: 0.0,
            off_role_flat_penalty: 0.0,
            role_matchup_delta_weight: 0.0,
            rd_priority_weight: 0.0,
            ..BalancedShuffler::default()
        };

        let score = shuffler
            .score_draft_pool(&captain_a, &captain_b, &pool)
            .expect("draft pool score");

        approx(score, 95.0);
    }

    #[test]
    fn test_draft_score_symmetric_captains_and_pool() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_600.0);
        let pool = draft_candidates(8, 1_500.0, 0.0);
        let score = BalancedShuffler::default()
            .score_draft_pool(&captain_a, &captain_b, &pool)
            .expect("symmetric draft pool score");
        assert!(score.abs() < 250.0);
    }

    #[test]
    fn test_draft_score_evaluates_all_splits() {
        let (captain_a, captain_b) = draft_captains(1_800.0, 1_200.0);
        let pool = draft_candidates(8, 1_300.0, 80.0);
        let score = BalancedShuffler::default()
            .score_draft_pool(&captain_a, &captain_b, &pool)
            .expect("varied draft pool score");
        assert!(score < 1_000.0);
    }

    #[test]
    fn test_draft_score_does_not_materialize_discarded_teams() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_550.0);
        let pool = draft_candidates(8, 1_400.0, 30.0);
        let shuffler = BalancedShuffler::default();
        let score = shuffler
            .score_draft_pool(&captain_a, &captain_b, &pool)
            .expect("score-only draft pool");
        assert!(score.is_finite());
        assert_eq!(shuffler.diagnostics().pool_matchups_evaluated, 0);
    }

    #[test]
    fn test_draft_select_exactly_8_candidates_returns_all() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_550.0);
        let candidates = draft_candidates(8, 1_400.0, 30.0);
        let result = BalancedShuffler::default()
            .select_draft_pool(&captain_a, &captain_b, &candidates, None, None)
            .expect("eight-candidate draft pool");
        assert_eq!(result.selected_players.len(), 8);
        assert!(result.excluded_players.is_empty());
        assert_eq!(names(&result.selected_players), names(&candidates));
    }

    #[test]
    fn test_draft_select_fewer_than_8_raises_pool_error() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_550.0);
        let candidates = draft_candidates(7, 1_400.0, 30.0);
        assert!(matches!(
            BalancedShuffler::default().select_draft_pool(
                &captain_a,
                &captain_b,
                &candidates,
                None,
                None
            ),
            Err(super::ShufflerError::TooFewPoolPlayers {
                minimum: 8,
                actual: 7
            })
        ));
    }

    #[test]
    fn test_draft_select_10_candidates_selects_8_excludes_2() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_550.0);
        let candidates = draft_candidates(10, 1_300.0, 40.0);
        let result = BalancedShuffler::default()
            .select_draft_pool(&captain_a, &captain_b, &candidates, None, None)
            .expect("ten-candidate draft pool");
        assert_eq!(result.selected_players.len(), 8);
        assert_eq!(result.excluded_players.len(), 2);
        let mut all_names = names(&result.selected_players);
        all_names.extend(names(&result.excluded_players));
        assert_eq!(all_names.len(), 10);
    }

    #[test]
    fn test_draft_select_12_candidates_selects_8_excludes_4() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_550.0);
        let candidates = draft_candidates(12, 1_300.0, 30.0);
        let result = BalancedShuffler::default()
            .select_draft_pool(&captain_a, &captain_b, &candidates, None, None)
            .expect("twelve-candidate draft pool");
        assert_eq!(result.selected_players.len(), 8);
        assert_eq!(result.excluded_players.len(), 4);
    }

    #[test]
    fn test_draft_select_pool_score_equals_best_split_for_8() {
        let (captain_a, captain_b) = draft_captains(1_700.0, 1_400.0);
        let candidates = draft_candidates(8, 1_300.0, 50.0);
        let shuffler = BalancedShuffler::default();
        let result = shuffler
            .select_draft_pool(&captain_a, &captain_b, &candidates, None, None)
            .expect("eight-candidate score");
        let expected = shuffler
            .score_draft_pool(&captain_a, &captain_b, &candidates)
            .expect("direct score");
        approx(result.pool_score, expected);
    }

    #[test]
    fn test_draft_select_exclusion_penalty_keeps_frequently_excluded() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_600.0);
        let candidates = draft_candidates(10, 1_500.0, 10.0);
        let counts = HashMap::from([
            (candidates[8].name.clone(), 100_usize),
            (candidates[9].name.clone(), 100_usize),
        ]);
        let result = BalancedShuffler::default()
            .select_draft_pool(&captain_a, &captain_b, &candidates, Some(&counts), None)
            .expect("exclusion-aware draft pool");
        let selected = names(&result.selected_players);
        assert!(selected.contains(&candidates[8].name));
        assert!(selected.contains(&candidates[9].name));
    }

    #[test]
    fn test_draft_select_recent_match_penalty_affects_selection() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_600.0);
        let candidates = draft_candidates(10, 1_500.0, 0.0);
        let recent = candidates
            .iter()
            .map(|player| player.name.clone())
            .collect();
        let shuffler = BalancedShuffler::default();
        let with_recent = shuffler
            .select_draft_pool(&captain_a, &captain_b, &candidates, None, Some(&recent))
            .expect("recent-aware draft pool");
        let without_recent = shuffler
            .select_draft_pool(&captain_a, &captain_b, &candidates, None, None)
            .expect("baseline draft pool");
        assert!(with_recent.pool_score >= without_recent.pool_score);
    }

    #[test]
    fn test_draft_select_result_is_draft_pool_result() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_550.0);
        let candidates = draft_candidates(8, 1_400.0, 30.0);
        let result: super::DraftPoolResult = BalancedShuffler::default()
            .select_draft_pool(&captain_a, &captain_b, &candidates, None, None)
            .expect("typed draft pool result");
        assert!(result.pool_score.is_finite());
    }

    #[test]
    fn test_draft_beam_search_quality_12_candidates() {
        let (captain_a, captain_b) = draft_captains(1_700.0, 1_400.0);
        let candidates = draft_candidates(12, 1_300.0, 50.0);
        let shuffler = BalancedShuffler::default();
        let exhaustive = shuffler
            .select_draft_pool(&captain_a, &captain_b, &candidates, None, None)
            .expect("exhaustive draft pool");
        let beam = shuffler
            .select_draft_pool_beam(&captain_a, &captain_b, &candidates, None, None)
            .expect("beam draft pool");
        assert!(beam.pool_score - exhaustive.pool_score <= 50.0);
    }

    #[test]
    fn test_draft_beam_search_varied_ratings() {
        let (captain_a, captain_b) = draft_captains(1_800.0, 1_200.0);
        let candidates = draft_candidates(15, 1_000.0, 100.0);
        let result = BalancedShuffler::default()
            .select_draft_pool_beam(&captain_a, &captain_b, &candidates, None, None)
            .expect("varied beam draft pool");
        assert_eq!(result.selected_players.len(), 8);
        assert_eq!(result.excluded_players.len(), 7);
        assert!(result.pool_score < 2_000.0);
    }

    #[test]
    fn test_draft_beam_search_respects_exclusion_counts() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_600.0);
        let candidates = draft_candidates(15, 1_500.0, 10.0);
        let counts = HashMap::from([
            (candidates[12].name.clone(), 100_usize),
            (candidates[13].name.clone(), 100_usize),
            (candidates[14].name.clone(), 100_usize),
        ]);
        let result = BalancedShuffler::default()
            .select_draft_pool_beam(&captain_a, &captain_b, &candidates, Some(&counts), None)
            .expect("exclusion-aware beam draft pool");
        let selected = names(&result.selected_players);
        assert!(
            [12, 13, 14]
                .iter()
                .filter(|index| selected.contains(&candidates[**index].name))
                .count()
                >= 2
        );
    }

    #[test]
    fn test_draft_beam_15_candidates_bounded_work() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_550.0);
        let candidates = draft_candidates(15, 1_400.0, 30.0);
        let shuffler = BalancedShuffler::default();
        let result = shuffler
            .select_draft_pool_beam(&captain_a, &captain_b, &candidates, None, None)
            .expect("15-player beam");
        assert_eq!(result.selected_players.len(), 8);
        assert!(shuffler.diagnostics().draft_pool_scores_evaluated <= 1_000);
    }

    #[test]
    fn test_draft_beam_18_candidates_bounded_work() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_550.0);
        let candidates = draft_candidates(18, 1_400.0, 25.0);
        let shuffler = BalancedShuffler::default();
        let result = shuffler
            .select_draft_pool_beam(&captain_a, &captain_b, &candidates, None, None)
            .expect("18-player beam");
        assert_eq!(result.selected_players.len(), 8);
        assert!(shuffler.diagnostics().draft_pool_scores_evaluated <= 1_300);
    }

    #[test]
    fn test_draft_beam_20_candidates_bounded_work() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_550.0);
        let candidates = draft_candidates(20, 1_400.0, 20.0);
        let shuffler = BalancedShuffler::default();
        let result = shuffler
            .select_draft_pool_beam(&captain_a, &captain_b, &candidates, None, None)
            .expect("20-player beam");
        assert_eq!(result.selected_players.len(), 8);
        assert!(shuffler.diagnostics().draft_pool_scores_evaluated <= 1_600);
    }

    #[test]
    fn test_draft_beam_24_candidates_bounded_work() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_550.0);
        let candidates = draft_candidates(24, 1_400.0, 15.0);
        let shuffler = BalancedShuffler::default();
        let result = shuffler
            .select_draft_pool_beam(&captain_a, &captain_b, &candidates, None, None)
            .expect("24-player beam");
        assert_eq!(result.selected_players.len(), 8);
        assert_eq!(result.excluded_players.len(), 16);
        assert!(shuffler.diagnostics().draft_pool_scores_evaluated <= 2_600);
    }

    #[test]
    fn test_draft_select_8_candidates_uses_direct_scoring() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_550.0);
        let candidates = draft_candidates(8, 1_400.0, 30.0);
        let shuffler = BalancedShuffler::default();
        let result = shuffler
            .select_draft_pool(&captain_a, &captain_b, &candidates, None, None)
            .expect("direct route");
        assert_eq!(result.selected_players.len(), 8);
        assert_eq!(shuffler.diagnostics().draft_pool_scores_evaluated, 0);
    }

    #[test]
    fn test_draft_select_12_candidates_uses_exhaustive() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_550.0);
        let candidates = draft_candidates(12, 1_400.0, 30.0);
        let result = BalancedShuffler::default()
            .select_draft_pool(&captain_a, &captain_b, &candidates, None, None)
            .expect("exhaustive route");
        assert_eq!(result.selected_players.len(), 8);
        assert_eq!(result.excluded_players.len(), 4);
    }

    #[test]
    fn test_draft_exhaustive_reuses_team_role_metrics() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_550.0);
        let candidates = draft_candidates(12, 1_400.0, 30.0);
        let shuffler = BalancedShuffler::default();
        let result = shuffler
            .select_draft_pool(&captain_a, &captain_b, &candidates, None, None)
            .expect("metric-cache route");
        assert_eq!(result.selected_players.len(), 8);
        assert_eq!(shuffler.diagnostics().role_metrics_built, 2 * 495 * 3);
    }

    #[test]
    fn test_draft_select_13_candidates_uses_beam() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_550.0);
        let candidates = draft_candidates(13, 1_400.0, 30.0);
        let shuffler = BalancedShuffler::default();
        let result = shuffler
            .select_draft_pool(&captain_a, &captain_b, &candidates, None, None)
            .expect("beam route");
        assert_eq!(result.selected_players.len(), 8);
        assert!(shuffler.diagnostics().draft_pool_scores_evaluated > 0);
    }

    #[test]
    fn test_draft_select_9_candidates_uses_exhaustive_edge() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_550.0);
        let candidates = draft_candidates(9, 1_400.0, 30.0);
        let result = BalancedShuffler::default()
            .select_draft_pool(&captain_a, &captain_b, &candidates, None, None)
            .expect("nine-player exhaustive edge");
        assert_eq!(result.selected_players.len(), 8);
        assert_eq!(result.excluded_players.len(), 1);
    }

    #[test]
    fn test_draft_beam_identical_ratings() {
        let (captain_a, captain_b) = draft_captains(1_500.0, 1_500.0);
        let candidates = draft_candidates(15, 1_500.0, 0.0);
        let result = BalancedShuffler::default()
            .select_draft_pool_beam(&captain_a, &captain_b, &candidates, None, None)
            .expect("identical-rating beam");
        assert_eq!(result.selected_players.len(), 8);
        assert_eq!(result.excluded_players.len(), 7);
        assert!(result.pool_score < 200.0);
    }

    #[test]
    fn test_draft_beam_with_ties() {
        let (captain_a, captain_b) = draft_captains(1_600.0, 1_600.0);
        let candidates = draft_candidates(15, 1_500.0, 1.0);
        let result = BalancedShuffler::default()
            .select_draft_pool_beam(&captain_a, &captain_b, &candidates, None, None)
            .expect("tied-rating beam");
        assert_eq!(result.selected_players.len(), 8);
        assert_eq!(result.excluded_players.len(), 7);
        assert!(result.pool_score.is_finite());
    }

    #[test]
    fn test_draft_beam_early_exit_with_balanced_pool() {
        let (captain_a, captain_b) = draft_captains(1_500.0, 1_500.0);
        let candidates = draft_candidates(15, 1_500.0, 0.0);
        let shuffler = BalancedShuffler::default();
        let result = shuffler
            .select_draft_pool_beam(&captain_a, &captain_b, &candidates, None, None)
            .expect("early-exit beam");
        assert!(result.pool_score < 150.0);
        assert_eq!(shuffler.diagnostics().draft_pool_scores_evaluated, 1);
    }

    #[test]
    fn test_tied_matchups_are_collected_and_selected_by_tie_breaker() {
        // Ten identical players: every one of the 126 splits scores the same,
        // so Python's `random.choice(best_matchups)` has 126 candidates. The
        // injected selectors must therefore be able to produce different
        // teams, proving ties are collected rather than first-split-wins.
        let players: Vec<Player> = (0..10)
            .map(|index| Player {
                mmr: Some(1_500),
                glicko_rating: Some(1_500.0),
                preferred_roles: role_list(&["1", "2", "3", "4", "5"]),
                ..Player::new(format!("Twin{index}"))
            })
            .collect();
        let first = BalancedShuffler::default()
            .with_tie_breaker(Arc::new(|_| 0))
            .shuffle(&players)
            .expect("first-tie shuffle");
        let last = BalancedShuffler::default()
            .with_tie_breaker(Arc::new(|count| count - 1))
            .shuffle(&players)
            .expect("last-tie shuffle");
        let names = |team: &Team| -> Vec<String> {
            team.players
                .iter()
                .map(|player| player.name.clone())
                .collect()
        };
        assert_ne!(
            names(&first.0),
            names(&last.0),
            "tie breaker index must select among distinct tied matchups"
        );
    }
}
