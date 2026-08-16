//! Discord-neutral pending-match voting policy.
//!
//! Result and abort votes share one durable per-user slot, matching Python's
//! `record_submissions` payload. The service re-evaluates eligibility and
//! quorum after every payload-CAS conflict so concurrent command handlers do
//! not overwrite one another.

use std::collections::BTreeSet;

use cama_db::match_recording_repository::TeamSide;
use cama_db::match_voting::{
    MatchVotingRepository, MatchVotingRepositoryError, PendingVoteKey, PendingVoteSnapshot,
    PersistedVoteSubmission, VoteSubmissionCas, VoteSubmissionCasOutcome,
};
use thiserror::Error;

pub const MIN_NON_ADMIN_SUBMISSIONS: usize = 3;
pub const MIN_ABORT_SUBMISSIONS: usize = 4;
const MAX_CAS_ATTEMPTS: usize = 16;

pub trait MatchVotingPort {
    type Error;

    fn snapshot(&self, key: PendingVoteKey) -> Result<Option<PendingVoteSnapshot>, Self::Error>;

    fn compare_and_set_submission(
        &self,
        request: VoteSubmissionCas<'_>,
    ) -> Result<VoteSubmissionCasOutcome, Self::Error>;
}

impl MatchVotingPort for MatchVotingRepository {
    type Error = MatchVotingRepositoryError;

    fn snapshot(&self, key: PendingVoteKey) -> Result<Option<PendingVoteSnapshot>, Self::Error> {
        MatchVotingRepository::snapshot(self, key)
    }

    fn compare_and_set_submission(
        &self,
        request: VoteSubmissionCas<'_>,
    ) -> Result<VoteSubmissionCasOutcome, Self::Error> {
        MatchVotingRepository::compare_and_set_submission(self, request)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MatchVoteCounts {
    pub radiant: usize,
    pub dire: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordSubmissionResult {
    pub pending_match_id: i64,
    pub non_admin_count: usize,
    pub total_count: usize,
    pub result: Option<TeamSide>,
    pub is_ready: bool,
    pub vote_counts: MatchVoteCounts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AbortSubmissionResult {
    pub pending_match_id: i64,
    pub non_admin_count: usize,
    pub total_count: usize,
    pub is_ready: bool,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MatchVotingPolicyError {
    #[error("No recent shuffle found.")]
    NoRecentShuffle,
    #[error("Result must be 'radiant' or 'dire'.")]
    InvalidResult,
    #[error("You already submitted a different result.")]
    AlreadySubmitted,
    #[error("Only match participants can vote on the result.")]
    RecordVoterNotEligible,
    #[error("Only players in the shuffled lobby can vote to abort this match.")]
    AbortVoterNotEligible,
    #[error("pending-match voting remained contested after repeated CAS retries")]
    ConcurrencyExhausted,
}

#[derive(Debug)]
pub enum MatchVotingServiceError<E> {
    Port(E),
    Policy(MatchVotingPolicyError),
}

#[derive(Clone, Debug)]
pub struct MatchVotingService<P> {
    port: P,
}

impl<P: MatchVotingPort> MatchVotingService<P> {
    #[must_use]
    pub const fn new(port: P) -> Self {
        Self { port }
    }

    #[must_use]
    pub const fn port(&self) -> &P {
        &self.port
    }

    pub fn get_vote_counts(&self, key: PendingVoteKey) -> Result<MatchVoteCounts, P::Error> {
        Ok(self
            .port
            .snapshot(key)?
            .as_ref()
            .map_or_else(MatchVoteCounts::default, vote_counts))
    }

    pub fn get_non_admin_submission_count(&self, key: PendingVoteKey) -> Result<usize, P::Error> {
        Ok(self
            .port
            .snapshot(key)?
            .as_ref()
            .map_or(0, non_admin_record_count))
    }

    pub fn get_abort_submission_count(&self, key: PendingVoteKey) -> Result<usize, P::Error> {
        Ok(self
            .port
            .snapshot(key)?
            .as_ref()
            .map_or(0, non_admin_abort_count))
    }

    pub fn has_admin_submission(&self, key: PendingVoteKey) -> Result<bool, P::Error> {
        Ok(self
            .port
            .snapshot(key)?
            .as_ref()
            .is_some_and(has_admin_record_submission))
    }

    pub fn has_admin_abort_submission(&self, key: PendingVoteKey) -> Result<bool, P::Error> {
        Ok(self
            .port
            .snapshot(key)?
            .as_ref()
            .is_some_and(has_admin_abort))
    }

    pub fn get_pending_record_result(
        &self,
        key: PendingVoteKey,
    ) -> Result<Option<TeamSide>, P::Error> {
        Ok(self
            .port
            .snapshot(key)?
            .as_ref()
            .and_then(pending_record_result))
    }

    pub fn can_record_match(&self, key: PendingVoteKey) -> Result<bool, P::Error> {
        self.get_pending_record_result(key)
            .map(|result| result.is_some())
    }

    pub fn can_abort_match(&self, key: PendingVoteKey) -> Result<bool, P::Error> {
        Ok(self.port.snapshot(key)?.as_ref().is_some_and(|snapshot| {
            has_admin_abort(snapshot) || non_admin_abort_count(snapshot) >= MIN_ABORT_SUBMISSIONS
        }))
    }

    pub fn add_record_submission(
        &self,
        key: PendingVoteKey,
        user_id: i64,
        result: &str,
        is_admin: bool,
    ) -> Result<RecordSubmissionResult, MatchVotingServiceError<P::Error>> {
        let side = TeamSide::parse(result).ok_or(MatchVotingServiceError::Policy(
            MatchVotingPolicyError::InvalidResult,
        ))?;
        let mut snapshot = self
            .port
            .snapshot(key)
            .map_err(MatchVotingServiceError::Port)?
            .ok_or(MatchVotingServiceError::Policy(
                MatchVotingPolicyError::NoRecentShuffle,
            ))?;
        for _ in 0..MAX_CAS_ATTEMPTS {
            if !is_admin && !record_voter_eligible(&snapshot, user_id) {
                return Err(MatchVotingServiceError::Policy(
                    MatchVotingPolicyError::RecordVoterNotEligible,
                ));
            }
            if let Some(existing) = submission_for(&snapshot, user_id) {
                if existing.result.as_deref() != Some(side.name()) {
                    return Err(MatchVotingServiceError::Policy(
                        MatchVotingPolicyError::AlreadySubmitted,
                    ));
                }
                if existing.is_admin == is_admin {
                    return Ok(record_summary(&snapshot));
                }
            }
            match self
                .port
                .compare_and_set_submission(VoteSubmissionCas {
                    guild_id: key.guild_id,
                    pending_match_id: snapshot.pending_match_id,
                    expected_payload: &snapshot.payload,
                    user_id,
                    result: side.name(),
                    is_admin,
                })
                .map_err(MatchVotingServiceError::Port)?
            {
                VoteSubmissionCasOutcome::Applied(applied) => {
                    return Ok(record_summary(&applied));
                }
                VoteSubmissionCasOutcome::Conflict(persisted) => snapshot = persisted,
                VoteSubmissionCasOutcome::MissingMatch => {
                    return Err(MatchVotingServiceError::Policy(
                        MatchVotingPolicyError::NoRecentShuffle,
                    ));
                }
            }
        }
        Err(MatchVotingServiceError::Policy(
            MatchVotingPolicyError::ConcurrencyExhausted,
        ))
    }

    pub fn add_abort_submission(
        &self,
        key: PendingVoteKey,
        user_id: i64,
        is_admin: bool,
    ) -> Result<AbortSubmissionResult, MatchVotingServiceError<P::Error>> {
        let mut snapshot = self
            .port
            .snapshot(key)
            .map_err(MatchVotingServiceError::Port)?
            .ok_or(MatchVotingServiceError::Policy(
                MatchVotingPolicyError::NoRecentShuffle,
            ))?;
        for _ in 0..MAX_CAS_ATTEMPTS {
            if !is_admin && !abort_voter_eligible(&snapshot, user_id) {
                return Err(MatchVotingServiceError::Policy(
                    MatchVotingPolicyError::AbortVoterNotEligible,
                ));
            }
            if let Some(existing) = submission_for(&snapshot, user_id) {
                if existing.result.as_deref() != Some("abort") {
                    return Err(MatchVotingServiceError::Policy(
                        MatchVotingPolicyError::AlreadySubmitted,
                    ));
                }
                if existing.is_admin == is_admin {
                    return Ok(abort_summary(&snapshot));
                }
            }
            match self
                .port
                .compare_and_set_submission(VoteSubmissionCas {
                    guild_id: key.guild_id,
                    pending_match_id: snapshot.pending_match_id,
                    expected_payload: &snapshot.payload,
                    user_id,
                    result: "abort",
                    is_admin,
                })
                .map_err(MatchVotingServiceError::Port)?
            {
                VoteSubmissionCasOutcome::Applied(applied) => {
                    return Ok(abort_summary(&applied));
                }
                VoteSubmissionCasOutcome::Conflict(persisted) => snapshot = persisted,
                VoteSubmissionCasOutcome::MissingMatch => {
                    return Err(MatchVotingServiceError::Policy(
                        MatchVotingPolicyError::NoRecentShuffle,
                    ));
                }
            }
        }
        Err(MatchVotingServiceError::Policy(
            MatchVotingPolicyError::ConcurrencyExhausted,
        ))
    }
}

fn record_summary(snapshot: &PendingVoteSnapshot) -> RecordSubmissionResult {
    let result = pending_record_result(snapshot);
    RecordSubmissionResult {
        pending_match_id: snapshot.pending_match_id,
        non_admin_count: non_admin_record_count(snapshot),
        total_count: snapshot.submissions.len(),
        result,
        is_ready: result.is_some(),
        vote_counts: vote_counts(snapshot),
    }
}

fn abort_summary(snapshot: &PendingVoteSnapshot) -> AbortSubmissionResult {
    let non_admin_count = non_admin_abort_count(snapshot);
    AbortSubmissionResult {
        pending_match_id: snapshot.pending_match_id,
        non_admin_count,
        total_count: snapshot.submissions.len(),
        is_ready: has_admin_abort(snapshot) || non_admin_count >= MIN_ABORT_SUBMISSIONS,
    }
}

fn pending_record_result(snapshot: &PendingVoteSnapshot) -> Option<TeamSide> {
    for submission in &snapshot.submissions {
        if submission.is_admin
            && let Some(side) = submission.result.as_deref().and_then(TeamSide::parse)
        {
            return Some(side);
        }
    }
    let counts = vote_counts(snapshot);
    if counts.radiant >= MIN_NON_ADMIN_SUBMISSIONS {
        Some(TeamSide::Radiant)
    } else if counts.dire >= MIN_NON_ADMIN_SUBMISSIONS {
        Some(TeamSide::Dire)
    } else {
        None
    }
}

fn vote_counts(snapshot: &PendingVoteSnapshot) -> MatchVoteCounts {
    let participants = participants(snapshot);
    let mut counts = MatchVoteCounts::default();
    for submission in snapshot.submissions.iter().filter(|submission| {
        !submission.is_admin
            && submission
                .user_id
                .is_some_and(|user_id| participants.contains(&user_id))
    }) {
        match submission.result.as_deref().and_then(TeamSide::parse) {
            Some(TeamSide::Radiant) => counts.radiant += 1,
            Some(TeamSide::Dire) => counts.dire += 1,
            None => {}
        }
    }
    counts
}

fn non_admin_record_count(snapshot: &PendingVoteSnapshot) -> usize {
    let counts = vote_counts(snapshot);
    counts.radiant + counts.dire
}

fn non_admin_abort_count(snapshot: &PendingVoteSnapshot) -> usize {
    let lobby_members = lobby_members(snapshot);
    snapshot
        .submissions
        .iter()
        .filter(|submission| {
            !submission.is_admin
                && submission.result.as_deref() == Some("abort")
                && submission
                    .user_id
                    .is_some_and(|user_id| lobby_members.contains(&user_id))
        })
        .count()
}

fn has_admin_record_submission(snapshot: &PendingVoteSnapshot) -> bool {
    snapshot.submissions.iter().any(|submission| {
        submission.is_admin
            && submission
                .result
                .as_deref()
                .and_then(TeamSide::parse)
                .is_some()
    })
}

fn has_admin_abort(snapshot: &PendingVoteSnapshot) -> bool {
    snapshot
        .submissions
        .iter()
        .any(|submission| submission.is_admin && submission.result.as_deref() == Some("abort"))
}

fn submission_for(
    snapshot: &PendingVoteSnapshot,
    user_id: i64,
) -> Option<&PersistedVoteSubmission> {
    snapshot
        .submissions
        .iter()
        .find(|submission| submission.user_id == Some(user_id))
}

fn record_voter_eligible(snapshot: &PendingVoteSnapshot, user_id: i64) -> bool {
    participants(snapshot).contains(&user_id)
}

fn abort_voter_eligible(snapshot: &PendingVoteSnapshot, user_id: i64) -> bool {
    lobby_members(snapshot).contains(&user_id)
}

fn participants(snapshot: &PendingVoteSnapshot) -> BTreeSet<i64> {
    snapshot
        .radiant_team_ids
        .iter()
        .chain(&snapshot.dire_team_ids)
        .copied()
        .collect()
}

fn lobby_members(snapshot: &PendingVoteSnapshot) -> BTreeSet<i64> {
    snapshot
        .radiant_team_ids
        .iter()
        .chain(&snapshot.dire_team_ids)
        .chain(&snapshot.excluded_player_ids)
        .chain(&snapshot.excluded_conditional_player_ids)
        .copied()
        .collect()
}

#[cfg(test)]
mod tests;
