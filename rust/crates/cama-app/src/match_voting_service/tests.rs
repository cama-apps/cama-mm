use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use super::*;

const GUILD: i64 = 12_345;

#[derive(Clone, Debug)]
struct MemoryPort {
    state: Arc<Mutex<MemoryState>>,
}

#[derive(Clone, Debug, Default)]
struct MemoryState {
    matches: Vec<PendingVoteSnapshot>,
    cas_calls: usize,
}

impl MemoryPort {
    fn new(matches: Vec<PendingVoteSnapshot>) -> Self {
        Self {
            state: Arc::new(Mutex::new(MemoryState {
                matches,
                cas_calls: 0,
            })),
        }
    }

    fn state(&self) -> MemoryState {
        self.state.lock().expect("memory port lock").clone()
    }
}

impl MatchVotingPort for MemoryPort {
    type Error = Infallible;

    fn snapshot(&self, key: PendingVoteKey) -> Result<Option<PendingVoteSnapshot>, Self::Error> {
        let guild_id = key.guild_id.unwrap_or(0);
        let state = self.state.lock().expect("memory port lock");
        let matches = state
            .matches
            .iter()
            .filter(|snapshot| snapshot.guild_id == guild_id)
            .collect::<Vec<_>>();
        Ok(if let Some(pending_match_id) = key.pending_match_id {
            matches
                .into_iter()
                .find(|snapshot| snapshot.pending_match_id == pending_match_id)
                .cloned()
        } else if matches.len() == 1 {
            matches.first().map(|snapshot| (*snapshot).clone())
        } else {
            None
        })
    }

    fn compare_and_set_submission(
        &self,
        request: VoteSubmissionCas<'_>,
    ) -> Result<VoteSubmissionCasOutcome, Self::Error> {
        let guild_id = request.guild_id.unwrap_or(0);
        let mut state = self.state.lock().expect("memory port lock");
        let Some(snapshot) = state.matches.iter_mut().find(|snapshot| {
            snapshot.guild_id == guild_id && snapshot.pending_match_id == request.pending_match_id
        }) else {
            return Ok(VoteSubmissionCasOutcome::MissingMatch);
        };
        if snapshot.payload != request.expected_payload {
            return Ok(VoteSubmissionCasOutcome::Conflict(snapshot.clone()));
        }
        if let Some(existing) = snapshot
            .submissions
            .iter_mut()
            .find(|submission| submission.user_id == Some(request.user_id))
        {
            existing.result = Some(request.result.to_owned());
            existing.is_admin = request.is_admin;
        } else {
            snapshot.submissions.push(PersistedVoteSubmission {
                user_id: Some(request.user_id),
                result: Some(request.result.to_owned()),
                is_admin: request.is_admin,
            });
        }
        snapshot.payload.push('#');
        let applied = snapshot.clone();
        state.cas_calls += 1;
        Ok(VoteSubmissionCasOutcome::Applied(applied))
    }
}

type TestService = MatchVotingService<MemoryPort>;

fn pending_match(pending_match_id: i64) -> PendingVoteSnapshot {
    PendingVoteSnapshot {
        pending_match_id,
        guild_id: GUILD,
        payload: format!("pending-{pending_match_id}"),
        radiant_team_ids: vec![1, 2, 3, 4, 5],
        dire_team_ids: vec![6, 7, 8, 9, 10],
        excluded_player_ids: Vec::new(),
        excluded_conditional_player_ids: Vec::new(),
        submissions: Vec::new(),
    }
}

fn service(matches: Vec<PendingVoteSnapshot>) -> TestService {
    MatchVotingService::new(MemoryPort::new(matches))
}

fn implicit_key() -> PendingVoteKey {
    PendingVoteKey {
        guild_id: Some(GUILD),
        pending_match_id: None,
    }
}

fn explicit_key(pending_match_id: i64) -> PendingVoteKey {
    PendingVoteKey {
        guild_id: Some(GUILD),
        pending_match_id: Some(pending_match_id),
    }
}

fn populated_service() -> TestService {
    service(vec![pending_match(1)])
}

fn add_record(service: &TestService, user_id: i64, side: &str, is_admin: bool) {
    service
        .add_record_submission(implicit_key(), user_id, side, is_admin)
        .expect("record vote");
}

fn add_abort(service: &TestService, user_id: i64, is_admin: bool) {
    service
        .add_abort_submission(implicit_key(), user_id, is_admin)
        .expect("abort vote");
}

#[test]
fn test_no_pending_match_reports_no_votes_and_not_ready() {
    let service = service(Vec::new());
    assert_eq!(
        service.get_vote_counts(implicit_key()).expect("counts"),
        MatchVoteCounts::default()
    );
    assert_eq!(
        service
            .get_non_admin_submission_count(implicit_key())
            .expect("count"),
        0
    );
    assert_eq!(
        service
            .get_abort_submission_count(implicit_key())
            .expect("count"),
        0
    );
    assert_eq!(
        service
            .get_pending_record_result(implicit_key())
            .expect("result"),
        None
    );
    assert!(!service.can_record_match(implicit_key()).expect("ready"));
    assert!(!service.can_abort_match(implicit_key()).expect("ready"));
    assert!(!service.has_admin_submission(implicit_key()).expect("admin"));
    assert!(
        !service
            .has_admin_abort_submission(implicit_key())
            .expect("admin abort")
    );
}

#[test]
fn test_query_for_unknown_match_id_is_safe() {
    let service = populated_service();
    let missing = explicit_key(1_000);
    assert_eq!(
        service.get_vote_counts(missing).expect("counts"),
        MatchVoteCounts::default()
    );
    assert!(!service.can_record_match(missing).expect("ready"));
    assert_eq!(
        service.get_pending_record_result(missing).expect("result"),
        None
    );
}

#[test]
fn test_record_submission_without_pending_match_raises() {
    let service = service(Vec::new());
    assert!(matches!(
        service.add_record_submission(implicit_key(), 1, "radiant", false),
        Err(MatchVotingServiceError::Policy(
            MatchVotingPolicyError::NoRecentShuffle
        ))
    ));
}

#[test]
fn test_abort_submission_without_pending_match_raises() {
    let service = service(Vec::new());
    assert!(matches!(
        service.add_abort_submission(implicit_key(), 1, false),
        Err(MatchVotingServiceError::Policy(
            MatchVotingPolicyError::NoRecentShuffle
        ))
    ));
}

#[test]
fn test_invalid_result_string_rejected() {
    let service = populated_service();
    for result in ["abort", "garbage"] {
        assert!(matches!(
            service.add_record_submission(implicit_key(), 1, result, false),
            Err(MatchVotingServiceError::Policy(
                MatchVotingPolicyError::InvalidResult
            ))
        ));
    }
    assert_eq!(service.port().state().cas_calls, 0);
}

#[test]
fn test_user_cannot_switch_record_vote() {
    let service = populated_service();
    add_record(&service, 1, "radiant", false);
    assert!(matches!(
        service.add_record_submission(implicit_key(), 1, "dire", false),
        Err(MatchVotingServiceError::Policy(
            MatchVotingPolicyError::AlreadySubmitted
        ))
    ));
}

#[test]
fn test_user_cannot_switch_from_record_to_abort() {
    let service = populated_service();
    add_record(&service, 1, "radiant", false);
    assert!(matches!(
        service.add_abort_submission(implicit_key(), 1, false),
        Err(MatchVotingServiceError::Policy(
            MatchVotingPolicyError::AlreadySubmitted
        ))
    ));
}

#[test]
fn test_user_cannot_switch_from_abort_to_record() {
    let service = populated_service();
    add_abort(&service, 1, false);
    assert!(matches!(
        service.add_record_submission(implicit_key(), 1, "dire", false),
        Err(MatchVotingServiceError::Policy(
            MatchVotingPolicyError::AlreadySubmitted
        ))
    ));
}

#[test]
fn test_duplicate_identical_vote_is_idempotent() {
    let service = populated_service();
    add_record(&service, 1, "radiant", false);
    let result = service
        .add_record_submission(implicit_key(), 1, "radiant", false)
        .expect("duplicate vote");
    assert_eq!(
        result.vote_counts,
        MatchVoteCounts {
            radiant: 1,
            dire: 0
        }
    );
    assert_eq!((result.total_count, result.non_admin_count), (1, 1));
    assert_eq!(service.port().state().cas_calls, 1);
}

#[test]
fn test_non_participant_cannot_vote_on_result() {
    let service = populated_service();
    assert!(matches!(
        service.add_record_submission(implicit_key(), 555_555, "radiant", false),
        Err(MatchVotingServiceError::Policy(
            MatchVotingPolicyError::RecordVoterNotEligible
        ))
    ));
    assert_eq!(
        service.get_vote_counts(implicit_key()).expect("counts"),
        MatchVoteCounts::default()
    );
    assert!(!service.can_record_match(implicit_key()).expect("ready"));
}

#[test]
fn test_excluded_lobby_player_cannot_vote_on_result() {
    let mut pending = pending_match(1);
    pending.excluded_player_ids.push(11);
    let service = service(vec![pending]);
    assert!(matches!(
        service.add_record_submission(implicit_key(), 11, "dire", false),
        Err(MatchVotingServiceError::Policy(
            MatchVotingPolicyError::RecordVoterNotEligible
        ))
    ));
}

#[test]
fn test_admin_non_participant_can_still_vote_on_result() {
    let service = populated_service();
    let result = service
        .add_record_submission(implicit_key(), 424_242, "dire", true)
        .expect("admin vote");
    assert!(result.is_ready);
    assert_eq!(result.result, Some(TeamSide::Dire));
}

#[test]
fn test_persisted_ineligible_result_votes_do_not_count_toward_quorum() {
    let mut pending = pending_match(1);
    pending.submissions = [501, 502, 503]
        .into_iter()
        .map(|user_id| PersistedVoteSubmission {
            user_id: Some(user_id),
            result: Some("radiant".to_owned()),
            is_admin: false,
        })
        .collect();
    let service = service(vec![pending]);
    assert_eq!(
        service.get_vote_counts(implicit_key()).expect("counts"),
        MatchVoteCounts::default()
    );
    assert_eq!(
        service
            .get_non_admin_submission_count(implicit_key())
            .expect("count"),
        0
    );
    assert_eq!(
        service
            .get_pending_record_result(implicit_key())
            .expect("result"),
        None
    );
    for user_id in 1..=3 {
        add_record(&service, user_id, "dire", false);
    }
    assert_eq!(
        service.get_vote_counts(implicit_key()).expect("counts"),
        MatchVoteCounts {
            radiant: 0,
            dire: 3
        }
    );
    assert_eq!(
        service
            .get_pending_record_result(implicit_key())
            .expect("result"),
        Some(TeamSide::Dire)
    );
}

#[test]
fn test_below_threshold_non_admin_votes_cannot_record() {
    let service = populated_service();
    add_record(&service, 1, "radiant", false);
    let result = service
        .add_record_submission(implicit_key(), 2, "radiant", false)
        .expect("second vote");
    assert_eq!(
        result.vote_counts,
        MatchVoteCounts {
            radiant: 2,
            dire: 0
        }
    );
    assert!(!result.is_ready);
    assert_eq!(result.result, None);
    assert!(!service.can_record_match(implicit_key()).expect("ready"));
}

#[test]
fn test_third_matching_non_admin_vote_reaches_threshold() {
    let service = populated_service();
    add_record(&service, 1, "radiant", false);
    add_record(&service, 2, "radiant", false);
    let result = service
        .add_record_submission(implicit_key(), 3, "radiant", false)
        .expect("third vote");
    assert!(result.is_ready);
    assert_eq!(result.result, Some(TeamSide::Radiant));
    assert!(service.can_record_match(implicit_key()).expect("ready"));
}

#[test]
fn test_split_non_admin_votes_do_not_reach_threshold() {
    let service = populated_service();
    for (user_id, side) in [(1, "radiant"), (2, "radiant"), (3, "dire"), (4, "dire")] {
        add_record(&service, user_id, side, false);
    }
    assert_eq!(
        service.get_vote_counts(implicit_key()).expect("counts"),
        MatchVoteCounts {
            radiant: 2,
            dire: 2
        }
    );
    assert_eq!(
        service
            .get_pending_record_result(implicit_key())
            .expect("result"),
        None
    );
}

#[test]
fn test_contested_vote_records_side_that_first_hits_threshold() {
    let service = populated_service();
    for (user_id, side) in [(1, "dire"), (2, "radiant"), (3, "radiant")] {
        add_record(&service, user_id, side, false);
    }
    let result = service
        .add_record_submission(implicit_key(), 4, "radiant", false)
        .expect("deciding vote");
    assert_eq!(
        result.vote_counts,
        MatchVoteCounts {
            radiant: 3,
            dire: 1
        }
    );
    assert_eq!(result.result, Some(TeamSide::Radiant));
}

#[test]
fn test_single_admin_record_vote_is_decisive() {
    let service = populated_service();
    let result = service
        .add_record_submission(implicit_key(), 999, "dire", true)
        .expect("admin vote");
    assert!(result.is_ready);
    assert_eq!(result.result, Some(TeamSide::Dire));
    assert_eq!(result.non_admin_count, 0);
    assert!(service.has_admin_submission(implicit_key()).expect("admin"));
}

#[test]
fn test_admin_vote_overrides_contested_non_admin_majority() {
    let service = populated_service();
    for user_id in 1..=3 {
        add_record(&service, user_id, "radiant", false);
    }
    add_record(&service, 999, "dire", true);
    assert_eq!(
        service
            .get_pending_record_result(implicit_key())
            .expect("result"),
        Some(TeamSide::Dire)
    );
}

#[test]
fn test_admin_vote_excluded_from_non_admin_tallies() {
    let service = populated_service();
    add_record(&service, 1, "radiant", false);
    add_record(&service, 2, "radiant", false);
    add_record(&service, 999, "radiant", true);
    assert_eq!(
        service.get_vote_counts(implicit_key()).expect("counts"),
        MatchVoteCounts {
            radiant: 2,
            dire: 0
        }
    );
    assert_eq!(
        service
            .get_non_admin_submission_count(implicit_key())
            .expect("count"),
        2
    );
    assert!(service.can_record_match(implicit_key()).expect("ready"));
}

#[test]
fn test_single_admin_abort_vote_is_decisive() {
    let service = populated_service();
    let result = service
        .add_abort_submission(implicit_key(), 999, true)
        .expect("admin abort");
    assert!(result.is_ready);
    assert!(
        service
            .has_admin_abort_submission(implicit_key())
            .expect("admin abort")
    );
    assert!(service.can_abort_match(implicit_key()).expect("ready"));
}

#[test]
fn test_below_threshold_non_admin_aborts_cannot_abort() {
    let service = populated_service();
    add_abort(&service, 1, false);
    add_abort(&service, 2, false);
    let result = service
        .add_abort_submission(implicit_key(), 3, false)
        .expect("third abort");
    assert_eq!(result.non_admin_count, 3);
    assert!(!result.is_ready);
}

#[test]
fn test_fourth_non_admin_abort_reaches_threshold() {
    let service = populated_service();
    for user_id in 1..=3 {
        add_abort(&service, user_id, false);
    }
    let result = service
        .add_abort_submission(implicit_key(), 4, false)
        .expect("fourth abort");
    assert_eq!(result.non_admin_count, 4);
    assert!(result.is_ready);
    assert!(service.can_abort_match(implicit_key()).expect("ready"));
}

#[test]
fn test_abort_votes_do_not_count_as_record_votes() {
    let service = populated_service();
    for user_id in 1..=4 {
        add_abort(&service, user_id, false);
    }
    assert_eq!(
        service.get_vote_counts(implicit_key()).expect("counts"),
        MatchVoteCounts::default()
    );
    assert!(!service.can_record_match(implicit_key()).expect("ready"));
    assert_eq!(
        service
            .get_pending_record_result(implicit_key())
            .expect("result"),
        None
    );
}

#[test]
fn test_admin_abort_vote_does_not_make_match_recordable() {
    let service = populated_service();
    add_abort(&service, 999, true);
    assert!(
        !service
            .has_admin_submission(implicit_key())
            .expect("admin record")
    );
    assert!(!service.can_record_match(implicit_key()).expect("record"));
    assert!(service.can_abort_match(implicit_key()).expect("abort"));
}

#[test]
fn test_duplicate_abort_vote_is_idempotent() {
    let service = populated_service();
    add_abort(&service, 1, false);
    let result = service
        .add_abort_submission(implicit_key(), 1, false)
        .expect("duplicate abort");
    assert_eq!((result.non_admin_count, result.total_count), (1, 1));
    assert_eq!(service.port().state().cas_calls, 1);
}

#[test]
fn test_non_lobby_player_cannot_vote_to_abort() {
    let service = populated_service();
    assert!(matches!(
        service.add_abort_submission(implicit_key(), 999_999, false),
        Err(MatchVotingServiceError::Policy(
            MatchVotingPolicyError::AbortVoterNotEligible
        ))
    ));
    assert_eq!(
        service
            .get_abort_submission_count(implicit_key())
            .expect("count"),
        0
    );
}

#[test]
fn test_excluded_lobby_player_can_vote_to_abort() {
    let mut pending = pending_match(1);
    pending.excluded_player_ids.push(11);
    pending.excluded_conditional_player_ids.push(12);
    let service = service(vec![pending]);
    add_abort(&service, 11, false);
    let result = service
        .add_abort_submission(implicit_key(), 12, false)
        .expect("conditional exclusion abort");
    assert_eq!(result.non_admin_count, 2);
    assert!(!result.is_ready);
}

#[test]
fn test_votes_persist_across_service_instances() {
    let port = MemoryPort::new(vec![pending_match(1)]);
    let first = MatchVotingService::new(port.clone());
    for user_id in 1..=3 {
        add_record(&first, user_id, "radiant", false);
    }
    let reloaded = MatchVotingService::new(port);
    assert_eq!(
        reloaded.get_vote_counts(implicit_key()).expect("counts"),
        MatchVoteCounts {
            radiant: 3,
            dire: 0
        }
    );
    assert!(reloaded.can_record_match(implicit_key()).expect("ready"));
    assert_eq!(
        reloaded
            .get_pending_record_result(implicit_key())
            .expect("result"),
        Some(TeamSide::Radiant)
    );
}

#[test]
fn test_votes_isolated_between_concurrent_matches() {
    let mut second = pending_match(2);
    second.radiant_team_ids = vec![11, 12];
    second.dire_team_ids = vec![13, 14];
    let service = service(vec![pending_match(1), second]);
    service
        .add_record_submission(explicit_key(1), 999, "radiant", true)
        .expect("match A admin vote");
    assert!(service.can_record_match(explicit_key(1)).expect("match A"));
    assert!(!service.can_record_match(explicit_key(2)).expect("match B"));
    assert_eq!(
        service
            .get_vote_counts(explicit_key(2))
            .expect("match B counts"),
        MatchVoteCounts::default()
    );
}
