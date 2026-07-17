import sqlite3
from concurrent.futures import ThreadPoolExecutor

import pytest

from domain.models.duel import DuelChallenge, DuelDueKind, DuelStatus, DuelTrial
from repositories.duel_challenge_repository import DuelChallengeRepository
from repositories.player_repository import PlayerRepository

GUILD_ID = 9001
NOW = 1_000_000
DAY = 86400


def seed_player(repo_db_path, discord_id, rating, balance, *, guild_id=GUILD_ID):
    players = PlayerRepository(repo_db_path)
    players.add(
        discord_id=discord_id,
        discord_username=f"Player {discord_id}",
        guild_id=guild_id,
        glicko_rating=rating,
        glicko_rd=80.0 if rating is not None else None,
        glicko_volatility=0.06 if rating is not None else None,
    )
    players.update_balance(discord_id, guild_id, balance)
    return players


def create_challenge(repo, challenger_id=1, recipient_id=2, *, guild_id=GUILD_ID, now=NOW):
    return repo.create_challenge_atomic(
        guild_id,
        77,
        challenger_id,
        recipient_id,
        500,
        now,
        30 * DAY,
        7 * DAY,
        7 * DAY,
        challenger_id,
    )


@pytest.fixture
def duel_fixture(repo_db_path):
    def make(*, wager=500, recipient_balance=0):
        players = seed_player(repo_db_path, 1, 1400.0, wager)
        seed_player(repo_db_path, 2, 1500.0, recipient_balance)
        repo = DuelChallengeRepository(repo_db_path)
        challenge = repo.create_challenge_atomic(
            GUILD_ID,
            77,
            1,
            2,
            wager,
            NOW,
            30 * DAY,
            7 * DAY,
            7 * DAY,
            1,
        )
        # Keep a constant 500-coin pre-escrow baseline for the exact odd-wager
        # accounting assertion below, including its existing one-coin debt.
        players.update_balance(1, GUILD_ID, 500 - wager)
        with sqlite3.connect(repo_db_path) as conn:
            conn.execute("DELETE FROM economy_ledger_entries")
        return repo, players, challenge

    return make


def ledger_rows(db_path, challenge_id):
    with sqlite3.connect(db_path) as conn:
        conn.row_factory = sqlite3.Row
        rows = conn.execute(
            """
            SELECT account_id, delta, source, actor_id, related_type,
                   related_id, reason
            FROM economy_ledger_entries
            WHERE related_type = 'duel_challenge' AND related_id = ?
            ORDER BY ledger_id
            """,
            (str(challenge_id),),
        ).fetchall()
    return [dict(row) for row in rows]


def test_duel_model_reads_sqlite_row(repo_db_path):
    conn = sqlite3.connect(repo_db_path)
    conn.row_factory = sqlite3.Row
    row = conn.execute(
        """
        SELECT 7 AS challenge_id, 42 AS guild_id, 9 AS channel_id,
               NULL AS message_id, 1 AS challenger_id, 2 AS recipient_id,
               501 AS wager, 'pending' AS status, NULL AS trial_type,
               1400.0 AS challenger_glicko, 80.0 AS challenger_rd,
               1500.0 AS recipient_glicko, 70.0 AS recipient_rd,
               100 AS created_at, 200 AS expires_at, 120 AS next_reminder_at,
               NULL AS responded_at, NULL AS resolved_at,
               NULL AS winner_id, NULL AS resolution_actor_id
        """
    ).fetchone()
    challenge = DuelChallenge.from_row(row)
    assert challenge.status is DuelStatus.PENDING
    assert challenge.trial_type is None
    assert challenge.decline_penalty == 251


def test_duel_schema_enforces_wager_and_status(repo_db_path):
    conn = sqlite3.connect(repo_db_path)
    columns = {row[1] for row in conn.execute("PRAGMA table_info(duel_challenges)")}
    assert {"challenge_id", "guild_id", "message_id", "next_reminder_at"} <= columns
    indexes = {row[1] for row in conn.execute("PRAGMA index_list(duel_challenges)")}
    assert {
        "idx_duel_guild_status",
        "idx_duel_challenger_history",
        "idx_duel_recipient_history",
        "idx_duel_due_expiry",
        "idx_duel_due_reminder",
    } <= indexes

    values = (42, 9, 1, 2, 499, "pending", 1400.0, 80.0, 1500.0, 70.0, 100, 200)
    with pytest.raises(sqlite3.IntegrityError):
        conn.execute(
            """
            INSERT INTO duel_challenges (
                guild_id, channel_id, challenger_id, recipient_id, wager, status,
                challenger_glicko, challenger_rd, recipient_glicko, recipient_rd,
                created_at, expires_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            values,
        )


def test_create_challenge_escrows_and_snapshots(repo_db_path):
    players = seed_player(repo_db_path, 1, 1400.0, 700)
    seed_player(repo_db_path, 2, 1500.0, 0)
    repo = DuelChallengeRepository(repo_db_path)

    challenge = repo.create_challenge_atomic(
        guild_id=GUILD_ID,
        channel_id=77,
        challenger_id=1,
        recipient_id=2,
        wager=501,
        now=NOW,
        challenger_cooldown_seconds=30 * DAY,
        recipient_cooldown_seconds=7 * DAY,
        response_seconds=7 * DAY,
        actor_id=1,
    )

    assert challenge.challenger_glicko == 1400.0
    assert challenge.challenger_rd == 80.0
    assert challenge.recipient_glicko == 1500.0
    assert challenge.recipient_rd == 80.0
    assert challenge.expires_at == NOW + 7 * DAY
    assert challenge.next_reminder_at == NOW + DAY
    assert players.get_balance(1, GUILD_ID) == 199


@pytest.mark.parametrize(
    ("recipient_rating", "wager", "balance", "message"),
    [
        (1300.0, 500, 500, "punching down"),
        (1500.0, 499, 500, "500"),
        (1500.0, 1001, 1001, "1000"),
        (1500.0, 500.5, 1000, "whole"),
        (1500.0, 500, 499, "balance"),
    ],
)
def test_create_rejects_invalid_challenges(
    repo_db_path, recipient_rating, wager, balance, message
):
    players = seed_player(repo_db_path, 1, 1400.0, balance)
    seed_player(repo_db_path, 2, recipient_rating, 0)
    repo = DuelChallengeRepository(repo_db_path)

    with pytest.raises(ValueError, match=message):
        repo.create_challenge_atomic(
            GUILD_ID,
            77,
            1,
            2,
            wager,
            NOW,
            30 * DAY,
            7 * DAY,
            7 * DAY,
            1,
        )

    assert players.get_balance(1, GUILD_ID) == balance


def test_create_rejects_self_challenge(repo_db_path):
    seed_player(repo_db_path, 1, 1400.0, 500)
    repo = DuelChallengeRepository(repo_db_path)

    with pytest.raises(ValueError, match="yourself"):
        create_challenge(repo, recipient_id=1)


@pytest.mark.parametrize(
    ("missing_id", "unrated_id", "message"),
    [(1, None, "registered"), (2, None, "registered"), (None, 1, "rated"), (None, 2, "rated")],
)
def test_create_rejects_unregistered_or_unrated_players(
    repo_db_path, missing_id, unrated_id, message
):
    seed_player(repo_db_path, 1, 1400.0, 500)
    seed_player(repo_db_path, 2, 1500.0, 500)
    with sqlite3.connect(repo_db_path) as conn:
        if missing_id is not None:
            conn.execute(
                "DELETE FROM players WHERE guild_id = ? AND discord_id = ?",
                (GUILD_ID, missing_id),
            )
        else:
            conn.execute(
                "UPDATE players SET glicko_rating = NULL, glicko_rd = NULL "
                "WHERE guild_id = ? AND discord_id = ?",
                (GUILD_ID, unrated_id),
            )

    with pytest.raises(ValueError, match=message):
        create_challenge(DuelChallengeRepository(repo_db_path))


def test_create_allows_equal_ratings(repo_db_path):
    seed_player(repo_db_path, 1, 1400.0, 500)
    seed_player(repo_db_path, 2, 1400.0, 0)

    challenge = create_challenge(DuelChallengeRepository(repo_db_path))

    assert challenge.status is DuelStatus.PENDING


def test_challenger_cooldown_allows_exact_boundary(repo_db_path):
    seed_player(repo_db_path, 1, 1400.0, 1500)
    seed_player(repo_db_path, 2, 1500.0, 0)
    repo = DuelChallengeRepository(repo_db_path)
    first = create_challenge(repo)
    with sqlite3.connect(repo_db_path) as conn:
        conn.execute(
            "UPDATE duel_challenges SET status = 'declined', resolved_at = ? "
            "WHERE challenge_id = ?",
            (NOW + 1, first.challenge_id),
        )

    with pytest.raises(ValueError, match="cooldown"):
        create_challenge(repo, now=NOW + 30 * DAY - 1)

    second = create_challenge(repo, now=NOW + 30 * DAY)
    assert second.status is DuelStatus.PENDING


def test_recipient_cooldown_allows_exact_boundary(repo_db_path):
    seed_player(repo_db_path, 1, 1400.0, 500)
    seed_player(repo_db_path, 2, 1600.0, 0)
    seed_player(repo_db_path, 3, 1500.0, 1000)
    repo = DuelChallengeRepository(repo_db_path)
    first = create_challenge(repo)
    with sqlite3.connect(repo_db_path) as conn:
        conn.execute(
            "UPDATE duel_challenges SET status = 'declined', resolved_at = ? "
            "WHERE challenge_id = ?",
            (NOW + 1, first.challenge_id),
        )

    with pytest.raises(ValueError, match="recently challenged"):
        create_challenge(repo, challenger_id=3, now=NOW + 7 * DAY - 1)

    second = create_challenge(repo, challenger_id=3, now=NOW + 7 * DAY)
    assert second.status is DuelStatus.PENDING


def test_unresolved_participation_blocks_either_role(repo_db_path):
    seed_player(repo_db_path, 1, 1400.0, 500)
    seed_player(repo_db_path, 2, 1500.0, 500)
    seed_player(repo_db_path, 3, 1600.0, 0)
    seed_player(repo_db_path, 4, 1300.0, 500)
    repo = DuelChallengeRepository(repo_db_path)
    create_challenge(repo)

    with pytest.raises(ValueError, match="already involved"):
        create_challenge(repo, challenger_id=2, recipient_id=3)
    with pytest.raises(ValueError, match="already involved"):
        create_challenge(repo, challenger_id=4, recipient_id=1)


def test_duel_creation_and_reads_are_guild_isolated(repo_db_path):
    other_guild = GUILD_ID + 1
    for guild_id in (GUILD_ID, other_guild):
        seed_player(repo_db_path, 1, 1400.0, 500, guild_id=guild_id)
        seed_player(repo_db_path, 2, 1500.0, 0, guild_id=guild_id)
    repo = DuelChallengeRepository(repo_db_path)

    first = create_challenge(repo)
    second = create_challenge(repo, guild_id=other_guild)

    assert repo.get_challenge(first.challenge_id, other_guild) is None
    assert [row.challenge_id for row in repo.list_outstanding(GUILD_ID)] == [
        first.challenge_id
    ]
    assert [row.challenge_id for row in repo.list_outstanding(other_guild)] == [
        second.challenge_id
    ]


def test_message_binding_is_guarded(repo_db_path):
    seed_player(repo_db_path, 1, 1400.0, 500)
    seed_player(repo_db_path, 2, 1500.0, 0)
    repo = DuelChallengeRepository(repo_db_path)
    challenge = create_challenge(repo)

    bound = repo.bind_message(challenge.challenge_id, GUILD_ID, 1234)

    assert bound.message_id == 1234
    with pytest.raises(ValueError, match="message"):
        repo.bind_message(challenge.challenge_id, GUILD_ID, 5678)
    with pytest.raises(ValueError, match="message"):
        repo.bind_message(challenge.challenge_id, GUILD_ID + 1, 5678)


def test_outstanding_reads_are_ordered_and_status_filtered(repo_db_path):
    for discord_id, rating, balance in (
        (1, 1400.0, 500),
        (2, 1500.0, 0),
        (3, 1450.0, 500),
        (4, 1550.0, 0),
    ):
        seed_player(repo_db_path, discord_id, rating, balance)
    repo = DuelChallengeRepository(repo_db_path)
    older = create_challenge(repo)
    newer = create_challenge(repo, challenger_id=3, recipient_id=4, now=NOW + 10)
    with sqlite3.connect(repo_db_path) as conn:
        conn.execute(
            "UPDATE duel_challenges SET status = 'accepted' WHERE challenge_id = ?",
            (older.challenge_id,),
        )

    assert [row.challenge_id for row in repo.list_outstanding(GUILD_ID)] == [
        newer.challenge_id,
        older.challenge_id,
    ]
    assert [row.challenge_id for row in repo.list_pending_all()] == [newer.challenge_id]
    assert repo.get_pending_for_recipient(4, GUILD_ID).challenge_id == newer.challenge_id


def test_delivery_failure_refunds_and_does_not_block_retry(repo_db_path):
    players = seed_player(repo_db_path, 1, 1400.0, 500)
    seed_player(repo_db_path, 2, 1500.0, 0)
    repo = DuelChallengeRepository(repo_db_path)
    first = create_challenge(repo)

    failed = repo.mark_delivery_failed_atomic(first.challenge_id, GUILD_ID, NOW + 1, 1)

    assert failed.status is DuelStatus.DELIVERY_FAILED
    assert failed.next_reminder_at is None
    assert players.get_balance(1, GUILD_ID) == 500
    retry = create_challenge(repo, now=NOW + 2)
    assert retry.status is DuelStatus.PENDING


def test_decline_uses_ceiling_half_and_allows_debt(duel_fixture):
    repo, players, challenge = duel_fixture(wager=501, recipient_balance=0)
    declined = repo.decline_atomic(challenge.challenge_id, GUILD_ID, 2, 1_000_100, 2)
    assert declined.status is DuelStatus.DECLINED
    assert players.get_balance(1, GUILD_ID) == 751
    assert players.get_balance(2, GUILD_ID) == -251
    assert declined.next_reminder_at is None


def test_expiry_uses_ceiling_half_and_allows_debt(duel_fixture):
    repo, players, challenge = duel_fixture(wager=501, recipient_balance=0)
    expired = repo.expire_atomic(challenge.challenge_id, GUILD_ID, challenge.expires_at)
    assert expired.status is DuelStatus.EXPIRED
    assert players.get_balance(1, GUILD_ID) == 751
    assert players.get_balance(2, GUILD_ID) == -251
    assert expired.next_reminder_at is None


def test_accept_then_winner_receives_double_pot(duel_fixture):
    repo, players, challenge = duel_fixture(wager=500, recipient_balance=0)
    accepted = repo.accept_atomic(
        challenge.challenge_id,
        GUILD_ID,
        2,
        DuelTrial.TRIAL_BY_COMBAT,
        1_000_100,
        2,
    )
    assert players.get_balance(2, GUILD_ID) == -500
    resolved = repo.resolve_atomic(
        accepted.challenge_id, GUILD_ID, winner_id=2, now=1_000_200, actor_id=99
    )
    assert resolved.status is DuelStatus.RESOLVED
    assert players.get_balance(2, GUILD_ID) == 500


def test_void_refunds_both_stakes(duel_fixture):
    repo, players, challenge = duel_fixture(wager=500, recipient_balance=0)
    accepted = repo.accept_atomic(
        challenge.challenge_id,
        GUILD_ID,
        2,
        DuelTrial.TRIAL_OF_FIVE,
        1_000_100,
        2,
    )
    voided = repo.resolve_atomic(
        accepted.challenge_id,
        GUILD_ID,
        winner_id=None,
        now=1_000_200,
        actor_id=99,
    )
    assert voided.status is DuelStatus.VOIDED
    assert players.get_balance(1, GUILD_ID) == 500
    assert players.get_balance(2, GUILD_ID) == 0


def test_accept_rejects_wrong_recipient(duel_fixture):
    repo, players, challenge = duel_fixture()

    with pytest.raises(ValueError, match="recipient"):
        repo.accept_atomic(
            challenge.challenge_id,
            GUILD_ID,
            3,
            DuelTrial.TRIAL_BY_COMBAT,
            NOW + 100,
            3,
        )

    assert players.get_balance(2, GUILD_ID) == 0
    assert repo.get_challenge(challenge.challenge_id, GUILD_ID).status is DuelStatus.PENDING


def test_expiry_rejects_before_deadline(duel_fixture):
    repo, players, challenge = duel_fixture()

    with pytest.raises(ValueError, match="expired"):
        repo.expire_atomic(challenge.challenge_id, GUILD_ID, challenge.expires_at - 1)

    assert players.get_balance(1, GUILD_ID) == 0
    assert players.get_balance(2, GUILD_ID) == 0


def test_resolution_rejects_invalid_winner_and_cross_guild(duel_fixture):
    repo, players, challenge = duel_fixture()
    accepted = repo.accept_atomic(
        challenge.challenge_id,
        GUILD_ID,
        2,
        DuelTrial.TRIAL_BY_COMBAT,
        NOW + 100,
        2,
    )

    with pytest.raises(ValueError, match="winner"):
        repo.resolve_atomic(accepted.challenge_id, GUILD_ID, 3, NOW + 200, 99)
    with pytest.raises(ValueError, match="accepted"):
        repo.resolve_atomic(accepted.challenge_id, GUILD_ID + 1, 1, NOW + 200, 99)

    assert players.get_balance(1, GUILD_ID) == 0
    assert players.get_balance(2, GUILD_ID) == -500


def test_repeated_transitions_cannot_double_pay(duel_fixture):
    repo, players, challenge = duel_fixture()
    declined = repo.decline_atomic(challenge.challenge_id, GUILD_ID, 2, NOW + 100, 2)

    with pytest.raises(ValueError, match="pending"):
        repo.decline_atomic(challenge.challenge_id, GUILD_ID, 2, NOW + 101, 2)
    with pytest.raises(ValueError, match="pending"):
        repo.accept_atomic(
            challenge.challenge_id,
            GUILD_ID,
            2,
            DuelTrial.TRIAL_OF_FIVE,
            NOW + 101,
            2,
        )

    assert declined.status is DuelStatus.DECLINED
    assert players.get_balance(1, GUILD_ID) == 750
    assert players.get_balance(2, GUILD_ID) == -250


def test_accept_and_decline_race_has_exactly_one_success(duel_fixture):
    repo, players, challenge = duel_fixture()

    def accept():
        return DuelChallengeRepository(repo.db_path).accept_atomic(
            challenge.challenge_id,
            GUILD_ID,
            2,
            DuelTrial.TRIAL_BY_COMBAT,
            NOW + 100,
            2,
        )

    def decline():
        return DuelChallengeRepository(repo.db_path).decline_atomic(
            challenge.challenge_id, GUILD_ID, 2, NOW + 100, 2
        )

    with ThreadPoolExecutor(max_workers=2) as executor:
        futures = [executor.submit(accept), executor.submit(decline)]
    outcomes = []
    for future in futures:
        try:
            outcomes.append(future.result())
        except ValueError:
            pass

    assert len(outcomes) == 1
    assert outcomes[0].status in {DuelStatus.ACCEPTED, DuelStatus.DECLINED}
    balances = (
        players.get_balance(1, GUILD_ID),
        players.get_balance(2, GUILD_ID),
    )
    assert balances in {(0, -500), (750, -250)}


def test_decline_ledger_context_has_actor_challenge_and_distinct_reasons(
    duel_fixture, repo_db_path
):
    repo, _, challenge = duel_fixture()

    repo.decline_atomic(challenge.challenge_id, GUILD_ID, 2, NOW + 100, 2)

    rows = ledger_rows(repo_db_path, challenge.challenge_id)
    assert [(row["account_id"], row["delta"]) for row in rows] == [
        (1, 500),
        (1, 250),
        (2, -250),
    ]
    assert {row["source"] for row in rows} == {"duel_challenge"}
    assert {row["actor_id"] for row in rows} == {2}
    assert {row["related_type"] for row in rows} == {"duel_challenge"}
    assert {row["related_id"] for row in rows} == {str(challenge.challenge_id)}
    assert len({row["reason"] for row in rows}) == 3


def test_due_expiry_takes_precedence_over_reminder(duel_fixture):
    repo, _, challenge = duel_fixture()

    due = repo.get_due_challenge_ids(challenge.expires_at)

    assert due == [challenge.challenge_id]
    assert (
        repo.claim_reminder_atomic(
            challenge.challenge_id, GUILD_ID, challenge.expires_at
        )
        is None
    )
    assert (
        repo.expire_atomic(challenge.challenge_id, GUILD_ID, challenge.expires_at).status
        is DuelStatus.EXPIRED
    )


def test_reminder_claim_catches_up_to_next_daily_boundary(duel_fixture):
    repo, _, challenge = duel_fixture()
    now = challenge.created_at + 3 * DAY

    claimed = repo.claim_reminder_atomic(challenge.challenge_id, GUILD_ID, now)

    assert claimed.kind is DuelDueKind.REMINDER
    assert claimed.remaining_seconds == challenge.expires_at - now
    assert claimed.challenge.next_reminder_at == challenge.created_at + 4 * DAY
    assert not claimed.ping_recipient


def test_only_one_concurrent_reminder_claim_succeeds(duel_fixture):
    repo, _, challenge = duel_fixture()
    now = challenge.created_at + DAY

    def claim():
        return DuelChallengeRepository(repo.db_path).claim_reminder_atomic(
            challenge.challenge_id, GUILD_ID, now
        )

    with ThreadPoolExecutor(max_workers=2) as executor:
        results = list(executor.map(lambda _: claim(), range(2)))

    assert sum(result is not None for result in results) == 1


def test_reminder_pings_recipient_in_final_48_hours(duel_fixture):
    repo, _, challenge = duel_fixture()
    now = challenge.expires_at - 48 * 3600

    claimed = repo.claim_reminder_atomic(challenge.challenge_id, GUILD_ID, now)

    assert claimed is not None
    assert claimed.remaining_seconds == 48 * 3600
    assert claimed.ping_recipient
