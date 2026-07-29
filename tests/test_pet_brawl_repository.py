"""Integration tests for PetBrawlRepository against a real schema DB."""

from __future__ import annotations

import sqlite3

import pytest

from repositories.pet_brawl_repository import PetBrawlRepository
from repositories.pet_repository import PetRepository
from repositories.player_repository import PlayerRepository
from tests.conftest import TEST_GUILD_ID

NOW = 1_800_000_000
DAY = 86400
DECAY = 20


@pytest.fixture
def brawl_repo(repo_db_path):
    return PetBrawlRepository(repo_db_path)


@pytest.fixture
def pet_repo(repo_db_path):
    return PetRepository(repo_db_path)


def seed_player(repo_db_path, discord_id, balance):
    repo = PlayerRepository(repo_db_path)
    repo.add(
        discord_id=discord_id,
        discord_username=f"Player {discord_id}",
        guild_id=TEST_GUILD_ID,
    )
    repo.update_balance(discord_id, TEST_GUILD_ID, balance)
    return repo


@pytest.fixture
def insert_pet(repo_db_path):
    def _insert(
        discord_id,
        *,
        hunger=100,
        last_fed_at=NOW,
        died_at=None,
        training_xp=0,
        training_str=0,
        training_int=0,
        training_dex=0,
    ):
        with sqlite3.connect(repo_db_path) as conn:
            cursor = conn.execute(
                "INSERT INTO pets (discord_id, guild_id, name, species, "
                "adopted_at, hatched_at, adopt_fee, last_fed_at, "
                "hunger_at_last_fed, died_at, training_xp, training_str, "
                "training_int, training_dex) "
                "VALUES (?, ?, 'Brawler', 'common_cama', ?, ?, 20, ?, ?, ?, ?, ?, ?, ?)",
                (
                    discord_id,
                    TEST_GUILD_ID,
                    NOW - 10 * DAY,
                    NOW - 9 * DAY,
                    last_fed_at,
                    hunger,
                    died_at,
                    training_xp,
                    training_str,
                    training_int,
                    training_dex,
                ),
            )
            return cursor.lastrowid

    return _insert


def make_pending(
    brawl_repo,
    insert_pet,
    challenger=100,
    recipient=200,
    *,
    wager=0,
    fee=0,
):
    pet_a = insert_pet(challenger)
    pet_b = insert_pet(recipient)
    brawl = brawl_repo.create_brawl_atomic(
        TEST_GUILD_ID, 555, challenger, recipient, pet_a,
        now=NOW, expires_at=NOW + 180, wager=wager, fee=fee,
    )
    return brawl, pet_a, pet_b


def make_active(brawl_repo, insert_pet, challenger=100, recipient=200):
    brawl, pet_a, pet_b = make_pending(brawl_repo, insert_pet, challenger, recipient)
    brawl = brawl_repo.accept_atomic(
        brawl.brawl_id, TEST_GUILD_ID, recipient, pet_b, NOW + 5
    )
    return brawl, pet_a, pet_b


def settle(brawl_repo, brawl, winner_pet, loser_pet, winner_id, *, now=NOW + 60, **kw):
    defaults = {
        "rounds": 4,
        "now": now,
        "decay_per_day": DECAY,
        "winner_gain": 10,
        "loser_loss": 15,
        "loss_floor": 10,
        "daily_win_cap": 3,
    }
    defaults.update(kw)
    return brawl_repo.settle_brawl_atomic(
        brawl.brawl_id,
        TEST_GUILD_ID,
        winner_id=winner_id,
        winner_pet_id=winner_pet,
        loser_pet_id=loser_pet,
        **defaults,
    )


class TestLifecycle:
    def test_create_and_read(self, brawl_repo, insert_pet):
        brawl, pet_a, _ = make_pending(brawl_repo, insert_pet)
        assert brawl.status == "pending"
        assert brawl.challenger_pet_id == pet_a
        assert brawl.recipient_pet_id is None
        assert brawl_repo.get_brawl(brawl.brawl_id, TEST_GUILD_ID) == brawl

    def test_one_open_brawl_per_player_cross_role(self, brawl_repo, insert_pet):
        make_pending(brawl_repo, insert_pet, challenger=100, recipient=200)
        pet_c = insert_pet(300)
        # 100 is a challenger in an open brawl -> blocked as recipient too.
        with pytest.raises(ValueError, match="brawl_busy"):
            brawl_repo.create_brawl_atomic(
                TEST_GUILD_ID, 555, 300, 100, pet_c, now=NOW, expires_at=NOW + 180
            )
        # 200 is a recipient in an open brawl -> blocked as challenger.
        with pytest.raises(ValueError, match="brawl_busy"):
            brawl_repo.create_brawl_atomic(
                TEST_GUILD_ID, 555, 200, 300, pet_c, now=NOW, expires_at=NOW + 180
            )
        # Unrelated players are fine.
        pet_d = insert_pet(400)
        brawl_repo.create_brawl_atomic(
            TEST_GUILD_ID, 555, 300, 400, pet_c, now=NOW, expires_at=NOW + 180
        )
        assert pet_d  # both pets exist independently

    def test_closed_brawls_do_not_block(self, brawl_repo, insert_pet):
        brawl, pet_a, pet_b = make_active(brawl_repo, insert_pet)
        settle(brawl_repo, brawl, pet_a, pet_b, 100)
        again = brawl_repo.create_brawl_atomic(
            TEST_GUILD_ID, 555, 100, 200, pet_a, now=NOW + 100, expires_at=NOW + 280
        )
        assert again.status == "pending"

    def test_accept_binds_recipient_pet(self, brawl_repo, insert_pet):
        brawl, _, pet_b = make_pending(brawl_repo, insert_pet)
        accepted = brawl_repo.accept_atomic(
            brawl.brawl_id, TEST_GUILD_ID, 200, pet_b, NOW + 5
        )
        assert accepted.status == "active"
        assert accepted.recipient_pet_id == pet_b

    def test_accept_guards(self, brawl_repo, insert_pet):
        brawl, _, pet_b = make_pending(brawl_repo, insert_pet)
        with pytest.raises(ValueError, match="not_recipient"):
            brawl_repo.accept_atomic(brawl.brawl_id, TEST_GUILD_ID, 999, pet_b, NOW)
        with pytest.raises(ValueError, match="expired"):
            brawl_repo.accept_atomic(
                brawl.brawl_id, TEST_GUILD_ID, 200, pet_b, NOW + 999
            )
        with pytest.raises(ValueError, match="no_brawl"):
            brawl_repo.accept_atomic(12345, TEST_GUILD_ID, 200, pet_b, NOW)
        brawl_repo.accept_atomic(brawl.brawl_id, TEST_GUILD_ID, 200, pet_b, NOW + 5)
        with pytest.raises(ValueError, match="not_pending"):
            brawl_repo.accept_atomic(
                brawl.brawl_id, TEST_GUILD_ID, 200, pet_b, NOW + 6
            )

    def test_decline(self, brawl_repo, insert_pet):
        brawl, _, _ = make_pending(brawl_repo, insert_pet)
        brawl_repo.decline_atomic(brawl.brawl_id, TEST_GUILD_ID, 200, NOW + 5)
        assert brawl_repo.get_brawl(brawl.brawl_id, TEST_GUILD_ID).status == "declined"
        with pytest.raises(ValueError, match="not_pending"):
            brawl_repo.decline_atomic(brawl.brawl_id, TEST_GUILD_ID, 200, NOW + 6)

    def test_void_open_states_only(self, brawl_repo, insert_pet):
        brawl, _, _ = make_pending(brawl_repo, insert_pet)
        brawl_repo.void_atomic(brawl.brawl_id, TEST_GUILD_ID, NOW + 5)
        assert brawl_repo.get_brawl(brawl.brawl_id, TEST_GUILD_ID).status == "void"
        with pytest.raises(ValueError, match="not_open"):
            brawl_repo.void_atomic(brawl.brawl_id, TEST_GUILD_ID, NOW + 6)

    def test_withdraw_atomic_pending_only(self, brawl_repo, insert_pet):
        brawl, _, _ = make_pending(brawl_repo, insert_pet)
        with pytest.raises(ValueError, match="not_challenger"):
            brawl_repo.withdraw_atomic(brawl.brawl_id, TEST_GUILD_ID, 200, NOW + 4)
        with pytest.raises(ValueError, match="no_brawl"):
            brawl_repo.withdraw_atomic(12345, TEST_GUILD_ID, 100, NOW + 4)
        brawl_repo.withdraw_atomic(brawl.brawl_id, TEST_GUILD_ID, 100, NOW + 5)
        assert brawl_repo.get_brawl(brawl.brawl_id, TEST_GUILD_ID).status == "void"
        # An accepted (active) brawl can no longer be withdrawn.
        active, _, _ = make_active(brawl_repo, insert_pet, 300, 400)
        with pytest.raises(ValueError, match="not_pending"):
            brawl_repo.withdraw_atomic(active.brawl_id, TEST_GUILD_ID, 300, NOW + 6)
        assert brawl_repo.get_brawl(active.brawl_id, TEST_GUILD_ID).status == "active"

    def test_sweep_stale(self, brawl_repo, insert_pet):
        pending, _, _ = make_pending(brawl_repo, insert_pet, 100, 200)
        active, _, _ = make_active(brawl_repo, insert_pet, 300, 400)
        counts = brawl_repo.sweep_stale(NOW + 10_000, active_ttl_seconds=1800)
        assert counts == {"expired": 1, "voided": 1}
        assert brawl_repo.get_brawl(pending.brawl_id, TEST_GUILD_ID).status == "expired"
        assert brawl_repo.get_brawl(active.brawl_id, TEST_GUILD_ID).status == "void"
        # Fresh rows untouched.
        fresh, _, _ = make_pending(brawl_repo, insert_pet, 500, 600)
        counts = brawl_repo.sweep_stale(NOW + 20, active_ttl_seconds=1800)
        assert counts == {"expired": 0, "voided": 0}
        assert brawl_repo.get_brawl(fresh.brawl_id, TEST_GUILD_ID).status == "pending"


class TestWagerEconomy:
    def balances(self, player_repo):
        return (
            player_repo.get_balance(100, TEST_GUILD_ID),
            player_repo.get_balance(200, TEST_GUILD_ID),
        )

    def test_wager_escrows_both_sides_and_settlement_pays_pot_and_reserve(
        self, repo_db_path, brawl_repo, insert_pet
    ):
        players = seed_player(repo_db_path, 100, 100)
        seed_player(repo_db_path, 200, 100)

        brawl, pet_a, pet_b = make_pending(
            brawl_repo, insert_pet, wager=20, fee=1
        )
        assert (brawl.wager, brawl.fee) == (20, 1)
        assert self.balances(players) == (79, 100)

        brawl = brawl_repo.accept_atomic(
            brawl.brawl_id, TEST_GUILD_ID, 200, pet_b, NOW + 5
        )
        assert self.balances(players) == (79, 80)

        result = settle(
            brawl_repo,
            brawl,
            pet_a,
            pet_b,
            100,
            winner_stat_gain="str",
            loser_stat_gain="int",
        )

        assert result["payout"] == 40
        assert self.balances(players) == (119, 80)
        assert PetRepository(repo_db_path).get_nonprofit_balance(TEST_GUILD_ID) == 1

        with sqlite3.connect(repo_db_path) as conn:
            conn.row_factory = sqlite3.Row
            ledger = conn.execute(
                "SELECT delta, source, related_type, related_id, reason "
                "FROM economy_ledger_entries WHERE related_type = 'pet_brawl' "
                "ORDER BY ledger_id"
            ).fetchall()
        assert [
            (row["delta"], row["source"], row["related_type"], row["reason"])
            for row in ledger
        ] == [
            (-21, "pet_brawl", "pet_brawl", "challenger_escrow"),
            (-20, "pet_brawl", "pet_brawl", "recipient_escrow"),
            (40, "pet_brawl", "pet_brawl", "winner_payout"),
            (1, "pet_brawl", "pet_brawl", "brawl_fee_reserve_credit"),
        ]
        assert {row["related_id"] for row in ledger} == {str(brawl.brawl_id)}

    def test_pending_decline_refunds_challenger_wager_and_fee(
        self, repo_db_path, brawl_repo, insert_pet
    ):
        players = seed_player(repo_db_path, 100, 100)
        seed_player(repo_db_path, 200, 100)
        brawl, _, _ = make_pending(brawl_repo, insert_pet, wager=20, fee=1)

        brawl_repo.decline_atomic(brawl.brawl_id, TEST_GUILD_ID, 200, NOW + 5)

        assert self.balances(players) == (100, 100)
        assert PetRepository(repo_db_path).get_nonprofit_balance(TEST_GUILD_ID) == 0

    def test_active_void_refunds_both_stakes_and_fee(
        self, repo_db_path, brawl_repo, insert_pet
    ):
        players = seed_player(repo_db_path, 100, 100)
        seed_player(repo_db_path, 200, 100)
        brawl, _, pet_b = make_pending(brawl_repo, insert_pet, wager=20, fee=1)
        brawl_repo.accept_atomic(
            brawl.brawl_id, TEST_GUILD_ID, 200, pet_b, NOW + 5
        )

        brawl_repo.void_atomic(brawl.brawl_id, TEST_GUILD_ID, NOW + 6)

        assert self.balances(players) == (100, 100)
        assert PetRepository(repo_db_path).get_nonprofit_balance(TEST_GUILD_ID) == 0

    def test_accept_insufficient_funds_keeps_pending_escrow_refundable(
        self, repo_db_path, brawl_repo, insert_pet
    ):
        players = seed_player(repo_db_path, 100, 100)
        seed_player(repo_db_path, 200, 20)
        brawl, _, pet_b = make_pending(brawl_repo, insert_pet, wager=20, fee=1)
        players.update_balance(200, TEST_GUILD_ID, 0)

        with pytest.raises(ValueError, match="recipient_funds"):
            brawl_repo.accept_atomic(
                brawl.brawl_id, TEST_GUILD_ID, 200, pet_b, NOW + 5
            )

        assert brawl_repo.get_brawl(brawl.brawl_id, TEST_GUILD_ID).status == "pending"
        brawl_repo.withdraw_atomic(brawl.brawl_id, TEST_GUILD_ID, 100, NOW + 6)
        assert self.balances(players) == (100, 0)

    def test_stale_sweep_refunds_pending_and_active_wagers(
        self, repo_db_path, brawl_repo, insert_pet
    ):
        players = seed_player(repo_db_path, 100, 100)
        for player_id in (200, 300, 400):
            seed_player(repo_db_path, player_id, 100)
        pending, _, _ = make_pending(
            brawl_repo, insert_pet, 100, 200, wager=20, fee=1
        )
        active, _, active_pet = make_pending(
            brawl_repo, insert_pet, 300, 400, wager=20, fee=1
        )
        brawl_repo.accept_atomic(
            active.brawl_id, TEST_GUILD_ID, 400, active_pet, NOW + 1
        )

        counts = brawl_repo.sweep_stale(
            NOW + 10_000, active_ttl_seconds=1_800
        )

        assert counts == {"expired": 1, "voided": 1}
        assert brawl_repo.get_brawl(
            pending.brawl_id, TEST_GUILD_ID
        ).status == "expired"
        assert brawl_repo.get_brawl(
            active.brawl_id, TEST_GUILD_ID
        ).status == "void"
        assert tuple(
            players.get_balance(player_id, TEST_GUILD_ID)
            for player_id in (100, 200, 300, 400)
        ) == (100, 100, 100, 100)


class TestSettlement:
    def hunger_of(self, pet_repo, pet_id, now=NOW + 60):
        pet = pet_repo.get_pet_by_id(pet_id, TEST_GUILD_ID)
        return pet.current_hunger(now, DECAY)

    def test_happy_path_applies_gain_and_loss(
        self, brawl_repo, pet_repo, insert_pet
    ):
        brawl, pet_a, pet_b = make_active(brawl_repo, insert_pet)
        with sqlite3.connect(brawl_repo.db_path) as conn:
            conn.execute(
                "UPDATE pets SET hunger_at_last_fed = 50, last_fed_at = ? "
                "WHERE pet_id = ?",
                (NOW + 60, pet_a),
            )
        result = settle(brawl_repo, brawl, pet_a, pet_b, 100)
        assert result["winner_delta"] == 10
        assert result["loser_delta"] == -15
        assert self.hunger_of(pet_repo, pet_a) == 60
        assert self.hunger_of(pet_repo, pet_b) == 85
        done = brawl_repo.get_brawl(brawl.brawl_id, TEST_GUILD_ID)
        assert done.status == "done"
        assert done.winner_id == 100
        assert done.rounds == 4
        assert (done.winner_hunger_delta, done.loser_hunger_delta) == (10, -15)

    def test_settlement_awards_xp_and_persists_threshold_stat_gain(
        self, brawl_repo, pet_repo, insert_pet
    ):
        pet_a = insert_pet(100, training_xp=4)
        pet_b = insert_pet(200, training_xp=4)
        brawl = brawl_repo.create_brawl_atomic(
            TEST_GUILD_ID,
            555,
            100,
            200,
            pet_a,
            now=NOW,
            expires_at=NOW + 180,
        )
        brawl = brawl_repo.accept_atomic(
            brawl.brawl_id, TEST_GUILD_ID, 200, pet_b, NOW + 5
        )

        result = settle(
            brawl_repo,
            brawl,
            pet_a,
            pet_b,
            100,
            winner_stat_gain="str",
            loser_stat_gain="dex",
            personality_event_key="str.victory",
        )

        winner = pet_repo.get_pet_by_id(pet_a, TEST_GUILD_ID)
        loser = pet_repo.get_pet_by_id(pet_b, TEST_GUILD_ID)
        assert (winner.training_xp, winner.training_str) == (6, 1)
        assert (loser.training_xp, loser.training_dex) == (5, 1)
        assert result["winner_xp_delta"] == 2
        assert result["loser_xp_delta"] == 1
        assert result["winner_stat_gain"] == "str"
        assert result["loser_stat_gain"] == "dex"

        done = brawl_repo.get_brawl(brawl.brawl_id, TEST_GUILD_ID)
        assert (done.challenger_xp_delta, done.recipient_xp_delta) == (2, 1)
        assert (done.challenger_stat_gain, done.recipient_stat_gain) == (
            "str",
            "dex",
        )
        assert done.personality_event_key == "str.victory"

    def test_winner_gain_caps_at_full(self, brawl_repo, pet_repo, insert_pet):
        brawl, pet_a, pet_b = make_active(brawl_repo, insert_pet)
        result = settle(brawl_repo, brawl, pet_a, pet_b, 100)
        assert result["winner_delta"] == 0  # already at 100
        assert self.hunger_of(pet_repo, pet_a) <= 100

    def test_loser_loss_floors_at_ten(self, brawl_repo, pet_repo, insert_pet):
        brawl, pet_a, pet_b = make_active(brawl_repo, insert_pet)
        with sqlite3.connect(brawl_repo.db_path) as conn:
            conn.execute(
                "UPDATE pets SET hunger_at_last_fed = 12, last_fed_at = ? "
                "WHERE pet_id = ?",
                (NOW + 60, pet_b),
            )
        result = settle(brawl_repo, brawl, pet_a, pet_b, 100)
        assert result["loser_delta"] == -2
        assert self.hunger_of(pet_repo, pet_b) == 10

        brawl2, pet_c, pet_d = make_active(brawl_repo, insert_pet, 300, 400)
        with sqlite3.connect(brawl_repo.db_path) as conn:
            conn.execute(
                "UPDATE pets SET hunger_at_last_fed = 8, last_fed_at = ? "
                "WHERE pet_id = ?",
                (NOW + 60, pet_d),
            )
        result = settle(brawl_repo, brawl2, pet_c, pet_d, 300)
        assert result["loser_delta"] == 0
        assert self.hunger_of(pet_repo, pet_d) == 8

    def test_settlement_never_kills(self, brawl_repo, pet_repo, insert_pet):
        brawl, pet_a, pet_b = make_active(brawl_repo, insert_pet)
        settle(brawl_repo, brawl, pet_a, pet_b, 100)
        assert pet_repo.get_pet_by_id(pet_b, TEST_GUILD_ID).died_at is None

    def test_skips_pet_that_died_mid_brawl(self, brawl_repo, pet_repo, insert_pet):
        brawl, pet_a, pet_b = make_active(brawl_repo, insert_pet)
        with sqlite3.connect(brawl_repo.db_path) as conn:
            conn.execute(
                "UPDATE pets SET died_at = ?, death_cause = 'starvation' "
                "WHERE pet_id = ?",
                (NOW + 30, pet_b),
            )
        result = settle(brawl_repo, brawl, pet_a, pet_b, 100)
        assert result["loser_delta"] == 0
        pet = pet_repo.get_pet_by_id(pet_b, TEST_GUILD_ID)
        assert pet.died_at == NOW + 30

    def test_skips_derived_starved_pet(self, brawl_repo, pet_repo, insert_pet):
        brawl, pet_a, pet_b = make_active(brawl_repo, insert_pet)
        # Anchor far in the past: derived hunger is 0 but death not yet claimed.
        with sqlite3.connect(brawl_repo.db_path) as conn:
            conn.execute(
                "UPDATE pets SET hunger_at_last_fed = 100, last_fed_at = ? "
                "WHERE pet_id = ?",
                (NOW - 30 * DAY, pet_b),
            )
        result = settle(brawl_repo, brawl, pet_a, pet_b, 100)
        assert result["loser_delta"] == 0
        pet = pet_repo.get_pet_by_id(pet_b, TEST_GUILD_ID)
        # Anchors untouched: original starvation time preserved.
        assert pet.last_fed_at == NOW - 30 * DAY
        assert pet.died_at is None

    def test_double_settle_rejected(self, brawl_repo, insert_pet):
        brawl, pet_a, pet_b = make_active(brawl_repo, insert_pet)
        settle(brawl_repo, brawl, pet_a, pet_b, 100)
        with pytest.raises(ValueError, match="already_done"):
            settle(brawl_repo, brawl, pet_a, pet_b, 100)

    def test_settle_requires_active(self, brawl_repo, insert_pet):
        brawl, pet_a, pet_b = make_pending(brawl_repo, insert_pet)
        with pytest.raises(ValueError, match="not_active"):
            settle(brawl_repo, brawl, pet_a, pet_b, 100)

    def test_daily_win_cap_and_next_day_reset(
        self, brawl_repo, pet_repo, insert_pet
    ):
        winner_pet = insert_pet(100, hunger=50)
        deltas = []
        for i in range(4):
            opponent = 200 + i
            opp_pet = insert_pet(opponent)
            brawl = brawl_repo.create_brawl_atomic(
                TEST_GUILD_ID, 555, 100, opponent, winner_pet,
                now=NOW + i * 10, expires_at=NOW + i * 10 + 180,
            )
            brawl = brawl_repo.accept_atomic(
                brawl.brawl_id, TEST_GUILD_ID, opponent, opp_pet, NOW + i * 10 + 1
            )
            result = settle(
                brawl_repo, brawl, winner_pet, opp_pet, 100, now=NOW + i * 10 + 2
            )
            deltas.append(result["winner_delta"])
        assert deltas == [10, 10, 10, 0]  # 4th same-day win unrewarded
        wins, losses = brawl_repo.get_pet_record(winner_pet, TEST_GUILD_ID)
        assert (wins, losses) == (4, 0)  # record still counts capped wins

        # Two game-days later the cap resets.
        later = NOW + 2 * DAY
        opp_pet = insert_pet(999)
        brawl = brawl_repo.create_brawl_atomic(
            TEST_GUILD_ID, 555, 100, 999, winner_pet,
            now=later, expires_at=later + 180,
        )
        brawl = brawl_repo.accept_atomic(
            brawl.brawl_id, TEST_GUILD_ID, 999, opp_pet, later + 1
        )
        result = settle(brawl_repo, brawl, winner_pet, opp_pet, 100, now=later + 2)
        assert result["winner_delta"] > 0

    def test_daily_xp_cap_stops_fourth_completed_brawl(
        self, brawl_repo, pet_repo, insert_pet
    ):
        winner_pet = insert_pet(100)
        xp_deltas = []
        for i in range(4):
            opponent = 300 + i
            opp_pet = insert_pet(opponent)
            brawl = brawl_repo.create_brawl_atomic(
                TEST_GUILD_ID,
                555,
                100,
                opponent,
                winner_pet,
                now=NOW + i * 10,
                expires_at=NOW + i * 10 + 180,
            )
            brawl = brawl_repo.accept_atomic(
                brawl.brawl_id,
                TEST_GUILD_ID,
                opponent,
                opp_pet,
                NOW + i * 10 + 1,
            )
            result = settle(
                brawl_repo,
                brawl,
                winner_pet,
                opp_pet,
                100,
                now=NOW + i * 10 + 2,
                winner_stat_gain="dex",
            )
            xp_deltas.append(result["winner_xp_delta"])

        assert xp_deltas == [2, 2, 2, 0]
        trained = pet_repo.get_pet_by_id(winner_pet, TEST_GUILD_ID)
        assert (trained.training_xp, trained.training_dex) == (6, 1)


class TestRecords:
    def test_get_pet_record_and_batch(self, brawl_repo, insert_pet):
        brawl, pet_a, pet_b = make_active(brawl_repo, insert_pet)
        settle(brawl_repo, brawl, pet_a, pet_b, 100)
        brawl2 = brawl_repo.create_brawl_atomic(
            TEST_GUILD_ID, 555, 100, 200, pet_a, now=NOW + 90, expires_at=NOW + 270
        )
        brawl2 = brawl_repo.accept_atomic(
            brawl2.brawl_id, TEST_GUILD_ID, 200, pet_b, NOW + 100
        )
        settle(brawl_repo, brawl2, pet_b, pet_a, 200, now=NOW + 200)
        assert brawl_repo.get_pet_record(pet_a, TEST_GUILD_ID) == (1, 1)
        assert brawl_repo.get_pet_record(pet_b, TEST_GUILD_ID) == (1, 1)
        batch = brawl_repo.get_records_for([pet_a, pet_b, 777], TEST_GUILD_ID)
        assert batch[pet_a] == (1, 1)
        assert batch[pet_b] == (1, 1)
        assert batch[777] == (0, 0)
        assert brawl_repo.get_records_for([], TEST_GUILD_ID) == {}
