"""
Tests for the match voting / veto service (``services/match_voting_service.py``).

MatchVotingService tracks two parallel votes on a pending match:
  * a *record* vote — which side (radiant/dire) won, and
  * an *abort* vote — discard the match.

The rules under test:
  * An admin vote is decisive on its own (single admin radiant/dire vote can
    record; single admin abort vote can abort).
  * Non-admin votes need ``MIN_NON_ADMIN_SUBMISSIONS`` (3) *matching* votes on
    one side before that side can record / abort.
  * A user may not switch their vote to a different result.
  * Vote tallies count non-admins only; admins are excluded from the counts so
    an admin can't be double-counted toward the non-admin threshold.

Each test asserts the *consequence* of the rule (can/can't record, which side
wins, the error raised) rather than echoing internal state, so it fails if the
threshold logic, the admin-override branch, or the conflict guard regress.
"""

import pytest

from services.match_state_service import MatchStateService
from services.match_voting_service import MatchVotingService
from tests.conftest import TEST_GUILD_ID

# =============================================================================
# FIXTURES
# =============================================================================


@pytest.fixture
def state_service(match_repository):
    """MatchStateService backed by a real, schema-initialized SQLite DB."""
    return MatchStateService(match_repository)


@pytest.fixture
def voting_service(state_service):
    """The service under test, wired with its real state-service dependency."""
    return MatchVotingService(state_service)


@pytest.fixture
def pending_match_id(state_service):
    """Create one pending match in the DB and return its id.

    A submission only works once a pending match exists; this gives every
    voting test a clean match to vote on.
    """
    from domain.models.pending_match_state import PendingMatchState

    state = PendingMatchState(
        radiant_team_ids=[1, 2, 3, 4, 5],
        dire_team_ids=[6, 7, 8, 9, 10],
    )
    return state_service.persist_state(TEST_GUILD_ID, state)


# =============================================================================
# NO-STATE / MISSING-MATCH EDGE CASES
# =============================================================================


def test_no_pending_match_reports_no_votes_and_not_ready(voting_service):
    """With no pending match at all, every query must degrade safely.

    The voting commands call these on guilds that may have nothing pending;
    they must return empty/False rather than raise or report a phantom match.
    """
    gid = TEST_GUILD_ID
    assert voting_service.get_vote_counts(gid) == {"radiant": 0, "dire": 0}
    assert voting_service.get_non_admin_submission_count(gid) == 0
    assert voting_service.get_abort_submission_count(gid) == 0
    assert voting_service.get_pending_record_result(gid) is None
    assert voting_service.can_record_match(gid) is False
    assert voting_service.can_abort_match(gid) is False
    assert voting_service.has_admin_submission(gid) is False
    assert voting_service.has_admin_abort_submission(gid) is False


def test_query_for_unknown_match_id_is_safe(voting_service, pending_match_id):
    """Querying a specific match id that does not exist must not leak the real one.

    Concurrent-match callers pass an explicit id; a stale/wrong id should look
    like an empty match, not silently fall back to another pending match.
    """
    missing_id = pending_match_id + 999
    assert voting_service.get_vote_counts(TEST_GUILD_ID, missing_id) == {"radiant": 0, "dire": 0}
    assert voting_service.can_record_match(TEST_GUILD_ID, missing_id) is False
    assert voting_service.get_pending_record_result(TEST_GUILD_ID, missing_id) is None


def test_record_submission_without_pending_match_raises(voting_service):
    """Voting before a match exists is a hard error, not a silent no-op.

    ``ensure_pending_state`` raises so a vote can never be recorded against a
    match that was never shuffled.
    """
    with pytest.raises(ValueError, match="No recent shuffle"):
        voting_service.add_record_submission(TEST_GUILD_ID, user_id=100, result="radiant", is_admin=False)


def test_abort_submission_without_pending_match_raises(voting_service):
    """Abort voting also requires an existing pending match."""
    with pytest.raises(ValueError, match="No recent shuffle"):
        voting_service.add_abort_submission(TEST_GUILD_ID, user_id=100, is_admin=False)


# =============================================================================
# INPUT VALIDATION
# =============================================================================


def test_invalid_result_string_rejected(voting_service, pending_match_id):
    """Only 'radiant'/'dire' are valid results — anything else is rejected.

    Guards against a typo'd or malicious result string being persisted as a
    vote that the threshold logic would then never be able to interpret.
    """
    with pytest.raises(ValueError, match="radiant.*dire"):
        voting_service.add_record_submission(TEST_GUILD_ID, user_id=100, result="abort", is_admin=False)
    with pytest.raises(ValueError, match="radiant.*dire"):
        voting_service.add_record_submission(TEST_GUILD_ID, user_id=100, result="garbage", is_admin=False)


def test_user_cannot_switch_record_vote(voting_service, pending_match_id):
    """A user who voted radiant cannot then vote dire.

    Vote-flipping would let one person inflate both tallies; the conflict guard
    must reject the second, differing vote.
    """
    voting_service.add_record_submission(TEST_GUILD_ID, user_id=100, result="radiant", is_admin=False)
    with pytest.raises(ValueError, match="already submitted"):
        voting_service.add_record_submission(TEST_GUILD_ID, user_id=100, result="dire", is_admin=False)


def test_user_cannot_switch_from_record_to_abort(voting_service, pending_match_id):
    """A user who voted a result cannot reuse their vote as an abort.

    record_submissions is a single per-user slot shared by record and abort
    votes, so abort must honor the same conflict guard.
    """
    voting_service.add_record_submission(TEST_GUILD_ID, user_id=100, result="radiant", is_admin=False)
    with pytest.raises(ValueError, match="already submitted"):
        voting_service.add_abort_submission(TEST_GUILD_ID, user_id=100, is_admin=False)


def test_user_cannot_switch_from_abort_to_record(voting_service, pending_match_id):
    """Symmetric guard: an abort voter cannot then cast a result vote."""
    voting_service.add_abort_submission(TEST_GUILD_ID, user_id=100, is_admin=False)
    with pytest.raises(ValueError, match="already submitted"):
        voting_service.add_record_submission(TEST_GUILD_ID, user_id=100, result="dire", is_admin=False)


def test_duplicate_identical_vote_is_idempotent(voting_service, pending_match_id):
    """Re-casting the *same* vote is allowed and does not double-count.

    Players often re-click the same button; the tally must stay at one vote
    for that user rather than rejecting them or counting them twice.
    """
    voting_service.add_record_submission(TEST_GUILD_ID, user_id=100, result="radiant", is_admin=False)
    result = voting_service.add_record_submission(TEST_GUILD_ID, user_id=100, result="radiant", is_admin=False)
    assert result["vote_counts"] == {"radiant": 1, "dire": 0}
    assert result["total_count"] == 1
    assert result["non_admin_count"] == 1


# =============================================================================
# NON-ADMIN RECORD THRESHOLD (requires MIN_NON_ADMIN_SUBMISSIONS matching votes)
# =============================================================================


def test_below_threshold_non_admin_votes_cannot_record(voting_service, pending_match_id):
    """Two matching non-admin votes are not enough — the threshold is 3.

    Recording on only two votes would let a minority decide the match result.
    """
    gid = TEST_GUILD_ID
    voting_service.add_record_submission(gid, user_id=100, result="radiant", is_admin=False)
    result = voting_service.add_record_submission(gid, user_id=101, result="radiant", is_admin=False)
    assert result["vote_counts"] == {"radiant": 2, "dire": 0}
    assert result["is_ready"] is False
    assert result["result"] is None
    assert voting_service.can_record_match(gid) is False


def test_third_matching_non_admin_vote_reaches_threshold(voting_service, pending_match_id):
    """The 3rd matching non-admin vote flips the match to recordable.

    This is the core non-admin record path: exactly MIN_NON_ADMIN_SUBMISSIONS
    agreeing voters wins.
    """
    gid = TEST_GUILD_ID
    voting_service.add_record_submission(gid, user_id=100, result="radiant", is_admin=False)
    voting_service.add_record_submission(gid, user_id=101, result="radiant", is_admin=False)
    result = voting_service.add_record_submission(gid, user_id=102, result="radiant", is_admin=False)
    assert result["is_ready"] is True
    assert result["result"] == "radiant"
    assert voting_service.can_record_match(gid) is True
    assert voting_service.get_pending_record_result(gid) == "radiant"


def test_split_non_admin_votes_do_not_reach_threshold(voting_service, pending_match_id):
    """Votes split across radiant and dire never sum to a quorum.

    The threshold is per-side: 2 radiant + 2 dire is 4 total votes but neither
    side hits 3, so the match must stay un-recordable instead of recording an
    arbitrary side.
    """
    gid = TEST_GUILD_ID
    voting_service.add_record_submission(gid, user_id=100, result="radiant", is_admin=False)
    voting_service.add_record_submission(gid, user_id=101, result="radiant", is_admin=False)
    voting_service.add_record_submission(gid, user_id=102, result="dire", is_admin=False)
    result = voting_service.add_record_submission(gid, user_id=103, result="dire", is_admin=False)
    assert result["vote_counts"] == {"radiant": 2, "dire": 2}
    assert result["total_count"] == 4
    assert result["is_ready"] is False
    assert voting_service.get_pending_record_result(gid) is None


def test_contested_vote_records_side_that_first_hits_threshold(voting_service, pending_match_id):
    """When both sides have votes, the side reaching 3 first is the result.

    Conflicting votes are allowed; whichever side first gathers
    MIN_NON_ADMIN_SUBMISSIONS supporters decides the recorded winner.
    """
    gid = TEST_GUILD_ID
    voting_service.add_record_submission(gid, user_id=100, result="dire", is_admin=False)
    voting_service.add_record_submission(gid, user_id=101, result="radiant", is_admin=False)
    voting_service.add_record_submission(gid, user_id=102, result="radiant", is_admin=False)
    result = voting_service.add_record_submission(gid, user_id=103, result="radiant", is_admin=False)
    assert result["vote_counts"] == {"radiant": 3, "dire": 1}
    assert result["result"] == "radiant"
    assert voting_service.can_record_match(gid) is True


# =============================================================================
# ADMIN OVERRIDE — a single admin vote is decisive
# =============================================================================


def test_single_admin_record_vote_is_decisive(voting_service, pending_match_id):
    """One admin radiant/dire vote records the match with zero non-admin votes.

    Admins override the quorum entirely — the whole point of the admin branch.
    """
    gid = TEST_GUILD_ID
    result = voting_service.add_record_submission(gid, user_id=999, result="dire", is_admin=True)
    assert result["is_ready"] is True
    assert result["result"] == "dire"
    assert result["non_admin_count"] == 0
    assert voting_service.has_admin_submission(gid) is True
    assert voting_service.can_record_match(gid) is True


def test_admin_vote_overrides_contested_non_admin_majority(voting_service, pending_match_id):
    """An admin's pick wins even when more non-admins voted the other way.

    get_pending_record_result checks the admin branch first, so an admin can
    correct a result the crowd got wrong.
    """
    gid = TEST_GUILD_ID
    # Non-admin majority for radiant (3 votes -> would itself be recordable).
    voting_service.add_record_submission(gid, user_id=100, result="radiant", is_admin=False)
    voting_service.add_record_submission(gid, user_id=101, result="radiant", is_admin=False)
    voting_service.add_record_submission(gid, user_id=102, result="radiant", is_admin=False)
    # Admin overrides with dire.
    voting_service.add_record_submission(gid, user_id=999, result="dire", is_admin=True)
    assert voting_service.get_pending_record_result(gid) == "dire"
    assert voting_service.can_record_match(gid) is True


def test_admin_vote_excluded_from_non_admin_tallies(voting_service, pending_match_id):
    """An admin's result vote never counts toward the non-admin tallies.

    If admins leaked into get_vote_counts, two non-admins plus an admin would
    wrongly reach the 3-vote non-admin quorum.
    """
    gid = TEST_GUILD_ID
    voting_service.add_record_submission(gid, user_id=100, result="radiant", is_admin=False)
    voting_service.add_record_submission(gid, user_id=101, result="radiant", is_admin=False)
    voting_service.add_record_submission(gid, user_id=999, result="radiant", is_admin=True)
    assert voting_service.get_vote_counts(gid) == {"radiant": 2, "dire": 0}
    assert voting_service.get_non_admin_submission_count(gid) == 2
    # The admin branch still makes it recordable, but via override, not the tally.
    assert voting_service.can_record_match(gid) is True


# =============================================================================
# ABORT VOTING
# =============================================================================


def test_single_admin_abort_vote_is_decisive(voting_service, pending_match_id):
    """One admin abort vote can abort the match immediately."""
    gid = TEST_GUILD_ID
    result = voting_service.add_abort_submission(gid, user_id=999, is_admin=True)
    assert result["is_ready"] is True
    assert voting_service.has_admin_abort_submission(gid) is True
    assert voting_service.can_abort_match(gid) is True


def test_below_threshold_non_admin_aborts_cannot_abort(voting_service, pending_match_id):
    """Two non-admin abort votes are not enough — the threshold is 3."""
    gid = TEST_GUILD_ID
    voting_service.add_abort_submission(gid, user_id=100, is_admin=False)
    result = voting_service.add_abort_submission(gid, user_id=101, is_admin=False)
    assert result["non_admin_count"] == 2
    assert result["is_ready"] is False
    assert voting_service.can_abort_match(gid) is False


def test_third_non_admin_abort_reaches_threshold(voting_service, pending_match_id):
    """Three non-admin abort votes meet the abort quorum."""
    gid = TEST_GUILD_ID
    voting_service.add_abort_submission(gid, user_id=100, is_admin=False)
    voting_service.add_abort_submission(gid, user_id=101, is_admin=False)
    result = voting_service.add_abort_submission(gid, user_id=102, is_admin=False)
    assert result["non_admin_count"] == 3
    assert result["is_ready"] is True
    assert voting_service.can_abort_match(gid) is True


def test_abort_votes_do_not_count_as_record_votes(voting_service, pending_match_id):
    """Three abort votes never make the match *recordable*.

    Record and abort are distinct outcomes sharing one submissions dict; abort
    votes must not bleed into the record tally and trigger a phantom result.
    """
    gid = TEST_GUILD_ID
    voting_service.add_abort_submission(gid, user_id=100, is_admin=False)
    voting_service.add_abort_submission(gid, user_id=101, is_admin=False)
    voting_service.add_abort_submission(gid, user_id=102, is_admin=False)
    assert voting_service.get_vote_counts(gid) == {"radiant": 0, "dire": 0}
    assert voting_service.can_record_match(gid) is False
    assert voting_service.get_pending_record_result(gid) is None


def test_admin_abort_vote_does_not_make_match_recordable(voting_service, pending_match_id):
    """An admin *abort* vote must not satisfy the record check.

    has_admin_submission only counts radiant/dire admin votes; an admin who
    voted abort should abort the match, never silently record it.
    """
    gid = TEST_GUILD_ID
    voting_service.add_abort_submission(gid, user_id=999, is_admin=True)
    assert voting_service.has_admin_submission(gid) is False
    assert voting_service.can_record_match(gid) is False
    assert voting_service.can_abort_match(gid) is True


def test_duplicate_abort_vote_is_idempotent(voting_service, pending_match_id):
    """Re-casting the same abort vote does not double-count the user."""
    gid = TEST_GUILD_ID
    voting_service.add_abort_submission(gid, user_id=100, is_admin=False)
    result = voting_service.add_abort_submission(gid, user_id=100, is_admin=False)
    assert result["non_admin_count"] == 1
    assert result["total_count"] == 1


# =============================================================================
# PERSISTENCE / MULTI-GUILD ISOLATION
# =============================================================================


def test_votes_persist_across_service_instances(match_repository, pending_match_id):
    """Votes survive being read back by a fresh service (they hit the DB).

    Voting and recording happen in separate command invocations; a vote cast
    in one must still count when a later command builds new service objects.
    """
    gid = TEST_GUILD_ID
    first = MatchVotingService(MatchStateService(match_repository))
    first.add_record_submission(gid, user_id=100, result="radiant", is_admin=False)
    first.add_record_submission(gid, user_id=101, result="radiant", is_admin=False)
    first.add_record_submission(gid, user_id=102, result="radiant", is_admin=False)

    # Brand-new service + state service, same repo/DB — no shared in-memory cache.
    reloaded = MatchVotingService(MatchStateService(match_repository))
    assert reloaded.get_vote_counts(gid) == {"radiant": 3, "dire": 0}
    assert reloaded.can_record_match(gid) is True
    assert reloaded.get_pending_record_result(gid) == "radiant"


def test_votes_isolated_between_concurrent_matches(voting_service, state_service, pending_match_id):
    """Votes on one pending match must not affect another match in the guild.

    Concurrent matches each carry their own submissions; a vote on match A
    must not make match B recordable.
    """
    from domain.models.pending_match_state import PendingMatchState

    gid = TEST_GUILD_ID
    match_a = pending_match_id
    match_b = state_service.persist_state(
        gid, PendingMatchState(radiant_team_ids=[11, 12], dire_team_ids=[13, 14])
    )
    assert match_a != match_b

    # An admin records match A only.
    voting_service.add_record_submission(gid, user_id=999, result="radiant", is_admin=True, pending_match_id=match_a)

    assert voting_service.can_record_match(gid, match_a) is True
    assert voting_service.can_record_match(gid, match_b) is False
    assert voting_service.get_vote_counts(gid, match_b) == {"radiant": 0, "dire": 0}
