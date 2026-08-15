//! Production orchestration for `/admin correctmatch` and restart recovery.
//!
//! The database repository owns each durable transition.  This module owns
//! their Python-compatible ordering: claim, winner rewards, old-winner
//! reversal, atomic match/rating flip, atomic bet correction, and finally the
//! full OpenSkill replay.  A failed replay deliberately leaves both the
//! `core_applied` claim and replay job behind so the same command (or READY
//! recovery) resumes without moving money twice.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::service_container::PersistentVanityTaxService;
use cama_db::match_correction_repository::{
    BetCorrectionOptions, ClaimState, CoreCorrectionRequest, MatchCorrectionError,
    MatchCorrectionRepository, MatchSide, OPENSKILL_REPLAY_ALGORITHM_VERSION,
};
use cama_domain::openskill::{CamaOpenSkillSystem, OpenSkillError};
use cama_domain::rating::CamaRatingSystem;
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{error, info};

use crate::admin_provider::{
    AdminCorrectMatchBetResult, AdminCorrectMatchError, AdminCorrectMatchRequest,
    AdminCorrectMatchResult, AdminCorrectMatchSide, AdminMatchCorrectionControl,
    CorrectionWinRewardControl, CorrectionWinRewardRequest,
};
use crate::application_config::ApplicationConfig;
use crate::gateway_events::{
    GatewayEventObserver, ReadyRecoveryContext, ReadyRecoveryFailure, ReadyRecoveryReport,
};

const CLAIM_STALE_AFTER_SECONDS: i64 = 300;
const RECOVERY_OBSERVER_NAME: &str = "admin-match-correction-recovery";
const CORRECTION_REASON_PREFIX: &str = "match_correction:";

static OWNER_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Error)]
pub enum AdminMatchCorrectionBuildError {
    #[error("invalid OpenSkill runtime configuration: {0}")]
    OpenSkill(#[from] OpenSkillError),
    #[error("invalid Glicko runtime configuration: {0}")]
    InvalidRatingConfig(&'static str),
}

/// One production composition exposes both the slash-command control and the
/// READY observer.  They share the same immutable policy and repository path.
#[derive(Clone)]
pub struct AdminMatchCorrectionRuntime {
    control: Arc<ProductionAdminMatchCorrectionControl>,
    observer: Arc<AdminMatchCorrectionRecoveryObserver>,
}

impl AdminMatchCorrectionRuntime {
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        rewards: Arc<dyn CorrectionWinRewardControl>,
        vanity_tax: Arc<PersistentVanityTaxService>,
    ) -> Result<Self, AdminMatchCorrectionBuildError> {
        let openskill = CamaOpenSkillSystem::with_config(config.migration.openskill.clone())?;
        openskill.calibrate_win_probability(0.5, None)?;
        let rating = config
            .glicko_rating_system()
            .map_err(AdminMatchCorrectionBuildError::InvalidRatingConfig)?;
        let control = Arc::new(ProductionAdminMatchCorrectionControl {
            repository: MatchCorrectionRepository::new(database_path.as_ref()),
            database_path: database_path.as_ref().to_path_buf(),
            openskill,
            rating,
            new_player_mmr_discount: config.migration.new_player_mmr_discount,
            house_payout_multiplier: config.values.house_payout_multiplier,
            vanity_tax_rate: config.values.vanity_tax_rate,
            rewards,
            vanity_tax: Arc::new(PersistentVanityTaxSource(vanity_tax)),
            replay_gate: Arc::new(AllowReplay),
        });
        let observer = Arc::new(AdminMatchCorrectionRecoveryObserver {
            control: Arc::clone(&control),
            startup_guild_leases_released: Mutex::new(BTreeSet::new()),
        });
        Ok(Self { control, observer })
    }

    #[must_use]
    pub fn control(&self) -> Arc<dyn AdminMatchCorrectionControl> {
        self.control.clone()
    }

    #[must_use]
    pub fn gateway_observer(&self) -> Arc<dyn GatewayEventObserver> {
        self.observer.clone()
    }
}

trait CorrectionVanityTaxSource: Send + Sync {
    fn taxable_ids(&self, guild_id: i64) -> BTreeSet<i64>;
}

struct PersistentVanityTaxSource(Arc<PersistentVanityTaxService>);

impl CorrectionVanityTaxSource for PersistentVanityTaxSource {
    fn taxable_ids(&self, guild_id: i64) -> BTreeSet<i64> {
        self.0.taxable_ids(guild_id)
    }
}

trait ReplayGate: Send + Sync {
    fn before_replay(&self, _guild_id: i64, _match_id: i64) -> Result<(), String> {
        Ok(())
    }
}

struct AllowReplay;

impl ReplayGate for AllowReplay {}

struct ProductionAdminMatchCorrectionControl {
    repository: MatchCorrectionRepository,
    database_path: PathBuf,
    openskill: CamaOpenSkillSystem,
    rating: CamaRatingSystem,
    new_player_mmr_discount: i32,
    house_payout_multiplier: f64,
    vanity_tax_rate: f64,
    rewards: Arc<dyn CorrectionWinRewardControl>,
    vanity_tax: Arc<dyn CorrectionVanityTaxSource>,
    replay_gate: Arc<dyn ReplayGate>,
}

impl Clone for ProductionAdminMatchCorrectionControl {
    fn clone(&self) -> Self {
        Self {
            repository: MatchCorrectionRepository::new(&self.database_path),
            database_path: self.database_path.clone(),
            openskill: self.openskill.clone(),
            rating: self.rating,
            new_player_mmr_discount: self.new_player_mmr_discount,
            house_payout_multiplier: self.house_payout_multiplier,
            vanity_tax_rate: self.vanity_tax_rate,
            rewards: Arc::clone(&self.rewards),
            vanity_tax: Arc::clone(&self.vanity_tax),
            replay_gate: Arc::clone(&self.replay_gate),
        }
    }
}

#[async_trait]
impl AdminMatchCorrectionControl for ProductionAdminMatchCorrectionControl {
    async fn correct_match(
        &self,
        request: AdminCorrectMatchRequest,
    ) -> Result<AdminCorrectMatchResult, AdminCorrectMatchError> {
        let control = self.clone();
        tokio::task::spawn_blocking(move || control.correct_match_blocking(request))
            .await
            .map_err(|error| {
                AdminCorrectMatchError::Unexpected(format!(
                    "match-correction worker failed: {error}"
                ))
            })?
    }
}

impl ProductionAdminMatchCorrectionControl {
    fn correct_match_blocking(
        &self,
        request: AdminCorrectMatchRequest,
    ) -> Result<AdminCorrectMatchResult, AdminCorrectMatchError> {
        let requested_side = database_side(request.correct_result);
        let owner_token = owner_token();
        let claim = self
            .repository
            .claim_match_correction(
                request.match_id,
                Some(request.guild_id),
                requested_side,
                &owner_token,
                unix_seconds(),
                CLAIM_STALE_AFTER_SECONDS,
            )
            .map_err(classify_repository_error)?;

        let result = self.correct_owned(request, &owner_token, &claim);
        let result = match result {
            Ok(result) if claim.state != ClaimState::Already => {
                match self
                    .repository
                    .complete_match_correction_claim(request.match_id, &owner_token)
                {
                    Ok(true) => Ok(result),
                    Ok(false) => Err(AdminCorrectMatchError::Unexpected(format!(
                        "Match {} correction completed without its owned claim",
                        request.match_id
                    ))),
                    Err(error) => Err(unexpected_repository_error(error)),
                }
            }
            other => other,
        };

        // Mirrors Python's finally block.  On failure this releases only the
        // lease; the durable pending/core_applied state remains retryable.
        if let Err(error) = self
            .repository
            .release_match_correction_claim(request.match_id, &owner_token)
        {
            error!(
                match_id = request.match_id,
                %error,
                "could not release admin match-correction lease"
            );
        }
        result
    }

    fn correct_owned(
        &self,
        request: AdminCorrectMatchRequest,
        owner_token: &str,
        claim: &cama_db::match_correction_repository::CorrectionClaim,
    ) -> Result<AdminCorrectMatchResult, AdminCorrectMatchError> {
        let reason = format!("{CORRECTION_REASON_PREFIX}{}", request.match_id);
        let pending_reason = self
            .repository
            .pending_openskill_replay(Some(request.guild_id))
            .map_err(unexpected_repository_error)?;

        match claim.state {
            ClaimState::Already if pending_reason.as_deref() != Some(reason.as_str()) => {
                return Err(AdminCorrectMatchError::InvalidRequest(format!(
                    "Match {} already has {} as winner",
                    request.match_id,
                    request.correct_result.label()
                )));
            }
            ClaimState::CoreApplied | ClaimState::Already => {
                return self.recover_after_core(request, claim, pending_reason.is_some());
            }
            ClaimState::Pending => {}
        }

        let context = self
            .repository
            .correction_context(request.match_id, Some(request.guild_id))
            .map_err(classify_repository_error)?;
        if context.participants.is_empty() {
            return Err(AdminCorrectMatchError::InvalidRequest(format!(
                "No participants found for match {}",
                request.match_id
            )));
        }
        let snapshots = self
            .repository
            .rating_snapshots(request.match_id, Some(request.guild_id))
            .map_err(unexpected_repository_error)?;
        if snapshots.is_empty() {
            return Err(AdminCorrectMatchError::InvalidRequest(format!(
                "No rating history found for match {}. Cannot correct matches without stored snapshots.",
                request.match_id
            )));
        }

        let new_side = database_side(request.correct_result);
        let rating_updates = self
            .repository
            .prepare_rating_correction_configured(
                request.match_id,
                Some(request.guild_id),
                new_side,
                &self.openskill,
                &self.rating,
            )
            .map_err(classify_repository_error)?;

        self.move_win_bonuses(request.guild_id, &context, new_side)?;
        let fingerprint = self.openskill.algorithm_fingerprint();
        let correction_id = self
            .repository
            .apply_core_correction(&CoreCorrectionRequest {
                match_id: request.match_id,
                guild_id: Some(request.guild_id),
                old_winning_team: context.winning_team,
                new_winning_team: new_side,
                corrected_by: Some(request.actor_id),
                owner_token,
                rating_updates: &rating_updates,
                openskill_algorithm_version: OPENSKILL_REPLAY_ALGORITHM_VERSION,
                openskill_algorithm_fingerprint: &fingerprint,
            })
            .map_err(classify_repository_error)?;

        let bet_correction = self.settle_bets(request.guild_id, &context, new_side)?;
        let replayed = self.replay(request.guild_id, request.match_id)?;
        Ok(AdminCorrectMatchResult {
            old_winning_team: runtime_side(context.winning_team),
            new_winning_team: request.correct_result,
            correction_id,
            players_affected: context.participants.len(),
            ratings_updated: rating_updates.len(),
            openskill_matches_replayed: replayed,
            bet_correction,
            replay_recovered: false,
        })
    }

    fn recover_after_core(
        &self,
        request: AdminCorrectMatchRequest,
        claim: &cama_db::match_correction_repository::CorrectionClaim,
        replay_pending: bool,
    ) -> Result<AdminCorrectMatchResult, AdminCorrectMatchError> {
        let context = self
            .repository
            .correction_context(request.match_id, Some(request.guild_id))
            .map_err(classify_repository_error)?;
        let requested_side = database_side(request.correct_result);
        let bet_correction = self.settle_bets(request.guild_id, &context, requested_side)?;
        // A core_applied claim with no job means replay committed and the
        // process died before deleting the claim.  Bet settlement precedes
        // replay and is exact-once, so completing the claim is safe.
        let replayed = if replay_pending {
            self.replay(request.guild_id, request.match_id)?
        } else {
            0
        };
        Ok(AdminCorrectMatchResult {
            old_winning_team: request.correct_result,
            new_winning_team: request.correct_result,
            correction_id: claim.correction_id,
            players_affected: 0,
            ratings_updated: 0,
            openskill_matches_replayed: replayed,
            bet_correction,
            replay_recovered: true,
        })
    }

    fn move_win_bonuses(
        &self,
        guild_id: i64,
        context: &cama_db::match_correction_repository::CorrectionMatchContext,
        new_side: MatchSide,
    ) -> Result<(), AdminCorrectMatchError> {
        for participant in context
            .participants
            .iter()
            .filter(|participant| participant.team == new_side)
        {
            if participant.win_bonus_jc.is_some_and(|amount| amount > 0) {
                continue;
            }
            // A ledger marker with a NULL participant snapshot means the
            // process exited after the exact-once reward transaction but
            // before this coordinator persisted its reversal amount. Re-enter
            // the production adapter: it recovers the full balance delta,
            // including Communion and protection-aware Blood Pact effects,
            // without paying twice. Merely skipping on the marker would leave
            // the snapshot permanently incomplete and a later re-correction
            // unable to reverse the amount that actually moved.
            let awarded = self
                .rewards
                .award_match_win_bonus(CorrectionWinRewardRequest {
                    guild_id,
                    match_id: context.match_id,
                    discord_id: participant.discord_id,
                    gross_reward: context.recorded_win_reward_jc,
                })
                .map_err(AdminCorrectMatchError::Unexpected)?;
            self.repository
                .snapshot_win_bonus(
                    context.match_id,
                    Some(guild_id),
                    participant.discord_id,
                    awarded.snapshot_balance_delta,
                )
                .map_err(unexpected_repository_error)?;
        }

        let debits = context
            .participants
            .iter()
            .filter(|participant| participant.team == context.winning_team)
            .filter_map(|participant| {
                let amount = participant
                    .win_bonus_jc
                    .unwrap_or(context.recorded_win_reward_jc);
                (amount > 0).then_some((participant.discord_id, amount))
            })
            .collect::<BTreeMap<_, _>>();
        self.repository
            .reverse_win_bonuses_atomic(context.match_id, Some(guild_id), &debits)
            .map_err(unexpected_repository_error)
    }

    fn settle_bets(
        &self,
        guild_id: i64,
        context: &cama_db::match_correction_repository::CorrectionMatchContext,
        new_side: MatchSide,
    ) -> Result<Option<AdminCorrectMatchBetResult>, AdminCorrectMatchError> {
        let bets = self
            .repository
            .get_settled_bets_for_match(context.match_id)
            .map_err(unexpected_repository_error)?;
        if bets.is_empty() {
            return Ok(None);
        }
        let taxable = self.vanity_tax.taxable_ids(guild_id);
        let result = self
            .repository
            .settle_bet_correction_atomic(
                context.match_id,
                Some(guild_id),
                new_side,
                BetCorrectionOptions {
                    pool_mode: context.pool_betting_mode,
                    house_payout_multiplier: self.house_payout_multiplier,
                    vanity_tax_rate: self.vanity_tax_rate,
                    vanity_taxable_ids: &taxable,
                },
            )
            .map_err(unexpected_repository_error)?;
        Ok(Some(AdminCorrectMatchBetResult {
            bets_affected: result.bets_affected,
            old_winners_reversed: result.old_winners_reversed,
            new_winners_paid: result.new_winners_paid,
        }))
    }

    fn replay(&self, guild_id: i64, match_id: i64) -> Result<usize, AdminCorrectMatchError> {
        self.replay_gate
            .before_replay(guild_id, match_id)
            .map_err(AdminCorrectMatchError::Unexpected)?;
        let (summary, _) = self
            .repository
            .replay_openskill_atomic_configured(
                Some(guild_id),
                &self.openskill,
                &self.rating,
                self.new_player_mmr_discount,
            )
            .map_err(unexpected_repository_error)?;
        if !summary.errors.is_empty() {
            return Err(AdminCorrectMatchError::Unexpected(format!(
                "OpenSkill replay failed: {}",
                summary.errors.join("; ")
            )));
        }
        Ok(summary.matches_processed)
    }
}

struct AdminMatchCorrectionRecoveryObserver {
    control: Arc<ProductionAdminMatchCorrectionControl>,
    /// READY also runs after gateway reconnects. Dead-process leases must be
    /// cleared exactly once per observed guild, never during a live command
    /// after that guild has resumed.
    startup_guild_leases_released: Mutex<BTreeSet<i64>>,
}

#[async_trait]
impl GatewayEventObserver for AdminMatchCorrectionRecoveryObserver {
    fn name(&self) -> &'static str {
        RECOVERY_OBSERVER_NAME
    }

    async fn ready_recovery(&self, context: ReadyRecoveryContext) -> ReadyRecoveryReport {
        let ready_guild_ids = context
            .guild_ids()
            .iter()
            .filter_map(|guild_id| i64::try_from(*guild_id).ok())
            .collect::<BTreeSet<_>>();
        {
            // Serialize simultaneous READY/reconnect callbacks around the
            // one-time lease release. A follower must not enumerate claims
            // until the first callback has finished clearing dead owners.
            let mut released = self.startup_guild_leases_released.lock().await;
            let unreleased = ready_guild_ids
                .difference(&released)
                .copied()
                .collect::<BTreeSet<_>>();
            if !unreleased.is_empty() {
                if let Err(error) = self
                    .control
                    .repository
                    .release_match_correction_leases_for_startup_guilds(&unreleased)
                {
                    return ReadyRecoveryReport {
                        observer: RECOVERY_OBSERVER_NAME,
                        guilds_attempted: 0,
                        guilds_refreshed: 0,
                        guilds_superseded: 0,
                        members_refreshed: 0,
                        failures: vec![ReadyRecoveryFailure {
                            guild_id: 0,
                            message: format!("could not release dead correction leases: {error}"),
                        }],
                    };
                }
                released.extend(unreleased);
            }
        }
        let claims = match self.control.repository.list_recoverable_match_corrections() {
            Ok(claims) => claims,
            Err(error) => {
                return ReadyRecoveryReport {
                    observer: RECOVERY_OBSERVER_NAME,
                    guilds_attempted: 0,
                    guilds_refreshed: 0,
                    guilds_superseded: 0,
                    members_refreshed: 0,
                    failures: vec![ReadyRecoveryFailure {
                        guild_id: 0,
                        message: error.to_string(),
                    }],
                };
            }
        };
        let jobs = match self.control.repository.list_pending_openskill_replays() {
            Ok(jobs) => jobs,
            Err(error) => {
                return ReadyRecoveryReport {
                    observer: RECOVERY_OBSERVER_NAME,
                    guilds_attempted: 0,
                    guilds_refreshed: 0,
                    guilds_superseded: 0,
                    members_refreshed: 0,
                    failures: vec![ReadyRecoveryFailure {
                        guild_id: 0,
                        message: error.to_string(),
                    }],
                };
            }
        };
        let mut corrections = claims
            .into_iter()
            .filter(|claim| ready_guild_ids.contains(&claim.guild_id))
            .map(|claim| {
                (
                    (claim.guild_id, claim.match_id),
                    Some(runtime_side(claim.new_winning_team)),
                )
            })
            .collect::<BTreeMap<_, _>>();
        for job in jobs
            .into_iter()
            .filter(|job| ready_guild_ids.contains(&job.guild_id))
        {
            if let Some(match_id) = job
                .reason
                .strip_prefix(CORRECTION_REASON_PREFIX)
                .and_then(|value| value.parse::<i64>().ok())
            {
                corrections.entry((job.guild_id, match_id)).or_default();
            }
        }
        let mut report =
            ReadyRecoveryReport::empty(RECOVERY_OBSERVER_NAME, context.guild_ids().len());
        for ((guild_id, match_id), requested_side) in corrections {
            let correct_result = if let Some(side) = requested_side {
                side
            } else {
                match self
                    .control
                    .repository
                    .current_winner(match_id, Some(guild_id))
                {
                    Ok(winner) => runtime_side(winner),
                    Err(error) => {
                        report.failures.push(ReadyRecoveryFailure {
                            guild_id: u64::try_from(guild_id).unwrap_or_default(),
                            message: error.to_string(),
                        });
                        continue;
                    }
                }
            };
            let request = AdminCorrectMatchRequest {
                guild_id,
                actor_id: 0,
                match_id,
                correct_result,
            };
            match self.control.correct_match(request).await {
                Ok(result) => {
                    report.guilds_refreshed += 1;
                    info!(
                        guild_id,
                        match_id,
                        replay_recovered = result.replay_recovered,
                        "recovered admin match correction"
                    );
                }
                Err(error) => report.failures.push(ReadyRecoveryFailure {
                    guild_id: u64::try_from(guild_id).unwrap_or_default(),
                    message: format!(
                        "match {match_id}: {}",
                        match error {
                            AdminCorrectMatchError::InvalidRequest(message)
                            | AdminCorrectMatchError::Unexpected(message) => message,
                        }
                    ),
                }),
            }
        }
        report
    }
}

fn classify_repository_error(error: MatchCorrectionError) -> AdminCorrectMatchError {
    match error {
        MatchCorrectionError::MatchNotFound(_)
        | MatchCorrectionError::AlreadyWinner { .. }
        | MatchCorrectionError::MissingParticipant { .. } => {
            AdminCorrectMatchError::InvalidRequest(error.to_string())
        }
        _ => unexpected_repository_error(error),
    }
}

fn unexpected_repository_error(error: MatchCorrectionError) -> AdminCorrectMatchError {
    AdminCorrectMatchError::Unexpected(error.to_string())
}

const fn database_side(side: AdminCorrectMatchSide) -> MatchSide {
    match side {
        AdminCorrectMatchSide::Radiant => MatchSide::Radiant,
        AdminCorrectMatchSide::Dire => MatchSide::Dire,
    }
}

const fn runtime_side(side: MatchSide) -> AdminCorrectMatchSide {
    match side {
        MatchSide::Radiant => AdminCorrectMatchSide::Radiant,
        MatchSide::Dire => AdminCorrectMatchSide::Dire,
    }
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

fn owner_token() -> String {
    let sequence = OWNER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("rust-admin-{}-{nanos}-{sequence}", std::process::id())
}

#[cfg(test)]
#[path = "admin_match_correction/tests.rs"]
mod tests;
